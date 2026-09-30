//! Keeping a resting drag moving, so a spring-loaded target springs.
//!
//! A posted drag resting still over a spring-loaded target lights it but never springs it,
//! because the drag manager decides only when a move comes (`docs/decisions/audio.md`, "Drag and
//! drop lands at the point", P0 (8b) and the helper's roles). Nor do small moves at a hand's
//! pace: one-point moves never reach the target at all, and moves every 650 ms or less never
//! spring it, however long they go on. A two-point move out and back each time the drag has
//! rested longer than the spring delay springs it at the second move. So while no move has come
//! for [`rest_us`] of the spring delay, the worker posts the drag [`STEP`] points to the right,
//! then back where the pointer rests, and so on, until the next real move or the end of the
//! drag.

/// How far a nudge moves the drag, in points: the least the drag manager passes on.
pub const STEP: f64 = 2.0;

/// The rest past the spring delay before a nudge springs a target: 0.5 s of delay sprang
/// nudges 700 ms apart and never 650 ms ones, so the margin is 300 ms.
const MARGIN_US: u64 = 300_000;

/// The spring delay a Mac uses when `com.apple.springing.delay` is unset, in seconds.
pub const DEFAULT_SPRING_DELAY_S: f64 = 0.5;

/// How long a drag rests before it is nudged, and between nudges, for a spring delay of
/// `spring_delay_s` seconds (`com.apple.springing.delay`, [`spring_delay_s`]).
#[must_use]
pub fn rest_us(spring_delay_s: f64) -> u64 {
    let delay = if spring_delay_s.is_finite() && spring_delay_s > 0.0 {
        spring_delay_s
    } else {
        DEFAULT_SPRING_DELAY_S
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive delay of seconds, rounded to microseconds; `as` saturates"
    )]
    let delay_us = (delay * 1e6).round() as u64;
    delay_us.saturating_add(MARGIN_US)
}

/// This Mac's spring delay in seconds: `com.apple.springing.delay` in the global domain, as
/// System Settings writes it, else [`DEFAULT_SPRING_DELAY_S`].
#[cfg(target_os = "macos")]
#[must_use]
pub fn spring_delay_s() -> f64 {
    use objc2_foundation::{NSString, NSUserDefaults};
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("com.apple.springing.delay");
    if defaults.objectForKey(&key).is_some() {
        defaults.doubleForKey(&key)
    } else {
        DEFAULT_SPRING_DELAY_S
    }
}

/// Where a posted drag is, and when it was last posted, so a resting one is nudged.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Nudge {
    /// Where the last real move put the drag.
    at: (f64, f64),
    /// When the drag was last posted, a real move or a nudge, in microseconds.
    posted_us: u64,
    /// How long a rest is, in microseconds ([`rest_us`]).
    rest_us: u64,
    /// Whether the last nudge went out, so the next comes back.
    out: bool,
}

impl Nudge {
    /// A drag posted at `at` at `now_us`, nudged after rests of `rest_us` ([`rest_us`]).
    #[must_use]
    pub const fn new(at: (f64, f64), now_us: u64, rest_us: u64) -> Self {
        Self { at, posted_us: now_us, rest_us, out: false }
    }

    /// A real move to `at` was posted at `now_us`: the rest starts again from there.
    pub const fn moved(&mut self, at: (f64, f64), now_us: u64) {
        *self = Self::new(at, now_us, self.rest_us);
    }

    /// When the next nudge is due, in microseconds.
    #[must_use]
    pub const fn due_us(&self) -> u64 {
        self.posted_us.saturating_add(self.rest_us)
    }

    /// The point to post the drag at now, if it has rested since it was last posted: [`STEP`]
    /// points to the right of where the pointer rests, then back there, and so on.
    pub fn nudge(&mut self, now_us: u64) -> Option<(f64, f64)> {
        if now_us < self.due_us() {
            return None;
        }
        self.out = !self.out;
        self.posted_us = now_us;
        let (x, y) = self.at;
        Some(if self.out { (x + STEP, y) } else { (x, y) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REST: u64 = 800_000;

    /// A drag moving faster than a rest is never nudged; once it rests, it goes [`STEP`] points
    /// out and back every rest; a real move starts the rest again, from the new point.
    #[test]
    fn a_resting_drag_goes_out_and_back_and_a_move_resets_it() {
        let mut nudge = Nudge::new((100.0, 50.0), 0, REST);
        assert_eq!(nudge.nudge(REST - 1), None, "not rested yet");
        nudge.moved((110.5, 50.0), 600_000);
        assert_eq!(nudge.nudge(1_300_000), None, "the move started the rest again");
        assert_eq!(nudge.due_us(), 1_400_000);
        assert_eq!(nudge.nudge(1_400_000), Some((112.5, 50.0)), "out");
        assert_eq!(nudge.nudge(2_000_000), None, "not due yet");
        assert_eq!(nudge.nudge(2_200_000), Some((110.5, 50.0)), "and back, exactly");
        assert_eq!(nudge.nudge(3_000_000), Some((112.5, 50.0)), "and out again");
        nudge.moved((5.0, 6.0), 3_100_000);
        assert_eq!(nudge.nudge(3_900_000), Some((7.0, 6.0)), "from where the pointer rests now");
    }

    /// Late wakes (a busy thread) nudge once, not once per missed rest.
    #[test]
    fn a_late_wake_nudges_once() {
        let mut nudge = Nudge::new((0.0, 0.0), 0, REST);
        assert_eq!(nudge.nudge(5_000_000), Some((2.0, 0.0)));
        assert_eq!(nudge.nudge(5_000_001), None, "the next is a whole rest later");
        assert_eq!(nudge.due_us(), 5_800_000);
    }

    /// The rest is the spring delay and the margin; a delay that is unset, zero or not a number
    /// is the system's default.
    #[test]
    fn the_rest_follows_the_spring_delay() {
        assert_eq!(rest_us(0.5), 800_000);
        assert_eq!(rest_us(1.0), 1_300_000);
        assert_eq!(rest_us(0.0), 800_000);
        assert_eq!(rest_us(f64::NAN), 800_000);
        assert_eq!(rest_us(-1.0), 800_000);
    }
}
