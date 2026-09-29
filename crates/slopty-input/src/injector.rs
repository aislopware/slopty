//! The `CGEvent` injector: what [`Injector`] posts for each [`ScreenInput`], and where.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEventFlags, CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use slopty_capture::Rect;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput};

use crate::backend::{Backend, Event, Post, Route, System};
use crate::{InputError, PointerWatch, keymap, text};

/// How long bounds stay valid before a pointer event reads them itself. The stream's geometry
/// probe hands fresh ones over every 100 ms while the stream takes input
/// ([`Injector::set_bounds`]); this is long enough that a probe late by a slow window-server
/// answer does not put a read in front of a move, and short enough that an injector nobody
/// feeds still follows a window that moved.
pub(crate) const BOUNDS_TTL: Duration = Duration::from_millis(250);

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
    /// The same, for the stream's cursor samples.
    placed: PointerWatch,
    /// Keys pressed and not released, in press order.
    keys: Vec<KeyCode>,
    /// Modifier flags from the latest event, kept so bare modifier presses post correctly.
    flags: CGEventFlags,
    /// This injector's place among those holding Caps Lock ([`CapsClaims`]).
    caps: CapsHolder,
}

/// Caps Lock is the worker's, not a stream's: every stream that sets it shares one claim.
///
/// The claim takes the worker's own state as the first stream sets the lock, and puts it back
/// when the last lets go, unless the lock is no longer as the claim last set it: then the person
/// at the worker changed it, and it is theirs. Held per stream, a second stream read the first's
/// state as the worker's own and left the lock on as it went. While a stream holds it, the
/// worker's own state is kept on disk ([`keep_caps`]), so a crash puts it back at the next
/// start. The claim is bookkeeping only: the HID system is asked outside it.
#[derive(Debug, Default)]
pub struct CapsClaims {
    holders: Vec<u64>,
    /// The worker's own state, taken as the first holder set the lock; `None` when the HID
    /// system would not say.
    original: Option<bool>,
    /// The state the claim last set.
    set: Option<bool>,
    next: u64,
    /// Where the worker's own state is kept while a stream holds the lock.
    kept_at: Option<PathBuf>,
}

/// What the worker keeps on disk while a stream holds Caps Lock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapsKept {
    /// The worker's own state.
    pub original: bool,
    /// The state the claim last set.
    pub set: bool,
}

impl CapsKept {
    /// `original on|off`, then `set on|off`.
    #[must_use]
    pub fn to_text(self) -> String {
        let word = |on: bool| if on { "on" } else { "off" };
        format!("original {}\nset {}\n", word(self.original), word(self.set))
    }

    /// Read back what [`Self::to_text`] wrote; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut original = None;
        let mut set = None;
        for line in text.lines() {
            let (key, value) = line.split_once(' ')?;
            let on = match value {
                "on" => true,
                "off" => false,
                _ => return None,
            };
            match key {
                "original" => original = Some(on),
                "set" => set = Some(on),
                _ => return None,
            }
        }
        Some(Self { original: original?, set: set? })
    }

    /// The state to put back while the lock is `current`: the worker's own, unless the lock is
    /// no longer as the claim set it.
    #[must_use]
    pub fn restore(self, current: Option<bool>) -> Option<bool> {
        (current == Some(self.set) && self.set != self.original).then_some(self.original)
    }
}

impl CapsClaims {
    /// No stream holds the lock.
    #[must_use]
    pub const fn new() -> Self {
        Self { holders: Vec::new(), original: None, set: None, next: 0, kept_at: None }
    }

    /// A new holder's id.
    const fn id(&mut self) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// `who` sets the lock while it is `current` (read before, outside the claim): the worker's
    /// own state when `who` is the first holder.
    pub fn claim(&mut self, who: u64, current: Option<bool>) {
        if self.holders.contains(&who) {
            return;
        }
        if self.holders.is_empty() {
            self.original = current;
            self.set = None;
        }
        self.holders.push(who);
    }

    /// The claim set the lock `on`: what to keep on disk, and where.
    pub fn set(&mut self, on: bool) -> Option<(PathBuf, Option<CapsKept>)> {
        self.set = Some(on);
        self.kept()
    }

    /// `who` let go: what to put back once it was the last holder, and what to keep on disk.
    pub fn release(&mut self, who: u64) -> (Option<CapsKept>, Option<(PathBuf, Option<CapsKept>)>) {
        let before = self.holders.len();
        self.holders.retain(|&h| h != who);
        if self.holders.len() == before || !self.holders.is_empty() {
            return (None, None);
        }
        let back = self.snapshot();
        self.original = None;
        self.set = None;
        (back, self.kept())
    }

    fn snapshot(&self) -> Option<CapsKept> {
        Some(CapsKept { original: self.original?, set: self.set? })
    }

    /// Where to keep what, now.
    fn kept(&self) -> Option<(PathBuf, Option<CapsKept>)> {
        let path = self.kept_at.clone()?;
        let kept = if self.holders.is_empty() { None } else { self.snapshot() };
        Some((path, kept))
    }
}

/// Write `kept` at `path`, or remove it once there is none.
fn write_kept(path: &std::path::Path, kept: Option<CapsKept>) {
    let done = match kept {
        Some(kept) => std::fs::write(path, kept.to_text()),
        None => std::fs::remove_file(path)
            .or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
    };
    if let Err(e) = done {
        tracing::warn!(error = %e, path = %path.display(), "caps lock kept");
    }
}

/// Keep the worker's own Caps Lock at `path` while a stream holds it, through `backend`'s
/// claim, and first put back what a run that ended while holding it left there, unless the
/// lock was changed since.
pub fn keep_caps(path: PathBuf, backend: &mut impl Backend) {
    if let Some(left) = std::fs::read_to_string(&path).ok().as_deref().and_then(CapsKept::parse)
        && let Some(back) = left.restore(backend.caps_lock())
        && let Err(e) = backend.set_caps_lock(back)
    {
        tracing::warn!(error = %e, "caps lock not put back");
    }
    write_kept(&path, None);
    backend.caps_claims().lock().kept_at = Some(path);
}

/// The worker's one Caps Lock claim, shared by every stream on every connection.
pub type SharedCaps = std::sync::Arc<parking_lot::Mutex<CapsClaims>>;

/// An injector's place in its backend's [`CapsClaims`].
#[derive(Debug)]
struct CapsHolder {
    claims: SharedCaps,
    id: u64,
    holding: bool,
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
        let claims = backend.caps_claims();
        let id = claims.lock().id();
        let placed = PointerWatch::default();
        if route == Route::Hid {
            placed.follow_real();
        }
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
            placed,
            keys: Vec::new(),
            flags: CGEventFlags::empty(),
            caps: CapsHolder { claims, id, holding: false },
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

    /// Report where the pointer is put through `watch` from now on: events posted to the owner
    /// leave the worker's pointer alone, so the last place one was put is the pointer the
    /// stream shows; events through the HID tap move the real one.
    pub fn report_pointer(&mut self, watch: PointerWatch) {
        match (self.route, self.at) {
            (Route::Hid, _) => watch.follow_real(),
            (Route::Pid(_), Some(at)) => watch.place(at.x, at.y),
            (Route::Pid(_), None) => {}
        }
        self.placed = watch;
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
            ScreenInput::Key { code, action, mods } => {
                if !matches!(action, KeyAction::Release) {
                    self.ensure_active();
                }
                self.flags = flags_for(*mods);
                self.post_key(*code, *action)
            }
            ScreenInput::Text { text } => {
                self.ensure_active();
                self.post_text(text)
            }
            ScreenInput::Lock { caps } => {
                if !self.caps.holding {
                    self.caps.holding = true;
                    let current = self.backend.caps_lock();
                    self.caps.claims.lock().claim(self.caps.id, current);
                }
                self.flags.set(CGEventFlags::MaskAlphaShift, *caps);
                self.backend.set_caps_lock(*caps)?;
                let kept = self.caps.claims.lock().set(*caps);
                if let Some((path, kept)) = kept {
                    write_kept(&path, kept);
                }
                Ok(())
            }
            // A media key belongs to the worker's Now Playing app, not the target: it goes to
            // the system, from any stream.
            ScreenInput::Media { key, down } => {
                self.post(Some(Route::Hid), Event::Media { key: *key, down: *down })
            }
            // The stream's task selects the input source (`crate::sources`); nothing to post.
            ScreenInput::KeyboardSource { .. }
            | ScreenInput::KeyboardReleased
            | ScreenInput::Magnify { .. } => Ok(()),
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
    /// then each modifier, whose release carries the modifiers still down after it; then let go
    /// of Caps Lock, which goes back as the worker had it once no stream holds it. Nothing held
    /// posts nothing. Called when the stream ends, and on drop.
    pub fn release_all(&mut self) {
        for button in BUTTONS {
            if self.held & button.bit() == 0 {
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
        if std::mem::take(&mut self.caps.holding) {
            let (back, kept) = self.caps.claims.lock().release(self.caps.id);
            if let Some(to) = back.and_then(|back| back.restore(self.backend.caps_lock()))
                && let Err(e) = self.backend.set_caps_lock(to)
            {
                tracing::debug!(target = ?self.target, error = %e, "caps lock");
            }
            if let Some((path, kept)) = kept {
                write_kept(&path, kept);
            }
        }
    }

    fn release_key(&mut self, code: KeyCode) {
        if let Err(e) = self.post_key(code, KeyAction::Release) {
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
        if matches!(self.route, Route::Pid(_)) {
            self.placed.place(at.x, at.y);
        }
        Ok(at)
    }

    const fn move_type(&self) -> CGEventType {
        if self.held & MouseButton::Left.bit() != 0 {
            CGEventType::LeftMouseDragged
        } else if self.held & MouseButton::Right.bit() != 0 {
            CGEventType::RightMouseDragged
        } else if self.held != 0 {
            CGEventType::OtherMouseDragged
        } else {
            CGEventType::MouseMoved
        }
    }

    const fn set_held(&mut self, button: MouseButton, down: bool) {
        if down {
            self.held |= button.bit();
        } else {
            self.held &= !button.bit();
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

    fn post_key(&mut self, code: KeyCode, action: KeyAction) -> Result<(), InputError> {
        // Caps Lock is a lock the client sets (`ScreenInput::Lock`); its key would toggle it
        // on its press, whatever the client's state.
        let Some(vk) = keymap::virtual_key(code).filter(|_| code != KeyCode::CapsLock) else {
            tracing::debug!(?code, "no virtual key; dropped");
            return Ok(());
        };
        let down = !matches!(action, KeyAction::Release);
        let modifier = keymap::is_modifier(code);
        let posted = self.post(
            None,
            Event::Key { vk, down, modifier, repeat: matches!(action, KeyAction::Repeat) },
        );
        if !down {
            self.keys.retain(|&held| held != code);
        } else if posted.is_ok() && !self.keys.contains(&code) {
            self.keys.push(code);
        }
        posted
    }

    /// Type committed text: each piece of at most [`text::MOST_UNITS`] units on a press and a
    /// release of its own, with no modifiers, so ⌘ or ⌥ still held from a chord cannot make
    /// the text a shortcut.
    fn post_text(&mut self, text: &str) -> Result<(), InputError> {
        let flags = self.flags & CGEventFlags::MaskAlphaShift;
        for piece in text::chunks(text) {
            for down in [true, false] {
                let event = Event::Text { text: piece.clone(), down };
                self.backend.post(Post { route: self.route, flags, event })?;
            }
        }
        Ok(())
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

/// The flags a held modifier key sets, its side's device bit with them.
fn modifier_flag(code: KeyCode) -> CGEventFlags {
    flags_for(match code {
        KeyCode::ShiftLeft => Mods::SHIFT,
        KeyCode::ShiftRight => Mods::SHIFT | Mods::SHIFT_RIGHT,
        KeyCode::ControlLeft => Mods::CTRL,
        KeyCode::ControlRight => Mods::CTRL | Mods::CTRL_RIGHT,
        KeyCode::AltLeft => Mods::ALT,
        KeyCode::AltRight => Mods::ALT | Mods::ALT_RIGHT,
        KeyCode::MetaLeft => Mods::SUPER,
        KeyCode::MetaRight => Mods::SUPER | Mods::SUPER_RIGHT,
        KeyCode::Fn => Mods::FN,
        // Caps lock is a lock, not a hold: its flag says whether it is on, which letting go of
        // the key does not change.
        _ => Mods::empty(),
    })
}

/// Stream pixel → global point for a target with these bounds and pixels-per-point scale.
#[must_use]
pub fn to_point(bounds: Rect, scale: f64, x: f64, y: f64) -> CGPoint {
    let px = bounds.x + x / scale;
    let py = bounds.y + y / scale;
    CGPoint::new(px.clamp(bounds.x, bounds.x + bounds.w), py.clamp(bounds.y, bounds.y + bounds.h))
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

/// Wire modifiers → `CGEventFlags`: each modifier with its side's device bit (the left key
/// unless the client said right), Caps Lock's lock and fn.
#[must_use]
pub fn flags_for(mods: Mods) -> CGEventFlags {
    use slopty_platform::keyboard::device;
    let mut raw = 0_u64;
    for (held, right, flag, left_bit, right_bit) in [
        (
            Mods::SHIFT,
            Mods::SHIFT_RIGHT,
            CGEventFlags::MaskShift,
            device::LEFT_SHIFT,
            device::RIGHT_SHIFT,
        ),
        (
            Mods::CTRL,
            Mods::CTRL_RIGHT,
            CGEventFlags::MaskControl,
            device::LEFT_CONTROL,
            device::RIGHT_CONTROL,
        ),
        (
            Mods::ALT,
            Mods::ALT_RIGHT,
            CGEventFlags::MaskAlternate,
            device::LEFT_OPTION,
            device::RIGHT_OPTION,
        ),
        (
            Mods::SUPER,
            Mods::SUPER_RIGHT,
            CGEventFlags::MaskCommand,
            device::LEFT_COMMAND,
            device::RIGHT_COMMAND,
        ),
    ] {
        if mods.contains(held) {
            let side = if mods.contains(right) { right_bit } else { left_bit };
            raw |= flag.0 | u64::try_from(side).unwrap_or_default();
        }
    }
    if mods.contains(Mods::CAPS_LOCK) {
        raw |= CGEventFlags::MaskAlphaShift.0;
    }
    if mods.contains(Mods::FN) {
        raw |= CGEventFlags::MaskSecondaryFn.0;
    }
    CGEventFlags(raw)
}

#[cfg(test)]
mod tests {
    use slopty_core::{DisplayId, WindowId};
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
        Injector::with_backend(CaptureTarget::Display(DisplayId(1)), 1.0, Recorder::display(BOUNDS))
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
        assert!(flags_for(Mods::FN).contains(CGEventFlags::MaskSecondaryFn), "fn-click reads it");
        // The side rides in the device bits `NX_DEVICE{L,R}SHIFTKEYMASK`.
        assert_eq!(flags_for(Mods::SHIFT).0 & 0x6, 0x2, "left shift");
        assert_eq!(flags_for(Mods::SHIFT | Mods::SHIFT_RIGHT).0 & 0x6, 0x4, "right shift");
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

    /// A key goes by position with its modifiers and no text, activates the owner once, and
    /// its release goes too (`keys_carry_no_unicode_string` checks the built event).
    #[test]
    fn keys_go_by_position_activate_the_owner_and_release() {
        let mut inj = window();
        let key = |action: KeyAction| ScreenInput::Key {
            code: KeyCode::Q,
            action,
            mods: Mods::SUPER | Mods::SUPER_RIGHT,
        };
        inj.inject(&key(KeyAction::Press)).unwrap();
        inj.inject(&key(KeyAction::Repeat)).unwrap();
        inj.inject(&key(KeyAction::Release)).unwrap();
        let rec = inj.backend();
        assert_eq!(rec.activations, [PID]);
        let vk = 0x0c;
        assert_eq!(
            rec.events(),
            [
                &Event::Key { vk, down: true, modifier: false, repeat: false },
                &Event::Key { vk, down: true, modifier: false, repeat: true },
                &Event::Key { vk, down: false, modifier: false, repeat: false },
            ]
        );
        let right_cmd = CGEventFlags(
            CGEventFlags::MaskCommand.0
                | u64::try_from(slopty_platform::keyboard::device::RIGHT_COMMAND).unwrap(),
        );
        assert!(rec.posts.iter().all(|p| p.flags == right_cmd), "⌘ held by its right key");
    }

    #[test]
    fn bare_modifiers_post_as_flags_changed() {
        let mut inj = display();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::ShiftLeft,
            action: KeyAction::Press,
            mods: Mods::SHIFT,
        })
        .unwrap();
        assert!(matches!(
            inj.backend().posts[0].event,
            Event::Key { modifier: true, down: true, .. }
        ));
        // Display streams never activate anything.
        assert!(inj.backend().activations.is_empty());
    }

    /// Committed text activates the owner and goes in pieces a key event can carry, each a
    /// press and a release on the stream's route, with no modifier but Caps Lock: ⌘ held from
    /// a chord must not make the text a shortcut.
    #[test]
    fn text_goes_in_pieces_without_modifiers() {
        let mut inj = window();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::MetaLeft,
            action: KeyAction::Press,
            mods: Mods::SUPER,
        })
        .unwrap();
        let text = "Tiếng Việt có dấu và 日本語の文章";
        inj.inject(&ScreenInput::Text { text: text.to_owned() }).unwrap();
        let rec = inj.backend();
        assert_eq!(rec.activations, [PID]);
        let texts: Vec<(&str, bool, CGEventFlags, Route)> = rec
            .posts
            .iter()
            .filter_map(|p| match &p.event {
                Event::Text { text, down } => Some((text.as_str(), *down, p.flags, p.route)),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 4, "two pieces, each pressed and let go: {texts:?}");
        let typed: String = texts.iter().filter(|t| t.1).map(|t| t.0).collect();
        assert_eq!(typed, text);
        assert!(texts.iter().all(|t| t.2.is_empty() && t.3 == Route::Pid(PID)));
    }

    /// Caps Lock is set as a lock, never posted as a key, and put back as it was when the
    /// stream ends; a Caps Lock key press is dropped.
    #[test]
    fn caps_lock_sets_the_lock_not_a_key() {
        let mut inj = window();
        inj.backend.caps = Some(false);
        inj.inject(&ScreenInput::Lock { caps: true }).unwrap();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::CapsLock,
            action: KeyAction::Press,
            mods: Mods::empty(),
        })
        .unwrap();
        inj.inject(&ScreenInput::Lock { caps: false }).unwrap();
        inj.inject(&ScreenInput::Lock { caps: true }).unwrap();
        assert!(inj.backend().posts.is_empty(), "no key posted");
        assert_eq!(inj.backend().locks, [true, false, true]);
        inj.release_all();
        assert_eq!(inj.backend().locks, [true, false, true, false], "back as the worker had it");
        inj.release_all();
        assert_eq!(inj.backend().locks.len(), 4, "once");
    }

    /// One HID lock under every injector, as the worker has; each call checks that the shared
    /// claim is not held across it.
    #[derive(Debug, Clone)]
    struct OneLock(Recorder, std::sync::Arc<parking_lot::Mutex<(Option<bool>, Vec<bool>)>>);

    impl OneLock {
        fn off() -> Self {
            Self(
                Recorder::display(BOUNDS),
                std::sync::Arc::new(parking_lot::Mutex::new((Some(false), Vec::new()))),
            )
        }

        fn free(&self) {
            assert!(
                self.0.caps_claims.try_lock().is_some(),
                "the HID system asked under the claim"
            );
        }

        fn state(&self) -> Option<bool> {
            self.1.lock().0
        }

        fn sets(&self) -> Vec<bool> {
            self.1.lock().1.clone()
        }
    }

    impl Backend for OneLock {
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
            self.0.post(post)
        }

        fn caps_lock(&mut self) -> Option<bool> {
            self.free();
            self.state()
        }

        fn set_caps_lock(&mut self, on: bool) -> Result<(), InputError> {
            self.free();
            let mut lock = self.1.lock();
            lock.0 = Some(on);
            lock.1.push(on);
            drop(lock);
            Ok(())
        }

        fn caps_claims(&self) -> SharedCaps {
            self.0.caps_claims()
        }
    }

    fn lock_display(backend: OneLock) -> Injector<OneLock> {
        Injector::with_backend(CaptureTarget::Display(DisplayId(1)), 1.0, backend)
    }

    /// Two streams set Caps Lock on a worker whose lock was off: the first stream ending leaves
    /// it as the second wants it, and the second ending puts the worker's own state back. Held
    /// per stream, the second read the first's state as the worker's and left the lock on. The
    /// HID system is never asked while the claim every stream shares is held.
    #[test]
    fn caps_lock_goes_back_when_the_last_stream_lets_go() {
        let lock = OneLock::off();
        let mut first = lock_display(lock.clone());
        let mut second = lock_display(lock.clone());
        first.inject(&ScreenInput::Lock { caps: true }).unwrap();
        second.inject(&ScreenInput::Lock { caps: true }).unwrap();
        first.release_all();
        assert_eq!(lock.sets(), [true, true], "the second still holds it: left on");
        second.release_all();
        assert_eq!(lock.state(), Some(false), "back as the worker had it");
        assert_eq!(lock.sets(), [true, true, false]);
    }

    /// The person at the worker turns Caps Lock off while a stream holds it on: the stream
    /// ending leaves it as they set it rather than putting back the state from before.
    #[test]
    fn caps_lock_the_person_changed_is_left_as_they_set_it() {
        let lock = OneLock::off();
        lock.1.lock().0 = Some(true);
        let mut stream = lock_display(lock.clone());
        stream.inject(&ScreenInput::Lock { caps: false }).unwrap();
        lock.1.lock().0 = Some(true);
        stream.release_all();
        assert_eq!(lock.sets(), [false], "nothing put back over the person's change");
        assert_eq!(lock.state(), Some(true));
    }

    /// While a stream holds Caps Lock, the worker's own state and the one set are on disk; a
    /// start after a crash puts the worker's own back, unless the lock was changed since, and
    /// the file goes. A stream letting go removes it.
    #[test]
    fn caps_lock_is_kept_and_put_back_at_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("caps-lock");
        let lock = OneLock::off();
        keep_caps(path.clone(), &mut lock.clone());
        let mut stream = lock_display(lock);
        stream.inject(&ScreenInput::Lock { caps: true }).unwrap();
        let crashed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(CapsKept::parse(&crashed), Some(CapsKept { original: false, set: true }));
        stream.release_all();
        assert!(!path.exists(), "let go: nothing kept");
        std::fs::write(&path, crashed).unwrap();

        let after = OneLock::off();
        after.1.lock().0 = Some(true);
        keep_caps(path.clone(), &mut after.clone());
        assert_eq!(after.state(), Some(false), "the worker's own is back");
        assert!(!path.exists());

        std::fs::write(&path, CapsKept { original: false, set: true }.to_text()).unwrap();
        let changed = OneLock::off();
        keep_caps(path.clone(), &mut changed.clone());
        assert!(changed.sets().is_empty(), "changed since: left as it is");
        assert!(!path.exists());
    }

    /// Play/pause and the track keys go to the system, not the window's owner, which is not
    /// activated for them.
    #[test]
    fn media_keys_go_to_the_system() {
        use slopty_proto::screen::MediaKey;
        let mut inj = window();
        for down in [true, false] {
            inj.inject(&ScreenInput::Media { key: MediaKey::PlayPause, down }).unwrap();
        }
        let rec = inj.backend();
        assert!(rec.activations.is_empty());
        assert_eq!(
            rec.posts.iter().map(|p| (p.route, &p.event)).collect::<Vec<_>>(),
            [
                (Route::Hid, &Event::Media { key: MediaKey::PlayPause, down: true }),
                (Route::Hid, &Event::Media { key: MediaKey::PlayPause, down: false }),
            ]
        );
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
        let key = |code, mods| ScreenInput::Key { code, action: KeyAction::Press, mods };
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
        let key_up =
            |code, modifier| Event::Key { vk: vk(code), down: false, modifier, repeat: false };
        let both = flags_for(Mods::SUPER | Mods::SHIFT);
        assert!(both.contains(CGEventFlags::MaskCommand | CGEventFlags::MaskShift));
        assert_eq!(
            ups,
            [
                (mouse_up(CGEventType::LeftMouseUp, CGMouseButton::Left, 0), both),
                (mouse_up(CGEventType::OtherMouseUp, CGMouseButton::Center, 2), both),
                (key_up(KeyCode::Z, false), both),
                (key_up(KeyCode::MetaLeft, true), flags_for(Mods::SHIFT)),
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
        let key = |action| ScreenInput::Key { code: KeyCode::A, action, mods: Mods::empty() };
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
        let mut inj = Injector::with_backend(CaptureTarget::Display(DisplayId(1)), 1.0, backend);
        inj.inject(&ScreenInput::Key {
            code: KeyCode::MetaLeft,
            action: KeyAction::Press,
            mods: Mods::SUPER,
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
