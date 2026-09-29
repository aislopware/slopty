//! The quick terminal's window as AppKit holds it: a borderless panel along the top of the
//! screen under the pointer, shown over every app without bringing this one forward.
//!
//! GPUI opens the window as a `WindowKind::PopUp`: an `NSPanel` with the non-activating style,
//! on every Space and beside a full-screen app. [`Panel::dress`] takes its title bar off and
//! makes it clear where the app draws nothing, so the terminal can slide in over the desktop;
//! [`Panel::place`], [`Panel::show`] and [`Panel::hide`] move it to a screen and in and out of
//! view. The session in it never moves: hiding orders the window out, and GPUI keeps it.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSColor, NSEvent, NSScreen, NSView, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

/// The panel's window. Made, used and dropped on the main thread.
#[derive(Clone, Debug)]
pub struct Panel {
    window: Retained<NSWindow>,
}

impl Panel {
    /// The window of `view`, the `NSView` a GPUI window draws into (its `raw_window_handle`
    /// AppKit handle). `None` off the main thread or before the view is in a window.
    #[must_use]
    pub fn of_view(view: NonNull<c_void>) -> Option<Self> {
        MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; retained here, it outlives any close.
        let view: Retained<NSView> = unsafe { Retained::retain(view.as_ptr().cast::<NSView>()) }?;
        view.window().map(|window| Self { window })
    }

    /// Take the title bar off, show the desktop where nothing is drawn, keep it on screen
    /// while another app is in front, out of ⌘\` and Mission Control, and with no zoom of
    /// AppKit's own when it appears (the app draws its slide).
    pub fn dress(&self) {
        let w = &self.window;
        w.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
        w.setOpaque(false);
        w.setBackgroundColor(Some(&NSColor::clearColor()));
        w.setHasShadow(true);
        w.setHidesOnDeactivate(false);
        w.setMovable(false);
        w.setAnimationBehavior(NSWindowAnimationBehavior::None);
        w.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle
                | NSWindowCollectionBehavior::Transient,
        );
    }

    /// Put the panel along the top of the screen under the pointer, below its menu bar, the
    /// screen's width and `height` of its height (0 to 1). Returns its size in points.
    #[must_use]
    pub fn place(&self, height: f64) -> Option<(f64, f64)> {
        let frame = top_band(pointer_screen()?.visibleFrame(), height);
        self.window.setFrame_display(frame, true);
        Some((frame.size.width, frame.size.height))
    }

    /// Order the panel in front of every app and, with `keyboard`, give it the keyboard,
    /// leaving whichever app is active active.
    pub fn show(&self, keyboard: bool) {
        self.window.orderFrontRegardless();
        if keyboard {
            self.window.makeKeyWindow();
        }
    }

    /// Order the panel out. The keyboard goes back to whoever had it.
    pub fn hide(&self) {
        self.window.orderOut(None);
    }

    /// Draw the window's shadow again from what the window shows now: AppKit keeps the shadow
    /// of a clear window from when it was last worked out.
    pub fn refresh_shadow(&self) {
        self.window.invalidateShadow();
    }

    /// Whether the panel is on screen.
    #[must_use]
    pub fn is_shown(&self) -> bool {
        self.window.isVisible()
    }
}

/// The screen the pointer is on, or the main screen.
fn pointer_screen() -> Option<Retained<NSScreen>> {
    let mtm = MainThreadMarker::new()?;
    let at = NSEvent::mouseLocation();
    let within = |r: NSRect| {
        at.x >= r.origin.x
            && at.x < r.origin.x + r.size.width
            && at.y >= r.origin.y
            && at.y < r.origin.y + r.size.height
    };
    NSScreen::screens(mtm)
        .iter()
        .find(|screen| within(screen.frame()))
        .or_else(|| NSScreen::mainScreen(mtm))
}

/// The band `height` (0 to 1) of `visible` tall along its top, in AppKit's bottom-up points.
fn top_band(visible: NSRect, height: f64) -> NSRect {
    let h = (visible.size.height * height.clamp(0.0, 1.0)).round().max(1.0);
    NSRect::new(
        NSPoint::new(visible.origin.x, visible.origin.y + visible.size.height - h),
        NSSize::new(visible.size.width, h),
    )
}

#[cfg(test)]
mod tests {
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    /// The band hangs from the top of the visible frame (below the menu bar), the screen's
    /// width, on a second screen as on the first.
    #[test]
    fn the_band_hangs_from_the_top_of_the_visible_frame() {
        let visible = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 945.0));
        let band = super::top_band(visible, 0.4);
        assert_eq!((band.origin.x, band.origin.y), (0.0, 567.0));
        assert_eq!((band.size.width, band.size.height), (1512.0, 378.0));
        let side = NSRect::new(NSPoint::new(1512.0, -300.0), NSSize::new(2560.0, 1415.0));
        let band = super::top_band(side, 1.0);
        assert_eq!((band.origin.y, band.size.height), (-300.0, 1415.0), "all of it");
        assert_eq!(super::top_band(side, 7.0), band, "past the screen is the screen");
    }
}
