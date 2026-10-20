//! slopty: the rows Claude Code's TUI folds or adds around a turn, which the transcript's own
//! items do not make (`.research/tui-strings-2026-10-16.md` §2.Queue, §3.Done, §5.Stop):
//!
//! - **Task notices together.** Three or more background tasks that completed one after another
//!   read as the first two, then "+N more tasks completed", whose rows open under it. Background
//!   agents stopped together read as one row naming them ("2 background agents were stopped: …"),
//!   or give way to the agent's own row that names them. A failure is never folded.
//! - **The turn's done line.** A turn the agent finished ends on a quiet "Worked for 1m 5s · done
//!   3:42 PM", the TUI's line with its plain verb. What the turn left running is not repeated
//!   there: the agent's count is a snapshot at the turn's end, and the Background tray says what
//!   runs now, by kind (`docs/decisions/ui.md`, "What runs is said once, by the tray").

use chrono::TimeZone;
use slopty_proto::thread::{Notice, ThreadState, TurnId, TurnState};
use slopty_view_doc::{
    MessagePart, MessageRole, NoteLine, NoteRow, SessionMessageEntry, TaskRow, WorkRow,
    duration_words,
};

/// How many task notices in a row show before the rest fold.
const TASKS_SHOWN: usize = 2;

/// The kind of the row that folds task notices.
pub const TASKS_FOLD: &str = "tasks";

/// The kind of a turn's done line.
pub const DONE: &str = "done";

/// What a turn's done line says: how long it worked and the time it ended, in `tz`.
#[must_use]
pub fn done_words<Tz: TimeZone>(duration_ms: u64, ended_ms: i64, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let worked = duration_words(duration_ms.max(1_000));
    let clock = chrono::DateTime::from_timestamp_millis(ended_ms).map(|utc| {
        utc.with_timezone(tz).format(slopty_shell::transcript::clock_format()).to_string()
    });
    match clock {
        Some(clock) => format!("Worked for {worked} · done {clock}"),
        None => format!("Worked for {worked}"),
    }
}

/// The kind of the row over agents started together.
pub const AGENTS: &str = "agents";

/// Whether `part` is the card of a call that started a subagent; its kind when it says.
fn spawn(part: &MessagePart) -> Option<(bool, Option<&str>)> {
    match part {
        MessagePart::Tool { call, resolved, facts, .. } if call.is_subagent_spawn() => {
            let progress = facts.as_ref().and_then(|f| f.subagent_progress.as_ref());
            let running = progress.map_or(!*resolved, |p| p.running);
            Some((running, progress.and_then(|p| p.agent_type.as_deref())))
        }
        _ => None,
    }
}

/// slopty: agents started together read under one row, as the TUI groups them: "Running 3
/// Explore agents…" while any runs, "3 agents finished" once all are done. Each keeps its card.
pub fn agent_groups(parts: &mut Vec<MessagePart>) {
    let mut ix = 0;
    while ix < parts.len() {
        let run = parts.iter().skip(ix).map_while(spawn).count();
        if run < 2 {
            ix = ix.saturating_add(run.max(1));
            continue;
        }
        let spawns: Vec<(bool, Option<&str>)> =
            parts.iter().skip(ix).take(run).filter_map(spawn).collect();
        let running = spawns.iter().any(|(running, _)| *running);
        let kinds: Vec<Option<&str>> = spawns.iter().map(|(_, kind)| *kind).collect();
        let noun = match kinds.first().copied().flatten() {
            Some(kind) if kinds.iter().all(|k| *k == Some(kind)) => format!("{kind} agents"),
            _ => "agents".to_owned(),
        };
        let text = if running {
            format!("Running {run} {noun}…")
        } else {
            format!("{run} {noun} finished")
        };
        let first = parts.get(ix).map(|p| p.id().to_owned()).unwrap_or_default();
        let note = NoteRow {
            kind: AGENTS.to_owned(),
            text,
            tone: NoteLine::INFO.to_owned(),
            ..NoteRow::default()
        };
        parts.insert(ix, MessagePart::Note { id: format!("{first}:agents"), note: Box::new(note) });
        ix = ix.saturating_add(run).saturating_add(1);
    }
}

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

/// Each finished turn of `state` ends on its done line, in the last entry that answers it.
///
/// `entries` come with `turns` saying which turn each answers. The turn's own footer notices
/// (what it left running) give way to that line, which says the turn's time alone: what still
/// runs is the Background tray's to say, live.
pub fn done_lines<Tz: TimeZone>(
    state: &ThreadState,
    entries: &mut [SessionMessageEntry],
    turns: &[Option<TurnId>],
    tz: &Tz,
) where
    Tz::Offset: std::fmt::Display,
{
    // A subagent's run is shown inside its parent's turn, which says when it finished.
    if state.meta.parent.is_some() {
        return;
    }
    for turn in &state.turns {
        let (TurnState::Complete, Some(ended)) = (&turn.state, turn.ended_ms) else { continue };
        let Some(at) = turns.iter().rposition(|t| *t == Some(turn.id)) else { continue };
        let Some(entry) = entries.get_mut(at) else { continue };
        if entry.role != MessageRole::Assistant {
            continue;
        }
        entry.parts.retain(
            |part| !matches!(part, MessagePart::Note { note, .. } if note.kind == Notice::TURN),
        );
        let worked = ended.as_millis().saturating_sub(turn.started_ms.as_millis());
        let ended_at = i64::try_from(ended.as_millis()).unwrap_or(i64::MAX);
        // slopty: the turn's divider says the same time as its done line, which it folds in.
        entry.duration_ms = Some(i64::try_from(worked.max(1_000)).unwrap_or(i64::MAX));
        let text = done_words(worked, ended_at, tz);
        entry.parts.push(MessagePart::Note {
            id: format!("{}:done", entry.id),
            note: Box::new(NoteRow {
                kind: DONE.to_owned(),
                text,
                tone: NoteLine::INFO.to_owned(),
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

    /// The done line says how long the turn worked and when it ended, in the person's time.
    #[test]
    fn the_done_line_says_its_time_and_clock() {
        let tz = chrono::FixedOffset::east_opt(0).expect("utc");
        // 2026-07-01 15:42 UTC.
        let ended = 1_782_920_520_000;
        // On the system's clock.
        let clock = if slopty_shell::transcript::clock_format().contains('p') {
            "3:42 PM"
        } else {
            "15:42"
        };
        assert_eq!(done_words(65_000, ended, &tz), format!("Worked for 1m 5s · done {clock}"));
        assert_eq!(done_words(200, ended, &tz), format!("Worked for 1s · done {clock}"));
        assert_eq!(done_words(60_000, ended, &tz), format!("Worked for 1m · done {clock}"));
    }

    fn spawn_card(id: &str, kind: &str, running: bool) -> MessagePart {
        let progress = slopty_view_doc::SubagentProgress {
            agent_type: Some(kind.to_owned()),
            running,
            ..slopty_view_doc::SubagentProgress::default()
        };
        MessagePart::Tool {
            id: id.to_owned(),
            call: slopty_view_proto::ToolCall::Unknown { name: "Agent".to_owned(), input: None },
            is_error: false,
            resolved: !running,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            facts: Some(Box::new(slopty_view_doc::ToolFacts {
                subagent_progress: Some(progress),
                ..slopty_view_doc::ToolFacts::default()
            })),
        }
    }

    /// Agents started together read under one row while any runs and once all are done, by
    /// their kind when they share one; a lone agent has no row over it.
    #[test]
    fn agents_started_together_read_under_one_row() {
        let mut parts = vec![spawn_card("a", "Explore", true), spawn_card("b", "Explore", false)];
        agent_groups(&mut parts);
        assert_eq!(said(&parts)[0], "agents: Running 2 Explore agents…");
        assert_eq!(parts.len(), 3, "each keeps its card");
        let mut parts = vec![spawn_card("a", "Explore", false), spawn_card("b", "Plan", false)];
        agent_groups(&mut parts);
        assert_eq!(said(&parts)[0], "agents: 2 agents finished");
        let mut lone = vec![spawn_card("a", "Explore", true), row("t", "command", "running", "x")];
        agent_groups(&mut lone);
        assert_eq!(lone.len(), 2, "{:?}", said(&lone));
    }
}
