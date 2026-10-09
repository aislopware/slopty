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
//!   `kCGEventSourceUserData`. The worker's own events carry it, so the client still drives. The
//!   hold is a lease its owner renews ([`Lease`]): a worker that is alive but stuck stops renewing
//!   it, and the Mac's own input passes again, so the desk is never shut out.
//! - [`lock_screen`]: the Mac locked when the curtain falls because the client went, so the session
//!   is never left open at the desk.
//!
//! Each part ends with its owner: a worker that dies takes its windows and its tap with it, so
//! nothing outlives the process to leave a Mac dark or deaf.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSEventType, NSFont, NSTextAlignment, NSTextField, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFMachPort, CFRetained, CFRunLoop, CGFloat, CGRect, kCFRunLoopCommonModes,
};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayMirrorsDisplay, CGError, CGEvent, CGEventField,
    CGEventMask, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGGetActiveDisplayList, CGMainDisplayID,
    CGMouseButton, CGShieldingWindowLevel,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

/// What the shield says to whoever sits at the Mac.
pub const MESSAGE: &str = "This Mac is in use remotely";

/// The most displays a Mac drives that the shield looks for.
const MAX_DISPLAYS: u32 = 32;

/// The displays to cover now ([`to_cover`]); `None` when CoreGraphics would not list them, so
/// the shield keeps what it covers rather than uncover every screen.
fn displays(skip: &dyn Fn(CGDirectDisplayID) -> bool) -> Option<Vec<CGDirectDisplayID>> {
    let mut ids = [0; MAX_DISPLAYS as usize];
    let mut count = 0;
    // SAFETY: `ids` holds `MAX_DISPLAYS` entries and `count` is a live `u32`, as the call asks.
    let error = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if error != CGError::Success {
        return None;
    }
    let active = ids.into_iter().take(count as usize);
    Some(to_cover(active, skip, &|id| CGDisplayMirrorsDisplay(id)))
}

/// Of the `active` displays, the ones to cover: all but those `skip` names (the worker's own
/// virtual displays, which are what the client sees), and but a mirror of a display already
/// covered, whose window shows there too. A display that mirrors one `skip` names (a client's
/// virtual display shown on the Mac's own panel) is covered: its window sits over the virtual
/// display as well, out of every capture, so the client still sees the session.
fn to_cover(
    active: impl Iterator<Item = CGDirectDisplayID>,
    skip: &dyn Fn(CGDirectDisplayID) -> bool,
    mirrors: &dyn Fn(CGDirectDisplayID) -> CGDirectDisplayID,
) -> Vec<CGDirectDisplayID> {
    active
        .filter(|id| {
            let master = mirrors(*id);
            !skip(*id) && (master == 0 || skip(master))
        })
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
    /// exclusion is made again ([`Self::window_ids`]). Displays CoreGraphics would not list
    /// leave the shield as it is.
    pub fn cover(
        &mut self,
        mtm: MainThreadMarker,
        skip: &dyn Fn(CGDirectDisplayID) -> bool,
    ) -> bool {
        let Some(wanted) = displays(skip) else {
            return false;
        };
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
/// scrolls, a tablet's pen, the trackpad's gestures, and the media and brightness keys
/// (`NSEventTypeSystemDefined`). The gesture kinds have no `CGEventType`, so AppKit's numbers
/// name them: the two share one numbering.
fn held_kinds() -> Vec<u64> {
    let cg = [
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
        CGEventType::ScrollWheel,
        CGEventType::TabletPointer,
        CGEventType::TabletProximity,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::OtherMouseDragged,
    ]
    .map(|kind| u64::from(kind.0));
    let app_kit = [
        NSEventType::SystemDefined,
        NSEventType::Rotate,
        NSEventType::BeginGesture,
        NSEventType::EndGesture,
        NSEventType::Gesture,
        NSEventType::Magnify,
        NSEventType::Swipe,
        NSEventType::SmartMagnify,
        NSEventType::Pressure,
    ];
    cg.into_iter()
        .chain(app_kit.into_iter().filter_map(|kind| u64::try_from(kind.0).ok()))
        .collect()
}

/// The tap's mask: one bit per kind in [`held_kinds`].
#[must_use]
fn held_mask() -> CGEventMask {
    held_kinds().iter().fold(0, |mask, kind| {
        mask | u32::try_from(*kind).ok().and_then(|k| 1_u64.checked_shl(k)).unwrap_or(0)
    })
}

/// Whether an event goes on while the input is held: one carrying `ours` in
/// `kCGEventSourceUserData`, the worker's own, and every event once the hold's lease has lapsed.
#[must_use]
pub const fn passes(tag: i64, ours: i64, lapsed: bool) -> bool {
    lapsed || tag == ours
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

/// What the tap's callback reads: the tag that passes, the count of events held, the tap, to
/// turn back on when macOS turns it off, and the lease: until when the hold holds.
struct HoldState {
    ours: i64,
    held: AtomicU64,
    port: std::sync::OnceLock<PortRef>,
    /// The clock the lease is read on.
    born: Instant,
    /// The lease's end, in nanoseconds after `born`.
    until: AtomicU64,
}

impl HoldState {
    /// Holding for `lease` from now.
    fn new(ours: i64, lease: Duration) -> Self {
        let state = Self {
            ours,
            held: AtomicU64::new(0),
            port: std::sync::OnceLock::new(),
            born: Instant::now(),
            until: AtomicU64::new(0),
        };
        state.renew(lease);
        state
    }

    /// Nanoseconds since `born`.
    fn now(&self) -> u64 {
        u64::try_from(self.born.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Hold for `lease` from now.
    fn renew(&self, lease: Duration) {
        let lease = u64::try_from(lease.as_nanos()).unwrap_or(u64::MAX);
        self.until.store(self.now().saturating_add(lease), Ordering::Release);
    }

    /// The lease ran out unrenewed.
    fn lapsed(&self) -> bool {
        self.now() >= self.until.load(Ordering::Acquire)
    }
}

/// The tap's port, read by the callback on the tap's own thread only.
struct PortRef(NonNull<CFMachPort>);

// SAFETY: the port is set once by the tap's thread and read only by the callback, which runs on
// that same thread; no other thread dereferences it.
unsafe impl Send for PortRef {}
// SAFETY: as above.
unsafe impl Sync for PortRef {}

/// The owner's ends of a port served on a thread of its own: the port, which ends the thread's
/// loop once it is invalidated, and the loop.
struct Ender {
    port: CFRetained<CFMachPort>,
    run_loop: CFRetained<CFRunLoop>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "off its thread the port is only invalidated and the loop only stopped"
)]
// SAFETY: off its own thread the port is only invalidated and the loop only stopped:
// `CFMachPortInvalidate` and `CFRunLoopStop` take Core Foundation's own locks on the object and
// are documented as callable from any thread (Threading Programming Guide, "Run Loop Objects":
// "you can call CFRunLoopStop from any thread").
unsafe impl Send for Ender {}
// SAFETY: as above; `end` is the only use.
unsafe impl Sync for Ender {}

impl Ender {
    /// End the loop, whether it runs yet or not. A stop alone is lost on a loop that has not
    /// started (the run's start clears it); a loop whose only source went returns as soon as
    /// it runs, and one that runs already is woken by the stop.
    fn end(&self) {
        self.port.invalidate();
        self.run_loop.stop();
    }
}

/// How long dropping a hold waits for its thread before it leaves it: the loop ends at once once
/// its port is gone, so only a thread stuck inside a callback takes longer.
const JOIN_BOUND: Duration = Duration::from_secs(2);

/// A thread serving one port on its run loop until its [`Ender`] ends it.
struct Served {
    ender: Ender,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Hung up as the thread ends, however it ends.
    ended: mpsc::Receiver<()>,
}

impl Served {
    /// End the loop and wait, at most [`JOIN_BOUND`], for the thread. Never unbounded: this
    /// runs on the main queue, which a stuck thread must not take down with it.
    fn end(&mut self) {
        self.ender.end();
        match self.ended.recv_timeout(JOIN_BOUND) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("the input hold's thread did not end; left behind");
            }
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Some(thread) = self.thread.take() {
                    let _ended = thread.join();
                }
            }
        }
    }
}

/// Start a thread that makes a port (`make`), with what its callback reads, adds it to its run
/// loop and runs the loop until the returned [`Served`] ends it; `before_run` runs on that
/// thread just before the loop does. What the callback reads is kept until after the port is
/// invalidated, by the thread, so a thread left behind never leaves it dangling.
fn serve<K: 'static>(
    name: &str,
    make: impl FnOnce() -> Result<(CFRetained<CFMachPort>, K), HoldError> + Send + 'static,
    before_run: impl FnOnce() + Send + 'static,
) -> Result<Served, HoldError> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (ended_tx, ended_rx) = mpsc::sync_channel::<()>(0);
    let thread = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _hung_up_as_it_ends = ended_tx;
            match make() {
                Ok((port, kept)) => {
                    run_port(&port, &ready_tx, before_run);
                    drop(kept);
                }
                Err(e) => {
                    let _told = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|_spawn| HoldError::Thread)?;
    match ready_rx.recv() {
        Ok(Ok(ender)) => Ok(Served { ender, thread: Some(thread), ended: ended_rx }),
        Ok(Err(e)) => {
            let _ended = thread.join();
            Err(e)
        }
        // The thread ended before it said: it can only have panicked.
        Err(_gone) => {
            let _ended = thread.join();
            Err(HoldError::Thread)
        }
    }
}

/// On the serving thread: add `port` to this thread's run loop, hand the owner its ends, and
/// run the loop until they end it; then take the port down.
fn run_port(
    port: &CFRetained<CFMachPort>,
    ready: &mpsc::SyncSender<Result<Ender, HoldError>>,
    before_run: impl FnOnce(),
) {
    let (Some(source), Some(run_loop)) =
        (CFMachPort::new_run_loop_source(None, Some(port), 0), CFRunLoop::current())
    else {
        port.invalidate();
        let _told = ready.send(Err(HoldError::Thread));
        return;
    };
    // SAFETY: a `CFRunLoopMode` static Core Foundation defines, read-only.
    let common = unsafe { kCFRunLoopCommonModes };
    run_loop.add_source(Some(&source), common);
    let ender = Ender { port: CFRetained::clone(port), run_loop: CFRetained::clone(&run_loop) };
    if ready.send(Ok(ender)).is_err() {
        port.invalidate();
        return;
    }
    before_run();
    // Returns once the owner ends it: stopped while it runs, or with no source left to serve
    // when the port went before it started.
    CFRunLoop::run();
    port.invalidate();
    run_loop.remove_source(Some(&source), common);
}

/// The local keyboard, pointer and trackpad held off.
///
/// Every input event without the worker's tag is dropped at the HID level, before any
/// application or the window server's shortcuts see it, for as long as its lease is renewed
/// ([`Lease`]). Dropping the hold lets input through again.
pub struct InputHold {
    served: Served,
    state: Arc<HoldState>,
    lease: Duration,
}

impl std::fmt::Debug for InputHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputHold").field("held", &self.held()).finish_non_exhaustive()
    }
}

impl InputHold {
    /// Hold every input event that does not carry `ours` in `kCGEventSourceUserData`, from a
    /// tap on a thread of its own (a busy main thread or runtime never slows the Mac's input),
    /// for `lease` unless it is renewed ([`Self::lease`]).
    ///
    /// # Errors
    ///
    /// [`HoldError::NotAllowed`] without Accessibility, [`HoldError::Thread`] when the thread
    /// does not start.
    pub fn start(ours: i64, lease: Duration) -> Result<Self, HoldError> {
        let state = Arc::new(HoldState::new(ours, lease));
        let shared = Arc::clone(&state);
        let served = serve("slopty-input-hold", move || tap(shared), || {})?;
        Ok(Self { served, state, lease })
    }

    /// Events held since the hold started.
    #[must_use]
    pub fn held(&self) -> u64 {
        self.state.held.load(Ordering::Relaxed)
    }

    /// What renews the hold's lease, from any thread.
    #[must_use]
    pub fn lease(&self) -> Lease {
        Lease { state: Arc::clone(&self.state), lease: self.lease }
    }
}

impl Drop for InputHold {
    fn drop(&mut self) {
        self.served.end();
    }
}

/// An [`InputHold`]'s lease, renewed on the owner's own clock (its runtime, not the main
/// thread).
///
/// Each renewal holds for the lease's length from then. A worker alive but stuck renews
/// nothing, and once the lease lapses every event passes, so the desk is never shut out by a
/// worker that cannot let go.
#[derive(Clone)]
pub struct Lease {
    state: Arc<HoldState>,
    lease: Duration,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease").field("lease", &self.lease).finish_non_exhaustive()
    }
}

impl Lease {
    /// Hold for the lease's length from now.
    pub fn renew(&self) {
        self.state.renew(self.lease);
    }
}

/// On the hold's thread: the tap, its callback reading `state`, which the thread keeps alive
/// until after the port is invalidated ([`serve`]).
fn tap(state: Arc<HoldState>) -> Result<(CFRetained<CFMachPort>, Arc<HoldState>), HoldError> {
    let info = Arc::as_ptr(&state).cast_mut().cast::<c_void>();
    // SAFETY: `callback` has the `CGEventTapCallBack` signature, and `info` points at the
    // `HoldState` returned with the port, which `serve` keeps until after the port's
    // invalidation in `run_port`.
    let port = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            held_mask(),
            Some(callback),
            info,
        )
    }
    .ok_or(HoldError::NotAllowed)?;
    let _set = state.port.set(PortRef(NonNull::from(&*port)));
    Ok((port, state))
}

/// The tap's callback: an event without the worker's tag is dropped (null returned) and
/// counted while the lease holds; the worker's own goes on, and every event once the lease
/// lapsed. A tap macOS turned off for being slow is turned back on.
unsafe extern "C-unwind" fn callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: NonNull<CGEvent>,
    info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `info` is the `HoldState` the tap's maker keeps alive while the tap exists.
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
    if passes(tag, state.ours, state.lapsed()) {
        return event.as_ptr();
    }
    state.held.fetch_add(1, Ordering::Relaxed);
    std::ptr::null_mut()
}

/// What the Mac's own keyboard and pointer hold down: a key by its virtual key code, a button
/// by its number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Down {
    Key(u16),
    Button(u32),
}

/// The highest virtual key code a keyboard sends.
const LAST_KEY: u16 = 127;
/// The highest button number a pointer sends.
const LAST_BUTTON: u32 = 31;

/// What is down now, as `key_down` and `button_down` say.
fn held_down(key_down: &dyn Fn(u16) -> bool, button_down: &dyn Fn(u32) -> bool) -> Vec<Down> {
    (0..=LAST_KEY)
        .filter(|key| key_down(*key))
        .map(Down::Key)
        .chain((0..=LAST_BUTTON).filter(|button| button_down(*button)).map(Down::Button))
        .collect()
}

/// The kind of event that lets `button` go.
const fn button_up(button: u32) -> CGEventType {
    match button {
        0 => CGEventType::LeftMouseUp,
        1 => CGEventType::RightMouseUp,
        _ => CGEventType::OtherMouseUp,
    }
}

/// Let go of whatever the Mac's own keyboard and pointer hold down as the hold starts.
///
/// The hold drops their own releases, which would leave a key or a button down under the
/// session the client drives. Each release carries `ours`, so it passes the hold.
pub fn release_held(ours: i64) {
    let state = CGEventSourceStateID::HIDSystemState;
    let down = held_down(&|key| CGEventSource::key_state(state, key), &|button| {
        CGEventSource::button_state(state, CGMouseButton(button))
    });
    if down.is_empty() {
        return;
    }
    let at = CGEvent::location(CGEvent::new(None).as_deref());
    for down in down {
        let event = match down {
            Down::Key(key) => CGEvent::new_keyboard_event(None, key, false),
            Down::Button(button) => {
                CGEvent::new_mouse_event(None, button_up(button), at, CGMouseButton(button))
            }
        };
        let Some(event) = event else { continue };
        CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, ours);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    }
}

/// Why the Mac could not be locked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LockError {
    /// The login framework or its lock call is not on this macOS.
    #[error("this macOS has no SACLockScreenImmediate")]
    Unavailable,
    /// The lock call returned this status rather than zero.
    #[error("SACLockScreenImmediate returned {0}")]
    Refused(i32),
}

/// Ask for the Mac's screens to be locked at once, whatever the screen saver's password delay
/// says.
///
/// This is what the menu bar's Lock Screen does: `SACLockScreenImmediate` in the private `login`
/// framework, looked up at runtime so a macOS without it says so instead of failing to load. It
/// posts the lock to loginwindow and returns before the screens are locked: the caller waits
/// for the session to read as locked before it trusts it.
///
/// # Errors
///
/// [`LockError::Unavailable`] when the framework or the call is missing,
/// [`LockError::Refused`] when the call says it failed.
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
    let status = unsafe { lock() };
    if status != 0 {
        return Err(LockError::Refused(status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGSize};

    use super::*;

    /// Only the worker's own events pass while the input is held, and every event once its
    /// lease has lapsed.
    #[test]
    fn only_the_worker_s_own_events_pass() {
        let ours = 0x534c_4f50;
        assert!(passes(ours, ours, false));
        assert!(!passes(0, ours, false), "a keyboard's or a trackpad's event has no tag");
        assert!(!passes(7, ours, false), "another program's tag is not the worker's");
        assert!(passes(0, ours, true), "the lease lapsed: the desk has its input back");
    }

    /// A hold's lease holds until it runs out, and a renewal holds it for its length again.
    #[test]
    fn a_lease_lapses_unless_it_is_renewed() {
        let state = Arc::new(HoldState::new(1, Duration::from_secs(3600)));
        assert!(!state.lapsed(), "held for the lease");
        let lease = Lease { state: Arc::clone(&state), lease: Duration::ZERO };
        lease.renew();
        assert!(state.lapsed(), "a lease renewed for nothing has run out");
        let lease = Lease { state: Arc::clone(&state), lease: Duration::from_secs(3600) };
        lease.renew();
        assert!(!state.lapsed(), "renewed: held again");
    }

    /// A port's thread ended before its loop runs still ends: the stop a loop not yet running
    /// would lose is not what ends it, and the owner's wait is bounded. Any port serves; this
    /// one is a plain Mach port, so no Accessibility is asked.
    #[test]
    fn a_hold_ended_before_its_loop_runs_still_ends() {
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let make = || {
            // SAFETY: no callout, no context: a port that is only served and invalidated.
            let port =
                unsafe { CFMachPort::new(None, None, std::ptr::null_mut(), std::ptr::null_mut()) };
            port.map(|p| (p, ())).ok_or(HoldError::Thread)
        };
        let mut served = serve("slopty-test-port", make, move || {
            let _go = go_rx.recv();
        })
        .unwrap();
        // Ended while the thread waits short of its loop.
        served.ender.end();
        go_tx.send(()).unwrap();
        let ended = served.ended.recv_timeout(Duration::from_secs(5));
        assert_eq!(ended, Err(mpsc::RecvTimeoutError::Disconnected), "the thread ended");
        served.end();
    }

    /// What is down as the hold starts is let go: each key and each button, nothing else.
    #[test]
    fn what_is_down_as_the_hold_starts_is_let_go() {
        let down = held_down(&|key| key == 0x37 || key == 12, &|button| button == 0);
        assert_eq!(down, [Down::Key(12), Down::Key(0x37), Down::Button(0)]);
        assert_eq!(held_down(&|_| false, &|_| false), []);
        assert_eq!(button_up(0), CGEventType::LeftMouseUp);
        assert_eq!(button_up(1), CGEventType::RightMouseUp);
        assert_eq!(button_up(4), CGEventType::OtherMouseUp);
    }

    /// The shield covers each display the client does not see, once per mirror set: a mirror of
    /// a covered display is passed over, and a display mirroring a client's virtual display is
    /// covered.
    #[test]
    fn the_shield_covers_each_physical_display_once() {
        let virtual_display = 9;
        let skip = |id: CGDirectDisplayID| id == virtual_display;
        // 1 is the main panel, 2 mirrors it, 3 mirrors the virtual display, 4 stands alone.
        let mirrors = |id: CGDirectDisplayID| match id {
            2 => 1,
            3 => virtual_display,
            _ => 0,
        };
        let covered = to_cover([1, 2, 3, 4, virtual_display].into_iter(), &skip, &mirrors);
        assert_eq!(covered, [1, 3, 4]);
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
        for kind in [NSEventType::SystemDefined, NSEventType::Magnify, NSEventType::Pressure] {
            assert_ne!(mask & (1 << kind.0), 0, "{kind:?} is held");
        }
        assert_eq!(mask & 1, 0, "the null event is not");
        assert_eq!(mask.count_ones() as usize, held_kinds().len(), "no kind twice");
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
