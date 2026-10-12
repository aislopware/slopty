//! A stand-in for an app's window, for the display holder's live measurements
//! (`apps/slopty-worker/tests/holder.rs`) and the live capture tests, which need a window of
//! their own that draws as a person's terminal or editor does, on any Mac, a runner's included.
//!
//! `slopty-stub-window <display> <width> <height> [<drawing>]` waits up to 10 s for the screen
//! of display `<display>` (a `CGDirectDisplayID`, or `main`), then puts a titled, resizable
//! AppKit window of `<width>` × `<height>` points on it, near its top left corner. What it draws
//! there is `<drawing>`:
//!
//! - `still` (the default): a page of text, drawn once, as a terminal running `sleep`;
//! - `scroll`: lines of text scrolling up each turn of its event loop, as `yes` in a terminal;
//! - `tick`: the page drawn again once a second, as `top -s 1`;
//! - `white`: bare white and nothing else, for a picture to be told from its padding.
//!
//! From then on it handles
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
    let (display, width, height, drawing) = match args.as_slice() {
        [display, width, height] => (display, width, height, "still"),
        [display, width, height, drawing] => (display, width, height, drawing.as_str()),
        _ => return usage(),
    };
    let display = match display.as_str() {
        "main" => None,
        id => match id.parse::<u32>() {
            Ok(id) => Some(id),
            Err(_) => return usage(),
        },
    };
    let (Ok(width), Ok(height), Some(drawing)) =
        (width.parse::<f64>(), height.parse::<f64>(), Drawing::named(drawing))
    else {
        return usage();
    };
    run(display, width, height, drawing)
}

fn usage() -> ExitCode {
    eprintln!("usage: slopty-stub-window <display|main> <width> <height> [<drawing>]");
    ExitCode::FAILURE
}

/// What the window draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drawing {
    /// A page of text, once.
    Still,
    /// Lines scrolling up each turn of the event loop.
    Scroll,
    /// The page again once a second.
    Tick,
    /// Bare white.
    White,
}

impl Drawing {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "still" => Self::Still,
            "scroll" => Self::Scroll,
            "tick" => Self::Tick,
            "white" => Self::White,
            _ => return None,
        })
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_display: Option<u32>, _width: f64, _height: f64, _drawing: Drawing) -> ExitCode {
    eprintln!("slopty-stub-window: an AppKit window needs macOS");
    ExitCode::FAILURE
}

#[cfg(target_os = "macos")]
fn run(display: Option<u32>, width: f64, height: f64, drawing: Drawing) -> ExitCode {
    mac::run(display, width, height, drawing)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::fmt::Write as _;
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use super::Drawing;

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

    pub fn run(display: Option<u32>, width: f64, height: f64, drawing: Drawing) -> ExitCode {
        // SAFETY: AppKit's documented messages, sent on the main thread (a binary's `main`)
        // with the argument and return types their headers declare. The objects are never
        // released: they live as long as the process.
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _regular: bool = msg_send![app, setActivationPolicy: REGULAR];
            let () = msg_send![app, finishLaunching];

            let asked = Instant::now();
            let screen = loop {
                let found = if let Some(display) = display {
                    screen_of(display)
                } else {
                    let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
                    (!main.is_null()).then_some(main)
                };
                if let Some(screen) = found {
                    break screen;
                }
                if asked.elapsed() >= SCREEN_WAIT {
                    eprintln!("slopty-stub-window: no screen for display {display:?}");
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
            let page = page(window, drawing);
            let () = msg_send![window, orderFrontRegardless];
            watch(app, window, page, drawing)
        }
    }

    /// Put what `drawing` shows in `window`: a text field filling its content for the drawings
    /// of text, which [`draw`] fills; white for [`Drawing::White`], with no field.
    ///
    /// # Safety
    ///
    /// On the main thread, with `window` an `NSWindow`.
    unsafe fn page(window: *mut AnyObject, drawing: Drawing) -> Option<*mut AnyObject> {
        // SAFETY: as the caller vouches; `NSColor`, `NSFont`, `NSTextField` and `NSWindow`'s
        // documented messages with the types their headers declare. The field lives as long as
        // the window, which holds it as its content view.
        unsafe {
            if drawing == Drawing::White {
                let white: *mut AnyObject = msg_send![class!(NSColor), whiteColor];
                let () = msg_send![window, setBackgroundColor: white];
                return None;
            }
            let empty = NSString::from_str("");
            let field: *mut AnyObject =
                msg_send![class!(NSTextField), wrappingLabelWithString: &*empty];
            let font: *mut AnyObject =
                msg_send![class!(NSFont), monospacedSystemFontOfSize: 12.0_f64, weight: 0.0_f64];
            let () = msg_send![field, setFont: font];
            let () = msg_send![window, setContentView: field];
            draw(field, 0);
            Some(field)
        }
    }

    /// Lines on a page of the stand-in's text.
    const LINES: u64 = 60;

    /// Fill `field` with the page that starts at line `top`.
    ///
    /// # Safety
    ///
    /// On the main thread, with `field` an `NSTextField`.
    unsafe fn draw(field: *mut AnyObject, top: u64) {
        let mut text = String::new();
        for n in top..top.saturating_add(LINES) {
            let _written = writeln!(text, "{n:>8}  the quick brown fox jumps over the lazy dog");
        }
        let text = NSString::from_str(&text);
        // SAFETY: as the caller vouches; `setStringValue:` is `NSControl`'s.
        unsafe {
            let () = msg_send![field, setStringValue: &*text];
        }
    }

    /// Print `window`'s frame and screen each time they change, turning the event loop between
    /// looks, until the process is killed.
    ///
    /// # Safety
    ///
    /// On the main thread, with `app` the shared application and `window` one of its windows.
    unsafe fn watch(
        app: *mut AnyObject,
        window: *mut AnyObject,
        page: Option<*mut AnyObject>,
        drawing: Drawing,
    ) -> ! {
        let mut said = String::new();
        let started = Instant::now();
        let (mut top, mut drawn_at) = (0_u64, 0_u64);
        loop {
            if let Some(field) = page {
                let now = match drawing {
                    Drawing::Scroll => top.saturating_add(1),
                    Drawing::Tick => started.elapsed().as_secs(),
                    Drawing::Still | Drawing::White => top,
                };
                if now != drawn_at {
                    // SAFETY: as the caller vouches; `field` is the window's text field.
                    unsafe {
                        draw(field, now);
                    }
                    drawn_at = now;
                }
                top = now;
            }
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
