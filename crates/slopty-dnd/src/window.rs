//! The helper's windows: a few points at a global point, above every app's, in an accessory
//! application that is never made active.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSWindow,
    NSWindowStyleMask,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGMainDisplayID, CGWindowLevelForKey, CGWindowLevelKey,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// The helper's application: an accessory, with no Dock icon or menu bar, which this crate never
/// activates. Never `prohibited`, which has hung on pasteboard access (FB17775671).
#[must_use]
pub fn application(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    let _set = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    app
}

/// The main display's height in points: global points run down from its top left, Cocoa's up
/// from its bottom left.
fn main_height() -> f64 {
    CGDisplayBounds(CGMainDisplayID()).size.height
}

/// A Cocoa screen point as a global point, from the main display's top left.
#[must_use]
pub fn global(at: NSPoint) -> (f64, f64) {
    (at.x, main_height() - at.y)
}

/// The frame of a `side`-point square centred on the global point `at`, in Cocoa's screen
/// coordinates.
fn frame((x, y): (f64, f64), side: f64) -> NSRect {
    NSRect {
        origin: NSPoint { x: x - side / 2.0, y: main_height() - y - side / 2.0 },
        size: NSSize { width: side, height: side },
    }
}

/// A borderless window of `side` points, all but invisible, at pop-up menu level so it sits
/// over whatever app is under the point, taking the mouse even where it is clear. Out of sight
/// until placed.
pub(crate) fn square(mtm: MainThreadMarker, side: f64) -> Retained<NSWindow> {
    // SAFETY: the designated `NSWindow` initialiser; every argument is a plain value and the
    // window is made and used on the main thread (AppKit, `NSWindow`).
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame((0.0, 0.0), side),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: `false` stops AppKit freeing a window this code still holds (AppKit,
    // `NSWindow.isReleasedWhenClosed`).
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window.setOpaque(false);
    // Clear enough to see nothing through, not so clear that the window server passes the
    // mouse through it.
    window.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.02)));
    window.setHasShadow(false);
    window.setIgnoresMouseEvents(false);
    window.setLevel(CGWindowLevelForKey(CGWindowLevelKey::PopUpMenuWindowLevelKey) as isize);
    window
}

/// Put `window`, a `side`-point square, centred on the global point `at` and in front, without
/// making the application active.
pub(crate) fn place(window: &NSWindow, at: (f64, f64), side: f64) {
    window.setFrame_display(frame(at, side), false);
    window.orderFrontRegardless();
}
