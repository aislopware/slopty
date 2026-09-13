//! The accessibility API's word that a window of the target's application went away, which it
//! gives the moment AppKit orders the window out — about 260 ms before the window list does
//! (MEASUREMENTS.md, "which signal knows first").
//!
//! It cannot name the window: the public API exposes no window number on an `AXUIElement`. What
//! it can do is match the target once, at registration, by the frame and title the window list
//! gives for it, and then tell that element from every other window of the application by
//! identity ([`Went::Target`] against [`Went::Other`]). The target going is a *suspicion* the
//! stream holds frames on until the window list confirms or denies; another window going is
//! not, and matters only because the capture framework stalls on it (DECISIONS.md, "The
//! accessibility API knows about a hide 260 ms before core graphics does" and "A suspicion
//! moves the stream to the window filter"). When no element matches — the application does not
//! list the window, or it moved between the two reads — every window's going is a suspicion,
//! as before.
//!
//! One thread per watch runs a `CFRunLoop` for the observer; the callback does nothing but
//! count and call back, so the run loop is never behind work.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use objc2_application_services::{
    AXError, AXIsProcessTrusted, AXObserver, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFRetained, CFRunLoop, CFString, CFType, CGPoint, CGSize, Type as _,
    kCFRunLoopDefaultMode,
};

use crate::geometry::Rect;

// The names below are `#define kAX… CFSTR("…")` macros in the SDK's
// `HIServices/AXAttributeConstants.h` and `HIServices/AXNotificationConstants.h`. No symbol is
// exported for a `CFSTR` macro, so no objc2 static exists, and this is the one place in the
// workspace that spells them (DECISIONS.md, "Constants the SDK defines as `CFSTR` macros").

/// `kAXWindowsAttribute`: the application's windows, as an array of elements.
const WINDOWS_ATTRIBUTE: &str = "AXWindows";
/// `kAXWindowMiniaturizedNotification`.
const WINDOW_MINIATURIZED: &str = "AXWindowMiniaturized";
/// `kAXApplicationHiddenNotification`.
const APPLICATION_HIDDEN: &str = "AXApplicationHidden";
/// `kAXUIElementDestroyedNotification`: what AppKit posts for a window it orders out.
const ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
/// `kAXWindowCreatedNotification`: a new window to watch for destruction.
const WINDOW_CREATED: &str = "AXWindowCreated";
/// `kAXPositionAttribute`: a window's top-left corner in screen points, as an `AXValue`.
const POSITION_ATTRIBUTE: &str = "AXPosition";
/// `kAXSizeAttribute`: a window's size in points, as an `AXValue`.
const SIZE_ATTRIBUTE: &str = "AXSize";
/// `kAXTitleAttribute`: a window's title.
const TITLE_ATTRIBUTE: &str = "AXTitle";

/// How far, in points, the accessibility frame may sit from the window list's for the two to
/// be one window: the two APIs round differently at fractional scales.
const FRAME_TOLERANCE: f64 = 1.0;

/// How long one turn of the watch's run loop lasts before it looks at the stop flag. Only a
/// stop that lands between the flag check and the run call waits this long; every other one
/// wakes the loop at once.
const RUN_SLICE_SECS: f64 = 1.0;

/// Why a watch could not be started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AxError {
    /// The process is not trusted for accessibility; the API answers nothing without it.
    #[error("this process is not trusted for accessibility")]
    NotTrusted,
    /// The accessibility API refused (an `AXError` code: -25204 is "cannot complete", the
    /// usual answer for a process that is gone or not an application).
    #[error("accessibility observer failed with AXError {0}")]
    Observer(i32),
    /// The watch thread ended before it reported.
    #[error("the accessibility watch thread ended before it was ready")]
    Thread,
    /// The application lists no window with the target's frame and title.
    #[error("the application lists no window matching the target")]
    NoWindow,
    /// The accessibility API refused to set an attribute (an `AXError` code).
    #[error("accessibility attribute write failed with AXError {0}")]
    Attribute(i32),
}

/// Give the window of the application `pid` that matches `target` the size in points.
///
/// Written through `kAXSizeAttribute`. The application applies its own limits (a minimum
/// size, a fixed aspect), so the size it ends up with is read back from the window list, not
/// assumed. Blocks for the accessibility round trips (a few milliseconds).
pub fn resize_window(
    pid: i32,
    target: &TargetWindow,
    width: f64,
    height: f64,
) -> Result<(), AxError> {
    // SAFETY: the documented no-argument query; it takes and returns nothing owned.
    if !unsafe { AXIsProcessTrusted() } {
        return Err(AxError::NotTrusted);
    }
    // SAFETY: the documented constructor for an application element; it returns +1.
    let app = unsafe { AXUIElement::new_application(pid) };
    let window =
        windows_of(&app).into_iter().find(|w| matches(w, target)).ok_or(AxError::NoWindow)?;
    let mut size = CGSize::new(width, height);
    // SAFETY: `size` outlives the call and is the type `AXValueType::CGSize` names
    // (`AXValueCreate` copies it).
    let value = unsafe { AXValue::new(AXValueType::CGSize, NonNull::from(&mut size).cast()) }
        .ok_or(AxError::Attribute(0))?;
    let attribute = CFString::from_str(SIZE_ATTRIBUTE);
    // SAFETY: `window` is live and `value` is an `AXValue` of the type the attribute takes
    // (`AXUIElementSetAttributeValue`).
    let status = unsafe { window.set_attribute_value(&attribute, &value) };
    if status == AXError::Success { Ok(()) } else { Err(AxError::Attribute(status.0)) }
}

/// A +1 reference to the watch thread's `CFRunLoop`, handed to the owner that stops it.
///
/// objc2 marks `CFRunLoop` as not thread-safe; the one call made from another thread is
/// `CFRunLoopStop`, which Core Foundation documents as safe to call from any thread.
struct StopHandle(NonNull<CFRunLoop>);

// SAFETY: the only use of the wrapped loop off its own thread is `CFRunLoop::stop`, which
// Core Foundation documents as callable from any thread; the loop is never run, queried or
// given sources from anywhere but the thread that created it.
unsafe impl Send for StopHandle {}

// SAFETY: a shared handle exposes nothing: the pointer is read only in `Drop`, which needs
// `&mut self`, and the call it makes is the thread-safe `CFRunLoopStop` above.
unsafe impl Sync for StopHandle {}

impl StopHandle {
    fn new(run_loop: &CFRunLoop) -> Self {
        Self(CFRetained::into_raw(run_loop.retain()))
    }
}

impl Drop for StopHandle {
    fn drop(&mut self) {
        // SAFETY: `new` stored the +1 reference `into_raw` produced, and this is the only place
        // that takes it back.
        let run_loop = unsafe { CFRetained::from_raw(self.0) };
        run_loop.stop();
    }
}

/// What the watch heard go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Went {
    /// The target: its element was destroyed or minimised, or the application was hidden. Also
    /// every window of the application when no element could be matched to the target.
    Target,
    /// Another window of the application; the target is untouched.
    Other,
}

/// The target as the window list describes it, for matching it to its accessibility element.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetWindow {
    /// `kCGWindowBounds`: screen points, top-left origin, the same space as `AXPosition`.
    pub bounds: Rect,
    /// `kCGWindowName`, if the window has one.
    pub title: Option<String>,
}

/// What the callback and the owner share.
struct WatchState {
    on_went: Box<dyn Fn(Went) + Send + Sync>,
    suspicions: AtomicU64,
    others: AtomicU64,
    targeted: AtomicBool,
    stopped: AtomicBool,
}

/// What the observer's `refcon` points at: the shared state and the elements matched to the
/// target. Owned by the watch thread and boxed so that its address is fixed; the box is
/// reclaimed after the observer is released, so no callback outlives it.
struct Watcher {
    state: Arc<WatchState>,
    targets: Vec<CFRetained<AXUIElement>>,
}

/// An accessibility observer on one application, calling back whenever a window of that
/// application is hidden, minimised or destroyed. Dropping it stops the observer.
pub struct HideWatch {
    /// Stops the watch thread's loop when dropped; kept for that alone.
    _run_loop: StopHandle,
    state: Arc<WatchState>,
}

impl std::fmt::Debug for HideWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HideWatch")
            .field("targeted", &self.targeted())
            .field("suspicions", &self.suspicions())
            .field("others", &self.others())
            .finish_non_exhaustive()
    }
}

impl HideWatch {
    /// Watch the application with process id `pid` for `target`, one of its windows. `on_went`
    /// runs on the watch's own thread, once per notification, and must be quick.
    ///
    /// Blocks for the accessibility round trips that register the observer (a few
    /// milliseconds; call it off the async runtime).
    pub fn start(
        pid: i32,
        target: TargetWindow,
        on_went: impl Fn(Went) + Send + Sync + 'static,
    ) -> Result<Self, AxError> {
        // SAFETY: the documented no-argument query; it takes and returns nothing owned.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(AxError::NotTrusted);
        }
        let state = Arc::new(WatchState {
            on_went: Box::new(on_went),
            suspicions: AtomicU64::new(0),
            others: AtomicU64::new(0),
            targeted: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = Arc::clone(&state);
        std::thread::Builder::new()
            .name(format!("slopty-ax-{pid}"))
            .spawn(move || serve(pid, &target, &worker, &ready_tx))
            .map_err(|_spawn| AxError::Thread)?;
        let run_loop = ready_rx.recv().map_err(|_ended| AxError::Thread)??;
        Ok(Self { _run_loop: run_loop, state })
    }

    /// Notifications so far that meant the target went ([`Went::Target`]).
    #[must_use]
    pub fn suspicions(&self) -> u64 {
        self.state.suspicions.load(Ordering::Relaxed)
    }

    /// Notifications so far for another window of the application ([`Went::Other`]).
    #[must_use]
    pub fn others(&self) -> u64 {
        self.state.others.load(Ordering::Relaxed)
    }

    /// Whether the target was matched to its element, so that other windows are told apart
    /// from it. `false` means every window of the application counts as the target.
    #[must_use]
    pub fn targeted(&self) -> bool {
        self.state.targeted.load(Ordering::Acquire)
    }
}

impl Drop for HideWatch {
    fn drop(&mut self) {
        // The flag first, so the loop that the handle's drop wakes sees it and returns.
        self.state.stopped.store(true, Ordering::Release);
    }
}

/// The watch thread: register, report, run the loop until stopped, then release the observer
/// before the state it points at can go.
fn serve(
    pid: i32,
    target: &TargetWindow,
    state: &Arc<WatchState>,
    ready: &mpsc::SyncSender<Result<StopHandle, AxError>>,
) {
    let observer = match create_observer(pid) {
        Ok(observer) => observer,
        Err(e) => {
            let _owner_gone = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: the documented constructor for an application element; it returns +1.
    let app = unsafe { AXUIElement::new_application(pid) };
    let windows = windows_of(&app);
    let targets: Vec<_> = windows.iter().filter(|w| matches(w, target)).cloned().collect();
    state.targeted.store(!targets.is_empty(), Ordering::Release);
    // The `refcon` for every registration: a fixed address the callbacks read until the
    // observer is released, and reclaimed below.
    let refcon = Box::into_raw(Box::new(Watcher { state: Arc::clone(state), targets }));
    let registered = register(&observer, &app, &windows, refcon.cast::<c_void>());
    let report = match (registered, CFRunLoop::current()) {
        (Err(e), _) => Err(e),
        (Ok(()), None) => Err(AxError::Thread),
        (Ok(()), Some(run_loop)) => Ok(run_loop),
    };
    let run_loop = match report {
        Ok(run_loop) => run_loop,
        Err(e) => {
            drop(observer);
            // SAFETY: the pointer came from `Box::into_raw` above, no callback can run once the
            // observer is released, and this is the only place that reclaims it on this path.
            drop(unsafe { Box::from_raw(refcon) });
            let _owner_gone = ready.send(Err(e));
            return;
        }
    };
    // SAFETY: framework-provided constant string.
    let mode = unsafe { kCFRunLoopDefaultMode };
    // SAFETY: the observer is live and its source is documented to go on the run loop of the
    // thread that will service it, which is this one (`AXObserverGetRunLoopSource`).
    let source = unsafe { observer.run_loop_source() };
    run_loop.add_source(Some(&source), mode);
    if ready.send(Ok(StopHandle::new(&run_loop))).is_ok() {
        while !state.stopped.load(Ordering::Acquire) {
            CFRunLoop::run_in_mode(mode, RUN_SLICE_SECS, false);
        }
    }
    // No callback runs once the observer is released, so what the `refcon` pointed at can go.
    drop(observer);
    // SAFETY: the pointer came from `Box::into_raw` above and is reclaimed exactly once, here,
    // after the last thing that could read it is gone.
    drop(unsafe { Box::from_raw(refcon) });
}

/// The observer for one process.
fn create_observer(pid: i32) -> Result<CFRetained<AXObserver>, AxError> {
    let mut raw: *mut AXObserver = ptr::null_mut();
    // SAFETY: `raw` is a valid out-pointer for one observer and the callback has the C
    // signature the API documents (`AXObserverCreate`, +1 on success).
    let status = unsafe { AXObserver::create(pid, Some(on_notification), NonNull::from(&mut raw)) };
    match (status, NonNull::new(raw)) {
        // SAFETY: the call returned success, so it stored a +1 reference here.
        (AXError::Success, Some(observer)) => Ok(unsafe { CFRetained::from_raw(observer) }),
        (status, _) => Err(AxError::Observer(status.0)),
    }
}

/// Register for everything that means "a window went".
fn register(
    observer: &AXObserver,
    app: &AXUIElement,
    windows: &[CFRetained<AXUIElement>],
    refcon: *mut c_void,
) -> Result<(), AxError> {
    for name in [WINDOW_MINIATURIZED, APPLICATION_HIDDEN, WINDOW_CREATED] {
        // SAFETY: `app` and `observer` are live; `refcon` points at the watcher, which the
        // watch thread keeps alive until after the observer is released (`serve`).
        let status = unsafe { observer.add_notification(app, &CFString::from_str(name), refcon) };
        if status != AXError::Success {
            return Err(AxError::Observer(status.0));
        }
    }
    // Destruction is posted by the window, not the application, so each window is registered
    // on its own; a window that vanishes between the listing and the call is simply skipped.
    let destroyed = CFString::from_str(ELEMENT_DESTROYED);
    for window in windows {
        // SAFETY: as above; the observer keeps its own reference to the element.
        let _may_be_gone = unsafe { observer.add_notification(window, &destroyed, refcon) };
    }
    Ok(())
}

/// Whether `window` is the target: the same frame within [`FRAME_TOLERANCE`] and, when both
/// sides have one, the same title.
fn matches(window: &AXUIElement, target: &TargetWindow) -> bool {
    let (Some(origin), Some(size)) = (position_of(window), size_of(window)) else {
        return false;
    };
    let near = |a: f64, b: f64| (a - b).abs() <= FRAME_TOLERANCE;
    let same_frame = near(origin.x, target.bounds.x)
        && near(origin.y, target.bounds.y)
        && near(size.width, target.bounds.w)
        && near(size.height, target.bounds.h);
    same_frame
        && match (title_of(window), &target.title) {
            (Some(theirs), Some(ours)) => theirs == *ours,
            _ => true,
        }
}

/// One attribute of an element, or none if it will not say.
fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let attribute = CFString::from_str(name);
    let mut value: *const CFType = ptr::null();
    // SAFETY: `element` is live and `value` is a valid out-pointer for one `CFTypeRef`
    // (`AXUIElementCopyAttributeValue`, +1 on success).
    let status = unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut value)) };
    let value = NonNull::new(value.cast_mut())?;
    // SAFETY: a non-null result is the +1 reference the call stored.
    let value = unsafe { CFRetained::from_raw(value) };
    (status == AXError::Success).then_some(value)
}

/// `AXPosition`: the window's top-left corner in screen points.
fn position_of(window: &AXUIElement) -> Option<CGPoint> {
    let value = attribute(window, POSITION_ATTRIBUTE)?.downcast::<AXValue>().ok()?;
    let mut point = CGPoint::default();
    // SAFETY: `point` is a valid out-pointer for the type asked for (`AXValueGetValue`).
    let ok = unsafe { value.value(AXValueType::CGPoint, NonNull::from(&mut point).cast()) };
    ok.then_some(point)
}

/// `AXSize`: the window's size in points.
fn size_of(window: &AXUIElement) -> Option<CGSize> {
    let value = attribute(window, SIZE_ATTRIBUTE)?.downcast::<AXValue>().ok()?;
    let mut size = CGSize::default();
    // SAFETY: `size` is a valid out-pointer for the type asked for (`AXValueGetValue`).
    let ok = unsafe { value.value(AXValueType::CGSize, NonNull::from(&mut size).cast()) };
    ok.then_some(size)
}

/// `AXTitle`, if the window has a non-empty one.
fn title_of(window: &AXUIElement) -> Option<String> {
    let title = attribute(window, TITLE_ATTRIBUTE)?.downcast::<CFString>().ok()?.to_string();
    (!title.is_empty()).then_some(title)
}

/// The application's windows right now, or none if it will not say.
fn windows_of(app: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let Some(value) = attribute(app, WINDOWS_ATTRIBUTE) else {
        return Vec::new();
    };
    let Ok(windows) = value.downcast::<CFArray>() else {
        return Vec::new();
    };
    // SAFETY: `kAXWindowsAttribute` is documented to be an array of `AXUIElement`.
    let windows: CFRetained<CFArray<AXUIElement>> = unsafe { CFRetained::cast_unchecked(windows) };
    windows.to_vec()
}

/// The observer callback: a new window is put under watch; anything else went, and is the
/// target if it is one of the matched elements, the application being hidden, or anything at
/// all when nothing was matched.
unsafe extern "C-unwind" fn on_notification(
    observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    name: NonNull<CFString>,
    refcon: *mut c_void,
) {
    // SAFETY: `refcon` is the `Watcher` the watch thread boxed for this observer, alive until
    // after the observer is released (`serve`).
    let watcher = unsafe { &*refcon.cast_const().cast::<Watcher>() };
    // SAFETY: the notification name is live for the duration of the callback.
    let name = unsafe { name.as_ref() }.to_string();
    // SAFETY: so is the element it is about.
    let element = unsafe { element.as_ref() };
    if name == WINDOW_CREATED {
        // SAFETY: the observer is live for the duration of the callback.
        let observer = unsafe { observer.as_ref() };
        let destroyed = CFString::from_str(ELEMENT_DESTROYED);
        // SAFETY: as in `register`: live element and observer, `refcon` the same pointer.
        let _may_be_gone = unsafe { observer.add_notification(element, &destroyed, refcon) };
        return;
    }
    let state = &watcher.state;
    let is_target = |candidate: &CFRetained<AXUIElement>| {
        let (a, b): (&CFType, &CFType) = (candidate, element);
        a == b
    };
    let went = if name == APPLICATION_HIDDEN
        || watcher.targets.is_empty()
        || watcher.targets.iter().any(is_target)
    {
        Went::Target
    } else {
        Went::Other
    };
    match went {
        Went::Target => state.suspicions.fetch_add(1, Ordering::Relaxed),
        Went::Other => state.others.fetch_add(1, Ordering::Relaxed),
    };
    (state.on_went)(went);
}
