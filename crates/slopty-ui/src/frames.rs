//! UI frame-time probe: how long each frame takes to draw and how evenly frames arrive.
//!
//! The root view calls [`begin`] first thing in its `render`; a zero-size [`probe`] element,
//! the last child of the root, calls [`end`] from its paint. The span between them is the
//! frame's draw: every `render`, layout, prepaint (terminal shaping) and paint the window did.
//! Consecutive `begin`s give the frame interval. A draw that runs past the display period
//! costs the slots it ran through: that is the dropped-frame count (intervals cannot tell a
//! drop from an app idling between keystrokes, so they are reported, not judged). Nothing
//! here touches a clock: the callers pass `now`, so the ring and the percentiles are checked
//! with hand-made instants.
//!
//! The probe lives on the [`App`] as a global (one window per app), so the stats overlay and
//! the self-test `dump` read the same numbers.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::{App, IntoElement as _, Styled as _, canvas};
use slopty_client::pacing::percentile;

/// Frames kept for the percentiles: sixteen seconds at 60 Hz, eight at 120 Hz.
pub const RING: usize = 1024;

/// A gap between frames longer than this is the app idling (nothing invalidated): it is left
/// out of the interval percentiles.
pub const IDLE: Duration = Duration::from_millis(250);

/// The display period the app is measured against (`SLOPTY_FRAME_HZ` overrides it): 60 Hz on
/// the Mac Studio's display, 120 Hz `ProMotion` on the iPad Pro and iPhone Pro.
#[must_use]
pub fn default_nominal() -> Duration {
    let hz = if cfg!(target_os = "ios") { 120.0 } else { 60.0 };
    Duration::from_secs_f64(1.0 / hz)
}

/// Percentiles and counters over the last [`RING`] frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameStats {
    /// Frames drawn since the last reset (all of them, not only those in the ring).
    pub frames: u64,
    /// Frames whose draw took longer than the display period.
    pub over_budget: u64,
    /// Display slots lost to draws that ran past the period (a 30 ms draw at 60 Hz loses one).
    pub dropped: u64,
    /// Draw duration, median.
    pub draw_p50: Duration,
    /// Draw duration, 95th percentile.
    pub draw_p95: Duration,
    /// Draw duration, 99th percentile.
    pub draw_p99: Duration,
    /// Worst draw in the ring.
    pub draw_max: Duration,
    /// Interval between consecutive frames, median.
    pub interval_p50: Duration,
    /// Interval, 95th percentile.
    pub interval_p95: Duration,
    /// Interval, 99th percentile.
    pub interval_p99: Duration,
    /// The display period the counters are measured against.
    pub nominal: Duration,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    draw: Duration,
    /// Since the previous frame's start; `None` after an idle gap or for the first frame.
    interval: Option<Duration>,
}

/// The ring and its counters.
#[derive(Debug)]
pub struct FrameProbe {
    nominal: Duration,
    ring: VecDeque<Sample>,
    /// The frame being drawn: its `begin` and the interval since the one before.
    open: Option<(Instant, Option<Duration>)>,
    last_begin: Option<Instant>,
    frames: u64,
    over_budget: u64,
    dropped: u64,
    /// Frames begun since the probe was made; never reset, so it can stamp per-frame work.
    begun: u64,
}

impl FrameProbe {
    /// A probe measured against `nominal` (the display period).
    #[must_use]
    pub fn new(nominal: Duration) -> Self {
        Self {
            nominal,
            ring: VecDeque::with_capacity(RING),
            open: None,
            last_begin: None,
            frames: 0,
            over_budget: 0,
            dropped: 0,
            begun: 0,
        }
    }

    /// The display period.
    #[must_use]
    pub const fn nominal(&self) -> Duration {
        self.nominal
    }

    /// A frame starts drawing.
    pub fn begin(&mut self, now: Instant) {
        let interval = self
            .last_begin
            .map(|last| now.saturating_duration_since(last))
            .filter(|gap| *gap < IDLE);
        self.last_begin = Some(now);
        self.open = Some((now, interval));
        self.begun = self.begun.wrapping_add(1);
    }

    /// Frames begun so far (never reset).
    #[must_use]
    pub const fn index(&self) -> u64 {
        self.begun
    }

    /// The frame that began last has been painted.
    pub fn end(&mut self, now: Instant) {
        let Some((start, interval)) = self.open.take() else { return };
        let draw = now.saturating_duration_since(start);
        self.frames = self.frames.saturating_add(1);
        if draw > self.nominal {
            self.over_budget = self.over_budget.saturating_add(1);
            // Slots the display showed the old picture in while this draw ran.
            let slots = (draw.as_secs_f64() / self.nominal.as_secs_f64()).floor();
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 1")]
            let lost = slots as u64;
            self.dropped = self.dropped.saturating_add(lost);
        }
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(Sample { draw, interval });
    }

    /// Forget everything; the next frame starts a new window (no interval).
    pub fn reset(&mut self) {
        self.ring.clear();
        self.open = None;
        self.last_begin = None;
        self.frames = 0;
        self.over_budget = 0;
        self.dropped = 0;
    }

    /// The counters plus the ring's percentiles.
    #[must_use]
    pub fn stats(&self) -> FrameStats {
        let mut draw: Vec<Duration> = self.ring.iter().map(|s| s.draw).collect();
        let mut interval: Vec<Duration> = self.ring.iter().filter_map(|s| s.interval).collect();
        draw.sort_unstable();
        interval.sort_unstable();
        FrameStats {
            frames: self.frames,
            over_budget: self.over_budget,
            dropped: self.dropped,
            draw_p50: percentile(&draw, 50),
            draw_p95: percentile(&draw, 95),
            draw_p99: percentile(&draw, 99),
            draw_max: draw.last().copied().unwrap_or_default(),
            interval_p50: percentile(&interval, 50),
            interval_p95: percentile(&interval, 95),
            interval_p99: percentile(&interval, 99),
            nominal: self.nominal,
        }
    }
}

/// The app's probe.
struct Probe(FrameProbe);

impl gpui::Global for Probe {}

/// Put a probe on the app, measured against `nominal`. Call once, before the window opens.
pub fn install(cx: &mut App, nominal: Duration) {
    cx.set_global(Probe(FrameProbe::new(nominal)));
}

/// The index of the frame being drawn, when the probe is installed: work that must happen
/// once per frame (a cache sweep) compares it with the last one it saw.
#[must_use]
pub fn index(cx: &App) -> Option<u64> {
    cx.try_global::<Probe>().map(|probe| probe.0.index())
}

/// First line of the root view's `render`.
pub fn begin(cx: &mut App) {
    if cx.has_global::<Probe>() {
        let now = Instant::now();
        cx.global_mut::<Probe>().0.begin(now);
    }
}

/// What the [`probe`] element calls from its paint.
pub fn end(cx: &mut App) {
    if cx.has_global::<Probe>() {
        let now = Instant::now();
        cx.global_mut::<Probe>().0.end(now);
    }
}

/// The current numbers, when a probe is installed.
#[must_use]
pub fn stats(cx: &App) -> Option<FrameStats> {
    cx.try_global::<Probe>().map(|p| p.0.stats())
}

/// Start a fresh measurement window.
pub fn reset(cx: &mut App) {
    if cx.has_global::<Probe>() {
        cx.global_mut::<Probe>().0.reset();
    }
}

/// The zero-size element that closes each frame's measurement: make it the root's last child.
#[must_use]
pub fn probe() -> gpui::AnyElement {
    canvas(|_bounds, _window, _cx| (), |_bounds, (), _window, cx| end(cx))
        .absolute()
        .size_0()
        .into_any_element()
}

/// One line for the stats overlay: draw percentiles, cadence and the two counters.
#[must_use]
pub fn hud_line(stats: Option<&FrameStats>) -> String {
    let Some(s) = stats else { return "ui –".to_owned() };
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    format!(
        "ui draw {:.1} / {:.1} / {:.1} ms (max {:.1})  ·  every {:.1} / {:.1} ms  ·  {} frames, {} over {:.1} ms, {} dropped",
        ms(s.draw_p50),
        ms(s.draw_p95),
        ms(s.draw_p99),
        ms(s.draw_max),
        ms(s.interval_p50),
        ms(s.interval_p95),
        s.frames,
        s.over_budget,
        ms(s.nominal),
        s.dropped,
    )
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "hand-made instants and small counts"
)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);
    const MS: Duration = Duration::from_millis(1);

    /// `n` frames at a steady cadence, each drawn in `draw`.
    fn steady(probe: &mut FrameProbe, t0: Instant, n: u32, draw: Duration) -> Instant {
        let mut t = t0;
        for _ in 0..n {
            probe.begin(t);
            probe.end(t + draw);
            t += FRAME;
        }
        t
    }

    #[test]
    fn a_steady_run_has_no_drops_and_reports_its_draw_and_interval() {
        let mut probe = FrameProbe::new(FRAME);
        let t0 = Instant::now();
        steady(&mut probe, t0, 300, 4 * MS);
        let s = probe.stats();
        assert_eq!(s.frames, 300);
        assert_eq!(s.dropped, 0);
        assert_eq!(s.over_budget, 0);
        assert_eq!(s.draw_p50, 4 * MS);
        assert_eq!(s.draw_p99, 4 * MS);
        assert_eq!(s.draw_max, 4 * MS);
        assert_eq!(s.interval_p50, FRAME);
        assert_eq!(s.interval_p99, FRAME);
        assert_eq!(s.nominal, FRAME);
    }

    #[test]
    fn a_slow_frame_counts_over_budget_and_the_slot_it_ran_through_is_a_drop() {
        let mut probe = FrameProbe::new(FRAME);
        let t0 = Instant::now();
        let t = steady(&mut probe, t0, 100, 4 * MS);
        // One 30 ms draw: the next frame can only start two periods later.
        probe.begin(t);
        probe.end(t + 30 * MS);
        let t = t + 2 * FRAME;
        steady(&mut probe, t, 100, 4 * MS);
        let s = probe.stats();
        assert_eq!(s.frames, 201);
        assert_eq!(s.over_budget, 1);
        assert_eq!(s.dropped, 1, "{s:?}");
        assert_eq!(s.draw_max, 30 * MS);
        assert_eq!(s.draw_p50, 4 * MS);
        assert_eq!(s.draw_p99, 4 * MS, "1 of 201 is below the top 1 % (nearest rank)");
        assert_eq!(s.interval_p99, FRAME, "one long interval in 200 is below the top 1 %");
    }

    #[test]
    fn an_idle_gap_is_not_an_interval() {
        let mut probe = FrameProbe::new(FRAME);
        let t0 = Instant::now();
        let t = steady(&mut probe, t0, 10, 2 * MS);
        let t = t + Duration::from_secs(3);
        steady(&mut probe, t, 10, 2 * MS);
        let s = probe.stats();
        assert_eq!(s.frames, 20);
        assert_eq!(s.dropped, 0);
        assert_eq!(s.interval_p99, FRAME, "the 3 s gap is not in the ring");
    }

    #[test]
    fn the_ring_forgets_and_reset_starts_over() {
        let mut probe = FrameProbe::new(FRAME);
        let t0 = Instant::now();
        let t = steady(&mut probe, t0, 5, 40 * MS);
        // 40 ms draws every 16.7 ms is impossible on a display, but the ring does not care.
        steady(&mut probe, t, RING as u32, 3 * MS);
        let s = probe.stats();
        assert_eq!(s.draw_max, 3 * MS, "the slow start fell out of the ring");
        assert_eq!(s.frames, 5 + RING as u64, "the counter keeps every frame");
        assert_eq!(s.over_budget, 5);
        assert_eq!(s.dropped, 10, "each 40 ms draw ran through two 16.7 ms slots");
        probe.reset();
        let s = probe.stats();
        assert_eq!(s, FrameStats { nominal: FRAME, ..FrameStats::default() });
        probe.begin(t0);
        probe.end(t0 + MS);
        assert_eq!(probe.stats().interval_p50, Duration::ZERO, "no interval across a reset");
    }

    #[test]
    fn an_end_without_a_begin_is_ignored() {
        let mut probe = FrameProbe::new(FRAME);
        probe.end(Instant::now());
        assert_eq!(probe.stats().frames, 0);
    }

    #[test]
    fn the_hud_line_reads_the_numbers_back() {
        assert_eq!(hud_line(None), "ui –");
        let mut probe = FrameProbe::new(FRAME);
        steady(&mut probe, Instant::now(), 60, 5 * MS);
        let line = hud_line(Some(&probe.stats()));
        assert!(line.starts_with("ui draw 5.0 / 5.0 / 5.0 ms (max 5.0)"), "{line}");
        assert!(line.contains("every 16.7 / 16.7 ms"), "{line}");
        assert!(line.ends_with("60 frames, 0 over 16.7 ms, 0 dropped"), "{line}");
    }
}
