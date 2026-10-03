//! What a project's agents spent, tallied from their threads' own meters, and the moments its
//! budget passes (`docs/decisions/projects.md`, "A project may have a budget per meter").
//!
//! Each thread that ever worked for the project keeps its latest figures: the agent says its
//! whole cost for the thread each time, so a later figure replaces the earlier one, and an
//! assignment given again or a subagent adds a thread of its own. A plan window is the
//! fullest any thread last read that has not reset since.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
use slopty_proto::items::FACT_KEY_MAX;
use slopty_proto::project::{Budget, Moment, Spend};
use slopty_proto::thread::{Limit, ThreadId};

/// Threads a project keeps apart. Past it, the least recently heard is folded into the settled
/// cost, and should it speak again its whole figure counts again.
pub(crate) const READINGS_KEPT: usize = 512;
/// How often a tally that crossed no threshold is written. The readings between are only
/// pushed: after a restart each agent's next figure, whole, puts its thread right.
pub(crate) const WRITE_EVERY_MS: u64 = 60_000;
/// The most plan windows of one thread the tally keeps.
const WINDOWS_MAX: usize = 8;
/// A whole cap, in hundredths of a percent.
const WHOLE_BP: u64 = 10_000;

/// Every thread's latest figures, and what threads no longer kept apart cost.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Tally {
    readings: BTreeMap<ThreadId, Reading>,
    settled_micro_usd: u64,
}

/// One thread's figures, as it last said them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Reading {
    cost_micro_usd: u64,
    windows: Vec<Limit>,
    at_ms: WallMs,
}

impl Tally {
    /// Take `thread`'s latest figures at `now`: a cost its agent does not say keeps the one it
    /// said before. Windows past [`WINDOWS_MAX`], or named past
    /// [`FACT_KEY_MAX`], are left out. Whether anything changed.
    pub(crate) fn take(
        &mut self,
        thread: ThreadId,
        cost_micro_usd: Option<u64>,
        windows: &[Limit],
        now: WallMs,
    ) -> bool {
        let windows: Vec<Limit> = windows
            .iter()
            .filter(|w| (1..=FACT_KEY_MAX).contains(&w.name.chars().count()))
            .take(WINDOWS_MAX)
            .map(|w| Limit { used_bp: w.used_bp.min(10_000), ..w.clone() })
            .collect();
        let had = self.readings.get(&thread);
        let cost = cost_micro_usd.or_else(|| had.map(|r| r.cost_micro_usd)).unwrap_or(0);
        if had.is_some_and(|r| r.cost_micro_usd == cost && r.windows == windows) {
            return false;
        }
        self.readings.insert(thread, Reading { cost_micro_usd: cost, windows, at_ms: now });
        while self.readings.len() > READINGS_KEPT {
            let oldest = self.readings.iter().min_by_key(|(_, r)| r.at_ms).map(|(id, _)| *id);
            let Some(gone) = oldest.and_then(|id| self.readings.remove(&id)) else { break };
            self.settled_micro_usd = self.settled_micro_usd.saturating_add(gone.cost_micro_usd);
        }
        true
    }

    /// What it comes to at `now`: every thread's cost with the settled, and each window at the
    /// fullest a thread read that has not reset since.
    #[must_use]
    pub(crate) fn spend(&self, now: WallMs) -> Spend {
        let mut spend = Spend { cost_micro_usd: self.settled_micro_usd, windows: BTreeMap::new() };
        for reading in self.readings.values() {
            spend.cost_micro_usd = spend.cost_micro_usd.saturating_add(reading.cost_micro_usd);
            for window in reading.windows.iter().filter(|w| w.resets_ms.is_none_or(|at| at > now)) {
                let fullest = spend.windows.entry(window.name.clone()).or_default();
                *fullest = (*fullest).max(window.used_bp);
            }
        }
        spend
    }

    /// The soonest a window `budget` caps resets after `now`, when one does: the spend falls
    /// then without a word from any agent.
    #[must_use]
    pub(crate) fn next_reset(&self, budget: &Budget, now: WallMs) -> Option<WallMs> {
        self.readings
            .values()
            .flat_map(|r| &r.windows)
            .filter(|w| budget.0.contains_key(&w.name))
            .filter_map(|w| w.resets_ms)
            .filter(|at| *at > now)
            .min()
    }
}

/// The moments a budget's meters passed between two readings of it ([`Budget::against`]):
/// for each meter that rose to [`Budget::NEAR_BP`] or to its whole from below, the higher of
/// the two it passed, at the share it stands at now. A meter that falls, or stays on one side,
/// says nothing.
#[must_use]
pub(crate) fn passed(before: &[(String, u64)], after: &[(String, u64)]) -> Vec<Moment> {
    after
        .iter()
        .filter(|(meter, share)| {
            let was = before.iter().find(|(m, _)| m == meter).map_or(0, |(_, s)| *s);
            [Budget::NEAR_BP, WHOLE_BP].iter().any(|line| was < *line && *share >= *line)
        })
        .map(|(meter, share)| Moment::Budget { meter: meter.clone(), share_bp: *share })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> WallMs {
        WallMs::from_millis(ms.saturating_add(1_790_000_000_000))
    }

    fn window(name: &str, used_bp: u32, resets: Option<u64>) -> Limit {
        Limit { name: name.to_owned(), used_bp, resets_ms: resets.map(at) }
    }

    /// Each thread counts its latest whole figure once: a second figure replaces the first, a
    /// thread given again adds its own, a figure with no cost keeps the cost said before, and
    /// the same figures again change nothing. A window is the fullest any thread read that has
    /// not reset.
    #[test]
    fn every_thread_counts_its_latest_figure_once() {
        let (first, again) = (ThreadId::new(), ThreadId::new());
        let mut tally = Tally::default();
        assert!(tally.take(
            first,
            Some(2_000_000),
            &[window("five-hour", 3_000, Some(100))],
            at(0)
        ));
        assert!(tally.take(
            first,
            Some(5_000_000),
            &[window("five-hour", 4_000, Some(100))],
            at(1)
        ));
        assert!(tally.take(again, Some(1_000_000), &[window("five-hour", 2_000, None)], at(2)));
        assert!(!tally.take(again, None, &[window("five-hour", 2_000, None)], at(3)), "the same");
        let spend = tally.spend(at(50));
        assert_eq!(spend.cost_micro_usd, 6_000_000);
        assert_eq!(spend.windows, BTreeMap::from([("five-hour".to_owned(), 4_000)]));
        assert_eq!(
            tally.spend(at(100)).windows,
            BTreeMap::from([("five-hour".to_owned(), 2_000)]),
            "the fuller reading reset"
        );
        let budget = Budget(BTreeMap::from([("five-hour".to_owned(), 5_000)]));
        assert_eq!(tally.next_reset(&budget, at(50)), Some(at(100)));
        assert_eq!(tally.next_reset(&budget, at(100)), None);
    }

    /// Past the threads kept apart, the least recently heard is folded into the settled cost:
    /// the total stays whole.
    #[test]
    fn a_thread_past_those_kept_still_counts() {
        let mut tally = Tally::default();
        let first = ThreadId::new();
        assert!(tally.take(first, Some(7), &[], at(0)));
        for n in 1..=READINGS_KEPT {
            let n = u64::try_from(n).unwrap_or(u64::MAX);
            assert!(tally.take(ThreadId::new(), Some(1), &[], at(n)));
        }
        assert_eq!(tally.readings.len(), READINGS_KEPT);
        assert!(!tally.readings.contains_key(&first), "the oldest folded");
        let kept = u64::try_from(READINGS_KEPT).unwrap_or(u64::MAX);
        assert_eq!(tally.spend(at(0)).cost_micro_usd, kept.saturating_add(7));
    }

    /// A meter that rises past 80 % says so once, past its whole once more, and the higher of
    /// the two when one figure passes both; falling, or rising on one side, says nothing.
    #[test]
    fn a_meter_says_each_line_it_rises_past_once() {
        let shares = |usd: u64| vec![(Budget::USD.to_owned(), usd)];
        let budget = |share_bp| Moment::Budget { meter: Budget::USD.to_owned(), share_bp };
        assert_eq!(passed(&shares(7_000), &shares(8_100)), [budget(8_100)]);
        assert_eq!(passed(&shares(8_100), &shares(9_000)), Vec::<Moment>::new());
        assert_eq!(passed(&shares(9_000), &shares(10_200)), [budget(10_200)]);
        assert_eq!(passed(&shares(1_000), &shares(12_000)), [budget(12_000)], "one moment");
        assert_eq!(passed(&shares(12_000), &shares(3_000)), Vec::<Moment>::new());
        assert_eq!(passed(&[], &shares(10_000)), [budget(10_000)], "a budget set at its cap");
    }
}
