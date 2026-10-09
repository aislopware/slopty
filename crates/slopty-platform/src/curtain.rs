//! The curtain over a Mac a client drives (`docs/decisions/video.md`, "The curtain").
//!
//! The Mac's own screens show a shield instead of the session, and its own keyboard, pointer and
//! trackpad are held off, as Apple Remote Desktop's curtain and Parsec's and Jump Desktop's
//! privacy mode do.
//!
//! Three parts, each owned by the worker for as long as a client asks for the curtain:
//!
//! - [`Shield`]: a borderless window over each physical display at `CGShieldingWindowLevel`, above
//!   the menu bar, the Dock and full-screen spaces. It takes no clicks, so the pointer events the
//!   worker posts reach the windows under it. The worker leaves its windows out of every display
//!   capture by name (`slopty_capture`'s exclusion): ScreenCaptureKit ignores a window's
//!   `sharingType` since macOS 15, so nothing else keeps a shield out of the picture.
//! - [`InputHold`]: an event tap at the HID level, on a thread of its own, that drops every
//!   keyboard, pointer, scroll, gesture and tablet event that does not carry the worker's tag in
//!   `kCGEventSourceUserData`. The worker's own events carry it, so the client still drives.
//! - [`lock_screen`]: the Mac locked when the curtain falls because the client went, so the session
//!   is never left open at the desk.
//!
//! Each part ends with its owner: a worker that dies takes its windows and its tap with it, so
//! nothing outlives the process to leave a Mac dark or deaf.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSFont, NSTextAlignment, NSTextField, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CGFloat, CGRect, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayMirrorsDisplay, CGError, CGEvent, CGEventField,
    CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy,
    CGEventType, CGGetActiveDisplayList, CGMainDisplayID, CGShieldingWindowLevel,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// What the shield says to whoever sits at the Mac.
pub const MESSAGE: &str = "This Mac is in use remotely";

/// The most displays a Mac drives that the shield looks for.
const MAX_DISPLAYS: u32 = 32;

/// The displays to cover: every active display but the ones `skip` names (the worker's own
/// virtual displays, which are what the client sees) and the second of a mirror pair.
fn displays(skip: &dyn Fn(CGDirectDisplayID) -> bool) -> Vec<CGDirectDisplayID> {
    let mut ids = [0; MAX_DISPLAYS as usize];
    let mut count = 0;
    // SAFETY: `ids` holds `MAX_DISPLAYS` entries and `count` is a live `u32`, as the call asks.
    let error = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if error != CGError::Success {
        return Vec::new();
    }
    ids.into_iter()
        .take(count as usize)
        .filter(|id| !skip(*id) && CGDisplayMirrorsDisplay(*id) == 0)
        .collect()
}

/// `bounds`, in CoreGraphics' global space (origin at the main display's top left, y down), as
/// AppKit places a window (origin at the main display's bottom left, y up).
fn appkit_frame(bounds: CGRect, main_height: CGFloat) -> NSRect {
    let y = main_height - (bounds.origin.y + bounds.size.height);
    NSRect::new(
        NSPoint::new(bounds.origin.x, y),
        NSSize::new(bounds.size.width, bounds.size.height),
    )
}

/// The shield's windows, one per physical display. Made, changed and dropped on the main
/// thread, as AppKit asks.
#[derive(Debug)]
pub struct Shield {
    /// Each display covered, with its window.
    windows: Vec<(CGDirectDisplayID, Retained<NSWindow>)>,
    shown: bool,
}

impl Shield {
    /// Make the shield's windows over every display but those `skip` names, not yet shown, so a
    /// capture can be told to leave them out before they are on screen ([`Self::show`]).
    #[must_use]
    pub fn new(mtm: MainThreadMarker, skip: &dyn Fn(CGDirectDisplayID) -> bool) -> Self {
        let app = NSApplication::sharedApplication(mtm);
        // A daemon's windows put no icon in the Dock and never take the menu bar.
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            let _set = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        let mut shield = Self { windows: Vec::new(), shown: false };
        shield.cover(mtm, skip);
        shield
    }

    /// Follow the displays: a window for each physical display now active, none for one gone,
    /// each sized to its display. Returns whether the set of windows changed, so the capture's
    /// exclusion is made again ([`Self::window_ids`]).
    pub fn cover(
        &mut self,
        mtm: MainThreadMarker,
        skip: &dyn Fn(CGDirectDisplayID) -> bool,
    ) -> bool {
        let wanted = displays(skip);
        let main_height = CGDisplayBounds(CGMainDisplayID()).size.height;
        let before = self.windows.len();
        self.windows.retain(|(id, window)| {
            let keep = wanted.contains(id);
            if !keep {
                window.orderOut(None);
                window.close();
            }
            keep
        });
        let mut changed = self.windows.len() != before;
        for id in wanted {
            let frame = appkit_frame(CGDisplayBounds(id), main_height);
            if let Some((_, window)) = self.windows.iter().find(|(covered, _)| *covered == id) {
                window.setFrame_display(frame, true);
                continue;
            }
            let window = shield_window(mtm, frame);
            if self.shown {
                window.orderFrontRegardless();
            }
            self.windows.push((id, window));
            changed = true;
        }
        changed
    }

    /// Put the shield on the screens.
    pub fn show(&mut self) {
        self.shown = true;
        for (_, window) in &self.windows {
            window.orderFrontRegardless();
        }
    }

    /// The window server's numbers of the shield's windows, which a display capture leaves out.
    #[must_use]
    pub fn window_ids(&self) -> Vec<u32> {
        self.windows
            .iter()
            .filter_map(|(_, window)| u32::try_from(window.windowNumber()).ok())
            .collect()
    }

    /// The displays the shield covers.
    #[must_use]
    pub fn displays(&self) -> Vec<CGDirectDisplayID> {
        self.windows.iter().map(|(id, _)| *id).collect()
    }
}

impl Drop for Shield {
    fn drop(&mut self) {
        for (_, window) in self.windows.drain(..) {
            window.orderOut(None);
            window.close();
        }
    }
}

/// One shield window: borderless and black over `frame`, the message in its middle, above
/// everything, on every space, taking no clicks and never the keyboard.
fn shield_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    // SAFETY: a borderless window over `frame`, buffered, made now (not deferred) so it has its
    // window server number at once; `NSWindow`'s designated initialiser with valid arguments.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by the `Retained` here, so `close` must not release it.
    unsafe {
        window.setReleasedWhenClosed(false);
    }
    window.setLevel(CGShieldingWindowLevel() as isize);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    window.setIgnoresMouseEvents(true);
    window.setOpaque(true);
    window.setHasShadow(false);
    window.setCanHide(false);
    window.setHidesOnDeactivate(false);
    window.setExcludedFromWindowsMenu(true);
    window.setBackgroundColor(Some(&NSColor::blackColor()));
    let label = NSTextField::labelWithString(&NSString::from_str(MESSAGE), mtm);
    label.setTextColor(Some(&NSColor::colorWithWhite_alpha(0.55, 1.0)));
    label.setFont(Some(&NSFont::systemFontOfSize(15.0)));
    label.setAlignment(NSTextAlignment::Center);
    let height: CGFloat = 24.0;
    let y = (frame.size.height - height) / 2.0;
    label.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(frame.size.width, height)));
    label.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
    if let Some(content) = window.contentView() {
        content.addSubview(&label);
    } else {
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        content.addSubview(&label);
        window.setContentView(Some(&content));
    }
    window
}

/// The input kinds the hold looks at: keys and modifiers, every button and move of the pointer,
/// scrolls, the trackpad's gestures and a tablet's pen, and the media and brightness keys
/// (`NX_SYSDEFINED`).
const HELD_KINDS: [u32; 25] = [
    1, 2, 3, 4, 5, 6, 7, // the left and right buttons, moves and drags
    10, 11, 12, // keys, modifiers
    14, // NX_SYSDEFINED: media, volume, brightness
    18, 19, 20, // rotate, gesture begin and end
    22, 23, 24, // scroll, tablet pointer and proximity
    25, 26, 27, // the other buttons
    29, 30, 31, 32, 34, // gestures, magnify, swipe, smart magnify, pressure
];

/// The tap's mask: one bit per kind in [`HELD_KINDS`].
#[must_use]
fn held_mask() -> CGEventMask {
    HELD_KINDS.iter().fold(0, |mask, kind| mask | 1_u64.checked_shl(*kind).unwrap_or(0))
}

/// Whether an event goes on while the input is held: only one carrying `ours` in
/// `kCGEventSourceUserData`, the worker's own.
#[must_use]
pub const fn passes(tag: i64, ours: i64) -> bool {
    tag == ours
}

/// Why the hold could not start.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HoldError {
    /// macOS refused the tap: this process is not allowed Accessibility.
    #[error("Accessibility is not allowed, so local input cannot be held")]
    NotAllowed,
    /// The tap's thread could not start.
    #[error("the input hold's thread did not start")]
    Thread,
}

/// What the tap's callback reads: the tag that passes, the count of events held, and the tap,
/// to turn back on when macOS turns it off.
struct HoldState {
    ours: i64,
    held: AtomicU64,
    port: std::sync::OnceLock<PortRef>,
}

/// The tap's port, read by the callback on the tap's own thread only.
struct PortRef(NonNull<CFMachPort>);

// SAFETY: the port is set once by the tap's thread and read only by the callback, which runs on
// that same thread; no other thread dereferences it.
unsafe impl Send for PortRef {}
// SAFETY: as above.
unsafe impl Sync for PortRef {}

/// A +1 reference to the hold thread's `CFRunLoop`, which the owner stops.
struct StopHandle(NonNull<CFRunLoop>);

// SAFETY: the only use of the loop off its own thread is `CFRunLoop::stop`, which Core
// Foundation documents as callable from any thread.
unsafe impl Send for StopHandle {}
// SAFETY: a shared handle exposes nothing; the pointer is read only in `Drop`.
unsafe impl Sync for StopHandle {}

impl Drop for StopHandle {
    fn drop(&mut self) {
        // SAFETY: the thread stored the +1 reference `into_raw` produced, taken back once here.
        let run_loop = unsafe { CFRetained::from_raw(self.0) };
        run_loop.stop();
    }
}

/// The local keyboard, pointer and trackpad held off.
///
/// Every input event without the worker's tag is dropped at the HID level, before any
/// application or the window server's shortcuts see it. Dropping the hold lets input through
/// again.
pub struct InputHold {
    stop: Option<StopHandle>,
    thread: Option<std::thread::JoinHandle<()>>,
    state: Arc<HoldState>,
}

impl std::fmt::Debug for InputHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputHold").field("held", &self.held()).finish_non_exhaustive()
    }
}

impl InputHold {
    /// Hold every input event that does not carry `ours` in `kCGEventSourceUserData`, from a
    /// tap on a thread of its own: a busy main thread or runtime never slows the Mac's input.
    ///
    /// # Errors
    ///
    /// [`HoldError::NotAllowed`] without Accessibility, [`HoldError::Thread`] when the thread
    /// does not start.
    pub fn start(ours: i64) -> Result<Self, HoldError> {
        let state =
            Arc::new(HoldState { ours, held: AtomicU64::new(0), port: std::sync::OnceLock::new() });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let shared = Arc::clone(&state);
        let thread = std::thread::Builder::new()
            .name("slopty-input-hold".to_owned())
            .spawn(move || hold(&shared, &ready_tx))
            .map_err(|_spawn| HoldError::Thread)?;
        match ready_rx.recv() {
            Ok(Ok(stop)) => Ok(Self { stop: Some(stop), thread: Some(thread), state }),
            Ok(Err(error)) => {
                let _ended = thread.join();
                Err(error)
            }
            Err(_gone) => Err(HoldError::Thread),
        }
    }

    /// Events held since the hold started.
    #[must_use]
    pub fn held(&self) -> u64 {
        self.state.held.load(Ordering::Relaxed)
    }
}

impl Drop for InputHold {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ended = thread.join();
        }
    }
}

/// The hold's thread: make the tap, say so, run the loop until stopped, then take the tap down.
fn hold(state: &Arc<HoldState>, ready: &mpsc::SyncSender<Result<StopHandle, HoldError>>) {
    let info = Arc::as_ptr(state).cast_mut().cast::<c_void>();
    // SAFETY: `callback` has the `CGEventTapCallBack` signature, and `info` points at the
    // `HoldState` the `Arc` this thread holds keeps alive until after the port is invalidated
    // below.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            held_mask(),
            Some(callback),
            info,
        )
    };
    let Some(port) = port else {
        let _told = ready.send(Err(HoldError::NotAllowed));
        return;
    };
    let (Some(source), Some(run_loop)) =
        (CFMachPort::new_run_loop_source(None, Some(&port), 0), CFRunLoop::current())
    else {
        port.invalidate();
        let _told = ready.send(Err(HoldError::Thread));
        return;
    };
    let _set = state.port.set(PortRef(NonNull::from(&*port)));
    // SAFETY: a `CFRunLoopMode` static Core Foundation defines, read-only.
    let common = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(Some(&source), common);
    let stop = StopHandle(CFRetained::into_raw(CFRetained::clone(&run_loop)));
    if ready.send(Ok(stop)).is_err() {
        port.invalidate();
        return;
    }
    CFRunLoop::run();
    port.invalidate();
    run_loop.remove_source(Some(&source), common);
}

/// The tap's callback: an event without the worker's tag is dropped (null returned) and
/// counted; the worker's own goes on. A tap macOS turned off for being slow is turned back on.
unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `info` is the `HoldState` the hold's thread keeps alive while the tap exists.
    let Some(state) = (unsafe { info.cast::<HoldState>().as_ref() }) else {
        return event.as_ptr();
    };
    if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput {
        if let Some(port) = state.port.get() {
            // SAFETY: the port the hold's thread made, valid until it invalidates it after
            // the loop ends, and this callback runs on that thread inside the loop.
            CGEvent::tap_enable(unsafe { port.0.as_ref() }, true);
        }
        return event.as_ptr();
    }
    // SAFETY: CoreGraphics hands the callback a live event.
    let tag = CGEvent::integer_value_field(
        Some(unsafe { event.as_ref() }),
        CGEventField::EventSourceUserData,
    );
    if passes(tag, state.ours) {
        return event.as_ptr();
    }
    state.held.fetch_add(1, Ordering::Relaxed);
    std::ptr::null_mut()
}

/// Why the Mac could not be locked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LockError {
    /// The login framework or its lock call is not on this macOS.
    #[error("this macOS has no SACLockScreenImmediate")]
    Unavailable,
}

/// Lock the Mac's screens at once, whatever the screen saver's password delay says.
///
/// This is what the menu bar's Lock Screen does: `SACLockScreenImmediate` in the private `login`
/// framework, looked up at runtime so a macOS without it says so instead of failing to load.
///
/// # Errors
///
/// [`LockError::Unavailable`] when the framework or the call is missing.
pub fn lock_screen() -> Result<(), LockError> {
    type Lock = unsafe extern "C-unwind" fn() -> i32;
    let path = c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login";
    // SAFETY: `dlopen(3)` with a NUL-terminated path; the handle is kept for the process's
    // life, as the framework is never unloaded.
    let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if handle.is_null() {
        return Err(LockError::Unavailable);
    }
    // SAFETY: `dlsym(3)` on the live handle with a NUL-terminated name.
    let found = unsafe { libc::dlsym(handle, c"SACLockScreenImmediate".as_ptr()) };
    if found.is_null() {
        return Err(LockError::Unavailable);
    }
    // SAFETY: the login framework exports `SACLockScreenImmediate` as `int (void)`, the
    // signature `Lock` names (Timac's and pudquick's notes, and every lock tool that calls it).
    let lock = unsafe { std::mem::transmute::<*mut c_void, Lock>(found) };
    // SAFETY: a call with no arguments; it posts the lock to loginwindow and returns.
    let _status = unsafe { lock() };
    Ok(())
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    /// Only the worker's own events pass while the input is held.
    #[test]
    fn only_the_worker_s_own_events_pass() {
        let ours = 0x534c_4f50;
        assert!(passes(ours, ours));
        assert!(!passes(0, ours), "a keyboard's or a trackpad's event has no tag");
        assert!(!passes(7, ours), "another program's tag is not the worker's");
    }

    /// The mask holds keys, every button, moves, scrolls, gestures and the media keys, and
    /// nothing else: no tap-disabled notice, no null event.
    #[test]
    fn the_mask_is_the_input_kinds() {
        let mask = held_mask();
        for kind in [
            CGEventType::KeyDown,
            CGEventType::KeyUp,
            CGEventType::FlagsChanged,
            CGEventType::LeftMouseDown,
            CGEventType::RightMouseUp,
            CGEventType::MouseMoved,
            CGEventType::OtherMouseDragged,
            CGEventType::ScrollWheel,
            CGEventType::TabletPointer,
        ] {
            assert_ne!(mask & (1 << kind.0), 0, "{kind:?} is held");
        }
        assert_eq!(mask & 1, 0, "the null event is not");
        assert_eq!(mask.count_ones() as usize, HELD_KINDS.len(), "no kind twice");
    }

    /// A display's CoreGraphics bounds land where AppKit puts a window over it: the main
    /// display's at the origin, one above it at its height, one to its left below its top.
    #[test]
    fn a_display_s_bounds_become_its_appkit_frame() {
        let rect = |x, y, w, h| CGRect::new(CGPoint::new(x, y), CGSize::new(w, h));
        let main = appkit_frame(rect(0.0, 0.0, 1512.0, 982.0), 982.0);
        assert_eq!((main.origin.x, main.origin.y), (0.0, 0.0));
        let above = appkit_frame(rect(0.0, -1440.0, 2560.0, 1440.0), 982.0);
        assert_eq!((above.origin.x, above.origin.y), (0.0, 982.0));
        let left = appkit_frame(rect(-1920.0, 100.0, 1920.0, 1080.0), 982.0);
        assert_eq!((left.origin.x, left.origin.y), (-1920.0, -198.0));
    }
}
