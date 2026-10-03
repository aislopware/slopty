//! What a task's agent came to, for the node above it (`docs/decisions/projects.md`, "A
//! task's outcome reaches the node above it without a report").
//!
//! A task's agent is meant to say how its work went with `task_report`, and often does not: it
//! ends its turn with its answer in its last words, it exits, or it waits on the person. Each
//! of those is something its parent (the task above it, or the orchestrator) is to hear, from
//! the server since the agent said nothing:
//!
//! - it came to rest from a turn in which it said no word of its own (a need, a block or a finish:
//!   a checkpoint is progress, not an answer). Heard only once no task under it is still at work,
//!   since the result is not in while they work; held until then;
//! - it waits on the person: a permission, a question. Only the person answers it, so the parent
//!   hears it only to stop waiting blindly;
//! - it exited, or its terminal closed, while its task still followed it.
//!
//! When it goes back to work, what its parent was to hear no longer holds ([`Upshot::Moved`]).
//! Only a task whose state follows its agent is heard of: one verifying, done, merged or given
//! up is the merge queue's or the person's to speak of.
//!
//! Kept in memory only: after a restart an agent's next status starts afresh.

use std::collections::HashMap;

use slopty_proto::agent::{AgentStatus, BlockReason};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{ReportKind, Spent};

use super::{ProjectId, Projects, Record, TaskId, open_term};

/// Each task's agent's turn, by its terminal, and what the nodes above them are to hear.
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
    /// It came to rest unanswered while tasks under it worked: heard once they rest.
    held: bool,
    /// It waits on the person, and its parent was to hear so.
    waits: bool,
    /// Its exit was heard.
    gone: bool,
}

/// Something the node above a task is to hear of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Heard {
    /// The project.
    pub project: ProjectId,
    /// The task.
    pub task: TaskId,
    /// The node above it: its parent task, or the orchestrator's when absent.
    pub parent: Option<TaskId>,
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

impl Upshot {
    /// The kind of report it is delivered as, which paces it ([`crate::deliver`]); none for
    /// [`Self::Moved`], which takes back rather than adds.
    pub(crate) const fn kind(&self) -> Option<ReportKind> {
        match self {
            Self::Rested => Some(ReportKind::Done),
            Self::Waits(_) => Some(ReportKind::NeedsInput),
            Self::Exited => Some(ReportKind::Stuck),
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
    if Spent::works(status) {
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
    /// The agent in `term` says `status`: what its task's turn came to, for the node above it.
    /// Read after the task followed the status.
    pub(super) fn turn(&mut self, term: TermRef, status: &AgentStatus) {
        let Some((project, task, parent, follows)) = self.task_of(term) else { return };
        let heard = |upshot| Heard { project: project.clone(), task, parent, term, upshot };
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
                if !turn.open || turn.waits || turn.held {
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
                if std::mem::take(&mut turn.open) && !turn.answered && follows {
                    turn.held = true;
                }
                if was_waiting {
                    self.turns.heard.push(heard(Upshot::Moved));
                }
            }
            Step::Gone => {}
        }
        self.release_held(&project);
    }

    /// The agent in `term` exited, or its terminal closed: heard once, while its task follows
    /// it. Read before its assignment ends.
    pub(crate) fn exited(&mut self, term: TermRef) {
        let Some((project, task, parent, follows)) = self.task_of(term) else { return };
        let turn = self.turns.by_term.entry(term).or_default();
        let first = !turn.gone;
        *turn = Turn { gone: true, ..Turn::default() };
        if first && follows {
            let upshot = Upshot::Exited;
            self.turns.heard.push(Heard { project: project.clone(), task, parent, term, upshot });
        }
        self.release_held(&project);
    }

    /// The terminal `term` closed: its turn is forgotten.
    pub(super) fn forget_turn(&mut self, term: TermRef) {
        self.turns.by_term.remove(&term);
    }

    /// `task`'s agent reported `kind`: a need, a block or a finish is its own answer for the
    /// turn under way, and what the server would have said of it is not said.
    pub(super) fn answered(&mut self, id: &ProjectId, task: TaskId, kind: ReportKind) {
        if kind == ReportKind::Checkpoint {
            return;
        }
        let term = self.records.get(id).and_then(|r| r.task(task).ok()).and_then(open_term);
        if let Some(turn) = term.and_then(|t| self.turns.by_term.get_mut(&t)) {
            turn.answered = true;
            turn.held = false;
        }
    }

    /// What the nodes above tasks are to hear since this was last asked, oldest first.
    pub(crate) fn heard(&mut self) -> Vec<Heard> {
        std::mem::take(&mut self.turns.heard)
    }

    /// The project, task and parent of the task `term` works on, and whether its state
    /// follows its agent.
    fn task_of(&self, term: TermRef) -> Option<(ProjectId, TaskId, Option<TaskId>, bool)> {
        let (id, task) = self.working_in(term)?;
        let t = self.records.get(id)?.task(task).ok()?;
        Some((id.clone(), task, t.parent, t.state.follows_the_agent()))
    }

    /// Every rest of project `id` held for the tasks under it, heard once none of them is at
    /// work. One that is no longer followed (its task moved on) is let go unheard.
    fn release_held(&mut self, id: &ProjectId) {
        let Some(record) = self.records.get(id) else { return };
        let turns = &mut self.turns;
        let mut held: Vec<(usize, TermRef, TaskId, Option<TaskId>, bool)> = record
            .tasks
            .iter()
            .filter_map(|t| {
                let term = open_term(t)?;
                turns.by_term.get(&term).filter(|turn| turn.held)?;
                let depth = record.depth_under(t.parent);
                Some((depth, term, t.id, t.parent, t.state.follows_the_agent()))
            })
            .collect();
        // The deepest first, so a task's result comes before what its parent made of it.
        held.sort_by_key(|(depth, ..)| std::cmp::Reverse(*depth));
        for (_, term, task, parent, follows) in held {
            if follows && busy_under(record, &turns.by_term, task) {
                continue;
            }
            if let Some(turn) = turns.by_term.get_mut(&term) {
                turn.held = false;
            }
            if follows {
                let upshot = Upshot::Rested;
                turns.heard.push(Heard { project: id.clone(), task, parent, term, upshot });
            }
        }
    }
}

/// Whether any task split from `root`, however deep, has its agent at work.
fn busy_under(record: &Record, turns: &HashMap<TermRef, Turn>, root: TaskId) -> bool {
    record.tasks.iter().any(|t| {
        t.id != root
            && record.under(root, t.id)
            && open_term(t).and_then(|term| turns.get(&term)).is_some_and(|turn| turn.open)
    })
}
