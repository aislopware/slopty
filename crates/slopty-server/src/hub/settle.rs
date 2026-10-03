//! Finished tasks settle (`docs/decisions/projects.md`, "A finished task's agent stops
//! counting and is closed once it rests").
//!
//! A task merged or given up keeps its agent's terminal open for whoever wants to look back or
//! carry on in it, and it counts against no limit while its agent rests
//! ([`crate::project::Projects::fleet`]). Once that agent has rested [`SETTLE_AFTER`] at its
//! prompt, with no request open, nothing scheduled and its tile on no client's screen, the
//! server closes the terminal the server started for it, through the worker's own `Close`. The
//! agent's session stays, so it can be taken up again. A terminal the person put on a task is
//! theirs and is never closed, nor is anything still at work.

use std::collections::HashMap;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::agent::{AgentStatus, BlockReason};
use slopty_proto::orchestration::TermRef;
use slopty_proto::terminal::SessionState;
use tokio::time::Instant;

use super::projects::live;
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
            .finished_agents(&terminals)
            .into_iter()
            .filter(|(_, _, term)| {
                rests(state, *term) && !state.board.asks(*term) && !state.board.shown(*term)
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
            let changes = state.projects.settled(&project, task, mins, WallMs::now());
            self.projects_moved(state, changes);
            tracing::info!(%project, %task, session = %term.session, "a finished task's agent closed");
            closing.push(term);
        }
        drop(guard);
        for term in &closing {
            self.close_soon(*term);
        }
        closing
    }
}

/// Whether the agent in `term` rests: at its prompt, done, gone from a terminal still open,
/// or the terminal's program ended. Busy, waiting on the person, or holding work or prompts
/// it scheduled, it does not.
fn rests(state: &State, term: TermRef) -> bool {
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
    session.agent.as_ref().is_none_or(|agent| {
        matches!(
            agent.status,
            AgentStatus::None
                | AgentStatus::Idle
                | AgentStatus::Done
                | AgentStatus::Blocked(BlockReason::IdlePrompt)
                | AgentStatus::Waiting { tasks: 0, crons: 0 }
        )
    })
}
