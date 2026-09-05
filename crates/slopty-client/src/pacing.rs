//! When a decoded frame reaches the screen, and what that cost.
//!
//! The rule is **present on arrival**: a frame goes up on the first paint after it comes out of
//! the decoder, and nothing is ever queued for a later one. A queue would buy smoother spacing at
//! the price of a whole frame of latency on every frame, which is the wrong trade for a remote
//! desktop — the source is a screen, not a film, and a late picture is worse than an unevenly
//! spaced one. So the only decision left is what to do when the decoder is ahead of the display:
//! [`Pacer::offer`] replaces the frame waiting to be painted rather than lining up behind it, and
//! counts the one it dropped.
//!
//! The [`Pacer`] is also the instrument. It keeps a ring of the last [`RING`] presented frames and
//! reports, over that window, how long each took from the arrival of the datagram that completed
//! it to the paint that showed it, how far apart the paints were, and how often the display saw
//! the same picture twice ([`PacingStats::repeats`]) or never saw one at all
//! ([`PacingStats::skipped`]). Those two counters are the double-present / skipped-present
//! pattern; on a steady source matched to the display both stay near zero.
//!
//! Everything here is pure: the caller supplies the clock ([`Clock`]), so the policy is testable
//! without a window.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Presented frames kept for the percentiles: four seconds at 60 fps.
pub const RING: usize = 240;

/// The clock a [`Pacer`] reads. Real in the app, fake in tests.
pub trait Clock: std::fmt::Debug {
    /// Now.
    fn now(&self) -> Instant;
}

/// [`Instant::now`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// How a decoded frame got here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameStamp {
    /// Presentation timestamp, from the host's capture clock; also the frame's identity.
    pub pts_us: u64,
    /// When the datagram that completed the frame arrived.
    pub arrived: Instant,
    /// When the decoder handed the picture back.
    pub decoded: Instant,
}

/// What to do with a frame the decoder just produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pace {
    /// Put it on screen and ask for a redraw.
    Present,
    /// Not newer than what is already up; drop it.
    Drop,
}

/// One presented frame, as the ring remembers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sample {
    /// Arrival of the completing datagram → the paint that showed it.
    latency: Duration,
    /// Arrival → the decoder handing the picture back.
    decode: Duration,
    /// Gap to the previous presented frame; `None` for the first one.
    interval: Option<Duration>,
}

/// What the ring says about the last [`RING`] frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PacingStats {
    /// Frames put on screen, for the lifetime of the stream.
    pub presented: u64,
    /// Frames replaced before they were ever painted: the decoder ran ahead of the display.
    pub skipped: u64,
    /// Paints that showed the picture already up, because no new frame was ready.
    pub repeats: u64,
    /// Frames dropped as not newer than what was already up (reordering, a stale retransmit).
    pub late: u64,
    /// Median arrival → present over the ring.
    pub latency_p50: Duration,
    /// 95th percentile of the same.
    pub latency_p95: Duration,
    /// Worst in the ring.
    pub latency_max: Duration,
    /// Median arrival → decoded over the ring: the part of the latency the decoder owns.
    pub decode_p50: Duration,
    /// Median gap between presented frames.
    pub interval_p50: Duration,
    /// Mean absolute deviation of that gap: the cadence's own jitter. A double- or
    /// skipped-present on an otherwise steady source shows up here before it shows up anywhere
    /// else.
    pub interval_jitter: Duration,
    /// Frames the ring holds.
    pub window: usize,
}

/// Decides when a decoded frame goes on screen and measures what happened.
#[derive(Debug)]
pub struct Pacer<C: Clock = SystemClock> {
    clock: C,
    /// Offered, installed, not yet painted.
    pending: Option<FrameStamp>,
    /// Presentation timestamp of the picture on screen.
    shown: Option<u64>,
    /// When the picture on screen went up.
    shown_at: Option<Instant>,
    ring: VecDeque<Sample>,
    stats: PacingStats,
}

impl Default for Pacer<SystemClock> {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl<C: Clock> Pacer<C> {
    /// A pacer reading `clock`.
    #[must_use]
    pub fn new(clock: C) -> Self {
        Self {
            clock,
            pending: None,
            shown: None,
            shown_at: None,
            ring: VecDeque::with_capacity(RING),
            stats: PacingStats::default(),
        }
    }

    /// A decoded frame is available. `Present` means install it now and ask for a redraw;
    /// nothing is ever held back for a later paint.
    pub fn offer(&mut self, stamp: FrameStamp) -> Pace {
        let newest = self.pending.map_or(self.shown, |p| Some(p.pts_us));
        if newest.is_some_and(|last| stamp.pts_us <= last) {
            self.stats.late = self.stats.late.saturating_add(1);
            return Pace::Drop;
        }
        if self.pending.is_some() {
            // The one it replaces was installed but never painted.
            self.stats.skipped = self.stats.skipped.saturating_add(1);
        }
        self.pending = Some(stamp);
        Pace::Present
    }

    /// The element painted. Records what the paint showed; call once per paint.
    pub fn presented(&mut self) {
        let now = self.clock.now();
        let Some(stamp) = self.pending.take() else {
            self.stats.repeats = self.stats.repeats.saturating_add(1);
            return;
        };
        let sample = Sample {
            latency: now.saturating_duration_since(stamp.arrived),
            decode: stamp.decoded.saturating_duration_since(stamp.arrived),
            interval: self.shown_at.map(|t| now.saturating_duration_since(t)),
        };
        if self.ring.len() >= RING {
            self.ring.pop_front();
        }
        self.ring.push_back(sample);
        self.shown = Some(stamp.pts_us);
        self.shown_at = Some(now);
        self.stats.presented = self.stats.presented.saturating_add(1);
    }

    /// Age of the picture on screen: how long ago the paint that put it up happened.
    #[must_use]
    pub fn age(&self) -> Option<Duration> {
        self.shown_at.map(|t| self.clock.now().saturating_duration_since(t))
    }

    /// The counters plus the ring's percentiles.
    #[must_use]
    pub fn stats(&self) -> PacingStats {
        let mut latency: Vec<Duration> = self.ring.iter().map(|s| s.latency).collect();
        let mut decode: Vec<Duration> = self.ring.iter().map(|s| s.decode).collect();
        let mut interval: Vec<Duration> = self.ring.iter().filter_map(|s| s.interval).collect();
        latency.sort_unstable();
        decode.sort_unstable();
        interval.sort_unstable();
        let interval_p50 = percentile(&interval, 50);
        PacingStats {
            latency_p50: percentile(&latency, 50),
            latency_p95: percentile(&latency, 95),
            latency_max: latency.last().copied().unwrap_or_default(),
            decode_p50: percentile(&decode, 50),
            interval_p50,
            interval_jitter: mean_deviation(&interval, interval_p50),
            window: self.ring.len(),
            ..self.stats
        }
    }
}

/// The `p`th percentile of a sorted slice, or zero when it is empty.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let index = sorted.len().saturating_mul(p) / 100;
    let index = index.min(sorted.len().saturating_sub(1));
    sorted.get(index).copied().unwrap_or_default()
}

/// Mean absolute deviation of `values` from `centre`.
fn mean_deviation(values: &[Duration], centre: Duration) -> Duration {
    let count = u32::try_from(values.len()).unwrap_or(u32::MAX);
    if count == 0 {
        return Duration::ZERO;
    }
    let total =
        values.iter().fold(Duration::ZERO, |acc, &v| acc.saturating_add(v.abs_diff(centre)));
    total.checked_div(count).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// A clock the test moves by hand.
    #[derive(Debug)]
    struct FakeClock {
        epoch: Instant,
        offset: Cell<Duration>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self { epoch: Instant::now(), offset: Cell::new(Duration::ZERO) }
        }

        fn advance(&self, by: Duration) {
            self.offset.set(self.offset.get().saturating_add(by));
        }

        fn at(&self, offset: Duration) -> Instant {
            self.epoch.checked_add(offset).expect("instant in range")
        }
    }

    impl Clock for &FakeClock {
        fn now(&self) -> Instant {
            self.at(self.offset.get())
        }
    }

    const FRAME: Duration = Duration::from_micros(16_667);
    const MS: Duration = Duration::from_millis(1);

    /// One arrival, one decode, one paint, `at` after the clock's epoch.
    fn stamp(clock: &FakeClock, index: u32, arrived: Duration, decode: Duration) -> FrameStamp {
        FrameStamp {
            pts_us: u64::from(index).saturating_mul(16_667),
            arrived: clock.at(arrived),
            decoded: clock.at(arrived.saturating_add(decode)),
        }
    }

    /// A steady 60 fps source painted at 60 Hz: every frame goes up on the paint that follows
    /// it, one interval apart, and nothing is skipped or repeated.
    #[test]
    fn a_steady_source_presents_every_frame_once() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        for i in 0..120 {
            let arrived = FRAME.saturating_mul(i);
            clock.advance(FRAME);
            assert_eq!(pacer.offer(stamp(&clock, i, arrived, 2 * MS)), Pace::Present);
            pacer.presented();
        }
        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.repeats, stats.late), (120, 0, 0, 0));
        assert_eq!(stats.window, RING.min(120));
        assert_eq!(stats.interval_p50, FRAME);
        assert_eq!(stats.interval_jitter, Duration::ZERO);
        // Arrival → present is the decode plus the wait for the paint; no extra frame of it.
        assert!(stats.latency_p95 < FRAME.saturating_add(2 * MS), "{stats:?}");
    }

    /// Two frames between paints: the newer one is shown, the older is counted skipped and never
    /// queued for the next paint (which is what an extra frame of buffering would look like).
    #[test]
    fn a_frame_decoded_between_paints_replaces_the_one_waiting() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        assert_eq!(pacer.offer(stamp(&clock, 0, Duration::ZERO, MS)), Pace::Present);
        pacer.presented();

        // Two decodes land before the next paint.
        assert_eq!(pacer.offer(stamp(&clock, 1, FRAME, MS)), Pace::Present);
        assert_eq!(pacer.offer(stamp(&clock, 2, FRAME + 8 * MS, MS)), Pace::Present);
        clock.advance(FRAME);
        pacer.presented();

        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.repeats), (2, 1, 0));
        // Two paints, two samples: the frame that was replaced never reached the ring.
        assert_eq!(stats.window, 2);
        // The second paint showed frame 2, so its latency is measured from *its* arrival — a
        // frame interval less eight milliseconds, which is under frame 0's whole interval.
        assert_eq!(stats.latency_max, FRAME);
        assert_eq!(stats.latency_p50, FRAME);
        // Nothing is left over: the next paint has no new frame, so it repeats.
        pacer.presented();
        assert_eq!(pacer.stats().repeats, 1);
    }

    /// A source slower than the display repeats pictures instead of holding paints back, and a
    /// frame that is not newer than what is up is dropped rather than shown out of order.
    #[test]
    fn a_slow_source_repeats_and_a_late_frame_is_dropped() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        clock.advance(FRAME);
        pacer.offer(stamp(&clock, 4, Duration::ZERO, MS));
        pacer.presented();
        // Three paints with nothing new.
        for _ in 0..3 {
            clock.advance(FRAME);
            pacer.presented();
        }
        // A straggler from before the picture on screen.
        assert_eq!(pacer.offer(stamp(&clock, 3, 4 * FRAME, MS)), Pace::Drop);
        // …and the same frame again.
        assert_eq!(pacer.offer(stamp(&clock, 4, 4 * FRAME, MS)), Pace::Drop);
        clock.advance(FRAME);
        pacer.presented();

        let stats = pacer.stats();
        assert_eq!((stats.presented, stats.skipped, stats.repeats, stats.late), (1, 0, 4, 2));
        assert_eq!(stats.interval_p50, Duration::ZERO, "one frame has no interval");
    }

    /// An uneven cadence (one paint missed in eight) shows up as interval jitter, which is what
    /// the overlay reads.
    #[test]
    fn a_missed_paint_shows_as_interval_jitter() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        let mut arrived = Duration::ZERO;
        for i in 0..64 {
            let step = if i % 8 == 7 { 2 * FRAME } else { FRAME };
            arrived = arrived.saturating_add(step);
            clock.advance(step);
            pacer.offer(stamp(&clock, i, arrived, MS));
            pacer.presented();
        }
        let stats = pacer.stats();
        assert_eq!(stats.interval_p50, FRAME);
        // One gap in eight is a frame long, so the mean deviation is about an eighth of one.
        let eighth = FRAME.checked_div(8).expect("nonzero");
        assert!(
            stats.interval_jitter > eighth.checked_div(2).expect("nonzero")
                && stats.interval_jitter < eighth.saturating_mul(2),
            "{:?} is not about {eighth:?}",
            stats.interval_jitter
        );
    }

    /// The ring forgets: only the last `RING` frames are in the percentiles.
    #[test]
    fn the_ring_keeps_the_last_frames_only() {
        let clock = FakeClock::new();
        let mut pacer = Pacer::new(&clock);
        for i in 0..(u32::try_from(RING).expect("small") + 40) {
            let arrived = FRAME.saturating_mul(i);
            // The first frames are slow, the last ones fast.
            let decode = if i < 100 { 50 * MS } else { MS };
            clock.advance(FRAME);
            pacer.offer(stamp(&clock, i, arrived, decode));
            pacer.presented();
        }
        let stats = pacer.stats();
        assert_eq!(stats.window, RING);
        assert_eq!(stats.presented, u64::try_from(RING).expect("small") + 40);
        assert_eq!(stats.decode_p50, MS, "the slow start has fallen out of the ring");
    }
}
