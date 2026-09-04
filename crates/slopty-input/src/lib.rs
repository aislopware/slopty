//! Remote-window input on the host: turn a client's [`ScreenInput`] into `CGEvent`s.
//!
//! One [`Injector`] per screen stream. It knows the stream's target and its pixels-per-point
//! scale, maps stream pixels back to global display points, and posts events either straight to
//! the owning process (`CGEventPostToPid`, window streams: the window need not be frontmost,
//! nothing on the host's own desktop moves) or to the HID event tap (display streams: the
//! whole screen is the target, so the real pointer follows). Posting needs the host process to
//! be granted *Accessibility* (post-event access); [`can_post`] and [`request_post`] wrap the
//! preflight and prompt.
//!
//! macOS only delivers keyboard events to the *active* application: events posted to an
//! inactive pid queue up until it is activated (observed macOS 26.5). So a window stream
//! activates its owner before the first click or key press after it lost activation; the
//! host's own desktop sees that app come to the front, which is the price of typing into it.
//!
//! Magnify gestures have no public `CGEvent` constructor and are ignored.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton,
    CGPreflightPostEventAccess, CGRequestPostEventAccess, CGScrollEventUnit,
};
use slopty_capture::Rect;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput, ScrollPhase};

pub mod keymap;

/// How long cached window bounds stay valid; windows move rarely, pointer events are dense.
const BOUNDS_TTL: Duration = Duration::from_millis(100);

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

/// What went wrong posting an event.
#[derive(thiserror::Error, Debug)]
pub enum InputError {
    /// The target window is gone (no bounds in the window list).
    #[error("target has no bounds; window closed?")]
    NoBounds,
    /// `CGEventCreate*` returned null.
    #[error("CGEvent creation failed")]
    Create,
    /// The owning application could not be found for `Focus`.
    #[error("owning application not running")]
    NoApplication,
}

/// Whether this process may post events (Accessibility / "post event access").
#[must_use]
pub fn can_post() -> bool {
    CGPreflightPostEventAccess()
}

/// Ask macOS for post-event access; shows the system prompt once and returns the current state.
pub fn request_post() -> bool {
    CGRequestPostEventAccess()
}

/// One scroll step as the client saw it.
#[derive(Clone, Copy, Debug)]
struct Scroll {
    dx: f32,
    dy: f32,
    precise: bool,
    phase: ScrollPhase,
    momentum: ScrollPhase,
}

/// Where events go.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    /// `CGEventPostToPid`: the owning app, whether or not it is frontmost.
    Pid(i32),
    /// The HID tap: the system, as if from real hardware.
    Hid,
}

/// Injects one stream's input.
#[derive(Debug)]
pub struct Injector {
    target: CaptureTarget,
    /// Stream pixels per display point.
    scale: f64,
    route: Route,
    bounds: Option<Rect>,
    bounds_at: Option<Instant>,
    /// Buttons currently held, so moves post as drags.
    held: u8,
    /// Modifier flags from the latest event, kept so bare modifier presses post correctly.
    flags: CGEventFlags,
}

impl Injector {
    /// An injector for `target` whose stream has `scale` pixels per display point.
    #[must_use]
    pub fn new(target: CaptureTarget, scale: f64) -> Self {
        let route = match target {
            CaptureTarget::Window(id) => {
                slopty_capture::window_owner_pid(id).map_or(Route::Hid, Route::Pid)
            }
            CaptureTarget::Display(_) => Route::Hid,
        };
        Self {
            target,
            scale: if scale > 0.0 { scale } else { 1.0 },
            route,
            bounds: None,
            bounds_at: None,
            held: 0,
            flags: CGEventFlags::empty(),
        }
    }

    /// Apply one input event.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        tracing::trace!(target = ?self.target, ?input, "inject");
        match *input {
            ScreenInput::Move { x, y } => {
                let at = self.point(x, y)?;
                self.post_mouse(self.move_type(), at, CGMouseButton::Left, 0, 0)
            }
            ScreenInput::Button { button, down, x, y, clicks, mods } => {
                let at = self.point(x, y)?;
                if down {
                    self.ensure_active();
                }
                self.flags = flags_for(mods);
                let (kind, cg_button, number) = button_event(button, down);
                self.set_held(button, down);
                self.post_mouse(kind, at, cg_button, number, i64::from(clicks.max(1)))
            }
            ScreenInput::Scroll { dx, dy, precise, phase, momentum, x, y, mods } => {
                let at = self.point(x, y)?;
                self.flags = flags_for(mods);
                self.post_scroll(at, Scroll { dx, dy, precise, phase, momentum })
            }
            ScreenInput::Key { code, action, mods, .. } => {
                if !matches!(action, KeyAction::Release) {
                    self.ensure_active();
                }
                self.flags = flags_for(mods);
                let text = match input {
                    ScreenInput::Key { text, .. } => text.as_deref(),
                    _ => None,
                };
                self.post_key(code, action, text)
            }
            ScreenInput::Magnify { .. } => Ok(()),
        }
    }

    /// Bring the target's application to the front so it takes keyboard events.
    pub fn focus(&self) -> Result<(), InputError> {
        let Route::Pid(pid) = self.route else { return Ok(()) };
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .ok_or(InputError::NoApplication)?;
        let _activated =
            app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        Ok(())
    }

    /// Activate the owner if it is not the active app (see the module docs).
    fn ensure_active(&self) {
        let Route::Pid(pid) = self.route else { return };
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
            return;
        };
        if !app.isActive() {
            let _activated =
                app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        }
    }

    /// The stream was re-scaled: `scale` stream pixels per display point from now on.
    pub const fn set_scale(&mut self, scale: f64) {
        if scale > 0.0 {
            self.scale = scale;
        }
    }

    /// The stream's target.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// Stream pixels → global display points, refreshing the cached bounds when stale.
    fn point(&mut self, x: f32, y: f32) -> Result<CGPoint, InputError> {
        let stale = self.bounds_at.is_none_or(|at| at.elapsed() > BOUNDS_TTL);
        if stale {
            self.bounds = slopty_capture::target_bounds(self.target);
            self.bounds_at = Some(Instant::now());
        }
        let rect = self.bounds.ok_or(InputError::NoBounds)?;
        Ok(to_point(rect, self.scale, f64::from(x), f64::from(y)))
    }

    const fn move_type(&self) -> CGEventType {
        if self.held & button_bit(MouseButton::Left) != 0 {
            CGEventType::LeftMouseDragged
        } else if self.held & button_bit(MouseButton::Right) != 0 {
            CGEventType::RightMouseDragged
        } else if self.held != 0 {
            CGEventType::OtherMouseDragged
        } else {
            CGEventType::MouseMoved
        }
    }

    const fn set_held(&mut self, button: MouseButton, down: bool) {
        if down {
            self.held |= button_bit(button);
        } else {
            self.held &= !button_bit(button);
        }
    }

    fn post_mouse(
        &self,
        kind: CGEventType,
        at: CGPoint,
        button: CGMouseButton,
        number: i64,
        clicks: i64,
    ) -> Result<(), InputError> {
        let event = CGEvent::new_mouse_event(None, kind, at, button).ok_or(InputError::Create)?;
        CGEvent::set_flags(Some(&event), self.flags);
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventButtonNumber,
            number,
        );
        if clicks > 0 {
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventClickState,
                clicks,
            );
        }
        self.post(&event);
        Ok(())
    }

    fn post_scroll(&self, at: CGPoint, scroll: Scroll) -> Result<(), InputError> {
        let Scroll { dx, dy, precise, phase, momentum } = scroll;
        let units = if precise { CGScrollEventUnit::Pixel } else { CGScrollEventUnit::Line };
        let (wheel1, wheel2) = (round(dy), round(dx));
        let event = CGEvent::new_scroll_wheel_event2(None, units, 2, wheel1, wheel2, 0)
            .ok_or(InputError::Create)?;
        CGEvent::set_location(Some(&event), at);
        CGEvent::set_flags(Some(&event), self.flags);
        let set = |field: CGEventField, value: i64| {
            CGEvent::set_integer_value_field(Some(&event), field, value);
        };
        if precise {
            set(CGEventField::ScrollWheelEventIsContinuous, 1);
            set(CGEventField::ScrollWheelEventPointDeltaAxis1, i64::from(wheel1));
            set(CGEventField::ScrollWheelEventPointDeltaAxis2, i64::from(wheel2));
            CGEvent::set_double_value_field(
                Some(&event),
                CGEventField::ScrollWheelEventFixedPtDeltaAxis1,
                f64::from(dy),
            );
            CGEvent::set_double_value_field(
                Some(&event),
                CGEventField::ScrollWheelEventFixedPtDeltaAxis2,
                f64::from(dx),
            );
        }
        set(CGEventField::ScrollWheelEventScrollPhase, scroll_phase_value(phase));
        set(CGEventField::ScrollWheelEventMomentumPhase, momentum_phase_value(momentum));
        self.post(&event);
        Ok(())
    }

    fn post_key(
        &self,
        code: KeyCode,
        action: KeyAction,
        text: Option<&str>,
    ) -> Result<(), InputError> {
        let Some(vk) = keymap::virtual_key(code) else {
            tracing::debug!(?code, "no virtual key; dropped");
            return Ok(());
        };
        let down = !matches!(action, KeyAction::Release);
        let event = CGEvent::new_keyboard_event(None, vk, down).ok_or(InputError::Create)?;
        if keymap::is_modifier(code) {
            CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
        }
        CGEvent::set_flags(Some(&event), self.flags);
        if matches!(action, KeyAction::Repeat) {
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::KeyboardEventAutorepeat,
                1,
            );
        }
        if let Some(text) = text.filter(|t| down && !t.is_empty() && !keymap::is_modifier(code)) {
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let len = u64::try_from(utf16.len()).unwrap_or(u64::MAX);
            // SAFETY: `utf16` outlives the call and `len` is its exact length, as the
            // function requires; the event copies the string.
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), len, utf16.as_ptr());
            }
        }
        self.post(&event);
        Ok(())
    }

    fn post(&self, event: &CGEvent) {
        match self.route {
            Route::Pid(pid) => CGEvent::post_to_pid(pid, Some(event)),
            Route::Hid => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event)),
        }
    }
}

/// Stream pixel → global point for a target with these bounds and pixels-per-point scale.
#[must_use]
pub fn to_point(bounds: Rect, scale: f64, x: f64, y: f64) -> CGPoint {
    let px = bounds.x + x / scale;
    let py = bounds.y + y / scale;
    CGPoint::new(px.clamp(bounds.x, bounds.x + bounds.w), py.clamp(bounds.y, bounds.y + bounds.h))
}

const fn button_bit(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 4,
        MouseButton::Back => 8,
        MouseButton::Forward => 16,
    }
}

/// Event type, CG button class and button number for a press or release.
const fn button_event(button: MouseButton, down: bool) -> (CGEventType, CGMouseButton, i64) {
    match (button, down) {
        (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left, 0),
        (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left, 0),
        (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right, 1),
        (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right, 1),
        (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 2),
        (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 2),
        (MouseButton::Back, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 3),
        (MouseButton::Back, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 3),
        (MouseButton::Forward, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center, 4),
        (MouseButton::Forward, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center, 4),
    }
}

/// Wire modifiers → `CGEventFlags`.
#[must_use]
pub fn flags_for(mods: Mods) -> CGEventFlags {
    let mut flags = CGEventFlags::empty();
    if mods.contains(Mods::SHIFT) {
        flags |= CGEventFlags::MaskShift;
    }
    if mods.contains(Mods::CTRL) {
        flags |= CGEventFlags::MaskControl;
    }
    if mods.contains(Mods::ALT) {
        flags |= CGEventFlags::MaskAlternate;
    }
    if mods.contains(Mods::SUPER) {
        flags |= CGEventFlags::MaskCommand;
    }
    if mods.contains(Mods::CAPS_LOCK) {
        flags |= CGEventFlags::MaskAlphaShift;
    }
    flags
}

const fn scroll_phase_value(phase: ScrollPhase) -> i64 {
    match phase {
        ScrollPhase::None => 0,
        ScrollPhase::Began => scroll_phase::BEGAN,
        ScrollPhase::Changed => scroll_phase::CHANGED,
        ScrollPhase::Ended => scroll_phase::ENDED,
        ScrollPhase::Cancelled => scroll_phase::CANCELLED,
        ScrollPhase::MayBegin => scroll_phase::MAY_BEGIN,
    }
}

const fn momentum_phase_value(phase: ScrollPhase) -> i64 {
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
    fn points_scale_back_from_stream_pixels() {
        let bounds = Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
        // A 2× display streamed at half size: 1 stream pixel per point.
        let p = to_point(bounds, 1.0, 10.0, 20.0);
        assert!((p.x - 110.0).abs() < 1e-9 && (p.y - 70.0).abs() < 1e-9, "{p:?}");
        // Full 2× stream: 2 stream pixels per point.
        let p = to_point(bounds, 2.0, 10.0, 20.0);
        assert!((p.x - 105.0).abs() < 1e-9 && (p.y - 60.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn points_clamp_to_the_target() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let p = to_point(bounds, 1.0, -5.0, 500.0);
        assert!((p.x - 0.0).abs() < 1e-9 && (p.y - 100.0).abs() < 1e-9, "{p:?}");
    }

    #[test]
    fn flags_map_each_modifier() {
        let all = Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER | Mods::CAPS_LOCK;
        let flags = flags_for(all);
        assert!(flags.contains(CGEventFlags::MaskShift));
        assert!(flags.contains(CGEventFlags::MaskControl));
        assert!(flags.contains(CGEventFlags::MaskAlternate));
        assert!(flags.contains(CGEventFlags::MaskCommand));
        assert!(flags.contains(CGEventFlags::MaskAlphaShift));
        assert_eq!(flags_for(Mods::empty()), CGEventFlags::empty());
    }

    #[test]
    fn drags_follow_held_buttons() {
        let mut inj = Injector::new(CaptureTarget::Display(1), 1.0);
        assert_eq!(inj.move_type(), CGEventType::MouseMoved);
        inj.set_held(MouseButton::Left, true);
        assert_eq!(inj.move_type(), CGEventType::LeftMouseDragged);
        inj.set_held(MouseButton::Right, true);
        inj.set_held(MouseButton::Left, false);
        assert_eq!(inj.move_type(), CGEventType::RightMouseDragged);
        inj.set_held(MouseButton::Right, false);
        inj.set_held(MouseButton::Back, true);
        assert_eq!(inj.move_type(), CGEventType::OtherMouseDragged);
    }

    #[test]
    fn phases_use_the_iokit_values() {
        assert_eq!(scroll_phase_value(ScrollPhase::Began), 1);
        assert_eq!(scroll_phase_value(ScrollPhase::Ended), 4);
        assert_eq!(scroll_phase_value(ScrollPhase::MayBegin), 128);
        assert_eq!(momentum_phase_value(ScrollPhase::Changed), 2);
        assert_eq!(momentum_phase_value(ScrollPhase::Ended), 3);
    }
}
