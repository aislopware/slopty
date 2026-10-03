//! Fingers on a stream tile, read as a trackpad or as two-finger gestures.
//!
//! In trackpad mode one finger moves a pointer the way an iPad's trackpad does: relative to
//! where it was, faster the faster the finger moves. A tap clicks where the pointer is, a
//! finger held still and lifted right-clicks, and a tap followed at once by a drag drags with
//! the button held. Two fingers are a pinch or a drag, decided by whichever passes its
//! threshold first: in trackpad mode the drag scrolls the remote app, in direct mode the two
//! go together, zoom and pan at once, as a photo does. Two fingers tapped and lifted
//! right-click in either mode.
//!
//! Positions are the body's points, times are given, so all of it is tested without a window.

use std::time::{Duration, Instant};

/// Travel under which a finger that lifts has tapped, in points (gpui's own touch slop).
pub const TAP_SLOP: f32 = 8.0;
/// How long a still finger is held before its lift is a right click (gpui's long press).
pub const HOLD: Duration = Duration::from_millis(500);
/// How soon after a tap the next touch makes a double click, or a drag with the button held.
pub const MULTI_TAP: Duration = Duration::from_millis(400);
/// How long two fingers may rest and still be a tap.
pub const TWO_FINGER_TAP: Duration = Duration::from_millis(300);

/// Finger speed up to which the pointer moves point for point, in points a second: slow
/// motion is for aiming.
const PRECISE_SPEED: f32 = 150.0;
/// Finger speed at which the gain tops out, in points a second: a flick crosses the picture.
const FAST_SPEED: f32 = 1500.0;
/// The gain at and past [`FAST_SPEED`].
const MAX_GAIN: f32 = 3.0;
/// Scale change (as a fraction) that makes two fingers a pinch.
const PINCH_LOCK: f32 = 0.06;
/// Centroid travel that makes two fingers a drag, in points.
const DRAG_LOCK: f32 = 10.0;

/// How much further the pointer goes than the finger at `speed` points a second: 1 while
/// aiming, rising linearly to [`MAX_GAIN`] for a flick, as a pointer acceleration curve does.
#[must_use]
pub fn gain(speed: f32) -> f32 {
    let t = ((speed - PRECISE_SPEED) / (FAST_SPEED - PRECISE_SPEED)).clamp(0.0, 1.0);
    (MAX_GAIN - 1.0).mul_add(t, 1.0)
}

/// A finger's step `by` (points) over `dt`, accelerated. A step with no time behind it (two
/// reports in one frame) takes the gain of the step before it.
#[must_use]
pub fn accelerate(by: (f32, f32), dt: Duration, last_gain: f32) -> ((f32, f32), f32) {
    let secs = dt.as_secs_f32();
    let g = if secs > 0.0 { gain(by.0.hypot(by.1) / secs) } else { last_gain };
    ((by.0 * g, by.1 * g), g)
}

/// A mouse button, as the trackpad presses it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// The primary button.
    Left,
    /// The secondary button (context menus).
    Right,
}

/// What the trackpad asks of the remote pointer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    /// The pointer moved to [`Trackpad::at`].
    Move,
    /// A button went down at the pointer, as the `clicks`th click.
    Press(Button, u8),
    /// A button came up at the pointer.
    Release(Button, u8),
}

/// One finger down on the trackpad.
#[derive(Clone, Copy, Debug)]
struct Finger {
    last: (f32, f32),
    last_at: Instant,
    began: Instant,
    travel: f32,
    gain: f32,
    /// This touch followed a tap closely: a drag holds the button, a tap is a double click.
    after_tap: Option<u8>,
    /// The button is held for a drag.
    dragging: bool,
}

/// The trackpad: where its pointer is on the picture, and the finger on it.
#[derive(Clone, Copy, Debug)]
pub struct Trackpad {
    /// The pointer on the picture, 0 to 1 on each axis.
    at: (f32, f32),
    finger: Option<Finger>,
    /// When the last tap clicked, and which click it was.
    last_tap: Option<(Instant, u8)>,
}

impl Trackpad {
    /// A trackpad whose pointer starts at `at` on the picture.
    #[must_use]
    pub const fn new(at: (f32, f32)) -> Self {
        Self { at: clamp_unit(at), finger: None, last_tap: None }
    }

    /// The pointer on the picture, 0 to 1 on each axis.
    #[must_use]
    pub const fn at(&self) -> (f32, f32) {
        self.at
    }

    /// Whether a finger is down.
    #[must_use]
    pub const fn touching(&self) -> bool {
        self.finger.is_some()
    }

    /// A finger landed at `p`.
    pub fn begin(&mut self, p: (f32, f32), now: Instant) {
        let after_tap = self
            .last_tap
            .filter(|(at, _)| now.saturating_duration_since(*at) <= MULTI_TAP)
            .map(|(_, clicks)| clicks);
        self.finger = Some(Finger {
            last: p,
            last_at: now,
            began: now,
            travel: 0.0,
            gain: 1.0,
            after_tap,
            dragging: false,
        });
    }

    /// The finger moved to `p`. `span` is the picture's drawn size in points, so a point of
    /// finger travel is a point of pointer travel on the screen whatever the zoom: zoomed in,
    /// the same stroke covers fewer of the target's pixels.
    pub fn moved(&mut self, p: (f32, f32), now: Instant, span: (f32, f32)) -> Vec<Act> {
        let Some(finger) = self.finger.as_mut() else { return Vec::new() };
        let by = (p.0 - finger.last.0, p.1 - finger.last.1);
        let dt = now.saturating_duration_since(finger.last_at);
        let (step, gain) = accelerate(by, dt, finger.gain);
        finger.gain = gain;
        finger.last = p;
        finger.last_at = now;
        finger.travel += by.0.hypot(by.1);
        let mut acts = Vec::new();
        if let Some(clicks) = finger.after_tap
            && !finger.dragging
            && finger.travel >= TAP_SLOP
        {
            finger.dragging = true;
            acts.push(Act::Press(Button::Left, clicks));
        }
        let before = self.at;
        self.at = clamp_unit((
            self.at.0 + step.0 / span.0.max(1.0),
            self.at.1 + step.1 / span.1.max(1.0),
        ));
        if self.at != before {
            acts.push(Act::Move);
        }
        acts
    }

    /// The finger lifted: a tap clicks, a held still finger right-clicks, a drag lets go.
    pub fn ended(&mut self, now: Instant) -> Vec<Act> {
        let Some(finger) = self.finger.take() else { return Vec::new() };
        if finger.dragging {
            self.last_tap = None;
            return vec![Act::Release(Button::Left, 1)];
        }
        if finger.travel >= TAP_SLOP {
            self.last_tap = None;
            return Vec::new();
        }
        if now.saturating_duration_since(finger.began) >= HOLD {
            self.last_tap = None;
            return click(Button::Right, 1);
        }
        let clicks = finger.after_tap.map_or(1, |c| c.saturating_add(1));
        self.last_tap = Some((now, clicks));
        click(Button::Left, clicks)
    }

    /// The touch was taken away (the system's pinch recognizer took both fingers): a held
    /// button is let go, nothing is clicked.
    pub fn cancelled(&mut self) -> Vec<Act> {
        let dragging = self.finger.take().is_some_and(|f| f.dragging);
        self.last_tap = None;
        if dragging { vec![Act::Release(Button::Left, 1)] } else { Vec::new() }
    }

    /// Put the pointer at `at` on the picture (where a real pointer put it).
    pub const fn place(&mut self, at: (f32, f32)) {
        self.at = clamp_unit(at);
    }
}

fn click(button: Button, clicks: u8) -> Vec<Act> {
    vec![Act::Press(button, clicks), Act::Release(button, clicks)]
}

const fn clamp_unit(p: (f32, f32)) -> (f32, f32) {
    (p.0.clamp(0.0, 1.0), p.1.clamp(0.0, 1.0))
}

/// What two fingers turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TwoKind {
    /// Not yet past either threshold.
    Undecided,
    /// The scale moved first.
    Pinch,
    /// The centroid moved first.
    Drag,
}

/// Two fingers on the picture, from the pinch recognizer's reports.
#[derive(Clone, Copy, Debug)]
pub struct Two {
    began: Instant,
    /// The product of the steps' scales so far.
    scale: f32,
    /// The centroid as last reported, and how far it has gone.
    last: (f32, f32),
    travel: f32,
    kind: TwoKind,
}

/// One report of two fingers, read.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TwoStep {
    /// The scale step (1 is none).
    pub factor: f32,
    /// The centroid's move since the last report, in points.
    pub by: (f32, f32),
    /// What the gesture is, so far.
    pub kind: TwoKind,
}

impl Two {
    /// Fingers came down with their centroid at `at`.
    #[must_use]
    pub const fn begin(at: (f32, f32), now: Instant) -> Self {
        Self { began: now, scale: 1.0, last: at, travel: 0.0, kind: TwoKind::Undecided }
    }

    /// The recognizer reported a scale step of `factor` with the centroid at `at`.
    pub fn step(&mut self, factor: f32, at: (f32, f32)) -> TwoStep {
        let by = (at.0 - self.last.0, at.1 - self.last.1);
        self.last = at;
        self.scale *= factor;
        self.travel += by.0.hypot(by.1);
        if self.kind == TwoKind::Undecided {
            if (self.scale - 1.0).abs() >= PINCH_LOCK {
                self.kind = TwoKind::Pinch;
            } else if self.travel >= DRAG_LOCK {
                self.kind = TwoKind::Drag;
            }
        }
        TwoStep { factor, by, kind: self.kind }
    }

    /// The fingers lifted: whether they only tapped (short, still, no pinch).
    #[must_use]
    pub fn tapped(&self, now: Instant) -> bool {
        self.kind == TwoKind::Undecided
            && self.travel < TAP_SLOP
            && now.saturating_duration_since(self.began) <= TWO_FINGER_TAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: (f32, f32) = (400.0, 300.0);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// The gain is 1 while aiming, rises with speed, and tops out; it never falls as the finger
    /// speeds up.
    #[test]
    fn acceleration_is_one_while_aiming_and_rises_to_a_cap() {
        assert!((gain(0.0) - 1.0).abs() < f32::EPSILON);
        assert!((gain(PRECISE_SPEED) - 1.0).abs() < f32::EPSILON);
        assert!((gain(FAST_SPEED) - MAX_GAIN).abs() < f32::EPSILON);
        assert!((gain(FAST_SPEED * 10.0) - MAX_GAIN).abs() < f32::EPSILON, "capped");
        let mut last = 0.0;
        for speed in (0_u16..40).map(|n| f32::from(n) * 60.0) {
            let g = gain(speed);
            assert!(g >= last, "monotone at {speed}");
            last = g;
        }
        // Direction is kept, and a step with no time behind it takes the last gain.
        let ((x, y), g) = accelerate((-3.0, 4.0), ms(1), 1.0);
        assert!((g - MAX_GAIN).abs() < f32::EPSILON, "5 pt in 1 ms is a flick");
        assert!(x < 0.0 && y > 0.0 && (y / x + 4.0 / 3.0).abs() < 1e-5);
        let (step, g) = accelerate((1.0, 0.0), Duration::ZERO, 2.5);
        assert!((g - 2.5).abs() < f32::EPSILON && (step.0 - 2.5).abs() < f32::EPSILON);
    }

    /// A slow drag moves the pointer point for point on the drawn picture; a fast one moves it
    /// further; the pointer stops at the picture's edges.
    #[test]
    fn a_drag_moves_the_pointer_relatively() {
        let t = Instant::now();
        let mut pad = Trackpad::new((0.5, 0.5));
        pad.begin((10.0, 10.0), t);
        // 4 pt in 40 ms is 100 pt/s: aiming.
        assert_eq!(pad.moved((14.0, 10.0), t + ms(40), SPAN), vec![Act::Move]);
        assert!((pad.at().0 - (0.5 + 4.0 / 400.0)).abs() < 1e-6, "{:?}", pad.at());
        // 40 pt in 10 ms is a flick: three times as far.
        pad.moved((54.0, 10.0), t + ms(50), SPAN);
        assert!((pad.at().0 - (0.51 + 120.0 / 400.0)).abs() < 1e-5, "{:?}", pad.at());
        // Zoomed in (a picture drawn four times as big), the same stroke covers a quarter.
        let mut zoomed = Trackpad::new((0.5, 0.5));
        zoomed.begin((0.0, 0.0), t);
        zoomed.moved((4.0, 0.0), t + ms(40), (1600.0, 1200.0));
        assert!((zoomed.at().0 - (0.5 + 4.0 / 1600.0)).abs() < 1e-6);
        // Past the edge the pointer holds, and a move that moves nothing says nothing.
        pad.moved((5000.0, -5000.0), t + ms(60), SPAN);
        assert_eq!(pad.at(), (1.0, 0.0));
        assert_eq!(pad.moved((5100.0, -5100.0), t + ms(70), SPAN), Vec::<Act>::new());
        assert!(pad.ended(t + ms(80)).is_empty(), "a drag clicks nothing");
    }

    /// A tap clicks at the pointer, a quick second tap is a double click, a still finger held
    /// right-clicks, and a pointer that did not move is not moved.
    #[test]
    fn taps_click_at_the_pointer() {
        let t = Instant::now();
        let mut pad = Trackpad::new((0.2, 0.3));
        pad.begin((50.0, 50.0), t);
        assert!(pad.moved((52.0, 51.0), t + ms(30), SPAN).len() <= 1, "a wobble");
        let at = pad.at();
        assert_eq!(
            pad.ended(t + ms(90)),
            vec![Act::Press(Button::Left, 1), Act::Release(Button::Left, 1)]
        );
        pad.begin((60.0, 60.0), t + ms(200));
        assert_eq!(
            pad.ended(t + ms(260)),
            vec![Act::Press(Button::Left, 2), Act::Release(Button::Left, 2)],
            "the second tap soon after is a double click"
        );
        pad.begin((60.0, 60.0), t + ms(2000));
        assert_eq!(
            pad.ended(t + ms(2060)),
            vec![Act::Press(Button::Left, 1), Act::Release(Button::Left, 1)],
            "late: a new first click"
        );
        pad.begin((60.0, 60.0), t + ms(4000));
        assert_eq!(
            pad.ended(t + ms(4000) + HOLD),
            vec![Act::Press(Button::Right, 1), Act::Release(Button::Right, 1)],
            "held still: a right click"
        );
        assert!((pad.at().0 - at.0).abs() < 0.01, "taps do not move the pointer");
    }

    /// A tap and then a drag drags with the button held, and lets go when the finger lifts or
    /// the touch is taken away.
    #[test]
    fn tap_then_drag_holds_the_button() {
        let t = Instant::now();
        let mut pad = Trackpad::new((0.5, 0.5));
        pad.begin((0.0, 0.0), t);
        pad.ended(t + ms(80));
        pad.begin((0.0, 0.0), t + ms(200));
        assert_eq!(pad.moved((3.0, 0.0), t + ms(240), SPAN), vec![Act::Move], "within the slop");
        assert_eq!(
            pad.moved((12.0, 0.0), t + ms(280), SPAN),
            vec![Act::Press(Button::Left, 1), Act::Move],
            "past the slop the button goes down"
        );
        assert_eq!(pad.ended(t + ms(400)), vec![Act::Release(Button::Left, 1)]);
        pad.begin((0.0, 0.0), t + ms(420));
        pad.ended(t + ms(460));
        pad.begin((0.0, 0.0), t + ms(500));
        pad.moved((20.0, 0.0), t + ms(540), SPAN);
        assert_eq!(pad.cancelled(), vec![Act::Release(Button::Left, 1)], "taken away: let go");
        assert!(pad.cancelled().is_empty(), "once");
        assert!(pad.ended(t + ms(600)).is_empty(), "no finger, nothing");
    }

    /// Two fingers are a pinch or a drag by whichever threshold they pass first, and a short
    /// still pair is a tap.
    #[test]
    fn two_fingers_lock_to_a_pinch_or_a_drag() {
        let t = Instant::now();
        let mut two = Two::begin((100.0, 100.0), t);
        assert_eq!(two.step(1.02, (101.0, 100.0)).kind, TwoKind::Undecided);
        assert_eq!(two.step(1.05, (102.0, 100.0)).kind, TwoKind::Pinch);
        assert_eq!(two.step(1.0, (140.0, 100.0)).kind, TwoKind::Pinch, "locked");
        let mut two = Two::begin((100.0, 100.0), t);
        let step = two.step(1.01, (106.0, 108.0));
        assert_eq!((step.kind, step.by), (TwoKind::Drag, (6.0, 8.0)));
        assert_eq!(two.step(1.5, (106.0, 108.0)).kind, TwoKind::Drag, "locked");
        assert!(!two.tapped(t + ms(100)), "a drag is no tap");
        let mut two = Two::begin((100.0, 100.0), t);
        two.step(1.01, (102.0, 101.0));
        assert!(two.tapped(t + ms(150)), "short and still");
        assert!(!two.tapped(t + ms(800)), "resting too long");
    }
}
