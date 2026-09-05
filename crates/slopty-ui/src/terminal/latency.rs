//! Keystroke → paint: how long a typed key takes to show, predicted and echoed.
//!
//! The view records each key it sends ([`KeyLatency::pressed`]) and the element reports every
//! paint ([`KeyLatency::painted`]) with two facts: whether the local-echo overlay is showing
//! and which key the host had applied in the frame on screen (`Frame::input_ack`). The first
//! paint with the overlay up after a key is that key's *predicted* time; the first paint whose
//! frame acknowledges it is its *echoed* time. Pure: callers pass `now`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Keys kept for the percentiles.
pub const RING: usize = 256;

/// A key unanswered for this long fell off the shell (a control key, a dead prompt): dropped.
pub const STALE: Duration = Duration::from_secs(5);

/// Percentiles and counts over the last [`RING`] keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LatencyStats {
    /// Keys whose echo from the host has been painted.
    pub echoed: u64,
    /// Key → paint of the host's echo, median.
    pub echo_p50: Duration,
    /// Same, 95th percentile.
    pub echo_p95: Duration,
    /// Same, worst in the ring.
    pub echo_max: Duration,
    /// Keys the local-echo overlay showed before the host answered.
    pub predicted: u64,
    /// Key → paint of the prediction, median.
    pub predicted_p50: Duration,
    /// Same, 95th percentile.
    pub predicted_p95: Duration,
    /// Same, worst in the ring.
    pub predicted_max: Duration,
}

#[derive(Clone, Copy, Debug)]
struct Pending {
    seq: u64,
    at: Instant,
    predicted: bool,
}

/// The meter.
#[derive(Debug, Default)]
pub struct KeyLatency {
    pending: VecDeque<Pending>,
    echo: VecDeque<Duration>,
    predicted: VecDeque<Duration>,
    echoed_count: u64,
    predicted_count: u64,
}

impl KeyLatency {
    /// A key with sequence number `seq` left for the host at `now`.
    pub fn pressed(&mut self, seq: u64, now: Instant) {
        self.pending.push_back(Pending { seq, at: now, predicted: false });
        while self.pending.len() > RING {
            self.pending.pop_front();
        }
    }

    /// A frame was painted at `now`: the overlay was `predicting` and the picture on screen
    /// carries the host's state after key `input_ack`.
    pub fn painted(&mut self, now: Instant, predicting: bool, input_ack: u64) {
        let mut keep = VecDeque::with_capacity(self.pending.len());
        for mut key in self.pending.drain(..) {
            let age = now.saturating_duration_since(key.at);
            if age > STALE {
                continue;
            }
            if predicting && !key.predicted {
                key.predicted = true;
                self.predicted_count = self.predicted_count.saturating_add(1);
                push(&mut self.predicted, age);
            }
            if key.seq <= input_ack {
                self.echoed_count = self.echoed_count.saturating_add(1);
                push(&mut self.echo, age);
            } else {
                keep.push_back(key);
            }
        }
        self.pending = keep;
    }

    /// The counters plus the rings' percentiles.
    #[must_use]
    pub fn stats(&self) -> LatencyStats {
        let (mut echo, mut predicted): (Vec<Duration>, Vec<Duration>) =
            (self.echo.iter().copied().collect(), self.predicted.iter().copied().collect());
        echo.sort_unstable();
        predicted.sort_unstable();
        LatencyStats {
            echoed: self.echoed_count,
            echo_p50: percentile(&echo, 50),
            echo_p95: percentile(&echo, 95),
            echo_max: echo.last().copied().unwrap_or_default(),
            predicted: self.predicted_count,
            predicted_p50: percentile(&predicted, 50),
            predicted_p95: percentile(&predicted, 95),
            predicted_max: predicted.last().copied().unwrap_or_default(),
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

/// The `p`th percentile of a sorted slice (nearest rank), or zero when it is empty.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = p.saturating_mul(sorted.len()).div_ceil(100).max(1);
    sorted.get(rank.saturating_sub(1)).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_key_is_echoed_at_the_first_paint_that_acknowledges_it() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0);
        m.painted(t0 + 8 * MS, false, 0);
        assert_eq!(m.stats().echoed, 0, "the frame on screen predates the key");
        m.painted(t0 + 24 * MS, false, 1);
        let s = m.stats();
        assert_eq!(s.echoed, 1);
        assert_eq!(s.echo_p50, 24 * MS);
        assert_eq!(s.echo_max, 24 * MS);
        assert_eq!(s.predicted, 0);
        m.painted(t0 + 40 * MS, false, 1);
        assert_eq!(m.stats().echoed, 1, "an answered key is not counted twice");
    }

    #[test]
    fn a_prediction_is_timed_once_and_the_echo_still_follows() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(7, t0);
        m.painted(t0 + 5 * MS, true, 6);
        m.painted(t0 + 21 * MS, true, 6);
        m.painted(t0 + 38 * MS, false, 7);
        let s = m.stats();
        assert_eq!((s.predicted, s.predicted_p50), (1, 5 * MS));
        assert_eq!((s.echoed, s.echo_p50), (1, 38 * MS));
    }

    #[test]
    fn keys_the_host_never_answers_go_stale_and_percentiles_span_the_ring() {
        let mut m = KeyLatency::default();
        let t0 = Instant::now();
        m.pressed(1, t0);
        m.painted(t0 + STALE + MS, false, 0);
        assert_eq!(m.stats().echoed, 0);
        assert!(m.pending.is_empty(), "stale key dropped");
        for i in 1..=100_u64 {
            let at = t0 + Duration::from_secs(10) + Duration::from_millis(i * 100);
            m.pressed(i, at);
            m.painted(at + Duration::from_millis(i), false, i);
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
