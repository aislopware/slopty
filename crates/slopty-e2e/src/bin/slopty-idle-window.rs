//! A window that does nothing, for the refresh-storm guard.
//!
//! The app self-test needs a capture target that produces no frame at all: that is what puts the
//! host into `SourceState::Idle` and the item into its waiting state, and it is the only way to
//! see the receiver's refresh cap from outside. Nothing
//! already on the desktop can play the part — the window picker lists on-screen windows only, so
//! the target has to be on screen when it is listed and off screen when the stream opens, and no
//! window this session does not own may be touched.
//!
//! So it owns one. `slopty-idle-window <dir> <title> [x,y]` opens a small window with that title,
//! orders it in (without taking the keyboard: `orderFrontRegardless`, never `makeKey`) and then
//! watches `<dir>` for one-word markers:
//!
//! * `hide` — order the window out. It stays in ScreenCaptureKit's window list (the host enumerates
//!   with `onScreenWindowsOnly: false`) so a stream can still be opened on it, and that stream
//!   never sees a frame.
//! * `show` — order it back in and repaint it forever, which is the host's cue that the source is
//!   live again.
//! * `sibling` — open a second window of this same process beside the first, and `unsibling` —
//!   order that one out again. A window of the *same application* going away is what the host's
//!   accessibility hide watch cannot tell from the target going away, so this is how a false
//!   suspicion is produced on purpose.
//! * `popup` — open a borderless, non-activating panel at the pop-up menu level below the window,
//!   the shape of an autocomplete list or a tooltip, and `unpopup` — order it out. Whether the
//!   accessibility watch counts such a window's going as a suspicion is what decides how often a
//!   real editor would freeze the stream.
//! * `quit` — leave.
//!
//! It writes `ready` into `<dir>` once the window is up.
//!
//! The optional origin is for the crop-path test, which needs a second one of these directly
//! behind the target: while the target covers it the crop shows the target, and the moment the
//! target is ordered out the same rectangle is a window that keeps repainting — which is what
//! makes a leak from the crop something the host has a frame to leak.

#[cfg(target_os = "macos")]
fn main() -> Result<(), String> {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), String> {
    Err("slopty-idle-window is macOS only".to_owned())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};

    use objc2::{MainThreadMarker, MainThreadOnly as _};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSPanel,
        NSWindow, NSWindowLevel, NSWindowStyleMask,
    };
    use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize, NSString};

    /// How long the run loop is pumped between two looks at the marker directory.
    const TICK: f64 = 0.05;
    /// Content size in points. Small: it is on screen for as long as the picker takes to list it.
    const SIZE: NSSize = NSSize { width: 240.0, height: 160.0 };
    /// Where it opens by default, in screen points from the bottom left.
    const ORIGIN: NSPoint = NSPoint { x: 40.0, y: 40.0 };

    pub fn run() -> Result<(), String> {
        let mut args = std::env::args().skip(1);
        let (Some(dir), Some(title)) = (args.next(), args.next()) else {
            return Err("usage: slopty-idle-window <marker-dir> <title> [x,y]".to_owned());
        };
        let origin = match args.next() {
            None => ORIGIN,
            Some(pair) => {
                let (x, y) = pair.split_once(',').ok_or_else(|| format!("origin {pair}"))?;
                let read = |s: &str| s.parse::<f64>().map_err(|e| format!("origin {pair}: {e}"));
                NSPoint { x: read(x)?, y: read(y)? }
            }
        };
        let dir = PathBuf::from(dir);
        let mtm = MainThreadMarker::new().ok_or_else(|| "not on the main thread".to_owned())?;

        let app = NSApplication::sharedApplication(mtm);
        // Accessory: a window, no dock icon, and no chance of stealing the app's keyboard.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // The documented way to bring AppKit up without `NSApplication::run`: this process
        // pumps the main run loop itself below.
        app.finishLaunching();

        let frame = NSRect { origin, size: SIZE };
        // SAFETY: the designated `NSWindow` initialiser; every argument is a plain value and the
        // window is created and used on the main thread (AppKit, `NSWindow`).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(&title));
        // SAFETY: `setReleasedWhenClosed` only flips the window's own ownership flag, and
        // `false` is the safe direction — it stops AppKit freeing a window this code still holds
        // (AppKit, `NSWindow.isReleasedWhenClosed`).
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        window.setBackgroundColor(Some(&NSColor::blueColor()));
        window.orderFrontRegardless();
        pump();
        let _ready = std::fs::write(dir.join("ready"), b"");

        let mut shown = true;
        let mut ticks: u32 = 0;
        let mut sibling: Option<objc2::rc::Retained<NSWindow>> = None;
        let mut popup: Option<objc2::rc::Retained<NSPanel>> = None;
        loop {
            pump();
            if dir.join("quit").exists() {
                break;
            }
            if consume(&dir, "sibling") {
                let beside = NSRect {
                    origin: NSPoint { x: origin.x + SIZE.width + 40.0, y: origin.y },
                    size: SIZE,
                };
                // SAFETY: as for the first window: the designated initialiser with plain
                // values, on the main thread (AppKit, `NSWindow`).
                let second = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        NSWindow::alloc(mtm),
                        beside,
                        NSWindowStyleMask::Titled,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                second.setTitle(&NSString::from_str(&format!("{title} sibling")));
                // SAFETY: as above (AppKit, `NSWindow.isReleasedWhenClosed`).
                unsafe {
                    second.setReleasedWhenClosed(false);
                }
                second.setBackgroundColor(Some(&NSColor::redColor()));
                second.orderFrontRegardless();
                note(&dir, "sibling", second.isVisible());
                sibling = Some(second);
            }
            if consume(&dir, "unsibling") {
                if let Some(second) = sibling.take() {
                    second.orderOut(None);
                    note(&dir, "unsibling", second.isVisible());
                } else {
                    note(&dir, "unsibling", false);
                }
            }
            if consume(&dir, "popup") {
                let below = NSRect {
                    origin: NSPoint { x: origin.x, y: origin.y - 100.0 },
                    size: NSSize { width: 200.0, height: 60.0 },
                };
                let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                    NSPanel::alloc(mtm),
                    below,
                    NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                    NSBackingStoreType::Buffered,
                    false,
                );
                // SAFETY: as above (AppKit, `NSWindow.isReleasedWhenClosed`).
                unsafe {
                    panel.setReleasedWhenClosed(false);
                }
                let level = CGWindowLevelForKey(CGWindowLevelKey::PopUpMenuWindowLevelKey);
                panel.setLevel(NSWindowLevel::try_from(level).map_err(|e| e.to_string())?);
                panel.setFloatingPanel(true);
                panel.setBecomesKeyOnlyIfNeeded(true);
                panel.setHidesOnDeactivate(false);
                panel.setBackgroundColor(Some(&NSColor::yellowColor()));
                panel.orderFrontRegardless();
                note(&dir, "popup", panel.isVisible());
                popup = Some(panel);
            }
            if consume(&dir, "unpopup") {
                if let Some(panel) = popup.take() {
                    panel.orderOut(None);
                    note(&dir, "unpopup", panel.isVisible());
                } else {
                    note(&dir, "unpopup", false);
                }
            }
            if consume(&dir, "hide") {
                shown = false;
                window.orderOut(None);
                note(&dir, "hide", window.isVisible());
            }
            if consume(&dir, "show") {
                shown = true;
                window.orderFrontRegardless();
                note(&dir, "show", window.isVisible());
            }
            if shown {
                // Something to capture: a window whose colour never changes is a window
                // ScreenCaptureKit has no frame to deliver for either.
                ticks = ticks.wrapping_add(1);
                let colour = if ticks.is_multiple_of(2) {
                    NSColor::blueColor()
                } else {
                    NSColor::greenColor()
                };
                window.setBackgroundColor(Some(&colour));
                window.display();
            }
        }
        Ok(())
    }

    /// Leave what AppKit thinks of the window after an order, so a test that disagrees with the
    /// host's window list can tell the two apart.
    fn note(dir: &Path, action: &str, visible: bool) {
        let _written = std::fs::write(dir.join("state"), format!("{action} visible={visible}\n"));
    }

    /// Whether the marker was there, taking it away if it was. They are events, not states: a
    /// `hide` left lying around would order the window out again on the tick after every `show`,
    /// and a second `hide` after that would have nothing to write.
    fn consume(dir: &Path, marker: &str) -> bool {
        std::fs::remove_file(dir.join(marker)).is_ok()
    }

    /// Let AppKit run for [`TICK`], so the window server sees the window and its repaints.
    fn pump() {
        let until = NSDate::dateWithTimeIntervalSinceNow(TICK);
        NSRunLoop::mainRunLoop().runUntilDate(&until);
    }
}
