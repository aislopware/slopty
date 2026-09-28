//! The system's side on iOS: the continued-processing task of `BackgroundTasks`.
//!
//! objc2 has no `BackgroundTasks` crate in this workspace yet, so the few messages sent here are
//! typed by hand from `BackgroundTasks/BGTask.h`, `BGTaskRequest.h` and `BGTaskScheduler.h`
//! (the iOS 26 SDK's, unchanged in 27), and the classes are looked up by name. Everything the
//! system's task is told goes through one serial queue, the one its launch handler runs on, so
//! a task is only ever touched from that queue.

use std::sync::OnceLock;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSBundle, NSError, NSProgress, NSString};
use parking_lot::Mutex;

use super::Expired;
use super::ledger::{Ended, Launched, Ledger};

// The classes below are looked up at run time; linking the framework is what makes them there.
#[link(name = "BackgroundTasks", kind = "framework")]
unsafe extern "C" {}

/// A task the system started (a `BGContinuedProcessingTask`).
#[derive(Clone, Debug)]
struct Task(Retained<AnyObject>);

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the task is only messaged on the one serial queue, as the safety comment says"
)]
// SAFETY: a task is moved between threads only to reach the one serial queue that every message
// to it is sent on (`queue`), which is also the queue BackgroundTasks runs its launch handler
// on (`registerForTaskWithIdentifier:usingQueue:launchHandler:`), so it is never used from two
// threads at once. Retain and release are atomic on every object.
unsafe impl Send for Task {}

static LEDGER: Mutex<Ledger<Task>> = Mutex::new(Ledger::new());

/// The queue every message to the scheduler and to a task is sent on.
fn queue() -> &'static DispatchRetained<DispatchQueue> {
    static QUEUE: OnceLock<DispatchRetained<DispatchQueue>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        DispatchQueue::new("dev.aislopware.slopty.continued", DispatchQueueAttr::SERIAL)
    })
}

/// The scheduler, when `BackgroundTasks` is there.
fn scheduler() -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"BGTaskScheduler")?;
    // SAFETY: BGTaskScheduler.h: `sharedScheduler` is a class property returning the non-null
    // shared instance.
    let shared: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedScheduler] };
    shared
}

/// The identifier of work `key`: under the bundle's id, as the scheduler requires, with the
/// process id so a new run never reuses one the system still knows.
fn identifier(key: u64) -> Option<String> {
    let bundle = NSBundle::mainBundle().bundleIdentifier()?;
    Some(format!("{bundle}.transfer.{}-{key}", std::process::id()))
}

/// Ask the system to keep work `title` going off screen; its key in the ledger.
pub(super) fn begin(title: &str, subtitle: &str, expired: Expired) -> u64 {
    let key = LEDGER.lock().begin(expired);
    let (title, subtitle) = (title.to_owned(), subtitle.to_owned());
    queue().exec_async(move || {
        if let Err(why) = submit(key, &title, &subtitle) {
            tracing::info!(key, %why, "transfer runs only while the app is on screen");
            LEDGER.lock().refused(key);
        }
    });
    key
}

/// Register work `key`'s launch handler and submit its request.
fn submit(key: u64, title: &str, subtitle: &str) -> Result<(), String> {
    let id = identifier(key).ok_or("no bundle identifier")?;
    submit_as(&id, key, title, subtitle)
}

/// [`submit`] under identifier `id`.
fn submit_as(id: &str, key: u64, title: &str, subtitle: &str) -> Result<(), String> {
    let scheduler = scheduler().ok_or("no BackgroundTasks")?;
    let class = AnyClass::get(c"BGContinuedProcessingTaskRequest").ok_or("no continued tasks")?;
    let id = NSString::from_str(id);
    let launched = RcBlock::new(move |task: *mut AnyObject| {
        // SAFETY: BGTaskScheduler.h: the launch handler is given the non-null task the system
        // started, valid for the call; retaining it keeps it past the call.
        if let Some(task) = unsafe { Retained::retain(task) } {
            run(key, Task(task));
        }
    });
    let queue: &DispatchQueue = queue();
    // SAFETY: BGTaskScheduler.h: `registerForTaskWithIdentifier:usingQueue:launchHandler:`
    // takes an identifier, the queue the handler runs on and a block of one `BGTask *`; the
    // scheduler copies the block. Each identifier is registered once (the key is new), and
    // continued-processing registrations may come after launch.
    let registered: Bool = unsafe {
        msg_send![&*scheduler, registerForTaskWithIdentifier: &*id, usingQueue: queue,
            launchHandler: &*launched]
    };
    if !registered.as_bool() {
        return Err("identifier not permitted by Info.plist".to_owned());
    }
    // SAFETY: NSObject rule: `alloc` on a class returns a fresh instance to initialise.
    let allocated: Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
    let (title, subtitle) = (NSString::from_str(title), NSString::from_str(subtitle));
    // SAFETY: BGTaskRequest.h: `initWithIdentifier:title:subtitle:` is the designated
    // initialiser, taking three non-null strings; the default strategy queues the request when
    // the system is busy.
    let request: Retained<AnyObject> = unsafe {
        msg_send![allocated, initWithIdentifier: &*id, title: &*title, subtitle: &*subtitle]
    };
    // SAFETY: BGTaskScheduler.h: `submitTaskRequest:error:` takes a request and returns NO with
    // the error set when it is refused (unavailable in the simulator, not permitted, too many).
    // Its completion-handler twin is iOS 27 and later, above the floor.
    let submitted: Result<(), Retained<NSError>> =
        unsafe { msg_send![&*scheduler, submitTaskRequest: &*request, error: _] };
    submitted.map_err(|e| e.to_string())
}

/// The system started work `key` as `task` (on the queue).
fn run(key: u64, task: Task) {
    let launched = LEDGER.lock().launched(key, task);
    match launched {
        Launched::Run { task, done, total } => {
            let expire = RcBlock::new(move || queue().exec_async(move || expire(key)));
            // SAFETY: BGTask.h: `expirationHandler` is a strong block property of no arguments;
            // the scheduler clears it after calling it. The block holds only the key, so no
            // cycle through the task.
            let () = unsafe { msg_send![&*task.0, setExpirationHandler: &*expire] };
            show(&task, done, total);
            tracing::debug!(key, "transfer kept running off screen");
        }
        Launched::Finish { task, success } => complete(&task, success),
    }
}

/// The system is taking work `key` back (on the queue).
fn expire(key: u64) {
    let expired = LEDGER.lock().expired(key);
    if let Some((task, tell)) = expired {
        tracing::info!(key, "transfer ended by the system or from its Live Activity");
        complete(&task, false);
        tell();
    }
}

/// Work `key` is `done` of `total` through.
pub(super) fn progress(key: u64, done: u64, total: u64) {
    queue().exec_async(move || {
        let task = LEDGER.lock().progress(key, done, total);
        if let Some(task) = task {
            show(&task, done, total);
        }
    });
}

/// Work `key` ended here.
pub(super) fn end(key: u64, success: bool) {
    queue().exec_async(move || {
        let ended = LEDGER.lock().end(key, success);
        match ended {
            Ended::Complete { task, success } => complete(&task, success),
            Ended::Withdraw => {
                if let (Some(scheduler), Some(id)) = (scheduler(), identifier(key)) {
                    let id = NSString::from_str(&id);
                    // SAFETY: BGTaskScheduler.h: cancelling an identifier with no pending
                    // request does nothing.
                    let () =
                        unsafe { msg_send![&*scheduler, cancelTaskRequestWithIdentifier: &*id] };
                }
            }
            Ended::Nothing => {}
        }
    });
}

/// Show `done` of `total` on the task's progress, which the system's UI draws.
fn show(task: &Task, done: u64, total: u64) {
    // SAFETY: BGTask.h: a continued-processing task conforms to `NSProgressReporting`, whose
    // `progress` is a non-null `NSProgress`.
    let progress: Retained<NSProgress> = unsafe { msg_send![&*task.0, progress] };
    let unit = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
    progress.setTotalUnitCount(unit(total));
    progress.setCompletedUnitCount(unit(done));
}

/// Tell the system the task is over.
fn complete(task: &Task, success: bool) {
    // SAFETY: BGTask.h: `setTaskCompletedWithSuccess:` ends the task; the system suspends the
    // app once none is left.
    let () = unsafe { msg_send![&*task.0, setTaskCompletedWithSuccess: success] };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classes the work needs resolve from the linked framework (run in the simulator:
    /// `xcrun simctl spawn <device> <test binary> continued`).
    #[test]
    fn the_scheduler_and_continued_requests_are_there() {
        assert!(scheduler().is_some(), "BGTaskScheduler");
        assert!(AnyClass::get(c"BGContinuedProcessingTaskRequest").is_some(), "iOS 26's request");
        assert!(AnyClass::get(c"BGContinuedProcessingTask").is_some(), "iOS 26's task");
    }

    /// Where the system will not take a work (no permitted identifier, or the simulator, which
    /// runs no background tasks), registering and submitting say so and nothing throws.
    #[test]
    fn an_unpermitted_submission_is_refused_without_throwing() {
        let id = format!("dev.aislopware.slopty.transfer.test-{}", std::process::id());
        let refused = submit_as(&id, u64::MAX, "Uploading 1 file", "notes.txt");
        println!("submission: {refused:?}");
        assert!(refused.is_err(), "the test binary is permitted nothing");
    }

    /// A work the system cannot take (a test binary has no bundle identifier) is refused and
    /// forgotten, and never expires.
    #[test]
    fn a_refused_work_is_forgotten() {
        let expired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let told = std::sync::Arc::clone(&expired);
        let key = begin(
            "Uploading 1 file",
            "notes.txt",
            std::sync::Arc::new(move || {
                told.store(true, std::sync::atomic::Ordering::Relaxed);
            }),
        );
        queue().exec_sync(|| {});
        assert!(LEDGER.lock().progress(key, 1, 2).is_none(), "refused, so forgotten");
        end(key, true);
        queue().exec_sync(|| {});
        assert!(!expired.load(std::sync::atomic::Ordering::Relaxed));
    }
}
