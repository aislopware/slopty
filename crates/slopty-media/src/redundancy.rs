//! Host side: turn receiver reports into a parity ratio.
//!
//! Starting point, to be tuned against measurements: parity tracks twice the smoothed datagram
//! loss rate plus a floor, and jumps when a frame was lost outright (parity was not enough).

use slopty_proto::screen::ReceiverReport;

use crate::DEFAULT_PARITY_PERMILLE;

/// Adaptive parity ratio.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Redundancy {
    permille: u16,
    loss_permille: u32,
}

impl Default for Redundancy {
    fn default() -> Self {
        Self::new()
    }
}

impl Redundancy {
    /// Most parity ever sent (50 %); beyond this the bitrate should drop instead.
    pub const MAX: u16 = 500;
    /// Least parity ever sent (5 %): one fragment per small frame costs little and covers the
    /// single-packet loss that dominates on good links.
    pub const MIN: u16 = 50;

    /// Start at the default ratio.
    #[must_use]
    pub const fn new() -> Self {
        Self { permille: DEFAULT_PARITY_PERMILLE, loss_permille: 0 }
    }

    /// Current parity ratio in thousandths.
    #[must_use]
    pub const fn permille(self) -> u16 {
        self.permille
    }

    /// Smoothed datagram loss in thousandths.
    #[must_use]
    pub const fn loss_permille(self) -> u32 {
        self.loss_permille
    }

    /// Fold in a report. `datagrams_sent` is how many datagrams went out since the previous
    /// report. Returns the new ratio.
    pub fn on_report(&mut self, report: &ReceiverReport, datagrams_sent: u32) -> u16 {
        let sent = datagrams_sent.max(report.datagrams_lost).max(1);
        let loss = report.datagrams_lost.saturating_mul(1000).checked_div(sent).unwrap_or(0);
        // EWMA with a quarter weight on the newest sample.
        self.loss_permille = self.loss_permille.saturating_mul(3).saturating_add(loss) / 4;
        let mut target = self.loss_permille.saturating_mul(2).saturating_add(u32::from(Self::MIN));
        if report.frames_lost > 0 {
            target = target.max(u32::from(self.permille).saturating_mul(3) / 2);
        }
        self.permille = u16::try_from(target.clamp(u32::from(Self::MIN), u32::from(Self::MAX)))
            .unwrap_or(Self::MAX);
        self.permille
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_loss_and_recovers() {
        let mut r = Redundancy::new();
        assert_eq!(r.permille(), DEFAULT_PARITY_PERMILLE);
        let clean = ReceiverReport::default();
        for _ in 0..20 {
            r.on_report(&clean, 300);
        }
        assert_eq!(r.permille(), Redundancy::MIN, "clean link settles at the floor");

        let lossy = ReceiverReport { datagrams_lost: 30, ..ReceiverReport::default() };
        for _ in 0..20 {
            r.on_report(&lossy, 300);
        }
        // 10 % loss → ~20 % parity + floor.
        assert!((240..=260).contains(&r.permille()), "{}", r.permille());

        let broken =
            ReceiverReport { frames_lost: 2, datagrams_lost: 30, ..ReceiverReport::default() };
        let before = r.permille();
        assert!(r.on_report(&broken, 300) > before, "a lost frame bumps parity");

        let worst = ReceiverReport { datagrams_lost: 300, ..ReceiverReport::default() };
        for _ in 0..20 {
            r.on_report(&worst, 300);
        }
        assert_eq!(r.permille(), Redundancy::MAX);
        assert_eq!(r.on_report(&worst, 0), Redundancy::MAX, "zero sent does not divide by zero");
    }
}
