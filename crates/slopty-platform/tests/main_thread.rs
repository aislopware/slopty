//! What only the main thread can see (macOS): a download's progress as Finder sees it
//! (`slopty_platform::continued`), and a browser page's Web Inspector (`slopty_platform::web`).
//!
//! Finder subscribes to the progress published for a file URL, and Foundation hands the publish
//! to a subscriber on its main thread; a `WKWebView` is made only there. libtest never gives a
//! test the main thread, so this binary is its own harness (`harness = false`): `main` runs each
//! test there, pumping its run loop while one waits. It answers a runner's `--list --format
//! terse` with its tests, and none for `--ignored`, as nextest asks of a harness of its own.

#[cfg(target_os = "macos")]
#[expect(clippy::unwrap_used, reason = "a test, which fails by panicking")]
mod mac {
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    use objc2_foundation::{NSProgress, NSProgressUnpublishingHandler, NSString, NSURL};
    use parking_lot::Mutex;
    use slopty_platform::continued::Work;
    use slopty_platform::web::WebView;

    /// Every test, by name.
    pub const TESTS: [(&str, fn()); 2] = [
        (
            "a_download_shows_on_its_file_and_finders_cancel_ends_it",
            a_download_shows_on_its_file_and_finders_cancel_ends_it,
        ),
        ("every_page_is_open_to_web_inspector", every_page_is_open_to_web_inspector),
    ];

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
    fn a_download_shows_on_its_file_and_finders_cancel_ends_it() {
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

    /// Every page is open to Web Inspector, as a developer's browser is. Nothing is shown:
    /// the page is in no window, and no inspector is opened.
    fn every_page_is_open_to_web_inspector() {
        let mtm = MainThreadMarker::new().unwrap();
        let gpui = NSView::new(mtm);
        let host = NonNull::from(&*gpui).cast();
        // Worker 0: a store that keeps nothing on disk.
        let page = WebView::new(host, 0, "about:blank", std::rc::Rc::new(|_event| {})).unwrap();
        assert!(page.inspectable(), "open to the inspector");
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
                for (name, _test) in mac::TESTS {
                    println!("{name}: test");
                }
            }
            return;
        }
        let exact = args.iter().any(|a| a == "--exact");
        let named: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
        for (name, test) in mac::TESTS {
            let wanted = named.iter().all(|filter| {
                if exact { name == filter.as_str() } else { name.contains(filter.as_str()) }
            });
            if wanted {
                test();
                println!("test {name} ... ok");
            }
        }
    }
}
