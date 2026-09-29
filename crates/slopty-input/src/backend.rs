//! Where the injector's events go.
//!
//! [`Injector`](crate::Injector) decides *what* to post — the event kind, the global point,
//! the modifier flags, the route — and hands a [`Post`] to a [`Backend`]. [`System`] turns it
//! into a `CGEvent` and posts it; [`Recorder`] keeps it in a `Vec` so every decision the
//! injector makes can be asserted in a unit test without Accessibility access and without a
//! single real event reaching the desktop.

use objc2_app_kit::{
    NSApplicationActivationOptions, NSEvent, NSEventModifierFlags, NSEventType,
    NSRunningApplication,
};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGKeyCode, CGMouseButton, CGScrollEventUnit,
};
use objc2_foundation::NSPoint;
use slopty_capture::Rect;
use slopty_platform::keyboard::nx;
use slopty_proto::screen::{CaptureTarget, MediaKey, ScrollPhase};

use crate::InputError;
use crate::injector::SharedCaps;

/// What every event this worker posts carries in `kCGEventSourceUserData`, so the worker can
/// tell its own events from the person's at its desk ("SLOP").
pub const SLOPTY_EVENT: i64 = 0x534c_4f50;

/// `kVK_Space` (`<HIToolbox/Events.h>`): the key committed text rides on, as Chrome Remote
/// Desktop's does. Never keycode 0, which some input methods read as the A key.
const TEXT_CARRIER: CGKeyCode = 0x31;

/// A media key's `data1`: the key in bits 16–31, its state in bits 8–15.
#[must_use]
pub const fn media_data1(key: MediaKey, down: bool) -> isize {
    let key = match key {
        MediaKey::PlayPause => nx::KEYTYPE_PLAY,
        MediaKey::Next => nx::KEYTYPE_NEXT,
        MediaKey::Previous => nx::KEYTYPE_PREVIOUS,
    };
    (key << 16) | ((if down { nx::KEYDOWN } else { nx::KEYUP }) << 8)
}

/// `CGScrollPhase` values (`IOKit/hidsystem/IOLLEvent.h`).
mod scroll_phase {
    pub(super) const BEGAN: i64 = 1;
    pub(super) const CHANGED: i64 = 2;
    pub(super) const ENDED: i64 = 4;
    pub(super) const CANCELLED: i64 = 8;
    pub(super) const MAY_BEGIN: i64 = 128;
}

/// `CGMomentumScrollPhase` values.
mod momentum_phase {
    pub(super) const NONE: i64 = 0;
    pub(super) const BEGIN: i64 = 1;
    pub(super) const CONTINUE: i64 = 2;
    pub(super) const END: i64 = 3;
}

/// Where an event goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// `CGEventPostToPid`: the owning app, whether or not it is frontmost.
    Pid(i32),
    /// The HID tap: the system, as if from real hardware.
    Hid,
}

/// One event the injector decided on, before CoreGraphics builds it.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    /// Pointer move, drag, press or release.
    Mouse {
        /// `MouseMoved`, `LeftMouseDragged`, `LeftMouseDown`…
        kind: CGEventType,
        /// Global display point.
        at: CGPoint,
        /// The button the event is about.
        button: CGMouseButton,
        /// `MouseEventButtonNumber` (0 left, 1 right, 2 middle, 3 back, 4 forward).
        number: i64,
        /// `MouseEventClickState`; 0 for moves.
        clicks: i64,
    },
    /// Wheel or trackpad scroll.
    Scroll {
        /// Global display point.
        at: CGPoint,
        /// Horizontal delta as the client saw it.
        dx: f32,
        /// Vertical delta as the client saw it.
        dy: f32,
        /// Pixel (trackpad) rather than line (wheel) units.
        precise: bool,
        /// Gesture phase.
        phase: ScrollPhase,
        /// Momentum phase.
        momentum: ScrollPhase,
    },
    /// Key press, repeat or release, by position only: the target's layout makes the
    /// character.
    Key {
        /// Virtual keycode.
        vk: CGKeyCode,
        /// Press or repeat (`true`) vs release.
        down: bool,
        /// A bare modifier: posts as `FlagsChanged`.
        modifier: bool,
        /// Auto-repeat.
        repeat: bool,
    },
    /// Committed text, at most [`crate::text::MOST_UNITS`] UTF-16 units, on the Space key's
    /// press or release with `CGEventKeyboardSetUnicodeString`.
    Text {
        /// The text.
        text: String,
        /// The press (`true`) or the release.
        down: bool,
    },
    /// A media key: a system-defined event (`NSSystemDefined`, subtype 8).
    Media {
        /// Which.
        key: MediaKey,
        /// Pressed, or let go.
        down: bool,
    },
}

/// An [`Event`] with its route and modifier flags.
#[derive(Clone, PartialEq, Debug)]
pub struct Post {
    /// Where it goes.
    pub route: Route,
    /// Modifier flags on the event.
    pub flags: CGEventFlags,
    /// The event.
    pub event: Event,
}

/// The worker-side services an injector needs: window geometry, app activation, posting, and
/// the Caps Lock state.
pub trait Backend {
    /// The pid owning a window target; `None` for displays or unknown windows.
    fn owner_pid(&self, target: CaptureTarget) -> Option<i32>;
    /// Current bounds of the target in global display points.
    fn bounds(&mut self, target: CaptureTarget) -> Option<Rect>;
    /// Whether the app with this pid is the active application.
    fn is_active(&mut self, pid: i32) -> bool;
    /// Bring the app with this pid to the front.
    fn activate(&mut self, pid: i32) -> Result<(), InputError>;
    /// Post one event.
    fn post(&mut self, post: Post) -> Result<(), InputError>;
    /// Whether Caps Lock is on; `None` when the HID system would not say, as a stand-in that
    /// keeps no lock answers.
    fn caps_lock(&mut self) -> Option<bool> {
        None
    }
    /// Set Caps Lock's state, the lock itself rather than a key. A stand-in that keeps no lock
    /// takes it and does nothing.
    fn set_caps_lock(&mut self, _on: bool) -> Result<(), InputError> {
        Ok(())
    }
    /// The streams holding this backend's Caps Lock: the worker's one lock, shared by every
    /// injector.
    fn caps_claims(&self) -> SharedCaps {
        std::sync::Arc::clone(&WORKER_CAPS)
    }
}

/// The worker's Caps Lock claim ([`crate::CapsClaims`]).
static WORKER_CAPS: std::sync::LazyLock<SharedCaps> = std::sync::LazyLock::new(SharedCaps::default);

/// The real thing: CoreGraphics events, AppKit activation, the WindowServer's window list.
///
/// Its events come from a `CGEventSource` with its own private state, one per input thread (so
/// one per stream): the modifiers the client holds never mix with the keys the person at the
/// worker holds, and theirs never leak into the client's.
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

thread_local! {
    /// This thread's event source, made on its first post.
    static SOURCE: Option<CFRetained<CGEventSource>> =
        CGEventSource::new(CGEventSourceStateID::Private);
}

impl Backend for System {
    fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
        match target {
            CaptureTarget::Window(id) => slopty_capture::window_owner_pid(id),
            CaptureTarget::Display(_) => None,
        }
    }

    fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
        slopty_capture::target_bounds(target)
    }

    fn is_active(&mut self, pid: i32) -> bool {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| app.isActive())
    }

    fn activate(&mut self, pid: i32) -> Result<(), InputError> {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .ok_or(InputError::NoApplication)?;
        let _activated =
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        Ok(())
    }

    fn post(&mut self, post: Post) -> Result<(), InputError> {
        let event = SOURCE.with(|source| build(&post, source.as_deref()))?;
        match post.route {
            Route::Pid(pid) => CGEvent::post_to_pid(pid, Some(&event)),
            Route::Hid => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event)),
        }
        Ok(())
    }

    fn caps_lock(&mut self) -> Option<bool> {
        hid::caps_lock()
    }

    fn set_caps_lock(&mut self, on: bool) -> Result<(), InputError> {
        hid::set_caps_lock(on)
    }
}

/// Build the `CGEvent` for a [`Post`] from `source`, tagged [`SLOPTY_EVENT`].
///
/// # Errors
///
/// CoreGraphics or AppKit would not make the event.
pub fn build(
    post: &Post,
    source: Option<&CGEventSource>,
) -> Result<CFRetained<CGEvent>, InputError> {
    let event = match &post.event {
        Event::Mouse { kind, at, button, number, clicks } => {
            let event =
                CGEvent::new_mouse_event(source, *kind, *at, *button).ok_or(InputError::Create)?;
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventButtonNumber,
                *number,
            );
            if *clicks > 0 {
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::MouseEventClickState,
                    *clicks,
                );
            }
            event
        }
        Event::Scroll { at, dx, dy, precise, phase, momentum } => {
            let units = if *precise { CGScrollEventUnit::Pixel } else { CGScrollEventUnit::Line };
            let (wheel1, wheel2) = (round(*dy), round(*dx));
            let event = CGEvent::new_scroll_wheel_event2(source, units, 2, wheel1, wheel2, 0)
                .ok_or(InputError::Create)?;
            CGEvent::set_location(Some(&event), *at);
            let set = |field: CGEventField, value: i64| {
                CGEvent::set_integer_value_field(Some(&event), field, value);
            };
            if *precise {
                set(CGEventField::ScrollWheelEventIsContinuous, 1);
                set(CGEventField::ScrollWheelEventPointDeltaAxis1, i64::from(wheel1));
                set(CGEventField::ScrollWheelEventPointDeltaAxis2, i64::from(wheel2));
                CGEvent::set_double_value_field(
                    Some(&event),
                    CGEventField::ScrollWheelEventFixedPtDeltaAxis1,
                    f64::from(*dy),
                );
                CGEvent::set_double_value_field(
                    Some(&event),
                    CGEventField::ScrollWheelEventFixedPtDeltaAxis2,
                    f64::from(*dx),
                );
            }
            set(CGEventField::ScrollWheelEventScrollPhase, scroll_phase_value(*phase));
            set(CGEventField::ScrollWheelEventMomentumPhase, momentum_phase_value(*momentum));
            event
        }
        Event::Key { vk, down, modifier, repeat } => {
            let event =
                CGEvent::new_keyboard_event(source, *vk, *down).ok_or(InputError::Create)?;
            if *modifier {
                CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
            }
            if *repeat {
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::KeyboardEventAutorepeat,
                    1,
                );
            }
            event
        }
        Event::Text { text, down } => {
            let event = CGEvent::new_keyboard_event(source, TEXT_CARRIER, *down)
                .ok_or(InputError::Create)?;
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let len = u64::try_from(utf16.len()).map_err(|_too_long| InputError::Create)?;
            // SAFETY: `utf16` outlives the call and `len` is its exact length, as the function
            // requires; the event copies the string.
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), len, utf16.as_ptr());
            }
            event
        }
        Event::Media { key, down } => {
            let flags = NSEventModifierFlags(
                usize::try_from(post.flags.0).map_err(|_too_wide| InputError::Create)?,
            );
            NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::SystemDefined,
                NSPoint::ZERO,
                flags,
                0.0,
                0,
                None,
                nx::SUBTYPE_AUX_CONTROL_BUTTONS,
                media_data1(*key, *down),
                -1,
            )
            .and_then(|ns| ns.CGEvent())
            .map(CFRetained::from)
            .ok_or(InputError::Create)?
        }
    };
    if !matches!(post.event, Event::Media { .. }) {
        CGEvent::set_flags(Some(&event), post.flags);
    }
    CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, SLOPTY_EVENT);
    Ok(event)
}

/// Caps Lock through the HID system's parameter connection (`IOHIDSetModifierLockState`,
/// `<IOKit/hidsystem/IOHIDLib.h>`): the lock itself, as a real key's press would leave it, and
/// its light.
mod hid {
    use objc2_core_foundation::{CFDictionary, CFRetained};
    use objc2_io_kit::{
        IOHIDGetModifierLockState, IOHIDSetModifierLockState, IOObjectRelease,
        IOServiceGetMatchingService, IOServiceMatching, IOServiceOpen, io_connect_t,
        kIOHIDCapsLockState, kIOHIDParamConnectType, kIOMainPortDefault,
    };

    use crate::InputError;

    /// `kIOHIDCapsLockState` as the selector argument, an `int` in `IOHIDLib.h`.
    const CAPS_LOCK: i32 = kIOHIDCapsLockState.cast_signed();

    /// The process's parameter connection to the HID system, opened on first use and kept for
    /// the process's life, so a Caps Lock change costs its call alone. A failed open is tried
    /// again next time.
    static CONNECTION: parking_lot::Mutex<Option<io_connect_t>> = parking_lot::const_mutex(None);

    /// Open a parameter connection to the HID system.
    fn open() -> Result<io_connect_t, InputError> {
        // SAFETY: a NUL-terminated class name; the answer is a +1 dictionary or none.
        let matching = unsafe { IOServiceMatching(c"IOHIDSystem".as_ptr()) };
        // SAFETY: `kIOMainPortDefault` is IOKit's constant, written once at load.
        let main_port = unsafe { kIOMainPortDefault };
        // SAFETY: a mutable dictionary is a dictionary (CoreFoundation's own subtyping).
        let matching = matching.map(|m| unsafe { CFRetained::cast_unchecked::<CFDictionary>(m) });
        // SAFETY: the matching dictionary is consumed by the call, as IOKit documents; none
        // matches nothing.
        let service = unsafe { IOServiceGetMatchingService(main_port, matching) };
        if service == 0 {
            return Err(InputError::Create);
        }
        let mut connect = 0;
        #[expect(deprecated, reason = "libc points at the mach2 crate for this one call")]
        // SAFETY: reads this task's own port, which the kernel set at process start.
        let task = unsafe { libc::mach_task_self() };
        // SAFETY: a live service, this task's own port, and an out pointer that outlives the
        // call.
        let opened =
            unsafe { IOServiceOpen(service, task, kIOHIDParamConnectType, &raw mut connect) };
        // The service reference `IOServiceGetMatchingService` handed over, released once; the
        // connection keeps what it needs.
        IOObjectRelease(service);
        if opened != 0 {
            return Err(InputError::Create);
        }
        Ok(connect)
    }

    /// The kept connection, opened now if it is not yet. The lock covers the open only; the
    /// calls on the connection, Mach messages, need none.
    fn connection() -> Result<io_connect_t, InputError> {
        let mut kept = CONNECTION.lock();
        let connect = match *kept {
            Some(connect) => connect,
            None => *kept.insert(open()?),
        };
        drop(kept);
        Ok(connect)
    }

    /// Run `f` on the kept connection.
    fn with_connection<T>(
        f: impl FnOnce(io_connect_t) -> Result<T, InputError>,
    ) -> Result<T, InputError> {
        f(connection()?)
    }

    pub(super) fn caps_lock() -> Option<bool> {
        with_connection(|connect| {
            let mut on = false;
            // SAFETY: an open parameter connection and an out pointer that outlives the call.
            let got = unsafe { IOHIDGetModifierLockState(connect, CAPS_LOCK, &raw mut on) };
            if got == 0 { Ok(on) } else { Err(InputError::Create) }
        })
        .ok()
    }

    pub(super) fn set_caps_lock(on: bool) -> Result<(), InputError> {
        with_connection(|connect| {
            let set = IOHIDSetModifierLockState(connect, CAPS_LOCK, on);
            if set == 0 { Ok(()) } else { Err(InputError::Create) }
        })
    }
}

/// A fake backend for tests: fixed geometry, an activation counter, every post kept.
///
/// Nothing here touches CoreGraphics or AppKit, so the injector's whole decision path —
/// point mapping, drag tracking, routing, activation, text — runs under `cargo nextest`
/// with no permissions and no side effects.
#[derive(Debug, Default, Clone)]
pub struct Recorder {
    /// What [`Backend::bounds`] answers.
    pub bounds: Option<Rect>,
    /// What [`Backend::owner_pid`] answers for window targets.
    pub owner: Option<i32>,
    /// Whether the owner currently counts as the active app.
    pub active: bool,
    /// Calls to [`Backend::bounds`]: window-server reads, on the real backend.
    pub bounds_reads: usize,
    /// Calls to [`Backend::is_active`]: `NSRunningApplication` lookups, on the real backend.
    pub active_checks: usize,
    /// Pids passed to [`Backend::activate`], in order.
    pub activations: Vec<i32>,
    /// Everything posted, in order.
    pub posts: Vec<Post>,
    /// Caps Lock as it stands; `None`, the HID system would not say.
    pub caps: Option<bool>,
    /// Every Caps Lock state set, in order.
    pub locks: Vec<bool>,
    /// The Caps Lock claim this recorder's injectors share: its own, and its clones'.
    pub caps_claims: SharedCaps,
}

impl Recorder {
    /// A window whose owner is `pid`, with these bounds, currently not active.
    #[must_use]
    pub fn window(pid: i32, bounds: Rect) -> Self {
        Self { bounds: Some(bounds), owner: Some(pid), ..Self::default() }
    }

    /// A display with these bounds.
    #[must_use]
    pub fn display(bounds: Rect) -> Self {
        Self { bounds: Some(bounds), ..Self::default() }
    }

    /// The events posted so far, without route and flags.
    #[must_use]
    pub fn events(&self) -> Vec<&Event> {
        self.posts.iter().map(|p| &p.event).collect()
    }
}

impl Backend for Recorder {
    fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
        match target {
            CaptureTarget::Window(_) => self.owner,
            CaptureTarget::Display(_) => None,
        }
    }

    fn bounds(&mut self, _target: CaptureTarget) -> Option<Rect> {
        self.bounds_reads = self.bounds_reads.saturating_add(1);
        self.bounds
    }

    fn is_active(&mut self, pid: i32) -> bool {
        self.active_checks = self.active_checks.saturating_add(1);
        self.active && self.owner == Some(pid)
    }

    fn activate(&mut self, pid: i32) -> Result<(), InputError> {
        self.activations.push(pid);
        if self.owner == Some(pid) {
            self.active = true;
            Ok(())
        } else {
            Err(InputError::NoApplication)
        }
    }

    fn post(&mut self, post: Post) -> Result<(), InputError> {
        self.posts.push(post);
        Ok(())
    }

    fn caps_lock(&mut self) -> Option<bool> {
        self.caps
    }

    fn set_caps_lock(&mut self, on: bool) -> Result<(), InputError> {
        self.locks.push(on);
        self.caps = Some(on);
        Ok(())
    }

    fn caps_claims(&self) -> SharedCaps {
        std::sync::Arc::clone(&self.caps_claims)
    }
}

/// `ScrollPhase` → `CGScrollPhase`.
#[must_use]
pub const fn scroll_phase_value(phase: ScrollPhase) -> i64 {
    match phase {
        ScrollPhase::None => 0,
        ScrollPhase::Began => scroll_phase::BEGAN,
        ScrollPhase::Changed => scroll_phase::CHANGED,
        ScrollPhase::Ended => scroll_phase::ENDED,
        ScrollPhase::Cancelled => scroll_phase::CANCELLED,
        ScrollPhase::MayBegin => scroll_phase::MAY_BEGIN,
    }
}

/// `ScrollPhase` → `CGMomentumScrollPhase`.
#[must_use]
pub const fn momentum_phase_value(phase: ScrollPhase) -> i64 {
    match phase {
        ScrollPhase::None | ScrollPhase::MayBegin => momentum_phase::NONE,
        ScrollPhase::Began => momentum_phase::BEGIN,
        ScrollPhase::Changed => momentum_phase::CONTINUE,
        ScrollPhase::Ended | ScrollPhase::Cancelled => momentum_phase::END,
    }
}

const fn round(v: f32) -> i32 {
    #[expect(clippy::cast_possible_truncation, reason = "clamped")]
    let r = v.round().clamp(-1.0e6, 1.0e6) as i32;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string an event carries.
    fn unicode(event: &CGEvent) -> String {
        let mut units = [0_u16; 32];
        let mut len = 0;
        // SAFETY: `units` holds the 32 units the call may write, `len` outlives the call.
        unsafe {
            CGEvent::keyboard_get_unicode_string(Some(event), 32, &raw mut len, units.as_mut_ptr());
        }
        String::from_utf16_lossy(&units[..usize::try_from(len).unwrap()])
    }

    fn post(event: Event, flags: CGEventFlags) -> Post {
        Post { route: Route::Hid, flags, event }
    }

    fn private() -> CFRetained<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::Private).unwrap()
    }

    /// A key is its position, its press or release, its repeat and its flags, and carries no
    /// string, so the target's layout, dead keys and input method make the character; every
    /// event is tagged as the worker's own. Built, never posted.
    #[test]
    fn keys_carry_no_unicode_string() {
        let source = private();
        let flags = CGEventFlags::MaskCommand | CGEventFlags::MaskSecondaryFn;
        let key = Event::Key { vk: 0x0c, down: true, modifier: false, repeat: true };
        let event = build(&post(key, flags), Some(&source)).unwrap();
        let field = |f| CGEvent::integer_value_field(Some(&event), f);
        assert_eq!(field(CGEventField::KeyboardEventKeycode), 0x0c, "kVK_ANSI_Q");
        assert_eq!(field(CGEventField::KeyboardEventAutorepeat), 1);
        assert_eq!(field(CGEventField::EventSourceUserData), SLOPTY_EVENT);
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::KeyDown);
        assert_eq!(CGEvent::flags(Some(&event)), flags);
        assert_eq!(unicode(&event), "", "no string: the layout decides");

        let shift = Event::Key { vk: 0x3c, down: false, modifier: true, repeat: false };
        let event = build(&post(shift, CGEventFlags::empty()), Some(&source)).unwrap();
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::FlagsChanged);
        let code = CGEvent::integer_value_field(Some(&event), CGEventField::KeyboardEventKeycode);
        assert_eq!(code, 0x3c, "the right shift key");
    }

    /// Committed text rides on the Space key's press and release, carrying the whole piece.
    #[test]
    fn text_rides_on_space_with_its_string() {
        for (text, down) in [("tiếng Việt", true), ("日本語", false), ("🇻🇳", true)] {
            let piece = Event::Text { text: text.to_owned(), down };
            let event = build(&post(piece, CGEventFlags::empty()), None).unwrap();
            let code =
                CGEvent::integer_value_field(Some(&event), CGEventField::KeyboardEventKeycode);
            assert_eq!(code, 0x31, "kVK_Space, never keycode 0");
            let kind = if down { CGEventType::KeyDown } else { CGEventType::KeyUp };
            assert_eq!(CGEvent::r#type(Some(&event)), kind);
            assert_eq!(unicode(&event), text);
        }
    }

    /// A media key is a system-defined event of subtype 8 whose `data1` names the key and its
    /// state, as AppKit reads it back.
    #[test]
    fn media_keys_post_system_defined_subtype_8() {
        for (key, nx) in [(MediaKey::PlayPause, 16), (MediaKey::Next, 17), (MediaKey::Previous, 18)]
        {
            for (down, state) in [(true, 0x0a), (false, 0x0b)] {
                let event =
                    build(&post(Event::Media { key, down }, CGEventFlags::empty()), None).unwrap();
                let ns = NSEvent::eventWithCGEvent(&event).unwrap();
                assert_eq!(ns.r#type(), NSEventType::SystemDefined);
                assert_eq!(ns.subtype().0, 8);
                assert_eq!(ns.data1(), (nx << 16) | (state << 8), "{key:?} {down}");
                let tag =
                    CGEvent::integer_value_field(Some(&event), CGEventField::EventSourceUserData);
                assert_eq!(tag, SLOPTY_EVENT);
            }
        }
    }

    #[test]
    fn phases_use_the_iokit_values() {
        assert_eq!(scroll_phase_value(ScrollPhase::Began), 1);
        assert_eq!(scroll_phase_value(ScrollPhase::Ended), 4);
        assert_eq!(scroll_phase_value(ScrollPhase::MayBegin), 128);
        assert_eq!(momentum_phase_value(ScrollPhase::Changed), 2);
        assert_eq!(momentum_phase_value(ScrollPhase::Ended), 3);
    }

    #[test]
    fn recorder_activates_only_its_owner() {
        let mut r = Recorder::window(42, Rect::default());
        assert!(!r.is_active(42));
        assert!(r.activate(7).is_err());
        r.activate(42).unwrap();
        assert!(r.is_active(42));
        assert_eq!(r.activations, [7, 42]);
    }
}
