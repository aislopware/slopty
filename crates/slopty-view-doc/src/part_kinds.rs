//! slopty: what Slopty's transcript parts carry beyond zeron's: a call's facts in the TUI's
//! words, a subagent's progress on its spawn card, a background task's notice row and a quiet
//! notice row (Slopty's own; not in zeron).
//!
//! The view model fills them from the thread (`slopty_ui::view_model::transcript`), so the shell
//! draws the words here as they are: each is the TUI's own line for the same thing
//! (`.research/tui-strings-2026-10-16.md`), said once for every surface.

use serde::{Deserialize, Serialize};

use crate::dock::RetryLine;

/// What a tool call says beyond zeron's chip ([`crate::MessagePart::Tool`]'s `facts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolFacts {
    /// The call in the TUI's words, for a chip zeron draws as `Tool · {name}`:
    /// `Monitor(lane stall watch)`, `SendMessage → reviewer`, `ScheduleWakeup · in 5m`,
    /// `Skill(review)`. `None` where zeron's own chip already says it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What came of it, in a line, as the TUI's `⎿` line says it: "Read 120 lines", "Found 3
    /// files", "Found 12 lines across 3 files", "Received 12.3k (200 OK)", "exit code 2",
    /// "Running in the background · task bx7", "Monitor started · task bx7 · timeout 30m",
    /// "Interrupted", "Denied by auto mode classifier · rm is destructive".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// It ended badly in a way its red tint alone does not say: the result line reads as the
    /// failure.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
    /// How long it has run, while it runs: the TUI's "Running… (12s · timeout 2m)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer: Option<ToolTimer>,
    /// The tray row of the background task it started or became (`task:{id}`), which the chip
    /// links to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_row: Option<String>,
    /// It blocks the turn and can be moved to the background (`slopty.Background` with this
    /// part's id): "Run in background".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backgroundable: bool,
    /// A subagent's request tried again inside the call, while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryLine>,
    /// For a call that started a subagent: how it is getting on, for its card's step line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_progress: Option<SubagentProgress>,
    /// The chip's detail where it says a file's path shorter than zeron's whole path: inside the
    /// chat's folder relative to it, else `~/…`, its middle cut to keep the file's name
    /// (`src/…/view_model/facts.rs`), as the TUI's header shows a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What a command wrote to its standard error, apart from its output: drawn under the
    /// output in the danger colour, as the TUI does. Claude Code's own notes in it (the
    /// sandbox's report, "Shell cwd was reset to …") are left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// The lines an edit or a write added and removed, which its group's line totals: "edited
    /// 2 files +12 -3", as the TUI's group line counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<[u64; 2]>,
    /// What a command landed, as the TUI's group line says it: "committed 1a2b3c4", "pushed to
    /// main", "rebased onto main", "created PR #12".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<String>,
    /// For a thought riding a group: how long the model thought, which the group's line says
    /// ("thought for 12s").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_ms: Option<u64>,
}

/// How long a call has run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolTimer {
    /// How long it had run when the agent last said, in ms.
    pub elapsed_ms: u64,
    /// When the agent said so: the shell ticks on from here.
    pub at: i64,
    /// Its time limit, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// A subagent's progress, on the card of the call that started it.
///
/// Collapsed, the card reads "{agentType} · {step}" and "{figures} · {elapsed}"; it settles to
/// its outcome and figures once the subagent ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentProgress {
    /// Its kind (`general-purpose`, `Explore`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// The model its kind runs on, when the agent names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// It is still at work.
    pub running: bool,
    /// Where it stands: [`SubagentProgress::RUNNING`], [`SubagentProgress::WAITING`] (on its
    /// own background work), [`SubagentProgress::NEEDS_YOU`], [`SubagentProgress::DONE`],
    /// [`SubagentProgress::FAILED`], [`SubagentProgress::STOPPED`].
    pub state: String,
    /// What it is doing now, in a phrase: its own progress summary, else its latest call's
    /// title ("Edit src/main.rs"), else its last words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Tool calls it made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    /// Tokens it took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// How long it ran, once it says; a running one ticks from [`Self::started_at`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// When it began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    /// Its figures in words, time left out: "12 tools · 41k tokens"; empty with none.
    pub figures: String,
    /// How it ended, once it did, in words: "Done", "Failed: out of turns", "Stopped",
    /// "Waiting on its own background work".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// What it asks of the person, while it waits on them: answered in this chat's dock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you: Option<String>,
    /// Its run's chat, where Slopty keeps one: what expands in place and drills in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Its tray row (`task:{id}` or `chat:{chatId}`), which `slopty.StopWork` stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
}

impl SubagentProgress {
    /// It finished.
    pub const DONE: &'static str = "done";
    /// It failed.
    pub const FAILED: &'static str = "failed";
    /// It waits on the person: its request is answered in this chat's dock.
    pub const NEEDS_YOU: &'static str = "needs-you";
    /// It is at work.
    pub const RUNNING: &'static str = "running";
    /// It was stopped.
    pub const STOPPED: &'static str = "stopped";
    /// It stopped with background work of its own still running.
    pub const WAITING: &'static str = "waiting";
}

/// A background task's notice, a quiet row of its own linked to its task
/// ([`crate::MessagePart::Task`]): "Agent \"Look\" finished · 2m 5s", "Monitor event: STALL lane
/// WATCH idle 20 min", "Background command \"x\" failed with exit code 2".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRow {
    /// The task, by the agent's id.
    pub task: String,
    /// The task's tray row, `task:{id}`, the same as its [`crate::work::WorkRow::id`]: a click
    /// opens it there.
    pub row: String,
    /// What kind of task, as the tray says it ([`crate::work::WorkRow::kind`]).
    pub kind: String,
    /// What it says of the task: `completed`, `failed`, `killed`, `waiting`, `event`,
    /// `expired`, or the agent's own word.
    pub status: String,
    /// The row's words, its duration after a " · " when it says one.
    pub text: String,
    /// The task's description, muted on the right of the row ("lane stall watch").
    pub title: String,
    /// How it is drawn: [`crate::dock::NoteLine::INFO`], [`crate::dock::NoteLine::WARNING`] or
    /// [`crate::dock::NoteLine::ERROR`].
    pub tone: String,
    /// What opens under the row: a monitor's event, an agent's report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The tool part of the call that started the task, when the agent names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_part_id: Option<String>,
    /// Its figures in words: "12 tool uses · 41k tokens"; empty with none.
    pub figures: String,
    /// Tokens it took, as it ended or paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// Tool calls it made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    /// How long it ran, paused time left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// A command's exit code, when it ended with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The child chat its run is, where Slopty keeps one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// The resources its result linked, for an MCP task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkRow>,
}

/// A resource a result linked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRow {
    /// Its URI.
    pub uri: String,
    /// Its name, or title when it has one.
    pub label: String,
    /// Its MIME type, when said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A quiet row of the agent's own that is no failure ([`crate::MessagePart::Note`]): "Allowed
/// cargo test", "Saved 2 memories", "Switched to Sonnet due to high demand", "Goal achieved (5m ·
/// 3 turns)", "All background agents stopped", a hook's word, a request tried again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRow {
    /// What kind of notice, as the thread names it (`allowed`, `memory`, `fallback`, `goal`,
    /// `mode`, `stopped`, `turn`, `hook`, `retry`, `interrupted`), or `compaction` for a
    /// conversation summed up, `edits` for what is known of the files a command changed, `tasks`
    /// for completed task notices folded under one row, `done` for a turn's done line.
    pub kind: String,
    /// Its words.
    pub text: String,
    /// How it is drawn: [`crate::dock::NoteLine::INFO`], [`crate::dock::NoteLine::WARNING`] or
    /// [`crate::dock::NoteLine::ERROR`], or [`NoteRow::SUCCESS`].
    pub tone: String,
    /// More of it, under the row: a hook's output, a goal's last check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A request being tried again, while it is: the row counts down to its next attempt
    /// ([`RetryLine::words`]) and reads as recovered once the turn goes on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryLine>,
    /// A request tried again that went through in the end: the row reads "… recovered" and
    /// counts down no more.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovered: bool,
}

impl NoteRow {
    /// Something that went well (a goal met, a request that recovered).
    pub const SUCCESS: &'static str = "success";
}
