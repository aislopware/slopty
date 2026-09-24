//! Gesture helpers: niri's swipe tracker (velocity over the last 150 ms and where a fling would
//! stop), its rubber band, the axis lock that tells a horizontal swipe from a vertical one, and
//! the wheel accumulator. Pure: every reading carries its own timestamp.

use std::collections::VecDeque;
use std::time::Duration;

/// Readings older than this (behind the newest) do not count towards the velocity.
const HISTORY_LIMIT: Duration = Duration::from_millis(150);
/// niri's touchpad deceleration per millisecond.
const DECELERATION: f64 = 0.997;

/// One reading.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Event {
    delta: f64,
    at: Duration,
}

/// Tracks a one-axis gesture: where it is and how fast it moves.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SwipeTracker {
    history: VecDeque<Event>,
    pos: f64,
}

impl SwipeTracker {
    /// A tracker at 0.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a reading. One older than the newest is ignored (timestamps only move forward).
    pub fn push(&mut self, delta: f64, at: Duration) {
        if self.history.back().is_some_and(|last| at < last.at) {
            return;
        }
        self.history.push_back(Event { delta, at });
        self.pos += delta;
        self.trim();
    }

    /// Where the gesture is: the sum of every delta.
    #[must_use]
    pub const fn pos(&self) -> f64 {
        self.pos
    }

    /// Units per second over the readings still in the window.
    #[must_use]
    pub fn velocity(&self) -> f64 {
        let (Some(first), Some(last)) = (self.history.front(), self.history.back()) else {
            return 0.0;
        };
        let total = last.at.saturating_sub(first.at).as_secs_f64();
        if total <= 0.0 {
            return 0.0;
        }
        self.history.iter().map(|e| e.delta).sum::<f64>() / total
    }

    /// Where the gesture would come to rest if let go now and left to decelerate: about the
    /// velocity × 0.333 s further on.
    #[must_use]
    pub fn projected_end_pos(&self) -> f64 {
        self.pos - self.velocity() / (1000.0 * DECELERATION.ln())
    }

    fn trim(&mut self) {
        let Some(newest) = self.history.back().map(|e| e.at) else { return };
        while self.history.front().is_some_and(|e| newest > e.at.saturating_add(HISTORY_LIMIT)) {
            self.history.pop_front();
        }
    }
}

/// Resistance past an end: movement beyond it is compressed towards `limit`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct RubberBand {
    /// How stiff the band is.
    pub stiffness: f64,
    /// The most it ever gives.
    pub limit: f64,
}

impl RubberBand {
    /// `(1 − 1/(x·c/d + 1))·d`.
    #[must_use]
    pub fn band(&self, x: f64) -> f64 {
        let (c, d) = (self.stiffness, self.limit);
        (1.0 - (1.0 / (x * c / d + 1.0))) * d
    }

    /// d band / dx.
    #[must_use]
    pub fn derivative(&self, x: f64) -> f64 {
        let (c, d) = (self.stiffness, self.limit);
        c * d * d / c.mul_add(x, d).powi(2)
    }

    /// `x` within `min..=max`, or banded past whichever end it crossed.
    #[must_use]
    pub fn clamp(&self, min: f64, max: f64, x: f64) -> f64 {
        let clamped = x.clamp(min, max);
        let sign: f64 = if x < clamped { -1.0 } else { 1.0 };
        sign.mul_add(self.band((x - clamped).abs()), clamped)
    }

    /// The slope of [`Self::clamp`] at `x`: 1 inside, the band's derivative outside.
    #[must_use]
    pub fn clamp_derivative(&self, min: f64, max: f64, x: f64) -> f64 {
        if min <= x && x <= max {
            return 1.0;
        }
        let clamped = x.clamp(min, max);
        self.derivative((x - clamped).abs())
    }
}

/// Which way a two-finger gesture goes, once it has gone far enough to tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    /// Not yet [`AxisLock::DISTANCE`] of movement.
    Undecided,
    /// The strip: columns.
    Horizontal,
    /// Workspaces (or the content under the pointer).
    Vertical,
}

/// Decides a gesture's axis after 16 pt of movement, then keeps it for the gesture's life.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct AxisLock {
    dx: f32,
    dy: f32,
    axis: Axis,
}

impl Default for AxisLock {
    fn default() -> Self {
        Self::new()
    }
}

impl AxisLock {
    /// Movement before the axis is decided, in points.
    pub const DISTANCE: f32 = 16.0;

    /// A fresh gesture.
    #[must_use]
    pub const fn new() -> Self {
        Self { dx: 0.0, dy: 0.0, axis: Axis::Undecided }
    }

    /// Feed one delta; the axis once decided (horizontal when |dx| > |dy| of the total).
    pub fn feed(&mut self, dx: f32, dy: f32) -> Axis {
        if self.axis == Axis::Undecided {
            self.dx += dx;
            self.dy += dy;
            if self.dx.hypot(self.dy) >= Self::DISTANCE {
                self.axis =
                    if self.dx.abs() > self.dy.abs() { Axis::Horizontal } else { Axis::Vertical };
            }
        }
        self.axis
    }

    /// The axis so far.
    #[must_use]
    pub const fn axis(&self) -> Axis {
        self.axis
    }

    /// Movement accumulated before the axis was decided `(dx, dy)`, to replay into the
    /// gesture the lock hands over to.
    #[must_use]
    pub const fn pending(&self) -> (f32, f32) {
        (self.dx, self.dy)
    }
}

/// Turns wheel deltas into whole steps, resetting on a change of direction (niri's
/// `ScrollTracker`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WheelTracker {
    tick: f32,
    last: f32,
    acc: f32,
}

impl WheelTracker {
    /// Steps of `tick` points.
    #[must_use]
    pub const fn new(tick: f32) -> Self {
        Self { tick, last: 0.0, acc: 0.0 }
    }

    /// Add `amount`; the whole steps it completes (signed).
    pub fn accumulate(&mut self, amount: f32) -> i32 {
        let turned = (self.last > 0.0 && amount < 0.0) || (self.last < 0.0 && amount > 0.0);
        if turned {
            self.acc = 0.0;
        }
        self.last = amount;
        self.acc += amount;
        if self.tick <= 0.0 || self.acc.abs() < self.tick {
            return 0;
        }
        let steps = (self.acc / self.tick).trunc().clamp(-127.0, 127.0);
        self.acc %= self.tick;
        #[expect(clippy::cast_possible_truncation, reason = "clamped to ±127 just above")]
        let steps = steps as i32;
        steps
    }

    /// Forget any partial step.
    pub const fn reset(&mut self) {
        self.last = 0.0;
        self.acc = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn velocity_is_the_last_150_ms_and_the_projection_runs_a_third_of_a_second_on() {
        let mut t = SwipeTracker::new();
        assert!(t.velocity().abs() < f64::EPSILON, "no readings, no velocity");
        // 10 units every 10 ms: 1000 units/s.
        for i in 0..=20 {
            t.push(10.0, MS(i * 10));
        }
        assert!((t.pos() - 210.0).abs() < 1e-9, "{}", t.pos());
        // Only the last 150 ms (16 readings, 160 units over 150 ms) count.
        assert!((t.velocity() - 160.0 / 0.15).abs() < 1e-6, "{}", t.velocity());
        let projected = t.projected_end_pos() - t.pos();
        let expected = t.velocity() * (-1.0 / (1000.0 * 0.997_f64.ln()));
        assert!((projected - expected).abs() < 1e-9, "{projected}");
        assert!((0.33..0.34).contains(&(projected / t.velocity())), "≈ v × 0.333 s");
        // A reading from the past is ignored.
        t.push(1000.0, MS(5));
        assert!((t.pos() - 210.0).abs() < 1e-9, "ignored");
    }

    #[test]
    fn a_pause_before_letting_go_kills_the_fling() {
        let mut t = SwipeTracker::new();
        for i in 0..10 {
            t.push(20.0, MS(i * 10));
        }
        t.push(0.0, MS(400));
        assert!(t.velocity().abs() < f64::EPSILON, "the readings aged out: {}", t.velocity());
    }

    #[test]
    fn the_rubber_band_gives_less_and_less() {
        let band = RubberBand { stiffness: 0.5, limit: 0.05 };
        assert!((band.clamp(0.0, 2.0, 1.0) - 1.0).abs() < 1e-12, "inside is untouched");
        let past = band.clamp(0.0, 2.0, 3.0);
        assert!(past > 2.0 && past < 2.05, "{past}");
        let far = band.clamp(0.0, 2.0, 100.0);
        assert!(far < 2.05 && far > past, "{far}");
        let below = band.clamp(0.0, 2.0, -1.0);
        assert!(below < 0.0 && below > -0.05, "{below}");
        assert!((band.clamp_derivative(0.0, 2.0, 1.0) - 1.0).abs() < f64::EPSILON, "slope inside");
        assert!(band.clamp_derivative(0.0, 2.0, 3.0) < 0.1, "flat outside");
    }

    #[test]
    fn the_axis_is_decided_after_16_points_and_kept() {
        let mut lock = AxisLock::new();
        assert_eq!(lock.feed(5.0, 3.0), Axis::Undecided, "too little");
        assert_eq!(lock.feed(9.0, 2.0), Axis::Undecided, "14.9 of 16");
        assert_eq!(lock.feed(2.0, 0.0), Axis::Horizontal, "|dx| wins");
        assert_eq!(lock.feed(0.0, 100.0), Axis::Horizontal, "kept for the gesture");
        assert_eq!(lock.pending(), (16.0, 5.0), "what came before the decision");
        let mut lock = AxisLock::new();
        assert_eq!(lock.feed(3.0, -20.0), Axis::Vertical, "|dy| wins");
    }

    #[test]
    fn the_wheel_counts_whole_steps_and_resets_on_a_turn() {
        let mut w = WheelTracker::new(50.0);
        assert_eq!(w.accumulate(30.0), 0, "part of a step");
        assert_eq!(w.accumulate(30.0), 1, "a step, 10 carried");
        assert_eq!(w.accumulate(45.0), 1, "the carry counts");
        assert_eq!(w.accumulate(-30.0), 0, "a turn drops the carry");
        assert_eq!(w.accumulate(-30.0), -1, "then counts the other way");
        assert_eq!(w.accumulate(260.0), 5, "several at once");
        w.reset();
        assert_eq!(w.accumulate(49.0), 0, "reset");
    }
}
