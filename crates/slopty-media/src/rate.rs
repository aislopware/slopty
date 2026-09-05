//! Adaptive video bitrate, driven by the receiver's reports and the QUIC path.
//!
//! The client asks for a ceiling (`Quality::bitrate_bps`); the host sends at whatever the path
//! sustains below it. Every [`DECIDE_EVERY`] reports (about half a second at the client's
//! 50 ms cadence) the window is judged:
//!
//! * **Overuse** — datagram loss above 2 %, the client's present queue ≥ 3 frames, or its hold p95
//!   above 60 ms (frames waiting for their missing tail): cut to 75 % and hold for [`COOLDOWN`]
//!   decisions.
//! * **Clean** — loss ≤ 0.5 %, queue ≤ 1, hold p95 ≤ 25 ms: grow by an eighth (at least
//!   [`STEP_MIN_BPS`]) towards the ceiling.
//! * Otherwise stay.
//!
//! On top, the selected QUIC path's congestion window caps the target at 90 % of
//! `cwnd × 8 / rtt`: datagrams are congestion-controlled, so sending past the window only
//! fills the datagram queue on the host (`queue_full` in the stats) and never reaches the wire.

use slopty_core::Duration;
use slopty_proto::screen::ReceiverReport;

/// Reports per decision.
pub const DECIDE_EVERY: u32 = 10;
/// Decisions to wait after a cut before growing again.
pub const COOLDOWN: u32 = 4;
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

/// Per-stream bitrate controller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RateController {
    max_bps: u32,
    target_bps: u32,
    reports: u32,
    cooldown: u32,
    // The window under judgement.
    lost: u32,
    sent: u32,
    queue_max: u8,
    hold_max: Duration,
}

impl RateController {
    /// Start below `max_bps` (the client's ceiling) and grow into it.
    #[must_use]
    pub fn new(max_bps: u32) -> Self {
        let max_bps = max_bps.max(MIN_BPS);
        Self {
            max_bps,
            target_bps: max_bps.min(START_BPS),
            reports: 0,
            cooldown: 0,
            lost: 0,
            sent: 0,
            queue_max: 0,
            hold_max: Duration::ZERO,
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
        self.target_bps = self.target_bps.min(self.max_bps);
    }

    /// Fold in one report. Returns the new target when it changed.
    pub fn on_report(
        &mut self,
        report: &ReceiverReport,
        datagrams_sent: u32,
        path: Option<PathSample>,
    ) -> Option<u32> {
        self.lost = self.lost.saturating_add(report.datagrams_lost);
        self.sent = self.sent.saturating_add(datagrams_sent);
        self.queue_max = self.queue_max.max(report.queue_depth);
        if report.hold_p95 > self.hold_max {
            self.hold_max = report.hold_p95;
        }
        self.reports = self.reports.saturating_add(1);
        if self.reports < DECIDE_EVERY {
            return None;
        }
        let decision = self.decide(path);
        self.reports = 0;
        self.lost = 0;
        self.sent = 0;
        self.queue_max = 0;
        self.hold_max = Duration::ZERO;
        decision
    }

    fn decide(&mut self, path: Option<PathSample>) -> Option<u32> {
        let sent = self.sent.max(self.lost).max(1);
        let loss = self.lost.saturating_mul(1000).checked_div(sent).unwrap_or(0);
        let hold_ms = self.hold_max.as_millis();
        let overuse = loss > OVERUSE_LOSS_PERMILLE
            || self.queue_max >= OVERUSE_QUEUE
            || hold_ms > OVERUSE_HOLD_MS;
        let clean = loss <= CLEAN_LOSS_PERMILLE
            && self.queue_max <= CLEAN_QUEUE
            && hold_ms <= CLEAN_HOLD_MS;
        let before = self.target_bps;
        let mut next = before;
        if overuse {
            next = before.saturating_mul(3) / 4;
            self.cooldown = COOLDOWN;
        } else if self.cooldown > 0 {
            self.cooldown = self.cooldown.saturating_sub(1);
        } else if clean {
            next = before.saturating_add((before / 8).max(STEP_MIN_BPS));
        }
        if let Some(window) = path.and_then(PathSample::window_bps) {
            let cap = u32::try_from(window.saturating_mul(9) / 10).unwrap_or(u32::MAX);
            next = next.min(cap);
        }
        self.target_bps = next.clamp(MIN_BPS, self.max_bps);
        (self.target_bps != before).then_some(self.target_bps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(
        c: &mut RateController,
        report: &ReceiverReport,
        sent: u32,
        path: Option<PathSample>,
    ) -> Option<u32> {
        let mut last = None;
        for _ in 0..DECIDE_EVERY {
            if let Some(t) = c.on_report(report, sent, path) {
                last = Some(t);
            }
        }
        last
    }

    #[test]
    fn grows_into_the_ceiling_on_a_clean_path() {
        let mut c = RateController::new(30_000_000);
        assert_eq!(c.target_bps(), START_BPS);
        let clean = ReceiverReport::default();
        let mut steps = 0;
        while c.target_bps() < 30_000_000 {
            assert!(run(&mut c, &clean, 300, None).is_some());
            steps += 1;
            assert!(steps < 20, "never reached the ceiling");
        }
        assert_eq!(run(&mut c, &clean, 300, None), None, "stays at the ceiling");
    }

    #[test]
    fn cuts_on_loss_and_waits_before_growing() {
        let mut c = RateController::new(30_000_000);
        let lossy = ReceiverReport { datagrams_lost: 30, ..ReceiverReport::default() };
        assert_eq!(run(&mut c, &lossy, 300, None), Some(START_BPS / 4 * 3));
        let clean = ReceiverReport::default();
        for _ in 0..COOLDOWN {
            assert_eq!(run(&mut c, &clean, 300, None), None, "cooldown holds");
        }
        assert!(run(&mut c, &clean, 300, None).is_some_and(|t| t > START_BPS / 4 * 3));
    }

    #[test]
    fn client_queue_or_hold_counts_as_overuse() {
        let mut c = RateController::new(30_000_000);
        let queued = ReceiverReport { queue_depth: 3, ..ReceiverReport::default() };
        assert!(run(&mut c, &queued, 300, None).is_some_and(|t| t < START_BPS));
        let mut c = RateController::new(30_000_000);
        let held =
            ReceiverReport { hold_p95: Duration::from_millis(80), ..ReceiverReport::default() };
        assert!(run(&mut c, &held, 300, None).is_some_and(|t| t < START_BPS));
    }

    #[test]
    fn congestion_window_caps_the_target() {
        let mut c = RateController::new(30_000_000);
        // 13 KB window over 10 ms: ~10.4 Mbit/s, 90 % of that is the cap.
        let path = PathSample { rtt: Duration::from_millis(10), cwnd: 13_000 };
        let cap = path.window_bps().expect("window") * 9 / 10;
        assert_eq!(
            run(&mut c, &ReceiverReport::default(), 300, Some(path)),
            Some(u32::try_from(cap).expect("fits"))
        );
        assert_eq!(PathSample::default().window_bps(), None);
    }

    #[test]
    fn never_below_the_floor_or_above_the_ceiling() {
        let mut c = RateController::new(500_000);
        assert_eq!(c.max_bps(), MIN_BPS);
        assert_eq!(c.target_bps(), MIN_BPS);
        let lossy = ReceiverReport { datagrams_lost: 100, ..ReceiverReport::default() };
        assert_eq!(run(&mut c, &lossy, 300, None), None, "already at the floor");
        c.set_max(2_000_000);
        let clean = ReceiverReport::default();
        for _ in 0..COOLDOWN + 4 {
            run(&mut c, &clean, 300, None);
        }
        assert_eq!(c.target_bps(), 2_000_000);
    }
}
