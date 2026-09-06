//! The accessibility API's word that a window of the target's application went away, which it
//! gives the moment AppKit orders the window out — about 260 ms before the window list does
//! (MEASUREMENTS.md, "which signal knows first").
//!
//! It cannot name the window: the public API exposes no window number on an `AXUIElement`. So
//! what a [`HideWatch`] reports is *suspicion* — some window of that application was hidden,
//! minimised or destroyed — and the stream holds frames on it until the window list confirms
//! or denies (DECISIONS.md, "The accessibility API knows about a hide 260 ms before core
//! graphics does").
//!
//! One thread per watch runs a `CFRunLoop` for the observer; the callback does nothing but
//! count and call back, so the run loop is never behind work.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use objc2_application_services::{AXError, AXIsProcessTrusted, AXObserver, AXUIElement};
use objc2_core_foundation::{
    CFArray, CFRetained, CFRunLoop, CFString, CFType, Type as _, kCFRunLoopDefaultMode,
};

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

/// What the callback and the owner share. The callback reaches it through the observer's
/// `refcon`, which is why the watch thread keeps its own `Arc` until the observer is gone.
struct WatchState {
    on_suspicion: Box<dyn Fn() + Send + Sync>,
    suspicions: AtomicU64,
    stopped: AtomicBool,
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
        f.debug_struct("HideWatch").field("suspicions", &self.suspicions()).finish_non_exhaustive()
    }
}

impl HideWatch {
    /// Watch the application with process id `pid`. `on_suspicion` runs on the watch's own
    /// thread, once per notification, and must be quick.
    ///
    /// Blocks for the accessibility round trips that register the observer (a few
    /// milliseconds; call it off the async runtime).
    pub fn start(
        pid: i32,
        on_suspicion: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, AxError> {
        // SAFETY: the documented no-argument query; it takes and returns nothing owned.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(AxError::NotTrusted);
        }
        let state = Arc::new(WatchState {
            on_suspicion: Box::new(on_suspicion),
            suspicions: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = Arc::clone(&state);
        std::thread::Builder::new()
            .name(format!("slopty-ax-{pid}"))
            .spawn(move || serve(pid, &worker, &ready_tx))
            .map_err(|_spawn| AxError::Thread)?;
        let run_loop = ready_rx.recv().map_err(|_ended| AxError::Thread)??;
        Ok(Self { _run_loop: run_loop, state })
    }

    /// Notifications received so far.
    #[must_use]
    pub fn suspicions(&self) -> u64 {
        self.state.suspicions.load(Ordering::Relaxed)
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
fn serve(pid: i32, state: &Arc<WatchState>, ready: &mpsc::SyncSender<Result<StopHandle, AxError>>) {
    let observer = match register(pid, state) {
        Ok(observer) => observer,
        Err(e) => {
            let _owner_gone = ready.send(Err(e));
            return;
        }
    };
    let Some(run_loop) = CFRunLoop::current() else {
        let _owner_gone = ready.send(Err(AxError::Thread));
        return;
    };
    // SAFETY: framework-provided constant string.
    let mode = unsafe { kCFRunLoopDefaultMode };
    // SAFETY: the observer is live and its source is documented to go on the run loop of the
    // thread that will service it, which is this one (`AXObserverGetRunLoopSource`).
    let source = unsafe { observer.run_loop_source() };
    run_loop.add_source(Some(&source), mode);
    if ready.send(Ok(StopHandle::new(&run_loop))).is_err() {
        return;
    }
    while !state.stopped.load(Ordering::Acquire) {
        CFRunLoop::run_in_mode(mode, RUN_SLICE_SECS, false);
    }
    // No callback runs once the observer is released, so the `refcon` it carried — this
    // thread's `Arc`, dropped when `state` goes out of scope in the caller — is safe to let go.
    drop(observer);
}

/// Create the observer and register for everything that means "a window went".
fn register(pid: i32, state: &Arc<WatchState>) -> Result<CFRetained<AXObserver>, AxError> {
    let mut raw: *mut AXObserver = ptr::null_mut();
    // SAFETY: `raw` is a valid out-pointer for one observer and the callback has the C
    // signature the API documents (`AXObserverCreate`, +1 on success).
    let status = unsafe { AXObserver::create(pid, Some(on_notification), NonNull::from(&mut raw)) };
    let observer = match (status, NonNull::new(raw)) {
        (AXError::Success, Some(observer)) => observer,
        (status, _) => return Err(AxError::Observer(status.0)),
    };
    // SAFETY: the call above returned success, so it stored a +1 reference here.
    let observer = unsafe { CFRetained::from_raw(observer) };
    let refcon = Arc::as_ptr(state).cast_mut().cast::<c_void>();
    // SAFETY: the documented constructor for an application element; it returns +1.
    let app = unsafe { AXUIElement::new_application(pid) };
    for name in [WINDOW_MINIATURIZED, APPLICATION_HIDDEN, WINDOW_CREATED] {
        // SAFETY: `app` and `observer` are live; `refcon` points at the watch state, which the
        // watch thread keeps alive until after the observer is released (`serve`).
        let status = unsafe { observer.add_notification(&app, &CFString::from_str(name), refcon) };
        if status != AXError::Success {
            return Err(AxError::Observer(status.0));
        }
    }
    // Destruction is posted by the window, not the application, so each window is registered
    // on its own; a window that vanishes between the listing and the call is simply skipped.
    let destroyed = CFString::from_str(ELEMENT_DESTROYED);
    for window in windows_of(&app) {
        // SAFETY: as above; the observer keeps its own reference to the element.
        let _may_be_gone = unsafe { observer.add_notification(&window, &destroyed, refcon) };
    }
    Ok(observer)
}

/// The application's windows right now, or none if it will not say.
fn windows_of(app: &AXUIElement) -> Vec<CFRetained<AXUIElement>> {
    let attribute = CFString::from_str(WINDOWS_ATTRIBUTE);
    let mut value: *const CFType = ptr::null();
    // SAFETY: `app` is a live element and `value` is a valid out-pointer for one `CFTypeRef`
    // (`AXUIElementCopyAttributeValue`, +1 on success).
    let status = unsafe { app.copy_attribute_value(&attribute, NonNull::from(&mut value)) };
    let Some(value) = NonNull::new(value.cast_mut()) else {
        return Vec::new();
    };
    // SAFETY: a non-null result is the +1 reference the call stored.
    let value = unsafe { CFRetained::from_raw(value) };
    if status != AXError::Success {
        return Vec::new();
    }
    let Ok(windows) = value.downcast::<CFArray>() else {
        return Vec::new();
    };
    // SAFETY: `kAXWindowsAttribute` is documented to be an array of `AXUIElement`.
    let windows: CFRetained<CFArray<AXUIElement>> = unsafe { CFRetained::cast_unchecked(windows) };
    windows.to_vec()
}

/// The observer callback: a new window is put under watch, anything else is a suspicion.
unsafe extern "C-unwind" fn on_notification(
    observer: NonNull<AXObserver>,
    element: NonNull<AXUIElement>,
    name: NonNull<CFString>,
    refcon: *mut c_void,
) {
    // SAFETY: `refcon` is the pointer `register` took from the watch thread's `Arc`, which
    // outlives the observer (`serve`).
    let state = unsafe { &*refcon.cast_const().cast::<WatchState>() };
    // SAFETY: the notification name is live for the duration of the callback.
    let name = unsafe { name.as_ref() };
    if name.to_string() == WINDOW_CREATED {
        // SAFETY: the observer is live for the duration of the callback.
        let observer = unsafe { observer.as_ref() };
        // SAFETY: so is the new window's element.
        let element = unsafe { element.as_ref() };
        let destroyed = CFString::from_str(ELEMENT_DESTROYED);
        // SAFETY: as in `register`: live element and observer, `refcon` the same pointer.
        let _may_be_gone = unsafe { observer.add_notification(element, &destroyed, refcon) };
        return;
    }
    state.suspicions.fetch_add(1, Ordering::Relaxed);
    (state.on_suspicion)();
}
