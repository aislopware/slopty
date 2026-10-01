//! How a stream is doing, said two ways: the stats overlay's one plain line (the figures a
//! person reads, as Parsec's and Jump's overlays lead with them), and the header's health mark,
//! which says one word only while something is wrong.
//!
//! The frame rate is never flagged: a still window sends no frames, so a low rate is not a
//! fault. Frames that miss the display are, and so is a picture that takes long to show.

use std::time::{Duration, Instant};

use slopty_client::ScreenStats;
#[cfg(test)]
use slopty_client::pacing::Spread;
use slopty_client::pacing::{PacingStats, PaintRate};
use slopty_proto::screen::RateVerdict;

use super::HudInput;

/// A round trip slow enough to wear the warning tone, in the overlay and the status bar: typing
/// lags behind the fingers.
pub const RTT_WARN_FROM: Duration = Duration::from_millis(150);

/// How long the worker has to keep cutting the bitrate before the header says so: a cut on one
/// loss burst is the controller working, not a starved link.
pub const CUT_FOR: Duration = Duration::from_secs(3);

/// Frames a second that miss the display (late or never shown) from which the header says so.
pub const LATE_PER_SECOND: f64 = 5.0;

/// One figure of the overlay's plain line, and whether it is past its threshold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Figure {
    /// The figure as said: "60 fps", "12 ms to glass".
    pub text: String,
    /// Past its threshold: drawn in the warning tone.
    pub warn: bool,
}

/// The overlay's plain line: the painted rate, how long a frame takes to reach the glass
/// (p50), the bitrate received and the round trip, which is flagged from [`RTT_WARN_FROM`].
/// Frames that missed the display in the last second follow the rate as a figure of their own,
/// in the warning tone, and only while there are any: they are never in the rate.
///
/// "To glass" is from the capture on the worker once the stream's clock probes have placed the
/// worker's clock, the number a remote desktop is judged on, and flagged when its p95 passes
/// three display periods and half the round trip: one period each for the capture, the codec
/// and the display, and the path's one way. Until then it is from the arrival of the frame's
/// last datagram, flagged past two display periods.
#[must_use]
pub fn summary(input: &HudInput<'_>) -> Vec<Figure> {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let period = Duration::from_secs(1).checked_div(u32::from(input.target_fps.max(1)));
    let one_way = input.rtt.and_then(|rtt| rtt.checked_div(2)).unwrap_or_default();
    let captured = input.capture.count > 0 && input.stats.clock.is_some();
    let (p50, slow) = if captured {
        let limit = period.map(|p| p.saturating_mul(3).saturating_add(one_way));
        (input.capture.p50, limit.is_some_and(|limit| input.capture.p95 > limit))
    } else {
        let limit = period.map(|p| p.saturating_mul(2));
        (input.pacing.latency_p50, limit.is_some_and(|limit| input.pacing.latency_p95 > limit))
    };
    let presented = input.pacing.presented > 0;
    let glass = if presented {
        format!("{:.0} ms to glass", ms(p50))
    } else {
        "\u{2013} to glass".to_owned()
    };
    let rate = if input.mbps < 10.0 {
        format!("{:.1} Mb/s", input.mbps)
    } else {
        format!("{:.0} Mb/s", input.mbps)
    };
    let rtt =
        input.rtt.map_or_else(|| "RTT \u{2013}".to_owned(), |d| format!("RTT {:.1} ms", ms(d)));
    let missed = input.paint.missed;
    let late = (missed > 0).then(|| Figure { text: format!("{missed} late"), warn: true });
    std::iter::once(Figure { text: fps_label(input.paint), warn: false })
        .chain(late)
        .chain([
            Figure { text: glass, warn: presented && slow },
            Figure { text: rate, warn: false },
            Figure { text: rtt, warn: input.rtt.is_some_and(|d| d >= RTT_WARN_FROM) },
        ])
        .collect()
}

/// How every readout says a stream's rate: the pictures painted on this client in the last
/// second.
#[must_use]
pub fn fps_label(rate: PaintRate) -> String {
    format!("{} fps", rate.painted)
}

/// What is wrong with a stream, worst first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    /// The link has stopped delivering.
    Stalled,
    /// The worker has cut the bitrate for three seconds.
    LowBandwidth,
    /// Frames are missing the display, more than five a second.
    FramesLate,
}

impl Health {
    /// The header's one word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stalled => "Stalled",
            Self::LowBandwidth => "Low bandwidth",
            Self::FramesLate => "Frames late",
        }
    }
}

/// What the health mark reads: the stream's counters, sampled once a second.
#[derive(Clone, Copy, Debug, Default)]
pub struct Probe {
    /// When the counters were last read, and how many frames had missed the display by then.
    last: Option<(Instant, u64)>,
    /// Since when the worker's verdict has been a cut.
    cut_since: Option<Instant>,
}

impl Probe {
    /// The worker's latest verdict, at `now`.
    pub fn verdict(&mut self, verdict: RateVerdict, now: Instant) {
        if verdict == RateVerdict::Cut {
            self.cut_since.get_or_insert(now);
        } else {
            self.cut_since = None;
        }
    }

    /// Read the counters at `now`: what is wrong, if anything.
    pub fn read(
        &mut self,
        stats: &ScreenStats,
        pacing: &PacingStats,
        now: Instant,
    ) -> Option<Health> {
        let missed = pacing.late.saturating_add(pacing.skipped);
        let late = self.last.replace((now, missed)).is_some_and(|(at, before)| {
            let secs = now.saturating_duration_since(at).as_secs_f64();
            #[expect(clippy::cast_precision_loss, reason = "frames missed in a second or so")]
            let rate = missed.saturating_sub(before) as f64 / secs.max(f64::EPSILON);
            secs > 0.0 && rate > LATE_PER_SECOND
        });
        let cut =
            self.cut_since.is_some_and(|since| now.saturating_duration_since(since) >= CUT_FOR);
        if stats.stalled {
            Some(Health::Stalled)
        } else if cut {
            Some(Health::LowBandwidth)
        } else if late {
            Some(Health::FramesLate)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(stats: &'a ScreenStats, pacing: &'a PacingStats) -> HudInput<'a> {
        HudInput {
            size: (1920, 1080),
            scale: 1.0,
            chroma: None,
            seams: None,
            target_fps: 60,
            paint: PaintRate { painted: 60, missed: 0 },
            mbps: 18.25,
            rtt: Some(Duration::from_micros(1_700)),
            frame_age: None,
            rate: None,
            stats,
            pacing,
            capture: &NO_CAPTURE,
            ui: None,
        }
    }

    const NO_CAPTURE: Spread =
        Spread { p50: Duration::ZERO, p95: Duration::ZERO, max: Duration::ZERO, count: 0 };

    /// The plain line says the rate, the time to glass, the bitrate and the round trip; a
    /// frame slower than two display periods at p95 and a round trip from 150 ms are flagged,
    /// a low frame rate never is. Frames that missed the display are a flagged figure of their
    /// own after the rate, which they are never in.
    #[test]
    fn the_plain_line_leads_with_the_human_numbers_and_flags_trouble() {
        let stats = ScreenStats::default();
        let mut pacing = PacingStats {
            presented: 600,
            latency_p50: Duration::from_micros(12_300),
            latency_p95: Duration::from_millis(20),
            ..PacingStats::default()
        };
        let said = |figures: &[Figure]| {
            figures.iter().map(|f| (f.text.clone(), f.warn)).collect::<Vec<_>>()
        };
        let line = summary(&input(&stats, &pacing));
        assert_eq!(
            said(&line),
            [
                ("60 fps".to_owned(), false),
                ("12 ms to glass".to_owned(), false),
                ("18 Mb/s".to_owned(), false),
                ("RTT 1.7 ms".to_owned(), false),
            ]
        );
        pacing.latency_p95 = Duration::from_millis(40);
        let slow = HudInput {
            paint: PaintRate { painted: 3, missed: 0 },
            mbps: 0.42,
            rtt: Some(Duration::from_millis(180)),
            ..input(&stats, &pacing)
        };
        assert_eq!(
            said(&summary(&slow)),
            [
                ("3 fps".to_owned(), false),
                ("12 ms to glass".to_owned(), true),
                ("0.4 Mb/s".to_owned(), false),
                ("RTT 180.0 ms".to_owned(), true),
            ]
        );
        let missing =
            HudInput { paint: PaintRate { painted: 52, missed: 8 }, ..input(&stats, &pacing) };
        let line = summary(&missing);
        assert_eq!(
            said(&line[..2]),
            [("52 fps".to_owned(), false), ("8 late".to_owned(), true)],
            "the painted rate, and the misses apart"
        );
        let none = PacingStats::default();
        let blank = summary(&HudInput { rtt: None, ..input(&stats, &none) });
        assert_eq!(blank[1].text, "\u{2013} to glass");
        assert_eq!(blank[3].text, "RTT \u{2013}");
    }

    /// Once the worker's clock is placed, "to glass" is from the capture, and it is flagged
    /// only past three display periods and the path's one way: 50 ms plus 10 ms at 60 Hz over a
    /// 20 ms round trip. Timings from a capture with no clock behind them are not shown.
    #[test]
    fn to_glass_is_from_the_capture_once_the_clock_is_placed() {
        use slopty_client::pacing::{ClockAnchor, ClockEstimate};
        let pacing = PacingStats {
            presented: 600,
            latency_p50: Duration::from_millis(8),
            latency_p95: Duration::from_millis(12),
            ..PacingStats::default()
        };
        let capture = |p50: u64, p95: u64| Spread {
            p50: Duration::from_millis(p50),
            p95: Duration::from_millis(p95),
            max: Duration::from_millis(p95),
            count: 240,
        };
        let clocked = ScreenStats {
            clock: Some(ClockEstimate {
                anchor: ClockAnchor { at: Instant::now(), host_us: 0 },
                bound: Duration::from_millis(10),
                rtt: Duration::from_millis(20),
                drift_ppm: 0,
            }),
            ..ScreenStats::default()
        };
        let rtt = Some(Duration::from_millis(20));
        let fast = capture(41, 59);
        let line = summary(&HudInput { capture: &fast, rtt, ..input(&clocked, &pacing) });
        assert_eq!((line[1].text.as_str(), line[1].warn), ("41 ms to glass", false));
        let slow = capture(48, 61);
        let line = summary(&HudInput { capture: &slow, rtt, ..input(&clocked, &pacing) });
        assert_eq!((line[1].text.as_str(), line[1].warn), ("48 ms to glass", true));
        let unclocked = ScreenStats::default();
        let line = summary(&HudInput { capture: &fast, rtt, ..input(&unclocked, &pacing) });
        assert_eq!((line[1].text.as_str(), line[1].warn), ("8 ms to glass", false));
    }

    /// The mark is silent while all is well; a stall says so at once, a cut only once it has
    /// held three seconds, and frames missing the display once more than five a second do.
    #[test]
    fn the_health_mark_is_silent_until_something_is_wrong() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut probe = Probe::default();
        let mut stats = ScreenStats::default();
        let mut pacing = PacingStats { late: 10, skipped: 2, ..PacingStats::default() };
        assert_eq!(probe.read(&stats, &pacing, at(0)), None, "the first read is a baseline");
        pacing.late = 14;
        assert_eq!(probe.read(&stats, &pacing, at(1_000)), None, "four a second is fine");
        pacing.late = 24;
        assert_eq!(probe.read(&stats, &pacing, at(2_000)), Some(Health::FramesLate));
        assert_eq!(probe.read(&stats, &pacing, at(3_000)), None, "and goes when they stop");

        probe.verdict(RateVerdict::Cut, at(3_000));
        assert_eq!(probe.read(&stats, &pacing, at(4_000)), None, "one cut is the controller");
        probe.verdict(RateVerdict::Cut, at(5_000));
        assert_eq!(probe.read(&stats, &pacing, at(6_000)), Some(Health::LowBandwidth));
        stats.stalled = true;
        assert_eq!(probe.read(&stats, &pacing, at(7_000)), Some(Health::Stalled), "worst first");
        stats.stalled = false;
        probe.verdict(RateVerdict::Steady, at(7_500));
        assert_eq!(probe.read(&stats, &pacing, at(8_000)), None);
        assert_eq!(Health::LowBandwidth.word(), "Low bandwidth");
    }
}
