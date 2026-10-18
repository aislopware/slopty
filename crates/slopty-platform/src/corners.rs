//! The screen's corners while a remote screen has the keyboard (macOS 27).
//!
//! A remote Mac's screen shown full screen reaches to this Mac's corners, so the pointer runs
//! into them on the way to the remote's Apple menu or its own hot corners, and this Mac's hot
//! corners (Quick Note in the bottom right by default) open over the stream. macOS 27 turns the
//! corners off for the frontmost app while its Dock is hidden
//! (`NSApplicationPresentationDisableScreenCornerInteractions`), which full screen does. They go
//! off only while a remote screen has the keyboard, and the person's corners work everywhere
//! else, this app's own chats included.

use std::cell::Cell;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions};

/// `NSApplicationPresentationDisableScreenCornerInteractions` (`AppKit/NSApplication.h`, macOS
/// 27), which objc2-app-kit 0.3.2 predates.
pub const CORNERS_OFF: NSApplicationPresentationOptions = NSApplicationPresentationOptions(1 << 15);

thread_local! {
    /// Whether a remote screen has the keyboard: main thread only, as `NSApp` is.
    static REMOTE: Cell<bool> = const { Cell::new(false) };
}

/// A remote screen has the keyboard (`remote`), or none does: the corners go off or come back
/// as [`with_corners`] says. Main thread only; a no-op off it (a headless test's thread).
pub fn remote_keyboard(remote: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    REMOTE.with(|r| r.set(remote));
    follow(mtm);
}

/// The window went in or out of full screen: the Dock's state the corners depend on moved.
pub fn window_moved() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if REMOTE.with(Cell::get) {
        follow(mtm);
    }
}

fn follow(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    let now = app.presentationOptions();
    let wanted = with_corners(now, REMOTE.with(Cell::get));
    if wanted != now {
        app.setPresentationOptions(wanted);
    }
}

/// The options `now` with the corners off while `remote` and the Dock is hidden, which the
/// option needs (`NSApplication.h`), and on otherwise.
#[must_use]
pub fn with_corners(
    now: NSApplicationPresentationOptions,
    remote: bool,
) -> NSApplicationPresentationOptions {
    let docked =
        NSApplicationPresentationOptions::AutoHideDock | NSApplicationPresentationOptions::HideDock;
    if remote && now.intersects(docked) { now | CORNERS_OFF } else { now.difference(CORNERS_OFF) }
}

#[cfg(test)]
mod tests {
    use objc2_app_kit::NSApplicationPresentationOptions as O;

    use super::{CORNERS_OFF, with_corners};

    /// The corners go off only while a remote screen has the keyboard with the Dock hidden, and
    /// come back the moment either ends; nothing else in the options moves.
    #[test]
    fn the_corners_go_off_only_for_a_remote_screen_in_full_screen() {
        let full = O::FullScreen | O::AutoHideDock | O::AutoHideMenuBar;
        assert_eq!(with_corners(full, true), full | CORNERS_OFF);
        assert_eq!(with_corners(full | CORNERS_OFF, false), full);
        assert_eq!(with_corners(O::HideDock, true), O::HideDock | CORNERS_OFF);
        assert_eq!(with_corners(O::Default, true), O::Default, "a window: the Dock is there");
        assert_eq!(with_corners(O::AutoHideMenuBar | CORNERS_OFF, true), O::AutoHideMenuBar);
    }
}
