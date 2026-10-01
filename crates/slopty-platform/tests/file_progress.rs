//! A download's progress as Finder sees it (`slopty_platform::continued`, macOS).
//!
//! Finder subscribes to the progress published for a file URL; Foundation hands the publish to
//! a subscriber on its main thread, which libtest never gives a test. So this binary is its own
//! harness (`harness = false`): `main` runs the test on the main thread, pumping its run loop
//! while it waits. It answers a runner's `--list --format terse` with its one test, and none for
//! `--ignored`, as nextest asks of a harness of its own.

#[cfg(target_os = "macos")]
#[expect(clippy::unwrap_used, reason = "a test, which fails by panicking")]
mod mac {
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    use objc2_foundation::{NSProgress, NSProgressUnpublishingHandler, NSString, NSURL};
    use parking_lot::Mutex;
    use slopty_platform::continued::Work;

    pub const NAME: &str = "a_download_shows_on_its_file_and_finders_cancel_ends_it";

    /// Run the main run loop until `done`, five seconds at most.
    fn until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now().checked_add(Duration::from_secs(5)).unwrap();
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            // SAFETY: CoreFoundation rule: `kCFRunLoopDefaultMode` is a constant string valid
            // for the life of the process.
            let mode = unsafe { kCFRunLoopDefaultMode };
            let _ran = CFRunLoop::run_in_mode(mode, 0.01, true);
        }
    }

    /// A download shows on the file it lands as, as Finder subscribes to it: a progress
    /// published for that file and counting its bytes, on a placeholder holding its name.
    /// Finder's cancel on it ends the transfer, and the placeholder goes with the work.
    pub fn a_download_shows_on_its_file_and_finders_cancel_ends_it() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("report.pdf");
        let seen: Arc<Mutex<Option<Retained<NSProgress>>>> = Arc::default();
        let finder = Arc::clone(&seen);
        let published = RcBlock::new(move |progress: NonNull<NSProgress>| {
            // SAFETY: Foundation rule: the publishing handler is given a live progress.
            *finder.lock() = unsafe { Retained::retain(progress.as_ptr()) };
            let unpublished: NSProgressUnpublishingHandler = std::ptr::null_mut();
            unpublished
        });
        // The destination's own URL, as Finder watches the item it drew for the drop.
        let watched = NSURL::fileURLWithPath(&NSString::from_str(&at.to_string_lossy()));
        // SAFETY: Foundation rule: the handler is a valid block, copied by the call.
        let subscriber = unsafe {
            NSProgress::addSubscriberForFileURL_withPublishingHandler(
                &watched,
                RcBlock::as_ptr(&published),
            )
        };

        let cancels = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&cancels);
        let work = Work::begin("Downloading report.pdf", "From the worker", Some(&at), move || {
            counted.fetch_add(1, Ordering::Relaxed);
        });
        let empty = std::fs::metadata(&at).is_ok_and(|m| m.is_file() && m.len() == 0);
        assert!(empty, "a placeholder holds the name");
        until("Finder sees the progress", || seen.lock().is_some());
        let shown = seen.lock().clone().unwrap();
        work.progress(50, 200);
        until("its bytes", || (shown.fractionCompleted() - 0.25).abs() < 1e-9);
        assert!(shown.isCancellable(), "Finder offers to cancel it");

        shown.cancel();
        until("Finder's cancel reaches the transfer", || cancels.load(Ordering::Relaxed) == 1);
        work.end(false);
        assert!(!at.exists(), "the placeholder goes with the work");
        // SAFETY: Foundation rule: the subscriber is the one `addSubscriberForFileURL` gave.
        unsafe {
            NSProgress::removeSubscriber(&subscriber);
        }
    }
}

#[cfg_attr(
    target_os = "macos",
    expect(clippy::print_stdout, reason = "a harness of its own lists and reports its test")
)]
fn main() {
    #[cfg(target_os = "macos")]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.iter().any(|a| a == "--list") {
            if !args.iter().any(|a| a == "--ignored") {
                println!("{}: test", mac::NAME);
            }
            return;
        }
        let named: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        if named.iter().all(|filter| mac::NAME.contains(filter.as_str())) {
            mac::a_download_shows_on_its_file_and_finders_cancel_ends_it();
            println!("test {} ... ok", mac::NAME);
        }
    }
}
