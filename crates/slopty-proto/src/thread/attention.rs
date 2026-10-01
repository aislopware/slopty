//! The attention ladder: one rank for every thread, rolled up the same way everywhere, and
//! where the person is, so a notice goes only where it is wanted (`docs/decisions/agents.md`).
//!
//! A thread's [`Rung`] comes from its table row alone ([`Rung::of`]), so the server, which
//! holds every worker's rows, and a client, which holds its own workers', rank it alike. The
//! server folds each subagent into the thread it hangs from, rolls the rest up per tile,
//! worker, project node and the whole fleet, and publishes the result as a [`Ladder`]. A
//! client's workspaces are its own, so it folds the tiles a workspace holds with
//! [`Ladder::over`].
//!
//! A client says where the person is ([`Presence`]); the server picks which clients a notice
//! goes to from that and each client's [`Seat`].

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};

use super::wire::ThreadRow;
use super::{Phase, ThreadId};
use crate::orchestration::TermRef;
use crate::project::{ProjectId, TaskId};

/// A thread's place on the attention ladder, lowest first, so the higher of two is their
/// [`Ord::max`].
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default, Serialize, Deserialize,
)]
pub enum Rung {
    /// At rest with nothing to look at.
    #[default]
    Idle,
    /// Waiting on its own background work or a wakeup.
    Waiting,
    /// Working on a turn.
    Working,
    /// At rest, with changes in its tree the person has not kept.
    ToReview,
    /// It stopped on an error.
    Failed,
    /// It waits on the person.
    NeedsYou,
}

impl Rung {
    /// Every rung, highest first.
    pub const DOWN: [Self; 6] =
        [Self::NeedsYou, Self::Failed, Self::ToReview, Self::Working, Self::Waiting, Self::Idle];

    /// Where the thread `row` stands.
    #[must_use]
    pub const fn of(row: &ThreadRow) -> Self {
        if !row.requests.is_empty() {
            return Self::NeedsYou;
        }
        match row.status.phase {
            Phase::NeedsYou => Self::NeedsYou,
            Phase::Failed => Self::Failed,
            Phase::Working => Self::Working,
            Phase::Waiting => Self::Waiting,
            Phase::Idle | Phase::Done | Phase::Stopped if row.to_review => Self::ToReview,
            Phase::Idle | Phase::Done | Phase::Stopped => Self::Idle,
        }
    }

    /// Its word, the same on every surface; none at rest.
    #[must_use]
    pub const fn word(self) -> Option<&'static str> {
        match self {
            Self::NeedsYou => Some("Needs you"),
            Self::Failed => Some("Failed"),
            Self::ToReview => Some("To review"),
            Self::Working => Some("Working"),
            Self::Waiting => Some("Waiting"),
            Self::Idle => None,
        }
    }
}

/// How many threads stand on each rung, subagents folded into their parents.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    /// Waiting on the person.
    pub needs_you: u32,
    /// Stopped on an error.
    pub failed: u32,
    /// At rest with changes not kept.
    pub to_review: u32,
    /// Working.
    pub working: u32,
    /// Waiting on their own work.
    pub waiting: u32,
    /// At rest.
    pub idle: u32,
}

impl Counts {
    /// How many stand on `rung`.
    #[must_use]
    pub const fn on(&self, rung: Rung) -> u32 {
        match rung {
            Rung::NeedsYou => self.needs_you,
            Rung::Failed => self.failed,
            Rung::ToReview => self.to_review,
            Rung::Working => self.working,
            Rung::Waiting => self.waiting,
            Rung::Idle => self.idle,
        }
    }

    /// One more on `rung`.
    pub const fn count(&mut self, rung: Rung) {
        self.add(rung, 1);
    }

    /// `n` more on `rung`.
    pub const fn add(&mut self, rung: Rung, n: u32) {
        let at = match rung {
            Rung::NeedsYou => &mut self.needs_you,
            Rung::Failed => &mut self.failed,
            Rung::ToReview => &mut self.to_review,
            Rung::Working => &mut self.working,
            Rung::Waiting => &mut self.waiting,
            Rung::Idle => &mut self.idle,
        };
        *at = at.saturating_add(n);
    }

    /// Every thread counted.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.needs_you
            .saturating_add(self.failed)
            .saturating_add(self.to_review)
            .saturating_add(self.working)
            .saturating_add(self.waiting)
            .saturating_add(self.idle)
    }
}

/// A thread on a worker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ThreadAt {
    /// The worker that hosts it.
    pub worker: WorkerId,
    /// The thread.
    pub thread: ThreadId,
}

/// A node of a project's tree: a task, or the project's orchestrator.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct NodeAt {
    /// The project.
    pub project: ProjectId,
    /// The task; the orchestrator when absent.
    pub task: Option<TaskId>,
}

/// One thread with the rung it stands on, subagents folded in: what a roll-up is made of.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ranked {
    /// The thread.
    pub at: ThreadAt,
    /// Its rung, or the highest of its subagents'.
    pub rung: Rung,
    /// Since when it stands there, by its worker's clock: the longest waiting goes first.
    pub since_ms: WallMs,
}

/// Where a group of threads stands: a tile's, a worker's, a node's, the fleet's.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Standing {
    /// The highest rung among them.
    pub rung: Rung,
    /// How many stand on each.
    pub counts: Counts,
    /// The one to go to first: on the highest rung, there longest.
    pub top: Option<ThreadAt>,
    /// Since when `top` stands there.
    pub since_ms: WallMs,
}

impl Standing {
    /// Take `thread` in.
    pub fn add(&mut self, thread: &Ranked) {
        self.counts.count(thread.rung);
        self.lead(thread.at, thread.rung, thread.since_ms);
    }

    /// Take in every thread `other` stands for, which must be apart from this one's.
    pub fn join(&mut self, other: &Self) {
        for rung in Rung::DOWN {
            self.counts.add(rung, other.counts.on(rung));
        }
        if let Some(top) = other.top {
            self.lead(top, other.rung, other.since_ms);
        }
    }

    /// Make `at` the top when it goes before the one there: on a higher rung, or longer on
    /// the same one.
    fn lead(&mut self, at: ThreadAt, rung: Rung, since_ms: WallMs) {
        let ahead = self.top.is_none_or(|top| {
            (rung, std::cmp::Reverse((since_ms, at)))
                > (self.rung, std::cmp::Reverse((self.since_ms, top)))
        });
        if ahead {
            self.rung = rung;
            self.top = Some(at);
            self.since_ms = since_ms;
        }
    }

    /// Where `threads` stand together.
    #[must_use]
    pub fn of<'a>(threads: impl IntoIterator<Item = &'a Ranked>) -> Self {
        threads.into_iter().fold(Self::default(), |mut standing, thread| {
            standing.add(thread);
            standing
        })
    }
}

/// The whole fleet's ladder, as the server publishes it: each replaces the last.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Ladder {
    /// Every thread that hangs from no other, its subagents folded in, by worker and thread.
    pub threads: Vec<Ranked>,
    /// Each terminal's threads, by worker and session.
    pub tiles: Vec<(TermRef, Standing)>,
    /// Each worker's threads.
    pub workers: Vec<(WorkerId, Standing)>,
    /// Each project node's threads, a task's subtasks folded in.
    pub nodes: Vec<(NodeAt, Standing)>,
    /// Each project's threads: its orchestrator's and every task's.
    pub projects: Vec<(ProjectId, Standing)>,
    /// Every thread.
    pub fleet: Standing,
}

impl Ladder {
    /// Where the threads of `tiles` stand together: a workspace's, a column's.
    #[must_use]
    pub fn over(&self, tiles: impl IntoIterator<Item = TermRef>) -> Standing {
        let mut tiles: Vec<TermRef> = tiles.into_iter().collect();
        tiles.sort_by_key(|t| (t.worker, t.session));
        tiles.dedup();
        tiles.iter().fold(Standing::default(), |mut sum, tile| {
            if let Some((_, standing)) = self.tile(*tile) {
                sum.join(standing);
            }
            sum
        })
    }

    /// Where `tile`'s threads stand, when it has any.
    #[must_use]
    pub fn tile(&self, tile: TermRef) -> Option<&(TermRef, Standing)> {
        let key = |t: &TermRef| (t.worker, t.session);
        self.tiles
            .binary_search_by_key(&key(&tile), |(t, _)| key(t))
            .ok()
            .and_then(|at| self.tiles.get(at))
    }

    /// The rung `thread` stands on, its subagents folded in; `None` for a subagent, which
    /// stands with its parent, and for a thread the server does not know.
    #[must_use]
    pub fn rung(&self, thread: ThreadAt) -> Option<Rung> {
        self.threads
            .binary_search_by_key(&thread, |r| r.at)
            .ok()
            .and_then(|at| self.threads.get(at))
            .map(|r| r.rung)
    }
}

/// Which kind of seat a client is, for where a notice goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Seat {
    /// A machine the person sits at: a Mac, an iPad on its keyboard.
    Desk,
    /// One they carry: a phone, an iPad in hand. Nothing is pushed to it while they are at a
    /// desk.
    Handheld,
}

/// Where the person is on one client, as it tells the server on every change.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Presence {
    /// What kind of client it is.
    pub seat: Seat,
    /// Whether the person is at it: the app in front, and used lately by its own measure.
    pub active: bool,
    /// The workspace in front, by the client's name for it.
    pub workspace: Option<String>,
    /// The tiles on screen.
    pub showing: Vec<TermRef>,
    /// The tile with the keyboard.
    pub focus: Option<TermRef>,
}

/// A client and where the person is on it, as the server lists them to every client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Present {
    /// The server's number for the client's link.
    pub link: u64,
    /// What the client calls itself ("Cong's iPad").
    pub name: String,
    /// Where the person is on it.
    pub presence: Presence,
}

/// Why a notice is sent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoticeKind {
    /// A thread came to need the person.
    NeedsYou,
    /// A thread stopped on an error.
    Failed,
    /// A thread finished working. The client shows it only when it worked as long as its
    /// person's slow-command time.
    Finished,
}

/// A notice the server picked this client to show. A subagent's goes as its parent's.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Notice {
    /// Why.
    pub kind: NoticeKind,
    /// The thread, the one a subagent hangs from.
    pub thread: ThreadAt,
    /// The terminal to open for it.
    pub tile: Option<SessionId>,
    /// The thread's title.
    pub title: String,
    /// What it wants or said, in a line.
    pub text: String,
    /// For a finished thread: how long it worked, by its worker's clock.
    pub worked_ms: Option<u64>,
    /// The subagent it comes from, when it is one's and not the thread's own.
    pub via: Option<Via>,
}

/// The subagent a notice comes from.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Via {
    /// The subagent's thread.
    pub thread: ThreadId,
    /// Its title.
    pub title: String,
}
