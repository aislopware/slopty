//! What a thread does besides its conversation, as the shell's inspector reads it.
//!
//! Its background work beside the child chats it started ([`tray`]); and the agent's own prompt
//! box, mirrored ([`dock`]).
//!
//! Pure: the workspace hands in a thread's state and the rows of the threads it started, and
//! publishes what comes out (`workspace::view_model::work`).

use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{BackgroundTask, Notice, Phase, ThreadId, ThreadState};
use slopty_view_doc::{
    ChatDock, PromptBox, PromptImage, RetryLine, WorkRow, WorkTray, tokens_short,
};

use super::transcript::{at, image_path};

/// A chat another agent started under this thread, as its row says it: Claude Code's own
/// subagent of a call, or an agent started through Slopty's verbs.
#[derive(Clone, Copy, Debug)]
pub struct Child<'a> {
    /// Its row in its worker's table.
    pub row: &'a ThreadRow,
}

/// The tray's row id of the agent's task `task`.
#[must_use]
pub fn task_row(task: &str) -> String {
    format!("task:{task}")
}

/// The tray's row id of the child chat `chat`.
#[must_use]
pub fn chat_row(chat: &str) -> String {
    format!("chat:{chat}")
}

/// A background task's kind as the tray's, and its noun.
#[must_use]
pub fn kind_of(kind: &str) -> (&str, &'static str) {
    match kind {
        BackgroundTask::SHELL => (WorkRow::COMMAND, "command"),
        BackgroundTask::AGENT => (WorkRow::SUBAGENT, "subagent"),
        BackgroundTask::MONITOR => (WorkRow::MONITOR, "monitor"),
        BackgroundTask::WORKFLOW => (WorkRow::WORKFLOW, "workflow"),
        BackgroundTask::MCP => (WorkRow::MCP, "MCP task"),
        BackgroundTask::REMOTE => (WorkRow::REMOTE, "remote agent"),
        BackgroundTask::TEAMMATE => (WorkRow::TEAMMATE, "teammate"),
        other => (other, "background task"),
    }
}

/// A task's state in the tray's words.
#[must_use]
pub fn state_words(state: &str) -> String {
    match state {
        BackgroundTask::RUNNING => "running",
        BackgroundTask::PENDING => "queued",
        BackgroundTask::PAUSED => "paused",
        BackgroundTask::WAITING => "waiting",
        BackgroundTask::COMPLETED => "done",
        BackgroundTask::FAILED => "error",
        BackgroundTask::KILLED => "stopped",
        NEEDS_YOU => "needs you",
        other => other,
    }
    .to_owned()
}

/// A child chat's state when it waits on the person.
const NEEDS_YOU: &str = "needs-you";

/// Figures in words, time left out: "12 tool uses · 41k tokens".
#[must_use]
pub fn figures(tool_uses: Option<u64>, tokens: Option<u64>) -> String {
    let mut out = Vec::new();
    match tool_uses {
        Some(1) => out.push("1 tool use".to_owned()),
        Some(n) if n > 0 => out.push(format!("{n} tool uses")),
        _ => {}
    }
    if let Some(tokens) = tokens.filter(|n| *n > 0) {
        out.push(format!("{} tokens", tokens_short(tokens)));
    }
    out.join(" · ")
}

/// A child chat's state, as a task's would say it.
const fn child_state(row: &ThreadRow) -> &'static str {
    match row.status.phase {
        Phase::Working => BackgroundTask::RUNNING,
        Phase::Waiting => BackgroundTask::WAITING,
        Phase::NeedsYou => NEEDS_YOU,
        Phase::Failed => BackgroundTask::FAILED,
        Phase::Stopped => BackgroundTask::KILLED,
        Phase::Idle | Phase::Done => BackgroundTask::COMPLETED,
    }
}

/// What a child chat asks of the person now, in a line, while it does: its wait's words.
#[must_use]
pub fn child_asks(row: &ThreadRow) -> Option<String> {
    if row.status.phase != Phase::NeedsYou {
        return None;
    }
    let words = row.status.wait.as_ref().map_or("", |wait| wait.text.trim());
    Some(if words.is_empty() { "Needs your answer".to_owned() } else { words.to_owned() })
}

/// A row on its way into the tray, with what places it there.
struct Work {
    row: WorkRow,
    /// It is still at work in any way.
    live: bool,
    /// Housekeeping of the agent's own (a memory pass), which the tray leaves out.
    ambient: bool,
    /// It waits on the person.
    needs_you: bool,
    /// When it began.
    started: i64,
    /// The row that started it, for work a subagent started.
    parent: Option<String>,
}

/// `state`'s background work at work now beside the chats it started (`children`), as the tray
/// lists it. `chat_of` names a thread by the id the shell knows its chat by.
#[must_use]
pub fn tray(
    state: &ThreadState,
    children: &[Child<'_>],
    chat_of: &dyn Fn(ThreadId) -> String,
) -> WorkTray {
    // A task's chat: the one it names, else the one its call started (a subagent taken up again
    // names none).
    let child_of = |task: &BackgroundTask| {
        children.iter().find(|c| {
            task.child == Some(c.row.id)
                || task.item.as_ref().is_some_and(|item| {
                    c.row.parent.as_ref().is_some_and(|link| link.item == *item)
                })
        })
    };
    let mut work: Vec<Work> = Vec::new();
    for task in &state.tasks {
        let (kind, noun) = kind_of(&task.kind);
        let child = child_of(task);
        let needs_you = child.is_some_and(|c| c.row.status.phase == Phase::NeedsYou);
        work.push(Work {
            row: WorkRow {
                id: task_row(&task.id),
                kind: kind.to_owned(),
                noun: noun.to_owned(),
                title: task.title.clone(),
                state_words: state_words(if needs_you { NEEDS_YOU } else { &task.state }),
                step: task.step.clone().or_else(|| child.and_then(|c| c.row.doing.clone())),
                chat_id: child.map(|c| chat_of(c.row.id)),
            },
            live: task.is_live(),
            ambient: task.ambient,
            needs_you,
            started: at(task.started_ms),
            parent: task.parent.as_deref().map(task_row),
        });
    }
    // The chats it started that no task stands for.
    for child in children {
        if state.tasks.iter().any(|task| child_of(task).is_some_and(|c| c.row.id == child.row.id)) {
            continue;
        }
        let row = child.row;
        let needs_you = row.status.phase == Phase::NeedsYou;
        let live = matches!(row.status.phase, Phase::Working | Phase::Waiting | Phase::NeedsYou);
        let chat = chat_of(row.id);
        work.push(Work {
            row: WorkRow {
                id: chat_row(&chat),
                kind: WorkRow::SUBAGENT.to_owned(),
                noun: "subagent".to_owned(),
                title: row.title.clone(),
                state_words: state_words(if needs_you { NEEDS_YOU } else { child_state(row) }),
                step: row.doing.clone().or_else(|| row.last_line.clone()).filter(|_| live),
                chat_id: Some(chat),
            },
            live,
            ambient: false,
            needs_you,
            started: at(row.began_ms.filter(|_| live).unwrap_or(row.created_ms)),
            parent: None,
        });
    }
    // One row per piece of work, the last word on it: a task the thread names twice (its start
    // and its stream) is listed once, and one that ended since is gone.
    let mut work = distinct(work);
    work.retain(|one| one.live && !one.ambient);
    WorkTray { rows: order(&work) }
}

/// `work` with one row per id, the last word on each kept where the first stood.
fn distinct(work: Vec<Work>) -> Vec<Work> {
    let mut out: Vec<Work> = Vec::with_capacity(work.len());
    for one in work {
        match out.iter_mut().find(|kept| kept.row.id == one.row.id) {
            Some(kept) => *kept = one,
            None => out.push(one),
        }
    }
    out
}

/// The rows in the tray's order: what needs the person, then by when it began; each row's own
/// work after it.
fn order(work: &[Work]) -> Vec<WorkRow> {
    let mut top: Vec<&Work> = work
        .iter()
        .filter(|one| one.parent.as_ref().is_none_or(|p| !work.iter().any(|w| w.row.id == *p)))
        .collect();
    top.sort_by_key(|one| (!one.needs_you, one.started));
    let mut out = Vec::with_capacity(work.len());
    for one in top {
        place(one, 0, work, &mut out);
    }
    out
}

/// `one`, then the work it started, at most a few deep.
fn place(one: &Work, depth: u32, work: &[Work], out: &mut Vec<WorkRow>) {
    if out.iter().any(|placed| placed.id == one.row.id) || depth > 8 {
        return;
    }
    out.push(one.row.clone());
    for child in work.iter().filter(|w| w.parent.as_deref() == Some(one.row.id.as_str())) {
        place(child, depth.saturating_add(1), work, out);
    }
}

/// What a failed request is, in the TUI's first words: "API error", "Overloaded", a limit by
/// its name.
fn retry_what(text: &str) -> String {
    let lower = text.to_lowercase();
    if lower.starts_with("no response from the api after") {
        text.trim().to_owned()
    } else if lower.contains("overloaded") || lower.contains("529") {
        "Overloaded".to_owned()
    } else if lower.contains("rate limit") || lower.contains("429") {
        "Rate limited".to_owned()
    } else if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("no response")
    {
        "No response from the API".to_owned()
    } else {
        "API error".to_owned()
    }
}

/// The status page the TUI points at when an overload persists, from the third attempt.
const STATUS_PAGE: &str = "If it persists, check status.claude.com.";

/// A retry notice as the line under way says it.
#[must_use]
pub fn retry_line(notice: &Notice, at_ms: i64) -> Option<RetryLine> {
    let retry = notice.retry?;
    let what = retry_what(&notice.text.text);
    let overloaded = what == "Overloaded";
    Some(RetryLine {
        attempt: retry.attempt,
        max: retry.max,
        at: retry
            .in_ms
            .and_then(|ms| i64::try_from(ms).ok())
            .map(|ms| at_ms.saturating_add(ms))
            .filter(|_| at_ms > 0),
        what,
        hint: (overloaded && retry.attempt >= 3).then(|| STATUS_PAGE.to_owned()),
    })
}

/// The item a draft's pictures are read back under ([`image_path`]). An agent's items go by its
/// records' ids (a UUID, a tool call's id), never this word.
pub const DRAFT_ITEM: &str = "draft";

/// `state`'s dock: the agent's prompt box as its TUI holds it, while it holds anything.
#[must_use]
pub fn dock(state: &ThreadState) -> ChatDock {
    let (text, images) = state.draft.as_ref().map_or_else(Default::default, |draft| {
        let images = draft
            .images
            .iter()
            .enumerate()
            .map(|(index, pasted)| PromptImage {
                number: pasted.n,
                path: image_path(state.meta.id, DRAFT_ITEM, index, &pasted.image),
                mime_type: pasted.image.media_type.clone(),
            })
            .collect();
        (draft.text.clone(), images)
    });
    let prompt = PromptBox { queued: state.queued.clone(), text, images };
    ChatDock { prompt: (!prompt.is_empty()).then_some(prompt) }
}

#[cfg(test)]
mod tests;
