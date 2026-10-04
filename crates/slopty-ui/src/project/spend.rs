//! A project's time at work, the orchestrator's share apart from its tasks'.
//!
//! The server follows every agent's status ([`slopty_proto::project::Spent`]), so the time is
//! there for every task on every worker.

use slopty_core::WallMs;

use super::model::Board;

/// What a whole project spent, the orchestrator's share apart from its tasks'.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectSpend {
    /// The orchestrator's time at work, in milliseconds.
    pub orchestrator_ms: u64,
    /// Every task's, together.
    pub tasks_ms: u64,
}

impl ProjectSpend {
    /// Time at work in all.
    #[must_use]
    pub const fn total_ms(&self) -> u64 {
        self.orchestrator_ms.saturating_add(self.tasks_ms)
    }
}

impl Board {
    /// Whether any of its agents is at work now: its clocks move.
    #[must_use]
    pub fn at_work(&self) -> bool {
        self.project.orchestrator_spent.since_ms.is_some()
            || self.tasks.values().any(|c| c.spent.since_ms.is_some())
    }

    /// What the project spent as of `now`.
    #[must_use]
    pub fn project_spend(&self, now: WallMs) -> ProjectSpend {
        ProjectSpend {
            orchestrator_ms: self.project.orchestrator_spent.at(now),
            tasks_ms: self.tasks.values().map(|c| c.spent.at(now)).fold(0, u64::saturating_add),
        }
    }
}

/// Time at work as the board says it, to the minute: a clock that moves once a minute shows
/// no seconds. "under 1m", "12m", and from an hour as [`crate::kit::duration`] says it,
/// "1h 4m".
#[must_use]
pub fn worked(ms: u64) -> String {
    let mins = ms / 60_000;
    match mins {
        0 => "under 1m".to_owned(),
        1..60 => format!("{mins}m"),
        _ => crate::kit::duration(std::time::Duration::from_secs(mins.saturating_mul(60))),
    }
}
