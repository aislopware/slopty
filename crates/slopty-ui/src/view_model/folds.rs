//! The rows Claude Code's TUI folds or adds around a turn, which the transcript's own
//! items do not make (`.research/tui-strings-2026-10-16.md` §2.Queue, §3.Done, §5.Stop):
//!
//! - **Task notices together.** Three or more background tasks that completed one after another
//!   read as the first two, then "+N more tasks completed", whose rows open under it. Background
//!   agents stopped together read as one row naming them ("2 background agents were stopped: …"),
//!   or give way to the agent's own row that names them. A failure is never folded.
//! - **The turn's figures.** The prompt that opened a turn carries the turn's figures, which its
//!   row says (`docs/decisions/inspector.md`, "The turn is the root row"): its time there is the
//!   TUI's "Worked for 1m 5s", said once. What the turn left running is not said at its end: the
//!   agent's count is a snapshot at the turn's end, and the Background tray says what runs now, by
//!   kind (`docs/decisions/ui.md`, "What runs is said once, by the tray").

use slopty_proto::thread::{Notice, ThreadState, TurnId, TurnState};
use slopty_view_doc::{
    MessagePart, MessageRole, NoteLine, NoteRow, SessionMessageEntry, TaskRow, TurnFacts, WorkRow,
};

/// How many task notices in a row show before the rest fold.
const TASKS_SHOWN: usize = 2;

/// The kind of the row that folds task notices.
pub const TASKS_FOLD: &str = "tasks";

/// A part that is one of a run of task notices, or the agent's own row naming the agents it
/// stopped together.
fn in_run(part: &MessagePart) -> bool {
    match part {
        MessagePart::Task { .. } => true,
        MessagePart::Note { note, .. } => note.kind == Notice::STOPPED,
        _ => false,
    }
}

fn task(part: &MessagePart) -> Option<&TaskRow> {
    match part {
        MessagePart::Task { task, .. } => Some(task),
        _ => None,
    }
}

/// A background agent stopped.
fn agent_stopped(row: &TaskRow) -> bool {
    row.kind == WorkRow::SUBAGENT && row.status == "killed"
}

/// What a task's row says in a fold's list.
fn row_words(row: &TaskRow) -> String {
    if row.title.is_empty() { row.text.clone() } else { format!("{} · {}", row.text, row.title) }
}

/// A run of task notices as the TUI shows it.
fn fold_run(run: Vec<MessagePart>) -> Vec<MessagePart> {
    let named = run.iter().any(|part| matches!(part, MessagePart::Note { .. }));
    let stopped: Vec<&TaskRow> = run.iter().filter_map(task).filter(|t| agent_stopped(t)).collect();
    let merge_stopped = !named && stopped.len() >= 2;
    let together = merge_stopped.then(|| {
        let names: Vec<String> = stopped
            .iter()
            .map(|t| format!("\"{}\"", if t.title.is_empty() { &t.task } else { &t.title }))
            .collect();
        NoteRow {
            kind: Notice::STOPPED.to_owned(),
            text: format!("{} background agents were stopped: {}", names.len(), names.join(", ")),
            tone: NoteLine::WARNING.to_owned(),
            ..NoteRow::default()
        }
    });
    let mut out = Vec::new();
    let mut together = together;
    let mut completed: Vec<MessagePart> = Vec::new();
    let flush = |completed: &mut Vec<MessagePart>, out: &mut Vec<MessagePart>| {
        if completed.len() <= TASKS_SHOWN.saturating_add(1) {
            out.append(completed);
            return;
        }
        let rest = completed.split_off(TASKS_SHOWN);
        out.append(completed);
        let first = rest.first().map(part_id).unwrap_or_default();
        let detail: Vec<String> = rest.iter().filter_map(task).map(row_words).collect();
        let n = rest.len();
        out.push(MessagePart::Note {
            id: format!("{first}:fold"),
            note: Box::new(NoteRow {
                kind: TASKS_FOLD.to_owned(),
                text: format!("+{n} more tasks completed"),
                tone: NoteLine::INFO.to_owned(),
                detail: Some(detail.join("\n")),
                ..NoteRow::default()
            }),
        });
    };
    for part in run {
        let row = task(&part);
        if row.is_some_and(|t| t.status == "completed") {
            completed.push(part);
            continue;
        }
        flush(&mut completed, &mut out);
        if row.is_some_and(agent_stopped) && (named || merge_stopped) {
            // Named by the agent's own row, or by the one row for them all, here at the first.
            if let Some(note) = together.take() {
                out.push(MessagePart::Note {
                    id: format!("{}:stopped", part_id(&part)),
                    note: Box::new(note),
                });
            }
            continue;
        }
        out.push(part);
    }
    flush(&mut completed, &mut out);
    out
}

fn part_id(part: &MessagePart) -> String {
    match part {
        MessagePart::Task { id, .. } | MessagePart::Note { id, .. } => id.clone(),
        _ => String::new(),
    }
}

/// `parts` with each run of task notices folded as the TUI shows it (`fold_run`).
#[must_use]
pub fn fold_tasks(parts: Vec<MessagePart>) -> Vec<MessagePart> {
    let mut out = Vec::with_capacity(parts.len());
    let mut run = Vec::new();
    for part in parts {
        if in_run(&part) {
            run.push(part);
            continue;
        }
        if !run.is_empty() {
            out.extend(fold_run(std::mem::take(&mut run)));
        }
        out.push(part);
    }
    if !run.is_empty() {
        out.extend(fold_run(run));
    }
    out
}

/// Each turn a prompt opened carries its figures in that prompt's entry ([`TurnFacts`]), which
/// the prompt's row says (`docs/decisions/inspector.md`, "The turn is the root row").
///
/// `entries` come with `turns` saying which turn each answers. A finished turn's own footer
/// notices (what it left running) are left out: what still runs is the Background tray's to say,
/// live.
pub fn turn_roots(
    state: &ThreadState,
    entries: &mut [SessionMessageEntry],
    turns: &[Option<TurnId>],
) {
    // A subagent's run is one step of its parent's turn, whose drill header says its figures.
    if state.meta.parent.is_some() {
        return;
    }
    for turn in &state.turns {
        if turn.state == TurnState::Complete
            && let Some(at) = turns.iter().rposition(|t| *t == Some(turn.id))
            && let Some(entry) =
                entries.get_mut(at).filter(|entry| entry.role == MessageRole::Assistant)
        {
            entry.parts.retain(
                |part| !matches!(part, MessagePart::Note { note, .. } if note.kind == Notice::TURN),
            );
        }
        let Some(input) = &turn.input else { continue };
        let Some(entry) =
            entries.iter_mut().find(|entry| entry.role == MessageRole::User && entry.id == input.0)
        else {
            continue;
        };
        let (word, error) = match &turn.state {
            TurnState::Active => (TurnFacts::ACTIVE, None),
            TurnState::Complete => (TurnFacts::COMPLETE, None),
            TurnState::Interrupted => (TurnFacts::INTERRUPTED, None),
            TurnState::Failed { error } => (TurnFacts::FAILED, Some(error.clone())),
        };
        let facts = TurnFacts {
            state: word.to_owned(),
            error,
            started: super::transcript::at(turn.started_ms),
            ended: turn.ended_ms.map(super::transcript::at),
            models: turn.models.clone(),
            tokens: turn.usage.0.clone(),
            changed: [turn.changed.added, turn.changed.removed],
        };
        entry.parts.push(MessagePart::Note {
            id: format!("{}:turn", entry.id),
            note: Box::new(NoteRow {
                kind: TurnFacts::KIND.to_owned(),
                turn: Some(Box::new(facts)),
                ..NoteRow::default()
            }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: &str, status: &str, title: &str) -> MessagePart {
        MessagePart::Task {
            id: id.to_owned(),
            task: Box::new(TaskRow {
                task: id.to_owned(),
                kind: kind.to_owned(),
                status: status.to_owned(),
                text: format!("{title} {status}"),
                title: title.to_owned(),
                ..TaskRow::default()
            }),
        }
    }

    fn said(parts: &[MessagePart]) -> Vec<String> {
        parts
            .iter()
            .map(|part| match part {
                MessagePart::Task { task, .. } => format!("task {}", task.task),
                MessagePart::Note { note, .. } => format!("{}: {}", note.kind, note.text),
                _ => "other".to_owned(),
            })
            .collect()
    }

    /// Completions past the second fold under "+N more tasks completed", which lists them; a
    /// failure among them breaks the run and stays in sight; two completions never fold.
    #[test]
    fn completions_fold_past_the_second_and_failures_never() {
        let parts: Vec<MessagePart> = (1..=5)
            .map(|n| row(&format!("t{n}"), WorkRow::COMMAND, "completed", &format!("job {n}")))
            .collect();
        let folded = fold_tasks(parts);
        assert_eq!(said(&folded), ["task t1", "task t2", "tasks: +3 more tasks completed"]);
        let MessagePart::Note { note, .. } = &folded[2] else { panic!("a fold") };
        assert_eq!(
            note.detail.as_deref(),
            Some("job 3 completed · job 3\njob 4 completed · job 4\njob 5 completed · job 5")
        );
        let mixed = vec![
            row("a", WorkRow::COMMAND, "completed", "a"),
            row("b", WorkRow::COMMAND, "failed", "b"),
            row("c", WorkRow::COMMAND, "completed", "c"),
            row("d", WorkRow::COMMAND, "completed", "d"),
        ];
        assert_eq!(said(&fold_tasks(mixed)), ["task a", "task b", "task c", "task d"]);
    }

    /// Background agents stopped together are one row naming them, or give way to the agent's
    /// own row that does; one alone keeps its row.
    #[test]
    fn agents_stopped_together_are_one_row() {
        let two = vec![
            row("x1", WorkRow::SUBAGENT, "killed", "Survey"),
            row("x2", WorkRow::SUBAGENT, "killed", "Review"),
        ];
        assert_eq!(
            said(&fold_tasks(two.clone())),
            ["stopped: 2 background agents were stopped: \"Survey\", \"Review\""]
        );
        let mut with_note = vec![MessagePart::Note {
            id: "n".to_owned(),
            note: Box::new(NoteRow {
                kind: Notice::STOPPED.to_owned(),
                text: "2 background agents were stopped: \"Survey\", \"Review\"".to_owned(),
                ..NoteRow::default()
            }),
        }];
        with_note.extend(two);
        assert_eq!(said(&fold_tasks(with_note)).len(), 1);
        let one = vec![row("x1", WorkRow::SUBAGENT, "killed", "Survey")];
        assert_eq!(said(&fold_tasks(one)), ["task x1"]);
    }
}
