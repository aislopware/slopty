//! The `CGEvent` injector: what [`Injector`] posts for each [`ScreenInput`], and where.
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEventFlags, CGEventType, CGMouseButton, CGPreflightPostEventAccess, CGRequestPostEventAccess,
};
use slopty_capture::Rect;
use slopty_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use slopty_proto::screen::{CaptureTarget, ScreenInput, ScrollPhase};

use crate::backend::{Backend, Event, Gesture, Post, Route, System};
use crate::nudge::{self, Nudge};
use crate::{DragStep, InputError, PointerWatch, keymap, text};

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

/// How far [`Injector::press_at`]'s first drag goes from its press, in points: a drag the view
/// under the press sees (`mouseDragged:`), which begins a drag session from that press.
pub const DRAG_START: f64 = 2.0;

/// A gesture whose client stamps step by more than this, or back, starts its timeline again
/// ([`Timeline`]): whatever it was, it is not the next report of the same fingers.
const TIMELINE_GAP_US: i64 = 1_000_000;

/// The number the last press posted by any injector in this process took
/// ([`Event::Mouse::press`](crate::Event)): each press takes the next, so no two presses on
/// two streams share one.
static PRESSES: AtomicI64 = AtomicI64::new(0);

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
    /// The tile sends its trackpad gestures ([`ScreenInput::Gestures`]): a trackpad scroll
    /// comes with its gesture.
    gestures: bool,
    /// The gestures under way on the target, which a stream ending closes.
    open: Open,
    /// The number of each held button's press, by button number (0 when not held): what its
    /// drags and its release carry.
    presses: [i64; BUTTONS.len()],
    /// A drag session is being fed through the HID tap ([`Self::enter_drag`]).
    drag: Option<Drag>,
    /// The gesture under way on this Mac's event clock ([`ScreenInput::time_us`]).
    timeline: Timeline,
}

/// A gesture's events placed on this Mac's event clock at the client's spacing
/// ([`ScreenInput::time_us`]).
///
/// Each event is stamped at its client stamp plus the least delay any event of the gesture has
/// had from the client to here so far. An event the network or the scheduler held up is then
/// stamped where it would have been with no hold-up, since an event before it came through
/// faster; the delay itself is never taken off, so no stamp is ever later than when its event
/// is posted, and none goes back. The clocks are never compared: the delay is the difference
/// of two clocks plus the path, and only its changes within one gesture count, over which two
/// Macs' clocks drift apart by microseconds.
#[derive(Clone, Copy, Debug, Default)]
struct Timeline {
    /// The last client stamp, as the wire has it and widened to microseconds since the
    /// gesture's first.
    last: Option<(u32, i64)>,
    /// The least of (this Mac's clock at an event − its client stamp) over the gesture, ns.
    lag: i64,
    /// The last stamp given.
    given: u64,
}

impl Timeline {
    /// The stamp for an event the client stamped `client_us`, handled `now_ns`; `begins` when
    /// it opens a gesture.
    fn place(&mut self, client_us: u32, now_ns: u64, begins: bool) -> u64 {
        let now = i64::try_from(now_ns).unwrap_or(i64::MAX);
        let next = self.last.filter(|_| !begins).and_then(|(raw, wide)| {
            let step = i64::from(client_us.wrapping_sub(raw).cast_signed());
            (0..=TIMELINE_GAP_US).contains(&step).then(|| wide.saturating_add(step))
        });
        let (wide, lag) = match next {
            Some(wide) => (wide, self.lag.min(now.saturating_sub(wide.saturating_mul(1000)))),
            None => (0, now),
        };
        self.last = Some((client_us, wide));
        self.lag = lag;
        let at = u64::try_from(wide.saturating_mul(1000).saturating_add(lag)).unwrap_or(0);
        self.given = at.max(self.given).min(now_ns);
        self.given
    }
}

/// A drag the injector feeds through the HID tap, from [`Injector::enter_drag`] to
/// [`Injector::leave_drag`].
#[derive(Clone, Copy, Debug)]
struct Drag {
    /// Where the worker's real pointer was as a window stream's drag began: where it goes back
    /// to after. A display stream's input moves the real pointer anyway, and leaves it.
    home: Option<CGPoint>,
    /// Where the pressed drag rests and when it was last posted, so a rest is nudged; `None`
    /// until the press.
    nudge: Option<Nudge>,
    /// How long a rest is before a nudge, from the worker's spring delay as the drag began
    /// ([`nudge::rest_us`]).
    rest_us: u64,
    /// What the nudges' microseconds count from.
    epoch: Instant,
    /// The client's own left press on a window stream began it ([`Injector::follow_press`]),
    /// and its release ends it.
    pressed: bool,
}

impl Drag {
    /// Microseconds from the drag's epoch to `now`.
    fn us(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.epoch).as_micros()).unwrap_or(u64::MAX)
    }
}

/// The gestures under way on the target: what [`Injector::release_all`] closes, so a stream that
/// ends mid-swipe or mid-pinch leaves the app in none.
#[derive(Clone, Copy, Debug, Default)]
struct Open {
    /// A trackpad scroll in its gesture phases, and whether it comes with its gesture, as
    /// [`Injector::gestures`] was when it opened; `None` between. A scroll is paired from its
    /// first phase to its last or not at all, so the tile turning its gestures on or off halfway
    /// leaves none half open.
    scroll: Option<bool>,
    /// The scroll's last phase was may-begin: the began after it opens the same gesture.
    may_begin: bool,
    /// A scroll coasting, between its momentum's start and end.
    coast: bool,
    /// A pinch under way.
    magnify: bool,
    /// A rotation under way.
    rotate: bool,
}

impl Open {
    /// Whether the scroll in `phase` comes with its gesture, `gestures` being the tile's word
    /// now; the scroll's own state moves on with it.
    fn scroll(&mut self, phase: ScrollPhase, gestures: bool) -> bool {
        let paired = match phase {
            ScrollPhase::MayBegin => *self.scroll.insert(gestures),
            ScrollPhase::Began if !self.may_begin => *self.scroll.insert(gestures),
            // One that never opened here (a lost start, or a stream that took over mid-gesture)
            // goes on unpaired: a gesture must not end that never began.
            _ => *self.scroll.get_or_insert(false),
        };
        self.may_begin = phase == ScrollPhase::MayBegin;
        if matches!(phase, ScrollPhase::Ended | ScrollPhase::Cancelled) {
            self.scroll = None;
        }
        paired
    }
}

/// Whether a gesture in `phase` is still under way after it.
const fn under_way(phase: ScrollPhase) -> bool {
    !matches!(phase, ScrollPhase::Ended | ScrollPhase::Cancelled | ScrollPhase::None)
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
            gestures: false,
            open: Open::default(),
            presses: [0; BUTTONS.len()],
            drag: None,
            timeline: Timeline::default(),
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
            (Route::Pid(_) | Route::Window { .. }, Some(at)) => watch.place(at.x, at.y),
            (Route::Pid(_) | Route::Window { .. }, None) => {}
        }
        self.placed = watch;
    }

    /// Apply one input event.
    pub fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        tracing::trace!(target = ?self.target, ?input, "inject");
        match input {
            ScreenInput::Move { x, y } => self.move_to(*x, *y),
            ScreenInput::Button { button, down, clicks, x, y, mods } => {
                let at = self.point(*x, *y)?;
                if *down {
                    self.ensure_active();
                }
                let left = *button == MouseButton::Left;
                if left && *down {
                    self.follow_press(at);
                }
                self.flags = flags_for(*mods);
                let (kind, cg_button, number) = button_event(*button, *down);
                self.set_held(*button, *down);
                let posted =
                    self.post_mouse(kind, at, cg_button, number, i64::from((*clicks).max(1)));
                if left && !*down && self.drag.is_some_and(|d| d.pressed) {
                    self.leave_drag();
                }
                posted
            }
            ScreenInput::Scroll { dx, dy, precise, phase, momentum, x, y, mods, time_us } => {
                let at = self.point(*x, *y)?;
                self.flags = flags_for(*mods);
                let (dx, dy, precise, phase, momentum) = (*dx, *dy, *precise, *phase, *momentum);
                // A wheel's lines are no gesture, and nothing reads their timing.
                let stamp = if precise { self.stamp(*time_us, phase) } else { 0 };
                self.post(None, Event::Scroll { at, dx, dy, precise, phase, momentum, stamp })?;
                // A trackpad's scroll comes with a gesture, after it, and its coast without
                // one: what an app following a swipe between pages as it moves reads (Safari's
                // back and forward), which a scroll alone leaves stuck at its first step and
                // cancels (`tests/inject.rs`). Only while the tile sends its gestures.
                if precise && momentum != ScrollPhase::None {
                    self.open.coast = under_way(momentum);
                }
                if precise && phase != ScrollPhase::None {
                    let paired = self.open.scroll(phase, self.gestures);
                    if paired {
                        let gesture = Gesture::Scroll { dx, dy };
                        self.post(None, Event::Gesture { at, gesture, phase, stamp })?;
                    }
                }
                Ok(())
            }
            ScreenInput::Magnify { delta, phase, x, y, time_us } => {
                let at = self.point(*x, *y)?;
                let gesture = Gesture::Magnify(*delta);
                let (phase, stamp) = (*phase, self.stamp(*time_us, *phase));
                self.open.magnify = under_way(phase);
                self.post(None, Event::Gesture { at, gesture, phase, stamp })
            }
            ScreenInput::Rotate { degrees, phase, x, y, time_us } => {
                let at = self.point(*x, *y)?;
                let gesture = Gesture::Rotate(*degrees);
                let (phase, stamp) = (*phase, self.stamp(*time_us, *phase));
                self.open.rotate = under_way(phase);
                self.post(None, Event::Gesture { at, gesture, phase, stamp })
            }
            ScreenInput::SmartMagnify { x, y } => {
                let at = self.point(*x, *y)?;
                let gesture = Gesture::SmartMagnify;
                self.post(None, Event::Gesture { at, gesture, phase: ScrollPhase::None, stamp: 0 })
            }
            // AppKit makes one swipe of every navigation swipe posted, a directionless one
            // included, so a swipe is one event: its end, with its direction.
            ScreenInput::Swipe { direction, x, y } => {
                let at = self.point(*x, *y)?;
                let gesture = Gesture::Swipe(*direction);
                self.post(None, Event::Gesture { at, gesture, phase: ScrollPhase::Ended, stamp: 0 })
            }
            ScreenInput::Gestures { remote } => {
                self.gestures = *remote;
                Ok(())
            }
            ScreenInput::Key { code, action, mods } => {
                if !matches!(action, KeyAction::Release) {
                    self.ensure_active();
                }
                self.flags = flags_for(*mods);
                self.post_key(*code, *action)
            }
            // Held by the connection until the client's clipboard is on the pasteboard; here it
            // is the press it stands for.
            ScreenInput::PasteChord { code, mods } => {
                self.ensure_active();
                self.flags = flags_for(*mods);
                self.post_key(*code, KeyAction::Press)
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
            // The stream's task selects the input source (`crate::sources`) and carries a drag
            // as `DragStep`s, between the helper and here; nothing to post.
            ScreenInput::KeyboardSource { .. }
            | ScreenInput::KeyboardReleased
            | ScreenInput::Drag(_) => Ok(()),
        }
    }

    /// The pointer to the stream pixel `(x, y)`: a move, or a drag of what is held, a drag
    /// session's included, which rests there from now on.
    fn move_to(&mut self, x: f32, y: f32) -> Result<(), InputError> {
        let at = self.point(x, y)?;
        let kind = self.move_type();
        let (_press, cg_button, number) =
            button_event(self.held_button().unwrap_or(MouseButton::Left), true);
        self.post_mouse(kind, at, cg_button, number, 0)?;
        if let Some(drag) = &mut self.drag {
            let now = drag.us(Instant::now());
            if let Some(nudge) = &mut drag.nudge {
                nudge.moved((at.x, at.y), now);
            }
        }
        Ok(())
    }

    /// Carry a drag session one step on ([`DragStep`]).
    pub fn drag_step(&mut self, step: DragStep) {
        let done = match step {
            DragStep::Enter { x, y, answer } => {
                self.enter_drag();
                let at = self.point(x, y).map(|at| (at.x, at.y));
                if at.is_err() {
                    self.leave_drag();
                }
                let _gone = answer.send(at);
                Ok(())
            }
            DragStep::Press { x, y } => self.press_at(x, y).map(drop),
            DragStep::Move { x, y } if self.drag.is_some() => self.move_to(x, y),
            DragStep::Move { .. } => Ok(()),
            DragStep::Release => {
                self.leave_drag();
                Ok(())
            }
            DragStep::Cancel => self.cancel_held(),
            DragStep::Locate { x, y, answer } => {
                let _gone = answer.send(self.point(x, y).map(|at| (at.x, at.y)));
                Ok(())
            }
        };
        if let Err(e) = done {
            tracing::debug!(target = ?self.target, error = %e, "drag step");
        }
    }

    /// Bring the target's application to the front so it takes keyboard events.
    pub fn focus(&mut self) -> Result<(), InputError> {
        let Route::Pid(pid) = self.route else { return Ok(()) };
        self.backend.activate(pid, self.window())?;
        self.active_at = Some(Instant::now());
        Ok(())
    }

    /// Let go of everything held down: a drag, cancelled rather than dropped; each button where
    /// the pointer last was; each gesture under way, cancelled; then each key, then each
    /// modifier, whose release carries the modifiers still down after it; then Caps Lock, which
    /// goes back as the worker had it once no stream holds it. Nothing held posts nothing.
    /// Called when the stream ends, and on drop.
    pub fn release_all(&mut self) {
        if let Err(e) = self.cancel_drag() {
            tracing::debug!(target = ?self.target, error = %e, "drag cancel");
        }
        self.release_buttons();
        self.close_gestures();
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

    /// Cancel each gesture under way where the pointer last was: the scroll and its gesture,
    /// its coast, a pinch, a rotation. The next gesture's stamps start again.
    fn close_gestures(&mut self) {
        let open = std::mem::take(&mut self.open);
        self.timeline = Timeline::default();
        let Some(at) = self.at else { return };
        let cancelled = ScrollPhase::Cancelled;
        let mut events = Vec::new();
        if let Some(paired) = open.scroll {
            let (dx, dy, precise, momentum) = (0.0, 0.0, true, ScrollPhase::None);
            events.push(Event::Scroll {
                at,
                dx,
                dy,
                precise,
                phase: cancelled,
                momentum,
                stamp: 0,
            });
            if paired {
                let gesture = Gesture::Scroll { dx, dy };
                events.push(Event::Gesture { at, gesture, phase: cancelled, stamp: 0 });
            }
        }
        if open.coast {
            let (dx, dy, phase, momentum) = (0.0, 0.0, ScrollPhase::None, ScrollPhase::Ended);
            events.push(Event::Scroll { at, dx, dy, precise: true, phase, momentum, stamp: 0 });
        }
        if open.magnify {
            let gesture = Gesture::Magnify(0.0);
            events.push(Event::Gesture { at, gesture, phase: cancelled, stamp: 0 });
        }
        if open.rotate {
            let gesture = Gesture::Rotate(0.0);
            events.push(Event::Gesture { at, gesture, phase: cancelled, stamp: 0 });
        }
        for event in events {
            if let Err(e) = self.post(None, event) {
                tracing::debug!(target = ?self.target, error = %e, "gesture cancel");
            }
        }
    }

    /// Let go of each held button where the pointer last was, on the route it went down on.
    fn release_buttons(&mut self) {
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
    }

    /// Feed a drag session through the HID tap from now on (`docs/decisions/audio.md`, "Drag
    /// and drop lands at the point"). The drag manager picks its target under the real pointer,
    /// and an event posted to a pid moves neither, so until [`Self::leave_drag`] every pointer
    /// event of this stream goes through the HID tap and moves the worker's real pointer, a
    /// window stream's included. A window stream's owner is brought to the front first, since
    /// the HID tap reaches whatever window is on top at the point, and the real pointer's place
    /// is kept to put it back after. A button still held from before is let go first, on the
    /// route it went down on. A press, its drags and its release share one number here as
    /// everywhere. Entering twice changes nothing.
    pub fn enter_drag(&mut self) {
        if self.drag.is_some() {
            return;
        }
        self.release_buttons();
        let home = match self.route {
            Route::Hid => None,
            Route::Pid(pid) | Route::Window { pid, .. } => {
                // A vanished owner is reported by the next post, not here.
                let _gone = self.backend.activate(pid, self.window());
                self.active_at = Some(Instant::now());
                self.backend.pointer()
            }
        };
        self.feed_hid(home, false);
    }

    /// Feed this stream's pointer through the HID tap from now on, the real pointer at `home`
    /// to go back to after; `pressed` when the client's own press began it.
    fn feed_hid(&mut self, home: Option<CGPoint>, pressed: bool) {
        self.placed.follow_real();
        let rest_us = nudge::rest_us(self.backend.spring_delay_s());
        self.drag = Some(Drag { home, nudge: None, rest_us, epoch: Instant::now(), pressed });
    }

    /// A left press at the global point `at` on a window stream goes through the HID tap, its
    /// drags and its release after it, when the window is on top there: an app begins a drag
    /// session only from a press the window server saw (P0 (4)), and the drag manager follows
    /// the real pointer, so a drag out of the app can begin and be carried
    /// (`docs/decisions/audio.md`, "Drag out"). The release puts the real pointer back where it
    /// was. A window something covers at the point keeps its own route for the press, since the
    /// HID tap would reach what covers it, and a drag cannot begin from that press. Display
    /// streams go through the HID tap anyway.
    fn follow_press(&mut self, at: CGPoint) {
        let Route::Pid(pid) = self.route else { return };
        if self.drag.is_some() || !self.backend.uncovered_at(self.target, pid, at) {
            return;
        }
        let home = self.backend.pointer();
        self.feed_hid(home, true);
    }

    /// Whether a drag is being fed through the HID tap ([`Self::enter_drag`]).
    #[must_use]
    pub const fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The number the held left press carries, which each of its drags and its release carry
    /// too; `None` while the left button is up.
    #[must_use]
    pub fn press_number(&self) -> Option<i64> {
        self.presses.first().copied().filter(|&number| number != 0)
    }

    /// Press the left button at the stream pixel `(x, y)` and drag [`DRAG_START`] points to the
    /// right, both through the HID tap under one new number: what a drag source's view needs to
    /// begin its session from the press. Enters drag mode first ([`Self::enter_drag`]); a press
    /// still held is let go before the new one. The press's number comes back.
    ///
    /// # Errors
    ///
    /// The target is gone, or the system would not take an event. Then no drag is left on:
    /// what was posted is let go and the stream has its own route back.
    pub fn press_at(&mut self, x: f32, y: f32) -> Result<i64, InputError> {
        let at = self.point(x, y)?;
        self.enter_drag();
        self.release_buttons();
        self.set_held(MouseButton::Left, true);
        let pressed = self
            .post_mouse(CGEventType::LeftMouseDown, at, CGMouseButton::Left, 0, 1)
            .and_then(|()| {
                let start = self.within(CGPoint { x: at.x + DRAG_START, y: at.y });
                self.at = Some(start);
                self.post_mouse(CGEventType::LeftMouseDragged, start, CGMouseButton::Left, 0, 0)?;
                if let Some(drag) = &mut self.drag {
                    let now = drag.us(Instant::now());
                    drag.nudge = Some(Nudge::new((start.x, start.y), now, drag.rest_us));
                }
                Ok(())
            })
            .and_then(|()| self.press_number().ok_or(InputError::Create));
        if pressed.is_err() {
            self.leave_drag();
        }
        pressed
    }

    /// When the pressed drag, resting where it is, is next due a nudge ([`Self::nudge`]);
    /// `None` outside a pressed drag.
    #[must_use]
    pub fn next_nudge(&self) -> Option<Instant> {
        let drag = self.drag.as_ref()?;
        let nudge = drag.nudge.as_ref().filter(|_| self.held & MouseButton::Left.bit() != 0)?;
        drag.epoch.checked_add(Duration::from_micros(nudge.due_us()))
    }

    /// Keep a drag resting in one place moving, as a spring-loaded target needs
    /// ([`nudge`]): once it has rested [`nudge::rest_us`] since it was last posted, a drag
    /// [`nudge::STEP`] points to the right of where it rests, and a rest later one back there.
    /// A drag that moves on its own is left alone, and the release lands where the drag rests,
    /// never on the point off it. Whether a nudge went at `now`.
    ///
    /// # Errors
    ///
    /// The system would not take the event.
    pub fn nudge(&mut self, now: Instant) -> Result<bool, InputError> {
        if self.held & MouseButton::Left.bit() == 0 {
            return Ok(false);
        }
        let Some(drag) = &mut self.drag else { return Ok(false) };
        let now = drag.us(now);
        let Some((x, y)) = drag.nudge.as_mut().and_then(|nudge| nudge.nudge(now)) else {
            return Ok(false);
        };
        let to = self.within(CGPoint { x, y });
        self.post_mouse(CGEventType::LeftMouseDragged, to, CGMouseButton::Left, 0, 0)?;
        Ok(true)
    }

    /// End the drag with nothing dropped: Escape through the HID tap, which ends the session
    /// before the release (P0 (3)), then [`Self::leave_drag`], whose release drops nothing. The
    /// Escape carries no modifier but Caps Lock's: with ⌘ or ⌥⌘ still held from the drag it
    /// would be a system shortcut (⌥⌘⎋ opens Force Quit).
    ///
    /// # Errors
    ///
    /// The system would not take the Escape; the drag is left all the same.
    pub fn cancel_drag(&mut self) -> Result<(), InputError> {
        if self.drag.is_none() {
            return Ok(());
        }
        let escaped = self.escape();
        self.leave_drag();
        escaped
    }

    /// End a drag with nothing dropped: one fed through the HID tap as [`Self::cancel_drag`]
    /// does, or one the client's left press holds on a display stream, whose Escape and release
    /// go through the HID tap as all its input does. Nothing held posts nothing.
    fn cancel_held(&mut self) -> Result<(), InputError> {
        if self.drag.is_some() || self.held & MouseButton::Left.bit() == 0 {
            return self.cancel_drag();
        }
        let escaped = self.escape();
        self.release_buttons();
        escaped
    }

    /// Escape through the HID tap, carrying no modifier but Caps Lock's: with ⌘ or ⌥⌘ still
    /// held it would be a system shortcut (⌥⌘⎋ opens Force Quit).
    fn escape(&mut self) -> Result<(), InputError> {
        let flags = self.flags & CGEventFlags::MaskAlphaShift;
        let vk = keymap::virtual_key(KeyCode::Escape).ok_or(InputError::Create);
        vk.and_then(|vk| {
            [true, false].into_iter().try_for_each(|down| {
                let event = Event::Key { vk, down, modifier: false, repeat: false };
                self.backend.post(Post { route: Route::Hid, flags, event })
            })
        })
    }

    /// Stop feeding the drag: a button still held is let go where the drag rests, through the
    /// HID tap as it went down (the drop, when nothing ended the session before), then the
    /// stream's own route is back and a window stream's real pointer goes back where it was at
    /// [`Self::enter_drag`]. Nothing to do outside a drag.
    pub fn leave_drag(&mut self) {
        if self.drag.is_none() {
            return;
        }
        self.release_buttons();
        let Some(drag) = self.drag.take() else { return };
        if let Some(home) = drag.home {
            let moved = Event::Mouse {
                kind: CGEventType::MouseMoved,
                at: home,
                button: CGMouseButton::Left,
                number: 0,
                clicks: 0,
                press: 0,
            };
            if let Err(e) = self.post(Some(Route::Hid), moved) {
                tracing::debug!(target = ?self.target, error = %e, "pointer back after a drag");
            }
        }
        if self.route != Route::Hid
            && let Some(at) = self.at
        {
            self.placed.place(at.x, at.y);
        }
    }

    /// `point` held inside the target's bounds, as [`to_point`] holds a stream pixel.
    fn within(&self, point: CGPoint) -> CGPoint {
        self.bounds.map_or(point, |b| CGPoint {
            x: point.x.clamp(b.x, b.x + b.w),
            y: point.y.clamp(b.y, b.y + b.h),
        })
    }

    fn release_key(&mut self, code: KeyCode) {
        if let Err(e) = self.post_key(code, KeyAction::Release) {
            tracing::debug!(target = ?self.target, ?code, error = %e, "release");
        }
    }

    /// The event clock's stamp for a gesture's event the client stamped `client_us`
    /// ([`Timeline`]); a gesture opens on its first phase.
    fn stamp(&mut self, client_us: u32, phase: ScrollPhase) -> u64 {
        let now = self.backend.event_clock();
        let begins = matches!(phase, ScrollPhase::Began | ScrollPhase::MayBegin);
        self.timeline.place(client_us, now, begins)
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
            let _gone = self.backend.activate(pid, self.window());
        }
        self.active_at = Some(Instant::now());
    }

    /// The streamed window's `CGWindowID`; `None` for a display.
    const fn window(&self) -> Option<u32> {
        match self.target {
            CaptureTarget::Window(window) => Some(window.0),
            CaptureTarget::Display(_) => None,
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
        if matches!(self.route, Route::Pid(_)) && self.drag.is_none() {
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

    /// The held button a move drags with, as [`Self::move_type`] picks it.
    fn held_button(&self) -> Option<MouseButton> {
        BUTTONS.into_iter().find(|b| self.held & b.bit() != 0)
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
        let press = self.press(kind, number);
        self.post(route, Event::Mouse { kind, at, button, number, clicks, press })
    }

    /// The press number an event of `kind` for button `number` carries: a new one for a press,
    /// the press's own for its drags and its release, none for a move.
    fn press(&mut self, kind: CGEventType, number: i64) -> i64 {
        let Some(held) = usize::try_from(number).ok().and_then(|n| self.presses.get_mut(n)) else {
            return 0;
        };
        match kind {
            CGEventType::LeftMouseDown
            | CGEventType::RightMouseDown
            | CGEventType::OtherMouseDown => {
                *held = PRESSES.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
                *held
            }
            CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp => {
                std::mem::take(held)
            }
            CGEventType::LeftMouseDragged
            | CGEventType::RightMouseDragged
            | CGEventType::OtherMouseDragged => *held,
            _ => 0,
        }
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
        let route = route.unwrap_or_else(|| self.bound(&event));
        self.backend.post(Post { route, flags: self.flags, event })
    }

    /// The stream's route for `event`: a window stream's pointer events go bound to the
    /// window where it is now, so they reach its view (`backend::window_binding`); everything
    /// else, and a window whose bounds are unknown, goes to the pid alone. During a drag every
    /// pointer event goes through the HID tap ([`Self::enter_drag`]).
    const fn bound(&self, event: &Event) -> Route {
        let pointer =
            matches!(event, Event::Mouse { .. } | Event::Scroll { .. } | Event::Gesture { .. });
        if pointer && self.drag.is_some() {
            return Route::Hid;
        }
        match (self.route, self.target, self.bounds) {
            (Route::Pid(pid), CaptureTarget::Window(window), Some(rect)) if pointer => {
                Route::Window { pid, window: window.0, origin: CGPoint { x: rect.x, y: rect.y } }
            }
            (route, ..) => route,
        }
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
    use slopty_proto::screen::SwipeDirection;

    use super::*;
    use crate::backend::Recorder;

    const BOUNDS: Rect = Rect { x: 100.0, y: 50.0, w: 800.0, h: 600.0 };
    const PID: i32 = 4242;
    /// The route of `window()`'s pointer events: to its owner, bound to it where it is.
    const BOUND: Route =
        Route::Window { pid: PID, window: 9, origin: CGPoint { x: 100.0, y: 50.0 } };

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

    /// A window stream's pointer events go bound to its window, where it is, so they reach the
    /// view under them; its keys go to the owner alone, to the key window as a keyboard's do.
    #[test]
    fn a_window_stream_s_pointer_is_bound_to_its_window_and_its_keys_are_not() {
        let mut inj = window();
        inj.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        inj.inject(&ScreenInput::Scroll {
            dx: 0.0,
            dy: 1.0,
            precise: true,
            phase: ScrollPhase::Began,
            momentum: ScrollPhase::None,
            x: 20.0,
            y: 40.0,
            mods: Mods::empty(),
            time_us: 0,
        })
        .unwrap();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::A,
            action: KeyAction::Press,
            mods: Mods::empty(),
        })
        .unwrap();
        let routes: Vec<Route> = inj.backend().posts.iter().map(|p| p.route).collect();
        assert_eq!(routes, [BOUND, BOUND, Route::Pid(PID)]);

        // It moved: the next event is bound where it is now.
        let moved = Rect { x: 300.0, ..BOUNDS };
        inj.set_bounds(Some(moved), Instant::now());
        inj.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        let origin = CGPoint { x: 300.0, y: 50.0 };
        let last = inj.backend().posts.last().map(|p| p.route);
        assert_eq!(last, Some(Route::Window { pid: PID, window: 9, origin }));
    }

    #[test]
    fn moves_map_through_bounds_and_scale() {
        let mut inj = window();
        inj.inject(&ScreenInput::Move { x: 20.0, y: 40.0 }).unwrap();
        let posts = &inj.backend().posts;
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].route, BOUND);
        assert_eq!(
            posts[0].event,
            Event::Mouse {
                kind: CGEventType::MouseMoved,
                at: pt(110.0, 70.0),
                button: CGMouseButton::Left,
                number: 0,
                clicks: 0,
                press: 0,
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
        assert!(rec.posts.iter().all(|p| p.route == BOUND));
    }

    /// A press, each of its drags and its release carry one event number, which AppKit follows
    /// the drag by; the next press takes a new one. A drag with another button held carries that
    /// button and its press, and a move with nothing held carries none.
    #[test]
    fn a_press_its_drags_and_its_release_share_one_number() {
        let mut inj = window();
        let button = |button, down| ScreenInput::Button {
            button,
            down,
            clicks: 1,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
        };
        let to = |x| ScreenInput::Move { x, y: 0.0 };
        for input in [
            to(1.0),
            button(MouseButton::Left, true),
            to(2.0),
            to(3.0),
            button(MouseButton::Left, false),
            button(MouseButton::Left, true),
            button(MouseButton::Left, false),
            button(MouseButton::Middle, true),
            to(4.0),
            button(MouseButton::Middle, false),
        ] {
            inj.inject(&input).unwrap();
        }
        let seen: Vec<(CGEventType, i64, i64)> = inj
            .backend()
            .events()
            .into_iter()
            .map(|e| match e {
                Event::Mouse { kind, number, press, .. } => (*kind, *number, *press),
                other => panic!("{other:?}"),
            })
            .collect();
        let first = seen[1].2;
        let (second, third) = (seen[5].2, seen[7].2);
        assert!(first > 0 && second > first && third > second, "{seen:?}");
        assert_eq!(
            seen,
            [
                (CGEventType::MouseMoved, 0, 0),
                (CGEventType::LeftMouseDown, 0, first),
                (CGEventType::LeftMouseDragged, 0, first),
                (CGEventType::LeftMouseDragged, 0, first),
                (CGEventType::LeftMouseUp, 0, first),
                (CGEventType::LeftMouseDown, 0, second),
                (CGEventType::LeftMouseUp, 0, second),
                (CGEventType::OtherMouseDown, 2, third),
                (CGEventType::OtherMouseDragged, 2, third),
                (CGEventType::OtherMouseUp, 2, third),
            ]
        );
    }

    /// The tile turning its gestures on or off halfway through a trackpad scroll leaves the
    /// scroll as it began, paired or not, to its end: the next one takes the new word.
    #[test]
    fn a_scroll_gesture_keeps_its_pairing_to_its_end() {
        let mut inj = window();
        let scroll = |phase| ScreenInput::Scroll {
            dx: 0.0,
            dy: 3.0,
            precise: true,
            phase,
            momentum: ScrollPhase::None,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
            time_us: 0,
        };
        let turn = |remote| ScreenInput::Gestures { remote };
        for input in [
            turn(true),
            scroll(ScrollPhase::MayBegin),
            scroll(ScrollPhase::Began),
            turn(false),
            scroll(ScrollPhase::Changed),
            scroll(ScrollPhase::Ended),
            scroll(ScrollPhase::Began),
            turn(true),
            scroll(ScrollPhase::Changed),
            scroll(ScrollPhase::Cancelled),
            scroll(ScrollPhase::Began),
        ] {
            inj.inject(&input).unwrap();
        }
        let gestures: Vec<ScrollPhase> = inj
            .backend()
            .events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Gesture { gesture: Gesture::Scroll { .. }, phase, .. } => Some(*phase),
                _ => None,
            })
            .collect();
        assert_eq!(
            gestures,
            [
                ScrollPhase::MayBegin,
                ScrollPhase::Began,
                ScrollPhase::Changed,
                ScrollPhase::Ended,
                ScrollPhase::Began,
            ],
            "the first scroll paired to its end, the second not at all, the third paired"
        );
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

        fn activate(&mut self, pid: i32, window: Option<u32>) -> Result<(), InputError> {
            self.0.activate(pid, window)
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
            time_us: 0,
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
                stamp: 0,
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
        let pressed = |kind| {
            inj.backend().events().into_iter().find_map(|e| match e {
                Event::Mouse { kind: k, press, .. } if *k == kind => Some(*press),
                _ => None,
            })
        };
        let (left, middle) =
            (pressed(CGEventType::LeftMouseDown), pressed(CGEventType::OtherMouseDown));
        let (left, middle) = (left.unwrap(), middle.unwrap());
        inj.release_all();

        let ups: Vec<(Event, CGEventFlags)> =
            inj.backend().posts[before..].iter().map(|p| (p.event.clone(), p.flags)).collect();
        let vk = |code| keymap::virtual_key(code).unwrap();
        let mouse_up = |kind, button, number, press| Event::Mouse {
            kind,
            at: pt(108.0, 59.0),
            button,
            number,
            clicks: 1,
            press,
        };
        let key_up =
            |code, modifier| Event::Key { vk: vk(code), down: false, modifier, repeat: false };
        let both = flags_for(Mods::SUPER | Mods::SHIFT);
        assert!(both.contains(CGEventFlags::MaskCommand | CGEventFlags::MaskShift));
        assert_eq!(
            ups,
            [
                (mouse_up(CGEventType::LeftMouseUp, CGMouseButton::Left, 0, left), both),
                (mouse_up(CGEventType::OtherMouseUp, CGMouseButton::Center, 2, middle), both),
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

            fn activate(&mut self, pid: i32, window: Option<u32>) -> Result<(), InputError> {
                self.0.activate(pid, window)
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

    /// The stamps of the scrolls and gestures posted, in ms.
    fn stamps_ms(rec: &Recorder) -> Vec<f64> {
        #[expect(clippy::cast_precision_loss, reason = "test stamps are small")]
        rec.posts
            .iter()
            .filter_map(|p| match p.event {
                Event::Scroll { stamp, .. } | Event::Gesture { stamp, .. } => {
                    Some(stamp as f64 / 1e6)
                }
                _ => None,
            })
            .collect()
    }

    /// A trackpad scroll's reports, 8.3 ms apart on the client, reach the worker 20 ms later,
    /// one of them held up 40 ms more and the next right behind it: each is stamped at the
    /// client's spacing and the path's least delay, the held-up one included, so the app
    /// reads the fingers' pace and not the path's. No stamp is later than its event is handled,
    /// and none goes back.
    #[test]
    fn a_gesture_held_up_on_the_way_keeps_the_client_s_spacing() {
        let mut inj = display();
        inj.inject(&ScreenInput::Gestures { remote: true }).unwrap();
        let tick = 8_333_u32;
        let base_ns = 5_000_000_000_u64;
        // (client µs, handled here in ns)
        let arrivals: Vec<(u32, u64)> = (0..8_u32)
            .map(|k| {
                let client = k * tick;
                let late = match k {
                    3 => 40_000_000,
                    4 => 40_000_000 - 8_333_000 + 100_000,
                    _ => 0,
                };
                (client, base_ns + u64::from(client) * 1000 + 20_000_000 + late)
            })
            .collect();
        for (k, (client, handled)) in arrivals.iter().enumerate() {
            inj.backend.clock_ns = *handled;
            let phase = if k == 0 { ScrollPhase::Began } else { ScrollPhase::Changed };
            inj.inject(&ScreenInput::Scroll {
                dx: -10.0,
                dy: 0.0,
                precise: true,
                phase,
                momentum: ScrollPhase::None,
                x: 0.0,
                y: 0.0,
                mods: Mods::empty(),
                time_us: *client,
            })
            .unwrap();
        }
        let stamps = stamps_ms(inj.backend());
        assert_eq!(stamps.len(), 16, "each scroll and its gesture");
        let scrolls: Vec<f64> = stamps.iter().step_by(2).copied().collect();
        assert!(
            stamps.chunks(2).all(|pair| (pair[0] - pair[1]).abs() < 1e-9),
            "a scroll and its gesture share one"
        );
        let first = scrolls[0];
        for (k, stamp) in scrolls.iter().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "a small count")]
            let want = 8.333_f64.mul_add(k as f64, first);
            assert!((stamp - want).abs() < 1e-6, "report {k}: {stamp} for {want}: {scrolls:?}");
            #[expect(clippy::cast_precision_loss, reason = "test stamps are small")]
            let handled = arrivals[k].1 as f64 / 1e6;
            assert!(*stamp <= handled, "report {k} stamped after it was handled");
        }
    }

    /// A new gesture starts its timeline again, and so does one whose client stamp jumps by more
    /// than a second or back; its coast carries on the gesture's; a wheel's lines are left to
    /// the system's stamp; the client's 32-bit stamp wraps without a step.
    #[test]
    fn a_gesture_s_timeline_starts_again_and_its_coast_carries_on() {
        let mut inj = display();
        let scroll =
            |inj: &mut Injector<Recorder>, at_ms: u64, time_us: u32, phase, momentum, precise| {
                inj.backend.clock_ns = at_ms * 1_000_000;
                inj.inject(&ScreenInput::Scroll {
                    dx: 1.0,
                    dy: 0.0,
                    precise,
                    phase,
                    momentum,
                    x: 0.0,
                    y: 0.0,
                    mods: Mods::empty(),
                    time_us,
                })
                .unwrap();
            };
        let (on, none) = (ScrollPhase::Changed, ScrollPhase::None);
        let wrap = u32::MAX - 4_000;
        // Began near the wrap, a report 10 ms on (past it) arriving 30 ms late.
        scroll(&mut inj, 1_000, wrap, ScrollPhase::Began, none, true);
        scroll(&mut inj, 1_040, wrap.wrapping_add(10_000), on, none, true);
        // Its coast, 5 ms later on the client.
        scroll(&mut inj, 1_046, wrap.wrapping_add(15_000), none, ScrollPhase::Began, true);
        // A wheel notch.
        scroll(&mut inj, 1_050, 0, none, none, false);
        // A new gesture: its own start, whatever the old one's lag.
        scroll(&mut inj, 2_000, 7, ScrollPhase::Began, none, true);
        // A step of two seconds on the client: not the same fingers.
        scroll(&mut inj, 4_000, 2_000_007, on, none, true);
        assert_eq!(stamps_ms(inj.backend()), [1_000.0, 1_010.0, 1_015.0, 0.0, 2_000.0, 4_000.0]);
    }

    /// Whatever stamps a client sends (they are the peer's to choose: wrapped, stuck, jumping
    /// back or by hours, a gesture opened at any point) and however late they come, no stamp
    /// is ever later than the moment its event is handled, and none goes back.
    #[test]
    fn a_hostile_client_s_stamps_never_go_back_or_ahead() {
        let mut timeline = Timeline::default();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut now, mut client, mut given) = (0_u64, 0_u32, 0_u64);
        for _ in 0..100_000 {
            let r = next();
            now = now.saturating_add(r % 20_000_000);
            client = match r % 7 {
                0 => client,
                1 => client.wrapping_sub(u32::try_from(r >> 40).unwrap_or(0)),
                2 => u32::try_from(r >> 32).unwrap_or(0),
                _ => client.wrapping_add(u32::try_from(r % 20_000).unwrap_or(0)),
            };
            let stamp = timeline.place(client, now, r % 11 == 0);
            assert!(stamp <= now && stamp >= given, "{stamp} after {given}, now {now}");
            given = stamp;
        }
        let end = timeline.place(0, u64::MAX, true);
        assert!(end >= given, "the clock's end holds: {end}");
    }

    /// The mouse events posted, as (kind, point, press number, route).
    fn mice(rec: &Recorder) -> Vec<(CGEventType, CGPoint, i64, Route)> {
        rec.posts
            .iter()
            .filter_map(|p| match p.event {
                Event::Mouse { kind, at, press, .. } => Some((kind, at, press, p.route)),
                _ => None,
            })
            .collect()
    }

    /// A drag on a window stream: entering raises the owner and keeps where the real pointer
    /// was; the press at the source and the first drag 2 points on go through the HID tap under
    /// one new number, and so do the client's moves and the drop; leaving puts the real pointer
    /// back and the next pointer event goes to the window again.
    #[test]
    fn a_drag_goes_through_the_hid_tap_under_one_number_and_the_pointer_goes_back() {
        let mut inj = window();
        inj.backend.pointer = Some(pt(40.0, 30.0));
        inj.enter_drag();
        assert!(inj.dragging());
        assert_eq!(inj.backend().activations, [PID], "raised: the HID tap hits what is on top");
        assert_eq!(inj.press_number(), None);
        let number = inj.press_at(20.0, 40.0).unwrap();
        assert_eq!(inj.press_number(), Some(number));
        inj.inject(&ScreenInput::Move { x: 60.0, y: 40.0 }).unwrap();
        inj.inject(&ScreenInput::Button {
            button: MouseButton::Left,
            down: false,
            clicks: 1,
            x: 60.0,
            y: 40.0,
            mods: Mods::empty(),
        })
        .unwrap();
        assert_eq!(inj.press_number(), None, "let go");
        inj.leave_drag();
        assert!(!inj.dragging());
        inj.inject(&ScreenInput::Move { x: 0.0, y: 0.0 }).unwrap();
        let hid = Route::Hid;
        assert_eq!(
            mice(inj.backend()),
            [
                (CGEventType::LeftMouseDown, pt(110.0, 70.0), number, hid),
                (CGEventType::LeftMouseDragged, pt(112.0, 70.0), number, hid),
                (CGEventType::LeftMouseDragged, pt(130.0, 70.0), number, hid),
                (CGEventType::LeftMouseUp, pt(130.0, 70.0), number, hid),
                (CGEventType::MouseMoved, pt(40.0, 30.0), 0, hid),
                (CGEventType::MouseMoved, pt(100.0, 50.0), 0, BOUND),
            ],
            "the real pointer back where it was, then the window's route again"
        );
        assert_eq!(inj.placed.get(), crate::Pointer::Placed(Some((100.0, 50.0))));
    }

    /// A left press on a window stream whose window is on top at the point goes through the HID
    /// tap with its drags and its release, under one number, so an app there can begin a drag
    /// from it; the release puts the real pointer back and the window's own route is back. A
    /// covered window keeps its route for the press, and so does any other button.
    #[test]
    fn a_left_press_on_an_uncovered_window_goes_through_the_hid_tap() {
        let button = |button, down| ScreenInput::Button {
            button,
            down,
            clicks: 1,
            x: 20.0,
            y: 40.0,
            mods: Mods::empty(),
        };
        let mut inj = window();
        inj.backend.pointer = Some(pt(40.0, 30.0));
        inj.backend.uncovered = true;
        inj.inject(&button(MouseButton::Left, true)).unwrap();
        assert!(inj.dragging(), "fed through the HID tap while held");
        let number = inj.press_number().expect("held");
        inj.inject(&ScreenInput::Move { x: 60.0, y: 40.0 }).unwrap();
        inj.inject(&button(MouseButton::Left, false)).unwrap();
        assert!(!inj.dragging(), "the release ends it");
        inj.inject(&ScreenInput::Move { x: 0.0, y: 0.0 }).unwrap();
        inj.inject(&button(MouseButton::Middle, true)).unwrap();
        inj.inject(&button(MouseButton::Middle, false)).unwrap();
        let hid = Route::Hid;
        let seen = mice(inj.backend());
        assert_eq!(
            seen[..5],
            [
                (CGEventType::LeftMouseDown, pt(110.0, 70.0), number, hid),
                (CGEventType::LeftMouseDragged, pt(130.0, 70.0), number, hid),
                (CGEventType::LeftMouseUp, pt(110.0, 70.0), number, hid),
                (CGEventType::MouseMoved, pt(40.0, 30.0), 0, hid),
                (CGEventType::MouseMoved, pt(100.0, 50.0), 0, BOUND),
            ],
            "the real pointer back where it was, then the window's route again"
        );
        assert!(seen[5..].iter().all(|m| m.3 == BOUND), "another button keeps the route");

        let mut covered = window();
        covered.backend.pointer = Some(pt(40.0, 30.0));
        covered.inject(&button(MouseButton::Left, true)).unwrap();
        assert!(!covered.dragging(), "something over the window there");
        covered.inject(&button(MouseButton::Left, false)).unwrap();
        assert!(mice(covered.backend()).iter().all(|m| m.3 == BOUND));
    }

    /// A display stream's drag moves the real pointer as all its input does, so nothing is
    /// raised and the pointer is left where the drag ended.
    #[test]
    fn a_display_stream_s_drag_leaves_the_pointer_where_it_ended() {
        let mut inj = display();
        inj.backend.pointer = Some(pt(0.0, 0.0));
        let number = inj.press_at(10.0, 10.0).unwrap();
        assert!(inj.dragging(), "a press at the source enters drag mode");
        inj.leave_drag();
        assert!(inj.backend().activations.is_empty());
        assert_eq!(
            mice(inj.backend()),
            [
                (CGEventType::LeftMouseDown, pt(110.0, 60.0), number, Route::Hid),
                (CGEventType::LeftMouseDragged, pt(112.0, 60.0), number, Route::Hid),
                (CGEventType::LeftMouseUp, pt(112.0, 60.0), number, Route::Hid),
            ],
            "leaving with the press held drops it where the drag rests; no move after"
        );
        assert_eq!(inj.placed.get(), crate::Pointer::Real);
    }

    /// A press at the target's right edge still drags 2 points, inward: the drag stays on the
    /// target. Two drags take two numbers.
    #[test]
    fn the_first_drag_stays_on_the_target_and_each_press_is_new() {
        let mut inj = display();
        let first = inj.press_at(800.0, 0.0).unwrap();
        let started = mice(inj.backend())[1].1;
        assert_eq!(started, pt(900.0, 50.0), "held at the right edge");
        inj.leave_drag();
        let second = inj.press_at(0.0, 0.0).unwrap();
        assert!(second > first, "{first} then {second}");
    }

    /// A drag resting still is nudged two points out and back, a rest (the spring delay and its
    /// margin) after it was last posted, a nudge included; a real move starts the rest again
    /// from where it went; the drop lands where the drag rests, not two points off it. Outside a
    /// drag, or with the button up, there is no nudge to wait for and none is posted.
    #[test]
    fn a_resting_drag_is_nudged_two_points_out_and_back() {
        let rest = Duration::from_micros(nudge::rest_us(nudge::DEFAULT_SPRING_DELAY_S));
        let mut inj = display();
        assert_eq!(inj.next_nudge(), None, "not in a drag");
        assert!(!inj.nudge(Instant::now()).unwrap());
        let number = inj.press_at(10.0, 10.0).unwrap();
        let due = inj.next_nudge().expect("a pressed drag rests");
        let early = due.checked_sub(Duration::from_millis(1)).unwrap();
        assert!(!inj.nudge(early).unwrap(), "not rested yet");
        assert!(inj.nudge(due).unwrap(), "out");
        let back = inj.next_nudge().unwrap();
        assert_eq!(back.saturating_duration_since(due), rest, "a whole rest after the nudge");
        assert!(inj.nudge(back).unwrap(), "and back");
        // The drag's clock counts whole microseconds, so the rest may start up to one before.
        let before = Instant::now().checked_sub(Duration::from_micros(1)).unwrap();
        inj.inject(&ScreenInput::Move { x: 20.0, y: 10.0 }).unwrap();
        let moved = inj.next_nudge().unwrap();
        let after = Instant::now().checked_add(rest).unwrap();
        assert!(
            before.checked_add(rest).unwrap() <= moved && moved <= after,
            "the move started the rest again, from when it went"
        );
        assert!(!inj.nudge(Instant::now()).unwrap());
        assert!(inj.nudge(moved).unwrap(), "out from where the move left it");
        inj.inject(&ScreenInput::Button {
            button: MouseButton::Left,
            down: false,
            clicks: 1,
            x: 20.0,
            y: 10.0,
            mods: Mods::empty(),
        })
        .unwrap();
        assert_eq!(inj.next_nudge(), None, "let go");
        assert!(!inj.nudge(moved.checked_add(rest).unwrap()).unwrap());
        let drags: Vec<(CGEventType, CGPoint, i64)> =
            mice(inj.backend()).into_iter().skip(2).map(|m| (m.0, m.1, m.2)).collect();
        let dragged = CGEventType::LeftMouseDragged;
        assert_eq!(
            drags,
            [
                (dragged, pt(114.0, 60.0), number),
                (dragged, pt(112.0, 60.0), number),
                (dragged, pt(120.0, 60.0), number),
                (dragged, pt(122.0, 60.0), number),
                (CGEventType::LeftMouseUp, pt(120.0, 60.0), number),
            ]
        );
    }

    /// A drag carried as steps: entering answers the point in global points with the window
    /// raised, the press and its drags share one number through the HID tap, a step's move is a
    /// drag, and the release puts the stream's own route back. A move before any drag posts
    /// nothing; an entry at a window that is gone answers so and leaves no drag.
    #[test]
    fn a_drag_s_steps_enter_press_carry_and_let_go() {
        let mut inj = window();
        inj.drag_step(DragStep::Move { x: 1.0, y: 1.0 });
        assert!(inj.backend().posts.is_empty(), "no drag, nothing carried");
        let (answer, answered) = tokio::sync::oneshot::channel();
        inj.drag_step(DragStep::Enter { x: 20.0, y: 40.0, answer });
        assert_eq!(answered.blocking_recv().unwrap().unwrap(), (110.0, 70.0));
        assert_eq!(inj.backend().activations, [PID]);
        inj.drag_step(DragStep::Press { x: 20.0, y: 40.0 });
        inj.drag_step(DragStep::Move { x: 60.0, y: 40.0 });
        let number = inj.press_number().unwrap();
        inj.drag_step(DragStep::Release);
        assert!(!inj.dragging());
        let hid = Route::Hid;
        assert_eq!(
            mice(inj.backend()),
            [
                (CGEventType::LeftMouseDown, pt(110.0, 70.0), number, hid),
                (CGEventType::LeftMouseDragged, pt(112.0, 70.0), number, hid),
                (CGEventType::LeftMouseDragged, pt(130.0, 70.0), number, hid),
                (CGEventType::LeftMouseUp, pt(130.0, 70.0), number, hid),
            ]
        );
        let mut gone = Injector::with_backend(
            CaptureTarget::Window(WindowId(9)),
            1.0,
            Recorder { owner: Some(PID), ..Recorder::default() },
        );
        let (answer, answered) = tokio::sync::oneshot::channel();
        gone.drag_step(DragStep::Enter { x: 0.0, y: 0.0, answer });
        assert!(matches!(answered.blocking_recv().unwrap(), Err(InputError::NoBounds)));
        assert!(!gone.dragging());
    }

    /// Cancelling a drag posts Escape through the HID tap, press and release, before letting
    /// go of the button, so the session ends before the release drops anything; then the drag
    /// is left. Cancelling outside a drag posts nothing.
    /// A drag out of an app that the client's left press holds on a display stream ends with
    /// Escape then the release, through the HID tap; with nothing held a cancel posts nothing.
    /// Locating a point answers it in global points and posts nothing.
    #[test]
    fn a_held_press_on_a_display_cancels_and_a_point_is_located() {
        let mut inj = display();
        inj.drag_step(DragStep::Cancel);
        assert!(inj.backend().posts.is_empty());
        inj.inject(&ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            clicks: 1,
            x: 5.0,
            y: 5.0,
            mods: Mods::empty(),
        })
        .unwrap();
        let (answer, located) = tokio::sync::oneshot::channel();
        inj.drag_step(DragStep::Locate { x: 10.0, y: 20.0, answer });
        assert_eq!(located.blocking_recv().unwrap().unwrap(), (110.0, 70.0));
        assert_eq!(inj.backend().posts.len(), 1, "located with nothing posted");
        inj.drag_step(DragStep::Cancel);
        let escape = keymap::virtual_key(KeyCode::Escape).unwrap();
        let key = |down| Event::Key { vk: escape, down, modifier: false, repeat: false };
        let after: Vec<(Route, Event)> =
            inj.backend().posts[1..].iter().map(|p| (p.route, p.event.clone())).collect();
        assert_eq!(after[..2], [(Route::Hid, key(true)), (Route::Hid, key(false))]);
        assert!(
            matches!(after[2], (Route::Hid, Event::Mouse { kind: CGEventType::LeftMouseUp, .. })),
            "{after:?}"
        );
        assert_eq!(inj.press_number(), None, "let go");
    }

    #[test]
    fn cancelling_a_drag_escapes_before_the_release() {
        let mut inj = window();
        inj.cancel_drag().unwrap();
        assert!(inj.backend().posts.is_empty());
        let number = inj.press_at(0.0, 0.0).unwrap();
        let before = inj.backend().posts.len();
        inj.cancel_drag().unwrap();
        assert!(!inj.dragging());
        let escape = keymap::virtual_key(KeyCode::Escape).unwrap();
        let after: Vec<(Route, Event)> =
            inj.backend().posts[before..].iter().map(|p| (p.route, p.event.clone())).collect();
        let key = |down| Event::Key { vk: escape, down, modifier: false, repeat: false };
        assert_eq!(after[..2], [(Route::Hid, key(true)), (Route::Hid, key(false))]);
        assert!(
            matches!(after[2], (Route::Hid, Event::Mouse { kind: CGEventType::LeftMouseUp, press, .. }) if press == number),
            "{after:?}"
        );
        assert_eq!(after.len(), 3, "no pointer to put back: the recorder keeps none");
    }

    /// A stream ending mid-drag cancels it rather than dropping what it carries: Escape through
    /// the HID tap, then the release where the press went, then the real pointer back.
    #[test]
    fn a_stream_ending_mid_drag_cancels_it() {
        let mut inj = window();
        inj.backend.pointer = Some(pt(5.0, 5.0));
        let number = inj.press_at(0.0, 0.0).unwrap();
        let before = inj.backend().posts.len();
        inj.release_all();
        assert!(!inj.dragging());
        let tail: Vec<String> = inj.backend().posts[before..]
            .iter()
            .map(|p| match &p.event {
                Event::Key { down, .. } => format!("escape {down} {:?}", p.route),
                Event::Mouse { kind, press, .. } => format!("{kind:?} {press} {:?}", p.route),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            tail,
            [
                "escape true Hid".to_owned(),
                "escape false Hid".to_owned(),
                format!("{:?} {number} Hid", CGEventType::LeftMouseUp),
                format!("{:?} 0 Hid", CGEventType::MouseMoved),
            ]
        );
    }

    /// The Escape that cancels a drag carries no modifier but Caps Lock's, whatever the drag
    /// was made with: ⌥⌘⎋ is Force Quit.
    #[test]
    fn a_drag_s_escape_carries_no_modifier_but_caps_lock() {
        let mut inj = display();
        inj.inject(&ScreenInput::Key {
            code: KeyCode::MetaLeft,
            action: KeyAction::Press,
            mods: Mods::SUPER | Mods::ALT | Mods::CAPS_LOCK,
        })
        .unwrap();
        inj.press_at(0.0, 0.0).unwrap();
        inj.cancel_drag().unwrap();
        let escapes: Vec<CGEventFlags> = inj
            .backend()
            .posts
            .iter()
            .filter(|p| matches!(p.event, Event::Key { modifier: false, .. }))
            .map(|p| p.flags)
            .collect();
        assert_eq!(escapes, [CGEventFlags::MaskAlphaShift; 2]);
    }

    /// A press at a window that is gone posts nothing and leaves no drag on: the stream's own
    /// route stays, and nothing was raised.
    #[test]
    fn a_press_at_a_window_gone_leaves_no_drag() {
        let mut inj = Injector::with_backend(
            CaptureTarget::Window(WindowId(9)),
            1.0,
            Recorder { owner: Some(PID), ..Recorder::default() },
        );
        assert!(matches!(inj.press_at(0.0, 0.0), Err(InputError::NoBounds)));
        assert!(!inj.dragging());
        assert!(inj.backend().posts.is_empty() && inj.backend().activations.is_empty());
    }

    /// A button the client holds when a drag begins is let go first on the route it went down
    /// on, so the app that took the press takes its release; a second press at the source lets
    /// go of the first before it.
    #[test]
    fn a_drag_lets_go_of_a_press_on_its_own_route_first() {
        let mut inj = window();
        inj.inject(&ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            clicks: 1,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
        })
        .unwrap();
        let held = inj.press_number().unwrap();
        let first = inj.press_at(10.0, 0.0).unwrap();
        let second = inj.press_at(20.0, 0.0).unwrap();
        let kinds: Vec<(CGEventType, i64, Route)> =
            mice(inj.backend()).into_iter().map(|m| (m.0, m.2, m.3)).collect();
        let (down, up, dragged) =
            (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, CGEventType::LeftMouseDragged);
        let hid = Route::Hid;
        assert_eq!(
            kinds,
            [
                (down, held, BOUND),
                (up, held, BOUND),
                (down, first, hid),
                (dragged, first, hid),
                (up, first, hid),
                (down, second, hid),
                (dragged, second, hid),
            ]
        );
    }

    /// A stream that ends mid-gesture cancels each gesture under way where the pointer last
    /// was: the scroll and the gesture it came with, a pinch, a rotation; a coast gets its end.
    /// Once closed, nothing more is posted.
    #[test]
    fn a_stream_ending_mid_gesture_cancels_it() {
        let mut inj = window();
        inj.inject(&ScreenInput::Gestures { remote: true }).unwrap();
        let scroll = |phase, momentum| ScreenInput::Scroll {
            dx: 1.0,
            dy: 0.0,
            precise: true,
            phase,
            momentum,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
            time_us: 0,
        };
        for input in [
            scroll(ScrollPhase::Began, ScrollPhase::None),
            scroll(ScrollPhase::Changed, ScrollPhase::None),
            ScreenInput::Magnify {
                delta: 0.1,
                phase: ScrollPhase::Began,
                x: 0.0,
                y: 0.0,
                time_us: 0,
            },
            ScreenInput::Rotate {
                degrees: 1.0,
                phase: ScrollPhase::Changed,
                x: 0.0,
                y: 0.0,
                time_us: 0,
            },
        ] {
            inj.inject(&input).unwrap();
        }
        let before = inj.backend().posts.len();
        inj.release_all();
        let at = pt(100.0, 50.0);
        let cancelled = ScrollPhase::Cancelled;
        let closing: Vec<&Event> = inj.backend().posts[before..].iter().map(|p| &p.event).collect();
        assert_eq!(
            closing,
            [
                &Event::Scroll {
                    at,
                    dx: 0.0,
                    dy: 0.0,
                    precise: true,
                    phase: cancelled,
                    momentum: ScrollPhase::None,
                    stamp: 0,
                },
                &Event::Gesture {
                    at,
                    gesture: Gesture::Scroll { dx: 0.0, dy: 0.0 },
                    phase: cancelled,
                    stamp: 0,
                },
                &Event::Gesture { at, gesture: Gesture::Magnify(0.0), phase: cancelled, stamp: 0 },
                &Event::Gesture { at, gesture: Gesture::Rotate(0.0), phase: cancelled, stamp: 0 },
            ]
        );
        let after = inj.backend().posts.len();
        inj.release_all();
        assert_eq!(inj.backend().posts.len(), after, "closed once");

        let mut coasting = display();
        coasting.inject(&scroll(ScrollPhase::None, ScrollPhase::Began)).unwrap();
        coasting.release_all();
        assert!(matches!(
            coasting.backend().posts.last().map(|p| &p.event),
            Some(Event::Scroll { momentum: ScrollPhase::Ended, phase: ScrollPhase::None, .. })
        ));
    }

    /// A scroll whose gesture never opened here (its start lost, or a new stream taking over
    /// mid-swipe after a display switch) goes on unpaired to its end, since a gesture must not
    /// end that never began; the next one to open takes the tile's word. A may-begin and the
    /// began after it are one gesture.
    #[test]
    fn a_scroll_that_never_opened_here_goes_unpaired() {
        let mut inj = display();
        inj.inject(&ScreenInput::Gestures { remote: true }).unwrap();
        let scroll = |phase| ScreenInput::Scroll {
            dx: 1.0,
            dy: 0.0,
            precise: true,
            phase,
            momentum: ScrollPhase::None,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
            time_us: 0,
        };
        for phase in [
            ScrollPhase::Changed,
            ScrollPhase::Ended,
            ScrollPhase::MayBegin,
            ScrollPhase::Began,
            ScrollPhase::Ended,
        ] {
            inj.inject(&scroll(phase)).unwrap();
        }
        let gestures: Vec<ScrollPhase> = inj
            .backend()
            .events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Gesture { phase, .. } => Some(*phase),
                _ => None,
            })
            .collect();
        assert_eq!(gestures, [ScrollPhase::MayBegin, ScrollPhase::Began, ScrollPhase::Ended]);
    }

    /// A display stream's gestures go through the HID tap, as all its input does.
    #[test]
    fn a_display_stream_s_gestures_go_through_the_hid_tap() {
        let mut inj = display();
        inj.inject(&ScreenInput::SmartMagnify { x: 0.0, y: 0.0 }).unwrap();
        inj.inject(&ScreenInput::Magnify {
            delta: 0.1,
            phase: ScrollPhase::Began,
            x: 0.0,
            y: 0.0,
            time_us: 0,
        })
        .unwrap();
        assert!(inj.backend().posts.iter().all(|p| p.route == Route::Hid));
        assert_eq!(inj.backend().posts.len(), 2);
    }

    /// A pinch, a rotation, smart zoom and a swipe each reach the target as the gesture the
    /// trackpad makes, at the point under the fingers and on the stream's route: a swipe is one
    /// event, its end with its direction.
    #[test]
    fn gestures_reach_the_target_as_the_trackpad_makes_them() {
        let mut inj = window();
        let at = pt(100.0, 50.0);
        inj.inject(&ScreenInput::Magnify {
            delta: 0.2,
            phase: ScrollPhase::Changed,
            x: 0.0,
            y: 0.0,
            time_us: 0,
        })
        .unwrap();
        inj.inject(&ScreenInput::Rotate {
            degrees: -7.5,
            phase: ScrollPhase::Began,
            x: 0.0,
            y: 0.0,
            time_us: 0,
        })
        .unwrap();
        inj.inject(&ScreenInput::SmartMagnify { x: 0.0, y: 0.0 }).unwrap();
        inj.inject(&ScreenInput::Swipe { direction: SwipeDirection::Left, x: 0.0, y: 0.0 })
            .unwrap();
        let posts = &inj.backend().posts;
        assert!(posts.iter().all(|p| p.route == BOUND), "{posts:?}");
        let events: Vec<&Event> = posts.iter().map(|p| &p.event).collect();
        let gesture = |gesture, phase| Event::Gesture { at, gesture, phase, stamp: 0 };
        assert_eq!(
            events,
            [
                &gesture(Gesture::Magnify(0.2), ScrollPhase::Changed),
                &gesture(Gesture::Rotate(-7.5), ScrollPhase::Began),
                &gesture(Gesture::SmartMagnify, ScrollPhase::None),
                &gesture(Gesture::Swipe(SwipeDirection::Left), ScrollPhase::Ended),
            ]
        );
        assert!(inj.backend().activations.is_empty(), "a gesture does not raise the app");
    }

    /// While the tile sends its gestures, a trackpad scroll in its gesture comes with the
    /// gesture the trackpad sends beside it, after it and in the same phase, so a swipe between
    /// pages can follow it; its coast (momentum, no gesture phase) and a mouse wheel's lines
    /// come alone. A stream starts without, and the tile turning them off stops them from the
    /// next gesture on; the one under way ends as it began.
    #[test]
    fn a_trackpad_scroll_comes_with_its_gesture_and_its_coast_alone() {
        let mut inj = display();
        let scroll = |phase, momentum, precise| ScreenInput::Scroll {
            dx: 12.0,
            dy: -1.0,
            precise,
            phase,
            momentum,
            x: 0.0,
            y: 0.0,
            mods: Mods::empty(),
            time_us: 0,
        };
        inj.inject(&scroll(ScrollPhase::Began, ScrollPhase::None, true)).unwrap();
        inj.inject(&scroll(ScrollPhase::Ended, ScrollPhase::None, true)).unwrap();
        inj.inject(&ScreenInput::Gestures { remote: true }).unwrap();
        inj.inject(&scroll(ScrollPhase::MayBegin, ScrollPhase::None, true)).unwrap();
        inj.inject(&scroll(ScrollPhase::Changed, ScrollPhase::None, true)).unwrap();
        inj.inject(&scroll(ScrollPhase::None, ScrollPhase::Changed, true)).unwrap();
        inj.inject(&scroll(ScrollPhase::None, ScrollPhase::None, false)).unwrap();
        inj.inject(&ScreenInput::Gestures { remote: false }).unwrap();
        inj.inject(&scroll(ScrollPhase::Ended, ScrollPhase::None, true)).unwrap();
        inj.inject(&scroll(ScrollPhase::Began, ScrollPhase::None, true)).unwrap();
        let kinds: Vec<String> = inj
            .backend()
            .posts
            .iter()
            .map(|p| match &p.event {
                Event::Scroll { phase, momentum, .. } => format!("scroll {phase:?}/{momentum:?}"),
                Event::Gesture { gesture: Gesture::Scroll { dx, dy }, phase, .. } => {
                    format!("gesture {phase:?} {dx} {dy}")
                }
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "scroll Began/None",
                "scroll Ended/None",
                "scroll MayBegin/None",
                "gesture MayBegin 12 -1",
                "scroll Changed/None",
                "gesture Changed 12 -1",
                "scroll None/Changed",
                "scroll None/None",
                "scroll Ended/None",
                "gesture Ended 12 -1",
                "scroll Began/None",
            ]
        );
    }
}
