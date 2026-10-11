//! A stand-in for an app's window, for the display holder's live measurements
//! (`apps/slopty-worker/tests/holder.rs`).
//!
//! `slopty-stub-window <display> <width> <height>` waits up to 10 s for the screen of display
//! `<display>` (a `CGDirectDisplayID`), then puts a titled, resizable AppKit window of
//! `<width>` × `<height>` points on it, near its top left corner. From then on it handles
//! AppKit's events, and prints a line each time the window's frame or screen changes:
//! `frame <screen> <x> <y> <width> <height>`, the screen as its display id (0 for none) and
//! the frame in points from that screen's bottom-left corner (AppKit's global points when it
//! has none). Read in the same turn as the screen's own frame, the place holds while macOS
//! lays the displays out again around it, as it does when another display comes or goes. It
//! runs until it is killed. It fails on any other platform.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "stdout is its report; stderr its log"
)]
#![allow(
    clippy::multiple_unsafe_ops_per_block,
    reason = "each block is a short run of AppKit messages under the one rule its comment states"
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match args.as_slice() {
        [display, width, height] => {
            display.parse::<u32>().ok().zip(width.parse::<f64>().ok()).zip(height.parse().ok())
        }
        _ => None,
    };
    let Some(((display, width), height)) = parsed else {
        eprintln!("usage: slopty-stub-window <display> <width> <height>");
        return ExitCode::FAILURE;
    };
    run(display, width, height)
}

#[cfg(not(target_os = "macos"))]
fn run(_display: u32, _width: f64, _height: f64) -> ExitCode {
    eprintln!("slopty-stub-window: an AppKit window needs macOS");
    ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn run(display: u32, width: f64, height: f64) -> ExitCode {
    mac::run(display, width, height)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    // Links AppKit, whose classes (`NSApplication`, `NSWindow`, `NSScreen`) `class!` looks up by
    // name. It declares no item.
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    /// How long the display's screen is waited for.
    const SCREEN_WAIT: Duration = Duration::from_secs(10);

    /// How long one turn of the event loop waits for an event, in seconds.
    const TURN: f64 = 0.01;

    /// `NSApplicationActivationPolicyRegular` (`AppKit/NSRunningApplication.h`): an ordinary
    /// app, as the windows a remote desktop shows belong to.
    const REGULAR: isize = 0;

    /// `NSWindowStyleMaskTitled | Closable | Miniaturizable | Resizable` (`AppKit/NSWindow.h`).
    const STYLE: usize = 1 | 2 | 4 | 8;

    /// `NSBackingStoreBuffered` (`AppKit/NSGraphics.h`).
    const BUFFERED: usize = 2;

    /// `NSEventMaskAny` (`AppKit/NSEvent.h`).
    const ANY_EVENT: u64 = u64::MAX;

    pub fn run(display: u32, width: f64, height: f64) -> ExitCode {
        // SAFETY: AppKit's documented messages, sent on the main thread (a binary's `main`)
        // with the argument and return types their headers declare. The objects are never
        // released: they live as long as the process.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _regular: bool = msg_send![app, setActivationPolicy: REGULAR];
            let () = msg_send![app, finishLaunching];

            let asked = Instant::now();
            let screen = loop {
                if let Some(screen) = screen_of(display) {
                    break screen;
                }
                if asked.elapsed() >= SCREEN_WAIT {
                    eprintln!("slopty-stub-window: no screen for display {display}");
                    return ExitCode::FAILURE;
                }
                turn(app);
            };
            // The content rectangle is relative to the screen's bottom-left corner when the
            // screen is named (`initWithContentRect:styleMask:backing:defer:screen:`); without
            // it, AppKit places and keeps the window on the main screen.
            let on: NSRect = msg_send![screen, frame];
            let origin = NSPoint::new(20.0, on.size.height - height - 60.0);
            let content = NSRect::new(origin, NSSize::new(width, height));
            let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
            let window: *mut AnyObject = msg_send![
                window,
                initWithContentRect: content,
                styleMask: STYLE,
                backing: BUFFERED,
                defer: false,
                screen: screen
            ];
            let () = msg_send![window, setReleasedWhenClosed: false];
            let title = NSString::from_str("slopty-stub-window");
            let () = msg_send![window, setTitle: &*title];
            let () = msg_send![window, orderFrontRegardless];
            watch(app, window)
        }
    }

    /// Print `window`'s frame and screen each time they change, turning the event loop between
    /// looks, until the process is killed.
    ///
    /// # Safety
    ///
    /// On the main thread, with `app` the shared application and `window` one of its windows.
    unsafe fn watch(app: *mut AnyObject, window: *mut AnyObject) -> ! {
        let mut said = String::new();
        loop {
            // SAFETY: as the caller vouches; `screen` and `frame` are `NSWindow`'s documented
            // getters and `frame` `NSScreen`'s, with their declared types.
            let line = unsafe {
                let screen: *mut AnyObject = msg_send![window, screen];
                let frame: NSRect = msg_send![window, frame];
                let corner = if screen.is_null() {
                    NSPoint::new(0.0, 0.0)
                } else {
                    let on: NSRect = msg_send![screen, frame];
                    on.origin
                };
                let (x, y, size) =
                    (frame.origin.x - corner.x, frame.origin.y - corner.y, frame.size);
                let number = number_of(screen);
                format!("frame {number} {x} {y} {} {}", size.width, size.height)
            };
            if line != said {
                println!("{line}");
                let _flushed = std::io::stdout().flush();
                said = line;
            }
            // SAFETY: as the caller vouches.
            unsafe {
                turn(app);
            }
        }
    }

    /// One turn of the event loop: the next event, waited for up to [`TURN`], handed to AppKit.
    ///
    /// # Safety
    ///
    /// On the main thread, with `app` the shared application.
    unsafe fn turn(app: *mut AnyObject) {
        // SAFETY: as the caller vouches; the messages are `NSDate`'s and `NSApplication`'s
        // documented ones, with their declared types.
        unsafe {
            let until: *mut AnyObject =
                msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: TURN];
            let mode = NSString::from_str("kCFRunLoopDefaultMode");
            let event: *mut AnyObject = msg_send![
                app,
                nextEventMatchingMask: ANY_EVENT,
                untilDate: until,
                inMode: &*mode,
                dequeue: true
            ];
            if !event.is_null() {
                let () = msg_send![app, sendEvent: event];
            }
        }
    }

    /// The screen showing display `display`, when AppKit has one.
    ///
    /// # Safety
    ///
    /// On the main thread, once the shared application is made.
    unsafe fn screen_of(display: u32) -> Option<*mut AnyObject> {
        // SAFETY: as the caller vouches; `NSScreen.screens` is an array of screens, each read
        // by index below its count.
        unsafe {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            (0..count)
                .map(|at| -> *mut AnyObject { msg_send![screens, objectAtIndex: at] })
                .find(|screen| number_of(*screen) == display)
        }
    }

    /// The display id of `screen` (its `NSScreenNumber`), 0 for none.
    ///
    /// # Safety
    ///
    /// `screen` is null or an `NSScreen`.
    unsafe fn number_of(screen: *mut AnyObject) -> u32 {
        if screen.is_null() {
            return 0;
        }
        // SAFETY: as the caller vouches; a screen's device description holds its number as an
        // `NSNumber` under `NSScreenNumber` (`AppKit/NSScreen.h`).
        unsafe {
            let described: *mut AnyObject = msg_send![screen, deviceDescription];
            let key = NSString::from_str("NSScreenNumber");
            let number: *mut AnyObject = msg_send![described, objectForKey: &*key];
            if number.is_null() {
                return 0;
            }
            msg_send![number, unsignedIntValue]
        }
    }
}
