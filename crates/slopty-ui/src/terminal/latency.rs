//! Keystroke → glass: how long a typed key takes to show, predicted and echoed, and where
//! the time went.
//!
//! The view records each key it sends ([`KeyLatency::pressed`]) and each frame from the worker
//! it applies ([`KeyLatency::applied`], with when that frame's batch left the link). The element
//! reports every frame it drew once that frame reaches the display ([`KeyLatency::presented`],
//! with when it was painted and submitted) and two facts: which keys the local-echo overlay is
//! showing (the predictor stamps each guess with the key's sequence number) and which key the
//! worker had applied in the frame on screen (`Frame::input_ack`). The first frame that shows a
//! key's guess is that key's *predicted* time; the first frame that acknowledges it is its
//! *echoed* time. A key the predictor drew nothing for (an arrow, a control key) gets no
//! predicted time. Each time is also kept per hop ([`LatencyStats::echo_hops`]). Pure: callers
//! pass the instants.
//!
//! A key the predictor guessed at can still reach the glass as its echo first: on a fast link
//! the worker answers before the next paint, and that frame shows the echo instead of the
//! guess. That is right, and counted apart ([`LatencyStats::echo_first`]). A guessed key whose
//! guess was missing from a frame painted after the key, while its echo had not come either, is
//! the guess path falling behind ([`LatencyStats::guess_late`]).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use slopty_client::pacing::percentile;

use crate::shown::Shown;

/// Keys kept for the percentiles.
pub const RING: usize = 256;

/// A key unanswered for this long fell off the shell (a control key, a dead prompt): dropped.
pub const STALE: Duration = Duration::from_secs(5);

/// A median and a 95th percentile.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Spread {
    /// Median.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
}

/// Percentiles and counts over the last [`RING`] keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LatencyStats {
    /// Keys whose echo from the worker has been shown.
    pub echoed: u64,
    /// Key → glass of the worker's echo, median.
    pub echo_p50: Duration,
    /// Same, 95th percentile.
    pub echo_p95: Duration,
    /// Same, 99th percentile.
    pub echo_p99: Duration,
    /// Same, worst in the ring.
    pub echo_max: Duration,
    /// The echo's hops: key → its echo's batch left the link (the worker's round trip and the
    /// hand-over to the UI thread), → the frame applied to the grid, → painted, → submitted to
    /// the GPU, → on the glass.
    pub echo_hops: [Spread; 5],
    /// Keys the local-echo overlay showed before the worker answered.
    pub predicted: u64,
    /// Keys the predictor guessed at whose echo was on the first frame painted after them, so
    /// that frame showed the echo and no guess.
    pub echo_first: u64,
    /// Keys the predictor guessed at that a frame painted after them showed neither guessed nor
    /// echoed.
    pub guess_late: u64,
    /// Key → glass of the prediction, median.
    pub predicted_p50: Duration,
    /// Same, 95th percentile.
    pub predicted_p95: Duration,
    /// Same, 99th percentile.
    pub predicted_p99: Duration,
    /// Same, worst in the ring.
    pub predicted_max: Duration,
    /// The prediction's hops: key → the guess painted, → submitted, → on the glass.
    pub predicted_hops: [Spread; 3],
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    seq: u64,
    at: Instant,
    /// The predictor made a guess for it that the overlay would draw.
    guessed: bool,
    predicted: bool,
    /// A frame painted after it showed neither its guess nor its echo.
    missed: bool,
    /// The first frame acknowledging the key: when its batch left the link, and when it was
    /// applied.
    applied: Option<(Instant, Instant)>,
}

/// The meter.
#[derive(Debug, Default)]
pub struct KeyLatency {
    pending: VecDeque<Pending>,
    echo: VecDeque<Duration>,
    predicted: VecDeque<Duration>,
    echo_hops: [VecDeque<Duration>; 5],
    predicted_hops: [VecDeque<Duration>; 3],
    echoed_count: u64,
    predicted_count: u64,
    echo_first_count: u64,
    guess_late_count: u64,
}

impl KeyLatency {
    /// A key with sequence number `seq` left for the worker at `now`; `guessed` when the
    /// local-echo overlay has a guess of it to draw.
    pub fn pressed(&mut self, seq: u64, now: Instant, guessed: bool) {
        self.pending.push_back(Pending {
            seq,
            at: now,
            guessed,
            predicted: false,
            missed: false,
            applied: None,
        });
        while self.pending.len() > RING {
            self.pending.pop_front();
        }
    }

    /// Whether a key is still waiting for its guess or its echo to be shown.
    #[must_use]
    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// A frame carrying the worker's state after key `input_ack` was applied at `now`; its
    /// batch left the link at `arrived`.
    pub fn applied(&mut self, input_ack: u64, arrived: Instant, now: Instant) {
        for key in &mut self.pending {
            if key.seq <= input_ack && key.applied.is_none() {
                key.applied = Some((arrived.min(now), now));
            }
        }
    }

    /// A frame reached the display: the local-echo overlay showed guesses for the keys in
    /// `guessed` and the picture carries the worker's state after key `input_ack`.
    pub fn presented(&mut self, frame: Shown, guessed: &[u64], input_ack: u64) {
        let mut keep = VecDeque::with_capacity(self.pending.len());
        for mut key in self.pending.drain(..) {
            let age = frame.presented.saturating_duration_since(key.at);
            if age > STALE {
                continue;
            }
            let since = |from: Instant, to: Instant| to.saturating_duration_since(from);
            let tail = [
                since(key.at, frame.painted),
                since(frame.painted, frame.submitted),
                since(frame.submitted, frame.presented),
            ];
            let shown = guessed.contains(&key.seq);
            let echoed = key.seq <= input_ack;
            if key.guessed && !key.predicted && !shown && frame.painted >= key.at {
                if echoed && !key.missed {
                    self.echo_first_count = self.echo_first_count.saturating_add(1);
                } else if !key.missed {
                    key.missed = true;
                    self.guess_late_count = self.guess_late_count.saturating_add(1);
                }
            }
            if !key.predicted && shown {
                key.predicted = true;
                self.predicted_count = self.predicted_count.saturating_add(1);
                push(&mut self.predicted, age);
                for (ring, hop) in self.predicted_hops.iter_mut().zip(tail) {
                    push(ring, hop);
                }
            }
            if echoed {
                self.echoed_count = self.echoed_count.saturating_add(1);
                push(&mut self.echo, age);
                let (arrived, applied) = key.applied.unwrap_or((frame.painted, frame.painted));
                let hops = [
                    since(key.at, arrived),
                    since(arrived, applied),
                    since(applied, frame.painted),
                    since(frame.painted, frame.submitted),
                    since(frame.submitted, frame.presented),
                ];
                for (ring, hop) in self.echo_hops.iter_mut().zip(hops) {
                    push(ring, hop);
                }
            } else {
                keep.push_back(key);
            }
        }
        self.pending = keep;
    }

    /// The counters plus the rings' percentiles.
    #[must_use]
    pub fn stats(&self) -> LatencyStats {
        let echo = sorted(&self.echo);
        let predicted = sorted(&self.predicted);
        LatencyStats {
            echoed: self.echoed_count,
            echo_p50: percentile(&echo, 50),
            echo_p95: percentile(&echo, 95),
            echo_p99: percentile(&echo, 99),
            echo_max: echo.last().copied().unwrap_or_default(),
            echo_hops: self.echo_hops.each_ref().map(spread),
            predicted: self.predicted_count,
            echo_first: self.echo_first_count,
            guess_late: self.guess_late_count,
            predicted_p50: percentile(&predicted, 50),
            predicted_p95: percentile(&predicted, 95),
            predicted_p99: percentile(&predicted, 99),
            predicted_max: predicted.last().copied().unwrap_or_default(),
            predicted_hops: self.predicted_hops.each_ref().map(spread),
        }
    }

    /// Forget everything.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn push(ring: &mut VecDeque<Duration>, d: Duration) {
    if ring.len() == RING {
        ring.pop_front();
    }
    ring.push_back(d);
}

fn sorted(ring: &VecDeque<Duration>) -> Vec<Duration> {
    let mut all: Vec<Duration> = ring.iter().copied().collect();
    all.sort_unstable();
    all
}

fn spread(ring: &VecDeque<Duration>) -> Spread {
    let all = sorted(ring);
    Spread { p50: percentile(&all, 50), p95: percentile(&all, 95) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// A frame painted, submitted and shown all at `at`.
    fn on_glass_at(at: Instant) -> Shown {
        Shown { painted: at, submitted: at, presented: at }
    }

    /// Each hop is the gap between its two stamps, and the hops add up to the whole.
    #[test]
    fn the_hops_of_an_echo_and_a_guess_add_up_to_their_totals() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0, true);
        let guess = Shown { painted: t0 + MS, submitted: t0 + 2 * MS, presented: t0 + 20 * MS };
        m.presented(guess, &[1], 0);
        m.applied(1, t0 + 3 * MS, t0 + 4 * MS);
        let echo =
            Shown { painted: t0 + 15 * MS, submitted: t0 + 16 * MS, presented: t0 + 34 * MS };
        m.presented(echo, &[], 1);
        let s = m.stats();
        let p50 = |hops: &[Spread]| hops.iter().map(|h| h.p50).collect::<Vec<_>>();
        assert_eq!(p50(&s.predicted_hops), [MS, MS, 18 * MS]);
        assert_eq!(s.predicted_p50, 20 * MS);
        assert_eq!(p50(&s.echo_hops), [3 * MS, MS, 11 * MS, MS, 18 * MS]);
        assert_eq!(s.echo_p50, 34 * MS);
    }

    #[test]
    fn a_key_is_echoed_at_the_first_paint_that_acknowledges_it() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0, false);
        m.presented(on_glass_at(t0 + 8 * MS), &[], 0);
        assert_eq!(m.stats().echoed, 0, "the frame on screen predates the key");
        m.presented(on_glass_at(t0 + 24 * MS), &[], 1);
        let s = m.stats();
        assert_eq!(s.echoed, 1);
        assert_eq!(s.echo_p50, 24 * MS);
        assert_eq!(s.echo_max, 24 * MS);
        assert_eq!(s.predicted, 0);
        m.presented(on_glass_at(t0 + 40 * MS), &[], 1);
        assert_eq!(m.stats().echoed, 1, "an answered key is not counted twice");
    }

    #[test]
    fn a_prediction_is_timed_once_and_the_echo_still_follows() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(7, t0, true);
        m.presented(on_glass_at(t0 + 5 * MS), &[7], 6);
        m.presented(on_glass_at(t0 + 21 * MS), &[7], 6);
        m.presented(on_glass_at(t0 + 38 * MS), &[], 7);
        let s = m.stats();
        assert_eq!((s.predicted, s.predicted_p50), (1, 5 * MS));
        assert_eq!((s.echoed, s.echo_p50), (1, 38 * MS));
    }

    /// An arrow pressed while a letter's guess is still on screen is not "predicted": the
    /// overlay shows the letter's sequence, not the arrow's.
    #[test]
    fn a_key_the_predictor_drew_nothing_for_gets_no_predicted_time() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0, true);
        m.presented(on_glass_at(t0 + 4 * MS), &[1], 0);
        m.pressed(2, t0 + 10 * MS, false);
        m.presented(on_glass_at(t0 + 14 * MS), &[1], 0);
        m.presented(on_glass_at(t0 + 30 * MS), &[], 2);
        let s = m.stats();
        assert_eq!((s.predicted, s.predicted_p50, s.predicted_max), (1, 4 * MS, 4 * MS));
        assert_eq!((s.echoed, s.echo_max), (2, 30 * MS));
    }

    /// A guessed key whose echo is on the first frame painted after it counts as the echo
    /// winning, not as a miss; one whose guess is missing from a frame while its echo is still
    /// out is the guess path falling behind, counted once.
    #[test]
    fn an_echo_that_beats_its_guess_is_told_apart_from_a_late_guess() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0 + 2 * MS, true);
        m.presented(on_glass_at(t0 + MS), &[], 0);
        m.presented(on_glass_at(t0 + 4 * MS), &[], 1);
        let s = m.stats();
        assert_eq!((s.predicted, s.echo_first, s.guess_late, s.echoed), (0, 1, 0, 1));

        m.pressed(2, t0 + 10 * MS, true);
        m.presented(on_glass_at(t0 + 12 * MS), &[], 1);
        m.presented(on_glass_at(t0 + 14 * MS), &[], 1);
        m.presented(on_glass_at(t0 + 16 * MS), &[2], 1);
        m.presented(on_glass_at(t0 + 30 * MS), &[], 2);
        let s = m.stats();
        assert_eq!((s.predicted, s.echo_first, s.guess_late, s.echoed), (1, 1, 1, 2));

        m.pressed(3, t0 + 40 * MS, false);
        m.presented(on_glass_at(t0 + 42 * MS), &[], 2);
        m.presented(on_glass_at(t0 + 44 * MS), &[], 3);
        let s = m.stats();
        assert_eq!((s.echo_first, s.guess_late), (1, 1), "an unguessed key is neither");
    }

    #[test]
    fn keys_the_worker_never_answers_go_stale_and_percentiles_span_the_ring() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0, false);
        m.presented(on_glass_at(t0 + STALE + MS), &[], 0);
        assert_eq!(m.stats().echoed, 0);
        assert!(m.pending.is_empty(), "stale key dropped");
        for i in 1..=100_u64 {
            let at = t0 + Duration::from_secs(10) + Duration::from_millis(i * 100);
            m.pressed(i, at, false);
            m.presented(on_glass_at(at + Duration::from_millis(i)), &[], i);
        }
        let s = m.stats();
        assert_eq!(s.echoed, 100);
        assert_eq!(s.echo_p50, 50 * MS);
        assert_eq!(s.echo_p95, 95 * MS);
        assert_eq!(s.echo_max, 100 * MS);
        m.reset();
        assert_eq!(m.stats(), LatencyStats::default());
    }
}
