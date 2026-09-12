//! Adaptive video bitrate, driven by the receiver's reports and the QUIC path.
//!
//! The client asks for a ceiling (`Quality::bitrate_bps`); the host sends at whatever the path
//! sustains below it. Every [`DECIDE_EVERY`] reports (about half a second at the client's
//! 50 ms cadence) the window is judged by [`judge`], a pure function of the summed reports:
//!
//! * **Stall** — any report in the window said the link stalled (nothing arrived for a stall gap;
//!   packets held, then released together): freeze the target. Loss counted during a stall is the
//!   receiver giving up on frames the link is still holding, and the release fills the present
//!   queue for a moment; neither means the path is short of bandwidth, and sending less does not
//!   clear a Wi-Fi stall. The window is discarded (no cut, no grow, cooldown untouched) and the
//!   queue and hold figures of the next [`SETTLE_REPORTS`] reports are not counted, so the burst
//!   the release produces is not judged either. Loss in those reports still counts: a stall
//!   followed by real loss cuts once.
//! * **Overuse** — datagram loss above 2 %, the client's present queue ≥ 3 frames, or its hold p95
//!   above 60 ms (frames waiting for their missing tail): cut to 75 % and hold for [`COOLDOWN`]
//!   decisions.
//! * **Clean** — loss ≤ 0.5 %, queue ≤ 1, hold p95 ≤ 25 ms: grow by an eighth (at least
//!   [`STEP_MIN_BPS`]) towards the ceiling.
//! * Otherwise stay.
//!
//! The policy's value (`wanted`) and the target the encoder gets are two numbers: the target is
//! `wanted` capped at 90 % of the selected QUIC path's `cwnd × 8 / rtt`, in every state
//! including a stall, because datagrams are congestion-controlled and sending past the window
//! only fills the datagram queue on the host (`queue_full` in the stats). The path is sampled
//! with every report and the window keeps the *widest* sample: BBR shrinks the congestion
//! window to four packets for 200 ms every few seconds to re-measure the round trip
//! (`ProbeRTT`), and a cap read in those 200 ms would cut a loopback stream to a few Mbit/s
//! (MEASUREMENTS.md, "start-up on a cold connection"). The cap only shadows
//! `wanted`: a stall that shrinks the window drags the target down for as long as the window
//! is small and the target springs back when it recovers, instead of growing back an eighth
//! at a time. A cut is taken from the target actually sent; a clean window under the cap does
//! not grow `wanted` (nothing was learned about rates above the cap).

use slopty_core::Duration;
use slopty_proto::screen::{RateVerdict, ReceiverReport};

/// Reports per decision.
pub const DECIDE_EVERY: u32 = 10;
/// Decisions to wait after a cut before growing again.
pub const COOLDOWN: u32 = 4;
/// Reports after a stall whose queue and hold figures are ignored (the release burst).
pub const SETTLE_REPORTS: u32 = 2;
/// Floor for any target; below this the picture is unreadable anyway.
pub const MIN_BPS: u32 = 1_000_000;
/// Where a stream starts when the ceiling is higher: a mesh path sustains this, a LAN grows
/// out of it in a few seconds.
pub const START_BPS: u32 = 12_000_000;
/// Smallest growth step.
pub const STEP_MIN_BPS: u32 = 500_000;

const OVERUSE_LOSS_PERMILLE: u32 = 20;
const CLEAN_LOSS_PERMILLE: u32 = 5;
const OVERUSE_QUEUE: u8 = 3;
const CLEAN_QUEUE: u8 = 1;
const OVERUSE_HOLD_MS: u64 = 60;
const CLEAN_HOLD_MS: u64 = 25;

/// What the transport knows about the selected path when a report arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PathSample {
    /// Smoothed round trip.
    pub rtt: Duration,
    /// Congestion window in bytes.
    pub cwnd: u64,
}

impl PathSample {
    /// Throughput the window allows, bits per second; `None` when the sample is empty.
    #[must_use]
    pub const fn window_bps(self) -> Option<u64> {
        let rtt_us = self.rtt.as_micros();
        if rtt_us == 0 || self.cwnd == 0 {
            return None;
        }
        self.cwnd.saturating_mul(8).saturating_mul(1_000_000).checked_div(rtt_us)
    }
}

/// One decision window: the reports since the last decision, summed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Window {
    /// Datagrams the client counted lost.
    pub lost: u32,
    /// Datagrams the host sent.
    pub sent: u32,
    /// Deepest present queue reported.
    pub queue_max: u8,
    /// Longest hold p95 reported.
    pub hold_max: Duration,
    /// Milliseconds the link was stalled, summed over the reports.
    pub stalled_ms: u32,
    /// Stalls that released.
    pub stalls: u32,
    /// The widest path sample of the window (highest `cwnd × 8 / rtt`).
    pub path: Option<PathSample>,
}

impl Window {
    /// Fold one report in. `settling` means the report follows a stall closely: its queue and
    /// hold figures are the release burst and are not counted.
    pub fn add(&mut self, report: &ReceiverReport, datagrams_sent: u32, settling: bool) {
        self.lost = self.lost.saturating_add(report.datagrams_lost);
        self.sent = self.sent.saturating_add(datagrams_sent);
        self.stalled_ms = self.stalled_ms.saturating_add(u32::from(report.stalled_ms));
        self.stalls = self.stalls.saturating_add(u32::from(report.stalls));
        if !settling {
            self.queue_max = self.queue_max.max(report.queue_depth);
            if report.hold_p95 > self.hold_max {
                self.hold_max = report.hold_p95;
            }
        }
    }

    /// Keep `sample` if it allows more than the window's current path sample.
    pub fn add_path(&mut self, sample: Option<PathSample>) {
        let Some(sample) = sample else { return };
        let wider = match (self.path.and_then(PathSample::window_bps), sample.window_bps()) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(have), Some(new)) => new > have,
        };
        if wider {
            self.path = Some(sample);
        }
    }

    /// Whether the link stalled at any point in the window.
    #[must_use]
    pub const fn stalled(&self) -> bool {
        self.stalled_ms > 0 || self.stalls > 0
    }

    /// Lost datagrams per thousand sent.
    #[must_use]
    pub const fn loss_permille(&self) -> u32 {
        let sent = if self.sent > self.lost { self.sent } else { self.lost };
        match self.lost.saturating_mul(1000).checked_div(sent) {
            Some(permille) => permille,
            None => 0,
        }
    }
}

/// The policy: what one window asks of the target. `cooling` is whether a recent cut still
/// holds growth back.
#[must_use]
pub const fn judge(window: &Window, cooling: bool) -> RateVerdict {
    if window.stalled() {
        return RateVerdict::Stall;
    }
    let loss = window.loss_permille();
    let hold_ms = window.hold_max.as_millis();
    let overuse = loss > OVERUSE_LOSS_PERMILLE
        || window.queue_max >= OVERUSE_QUEUE
        || hold_ms > OVERUSE_HOLD_MS;
    let clean =
        loss <= CLEAN_LOSS_PERMILLE && window.queue_max <= CLEAN_QUEUE && hold_ms <= CLEAN_HOLD_MS;
    if overuse {
        RateVerdict::Cut
    } else if clean && !cooling {
        RateVerdict::Grow
    } else {
        RateVerdict::Steady
    }
}

/// The outcome of one decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Decision {
    /// What the window asked for.
    pub verdict: RateVerdict,
    /// The target after the decision (and the cwnd cap), bits per second.
    pub target_bps: u32,
    /// Whether the target differs from before the decision.
    pub changed: bool,
    /// The cwnd cap, not the policy, is what holds the target where it is.
    pub capped: bool,
    /// The window that was judged.
    pub window: Window,
}

/// Per-stream bitrate controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RateController {
    max_bps: u32,
    /// The policy's value: what the path has earned, cap aside.
    wanted_bps: u32,
    /// What the encoder is asked for: `wanted_bps` under the cwnd cap.
    target_bps: u32,
    reports: u32,
    cooldown: u32,
    /// Reports left whose queue and hold are ignored after a stall.
    settle: u32,
    window: Window,
}

impl RateController {
    /// Start below `max_bps` (the client's ceiling) and grow into it.
    #[must_use]
    pub fn new(max_bps: u32) -> Self {
        let max_bps = max_bps.max(MIN_BPS);
        Self {
            max_bps,
            wanted_bps: max_bps.min(START_BPS),
            target_bps: max_bps.min(START_BPS),
            reports: 0,
            cooldown: 0,
            settle: 0,
            window: Window::default(),
        }
    }

    /// Current target.
    #[must_use]
    pub const fn target_bps(self) -> u32 {
        self.target_bps
    }

    /// The client's ceiling.
    #[must_use]
    pub const fn max_bps(self) -> u32 {
        self.max_bps
    }

    /// A new ceiling from the client: clamp the target under it.
    pub fn set_max(&mut self, max_bps: u32) {
        self.max_bps = max_bps.max(MIN_BPS);
        self.wanted_bps = self.wanted_bps.min(self.max_bps);
        self.target_bps = self.target_bps.min(self.max_bps);
    }

    /// Fold in one report. Returns the decision every `DECIDE_EVERY` reports.
    pub fn on_report(
        &mut self,
        report: &ReceiverReport,
        datagrams_sent: u32,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        let settling = self.settle > 0;
        self.settle = self.settle.saturating_sub(1);
        if report.stalled_ms > 0 || report.stalls > 0 {
            self.settle = SETTLE_REPORTS;
        }
        self.window.add(report, datagrams_sent, settling);
        self.window.add_path(path);
        self.reports = self.reports.saturating_add(1);
        if self.reports < DECIDE_EVERY {
            return None;
        }
        let decision = self.decide();
        self.reports = 0;
        self.window = Window::default();
        Some(decision)
    }

    fn decide(&mut self) -> Decision {
        let path = self.window.path;
        let verdict = judge(&self.window, self.cooldown > 0);
        let before = self.target_bps;
        let under_cap = self.target_bps < self.wanted_bps;
        match verdict {
            RateVerdict::Cut => {
                self.wanted_bps = before.saturating_mul(3) / 4;
                self.cooldown = COOLDOWN;
            }
            RateVerdict::Grow if !under_cap => {
                self.wanted_bps = before.saturating_add((before / 8).max(STEP_MIN_BPS));
            }
            RateVerdict::Grow | RateVerdict::Stall => {}
            RateVerdict::Steady => self.cooldown = self.cooldown.saturating_sub(1),
        }
        self.wanted_bps = self.wanted_bps.clamp(MIN_BPS, self.max_bps);
        let cap = path
            .and_then(PathSample::window_bps)
            .map(|window| u32::try_from(window.saturating_mul(9) / 10).unwrap_or(u32::MAX));
        self.target_bps = cap.map_or(self.wanted_bps, |cap| cap.min(self.wanted_bps)).max(MIN_BPS);
        Decision {
            verdict,
            target_bps: self.target_bps,
            changed: self.target_bps != before,
            capped: self.target_bps < self.wanted_bps,
            window: self.window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLEAN: ReceiverReport = ReceiverReport {
        frames_ok: 0,
        frames_fec: 0,
        frames_lost: 0,
        datagrams_lost: 0,
        last_host_send_ts_us: 0,
        hold_p50: Duration::ZERO,
        hold_p95: Duration::ZERO,
        owd_jitter: Duration::ZERO,
        queue_depth: 0,
        late_frames: 0,
        acked_ltr: [0; 4],
        acked_ltr_len: 0,
        stalled_ms: 0,
        stalls: 0,
    };
    const LOSSY: ReceiverReport = ReceiverReport { datagrams_lost: 30, ..CLEAN };
    /// A stall released in this report, and the frames given up during it count as lost.
    const STALL: ReceiverReport =
        ReceiverReport { datagrams_lost: 30, stalled_ms: 180, stalls: 1, ..CLEAN };
    /// Right after the release: the present queue is full of the burst.
    const BURST: ReceiverReport =
        ReceiverReport { queue_depth: 4, hold_p95: Duration::from_millis(90), ..CLEAN };

    /// One decision window of identical reports.
    fn run(
        c: &mut RateController,
        report: &ReceiverReport,
        sent: u32,
        path: Option<PathSample>,
    ) -> Decision {
        let mut last = None;
        for _ in 0..DECIDE_EVERY {
            if let Some(d) = c.on_report(report, sent, path) {
                last = Some(d);
            }
        }
        last.expect("one decision per DECIDE_EVERY reports")
    }

    /// Feed report sequences (each entry one full decision window) and collect the targets
    /// and verdicts they produce.
    fn trajectory(windows: &[&ReceiverReport]) -> Vec<(RateVerdict, u32)> {
        let mut c = RateController::new(30_000_000);
        windows
            .iter()
            .map(|r| run(&mut c, r, 300, None))
            .map(|d| (d.verdict, d.target_bps))
            .collect()
    }

    #[test]
    fn the_policy_is_a_pure_function_of_the_window() {
        let clean = Window { sent: 300, ..Window::default() };
        assert_eq!(judge(&clean, false), RateVerdict::Grow);
        assert_eq!(judge(&clean, true), RateVerdict::Steady, "cooling: no growth");
        let lossy = Window { lost: 30, sent: 300, ..Window::default() };
        assert_eq!(judge(&lossy, false), RateVerdict::Cut);
        assert_eq!(judge(&Window { stalled_ms: 120, ..lossy }, false), RateVerdict::Stall);
        assert_eq!(judge(&Window { stalls: 1, ..lossy }, true), RateVerdict::Stall);
        let queued = Window { queue_max: 3, sent: 300, ..Window::default() };
        assert_eq!(judge(&queued, false), RateVerdict::Cut);
        let held = Window { hold_max: Duration::from_millis(80), sent: 300, ..Window::default() };
        assert_eq!(judge(&held, false), RateVerdict::Cut);
        let meh = Window { lost: 3, sent: 300, ..Window::default() };
        assert_eq!(judge(&meh, false), RateVerdict::Steady, "1 %: neither clean nor overuse");
        assert_eq!(Window::default().loss_permille(), 0);
        assert_eq!(Window { lost: 5, sent: 0, ..Window::default() }.loss_permille(), 1000);
    }

    #[test]
    fn report_sequences_and_their_target_trajectories() {
        use RateVerdict::{Cut, Grow, Stall, Steady};
        let start = START_BPS;
        let grown = start + start / 8;
        let cut = start / 4 * 3;
        // Clean → grow.
        assert_eq!(trajectory(&[&CLEAN, &CLEAN]), [(Grow, grown), (Grow, grown + grown / 8)]);
        // Loss while flowing → cut, then the cooldown holds.
        assert_eq!(
            trajectory(&[&LOSSY, &CLEAN, &CLEAN, &CLEAN, &CLEAN, &CLEAN]),
            [
                (Cut, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Grow, cut + cut / 8)
            ]
        );
        // Stall (with the loss the give-ups produce) → hold, then clean → grow straight away.
        assert_eq!(
            trajectory(&[&STALL, &STALL, &CLEAN]),
            [(Stall, start), (Stall, start), (Grow, grown)]
        );
        // Stall followed by real loss → one cut.
        assert_eq!(
            trajectory(&[&STALL, &LOSSY, &CLEAN]),
            [(Stall, start), (Cut, cut), (Steady, cut)]
        );
        // A cut, then a stall in the cooldown: the stall neither cuts again nor ends the cooldown.
        assert_eq!(
            trajectory(&[&LOSSY, &STALL, &CLEAN, &CLEAN, &CLEAN, &CLEAN, &CLEAN]),
            [
                (Cut, cut),
                (Stall, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Steady, cut),
                (Grow, cut + cut / 8)
            ]
        );
    }

    #[test]
    fn the_release_burst_after_a_stall_is_not_judged() {
        let mut c = RateController::new(30_000_000);
        // A window: eight clean reports, the stall releases in the ninth, the burst fills the
        // queue in the tenth.
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        assert_eq!(c.on_report(&STALL, 300, None), None);
        let d = c.on_report(&BURST, 300, None).expect("decision");
        assert_eq!((d.verdict, d.target_bps), (RateVerdict::Stall, START_BPS));
        // The next window starts with the burst still draining, then is clean: it grows.
        assert_eq!(c.on_report(&BURST, 300, None), None);
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&CLEAN, 300, None).expect("decision");
        assert_eq!(d.verdict, RateVerdict::Grow, "the burst report after a stall is not counted");
        // The same burst report without a stall before it is overuse.
        let mut c = RateController::new(30_000_000);
        assert_eq!(run(&mut c, &BURST, 300, None).verdict, RateVerdict::Cut);
        // Loss in a settling report still counts.
        let mut c = RateController::new(30_000_000);
        for _ in 0..9 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        assert_eq!(c.on_report(&STALL, 300, None).map(|d| d.verdict), Some(RateVerdict::Stall));
        // 100 of the window's 3000 datagrams: over the 2 % overuse line on its own.
        let burst_loss = ReceiverReport { datagrams_lost: 100, ..CLEAN };
        assert_eq!(c.on_report(&burst_loss, 300, None), None);
        for _ in 0..8 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&CLEAN, 300, None).expect("decision");
        assert_eq!(d.verdict, RateVerdict::Cut, "real loss right after a stall");
    }

    /// The window's folds: the hold keeps its maximum, the path keeps the wider sample and
    /// ignores an equal or empty one, and a sample with no time or no window is no throughput.
    #[test]
    fn a_window_keeps_the_longest_hold_and_the_widest_path() {
        assert_eq!(PathSample { rtt: Duration::ZERO, cwnd: 10 }.window_bps(), None);
        assert_eq!(PathSample { rtt: Duration::from_millis(1), cwnd: 0 }.window_bps(), None);
        let mut w = Window::default();
        w.add(&ReceiverReport { hold_p95: Duration::from_millis(30), ..CLEAN }, 10, false);
        w.add(&ReceiverReport { hold_p95: Duration::from_millis(20), ..CLEAN }, 10, false);
        assert_eq!((w.hold_max, w.sent), (Duration::from_millis(30), 20));
        let first = PathSample { rtt: Duration::from_millis(10), cwnd: 100_000 };
        let equal = PathSample { rtt: Duration::from_millis(20), cwnd: 200_000 };
        let wider = PathSample { rtt: Duration::from_millis(10), cwnd: 200_000 };
        w.add_path(Some(first));
        w.add_path(Some(equal));
        assert_eq!(w.path, Some(first), "an equal window does not replace the first");
        w.add_path(Some(wider));
        w.add_path(None);
        assert_eq!(w.path, Some(wider));
    }

    /// The overuse lines are exclusive: loss or a hold of exactly the figure is not yet a cut.
    #[test]
    fn the_overuse_lines_are_exclusive() {
        let at = |lost: u32, hold_ms: u64| Window {
            lost,
            sent: 1000,
            hold_max: Duration::from_millis(hold_ms),
            ..Window::default()
        };
        assert_ne!(judge(&at(OVERUSE_LOSS_PERMILLE, 0), false), RateVerdict::Cut);
        assert_eq!(judge(&at(OVERUSE_LOSS_PERMILLE + 1, 0), false), RateVerdict::Cut);
        assert_ne!(judge(&at(0, OVERUSE_HOLD_MS), false), RateVerdict::Cut);
        assert_eq!(judge(&at(0, OVERUSE_HOLD_MS + 1), false), RateVerdict::Cut);
    }

    /// Either stall figure alone starts the settling, and a report with neither never does:
    /// the burst after a clean run is judged.
    #[test]
    fn either_stall_figure_alone_settles_the_next_reports() {
        let only_count = ReceiverReport { stalls: 1, ..CLEAN };
        let only_time = ReceiverReport { stalled_ms: 50, ..CLEAN };
        for stall in [only_count, only_time] {
            let mut c = RateController::new(30_000_000);
            for _ in 0..8 {
                assert_eq!(c.on_report(&CLEAN, 300, None), None);
            }
            assert_eq!(c.on_report(&stall, 300, None), None);
            let d = c.on_report(&BURST, 300, None).expect("decision");
            assert_eq!(d.verdict, RateVerdict::Stall, "{stall:?}");
            assert_eq!(d.window.queue_max, 0, "the burst was not counted after {stall:?}");
        }
        let mut c = RateController::new(30_000_000);
        for _ in 0..9 {
            assert_eq!(c.on_report(&CLEAN, 300, None), None);
        }
        let d = c.on_report(&BURST, 300, None).expect("decision");
        assert_eq!((d.verdict, d.window.queue_max), (RateVerdict::Cut, 4), "nothing to settle");
    }

    #[test]
    fn grows_into_the_ceiling_on_a_clean_path() {
        let mut c = RateController::new(30_000_000);
        assert_eq!(c.target_bps(), START_BPS);
        let mut steps = 0;
        while c.target_bps() < 30_000_000 {
            assert!(run(&mut c, &CLEAN, 300, None).changed);
            steps += 1;
            assert!(steps < 20, "never reached the ceiling");
        }
        let d = run(&mut c, &CLEAN, 300, None);
        assert_eq!((d.verdict, d.changed), (RateVerdict::Grow, false), "stays at the ceiling");
    }

    #[test]
    fn congestion_window_caps_the_target_in_every_state() {
        // 13 KB window over 10 ms: ~10.4 Mbit/s, 90 % of that is the cap.
        let path = PathSample { rtt: Duration::from_millis(10), cwnd: 13_000 };
        let cap = u32::try_from(path.window_bps().expect("window") * 9 / 10).expect("fits");
        for report in [&CLEAN, &STALL, &LOSSY] {
            let mut c = RateController::new(30_000_000);
            let d = run(&mut c, report, 300, Some(path));
            assert!(d.target_bps <= cap, "{:?} → {} over the cap {cap}", d.verdict, d.target_bps);
            assert!(d.capped || d.verdict == RateVerdict::Cut, "{d:?}");
        }
        let mut c = RateController::new(30_000_000);
        assert_eq!(run(&mut c, &CLEAN, 300, Some(path)).target_bps, cap);
        assert_eq!(PathSample::default().window_bps(), None);
    }

    /// The cap shadows the policy's value: a stall that shrinks the window pulls the target
    /// down only while the window is small, and clean windows under the cap teach nothing.
    #[test]
    fn the_cap_shadows_the_wanted_rate_and_lets_go() {
        let small = Some(PathSample { rtt: Duration::from_millis(30), cwnd: 9_000 });
        let big = Some(PathSample { rtt: Duration::from_millis(10), cwnd: 1_000_000 });
        let mut c = RateController::new(30_000_000);
        let d = run(&mut c, &STALL, 300, small);
        assert_eq!((d.verdict, d.capped), (RateVerdict::Stall, true));
        assert!(d.target_bps < 3_000_000, "under the 2.4 Mbit/s window: {}", d.target_bps);
        let d = run(&mut c, &STALL, 300, big);
        assert_eq!((d.verdict, d.target_bps, d.capped), (RateVerdict::Stall, START_BPS, false));
        // Clean under a small cap: the target sits at the cap and wanted does not grow.
        assert_eq!(run(&mut c, &CLEAN, 300, small).verdict, RateVerdict::Grow);
        assert_eq!(run(&mut c, &CLEAN, 300, big).target_bps, START_BPS + START_BPS / 8);
        // A cut is taken from the rate that was actually sent.
        let capped = run(&mut c, &CLEAN, 300, small).target_bps;
        let d = run(&mut c, &LOSSY, 300, big);
        assert_eq!(d.target_bps, capped / 4 * 3);
    }

    /// Four of the ten reports in a window see BBR's `ProbeRTT` window (four packets); the
    /// other six see the real one. The cap follows the widest sample.
    #[test]
    fn a_probe_rtt_dip_inside_the_window_does_not_cap_the_target() {
        let wide = Some(PathSample { rtt: Duration::from_millis(1), cwnd: 80_000 });
        let dip = Some(PathSample { rtt: Duration::from_millis(5), cwnd: 5_808 });
        let mut c = RateController::new(30_000_000);
        let mut last = None;
        for i in 0..DECIDE_EVERY {
            let path = if (3..7).contains(&i) { dip } else { wide };
            if let Some(d) = c.on_report(&CLEAN, 300, path) {
                last = Some(d);
            }
        }
        let d = last.expect("decision");
        assert_eq!(
            (d.verdict, d.capped, d.target_bps),
            (RateVerdict::Grow, false, START_BPS + START_BPS / 8)
        );
        // A window that only ever saw the dip is capped by it.
        let mut c = RateController::new(30_000_000);
        let d = run(&mut c, &CLEAN, 300, dip);
        assert!(d.capped && d.target_bps < START_BPS, "{d:?}");
        let mut w = Window::default();
        w.add_path(None);
        assert_eq!(w.path, None);
        w.add_path(dip);
        w.add_path(Some(PathSample::default()));
        assert_eq!(w.path, dip, "an empty sample never replaces a real one");
    }

    #[test]
    fn never_below_the_floor_or_above_the_ceiling() {
        let mut c = RateController::new(500_000);
        assert_eq!(c.max_bps(), MIN_BPS);
        assert_eq!(c.target_bps(), MIN_BPS);
        let worst = ReceiverReport { datagrams_lost: 100, ..CLEAN };
        let d = run(&mut c, &worst, 300, None);
        assert_eq!((d.verdict, d.changed), (RateVerdict::Cut, false), "already at the floor");
        c.set_max(2_000_000);
        for _ in 0..COOLDOWN + 4 {
            run(&mut c, &CLEAN, 300, None);
        }
        assert_eq!(c.target_bps(), 2_000_000);
    }
}
