//! What the transcript's parts carry of Slopty's own: a call's facts in the TUI's
//! words, a subagent's progress on its spawn card, a background task's notice row and a quiet
//! notice row.
//!
//! The view model fills them from the thread (`slopty_ui::view_model::transcript`), so the shell
//! draws the words here as they are: each is the TUI's own line for the same thing
//! (`.research/tui-strings-2026-10-16.md`), said once for every surface.

use serde::{Deserialize, Serialize};

use crate::dock::RetryLine;

/// The lines of a tool's output an open step shows before "+ N lines": a longer one is fetched
/// whole when the person asks.
pub const OUTPUT_HEAD_LINES: usize = 24;

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
    /// The person turned it down (their own Deny): it ended, not failed, so it reads quiet,
    /// "Denied", never in the failure's colour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rejected: bool,
    /// How long it has run, while it runs: the TUI's "Running… (12s · timeout 2m)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer: Option<ToolTimer>,
    /// The tray row of the background task it started or became (`task:{id}`), which the chip
    /// links to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_row: Option<String>,
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
    /// When it ran, for its row's duration and its detail's Timing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<StepTime>,
    /// How a command exited, which its detail says on a line of its own: "Exit code 2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitCode>,
    /// Its output still grows: a command that runs, its output read from the file it writes. Its
    /// detail holds the output in a capped box that follows its own bottom, so the trace stays
    /// still while it grows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub growing: bool,
    /// The text the model read of its result, where the step shows something else in its place
    /// (the diff for an edit, a list for a task call): "Details for Claude", fetched when opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<BlobLink>,
    /// Where its agent's own records of it are, fetched as one JSON array when opened: its raw
    /// record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// How a command exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitCode {
    /// The code it exited with.
    pub code: i32,
    /// Its agent read a code other than 0 as no failure (grep's 1, "No matches found"), so it
    /// reads quiet rather than in the failure's colour.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quiet: bool,
}

/// A text fetched when the person opens it: where it is, and how long it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobLink {
    /// What the shell fetches it by.
    pub blob: String,
    /// Its lines.
    pub lines: u32,
}

/// When a step ran, as its records say: its start and end, and its run alone where the agent
/// says it apart from the wait for the person's permission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepTime {
    /// When it began, in ms since the epoch: its record's time.
    pub started: i64,
    /// When it ended, in ms since the epoch: its result's record's time; `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    /// How long its run alone took, in ms, by the agent's own measure, when it says (Claude
    /// Code's hooks): the wall time from `started` to `ended` holds it and the wait for the
    /// person's permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_ms: Option<u64>,
    /// When the turn it ran in began, in ms since the epoch: its row's tooltip says its start
    /// from there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_started: Option<i64>,
}

impl StepTime {
    /// Its wall time, from its start to its end, once it ended.
    #[must_use]
    pub fn wall_ms(&self) -> Option<u64> {
        let ended = self.ended?;
        u64::try_from(ended.saturating_sub(self.started)).ok()
    }

    /// How long it waited before it ran, once the agent said how long its run took: its wall
    /// time less its run.
    #[must_use]
    pub fn waited_ms(&self) -> Option<u64> {
        Some(self.wall_ms()?.saturating_sub(self.ran_ms?))
    }
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

/// A subagent's progress, on the row of the call that started it.
///
/// While it runs, the row reads its heartbeat ("{model} · {step} · {figures} · {elapsed}"); it
/// settles to its outcome and figures once the subagent ends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentProgress {
    /// Its kind (`general-purpose`, `Explore`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
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
    /// How it ended, once it did, in words: the first line of what it answered (else "Done"),
    /// "Failed: out of turns", "Stopped", "Waiting on its own background work".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// What it asks of the person, while it waits on them: answered in its own TUI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you: Option<String>,
    /// Its run's chat, where Slopty keeps one: what expands in place and drills in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    /// Its tray row (`task:{id}` or `chat:{chatId}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    /// The model it runs on, as the agent resolved it (`claude-haiku-5-5`), once it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// When its own records last moved (epoch ms), while it runs: a long quiet is said
    /// ("quiet 3m").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard_at: Option<i64>,
}

impl SubagentProgress {
    /// It finished.
    pub const DONE: &'static str = "done";
    /// It failed.
    pub const FAILED: &'static str = "failed";
    /// It waits on the person, who answers it in its own TUI.
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
    /// For a row of kind [`TurnFacts::KIND`], in the entry of the prompt that opened a turn: the
    /// turn's figures, which that prompt's row carries. It draws no row of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Box<TurnFacts>>,
}

impl NoteRow {
    /// Something that went well (a goal met, a request that recovered).
    pub const SUCCESS: &'static str = "success";
}

/// A turn's figures, on the row of the prompt that opened it (`docs/decisions/inspector.md`,
/// "The turn is the root row"): how it stands, when it ran, the models that answered, the tokens
/// it took by kind and the lines it changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnFacts {
    /// How it stands: [`TurnFacts::ACTIVE`], [`TurnFacts::COMPLETE`],
    /// [`TurnFacts::INTERRUPTED`] or [`TurnFacts::FAILED`].
    pub state: String,
    /// What went wrong, as the agent said it, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it began, in ms since the epoch.
    pub started: i64,
    /// When it ended, in ms since the epoch; `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    /// The models that answered in it, in the order they first did.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// The tokens it took, by kind as its agent counts them: `input`, `output`, `cache-read`,
    /// `cache-write`, `reasoning` and any other the agent names.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tokens: std::collections::BTreeMap<String, u64>,
    /// The lines added and removed in the working tree over it.
    #[serde(default)]
    pub changed: [u32; 2],
}

impl TurnFacts {
    /// The [`NoteRow::kind`] that carries a turn's figures.
    pub const KIND: &'static str = "turn-root";
    /// Under way.
    pub const ACTIVE: &'static str = "active";
    /// The agent answered.
    pub const COMPLETE: &'static str = "complete";
    /// The person stopped it.
    pub const INTERRUPTED: &'static str = "interrupted";
    /// It ended on an error.
    pub const FAILED: &'static str = "failed";

    /// How long it ran, until `now` while it runs.
    #[must_use]
    pub fn ran_ms(&self, now: i64) -> u64 {
        u64::try_from(self.ended.unwrap_or(now).saturating_sub(self.started)).unwrap_or(0)
    }

    /// The tokens it read fresh, from the cache, and wrote: what its row says, the split by
    /// every kind going in its tooltip.
    #[must_use]
    pub fn tokens_in_cached_out(&self) -> [u64; 3] {
        let of = |kind: &str| self.tokens.get(kind).copied().unwrap_or(0);
        [
            of("input").saturating_add(of("cache-write")),
            of("cache-read"),
            of("output"),
        ]
    }
}
