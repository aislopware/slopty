//! Whether a link is alive, judged by its control stream (`docs/decisions/transport.md`, "A
//! link is alive while its control stream answers").
//!
//! QUIC's keep-alive proves only that the peer's endpoint still answers packets: a control
//! stream whose reader or writer has stopped keeps a connection that looks alive for good,
//! and so do the media datagrams beside it. So the side that judges a link sends a `Ping` on
//! the control stream every [`PING_EVERY`] and counts the link alive only while its `Pong`s
//! come back:
//! - [`SILENT_AFTER`] without one (one ping missed) the link is in doubt;
//! - [`PONG_WITHIN`] without one (two missed) it is dead, and is probed once more, then given up
//!   and dialled again.
//!
//! A side that was not running for a while (its machine asleep, its process suspended in the
//! background, its clock leapt) cannot blame the peer for what it did not hear meanwhile: a
//! look that comes [`AWAY_AFTER`] after the one before counts only the silence before the gap,
//! and pings at once.
//!
//! QUIC's keep-alive stays, for NAT bindings and the path's round trip. A [`Beat`] is the pure
//! clock the app's links to workers, a worker's link to the server and the server's side of it
//! all run on.

use std::time::{Duration, Instant};

/// How often the judging side pings the control stream.
pub const PING_EVERY: Duration = Duration::from_secs(2);
/// Nothing back for this long, one ping missed: the link is in doubt.
pub const SILENT_AFTER: Duration = PING_EVERY.saturating_mul(2);
/// Nothing back for this long, two pings missed: the link is dead. A stream that stalls just
/// after an answer is called dead this long after the stall began, at most.
pub const PONG_WITHIN: Duration = PING_EVERY.saturating_mul(3);
/// How often a side looks at its [`Beat`]: four times a ping, so a bar is crossed within half
/// a second of the silence reaching it.
pub const LOOK_EVERY: Duration = match PING_EVERY.checked_div(4) {
    Some(quarter) => quarter,
    None => Duration::ZERO,
};
/// Two looks this far apart mean this side was not running in between, not that the peer
/// went quiet.
pub const AWAY_AFTER: Duration = PING_EVERY;

/// What a [`Beat`] says of its link now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pulse {
    /// Answered within [`SILENT_AFTER`].
    Live,
    /// Nothing back for this long, past [`SILENT_AFTER`].
    Silent(Duration),
    /// Nothing back for this long, past [`PONG_WITHIN`]: probe it once more, then give it up.
    Dead(Duration),
}

/// One link's liveness clock: when its control stream last answered, when it was last
/// pinged, and when it was last looked at.
#[derive(Clone, Copy, Debug)]
pub struct Beat {
    heard: Instant,
    pinged: Option<Instant>,
    looked: Option<Instant>,
}

impl Beat {
    /// A link that came up, or answered, at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self { heard: now, pinged: None, looked: None }
    }

    /// Its control stream answered at `now`.
    pub fn heard(&mut self, now: Instant) {
        self.heard = self.heard.max(now);
    }

    /// Whether a ping is due at `now`: none sent yet, or [`PING_EVERY`] since the last. Taking
    /// it counts it sent.
    pub fn ping_due(&mut self, now: Instant) -> bool {
        let due = self.pinged.is_none_or(|at| now.saturating_duration_since(at) >= PING_EVERY);
        if due {
            self.pinged = Some(now);
        }
        due
    }

    /// The link at `now`. A look [`AWAY_AFTER`] past the last one finds this side was away:
    /// the silence counts as it stood at that last look, and a ping is due at once.
    pub fn pulse(&mut self, now: Instant) -> Pulse {
        if let Some(looked) = self.looked
            && now.saturating_duration_since(looked) >= AWAY_AFTER
        {
            let before = looked.saturating_duration_since(self.heard);
            self.heard = now.checked_sub(before).unwrap_or(now).max(self.heard);
            self.pinged = None;
        }
        self.looked = Some(self.looked.map_or(now, |looked| looked.max(now)));
        let gap = now.saturating_duration_since(self.heard);
        if gap >= PONG_WITHIN {
            Pulse::Dead(gap)
        } else if gap >= SILENT_AFTER {
            Pulse::Silent(gap)
        } else {
            Pulse::Live
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link answered every ping stays live; one that stops answering is in doubt after one
    /// missed ping and dead after two, counted from its last answer; an answer brings it back.
    #[test]
    fn a_link_is_live_while_it_answers_and_dead_after_two_missed_pings() {
        let start = tokio::time::Instant::now().into_std();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut beat = Beat::new(start);
        assert!(beat.ping_due(start), "the first ping goes at once");
        assert!(!beat.ping_due(at(1_999)), "then one every two seconds");
        assert!(beat.ping_due(at(2_000)));
        beat.heard(at(2_010));
        assert_eq!(beat.pulse(at(5_000)), Pulse::Live);
        assert_eq!(beat.pulse(at(6_010)), Pulse::Silent(SILENT_AFTER), "one ping missed");
        assert_eq!(beat.pulse(at(7_500)), Pulse::Silent(Duration::from_millis(5_490)));
        assert_eq!(beat.pulse(at(8_010)), Pulse::Dead(PONG_WITHIN), "two missed");
        beat.heard(at(8_500));
        assert_eq!(beat.pulse(at(8_500)), Pulse::Live, "an answer brings it back");
        beat.heard(at(100));
        assert_eq!(beat.pulse(at(8_500)), Pulse::Live, "a late stamp never moves it back");
    }

    /// Looked at every half second, a stream that stops answering is dead six seconds after its
    /// last answer, at most half a second late.
    #[test]
    fn a_stalled_stream_is_dead_within_six_seconds_and_a_look() {
        let start = tokio::time::Instant::now().into_std();
        let mut beat = Beat::new(start);
        let mut now = start;
        let dead = loop {
            if let Pulse::Dead(gap) = beat.pulse(now) {
                break gap;
            }
            let _due = beat.ping_due(now);
            now += LOOK_EVERY;
        };
        assert!(dead >= PONG_WITHIN && dead < PONG_WITHIN + LOOK_EVERY, "{dead:?}");
    }

    /// A side that was away (asleep, suspended, its clock leapt) does not call its peer dead
    /// for the gap: the silence stands where it stood before, a ping goes at once, and the peer
    /// still has the rest of its six seconds to answer it.
    #[test]
    fn a_side_that_was_away_does_not_blame_its_peer() {
        let start = tokio::time::Instant::now().into_std();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut beat = Beat::new(start);
        assert_eq!(beat.pulse(at(1_000)), Pulse::Live);
        assert!(beat.ping_due(at(1_000)));
        // Asleep for an hour.
        let woke = 3_600_000;
        assert_eq!(beat.pulse(at(woke)), Pulse::Live, "the hour is not the peer's silence");
        assert!(beat.ping_due(at(woke)), "a ping goes at once on waking");
        assert_eq!(beat.pulse(at(woke + 1_500)), Pulse::Live);
        assert_eq!(beat.pulse(at(woke + 3_000)), Pulse::Silent(SILENT_AFTER));
        assert_eq!(beat.pulse(at(woke + 4_500)), Pulse::Silent(Duration::from_millis(5_500)));
        assert_eq!(beat.pulse(at(woke + 5_000)), Pulse::Dead(PONG_WITHIN), "nothing since");
        // Answered after the wake: live.
        let mut beat = Beat::new(start);
        let _live = beat.pulse(at(500));
        let _away = beat.pulse(at(woke));
        beat.heard(at(woke + 40));
        assert_eq!(beat.pulse(at(woke + 500)), Pulse::Live);
    }
}
