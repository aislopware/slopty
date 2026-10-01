//! Work a person started that keeps going while the app is off screen.
//!
//! An upload or a download on iOS 26 runs as a continued-processing background task
//! (`BGContinuedProcessingTask`, `BackgroundTasks`). The system shows its progress in a Live
//! Activity and a person can cancel it there, which ends the work through its expiry callback.
//!
//! A [`Work`] is begun where the transfer starts and ended where it ends. Between the two it
//! is told its progress; the system is told again only when the shown thousandth moves. Every
//! call is cheap and never blocks, so a transfer's hot loop may make it.
//!
//! On macOS a process keeps running when its window goes, so there is no background task: a
//! download given the file it lands as shows its progress on that file in Finder instead, and
//! Finder's cancel on it ends the work as the Live Activity's does (`mac`). On Linux a
//! [`Work`] does nothing.
//!
//! On iOS each work is its own task request, `<bundle id>.transfer.<pid>-<n>`, registered just
//! before it is submitted (a handler for the wildcard itself is refused by the scheduler). The
//! `Info.plist` must permit `<bundle id>.transfer.*` in `BGTaskSchedulerPermittedIdentifiers`
//! (`docs/decisions/platform.md`, "Transfers that survive pocketing the phone"). Without it, or
//! in the simulator, which runs no background tasks, the submission is refused and the work
//! goes on only while the app is on screen, as before.

use std::path::Path;
use std::sync::Arc;
#[cfg(any(target_vendor = "apple", test))]
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

#[cfg(target_os = "ios")]
mod ios;
#[cfg(any(target_os = "ios", test))]
mod ledger;
#[cfg(target_os = "macos")]
mod mac;

/// What the system is told when a person cancels the work from its Live Activity, or the
/// system takes its time back: stop, the transfer is over.
pub type Expired = Arc<dyn Fn() + Send + Sync>;

/// The steps a work's progress is shown in: the system hears a change only when the shown
/// thousandth moves.
#[cfg(any(target_vendor = "apple", test))]
const STEPS: u64 = 1000;

/// One transfer's claim on running while the app is off screen. Dropping it ends the work as
/// failed, unless [`Work::end`] ended it first.
#[derive(Debug)]
pub struct Work {
    /// The work's key in the process's ledger.
    #[cfg(target_os = "ios")]
    key: u64,
    /// The progress on the file a download lands as.
    #[cfg(target_os = "macos")]
    on_file: parking_lot::Mutex<Option<mac::OnFile>>,
    #[cfg(target_vendor = "apple")]
    shown: Shown,
}

impl Work {
    /// Begin work a person just asked for, titled for the system's progress UI (`title`, then
    /// `subtitle` below it), landing as the file `at` when it is a download to one. `expired`
    /// is called once, on a queue of the system's, if the person cancels it there or the
    /// system ends it early; it is never called after [`Work::end`].
    #[must_use]
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(unused_variables, reason = "only Apple's systems show the work and can expire it")
    )]
    #[cfg_attr(
        target_os = "macos",
        expect(unused_variables, reason = "macOS shows a download on its file, untitled")
    )]
    #[cfg_attr(
        target_os = "ios",
        expect(unused_variables, reason = "iOS shows the work in a Live Activity, not on a file")
    )]
    pub fn begin(
        title: &str,
        subtitle: &str,
        at: Option<&Path>,
        expired: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            #[cfg(target_os = "ios")]
            key: ios::begin(title, subtitle, Arc::new(expired)),
            #[cfg(target_os = "macos")]
            on_file: parking_lot::Mutex::new(
                at.map(|at| mac::OnFile::publish(at, Arc::new(expired))),
            ),
            #[cfg(target_vendor = "apple")]
            shown: Shown::default(),
        }
    }

    /// `done` of `total` units (bytes) are through. The system hears it when the shown
    /// thousandth moves or the total changes.
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(
            unused_variables,
            clippy::unused_self,
            clippy::missing_const_for_fn,
            reason = "only Apple's systems show progress"
        )
    )]
    pub fn progress(&self, done: u64, total: u64) {
        #[cfg(target_vendor = "apple")]
        if self.shown.moved(done, total) {
            #[cfg(target_os = "ios")]
            ios::progress(self.key, done.min(total), total);
            #[cfg(target_os = "macos")]
            if let Some(on_file) = &*self.on_file.lock() {
                on_file.progress(done, total);
            }
        }
    }

    /// The work is over: whole (`success`), or given up. Only the first end counts.
    #[cfg_attr(
        not(target_vendor = "apple"),
        expect(
            unused_variables,
            clippy::unused_self,
            clippy::missing_const_for_fn,
            reason = "only Apple's systems are told"
        )
    )]
    #[cfg_attr(
        target_os = "macos",
        expect(unused_variables, reason = "Finder's progress goes, whole or not")
    )]
    pub fn end(&self, success: bool) {
        #[cfg(target_vendor = "apple")]
        if self.shown.end() {
            #[cfg(target_os = "ios")]
            ios::end(self.key, success);
            // Unpublished, and its placeholder gone, before the download is renamed into place.
            #[cfg(target_os = "macos")]
            drop(self.on_file.lock().take());
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        self.end(false);
    }
}

/// What the system was last shown of a work, so it hears only what moved.
#[cfg(any(target_vendor = "apple", test))]
#[derive(Debug, Default)]
struct Shown {
    ended: AtomicBool,
    total: AtomicU64,
    /// The thousandth of `total` last shown.
    step: AtomicU32,
}

#[cfg(any(target_vendor = "apple", test))]
impl Shown {
    /// Whether `done` of `total` shows differently from the last: the thousandth or the total
    /// moved, and the work has not ended.
    fn moved(&self, done: u64, total: u64) -> bool {
        if self.ended.load(Ordering::Relaxed) {
            return false;
        }
        let step = thousandth(done, total);
        let total_moved = self.total.swap(total, Ordering::Relaxed) != total;
        let step_moved = self.step.swap(step, Ordering::Relaxed) != step;
        total_moved || step_moved
    }

    /// Whether this is the work's first end.
    fn end(&self) -> bool {
        !self.ended.swap(true, Ordering::Relaxed)
    }
}

/// `done` of `total` in thousandths, `total` of 0 being none done.
#[cfg(any(target_vendor = "apple", test))]
fn thousandth(done: u64, total: u64) -> u32 {
    if total == 0 {
        return 0;
    }
    let steps = u128::from(done.min(total))
        .saturating_mul(u128::from(STEPS))
        .checked_div(u128::from(total))
        .unwrap_or_default();
    u32::try_from(steps).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Progress is shown in thousandths, whatever the sizes, a total of 0 being none done.
    #[test]
    fn progress_is_shown_in_thousandths() {
        assert_eq!(thousandth(0, 0), 0);
        assert_eq!(thousandth(5, 0), 0);
        assert_eq!(thousandth(1, 3), 333);
        assert_eq!(thousandth(3, 3), 1000);
        assert_eq!(thousandth(9, 3), 1000, "never past the whole");
        assert_eq!(thousandth(u64::MAX / 2, u64::MAX), 499);
    }

    /// The system hears a work's progress only when the shown thousandth or the total moves,
    /// and nothing after its end, which counts once.
    #[test]
    fn only_what_moved_is_shown_and_only_until_the_end() {
        let shown = Shown::default();
        assert!(shown.moved(1, 1000), "the first thousandth");
        assert!(!shown.moved(1, 1000), "the same");
        assert!(shown.moved(1_001, 1_000_000), "the total moved");
        assert!(!shown.moved(1_999, 1_000_000), "still the first thousandth");
        assert!(shown.moved(2_000, 1_000_000), "the next thousandth");
        assert!(shown.end(), "the first end");
        assert!(!shown.end(), "counts once");
        assert!(!shown.moved(1_000_000, 1_000_000), "nothing after the end");
    }

    /// Off iOS and with no file to show on, a work asks nothing of the system, its expiry is
    /// never called, and ending it twice (explicitly, then by the drop) is fine.
    #[cfg(not(target_os = "ios"))]
    #[test]
    fn off_ios_a_work_without_a_file_is_inert() {
        use std::sync::atomic::AtomicUsize;

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let work = Work::begin("Uploading 2 files", "4 MB", None, move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        work.progress(1, 2);
        work.end(true);
        drop(work);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
