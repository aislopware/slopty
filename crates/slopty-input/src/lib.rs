//! Remote-window input on the host: turn a client's [`ScreenInput`] into `CGEvent`s.
//!
//! One [`Injector`] per screen stream. It knows the stream's target and its pixels-per-point
//! scale, maps stream pixels back to global display points, and decides what to post and
//! where: straight to the owning process (`CGEventPostToPid`, window streams: the window need
//! not be frontmost, nothing on the host's own desktop moves) or to the HID event tap
//! (display streams: the whole screen is the target, so the real pointer follows). Posting
//! itself is a [`Backend`]: [`System`] for the daemon, [`Recorder`] for tests, so every
//! decision here is unit-tested without Accessibility access and without a real event.
//! Posting needs the host process to be granted *Accessibility* (post-event access);
//! [`can_post`] and [`request_post`] wrap the preflight and prompt.
//!
//! macOS only delivers keyboard events to the *active* application: events posted to an
//! inactive pid queue up until it is activated (observed macOS 26.5). So a window stream
//! activates its owner before the first click or key press after it lost activation; the
//! host's own desktop sees that app come to the front, which is the price of typing into it.
//!
//! Magnify gestures have no public `CGEvent` constructor and are ignored.

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEventFlags, CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use slopty_capture::Rect;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput};

pub mod backend;
pub mod keymap;
pub mod pasteboard;

pub use backend::{Backend, Event, Post, Recorder, Route, System};
pub use pasteboard::{Board, MacBoard, Rep};

/// How long cached window bounds stay valid; windows move rarely, pointer events are dense.
const BOUNDS_TTL: Duration = Duration::from_millis(100);

/// What went wrong posting an event.
#[derive(Clone, Copy, thiserror::Error, Debug)]
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

/// Injects one stream's input through a [`Backend`].
#[derive(Debug)]
pub struct Injector<B = System> {
    backend: B,
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

impl Injector<System> {
    /// An injector for `target` whose stream has `scale` pixels per display point, posting
    /// real events.
    #[must_use]
    pub fn new(target: CaptureTarget, scale: f64) -> Self {
        Self::with_backend(target, scale, System)
    }
}

impl<B: Backend> Injector<B> {
    /// An injector for `target` whose stream has `scale` pixels per display point, posting
    /// through `backend`.
    #[must_use]
    pub fn with_backend(target: CaptureTarget, scale: f64, backend: B) -> Self {
        let route = backend.owner_pid(target).map_or(Route::Hid, Route::Pid);
        Self {
            backend,
            target,
            scale: if scale > 0.0 { scale } else { 1.0 },
            route,
            bounds: None,
            bounds_at: None,
            held: 0,
            flags: CGEventFlags::empty(),
        }
    }

    /// The backend.
    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Where events go.
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }

    /// Apply one input event.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        tracing::trace!(target = ?self.target, ?input, "inject");
        match input {
            ScreenInput::Move { x, y } => {
                let at = self.point(*x, *y)?;
                let kind = self.move_type();
                self.post_mouse(kind, at, CGMouseButton::Left, 0, 0)
            }
            ScreenInput::Button { button, down, clicks, x, y, mods } => {
                let at = self.point(*x, *y)?;
                if *down {
                    self.ensure_active();
                }
                self.flags = flags_for(*mods);
                let (kind, cg_button, number) = button_event(*button, *down);
                self.set_held(*button, *down);
                self.post_mouse(kind, at, cg_button, number, i64::from((*clicks).max(1)))
            }
            ScreenInput::Scroll { dx, dy, precise, phase, momentum, x, y, mods } => {
                let at = self.point(*x, *y)?;
                self.flags = flags_for(*mods);
                let (dx, dy, precise, phase, momentum) = (*dx, *dy, *precise, *phase, *momentum);
                self.post(None, Event::Scroll { at, dx, dy, precise, phase, momentum })
            }
            ScreenInput::Key { code, action, mods, text } => {
                if !matches!(action, KeyAction::Release) {
                    self.ensure_active();
                }
                self.flags = flags_for(*mods);
                self.post_key(*code, *action, text.as_deref())
            }
            ScreenInput::Magnify { .. } => Ok(()),
        }
    }

    /// Bring the target's application to the front so it takes keyboard events.
    pub fn focus(&mut self) -> Result<(), InputError> {
        let Route::Pid(pid) = self.route else { return Ok(()) };
        self.backend.activate(pid)
    }

    /// Activate the owner if it is not the active app (see the module docs).
    fn ensure_active(&mut self) {
        let Route::Pid(pid) = self.route else { return };
        if !self.backend.is_active(pid) {
            // A vanished owner is reported by the next post, not here.
            let _gone = self.backend.activate(pid);
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
            self.bounds = self.backend.bounds(self.target);
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
        &mut self,
        kind: CGEventType,
        at: CGPoint,
        button: CGMouseButton,
        number: i64,
        clicks: i64,
    ) -> Result<(), InputError> {
        // A context menu is tracked by AppKit against the window server's own event stream:
        // a right click posted to the pid reaches `rightMouseDown` but the menu never opens
        // (observed with Ghostty, macOS 26.5). So right-button events go through the HID tap
        // even for window streams; the host's pointer does move for those, which is the
        // price of a menu.
        let route = (button == CGMouseButton::Right).then_some(Route::Hid);
        self.post(route, Event::Mouse { kind, at, button, number, clicks })
    }

    fn post_key(
        &mut self,
        code: KeyCode,
        action: KeyAction,
        text: Option<&str>,
    ) -> Result<(), InputError> {
        let Some(vk) = keymap::virtual_key(code) else {
            tracing::debug!(?code, "no virtual key; dropped");
            return Ok(());
        };
        let down = !matches!(action, KeyAction::Release);
        let modifier = keymap::is_modifier(code);
        let text = text.filter(|t| down && !t.is_empty() && !modifier).map(str::to_owned);
        self.post(
            None,
            Event::Key { vk, down, modifier, repeat: matches!(action, KeyAction::Repeat), text },
        )
    }

    /// Post through the backend; `route` overrides the stream's own route.
    fn post(&mut self, route: Option<Route>, event: Event) -> Result<(), InputError> {
        let route = route.unwrap_or(self.route);
        self.backend.post(Post { route, flags: self.flags, event })
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

#[cfg(test)]
mod tests {
    use slopty_core::WindowId;
    use slopty_proto::screen::ScrollPhase;

    use super::*;

    const BOUNDS: Rect = Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
    const PID: i32 = 4242;

    fn window() -> Injector<Recorder> {
        Injector::with_backend(
            CaptureTarget::Window(WindowId(9)),
            2.0,
            Recorder::window(PID, BOUNDS),
        )
    }

    fn display() -> Injector<Recorder> {
        Injector::with_backend(CaptureTarget::Display(1), 1.0, Recorder::display(BOUNDS))
    }

    fn pt(x: f64, y: f64) -> CGPoint {
        CGPoint::new(x, y)
    }

    #[test]
    fn points_scale_back_from_stream_pixels() {
        // A 2× display streamed at half size: 1 stream pixel per point.
        let p = to_point(BOUNDS, 1.0, 10.0, 20.0);
        assert_eq!(p, pt(110.0, 70.0));
        // Full 2× stream: 2 stream pixels per point.
        let p = to_point(BOUNDS, 2.0, 10.0, 20.0);
        assert_eq!(p, pt(105.0, 60.0));
    }

    #[test]
    fn points_clamp_to_the_target() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        assert_eq!(to_point(bounds, 1.0, -5.0, 500.0), pt(0.0, 100.0));
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
    fn window_streams_route_to_the_owner_and_display_streams_to_hid() {
        assert_eq!(window().route(), Route::Pid(PID));
        assert_eq!(display().route(), Route::Hid);
        // A window whose owner is unknown falls back to the HID tap.
        let orphan = Injector::with_backend(
            CaptureTarget::Window(WindowId(9)),
            1.0,
            Recorder::display(BOUNDS),
        );
        assert_eq!(orphan.route(), Route::Hid);
    }

    #[test]
    fn moves_map_through_bounds_and_scale() {
        let mut inj = window();
        inj.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        let posts = &inj.backend().posts;
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].route, Route::Pid(PID));
        assert_eq!(
            posts[0].event,
            Event::Mouse {
                kind: CGEventType::MouseMoved,
                at: pt(110.0, 70.0),
                button: CGMouseButton::Left,
                number: 0,
                clicks: 0,
            }
        );
        // Nothing was activated for a bare move.
        assert!(inj.backend().activations.is_empty());
    }

    #[test]
    fn a_vanished_window_is_reported() {
        let mut inj = Injector::with_backend(
            CaptureTarget::Window(WindowId(9)),
            1.0,
            Recorder { owner: Some(PID), ..Recorder::default() },
        );
        assert!(matches!(
            inj.inject(&ScreenInput::Move { x: 0.0, y: 0.0 }),
            Err(InputError::NoBounds)
        ));
        assert!(inj.backend().posts.is_empty());
    }

    #[test]
    fn drags_follow_held_buttons() {
        let mut inj = display();
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
    fn a_click_activates_the_owner_once_and_drags_until_release() {
        let mut inj = window();
        let button = |down: bool, clicks: u8| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            clicks,
            x: 0.0,
            y: 0.0,
            mods: Mods::SHIFT,
        };
        inj.inject(&button(true, 1)).unwrap();
        inj.inject(&ScreenInput::Move { x: 10.0, y: 10.0 }).unwrap();
        inj.inject(&button(false, 1)).unwrap();
        inj.inject(&ScreenInput::Move { x: 20.0, y: 20.0 }).unwrap();
        inj.inject(&button(true, 2)).unwrap();

        let rec = inj.backend();
        // Activated before the first press only; the owner is active afterwards.
        assert_eq!(rec.activations, [PID]);
        let kinds: Vec<CGEventType> = rec
            .events()
            .into_iter()
            .map(|e| match e {
                Event::Mouse { kind, .. } => *kind,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseDragged,
                CGEventType::LeftMouseUp,
                CGEventType::MouseMoved,
                CGEventType::LeftMouseDown,
            ]
        );
        assert!(rec.posts[0].flags.contains(CGEventFlags::MaskShift));
        assert!(matches!(rec.posts[4].event, Event::Mouse { clicks: 2, .. }));
        assert!(rec.posts.iter().all(|p| p.route == Route::Pid(PID)));
    }

    #[test]
    fn right_clicks_go_through_the_hid_tap_even_for_windows() {
        let mut inj = window();
        let button = |down: bool| ScreenInput::Button {
            button: MouseButton::Right,
            down,
            clicks: 1,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
        };
        inj.inject(&button(true)).unwrap();
        inj.inject(&button(false)).unwrap();
        let rec = inj.backend();
        assert!(rec.posts.iter().all(|p| p.route == Route::Hid), "{:?}", rec.posts);
        assert!(matches!(
            rec.posts[0].event,
            Event::Mouse {
                kind: CGEventType::RightMouseDown,
                button: CGMouseButton::Right,
                number: 1,
                ..
            }
        ));
    }

    #[test]
    fn keys_carry_text_activate_the_owner_and_release_without_text() {
        let mut inj = window();
        let key = |action: KeyAction| ScreenInput::Key {
            code: KeyCode::A,
            action,
            mods: Mods::SUPER,
            text: Some("ä".into()),
        };
        inj.inject(&key(KeyAction::Press)).unwrap();
        inj.inject(&key(KeyAction::Repeat)).unwrap();
        inj.inject(&key(KeyAction::Release)).unwrap();
        let rec = inj.backend();
        assert_eq!(rec.activations, [PID]);
        let vk = keymap::virtual_key(KeyCode::A).unwrap();
        assert_eq!(
            rec.events(),
            [
                &Event::Key {
                    vk,
                    down: true,
                    modifier: false,
                    repeat: false,
                    text: Some("ä".into())
                },
                &Event::Key {
                    vk,
                    down: true,
                    modifier: false,
                    repeat: true,
                    text: Some("ä".into())
                },
                &Event::Key { vk, down: false, modifier: false, repeat: false, text: None },
            ]
        );
        assert!(rec.posts.iter().all(|p| p.flags.contains(CGEventFlags::MaskCommand)));
    }

    #[test]
    fn bare_modifiers_post_as_flags_changed_without_text() {
        let mut inj = display();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::ShiftLeft,
            action: KeyAction::Press,
            mods: Mods::SHIFT,
            text: Some("x".into()),
        })
        .unwrap();
        assert!(matches!(
            inj.backend().posts[0].event,
            Event::Key { modifier: true, down: true, text: None, .. }
        ));
        // Display streams never activate anything.
        assert!(inj.backend().activations.is_empty());
    }

    #[test]
    fn scrolls_keep_deltas_and_phases() {
        let mut inj = display();
        inj.inject(&ScreenInput::Scroll {
            dx: 1.5,
            dy: -3.0,
            precise: true,
            phase: ScrollPhase::Changed,
            momentum: ScrollPhase::None,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
        })
        .unwrap();
        assert_eq!(
            inj.backend().posts[0].event,
            Event::Scroll {
                at: pt(100.0, 50.0),
                dx: 1.5,
                dy: -3.0,
                precise: true,
                phase: ScrollPhase::Changed,
                momentum: ScrollPhase::None,
            }
        );
    }

    #[test]
    fn focus_activates_window_owners_only() {
        let mut inj = window();
        inj.focus().unwrap();
        assert_eq!(inj.backend().activations, [PID]);
        let mut inj = display();
        inj.focus().unwrap();
        assert!(inj.backend().activations.is_empty());
    }

    #[test]
    fn magnify_is_ignored() {
        let mut inj = window();
        inj.inject(&ScreenInput::Magnify {
            delta: 0.2,
            phase: ScrollPhase::Changed,
            x: 0.0,
            y: 0.0,
        })
        .unwrap();
        assert!(inj.backend().posts.is_empty());
    }
}
