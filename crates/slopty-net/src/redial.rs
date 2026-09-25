//! When a dropped link is dialled again.
//!
//! The server link and every worker link follow the one rule: a quarter second after a drop,
//! doubling per failure to [`MAX`], and back to the start once a link has held for [`STEADY`].

use std::time::{Duration, Instant};

/// The first redial after a drop waits this long.
pub const FIRST: Duration = Duration::from_millis(250);
/// Redials back off to this at most, so a peer that comes back is dialled within it.
pub const MAX: Duration = Duration::from_secs(2);
/// A link that held this long was healthy: its drop starts the backoff again from [`FIRST`].
pub const STEADY: Duration = Duration::from_secs(10);

/// The wait before redial number `failures` (0 for the first after a drop): 250 ms doubling to
/// [`MAX`].
#[must_use]
pub fn delay(failures: u32) -> Duration {
    FIRST.saturating_mul(1_u32 << failures.min(8)).min(MAX)
}

/// One link's backoff. A link that drops soon after it came up (a peer that accepts and then
/// fails) keeps backing off instead of being redialled at once forever.
#[derive(Clone, Copy, Debug, Default)]
pub struct Redial {
    failures: u32,
    linked: Option<Instant>,
}

impl Redial {
    /// The link came up at `now`.
    pub const fn linked(&mut self, now: Instant) {
        self.linked = Some(now);
    }

    /// The link dropped, or a dial failed, at `now`: how long to wait before the next dial.
    pub fn next(&mut self, now: Instant) -> Duration {
        if self.linked.take().is_some_and(|up| now.saturating_duration_since(up) >= STEADY) {
            self.failures = 0;
        }
        let wait = delay(self.failures);
        self.failures = self.failures.saturating_add(1);
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redials_back_off_from_a_quarter_second_to_two() {
        let delays: Vec<u128> = (0..6).map(|n| delay(n).as_millis()).collect();
        assert_eq!(delays, [250, 500, 1000, 2000, 2000, 2000]);
        assert_eq!(delay(u32::MAX), MAX);
    }

    #[test]
    fn a_steady_link_starts_the_backoff_again_and_a_flapping_one_does_not() {
        let start = Instant::now();
        let mut redial = Redial::default();
        let failed: Vec<u128> =
            std::iter::repeat_with(|| redial.next(start).as_millis()).take(4).collect();
        assert_eq!(failed, [250, 500, 1000, 2000], "dials that fail back off");

        redial.linked(start);
        assert_eq!(redial.next(start + STEADY / 2), MAX, "a link that flapped keeps backing off");

        redial.linked(start);
        assert_eq!(redial.next(start + STEADY), FIRST, "a link that held starts again");
        assert_eq!(redial.next(start + STEADY), delay(1), "and a failed dial after it doubles");
    }
}
