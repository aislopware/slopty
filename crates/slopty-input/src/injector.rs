//! The `CGEvent` injector: what [`Injector`] posts for each [`ScreenInput`], and where.
use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEventFlags, CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use slopty_capture::Rect;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput};

use crate::backend::{Backend, Event, Post, Route, System};
use crate::{InputError, keymap};

/// How long bounds stay valid; windows move rarely, pointer events are dense.
pub const BOUNDS_TTL: Duration = Duration::from_millis(100);

/// How long an owner found active, or just activated, is taken to stay active. Activation lands
/// some milliseconds after it is asked for, and the lookup behind the check costs 1–2 ms
/// (MEASUREMENTS.md, "input injection off the runtime"), so checking before every key-down
/// re-activated an owner that was already on its way and paid the lookup on every keystroke.
const ACTIVE_TTL: Duration = Duration::from_millis(250);

/// Every button, in the order [`Injector::release_all`] lets go of them.
const BUTTONS: [MouseButton; 5] = [
    MouseButton::Left,
    MouseButton::Right,
    MouseButton::Middle,
    MouseButton::Back,
    MouseButton::Forward,
];

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
///
/// It keeps what the client holds down (buttons, keys, modifiers) and lets go of all of it on
/// [`Self::release_all`] and when dropped, so a stream that ends mid-chord or mid-drag leaves
/// nothing down on the worker.
#[derive(Debug)]
pub struct Injector<B: Backend = System> {
    backend: B,
    target: CaptureTarget,
    /// Stream pixels per display point.
    scale: f64,
    route: Route,
    bounds: Option<Rect>,
    bounds_at: Option<Instant>,
    /// When the owner was last found active or activated.
    active_at: Option<Instant>,
    /// Buttons currently held, so moves post as drags.
    held: u8,
    /// Where the pointer was last put: where held buttons are let go.
    at: Option<CGPoint>,
    /// Keys pressed and not released, in press order.
    keys: Vec<KeyCode>,
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
            active_at: None,
            held: 0,
            at: None,
            keys: Vec::new(),
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
        self.backend.activate(pid)?;
        self.active_at = Some(Instant::now());
        Ok(())
    }

    /// Let go of everything held down: each button where the pointer last was, then each key,
    /// then each modifier, whose release carries the modifiers still down after it. Nothing
    /// held posts nothing. Called when the stream ends, and on drop.
    pub fn release_all(&mut self) {
        for button in BUTTONS {
            if self.held & button_bit(button) == 0 {
                continue;
            }
            self.set_held(button, false);
            let Some(at) = self.at else { continue };
            let (kind, cg_button, number) = button_event(button, false);
            if let Err(e) = self.post_mouse(kind, at, cg_button, number, 1) {
                tracing::debug!(target = ?self.target, ?button, error = %e, "release");
            }
        }
        let (modifiers, plain): (Vec<KeyCode>, Vec<KeyCode>) =
            std::mem::take(&mut self.keys).into_iter().partition(|&c| keymap::is_modifier(c));
        for code in plain {
            self.release_key(code);
        }
        for (n, code) in modifiers.iter().enumerate() {
            let still = modifiers.get(n.saturating_add(1)..).unwrap_or_default();
            let locked = self.flags & CGEventFlags::MaskAlphaShift;
            self.flags = still.iter().fold(locked, |flags, &c| flags | modifier_flag(c));
            self.release_key(*code);
        }
    }

    fn release_key(&mut self, code: KeyCode) {
        if let Err(e) = self.post_key(code, KeyAction::Release, None) {
            tracing::debug!(target = ?self.target, ?code, error = %e, "release");
        }
    }

    /// Bounds read somewhere else at `at`, so the next pointer event need not read them. Older
    /// than what the injector has, they are ignored.
    pub fn set_bounds(&mut self, bounds: Option<Rect>, at: Instant) {
        if self.bounds_at.is_none_or(|had| at > had) {
            self.bounds = bounds;
            self.bounds_at = Some(at);
        }
    }

    /// Activate the owner if it is not the active app (see the module docs), unless it was
    /// found active or activated within [`ACTIVE_TTL`].
    fn ensure_active(&mut self) {
        let Route::Pid(pid) = self.route else { return };
        if self.active_at.is_some_and(|at| at.elapsed() < ACTIVE_TTL) {
            return;
        }
        if !self.backend.is_active(pid) {
            // A vanished owner is reported by the next post, not here.
            let _gone = self.backend.activate(pid);
        }
        self.active_at = Some(Instant::now());
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

    /// Stream pixels → global display points, reading the bounds when they are stale.
    fn point(&mut self, x: f32, y: f32) -> Result<CGPoint, InputError> {
        let stale = self.bounds_at.is_none_or(|at| at.elapsed() > BOUNDS_TTL);
        if stale {
            let read_at = Instant::now();
            self.bounds = self.backend.bounds(self.target);
            self.bounds_at = Some(read_at);
        }
        let rect = self.bounds.ok_or(InputError::NoBounds)?;
        let at = to_point(rect, self.scale, f64::from(x), f64::from(y));
        self.at = Some(at);
        Ok(at)
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
        // even for window streams; the worker's pointer does move for those, which is the
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
        let posted = self.post(
            None,
            Event::Key { vk, down, modifier, repeat: matches!(action, KeyAction::Repeat), text },
        );
        if !down {
            self.keys.retain(|&held| held != code);
        } else if posted.is_ok() && !self.keys.contains(&code) {
            self.keys.push(code);
        }
        posted
    }

    /// Post through the backend; `route` overrides the stream's own route.
    fn post(&mut self, route: Option<Route>, event: Event) -> Result<(), InputError> {
        let route = route.unwrap_or(self.route);
        self.backend.post(Post { route, flags: self.flags, event })
    }
}

impl<B: Backend> Drop for Injector<B> {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// The flag a held modifier key sets.
const fn modifier_flag(code: KeyCode) -> CGEventFlags {
    match code {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => CGEventFlags::MaskShift,
        KeyCode::ControlLeft | KeyCode::ControlRight => CGEventFlags::MaskControl,
        KeyCode::AltLeft | KeyCode::AltRight => CGEventFlags::MaskAlternate,
        KeyCode::MetaLeft | KeyCode::MetaRight => CGEventFlags::MaskCommand,
        KeyCode::Fn => CGEventFlags::MaskSecondaryFn,
        // Caps lock is a lock, not a hold: its flag says whether it is on, which letting go of
        // the key does not change.
        _ => CGEventFlags::empty(),
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
    use crate::backend::Recorder;

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

    /// `release_all` lets go of every held button where the pointer last was, then every key,
    /// then every modifier, each modifier's release carrying only the modifiers still down; the
    /// held state is empty afterwards, so a second call posts nothing.
    #[test]
    fn release_all_lets_go_of_every_button_key_and_modifier() {
        let mut inj = display();
        let key =
            |code, mods| ScreenInput::Key { code, action: KeyAction::Press, mods, text: None };
        let button = |button| ScreenInput::Button {
            button,
            down: true,
            clicks: 1,
            x: 4.0,
            y: 6.0,
            mods: Mods::SUPER | Mods::SHIFT,
        };
        inj.inject(&key(KeyCode::MetaLeft, Mods::SUPER)).unwrap();
        inj.inject(&key(KeyCode::ShiftLeft, Mods::SUPER | Mods::SHIFT)).unwrap();
        inj.inject(&key(KeyCode::Z, Mods::SUPER | Mods::SHIFT)).unwrap();
        inj.inject(&button(MouseButton::Left)).unwrap();
        inj.inject(&button(MouseButton::Middle)).unwrap();
        inj.inject(&ScreenInput::Move { x: 8.0, y: 9.0 }).unwrap();
        let before = inj.backend().posts.len();
        inj.release_all();

        let ups: Vec<(Event, CGEventFlags)> =
            inj.backend().posts[before..].iter().map(|p| (p.event.clone(), p.flags)).collect();
        let vk = |code| keymap::virtual_key(code).unwrap();
        let mouse_up = |kind, button, number| Event::Mouse {
            kind,
            at: pt(108.0, 59.0),
            button,
            number,
            clicks: 1,
        };
        let key_up = |code, modifier| Event::Key {
            vk: vk(code),
            down: false,
            modifier,
            repeat: false,
            text: None,
        };
        let both = CGEventFlags::MaskCommand | CGEventFlags::MaskShift;
        assert_eq!(
            ups,
            [
                (mouse_up(CGEventType::LeftMouseUp, CGMouseButton::Left, 0), both),
                (mouse_up(CGEventType::OtherMouseUp, CGMouseButton::Center, 2), both),
                (key_up(KeyCode::Z, false), both),
                (key_up(KeyCode::MetaLeft, true), CGEventFlags::MaskShift),
                (key_up(KeyCode::ShiftLeft, true), CGEventFlags::empty()),
            ]
        );
        assert_eq!(inj.move_type(), CGEventType::MouseMoved, "no button is held");
        let after = inj.backend().posts.len();
        inj.release_all();
        assert_eq!(inj.backend().posts.len(), after, "nothing is held the second time");
    }

    /// A key released by the client is no longer held; a repeat does not hold it twice.
    #[test]
    fn released_keys_are_not_released_again() {
        let mut inj = display();
        let key =
            |action| ScreenInput::Key { code: KeyCode::A, action, mods: Mods::empty(), text: None };
        inj.inject(&key(KeyAction::Press)).unwrap();
        inj.inject(&key(KeyAction::Repeat)).unwrap();
        inj.inject(&key(KeyAction::Release)).unwrap();
        let posted = inj.backend().posts.len();
        inj.release_all();
        assert_eq!(inj.backend().posts.len(), posted);
    }

    /// Dropping the injector, as a closed stream does, lets go of what it held.
    #[test]
    fn dropping_the_injector_lets_go() {
        #[derive(Debug)]
        struct Shared(Recorder, std::rc::Rc<std::cell::RefCell<Vec<Post>>>);
        impl Backend for Shared {
            fn owner_pid(&self, target: CaptureTarget) -> Option<i32> {
                self.0.owner_pid(target)
            }

            fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
                self.0.bounds(target)
            }

            fn is_active(&mut self, pid: i32) -> bool {
                self.0.is_active(pid)
            }

            fn activate(&mut self, pid: i32) -> Result<(), InputError> {
                self.0.activate(pid)
            }

            fn post(&mut self, post: Post) -> Result<(), InputError> {
                self.1.borrow_mut().push(post);
                Ok(())
            }
        }
        let tap = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let backend = Shared(Recorder::display(BOUNDS), std::rc::Rc::clone(&tap));
        let mut inj = Injector::with_backend(CaptureTarget::Display(1), 1.0, backend);
        inj.inject(&ScreenInput::Key {
            code: KeyCode::MetaLeft,
            action: KeyAction::Press,
            mods: Mods::SUPER,
            text: None,
        })
        .unwrap();
        drop(inj);
        let posts = tap.borrow();
        assert!(
            matches!(posts.as_slice(), [_, Post { event: Event::Key { down: false, .. }, flags, .. }]
                if flags.is_empty()),
            "{posts:?}"
        );
    }

    /// Bounds handed in fresh are used as they are; stale ones are read again.
    #[test]
    fn fresh_bounds_from_outside_spare_the_read() {
        let mut inj = display();
        inj.set_bounds(Some(Rect { x: 1000.0, y: 0.0, w: 10.0, h: 10.0 }), Instant::now());
        inj.inject(&ScreenInput::Move { x: 5.0, y: 5.0 }).unwrap();
        assert_eq!(inj.backend().bounds_reads, 0);
        assert!(
            matches!(inj.backend().posts[0].event, Event::Mouse { at, .. } if at == pt(1005.0, 5.0))
        );
        // Older than what the injector has: ignored.
        let old = Instant::now().checked_sub(BOUNDS_TTL.saturating_mul(3)).unwrap();
        inj.set_bounds(None, old);
        inj.inject(&ScreenInput::Move { x: 5.0, y: 5.0 }).unwrap();
        assert_eq!(inj.backend().bounds_reads, 0);
        // Stale: the injector reads them itself.
        let mut inj = display();
        inj.set_bounds(Some(Rect { x: 1000.0, y: 0.0, w: 10.0, h: 10.0 }), old);
        inj.inject(&ScreenInput::Move { x: 5.0, y: 5.0 }).unwrap();
        assert_eq!(inj.backend().bounds_reads, 1);
        assert!(
            matches!(inj.backend().posts[0].event, Event::Mouse { at, .. } if at == pt(105.0, 55.0))
        );
    }

    /// A burst of key presses looks the owner up once, not once per key, while the owner was
    /// just found active or activated.
    #[test]
    fn a_burst_of_keys_checks_the_owner_once() {
        let mut inj = window();
        for _ in 0..20 {
            inj.inject(&ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Press,
                mods: Mods::empty(),
                text: Some("a".into()),
            })
            .unwrap();
        }
        assert_eq!(inj.backend().active_checks, 1);
        assert_eq!(inj.backend().activations, [PID]);
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
