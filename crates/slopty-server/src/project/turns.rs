//! What a task's agent came to, for the orchestrator (`docs/decisions/projects.md`, "A task's
//! outcome reaches the orchestrator without a report").
//!
//! A task's agent is meant to say how its work went with `task_report`, and often does not: it
//! ends its turn with its answer in its last words, it exits, or it waits on the person. Each
//! of those is something the project's orchestrator is to hear, from the server since the agent
//! said nothing:
//!
//! - it came to rest from a turn in which it did not report;
//! - it waits on the person: a permission, a question. Only the person answers it, so the
//!   orchestrator hears it only to stop waiting blindly;
//! - it exited, or its terminal closed, while its task still followed it.
//!
//! When it goes back to work, what the orchestrator was to hear no longer holds
//! ([`Upshot::Moved`]). Only a task whose state follows its agent is heard of: one verifying,
//! done, merged or given up is the merge queue's or the person's to speak of.
//!
//! Kept in memory. A server that comes back takes up each turn its store says was under way
//! ([`Projects::restore`]), so a turn that ended while it was away is still heard.

use std::collections::HashMap;

use slopty_agent::status::{AgentStatus, BlockReason};
use slopty_proto::orchestration::TermRef;

use super::{ProjectId, Projects, TaskId, open_term};
use crate::deliver::Kind;

/// Each task's agent's turn, by its terminal, and what the orchestrators are to hear.
#[derive(Debug, Default)]
pub(crate) struct Turns {
    by_term: HashMap<TermRef, Turn>,
    heard: Vec<Heard>,
}

/// Where one task's agent stands in its turn.
#[derive(Clone, Copy, Debug, Default)]
struct Turn {
    /// A turn is under way: it worked since it last came to rest.
    open: bool,
    /// The agent said its own word since the turn began.
    answered: bool,
    /// It waits on the person, and the orchestrator was to hear so.
    waits: bool,
    /// Its exit was heard.
    gone: bool,
}

/// Something a project's orchestrator is to hear of one of its tasks.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Heard {
    /// The project.
    pub project: ProjectId,
    /// The task.
    pub task: TaskId,
    /// Its agent's terminal, whose last words go with it.
    pub term: TermRef,
    /// What it came to.
    pub upshot: Upshot,
}

/// What a task's agent came to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Upshot {
    /// It came to rest from a turn without a word of its own.
    Rested,
    /// It waits on the person.
    Waits(BlockReason),
    /// It exited, or its terminal closed.
    Exited,
    /// It went back to work or moved on: what was to be heard of it no longer holds.
    Moved,
}

impl Turns {
    /// Take up a turn the store says was under way in `term` when the server stopped, and
    /// whether its agent had said its own word in it.
    pub(super) fn resume(&mut self, term: TermRef, answered: bool) {
        self.by_term.insert(term, Turn { open: true, answered, ..Turn::default() });
    }
}

impl Upshot {
    /// The kind of word it is delivered as, which says when it goes ([`crate::deliver`]);
    /// none for [`Self::Moved`], which takes back rather than adds.
    pub(crate) const fn kind(&self) -> Option<Kind> {
        match self {
            Self::Rested => Some(Kind::Done),
            Self::Waits(_) => Some(Kind::NeedsInput),
            Self::Exited => Some(Kind::Stuck),
            Self::Moved => None,
        }
    }
}

/// What an agent's status says of its turn.
enum Step {
    /// At work: thinking, a tool, its own background work.
    Busy,
    /// Waiting on the person.
    Waits(BlockReason),
    /// At rest: at its prompt, done, or holding only prompts it scheduled.
    Rest,
    /// No agent there any more.
    Gone,
}

fn step(status: &AgentStatus) -> Step {
    if status.works() {
        return Step::Busy;
    }
    match status {
        AgentStatus::None => Step::Gone,
        AgentStatus::Blocked(reason) if *reason != BlockReason::IdlePrompt => {
            Step::Waits(reason.clone())
        }
        _ => Step::Rest,
    }
}

impl Projects {
    /// The agent in `term` says `status`: what its task's turn came to, for the orchestrator.
    /// Read after the task followed the status.
    pub(super) fn turn(&mut self, term: TermRef, status: &AgentStatus) {
        let Some((project, task, follows)) = self.task_of(term) else { return };
        let heard = |upshot| Heard { project: project.clone(), task, term, upshot };
        let step = step(status);
        if matches!(step, Step::Gone) {
            // Only an agent seen there can have left: a terminal whose agent has not shown
            // yet says none too.
            if self.turns.by_term.contains_key(&term) {
                self.exited(term);
            }
            return;
        }
        let turn = self.turns.by_term.entry(term).or_default();
        match step {
            Step::Busy => {
                if !turn.open || turn.waits {
                    self.turns.heard.push(heard(Upshot::Moved));
                }
                // A turn begun afresh has said nothing yet; one going on keeps what it said.
                let answered = turn.open && turn.answered;
                *turn = Turn { open: true, answered, ..Turn::default() };
            }
            Step::Waits(reason) => {
                if !turn.waits && follows {
                    turn.waits = true;
                    self.turns.heard.push(heard(Upshot::Waits(reason)));
                }
            }
            Step::Rest => {
                let was_waiting = std::mem::take(&mut turn.waits);
                let rested = std::mem::take(&mut turn.open) && !turn.answered && follows;
                if was_waiting {
                    self.turns.heard.push(heard(Upshot::Moved));
                }
                if rested {
                    self.turns.heard.push(heard(Upshot::Rested));
                }
            }
            Step::Gone => {}
        }
    }

    /// The agent in `term` exited, or its terminal closed: heard once, while its task follows
    /// it. Read before its assignment ends.
    pub(crate) fn exited(&mut self, term: TermRef) {
        let Some((project, task, follows)) = self.task_of(term) else { return };
        let turn = self.turns.by_term.entry(term).or_default();
        let first = !turn.gone;
        *turn = Turn { gone: true, ..Turn::default() };
        if first && follows {
            self.turns.heard.push(Heard { project, task, term, upshot: Upshot::Exited });
        }
    }

    /// The terminal `term` closed: its turn is forgotten.
    pub(super) fn forget_turn(&mut self, term: TermRef) {
        self.turns.by_term.remove(&term);
    }

    /// `task`'s agent reported: its own answer for the turn under way, so what the server
    /// would have said of it is not said.
    pub(super) fn answered(&mut self, id: &ProjectId, task: TaskId) {
        let term = self.records.get(id).and_then(|r| r.task(task).ok()).and_then(open_term);
        if let Some(turn) = term.and_then(|t| self.turns.by_term.get_mut(&t)) {
            turn.answered = true;
        }
    }

    /// What the orchestrators are to hear since this was last asked, oldest first.
    pub(crate) fn heard(&mut self) -> Vec<Heard> {
        std::mem::take(&mut self.turns.heard)
    }

    /// The project and task of the task `term` works on, and whether its state follows its
    /// agent.
    fn task_of(&self, term: TermRef) -> Option<(ProjectId, TaskId, bool)> {
        let (id, task) = self.working_in(term)?;
        let t = self.records.get(id)?.task(task).ok()?;
        Some((id.clone(), task, t.state.follows_the_agent()))
    }
}
