//! Where the injector's events go.
//!
//! [`Injector`](crate::Injector) decides *what* to post — the event kind, the global point,
//! the modifier flags, the route — and hands a [`Post`] to a [`Backend`]. [`System`] turns it
//! into a `CGEvent` and posts it; [`Recorder`] keeps it in a `Vec` so every decision the
//! injector makes can be asserted in a unit test without Accessibility access and without a
//! single real event reaching the desktop.

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
    CGScrollEventUnit,
};
use slopty_capture::Rect;
use slopty_proto::screen::{CaptureTarget, ScrollPhase};

use crate::InputError;

/// `CGScrollPhase` values (`IOKit/hidsystem/IOLLEvent.h`).
mod scroll_phase {
    pub const BEGAN: i64 = 1;
    pub const CHANGED: i64 = 2;
    pub const ENDED: i64 = 4;
    pub const CANCELLED: i64 = 8;
    pub const MAY_BEGIN: i64 = 128;
}

/// `CGMomentumScrollPhase` values.
mod momentum_phase {
    pub const NONE: i64 = 0;
    pub const BEGIN: i64 = 1;
    pub const CONTINUE: i64 = 2;
    pub const END: i64 = 3;
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
    /// Key press, repeat or release.
    Key {
        /// Virtual keycode.
        vk: CGKeyCode,
        /// Press or repeat (`true`) vs release.
        down: bool,
        /// A bare modifier: posts as `FlagsChanged`.
        modifier: bool,
        /// Auto-repeat.
        repeat: bool,
        /// The client's text for the key, attached with `CGEventKeyboardSetUnicodeString`.
        text: Option<String>,
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

/// The host-side services an injector needs: window geometry, app activation, posting.
pub trait Backend {
    /// The pid owning a window target; `None` for displays or unknown windows.
    fn owner_pid(&self, target: CaptureTarget) -> Option<i32>;
    /// Current bounds of the target in global display points.
    fn bounds(&self, target: CaptureTarget) -> Option<Rect>;
    /// Whether the app with this pid is the active application.
    fn is_active(&self, pid: i32) -> bool;
    /// Bring the app with this pid to the front.
    fn activate(&mut self, pid: i32) -> Result<(), InputError>;
    /// Post one event.
    fn post(&mut self, post: Post) -> Result<(), InputError>;
}

/// The real thing: CoreGraphics events, AppKit activation, the WindowServer's window list.
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

impl Backend for System {
    fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
        match target {
            CaptureTarget::Window(id) => slopty_capture::window_owner_pid(id),
            CaptureTarget::Display(_) => None,
        }
    }

    fn bounds(&self, target: CaptureTarget) -> Option<Rect> {
        slopty_capture::target_bounds(target)
    }

    fn is_active(&self, pid: i32) -> bool {
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
        let event = build(&post)?;
        match post.route {
            Route::Pid(pid) => CGEvent::post_to_pid(pid, Some(&event)),
            Route::Hid => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event)),
        }
        Ok(())
    }
}

/// Build the `CGEvent` for a [`Post`].
fn build(post: &Post) -> Result<CFRetained<CGEvent>, InputError> {
    let event = match &post.event {
        Event::Mouse { kind, at, button, number, clicks } => {
            let event =
                CGEvent::new_mouse_event(None, *kind, *at, *button).ok_or(InputError::Create)?;
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
            let event = CGEvent::new_scroll_wheel_event2(None, units, 2, wheel1, wheel2, 0)
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
        Event::Key { vk, down, modifier, repeat, text } => {
            let event = CGEvent::new_keyboard_event(None, *vk, *down).ok_or(InputError::Create)?;
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
            if let Some(text) = text {
                let utf16: Vec<u16> = text.encode_utf16().collect();
                let len = u64::try_from(utf16.len()).unwrap_or(u64::MAX);
                // SAFETY: `utf16` outlives the call and `len` is its exact length, as the
                // function requires; the event copies the string.
                unsafe {
                    CGEvent::keyboard_set_unicode_string(Some(&event), len, utf16.as_ptr());
                }
            }
            event
        }
    };
    CGEvent::set_flags(Some(&event), post.flags);
    Ok(event)
}

/// A fake backend for tests: fixed geometry, an activation counter, every post kept.
///
/// Nothing here touches CoreGraphics or AppKit, so the injector's whole decision path —
/// point mapping, drag tracking, routing, activation, text — runs under `cargo nextest`
/// with no permissions and no side effects.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Recorder {
    /// What [`Backend::bounds`] answers.
    pub bounds: Option<Rect>,
    /// What [`Backend::owner_pid`] answers for window targets.
    pub owner: Option<i32>,
    /// Whether the owner currently counts as the active app.
    pub active: bool,
    /// Pids passed to [`Backend::activate`], in order.
    pub activations: Vec<i32>,
    /// Everything posted, in order.
    pub posts: Vec<Post>,
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

    fn bounds(&self, _target: CaptureTarget) -> Option<Rect> {
        self.bounds
    }

    fn is_active(&self, pid: i32) -> bool {
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
        assert!(r.activate(42).is_ok());
        assert!(r.is_active(42));
        assert_eq!(r.activations, [7, 42]);
    }
}
