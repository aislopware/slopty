//! What the two test apps share: their arguments, one window at a place in global points, the
//! ready line, one stdout line per callback, and leaving when the parent closes stdin. The
//! shape is the gesture app's (`crates/slopty-input/tests/support/gesture_app.rs`).

use std::collections::HashMap;
use std::io::Write as _;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
    NSEventMask, NSEventType, NSWindow, NSWindowStyleMask,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGMainDisplayID, CGWindowLevelForKey, CGWindowLevelKey,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// One line on stdout, flushed: the parent reads them as they come.
pub fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _written = writeln!(out, "{line}").and_then(|()| out.flush());
}

/// `--key value` pairs, a key given more than once keeping every value.
#[derive(Debug, Default)]
pub struct Args(HashMap<String, Vec<String>>);

impl Args {
    /// This process's arguments.
    pub fn read() -> Self {
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            let Some(key) = key.strip_prefix("--") else { continue };
            let value = args.next().unwrap_or_default();
            map.entry(key.to_owned()).or_default().push(value);
        }
        Self(map)
    }

    /// The last value of `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.last()).map(String::as_str)
    }

    /// Every value of `key`, in order.
    pub fn all(&self, key: &str) -> &[String] {
        self.0.get(key).map_or(&[], Vec::as_slice)
    }

    /// Whether `key` is `1`.
    pub fn on(&self, key: &str) -> bool {
        self.get(key) == Some("1")
    }

    /// `--at x,y,w,h`: a rectangle in global points from the main display's top left.
    pub fn at(&self) -> (f64, f64, f64, f64) {
        let parts: Vec<f64> = self
            .get("at")
            .unwrap_or("0,0,100,100")
            .split(',')
            .filter_map(|v| v.parse().ok())
            .collect();
        match parts.as_slice() {
            [x, y, w, h] => (*x, *y, *w, *h),
            _ => (0.0, 0.0, 100.0, 100.0),
        }
    }
}

/// An accessory application: no Dock icon, no menu bar, never made active by this code.
pub fn application(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    app
}

/// A window whose content is at `(x, y, w, h)` in global points from the main display's top
/// left, filled with `colour`, at the level `level` names (`normal`, `floating` or `popup`),
/// ordered in without making the app active. Borderless, or `titled` (a title bar above the
/// content, and so a window that can become key, as an app's document window can).
pub fn window(
    mtm: MainThreadMarker,
    (x, y, w, h): (f64, f64, f64, f64),
    colour: &NSColor,
    level: &str,
    titled: bool,
) -> Retained<NSWindow> {
    // Cocoa places windows from the main display's bottom left, CoreGraphics from its top left.
    let main = CGDisplayBounds(CGMainDisplayID());
    let frame = NSRect {
        origin: NSPoint { x, y: main.size.height - y - h },
        size: NSSize { width: w, height: h },
    };
    // SAFETY: the designated `NSWindow` initialiser; every argument is a plain value and the
    // window is created and used on the main thread (AppKit, `NSWindow`).
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            if titled {
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable
            } else {
                NSWindowStyleMask::Borderless
            },
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: `false` stops AppKit freeing a window this code still holds (AppKit,
    // `NSWindow.isReleasedWhenClosed`).
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window.setBackgroundColor(Some(colour));
    window.setHasShadow(false);
    let key = match level {
        "popup" => CGWindowLevelKey::PopUpMenuWindowLevelKey,
        "floating" => CGWindowLevelKey::FloatingWindowLevelKey,
        _ => CGWindowLevelKey::NormalWindowLevelKey,
    };
    window.setLevel(CGWindowLevelForKey(key) as isize);
    window.orderFrontRegardless();
    window
}

/// Say `ready pid=… window=…` and the rest, then leave when the parent closes stdin or goes.
pub fn ready(window: &NSWindow, rest: &str) {
    say(&format!("ready pid={} window={}{rest}", std::process::id(), window.windowNumber()));
    std::thread::spawn(|| {
        let _read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        #[expect(clippy::exit, reason = "a test app ends when its parent lets go of it")]
        std::process::exit(0);
    });
}

/// Say every mouse press, drag and release the application takes off its queue, with the
/// event number AppKit follows a press by: `event type=… number=… x=… y=…`. The monitor lasts
/// while what this returns is held.
pub fn log_presses() -> Option<Retained<AnyObject>> {
    let mask =
        NSEventMask::LeftMouseDown | NSEventMask::LeftMouseDragged | NSEventMask::LeftMouseUp;
    let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit rule: the monitor gets a valid event for the call.
        let seen = unsafe { event.as_ref() };
        let at = seen.locationInWindow();
        let kind = seen.r#type();
        if [NSEventType::LeftMouseDown, NSEventType::LeftMouseDragged, NSEventType::LeftMouseUp]
            .contains(&kind)
        {
            say(&format!(
                "event type={} number={} x={:.0} y={:.0}",
                kind.0,
                seen.eventNumber(),
                at.x,
                at.y
            ));
        }
        event.as_ptr()
    });
    // SAFETY: AppKit rule: the handler returns the event it was given, which stays valid, so
    // the application dispatches it as usual.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) }
}
