//! What a project's agents spent: time at work per node and per subtree, with the
//! orchestrator's share apart, and from each agent's thread meters its cost, its context and
//! the plan's quota.
//!
//! Time comes from the server, which follows every agent's status
//! ([`slopty_proto::project::Spent`]), so it is there for every node on every worker. Cost, context
//! and quota come from the agents' own threads ([`Meters`]), handed to the board by session as this
//! client hears them; a node whose thread it has not heard shows its time alone.

use std::collections::HashMap;

use slopty_core::{SessionId, WallMs};
use slopty_proto::project::TaskId;
use slopty_proto::thread::{Limit, Meters};

use super::model::{Board, Node};

/// Below this share of its window a node's context is not worth a mark.
pub const CONTEXT_QUIET_BP: u32 = 2_000;
/// From this share of its window a node's context warns.
pub const CONTEXT_WARN_BP: u32 = 8_000;

/// The threads' meters this client has heard, by the session their TUI runs in.
pub type MetersBySession = HashMap<SessionId, Meters>;

/// What one node spent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NodeSpend {
    /// Its own agents' time at work, in milliseconds.
    pub own_ms: u64,
    /// With every task split from it, at any depth.
    pub subtree_ms: u64,
    /// Its agent's session cost, in millionths of a US dollar, when its thread says.
    pub own_cost: Option<u64>,
    /// With what its subtree's threads say, when any of them says.
    pub subtree_cost: Option<u64>,
    /// How full its agent's context is, in hundredths of a percent, when its thread says.
    pub context_bp: Option<u32>,
}

impl NodeSpend {
    /// Whether it has split work off whose time counts with its own.
    #[must_use]
    pub const fn has_subtree(&self) -> bool {
        self.subtree_ms > self.own_ms
    }

    /// Its context, when it is full enough to show: hidden under [`CONTEXT_QUIET_BP`], and
    /// warning from [`CONTEXT_WARN_BP`].
    #[must_use]
    pub fn context_shown(&self) -> Option<(u32, bool)> {
        self.context_bp.filter(|bp| *bp >= CONTEXT_QUIET_BP).map(|bp| (bp, bp >= CONTEXT_WARN_BP))
    }
}

/// What a whole project spent, the orchestrator's share apart from its tasks'.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSpend {
    /// The orchestrator's time at work.
    pub orchestrator_ms: u64,
    /// Every task's, together.
    pub tasks_ms: u64,
    /// The orchestrator's cost, when its thread says.
    pub orchestrator_cost: Option<u64>,
    /// The tasks', when any of their threads says.
    pub tasks_cost: Option<u64>,
    /// The plan's rate windows, the fullest that any of its agents reports for each.
    pub limits: Vec<Limit>,
}

impl ProjectSpend {
    /// Time at work in all.
    #[must_use]
    pub const fn total_ms(&self) -> u64 {
        self.orchestrator_ms.saturating_add(self.tasks_ms)
    }

    /// Cost in all, when any thread says.
    #[must_use]
    pub fn total_cost(&self) -> Option<u64> {
        add(self.orchestrator_cost, self.tasks_cost)
    }
}

fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.saturating_add(b)),
        (a, b) => a.or(b),
    }
}

impl Board {
    /// Whether any of its agents is at work now: its clocks move.
    #[must_use]
    pub fn at_work(&self) -> bool {
        self.project.orchestrator_spent.since_ms.is_some()
            || self.tasks.values().any(|c| c.spent.since_ms.is_some())
    }

    /// The meters of `node`'s last agent, when its thread has said them: an ended agent's
    /// last word still says what it cost.
    fn meters<'m>(&self, node: Node, meters: &'m MetersBySession) -> Option<&'m Meters> {
        let session = match node {
            None => self.project.orchestrator?.session,
            Some(task) => self.tasks.get(&task)?.assignment.as_ref()?.term.session,
        };
        meters.get(&session)
    }

    /// What `node` spent as of `now`: the orchestrator's share alone for `None`, which the
    /// header sets beside its tasks'.
    #[must_use]
    pub fn spend(&self, node: Node, now: WallMs, meters: &MetersBySession) -> NodeSpend {
        let own_ms = match node {
            None => self.project.orchestrator_spent.at(now),
            Some(task) => self.tasks.get(&task).map_or(0, |c| c.spent.at(now)),
        };
        let mine = self.meters(node, meters);
        let own_cost = mine.and_then(|m| m.cost_micro_usd);
        let context_bp = mine.and_then(context_bp);
        let (mut subtree_ms, mut subtree_cost) = (own_ms, own_cost);
        if let Some(task) = node {
            for below in self.below(task) {
                subtree_ms = subtree_ms
                    .saturating_add(self.tasks.get(&below).map_or(0, |c| c.spent.at(now)));
                let cost = self.meters(Some(below), meters).and_then(|m| m.cost_micro_usd);
                subtree_cost = add(subtree_cost, cost);
            }
        }
        NodeSpend { own_ms, subtree_ms, own_cost, subtree_cost, context_bp }
    }

    /// What the project spent as of `now`.
    #[must_use]
    pub fn project_spend(&self, now: WallMs, meters: &MetersBySession) -> ProjectSpend {
        let orchestrator = self.meters(None, meters);
        let mut spend = ProjectSpend {
            orchestrator_ms: self.project.orchestrator_spent.at(now),
            orchestrator_cost: orchestrator.and_then(|m| m.cost_micro_usd),
            ..ProjectSpend::default()
        };
        let mut limits: Vec<Limit> = orchestrator.map(|m| m.limits.clone()).unwrap_or_default();
        for card in self.tasks.values() {
            spend.tasks_ms = spend.tasks_ms.saturating_add(card.spent.at(now));
            let Some(m) = self.meters(Some(card.id), meters) else { continue };
            spend.tasks_cost = add(spend.tasks_cost, m.cost_micro_usd);
            for limit in &m.limits {
                match limits.iter_mut().find(|l| l.name == limit.name) {
                    Some(held) if held.used_bp < limit.used_bp => *held = limit.clone(),
                    Some(_) => {}
                    None => limits.push(limit.clone()),
                }
            }
        }
        spend.limits = limits;
        spend
    }

    /// Every task split from `task`, at any depth.
    fn below(&self, task: TaskId) -> Vec<TaskId> {
        let mut found = Vec::new();
        let mut open = vec![task];
        while let Some(parent) = open.pop() {
            for card in self.tasks.values().filter(|c| c.parent == Some(parent)) {
                // A cycle cannot come from the server; a guard costs nothing.
                if card.id != task && !found.contains(&card.id) {
                    found.push(card.id);
                    open.push(card.id);
                }
            }
        }
        found
    }
}

/// How full a thread's context is, in hundredths of a percent.
fn context_bp(meters: &Meters) -> Option<u32> {
    let (used, window) = (meters.context_tokens?, meters.context_window?);
    let bp = used.saturating_mul(10_000).checked_div(window)?;
    Some(u32::try_from(bp.min(10_000)).unwrap_or(10_000))
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

/// A budget's meter in words: the estimated cost, or a plan window by its name ("five-hour").
#[must_use]
pub fn meter_words(meter: &str) -> String {
    if meter == slopty_proto::project::Budget::USD {
        "cost".to_owned()
    } else {
        format!("{} window", meter.replace(['-', '_'], " "))
    }
}

/// A budget moment in words: how much of a cap is spent, or that it was reached and no new work
/// starts until the person raises it.
#[must_use]
pub fn budget_line(meter: &str, share_bp: u64) -> String {
    let what = meter_words(meter);
    if share_bp >= 10_000 {
        format!("Reached its {what} budget: no new work starts until it is raised")
    } else {
        format!("Spent {}% of its {what} budget", share_bp / 100)
    }
}

/// A cost in US dollars, to the cent: "$0.42", "$12.08".
#[must_use]
pub fn dollars(micro_usd: u64) -> String {
    let cents = micro_usd.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// A rate window's name and use, as the header says it: "5-hour 42%".
#[must_use]
pub fn limit_line(limit: &Limit) -> String {
    let name = match limit.name.as_str() {
        "five-hour" => "5-hour",
        "seven-day" => "weekly",
        other => other,
    };
    format!("{name} {}%", limit.used_bp.saturating_add(50) / 100)
}
