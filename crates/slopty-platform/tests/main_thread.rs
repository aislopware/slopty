//! What only the main thread can see (macOS): a download's progress as Finder sees it
//! (`slopty_platform::continued`), a browser page's Web Inspector (`slopty_platform::web`), a
//! self-test window's size on a screen too short for it (`slopty_platform::asked_size`), what a
//! keyboard layout's keys type (`slopty_platform::input_source`, which `HIToolbox` asks of the
//! main thread), and a sandboxed page's own report of what it could reach
//! (`slopty_platform::web::page`, a real `WKWebView` each).
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
    pub const TESTS: [(&str, fn()); 17] = [
        (
            "a_download_shows_on_its_file_and_finders_cancel_ends_it",
            a_download_shows_on_its_file_and_finders_cancel_ends_it,
        ),
        ("every_page_is_open_to_web_inspector", every_page_is_open_to_web_inspector),
        (
            "a_forgotten_workers_store_goes_once_its_last_page_has",
            a_forgotten_workers_store_goes_once_its_last_page_has,
        ),
        (
            "a_self_test_window_keeps_its_size_on_a_short_screen",
            a_self_test_window_keeps_its_size_on_a_short_screen,
        ),
        (
            "a_layout_names_the_key_each_character_is_on",
            a_layout_names_the_key_each_character_is_on,
        ),
        ("a_keyboard_is_heard_as_it_comes_and_goes", a_keyboard_is_heard_as_it_comes_and_goes),
        (
            "the_corners_go_off_and_come_back_with_a_remote_keyboard",
            the_corners_go_off_and_come_back_with_a_remote_keyboard,
        ),
        (
            "a_page_reaches_no_network_and_no_other_page",
            pages::a_page_reaches_no_network_and_no_other_page,
        ),
        (
            "a_page_takes_the_theme_and_its_height_is_capped",
            pages::a_page_takes_the_theme_and_its_height_is_capped,
        ),
        ("a_page_cannot_navigate_away", pages::a_page_cannot_navigate_away),
        (
            "a_page_reaches_the_network_only_once_granted",
            pages::a_page_reaches_the_network_only_once_granted,
        ),
        ("a_views_requests_wait_for_the_app", pages::a_views_requests_wait_for_the_app),
        (
            "pages_are_capped_and_each_has_its_own_process",
            pages::pages_are_capped_and_each_has_its_own_process,
        ),
        ("a_vega_lite_fence_draws_offline", pages::a_vega_lite_fence_draws_offline),
        ("a_plotly_fence_draws_offline", pages::a_plotly_fence_draws_offline),
        (
            "a_page_moved_to_the_pane_keeps_its_document",
            pages::a_page_moved_to_the_pane_keeps_its_document,
        ),
        (
            "compiling_the_rule_list_leaves_other_processes_lists_alone",
            pages::compiling_the_rule_list_leaves_other_processes_lists_alone,
        ),
    ];

    /// Run as the other process of
    /// `compiling_the_rule_list_leaves_other_processes_lists_alone`, when this one is:
    /// whether it was.
    pub fn as_other_process() -> bool {
        if std::env::var_os(pages::OTHER_PROCESS).is_none() {
            return false;
        }
        pages::other_process();
        true
    }

    /// macOS 27 takes the corners option with the Dock hidden, and the app's options follow a
    /// remote keyboard both ways. The test binary is never the frontmost app, so the person's
    /// Dock and corners stay as they are.
    fn the_corners_go_off_and_come_back_with_a_remote_keyboard() {
        use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions as O};
        use slopty_platform::corners;

        let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
        let before = app.presentationOptions();
        app.setPresentationOptions(O::AutoHideDock | O::AutoHideMenuBar);
        corners::remote_keyboard(true);
        let off = app.presentationOptions();
        corners::remote_keyboard(false);
        let back = app.presentationOptions();
        app.setPresentationOptions(before);
        assert_eq!(off, O::AutoHideDock | O::AutoHideMenuBar | corners::CORNERS_OFF, "taken");
        assert_eq!(back, O::AutoHideDock | O::AutoHideMenuBar, "given back");
    }

    /// The first keyboard connecting is heard as attached and the last going as gone, on the
    /// main thread, as `GameController` posts them; a dropped watch hears nothing more.
    fn a_keyboard_is_heard_as_it_comes_and_goes() {
        use std::cell::RefCell;
        use std::rc::Rc;

        use objc2_foundation::NSNotificationCenter;
        use objc2_game_controller::{
            GCKeyboardDidConnectNotification, GCKeyboardDidDisconnectNotification,
        };

        let heard: Rc<RefCell<Vec<bool>>> = Rc::default();
        let into = Rc::clone(&heard);
        let watch = slopty_platform::keyboard_presence::watch(move |attached| {
            assert!(MainThreadMarker::new().is_some(), "heard on the main thread");
            into.borrow_mut().push(attached);
        });
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: an immutable `NSString` static GameController defines (GCKeyboard.h).
        let connected = unsafe { GCKeyboardDidConnectNotification };
        // SAFETY: as above.
        let gone = unsafe { GCKeyboardDidDisconnectNotification };
        // SAFETY: Foundation rule: a notification posted with no object, on the main thread.
        unsafe {
            center.postNotificationName_object(connected, None);
        }
        until("the keyboard connecting", || heard.borrow().len() == 1);
        // SAFETY: as above.
        unsafe {
            center.postNotificationName_object(gone, None);
        }
        until("the keyboard going", || heard.borrow().len() == 2);
        assert_eq!(*heard.borrow(), [true, false], "attached, then gone");
        drop(watch);
        // SAFETY: as above.
        unsafe {
            center.postNotificationName_object(connected, None);
        }
        let deadline = Instant::now().checked_add(Duration::from_millis(200)).unwrap();
        until("a moment", || Instant::now() >= deadline);
        assert_eq!(heard.borrow().len(), 2, "a dropped watch hears nothing");
    }

    /// What a ⌘ chord's character is on, read from layouts macOS ships, none of them selected.
    /// French AZERTY types `a` on Q's place and `q` on A's, German QWERTZ `z` on Y's. Under ⌘
    /// Russian types Latin letters at their US places and "Dvorak - QWERTY ⌘" types QWERTY,
    /// while plain Dvorak keeps its own: what a key types with ⌘ held is what an app matches.
    fn a_layout_names_the_key_each_character_is_on() {
        use slopty_platform::input_source::{SelectError, layout_keys_of};
        for (id, vk, with_command) in [
            ("com.apple.keylayout.French", 0x0c_u16, 'a'),
            ("com.apple.keylayout.French", 0x00, 'q'),
            ("com.apple.keylayout.German", 0x10, 'z'),
            ("com.apple.keylayout.German", 0x06, 'y'),
            ("com.apple.keylayout.US", 0x06, 'z'),
            ("com.apple.keylayout.Russian", 0x08, 'c'),
            ("com.apple.keylayout.DVORAK-QWERTYCMD", 0x08, 'c'),
            ("com.apple.keylayout.Dvorak", 0x22, 'c'),
        ] {
            let keys = layout_keys_of(id).unwrap();
            let key = keys.iter().find(|(at, ..)| *at == vk).copied();
            assert!(
                key.is_some_and(|(_, typed, _)| typed == Some(with_command)),
                "{id} {vk:#04x} types {with_command:?} with ⌘: {key:?}"
            );
        }
        assert_eq!(
            layout_keys_of("com.apple.keylayout.Nonexistent"),
            Err(SelectError::Unknown),
            "a layout this Mac does not have",
        );
    }

    /// AppKit fits a plain window into a screen too short for it; a window class the self-test
    /// asked to keep its sizes keeps the frame it is given, and is asked only once. No window
    /// is shown: AppKit is asked what it would fit the frame to.
    fn a_self_test_window_keeps_its_size_on_a_short_screen() {
        use objc2::runtime::{AnyClass, ClassBuilder};
        use objc2::{ClassType as _, msg_send};
        use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use slopty_platform::asked_size::keep_asked_sizes;

        let mtm = MainThreadMarker::new().unwrap();
        let screen = NSScreen::mainScreen(mtm).unwrap();
        let visible = screen.visibleFrame();
        let tall = NSRect::new(
            visible.origin,
            NSSize::new(visible.size.width.min(1000.0), visible.size.height + 400.0),
        );
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Resizable;
        let made = |class: &AnyClass| -> Retained<NSWindow> {
            // SAFETY: Objective-C runtime rule: `+alloc` on an `NSWindow` subclass returns an
            // uninitialised instance of it, which `initWithContentRect:…` takes.
            let allocated = unsafe { msg_send![class, alloc] };
            // SAFETY: AppKit rule: a window made on the main thread, never shown, released
            // with its last reference (`releasedWhenClosed` is not used: it is never closed).
            unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    allocated,
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)),
                    style,
                    NSBackingStoreType::Buffered,
                    true,
                )
            }
        };

        let plain = made(NSWindow::class());
        let fitted = plain.constrainFrameRect_toScreen(tall, Some(&screen));
        assert!(fitted.size.height < tall.size.height, "AppKit fits a plain window: {fitted:?}");

        let kept_class =
            ClassBuilder::new(c"SloptyAskedSizeTestWindow", NSWindow::class()).unwrap().register();
        assert!(keep_asked_sizes(c"SloptyAskedSizeTestWindow"), "the class keeps its sizes");
        assert!(!keep_asked_sizes(c"SloptyAskedSizeTestWindow"), "asked once, it is kept");
        assert!(!keep_asked_sizes(c"NoSuchWindowClass"), "no such class");
        let kept = made(kept_class);
        let asked = kept.constrainFrameRect_toScreen(tall, Some(&screen));
        assert_eq!(asked, tall, "the frame asked for, on a screen too short for it");
    }

    /// Run the main run loop until `done`, twenty seconds at most: what a loaded page does once
    /// it runs. Starting its process is [`until_started`]'s.
    fn until(what: &str, done: impl Fn() -> bool) {
        until_within(what, Duration::from_secs(20), done);
    }

    /// How long a page's process may take to start and load its first document. That is
    /// what a loaded machine stretches most (a gate's runner runs 1497 tests at once, and a
    /// page process started there past 20 s), so it is waited for on its own, before the 20 s
    /// of what the page then does.
    const PROCESS_START: Duration = Duration::from_secs(120);

    /// Run the main run loop until `done`, which a page's process starting and its document
    /// loading decide, [`PROCESS_START`] at most.
    fn until_started(what: &str, done: impl Fn() -> bool) {
        until_within(
            &format!("{what} (its process started and its document loaded)"),
            PROCESS_START,
            done,
        );
    }

    /// Run the main run loop until `done`, `limit` at most.
    fn until_within(what: &str, limit: Duration, done: impl Fn() -> bool) {
        let deadline = Instant::now().checked_add(limit).unwrap();
        while !done() {
            assert!(Instant::now() < deadline, "{what}, not within {limit:?}");
            // SAFETY: CoreFoundation rule: `kCFRunLoopDefaultMode` is a constant string valid
            // for the life of the process.
            let mode = unsafe { kCFRunLoopDefaultMode };
            // Each turn in a pool of its own, as AppKit's event loop drains one per event.
            let _ran = objc2::rc::autoreleasepool(|_| CFRunLoop::run_in_mode(mode, 0.01, true));
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

    /// A forgotten worker's store goes with its last page: forgotten as the page goes, as the app
    /// forgets it, with no wait or retry of the caller's.
    fn a_forgotten_workers_store_goes_once_its_last_page_has() {
        use std::cell::RefCell;
        use std::io::{Read as _, Write as _};
        use std::rc::Rc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Each connection on its own thread: WebKit may open one it sends nothing on, and a
        // server waiting on that one would never take the next load's.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                std::thread::spawn(move || {
                    let mut asked = [0_u8; 4096];
                    let _read = stream.read(&mut asked);
                    let page = "<html><head><title>a page</title></head><body>here</body></html>";
                    let said = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                         Set-Cookie: seen=1\r\nConnection: close\r\n\r\n{page}",
                        page.len()
                    );
                    let _wrote = stream.write_all(said.as_bytes());
                });
            }
        });
        let mtm = MainThreadMarker::new().unwrap();
        let gpui = NSView::new(mtm);
        let host = NonNull::from(&*gpui).cast();
        // A worker of this run's own, so a store left by another run is not this one.
        let worker = (u128::from(std::process::id()) << 64) | 0x5107_7e57;
        let loaded = Rc::new(RefCell::new(0_usize));
        let heard = Rc::clone(&loaded);
        let sink = Rc::new(move |event| {
            if matches!(event, slopty_platform::web::WebEvent::Loaded) {
                let more = heard.borrow().saturating_add(1);
                *heard.borrow_mut() = more;
            }
        });
        objc2::rc::autoreleasepool(|_| slopty_platform::web::route(worker, 9));
        let page = objc2::rc::autoreleasepool(|_| {
            WebView::new(host, worker, &format!("http://127.0.0.1:{port}/"), sink).unwrap()
        });
        until_started("the page loads", || *loaded.borrow() == 1);
        objc2::rc::autoreleasepool(|_| page.load(&format!("http://127.0.0.1:{port}/next")));
        until("and a second", || *loaded.borrow() == 2);
        // As the app does: the worker is forgotten as its tiles go, their pages still open.
        let gone = Rc::new(RefCell::new(None));
        let told = Rc::clone(&gone);
        slopty_platform::web::forget(worker, move |said| *told.borrow_mut() = Some(said));
        objc2::rc::autoreleasepool(|_| drop(page));
        until("WebKit lets the store go", || gone.borrow().is_some());
        assert_eq!(gone.borrow_mut().take(), Some(Ok(())), "the store goes with its last page");
    }

    /// Pages inline in a transcript (`slopty_platform::web::page`): each test opens real
    /// `WKWebView`s, and each page reports its own facts through MCP Apps' bridge, as a log
    /// (`notifications/message`) the host hands the test.
    mod pages {
        use std::cell::RefCell;
        use std::io::{Read as _, Write as _};
        use std::rc::Rc;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        use serde_json::{Value, json};
        use slopty_platform::web::page::{
            Ask, Declined, Device, HostConfig, MOUNTED_CAP, Network, Notice, PageEvent, PageHost,
            PageId, PageSpec, Presence, Refusal, Theme,
        };

        use super::{until, until_started};

        type Events = Rc<RefCell<Vec<(PageId, PageEvent)>>>;

        /// The theme's background, which a page must compute as its own.
        const BACKGROUND: &str = "#101418";

        /// What every page reports through: a log of `data`.
        const REPORT: &str = "const report = (data) => parent.postMessage({ jsonrpc: '2.0', \
            method: 'notifications/message', params: { level: 'info', data } }, '*');";

        /// What a page makes requests with, and the handshake.
        const REQUESTS: &str = "let asked = 0; const waiting = new Map(); \
            window.addEventListener('message', (event) => { const m = event.data; \
              if (m && m.id !== undefined && !m.method && waiting.has(m.id)) { \
                waiting.get(m.id)(m); waiting.delete(m.id); } }); \
            const request = (method, params) => new Promise((resolve) => { asked += 1; \
              waiting.set(asked, resolve); \
              parent.postMessage({ jsonrpc: '2.0', id: asked, method, params }, '*'); }); \
            const handshake = () => request('ui/initialize', { protocolVersion: '2026-01-26', \
              appCapabilities: {}, appInfo: { name: 'test', version: '1' } }).then((answer) => { \
              parent.postMessage({ jsonrpc: '2.0', method: 'ui/notifications/initialized' }, '*'); \
              return answer; });";

        fn host(device: Device, events: &Events) -> PageHost {
            let into = Rc::clone(events);
            let config = HostConfig {
                device,
                theme: Theme {
                    dark: true,
                    variables: vec![
                        ("--color-background-primary".to_owned(), BACKGROUND.to_owned()),
                        ("--color-text-primary".to_owned(), "#e8eaed".to_owned()),
                    ],
                },
                viewport: 1000.0,
                locale: "en-GB".to_owned(),
            };
            PageHost::new(config, move |page, event| into.borrow_mut().push((page, event))).unwrap()
        }

        fn page(title: &str, script: &str) -> PageSpec {
            PageSpec {
                title: title.to_owned(),
                html: format!(
                    "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>\
                     <p>{title}</p><script>{REPORT}{REQUESTS}{script}</script></body></html>"
                ),
                server: true,
                resources: None,
            }
        }

        /// Run the run loop until each of `pages` has loaded its view, before the test waits on
        /// what they do: a page's process starting is the slow part under load
        /// ([`until_started`]), and one that failed or crashed says so at once.
        fn view_loaded(host: &PageHost, events: &Events, pages: &[PageId]) {
            for page in pages {
                until_started(&format!("{page:?}'s view"), || {
                    let ended = events.borrow().iter().find_map(|(of, event)| match event {
                        PageEvent::Failed(why) if of == page => Some(format!("failed: {why}")),
                        PageEvent::Crashed if of == page => Some("crashed".to_owned()),
                        _ => None,
                    });
                    assert!(ended.is_none(), "{page:?} {}", ended.unwrap_or_default());
                    host.facts(*page).is_some_and(|f| f.view_loads >= 1)
                });
            }
        }

        /// Set in the other process of
        /// [`compiling_the_rule_list_leaves_other_processes_lists_alone`].
        pub(super) const OTHER_PROCESS: &str = "SLOPTY_TEST_RULES_OTHER_PROCESS";

        /// A process's compile of the rule list every page loads under leaves another process's
        /// compiled list alone, so two processes compiling it at once never race. A compile
        /// deletes the list's file, moves the new one in and maps it; processes of one executable
        /// compiled into the one store they shared, so one's delete could land between another's
        /// move and map, and that one's pages failed (`WKErrorDomain` 6), as a gate's run of this
        /// binary's page tests side by side did. Whether two compiles meet in that moment is luck;
        /// whether one replaces the other's file is not, so that is what this checks: the file
        /// this process's list is mapped from is still the file at its path after another process
        /// compiled the same list and loaded a page under it, and the other's store goes once it
        /// has ended, at this process's next compile.
        pub(super) fn compiling_the_rule_list_leaves_other_processes_lists_alone() {
            let events: Events = Rc::default();
            let first = host(Device::Mac, &events);
            let one = first.open(page("first", ""));
            let _changes = first.mount(one);
            view_loaded(&first, &events, &[one]);
            let (mine, file) = mapped_rules();
            let other = std::process::Command::new(std::env::current_exe().unwrap())
                .env(OTHER_PROCESS, "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&other.stderr);
            assert!(other.status.success(), "the other process's page: {said}");
            assert!(
                there(&mine, file),
                "{} is still the file this process's list is mapped from",
                mine.display()
            );
            let said = String::from_utf8(other.stdout).unwrap();
            let theirs = std::path::PathBuf::from(said.lines().last().unwrap_or_default());
            assert!(theirs.exists(), "the other process's list stays until the next compile");
            let next = host(Device::Mac, &events);
            let two = next.open(page("next", ""));
            let _changes = next.mount(two);
            view_loaded(&next, &events, &[two]);
            assert!(!theirs.exists(), "the ended process's store went at the next compile");
        }

        /// The other process: a page under a host of its own, then where its list is mapped
        /// from, on its standard output.
        #[expect(clippy::print_stdout, reason = "where its list is mapped from, for the test")]
        pub(super) fn other_process() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let one = host.open(page("other", ""));
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[one]);
            println!("{}", mapped_rules().0.display());
        }

        /// What the kernel tells of a region of a process's memory and the file it maps
        /// (`<sys/proc_info.h>`, `proc_regionwithpathinfo`).
        #[repr(C)]
        struct Region {
            /// `proc_regioninfo`'s protections, offset and page counts.
            _head: [u64; 10],
            address: u64,
            size: u64,
            vnode: libc::vnode_info_path,
        }

        /// Whether `file`, an inode, is the file at `path`.
        fn there(path: &std::path::Path, file: u64) -> bool {
            std::fs::metadata(path)
                .is_ok_and(|now| std::os::unix::fs::MetadataExt::ino(&now) == file)
        }

        /// The compiled rule list file this process maps, by its path and its inode: the one of
        /// its mapped files named as a content rule list (`ContentRuleList-`) that is still the
        /// file at its path, since an earlier compile's may be mapped too.
        fn mapped_rules() -> (std::path::PathBuf, u64) {
            use std::os::unix::ffi::OsStringExt as _;
            /// `<sys/proc_info.h>`: the region at or after an address, with its file's path.
            const PROC_PIDREGIONPATHINFO: libc::c_int = 8;
            let pid = libc::c_int::try_from(std::process::id()).unwrap();
            let size = libc::c_int::try_from(size_of::<Region>()).unwrap();
            let mut lists = Vec::new();
            let mut address = 0;
            loop {
                // SAFETY: a region is integers, all of which may be zero.
                let mut region: Region = unsafe { std::mem::zeroed() };
                // SAFETY: Darwin rule (`proc_info.h`): `proc_pidinfo` writes at most `size` bytes
                // of the region at or after `address` into the buffer, and says how many.
                let wrote = unsafe {
                    libc::proc_pidinfo(
                        pid,
                        PROC_PIDREGIONPATHINFO,
                        address,
                        (&raw mut region).cast(),
                        size,
                    )
                };
                if wrote != size {
                    break;
                }
                address = region.address.checked_add(region.size).unwrap();
                let path: Vec<u8> = region
                    .vnode
                    .vip_path
                    .as_flattened()
                    .iter()
                    .map(|c| c.cast_unsigned())
                    .take_while(|b| *b != 0)
                    .collect();
                let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(path));
                let named = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("ContentRuleList-"));
                let file = region.vnode.vip_vi.vi_stat.vst_ino;
                if named && !lists.contains(&(path.clone(), file)) {
                    lists.push((path, file));
                }
            }
            let mut live: Vec<(std::path::PathBuf, u64)> =
                lists.iter().filter(|(path, file)| there(path, *file)).cloned().collect();
            let said = "one compiled rule list mapped and still the file at its path";
            assert_eq!(live.len(), 1, "{said}: {lists:?}");
            live.pop().unwrap()
        }

        /// What `page` logged under `key`, the last time it did.
        fn logged(events: &Events, page: PageId, key: &str) -> Option<Value> {
            events.borrow().iter().rev().find_map(|(of, event)| match event {
                PageEvent::Log { data, .. } if *of == page => data.get(key).cloned(),
                _ => None,
            })
        }

        fn count(events: &Events, page: PageId, which: impl Fn(&PageEvent) -> bool) -> usize {
            events.borrow().iter().filter(|(of, event)| *of == page && which(event)).count()
        }

        /// Run the run loop for `ms` milliseconds, for what must not happen to have had its
        /// chance.
        fn settle(ms: u64) {
            let deadline = Instant::now().checked_add(Duration::from_millis(ms)).unwrap();
            until("a moment", || Instant::now() >= deadline);
        }

        /// A page's fetch and web socket both fail, an image from the web has no width, and a
        /// fetch of another page's origin fails: nothing outside its own origin is reached.
        pub(super) fn a_page_reaches_no_network_and_no_other_page() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let other = host.open(page("other", ""));
            let script = format!(
                "let ok = 0, failed = 0, left = 2; \
                 const settle = (good) => {{ if (good) ok += 1; else failed += 1; left -= 1; \
                   if (left === 0) report({{ network: {{ ok, failed }} }}); }}; \
                 fetch('https://example.com/').then(() => settle(true), () => settle(false)); \
                 try {{ const socket = new WebSocket('wss://example.com/'); \
                   socket.onopen = () => settle(true); socket.onerror = () => settle(false); }} \
                 catch (_) {{ settle(false); }} \
                 fetch('{}/x').then(() => report({{ other: 'reached' }}), \
                   () => report({{ other: 'failed' }})); \
                 window.addEventListener('load', () => report({{ naturalWidth: \
                   document.getElementById('far').naturalWidth }}));",
                other.origin()
            );
            let mut spec = page("network", &script);
            spec.html =
                spec.html.replace("<p>", "<img id=\"far\" src=\"https://example.com/x.png\"><p>");
            let one = host.open(spec);
            let _changes = host.mount(other);
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[other, one]);
            until("the page reports", || {
                ["network", "other", "naturalWidth"]
                    .iter()
                    .all(|k| logged(&events, one, k).is_some())
            });
            assert_eq!(
                logged(&events, one, "network"),
                Some(json!({ "ok": 0, "failed": 2 })),
                "two failed, none reached"
            );
            assert_eq!(
                logged(&events, one, "other"),
                Some(json!("failed")),
                "another page's origin"
            );
            assert_eq!(
                logged(&events, one, "naturalWidth"),
                Some(json!(0)),
                "an image from the web"
            );
        }

        /// The view's handshake carries the theme and the inline display mode, the theme's
        /// variable is the view's own computed value, and a height asked for is capped: 5000 to
        /// the cap, 300 kept. Each new height brings a snapshot, BGRA, its time measured.
        #[expect(clippy::print_stdout, reason = "the snapshot's time, for MEASUREMENTS.md")]
        pub(super) fn a_page_takes_the_theme_and_its_height_is_capped() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            assert!((host.cap() - 720.0).abs() < 1e-9, "0.8 of 1000 is past 720");
            let sized = |height: u32| {
                format!(
                    "handshake().then((answer) => {{ const context = answer.result.hostContext; \
                     report({{ handshake: {{ theme: context.theme, displayMode: context.displayMode, \
                       maxHeight: context.containerDimensions.maxHeight }}, \
                       css: getComputedStyle(document.documentElement) \
                         .getPropertyValue('--color-background-primary').trim() }}); \
                     parent.postMessage({{ jsonrpc: '2.0', method: 'ui/notifications/size-changed', \
                       params: {{ width: 600, height: {height} }} }}, '*'); }});"
                )
            };
            let tall = host.open(page("tall", &sized(5000)));
            let short = host.open(page("short", &sized(300)));
            let _changes = host.mount(tall);
            let _changes = host.mount(short);
            view_loaded(&host, &events, &[tall, short]);
            let resized = |page: PageId| {
                events.borrow().iter().find_map(|(of, event)| match event {
                    PageEvent::Resized { asked, height } if *of == page => Some((*asked, *height)),
                    _ => None,
                })
            };
            until("both pages resize", || resized(tall).is_some() && resized(short).is_some());
            for page in [tall, short] {
                assert_eq!(
                    logged(&events, page, "handshake"),
                    Some(json!({ "theme": "dark", "displayMode": "inline", "maxHeight": 720 })),
                    "the host's context"
                );
                assert_eq!(
                    logged(&events, page, "css"),
                    Some(json!(BACKGROUND)),
                    "the theme's hex"
                );
            }
            let (asked, height) = resized(tall).unwrap();
            assert!(
                (asked - 5000.0).abs() < 1e-9 && (height - 720.0).abs() < 1e-9,
                "{asked} → {height}"
            );
            let (asked, height) = resized(short).unwrap();
            assert!(
                (asked - 300.0).abs() < 1e-9 && (height - 300.0).abs() < 1e-9,
                "{asked} → {height}"
            );
            assert!(host.height(short).is_some_and(|h| (h - 300.0).abs() < 1e-9), "300 kept");

            until("a snapshot of the short page", || host.last_snapshot(short).is_some());
            let mut took = Vec::new();
            for _round in 0..20 {
                let before = count(&events, short, |e| matches!(e, PageEvent::Snapshot(_)));
                assert!(host.snapshot(short), "mounted");
                until("the snapshot", || {
                    count(&events, short, |e| matches!(e, PageEvent::Snapshot(_))) > before
                });
                let snapshot = host.last_snapshot(short).unwrap();
                let pixels = usize::try_from(snapshot.width)
                    .unwrap()
                    .saturating_mul(usize::try_from(snapshot.height).unwrap());
                assert!(pixels > 0, "{snapshot:?}");
                assert_eq!(snapshot.bgra.len(), pixels.saturating_mul(4), "four bytes a pixel");
                took.push(snapshot.took);
            }
            took.sort();
            let snapshot = host.last_snapshot(short).unwrap();
            println!(
                "page snapshot {}x{} px: p50 {:?}, p90 {:?}, max {:?} over {} takes",
                snapshot.width,
                snapshot.height,
                took.get(took.len() / 2).copied().unwrap_or_default(),
                took.get(took.len().saturating_mul(9) / 10).copied().unwrap_or_default(),
                took.last().unwrap(),
                took.len()
            );
        }

        /// A view sending its own frame to the web is refused: the host counts one refused
        /// navigation, and the view's document never loads a second time.
        pub(super) fn a_page_cannot_navigate_away() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let script = "window.addEventListener('load', () => setTimeout(() => { \
                location = 'https://example.com/away'; }, 50));";
            let one = host.open(page("leaving", script));
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[one]);
            until("the navigation is refused", || {
                host.facts(one).is_some_and(|f| f.navigations_refused >= 1)
            });
            settle(500);
            let facts = host.facts(one).unwrap();
            assert_eq!(facts.navigations_refused, 1, "{facts:?}");
            assert_eq!(facts.view_loads, 1, "the view loaded once: {facts:?}");
            let refused = count(
                &events,
                one,
                |e| matches!(e, PageEvent::Refused(Refusal::Navigation { url }) if url.starts_with("https://example.com")),
            );
            assert_eq!(refused, 1, "one refusal, named for where it went");
        }

        /// A page fetching from a server on this Mac fails until the app grants it that origin;
        /// then it reaches it, once, and a page granted nothing still does not.
        pub(super) fn a_page_reaches_the_network_only_once_granted() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let hits = Arc::new(AtomicUsize::new(0));
            let served = Arc::clone(&hits);
            // Each connection on its own thread, and a hit is a connection that asked: WebKit may
            // open one it sends nothing on.
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { return };
                    let served = Arc::clone(&served);
                    std::thread::spawn(move || {
                        let mut asked = [0_u8; 4096];
                        if stream.read(&mut asked).is_ok_and(|n| n > 0) {
                            served.fetch_add(1, Ordering::Relaxed);
                        }
                        let said = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                            Access-Control-Allow-Origin: *\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
                        let _wrote = stream.write_all(said.as_bytes());
                    });
                }
            });
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let script = format!(
                "fetch('http://127.0.0.1:{port}/x').then((r) => r.text()) \
                 .then((text) => report({{ fetched: text }}), () => report({{ fetched: 'failed' }}));"
            );
            let granted = host.open(page("granted", &script));
            let closed = host.open(page("closed", &script));
            let _changes = host.mount(granted);
            view_loaded(&host, &events, &[granted]);
            until("the first fetch", || logged(&events, granted, "fetched").is_some());
            assert_eq!(
                logged(&events, granted, "fetched"),
                Some(json!("failed")),
                "before the grant"
            );
            assert_eq!(hits.load(Ordering::Relaxed), 0, "nothing reached the server");

            let network = Network {
                connect: vec![format!("http://127.0.0.1:{port}")],
                resources: Vec::new(),
            };
            host.set_network(granted, &network).unwrap();
            until("the grant", || {
                count(&events, granted, |e| matches!(e, PageEvent::NetworkSet(1))) == 1
            });
            until("the fetch once granted", || {
                logged(&events, granted, "fetched") == Some(json!("hi"))
            });
            assert_eq!(hits.load(Ordering::Relaxed), 1, "one fetch reached the server");

            let _changes = host.mount(closed);
            view_loaded(&host, &events, &[closed]);
            until("the other page's fetch", || logged(&events, closed, "fetched").is_some());
            assert_eq!(
                logged(&events, closed, "fetched"),
                Some(json!("failed")),
                "granted nothing"
            );
            assert_eq!(hits.load(Ordering::Relaxed), 1, "a grant is one page's");
            assert!(
                host.set_network(
                    closed,
                    &Network { connect: vec!["file:///etc".to_owned()], resources: Vec::new() }
                )
                .is_err(),
                "a file is no origin"
            );
        }

        /// What would act for the person (a tool call, a message, context for the model) reaches
        /// the app unanswered; the view hears only what the app answers. A method off the
        /// allowlist is refused at once, and a notice sent before the handshake waits for it.
        pub(super) fn a_views_requests_wait_for_the_app() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let script = "window.addEventListener('message', (event) => { \
                  if (event.data && event.data.method === 'ui/notifications/tool-input') \
                    report({ toolInput: event.data.params }); }); \
                handshake().then(() => { \
                  request('tools/call', { name: 'echo', arguments: { v: 2 } }) \
                    .then((answer) => report({ called: answer.result })); \
                  request('ui/message', { role: 'user', content: [{ type: 'text', text: 'hi' }] }) \
                    .then((answer) => report({ message: answer.error.code })); \
                  request('ui/update-model-context', { content: [{ type: 'text', text: 'ctx' }] }); \
                  request('sampling/createMessage', {}).then((answer) => report({ refused: answer.error.code })); \
                });";
            let one = host.open(page("asking", script));
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[one]);
            assert!(
                host.notify(one, &Notice::Input(json!({ "arguments": { "v": 1 } }))),
                "mounted"
            );
            let asks = || {
                events
                    .borrow()
                    .iter()
                    .filter_map(|(of, event)| match event {
                        PageEvent::Asked(request) if *of == one => Some(request.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };
            until("three asks and a refusal", || {
                asks().len() == 3 && logged(&events, one, "refused").is_some()
            });
            assert_eq!(logged(&events, one, "refused"), Some(json!(-32601)), "method not found");
            assert_eq!(host.facts(one).unwrap().methods_refused, 1, "counted");
            assert_eq!(
                logged(&events, one, "toolInput"),
                Some(json!({ "arguments": { "v": 1 } })),
                "held for the handshake"
            );
            let asked = asks();
            assert!(asked.iter().all(|r| !r.activated), "no one acted in the page");
            let call = asked.iter().find(|r| matches!(&r.ask, Ask::CallTool { name, arguments } if name == "echo" && *arguments == json!({ "v": 2 }))).unwrap();
            let message = asked.iter().find(|r| matches!(&r.ask, Ask::Message { .. })).unwrap();
            assert!(
                asked.iter().any(|r| matches!(&r.ask, Ask::UpdateContext { .. })),
                "context for the model"
            );
            settle(300);
            assert_eq!(logged(&events, one, "called"), None, "the host answered nothing itself");

            let result = json!({ "content": [{ "type": "text", "text": "2" }] });
            assert!(host.answer(one, &call.id, Ok(result.clone())), "the call waits");
            assert!(!host.answer(one, &call.id, Ok(json!({}))), "answered once");
            assert!(
                host.answer(one, &message.id, Err(Declined::by_the_person())),
                "the message waits"
            );
            until("the view hears its answers", || {
                logged(&events, one, "called").is_some()
                    && logged(&events, one, "message").is_some()
            });
            assert_eq!(logged(&events, one, "called"), Some(result), "the app's answer");
            assert_eq!(logged(&events, one, "message"), Some(json!(-32000)), "declined");
        }

        /// At most eight pages are mounted and three live on a Mac, and each mounted page runs
        /// in a process of its own, whose memory is measured.
        #[expect(clippy::print_stdout, reason = "each page's memory, for MEASUREMENTS.md")]
        pub(super) fn pages_are_capped_and_each_has_its_own_process() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let all: Vec<PageId> =
                (0..9).map(|n| host.open(page(&format!("page {n}"), ""))).collect();
            for page in &all {
                let _changes = host.mount(*page);
            }
            assert_eq!(host.count(Presence::Warm), MOUNTED_CAP, "eight mounted");
            assert_eq!(
                host.presence(*all.first().unwrap()),
                Some(Presence::Cold),
                "the first cooled"
            );
            for page in all.iter().skip(4) {
                let _changes = host.engage(*page);
            }
            assert_eq!(host.count(Presence::Live), 3, "three live on a Mac");
            assert_eq!(host.count(Presence::Warm), MOUNTED_CAP.saturating_sub(3), "the rest warm");
            let mounted: Vec<PageId> =
                all.iter().copied().filter(|p| host.view(*p).is_some()).collect();
            assert_eq!(mounted.len(), MOUNTED_CAP, "a view each");
            view_loaded(&host, &events, &mounted);
            assert!(
                mounted.iter().all(|p| host.facts(*p).is_some_and(|f| f.view_loads == 1)),
                "each view loaded once"
            );
            let mut pids: Vec<i32> = mounted.iter().filter_map(|p| host.process(*p)).collect();
            assert_eq!(pids.len(), MOUNTED_CAP, "each page has a process");
            let mut footprints: Vec<f64> = pids
                .iter()
                .filter_map(|pid| slopty_testkit::process::usage(*pid))
                .map(|usage| {
                    #[expect(clippy::cast_precision_loss, reason = "a process's bytes, in MB")]
                    let mb = usage.footprint as f64 / 1_048_576.0;
                    mb
                })
                .collect();
            pids.sort_unstable();
            pids.dedup();
            assert_eq!(pids.len(), MOUNTED_CAP, "no two pages share a process");
            footprints.sort_by(f64::total_cmp);
            let total: f64 = footprints.iter().sum();
            println!(
                "page WebContent footprint over {} pages: min {:.1} MB, median {:.1} MB, max {:.1} MB, total {total:.1} MB",
                footprints.len(),
                footprints.first().unwrap(),
                footprints.get(footprints.len() / 2).copied().unwrap_or_default(),
                footprints.last().unwrap(),
            );
            let gone = *mounted.first().unwrap();
            let _changes = host.unmount(gone);
            assert!(host.view(gone).is_none() && host.process(gone).is_none(), "unmounted");
        }

        /// A fence's chart page, its libraries served from its own origin as the app serves
        /// them, and every file it asked for.
        fn chart(
            host: &PageHost,
            chart: slopty_platform::web::page::libs::Chart,
            source: &str,
        ) -> (PageId, Rc<RefCell<Vec<String>>>) {
            use slopty_platform::web::page::libs;
            let asked: Rc<RefCell<Vec<String>>> = Rc::default();
            let seen = Rc::clone(&asked);
            let resources: slopty_platform::web::page::Resources =
                Rc::new(move |path: &str, responder: slopty_platform::web::page::Responder| {
                    seen.borrow_mut().push(path.to_owned());
                    responder.answer(libs::lib(path));
                });
            let html = libs::fence_document(chart, source).unwrap();
            let id = host.open(PageSpec {
                title: "Chart".to_owned(),
                html,
                server: false,
                resources: Some(resources),
            });
            (id, asked)
        }

        /// A `vega-lite` fence draws from the vendored libraries alone: its view holds the
        /// source's rows (`view.data('source').length`), the page reaches nothing outside its
        /// own origin (no load blocked, nothing refused), every file it asked for is a library
        /// under `.slopty/lib/`, and it asks for its own height.
        #[expect(clippy::print_stdout, reason = "the time to draw, for MEASUREMENTS.md")]
        pub(super) fn a_vega_lite_fence_draws_offline() {
            use slopty_platform::web::page::libs::{Chart, PREFIX};
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let rows: Vec<Value> =
                (10..17).map(|n| json!({ "region": format!("r{n}"), "p99": n })).collect();
            let spec = json!({
                "$schema": "https://vega.github.io/schema/vega-lite/v6.json",
                "datasets": { "source": rows },
                "data": { "name": "source" },
                "mark": "bar",
                "encoding": {
                    "x": { "field": "region", "type": "nominal" },
                    "y": { "field": "p99", "type": "quantitative" },
                },
            });
            let (one, asked) = chart(&host, Chart::VegaLite, &spec.to_string());
            let started = Instant::now();
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[one]);
            until("the chart reports", || logged(&events, one, "chart").is_some());
            println!("MEASURE vega-lite fence → drawn: {:?}", started.elapsed());
            assert_eq!(logged(&events, one, "error"), None, "drawn");
            let data = logged(&events, one, "data").unwrap();
            assert_eq!(data.get("source"), Some(&json!(7)), "view.data('source').length: {data}");
            let facts = host.facts(one).unwrap();
            assert_eq!(facts.blocked, 0, "no load blocked: {facts:?}");
            let refused = count(&events, one, |e| matches!(e, PageEvent::Refused(_)));
            assert_eq!(refused, 0, "nothing refused");
            let asked = asked.borrow();
            assert_eq!(asked.len(), 4, "four libraries: {asked:?}");
            assert!(asked.iter().all(|path| path.starts_with(PREFIX)), "{asked:?}");
            until("its height", || {
                count(&events, one, |e| matches!(e, PageEvent::Resized { .. })) > 0
            });
        }

        /// A `plotly` fence draws from the vendored library alone: each trace has its points,
        /// and the page reaches nothing outside its own origin.
        #[expect(clippy::print_stdout, reason = "the time to draw, for MEASUREMENTS.md")]
        pub(super) fn a_plotly_fence_draws_offline() {
            use slopty_platform::web::page::libs::Chart;
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let figure = json!({
                "data": [
                    { "type": "scatter", "x": [1, 2, 3, 4, 5], "y": [2, 3, 5, 7, 11] },
                    { "type": "bar", "x": ["a", "b", "c"], "y": [3, 1, 2] },
                ],
                "layout": { "title": { "text": "Primes" } },
            });
            let (one, asked) = chart(&host, Chart::Plotly, &figure.to_string());
            let started = Instant::now();
            let _changes = host.mount(one);
            view_loaded(&host, &events, &[one]);
            until("the chart reports", || logged(&events, one, "chart").is_some());
            println!("MEASURE plotly fence → drawn: {:?}", started.elapsed());
            assert_eq!(logged(&events, one, "error"), None, "drawn");
            assert_eq!(logged(&events, one, "traces"), Some(json!([5, 3])), "each trace's points");
            assert_eq!(host.facts(one).unwrap().blocked, 0, "no load blocked");
            let refused = count(&events, one, |e| matches!(e, PageEvent::Refused(_)));
            assert_eq!(refused, 0, "nothing refused");
            assert_eq!(
                *asked.borrow(),
                [".slopty/lib/plotly-cartesian.min.js"],
                "the library alone"
            );
        }

        /// A page moved to the pane is the same document: shown full there and given the
        /// pane's width, its `performance.timeOrigin` is the one it loaded with, and its view
        /// loaded once.
        pub(super) fn a_page_moved_to_the_pane_keeps_its_document() {
            let events: Events = Rc::default();
            let host = host(Device::Mac, &events);
            let script = "window.addEventListener('message', (event) => { \
                  const m = event.data; \
                  if (m && m.method === 'ui/notifications/host-context-changed' \
                      && m.params.displayMode) \
                    report({ moved: { origin: performance.timeOrigin, mode: m.params.displayMode } }); }); \
                handshake().then(() => report({ loaded: performance.timeOrigin }));";
            let one = host.open(page("moving", script));
            let _changes = host.mount(one);
            let _changes = host.engage(one);
            view_loaded(&host, &events, &[one]);
            until("it loads", || logged(&events, one, "loaded").is_some());
            let loaded = logged(&events, one, "loaded").unwrap();
            host.set_display_mode(one, "fullscreen");
            host.resize(one, 900.0);
            until("it hears the move", || logged(&events, one, "moved").is_some());
            let moved = logged(&events, one, "moved").unwrap();
            assert_eq!(moved.get("mode"), Some(&json!("fullscreen")), "shown full");
            assert_eq!(moved.get("origin"), Some(&loaded), "the same document");
            assert_eq!(host.facts(one).unwrap().view_loads, 1, "loaded once");
            host.set_display_mode(one, "inline");
            settle(200);
            assert_eq!(host.facts(one).unwrap().view_loads, 1, "and back, still once");
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
        if mac::as_other_process() {
            return;
        }
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
