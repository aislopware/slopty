//! A finger as UIKit's own touch delivery makes one, for the app's self-test (iOS, `e2e`).
//!
//! The test socket's other touches are handed to GPUI's view at its UIKit boundary, so nothing
//! else sees them. A page's web view and its scroll views sit beside that view, and only a touch
//! UIKit itself delivers reaches them: a `UITouch` hit-tested where it lands and sent through
//! `UIApplication` in its touches event with the digitizer event UIKit reads it from. UIKit's
//! hit testing and gesture recognisers then decide where it goes, as with a person's finger.
//! The private setters are the ones KIF and UIKit's own tests use (as gpui-fast's
//! `native_scroll_test` does). The app sends these to itself, in the simulator, under test.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadMarker, class, msg_send, sel};
use objc2_core_foundation::CGPoint;
use objc2_foundation::NSProcessInfo;

// `IOHIDEvent`'s creators (IOKit's `IOHIDEvent.h`, exported though not in the SDK): the
// digitizer events UIKit reads a touch's position and contact from.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventCreateDigitizerEvent(
        allocator: *const c_void,
        time: u64,
        transducer: u32,
        index: u32,
        identity: u32,
        mask: u32,
        buttons: u32,
        x: f64,
        y: f64,
        z: f64,
        tip: f64,
        barrel: f64,
        range: u8,
        touch: u8,
        options: u32,
    ) -> *mut c_void;
    fn IOHIDEventCreateDigitizerFingerEventWithQuality(
        allocator: *const c_void,
        time: u64,
        index: u32,
        identity: u32,
        mask: u32,
        x: f64,
        y: f64,
        z: f64,
        tip: f64,
        twist: f64,
        minor: f64,
        major: f64,
        quality: f64,
        density: f64,
        irregularity: f64,
        range: u8,
        touch: u8,
        options: u32,
    ) -> *mut c_void;
    fn IOHIDEventAppendEvent(event: *mut c_void, child: *mut c_void, options: u32);
    fn IOHIDEventSetIntegerValue(event: *mut c_void, field: u32, value: i32);
    fn mach_absolute_time() -> u64;
    fn CFRelease(object: *const c_void);
}

/// An `IOHIDEventRef`, typed as UIKit's selectors declare it.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct HidEvent(*mut c_void);

// SAFETY: a pointer to the opaque `struct __IOHIDEvent`, as `IOHIDEventRef` is declared.
unsafe impl objc2::encode::Encode for HidEvent {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__IOHIDEvent", &[]));
}

/// `kIOHIDDigitizerTransducerTypeHand`, `kIOHIDDigitizerEvent{Range,Touch,Position}` and
/// `kIOHIDEventFieldDigitizerIsDisplayIntegrated` (IOKit's `IOHIDEventTypes.h`).
const TRANSDUCER_HAND: u32 = 3;
const EVENT_RANGE: u32 = 1 << 0;
const EVENT_TOUCH: u32 = 1 << 1;
const EVENT_POSITION: u32 = 1 << 2;
const FIELD_DISPLAY_INTEGRATED: u32 = (11 << 16) | 25;

/// `UITouchPhase`.
const BEGAN: isize = 0;
const MOVED: isize = 1;
const ENDED: isize = 3;

/// A hand's digitizer event holding one finger at `at` (window points) in `phase`; the caller
/// releases it.
fn hid_event(at: CGPoint, phase: isize) -> HidEvent {
    let touching = u8::from(phase != ENDED);
    let mask = if phase == MOVED { EVENT_POSITION } else { EVENT_RANGE | EVENT_TOUCH };
    // SAFETY: reads the host's clock.
    let time = unsafe { mach_absolute_time() };
    // SAFETY: IOKit's creator makes a new event the caller owns.
    let hand = unsafe {
        IOHIDEventCreateDigitizerEvent(
            std::ptr::null(),
            time,
            TRANSDUCER_HAND,
            0,
            0,
            EVENT_TOUCH,
            0,
            0.,
            0.,
            0.,
            0.,
            0.,
            0,
            touching,
            0,
        )
    };
    // SAFETY: `hand` is the live event just made.
    unsafe { IOHIDEventSetIntegerValue(hand, FIELD_DISPLAY_INTEGRATED, 1) };
    // SAFETY: as for the hand.
    let finger = unsafe {
        IOHIDEventCreateDigitizerFingerEventWithQuality(
            std::ptr::null(),
            time,
            1,
            2,
            mask,
            at.x,
            at.y,
            0.,
            0.,
            0.,
            5.,
            5.,
            1.,
            1.,
            1.,
            touching,
            touching,
            0,
        )
    };
    // SAFETY: `finger` is the live event just made.
    unsafe { IOHIDEventSetIntegerValue(finger, FIELD_DISPLAY_INTEGRATED, 1) };
    // SAFETY: both live; the hand retains the finger it is given.
    unsafe { IOHIDEventAppendEvent(hand, finger, 0) };
    // SAFETY: the finger was made above and is ours to release; the hand keeps its own hold.
    unsafe { CFRelease(finger) };
    HidEvent(hand)
}

/// One finger on the app's key window, from where it landed until it lifts.
pub struct Finger {
    touch: Retained<AnyObject>,
    _main: MainThreadMarker,
}

impl Finger {
    /// A finger landing at `(x, y)`, the key window's points.
    ///
    /// # Errors
    ///
    /// Off the main thread, or with no window to land on.
    pub fn down(x: f64, y: f64) -> Result<Self, String> {
        let main = MainThreadMarker::new().ok_or("a finger lands on the main thread")?;
        if let Some(missing) = missing_private() {
            return Err(format!("UIKit no longer has {missing}"));
        }
        let at = CGPoint { x, y };
        let window = key_window().ok_or("no window to land on")?;
        // SAFETY: a new `UITouch`, on the main thread.
        let touch: Retained<AnyObject> = unsafe { msg_send![class!(UITouch), new] };
        // SAFETY: `UITouch`'s own setter, on the touch just made.
        let () = unsafe { msg_send![&*touch, setWindow: &*window] };
        // SAFETY: as above.
        let () = unsafe { msg_send![&*touch, setTapCount: 1_usize] };
        // SAFETY: as above.
        let () = unsafe { msg_send![&*touch, _setLocationInWindow: at, resetPrevious: true] };
        // SAFETY: hit testing the app's own window on the main thread; the view may be nil.
        let view: *mut AnyObject =
            unsafe { msg_send![&*window, hitTest: at, withEvent: std::ptr::null::<AnyObject>()] };
        // SAFETY: as above; a nil view is what UIKit gives a touch that lands on nothing.
        let () = unsafe { msg_send![&*touch, setView: view] };
        // SAFETY: an `NSObject` query.
        let gesture_view: bool =
            unsafe { msg_send![&*touch, respondsToSelector: sel!(setGestureView:)] };
        if gesture_view {
            // SAFETY: the setter it just said it has.
            let () = unsafe { msg_send![&*touch, setGestureView: view] };
        }
        let finger = Self { touch, _main: main };
        finger.send(at, BEGAN);
        Ok(finger)
    }

    /// The finger, moved to `(x, y)`.
    pub fn moved(&self, x: f64, y: f64) {
        self.send(CGPoint { x, y }, MOVED);
    }

    /// The finger, lifted at `(x, y)`.
    pub fn lift(self, x: f64, y: f64) {
        self.send(CGPoint { x, y }, ENDED);
    }

    /// The touch at `at` in `phase`, sent through `UIApplication` in its one touches event,
    /// emptied and given this touch and its digitizer event.
    fn send(&self, at: CGPoint, phase: isize) {
        let hid = hid_event(at, phase);
        let touch = &*self.touch;
        let now = NSProcessInfo::processInfo().systemUptime();
        // SAFETY: `UITouch`'s own setters on this live touch, on the main thread (`_main`).
        let () = unsafe { msg_send![touch, setTimestamp: now] };
        if phase != BEGAN {
            // SAFETY: as above.
            let () = unsafe { msg_send![touch, _setLocationInWindow: at, resetPrevious: false] };
        }
        // SAFETY: as above.
        let () = unsafe { msg_send![touch, setPhase: phase] };
        // SAFETY: as above; the touch retains the event it is given.
        let () = unsafe { msg_send![touch, _setHidEvent: hid] };
        // SAFETY: the shared application, on the main thread.
        let application: *mut AnyObject =
            unsafe { msg_send![class!(UIApplication), sharedApplication] };
        // SAFETY: the application's touches event, which UIKit reuses for every delivery.
        let event: *mut AnyObject = unsafe { msg_send![application, _touchesEvent] };
        // SAFETY: the touches event's own reset, before it is given this touch.
        let () = unsafe { msg_send![event, _clearTouches] };
        // SAFETY: as above; the event retains its digitizer event.
        let () = unsafe { msg_send![event, _setHIDEvent: hid] };
        // SAFETY: as above.
        let () = unsafe { msg_send![event, _addTouch: touch, forDelayedDelivery: false] };
        // SAFETY: UIKit's own delivery of the event it holds.
        let () = unsafe { msg_send![application, sendEvent: event] };
        // SAFETY: made by `hid_event` for this send; the touch and the event hold their own.
        unsafe { CFRelease(hid.0) };
    }
}

/// The first private method a finger needs that UIKit lacks, by name; `None` with all there.
/// Checked before every finger lands, so a UIKit that dropped one fails the test by name.
fn missing_private() -> Option<&'static str> {
    let touch = [
        (
            "-[UITouch _setLocationInWindow:resetPrevious:]",
            sel!(_setLocationInWindow:resetPrevious:),
        ),
        ("-[UITouch _setHidEvent:]", sel!(_setHidEvent:)),
        ("-[UITouch setWindow:]", sel!(setWindow:)),
        ("-[UITouch setView:]", sel!(setView:)),
        ("-[UITouch setPhase:]", sel!(setPhase:)),
        ("-[UITouch setTimestamp:]", sel!(setTimestamp:)),
        ("-[UITouch setTapCount:]", sel!(setTapCount:)),
    ];
    let application = [("-[UIApplication _touchesEvent]", sel!(_touchesEvent))];
    let event = [
        ("-[UITouchesEvent _clearTouches]", sel!(_clearTouches)),
        ("-[UITouchesEvent _setHIDEvent:]", sel!(_setHIDEvent:)),
        ("-[UITouchesEvent _addTouch:forDelayedDelivery:]", sel!(_addTouch:forDelayedDelivery:)),
    ];
    let has = |class: &objc2::runtime::AnyClass, selector| class.responds_to(selector);
    let touches = class!(UITouch);
    let applications = class!(UIApplication);
    let events = objc2::runtime::AnyClass::get(c"UITouchesEvent");
    touch
        .into_iter()
        .find(|(_, selector)| !has(touches, *selector))
        .or_else(|| application.into_iter().find(|(_, selector)| !has(applications, *selector)))
        .or_else(|| match events {
            Some(events) => event.into_iter().find(|(_, selector)| !has(events, *selector)),
            None => Some(("UITouchesEvent", sel!(class))),
        })
        .map(|(name, _)| name)
}

/// The app's key window, else its first.
fn key_window() -> Option<Retained<AnyObject>> {
    // SAFETY: the shared application, on the main thread (the caller's marker).
    let application: *mut AnyObject =
        unsafe { msg_send![class!(UIApplication), sharedApplication] };
    if application.is_null() {
        return None;
    }
    // SAFETY: `keyWindow`, nil when none is key.
    let key: Option<Retained<AnyObject>> = unsafe { msg_send![application, keyWindow] };
    key.or_else(|| {
        // SAFETY: the application's windows, an array.
        let windows: Option<Retained<AnyObject>> = unsafe { msg_send![application, windows] };
        // SAFETY: `NSArray`'s `firstObject`, nil when empty.
        windows.and_then(|windows| unsafe { msg_send![&*windows, firstObject] })
    })
}
