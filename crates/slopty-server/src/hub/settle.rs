//! Finished tasks settle (`docs/decisions/projects.md`, "A finished task's agent stops
//! counting and is closed once it rests").
//!
//! A task merged or given up keeps its agent's terminal open for whoever wants to look back or
//! carry on in it, and it counts against no limit while its agent rests
//! ([`crate::project::Projects::fleet`]). Once that agent has rested [`SETTLE_AFTER`] at its
//! prompt, with no request open, nothing scheduled and its tile on no client's screen, the
//! server closes the terminal the server started for it, through the worker's own `Close`. An
//! agent that waits only on commands it left running (a dev server) rests too: closing its
//! terminal stops them with it, and the timeline says what was stopped. The
//! agent's session stays, so it can be taken up again. A terminal the person put on a task is
//! theirs and is never closed, nor is anything still at work. Once a merged task's agent is
//! closed, its worktree goes too, through the worker's `RemoveWorktree`, which keeps one with
//! anything uncommitted or a terminal in it, and the branch of work that did not land.

use std::collections::HashMap;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::agent::{AgentStatus, BlockReason};
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
use slopty_proto::project::{ProjectId, TaskId};
use slopty_proto::terminal::SessionState;
use tokio::time::Instant;

use super::projects::live;
use super::steps::said;
use super::{Hub, State, WeakHub};

/// How long a finished task's agent rests before its terminal is closed.
pub(crate) const SETTLE_AFTER: Duration = Duration::from_mins(10);
/// How often the finished tasks' agents are looked at.
const SETTLE_EVERY: Duration = Duration::from_secs(30);

impl Hub {
    /// Close every finished task's agent that has rested long enough, until the hub is gone.
    pub async fn settle_finished(hub: WeakHub) {
        let mut resting = HashMap::new();
        loop {
            tokio::time::sleep(SETTLE_EVERY).await;
            let Some(hub) = hub.upgrade() else { return };
            hub.settle_due(&mut resting, Instant::now());
        }
    }

    /// One round of [`Hub::settle_finished`] at `now`: `resting` holds since when each finished
    /// task's agent has been seen at rest, and loses those that are not. Answers the terminals
    /// it closes.
    pub(crate) fn settle_due(
        &self,
        resting: &mut HashMap<TermRef, Instant>,
        now: Instant,
    ) -> Vec<TermRef> {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let (terminals, _) = live(state);
        let quiet: Vec<_> = state
            .projects
            .finished_agents(&terminals, |term| state.board.left_running(term).is_some())
            .into_iter()
            .filter(|(_, _, term)| {
                (rests(state, *term) || state.board.left_running(*term).is_some())
                    && !state.board.asks(*term)
                    && !state.board.shown(*term)
            })
            .collect();
        resting.retain(|term, _| quiet.iter().any(|(_, _, t)| t == term));
        let mut closing = Vec::new();
        for (project, task, term) in quiet {
            let since = *resting.entry(term).or_insert(now);
            let rested = now.saturating_duration_since(since);
            if rested < SETTLE_AFTER {
                continue;
            }
            // Seen at rest afresh should the close not take: tried again a whole wait later.
            resting.insert(term, now);
            let mins = rested.as_secs() / 60;
            let left = state.board.left_running(term);
            let changes = state.projects.settled(&project, task, (mins, left), WallMs::now());
            self.projects_moved(state, changes);
            tracing::info!(%project, %task, session = %term.session, "a finished task's agent closed");
            let free = state.projects.to_free(&project, task);
            closing.push((term, free.map(|(worktree, landed)| (project, task, worktree, landed))));
        }
        drop(guard);
        closing
            .into_iter()
            .map(|(term, free)| {
                self.close_and_free(term, true, free);
                term
            })
            .collect()
    }

    /// Close `term` when `open`, then once it is closed free the worktree its task's agent
    /// worked in on its worker, saying on the task's timeline how that went.
    pub(super) fn close_and_free(
        &self,
        term: TermRef,
        open: bool,
        free: Option<(ProjectId, TaskId, String, Vec<String>)>,
    ) {
        let Some((project, task, worktree, landed)) = free else {
            if open {
                self.close_soon(term);
            }
            return;
        };
        let hub = self.clone();
        tokio::spawn(async move {
            if open
                && let failed @ Outcome::Error { .. } =
                    hub.forward(None, Verb::Close { term }).await
            {
                tracing::debug!(?failed, "a terminal not closed");
                return;
            }
            let remove =
                Verb::RemoveWorktree { worker: term.worker, worktree: worktree.clone(), landed };
            let went = match hub.forward(None, remove).await {
                Outcome::WorktreeRemoved { branch, branch_removed } => Ok((branch, branch_removed)),
                other => Err(said(&other)),
            };
            let mut state = hub.inner.state.lock();
            let changes = state.projects.freed(&project, (task, &worktree), went, WallMs::now());
            hub.projects_moved(&mut state, changes);
            drop(state);
        });
    }
}

/// Whether the agent in `term` rests: at its prompt, done, gone from a terminal still open,
/// or the terminal's program ended. Busy, waiting on the person, or holding work or prompts
/// it scheduled, it does not.
fn rests(state: &State, term: TermRef) -> bool {
    if let Some(status) = state.board.seat_status(term) {
        return resting(&status);
    }
    let Some(session) = state
        .workers
        .get(&term.worker)
        .and_then(|e| e.sessions.iter().find(|s| s.id == term.session))
    else {
        return false;
    };
    if matches!(session.state, SessionState::Exited { .. }) {
        return true;
    }
    session.agent.as_ref().is_none_or(|agent| resting(&agent.status))
}

/// Whether an agent with `status` rests at its prompt.
const fn resting(status: &AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::None
            | AgentStatus::Idle
            | AgentStatus::Done
            | AgentStatus::Failed { .. }
            | AgentStatus::Blocked(BlockReason::IdlePrompt)
            | AgentStatus::Waiting { tasks: 0, crons: 0 }
    )
}
