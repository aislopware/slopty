//! When the terminal's overlay scrollbar shows, as macOS and Zed decide it for theirs: never at
//! rest; while the viewport moves, while the pointer is near the grid's right edge and while
//! the thumb is held. Once the last of those ends it stays [`LINGER`], then fades out over
//! [`FADE`], or goes at once under Reduce Motion. Pure: the caller passes the time.

use std::time::{Duration, Instant};

/// How long the bar stays after the last reason to show it ended (Zed's hide delay).
pub const LINGER: Duration = Duration::from_secs(1);
/// How long it takes to fade out (Zed's hide duration).
pub const FADE: Duration = Duration::from_millis(400);

/// The bar's reasons to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visibility {
    /// The pointer is near the grid's right edge.
    near_edge: bool,
    /// The thumb is held by the pointer.
    held: bool,
    /// When the last reason ended: a step of scrolling, the pointer leaving the edge, the
    /// thumb let go. The linger counts from here.
    woke: Option<Instant>,
}

impl Visibility {
    /// The viewport moved.
    pub const fn scrolled(&mut self, now: Instant) {
        self.woke = Some(now);
    }

    /// The pointer is (or is not) near the right edge. True when that changed.
    pub const fn pointer(&mut self, near: bool, now: Instant) -> bool {
        if self.near_edge == near {
            return false;
        }
        self.near_edge = near;
        if !near {
            self.woke = Some(now);
        }
        true
    }

    /// The thumb was taken or let go.
    pub const fn hold(&mut self, held: bool, now: Instant) {
        if self.held && !held {
            self.woke = Some(now);
        }
        self.held = held;
    }

    /// Something keeps it up regardless of time.
    const fn pinned(&self) -> bool {
        self.near_edge || self.held
    }

    /// How much of the bar shows at `now`, 0 (hidden) to 1.
    #[must_use]
    pub fn opacity(&self, now: Instant, reduce_motion: bool) -> f32 {
        if self.pinned() {
            return 1.0;
        }
        let Some(woke) = self.woke else { return 0.0 };
        let Some(fading) = now.saturating_duration_since(woke).checked_sub(LINGER) else {
            return 1.0;
        };
        if reduce_motion {
            return 0.0;
        }
        (1.0 - fading.as_secs_f32() / FADE.as_secs_f32()).max(0.0)
    }

    /// How long until the linger ends and the fade starts; `None` while pinned, at rest or
    /// already fading. The caller wakes then to draw the fade.
    #[must_use]
    pub fn linger_left(&self, now: Instant) -> Option<Duration> {
        if self.pinned() {
            return None;
        }
        let since = now.saturating_duration_since(self.woke?);
        LINGER.checked_sub(since).filter(|left| !left.is_zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    /// Hidden at rest; a scroll shows it for the linger, then it fades out over `FADE` (or
    /// goes at once under Reduce Motion); the pointer at the edge or a held thumb keep it up
    /// however long, and the linger starts over when they end.
    #[test]
    fn the_bar_shows_while_used_then_lingers_and_fades() {
        let t0 = Instant::now();
        let at = |ms| t0 + MS(ms);
        let mut bar = Visibility::default();
        assert!(bar.opacity(at(0), false) <= 0.0, "hidden at rest");
        assert_eq!(bar.linger_left(at(0)), None, "nothing to wake for");

        bar.scrolled(at(0));
        assert!((bar.opacity(at(999), false) - 1.0).abs() < f32::EPSILON, "shown while it lingers");
        assert_eq!(bar.linger_left(at(400)), Some(MS(600)));
        let half = bar.opacity(at(1200), false);
        assert!((half - 0.5).abs() < 0.01, "half way through the fade: {half}");
        assert_eq!(bar.linger_left(at(1200)), None, "fading: drawn frame by frame, not woken");
        assert!(bar.opacity(at(1400), false) <= 0.0, "gone when the fade ends");
        assert!(bar.opacity(at(1000), true) <= 0.0, "Reduce Motion: gone when the linger ends");
        assert!((bar.opacity(at(999), true) - 1.0).abs() < f32::EPSILON);

        // A further scroll starts the linger over.
        bar.scrolled(at(1300));
        assert!((bar.opacity(at(2200), false) - 1.0).abs() < f32::EPSILON);

        // The pointer near the edge holds it up as long as it stays.
        assert!(bar.pointer(true, at(5000)));
        assert!(!bar.pointer(true, at(5100)), "no change, nothing to redraw");
        assert!((bar.opacity(at(60_000), true) - 1.0).abs() < f32::EPSILON);
        assert_eq!(bar.linger_left(at(60_000)), None);
        assert!(bar.pointer(false, at(60_000)));
        assert_eq!(bar.linger_left(at(60_000)), Some(LINGER), "leaving starts the linger");
        assert!(bar.opacity(at(61_400), false) <= 0.0);

        // So does a held thumb, even with the pointer dragged far from the edge.
        bar.hold(true, at(70_000));
        assert!((bar.opacity(at(90_000), false) - 1.0).abs() < f32::EPSILON);
        bar.hold(false, at(90_000));
        assert!((bar.opacity(at(90_500), false) - 1.0).abs() < f32::EPSILON);
        assert!(bar.opacity(at(91_400), false) <= 0.0);
    }
}
