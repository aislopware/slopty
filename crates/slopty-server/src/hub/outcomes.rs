//! What a task's agent came to, said to the orchestrator when the agent did not report
//! (`docs/decisions/projects.md`, "A task's outcome reaches the orchestrator without a
//! report"): the server's words, with the agent's last words from its thread's row.
//!
//! The rest of a turn settles before it goes ([`crate::deliver::DONE_SETTLE`]), so its words
//! are read again until then: the row with the agent's final line may come after the status
//! that ended the turn.

use slopty_agent::status::BlockReason;
use slopty_core::WallMs;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::TaskId;

use super::{Hub, State};
use crate::deliver::Kind;
use crate::project::{Change, Heard, Upshot, clipped};

/// The most of an agent's last words an outcome carries, in bytes.
pub(super) const LAST_WORDS_MAX: usize = 2048;

impl Hub {
    /// Hand what the orchestrators are to hear to the deliveries. A rest goes on the
    /// timeline too, as the task's move from running to waiting, so whoever follows the
    /// project (`task_wait`, a board) sees the turn end; the changes that makes come back.
    pub(super) fn hear(&self, state: &mut State) -> Vec<Change> {
        let heard = state.projects.heard();
        if heard.is_empty() {
            return Vec::new();
        }
        let at = tokio::time::Instant::now();
        let mut changes = Vec::new();
        for Heard { project, task, term, upshot } in heard {
            if upshot == Upshot::Rested {
                changes.extend(state.projects.rested(&project, task, WallMs::now()));
            }
            let node = (project, None);
            let Some(kind) = upshot.kind() else {
                state.deliveries.moved_on(&node, task);
                continue;
            };
            let words = words(state, task, term, &upshot);
            state.deliveries.outcome(node, task, kind, &words, at);
        }
        self.inner.deliver.notify_one();
        changes
    }

    /// Read again the words of every rest and wait still waiting: the row with the agent's
    /// last line, or with the request it waits on, may have come after its status.
    pub(super) fn reword_outcomes(state: &mut State) {
        let (projects, board) = (&state.projects, &state.board);
        state.deliveries.reword(|(project, _), task, kind| {
            let term = projects.task(project, task).ok()?.assignment.as_ref()?.term;
            match kind {
                Kind::Done => Some(rested_words(task, &board.last_words(term)?)),
                Kind::NeedsInput => Some(waits_words(task, &board.asking(term)?)),
                Kind::Stuck => None,
            }
        });
    }
}

/// What the orchestrator reads of what `task`'s agent in `term` came to.
fn words(state: &State, task: TaskId, term: TermRef, upshot: &Upshot) -> String {
    let last = state.board.last_words(term);
    match upshot {
        Upshot::Rested => rested_words(task, last.as_deref().unwrap_or_default()),
        Upshot::Waits(reason) => {
            waits_words(task, &state.board.asking(term).unwrap_or_else(|| waits_on(reason)))
        }
        Upshot::Exited => {
            let mut words = format!(
                "task {task}'s agent exited, or its terminal was closed, before it reported; \
                 nothing runs for it now."
            );
            push_last(&mut words, last.as_deref().unwrap_or_default());
            words
        }
        Upshot::Moved => String::new(),
    }
}

/// What a turn that came to rest with no report reads as, with the agent's `last` words.
fn rested_words(task: TaskId, last: &str) -> String {
    let mut words =
        format!("task {task} ended its turn without a task_report; its agent waits at its prompt.");
    push_last(&mut words, last);
    words
}

/// What a wait on the person reads as, naming `what` it waits on.
fn waits_words(task: TaskId, what: &str) -> String {
    format!(
        "task {task} waits on the person: {}. Only the person answers it; if it holds you up, \
         say so to the person rather than wait on it.",
        clipped(what.trim(), LAST_WORDS_MAX)
    )
}

fn push_last(words: &mut String, last: &str) {
    let last = last.trim();
    if last.is_empty() {
        return;
    }
    words.push_str(" Its last words:");
    for line in clipped(last, LAST_WORDS_MAX).lines() {
        words.push_str("\n  ");
        words.push_str(line);
    }
}

/// What an agent blocked for `reason` waits on, when its thread names no request.
fn waits_on(reason: &BlockReason) -> String {
    match reason {
        BlockReason::Permission { tool } => format!("a permission to use {tool}"),
        BlockReason::Question => "a question it asked".to_owned(),
        BlockReason::Elicitation => "a form a tools server asked it to fill".to_owned(),
        BlockReason::IdlePrompt => "its prompt".to_owned(),
    }
}
