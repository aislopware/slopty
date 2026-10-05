//! Coding-agent tracking on the worker.
//!
//! Claude Code reports what it is doing through hooks: for every registered event it runs a
//! command with a JSON description on stdin. Slopty registers `slopty hook` for the events it
//! cares about; the CLI forwards the JSON, together with the `SLOPTY_SESSION` the terminal was
//! spawned with, to `slopty-worker`, which feeds it to an [`AgentTable`]. The table keeps one
//! [`Tracker`] per terminal session and turns the raw hook stream into
//! [`AgentStatus`] transitions that the daemon broadcasts as [`AgentEvent`]s.
//!
//! The hook payload is parsed leniently ([`Hook`]): unknown events and unknown fields are
//! ignored, so a newer Claude Code never breaks the relay.
//!
//! When the agent stops or waits on the human, the event's `detail` says what it wants: the
//! question it asked, the elicitation's message, or (on `Stop`) the last line it said, from the
//! payload's `last_assistant_message`; when a question or an elicitation comes without its
//! text but names a transcript, the daemon reads the transcript tail ([`transcript`]) and fills
//! the detail in.
//!
//! Hooks are only the strongest of four signals. A `claude` the human started by hand in any
//! Slopty terminal — or one running before `slopty hook install` — is attributed from what the
//! worker can see anyway: its [`detect`]ed foreground process, the [`title`] it paints, and the
//! JSONL transcript [`discover`]ed from its working directory. [`Tracker::observe`] merges
//! them in [`AgentSource`] order, so a weaker signal never overwrites what a stronger one
//! said and hooks stay authoritative once they speak.
//!
//! The agent is used through its own TUI in the terminal, which stays the source of truth. The
//! conversation face projects it: [`conversation`] decodes the transcript into typed entries,
//! [`statusline`] reads the status line's meters, and [`permission`] carries the person's answer
//! to a permission prompt when they give it from the face instead of the TUI's dialog. Where
//! Claude Code runs Slopty's mod ([`claude_mod`]), [`live`] adds what the model is writing
//! before the transcript has it. Nothing here drives the agent or answers for the person on its
//! own.
//!
//! Codex is reached through its app-server instead, as one more client of the person's daemon
//! beside its TUI ([`codex`]).

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod acp;
pub mod attach;
pub mod claude_mod;
pub mod codex;
pub mod commands;
pub mod conversation;
pub mod detect;
pub mod discover;
pub mod driven;
pub mod history;
pub mod hooks;
pub mod live;
pub mod loosening;
pub mod managed;
pub mod observed;
pub mod permission;
pub mod pi;
pub mod queue;
pub mod reports;
pub mod resume;
pub mod roster;
pub mod status;
pub mod statusline;
pub mod title;
pub mod transcript;
pub mod trust;
pub mod vouch;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};
use slopty_proto::agent::{AgentBranch, AgentKind, PullRequest, Worktree};
use slopty_proto::project::{AgentReport, NativeTask};

use crate::detect::Program;
use crate::status::{AgentEvent, AgentSource, AgentStatus, BlockReason, HeardMode};
use crate::title::TitleSignal;
use crate::transcript::Progress;

/// Hook events `slopty hook install` registers. The relay ignores everything else.
///
/// The status events come first; the rest feed the conversation face: subagents starting and
/// stopping (with their transcripts), the task list, compaction, and a turn that ended on an
/// API error. `MessageDisplay` is left out on purpose: Claude Code holds the TUI's paint until
/// that hook returns, so it is measured before it is ever registered.
pub const HOOK_EVENTS: [HookEvent; 19] = [
    HookEvent::SessionStart,
    HookEvent::SessionEnd,
    HookEvent::UserPromptSubmit,
    HookEvent::PreToolUse,
    HookEvent::PostToolUse,
    HookEvent::PostToolUseFailure,
    HookEvent::PermissionRequest,
    HookEvent::PermissionDenied,
    HookEvent::Notification,
    HookEvent::Elicitation,
    HookEvent::ElicitationResult,
    HookEvent::Stop,
    HookEvent::StopFailure,
    HookEvent::SubagentStart,
    HookEvent::SubagentStop,
    HookEvent::TaskCreated,
    HookEvent::TaskCompleted,
    HookEvent::PreCompact,
    HookEvent::PostCompact,
];

/// A hook's event (`hook_event_name`): Claude Code's own, and the two that `slopty hook` posts
/// on the same path for itself. Each is spelled on the wire as its variant is named.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// A session started, resumed, was cleared or compacted (`source`).
    SessionStart,
    /// The session ended.
    SessionEnd,
    /// The person sent a prompt.
    UserPromptSubmit,
    /// A tool call is about to run.
    PreToolUse,
    /// A tool call returned.
    PostToolUse,
    /// A tool call failed.
    PostToolUseFailure,
    /// A tool call waits on the person's permission; the one event Claude Code waits on.
    PermissionRequest,
    /// A tool call was refused.
    PermissionDenied,
    /// Claude Code notified the person (`notification_type`).
    Notification,
    /// An MCP server asked the person for input.
    Elicitation,
    /// The person answered an elicitation.
    ElicitationResult,
    /// A turn finished.
    Stop,
    /// A turn ended on an API error.
    StopFailure,
    /// A subagent started.
    SubagentStart,
    /// A subagent finished.
    SubagentStop,
    /// A task was added to the list.
    TaskCreated,
    /// A task was completed.
    TaskCompleted,
    /// A compaction is about to run.
    PreCompact,
    /// A compaction finished.
    PostCompact,
    /// `slopty hook report`: any program's own word on its status.
    Report,
    /// `slopty hook statusline`: the status line's meters, which say nothing about the turn.
    Statusline,
    /// An event this build does not know, such as one a newer Claude Code adds; ignored.
    #[default]
    #[serde(other)]
    Other,
}

impl HookEvent {
    /// The event's name on the wire and in the settings' `hooks` keys.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::Notification => "Notification",
            Self::Elicitation => "Elicitation",
            Self::ElicitationResult => "ElicitationResult",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::TaskCreated => "TaskCreated",
            Self::TaskCompleted => "TaskCompleted",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::Report => "Report",
            Self::Statusline => "Statusline",
            Self::Other => "Other",
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Longest `detail` string sent to clients.
pub const DETAIL_MAX: usize = 60;

/// Bytes of JSON a forwarded hook keeps of each of a tool's input and response.
///
/// The relay cuts the rest ([`Hook::trimmed`]). A `Read` or a `cat` can hand the hook megabytes,
/// and the face only shows a result's head (the transcript keeps the whole).
pub const HOOK_JSON_BUDGET: usize = 16 * 1024;

/// Characters a forwarded hook keeps of a free text: a final message, a compaction summary.
pub const HOOK_TEXT_MAX: usize = conversation::PROSE.chars;

/// A Claude Code hook payload, the fields Slopty reads. Serialized after [`Hook::trimmed`], it is
/// the payload cut down to them, which is what the relay forwards.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Hook {
    /// Claude Code's session id.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The event (`hook_event_name`).
    #[serde(default, rename = "hook_event_name")]
    pub event: HookEvent,
    /// The agent's working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `default`, `plan`, `acceptEdits`, `auto`, `dontAsk`, `bypassPermissions`.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// Tool events.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// The call's own id on tool events, what its result names. `PermissionRequest` carries
    /// none.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Tool arguments.
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    /// `PostToolUse`: what the tool returned (its `toolUseResult`), trimmed to
    /// [`HOOK_JSON_BUDGET`].
    #[serde(default)]
    pub tool_response: Option<serde_json::Value>,
    /// `PostToolUse`: how long the tool ran, permission prompts not counted.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// `PermissionRequest`: the permission updates Claude Code suggests ("always allow"),
    /// what a decision may hand back as `updatedPermissions`.
    #[serde(default)]
    pub permission_suggestions: Option<serde_json::Value>,
    /// `Notification`.
    #[serde(default)]
    pub notification_type: Option<String>,
    /// `Notification`.
    #[serde(default)]
    pub message: Option<String>,
    /// `UserPromptSubmit`.
    #[serde(default)]
    pub prompt: Option<String>,
    /// `SessionStart`: `startup|resume|clear|compact|fork`.
    #[serde(default)]
    pub source: Option<String>,
    /// `SessionEnd`: `clear|resume|logout|prompt_input_exit|other`.
    #[serde(default)]
    pub reason: Option<String>,
    /// The conversation transcript (JSONL); on every event in practice.
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// `Stop` and `SubagentStop`: the text of the final response, so nobody has to read the
    /// transcript. `StopFailure`: the API error as shown.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// `Stop`/`SubagentStop`: a `Stop` hook held this turn's end already, so it continues
    /// because of a hook.
    #[serde(default)]
    pub stop_hook_active: Option<bool>,
    /// `SubagentStart`/`SubagentStop`: the subagent, its thread's key
    /// ([`conversation::ThreadId::Agent`]).
    #[serde(default)]
    pub agent_id: Option<String>,
    /// `SubagentStart`/`SubagentStop`: `general-purpose`, `Explore`, a custom agent's name.
    #[serde(default)]
    pub agent_type: Option<String>,
    /// `SubagentStop`: the subagent's own transcript, which the worker tails as its thread.
    #[serde(default)]
    pub agent_transcript_path: Option<String>,
    /// `TaskCreated`/`TaskCompleted`.
    #[serde(default)]
    pub task_id: Option<String>,
    /// `TaskCreated`/`TaskCompleted`: the task's title.
    #[serde(default)]
    pub task_subject: Option<String>,
    /// `TaskCreated`/`TaskCompleted`.
    #[serde(default)]
    pub task_description: Option<String>,
    /// `PreCompact`/`PostCompact`: `manual` or `auto`.
    #[serde(default)]
    pub trigger: Option<String>,
    /// `PreCompact`: what the person passed to `/compact`.
    #[serde(default)]
    pub custom_instructions: Option<String>,
    /// `PostCompact`: the summary the conversation continues from.
    #[serde(default)]
    pub compact_summary: Option<String>,
    /// `StopFailure`: `rate_limit`, `overloaded`, `authentication_failed`, …
    #[serde(default)]
    pub error: Option<String>,
    /// `StopFailure`.
    #[serde(default)]
    pub error_details: Option<String>,
    /// `Report` (`slopty hook report`): `working|blocked|done|idle|gone`, from any program.
    #[serde(default)]
    pub status: Option<String>,
    /// `Statusline` (`slopty hook statusline`): the meters Claude Code's status line reads.
    #[serde(default)]
    pub meters: Option<statusline::Meters>,
    /// `Stop`/`SubagentStop`: the background commands, subagents and monitors still out, which
    /// wake the session when they finish. Absent when Claude Code could not reach its task
    /// registry.
    #[serde(default)]
    pub background_tasks: Option<Vec<BackgroundTask>>,
    /// `Stop`/`SubagentStop`: the prompts scheduled on the session (`/loop`, `CronCreate`).
    #[serde(default)]
    pub session_crons: Option<Vec<SessionCron>>,
    /// `Statusline`: the open pull request the status line names.
    #[serde(default)]
    pub pr: Option<PullRequest>,
    /// `Statusline`: the worktree the session runs in.
    #[serde(default)]
    pub worktree: Option<Worktree>,
}

/// Characters a forwarded hook keeps of a background task's description or a scheduled prompt.
const PENDING_TEXT_MAX: usize = 200;

/// The tool Claude Code asks the person a question with.
const ASK_USER_QUESTION: &str = "AskUserQuestion";

/// One entry of a `Stop` hook's `background_tasks`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct BackgroundTask {
    /// The task's id.
    #[serde(default)]
    pub id: String,
    /// `shell`, `subagent`, `monitor`, `workflow`, …
    #[serde(default, rename = "type")]
    pub kind: String,
    /// `running`, as far as anyone has seen.
    #[serde(default)]
    pub status: String,
    /// What it is doing, as the model described it.
    #[serde(default)]
    pub description: String,
}

/// One entry of a `Stop` hook's `session_crons`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionCron {
    /// The schedule's id.
    #[serde(default)]
    pub id: String,
    /// A cron expression.
    #[serde(default)]
    pub schedule: String,
    /// It runs again after firing.
    #[serde(default)]
    pub recurring: bool,
    /// The prompt it sends.
    #[serde(default)]
    pub prompt: String,
}

impl Hook {
    /// Parse a payload; never fails on unknown shapes, only on non-JSON.
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The hook as the relay forwards it: a tool's input and response cut to
    /// [`HOOK_JSON_BUDGET`] each (an edit's `originalFile` dropped outright), free texts to
    /// [`HOOK_TEXT_MAX`] characters.
    #[must_use]
    pub fn trimmed(mut self) -> Self {
        for value in [&mut self.tool_input, &mut self.tool_response].into_iter().flatten() {
            clip_json(value, &mut HOOK_JSON_BUDGET.clone());
        }
        for text in [
            &mut self.last_assistant_message,
            &mut self.compact_summary,
            &mut self.prompt,
            &mut self.message,
            &mut self.custom_instructions,
            &mut self.task_description,
            &mut self.error_details,
        ]
        .into_iter()
        .flatten()
        {
            clip_str(text, HOOK_TEXT_MAX);
        }
        let tasks = self.background_tasks.iter_mut().flatten().map(|t| &mut t.description);
        let crons = self.session_crons.iter_mut().flatten().map(|c| &mut c.prompt);
        for text in tasks.chain(crons) {
            clip_str(text, PENDING_TEXT_MAX);
        }
        self
    }

    /// The work still out when a `Stop` fired: background tasks and scheduled prompts, when
    /// there is any.
    fn pending_work(&self) -> Option<(u32, u32)> {
        let count = |n: Option<usize>| u32::try_from(n.unwrap_or(0)).unwrap_or(u32::MAX);
        let tasks = count(self.background_tasks.as_ref().map(Vec::len));
        let crons = count(self.session_crons.as_ref().map(Vec::len));
        (tasks > 0 || crons > 0).then_some((tasks, crons))
    }

    /// What a paused turn waits on, for the badge: the first task's description, else the
    /// first scheduled prompt.
    fn pending_detail(&self) -> Option<String> {
        let task = self.background_tasks.iter().flatten().map(|t| t.description.as_str());
        let cron = self.session_crons.iter().flatten().map(|c| c.prompt.as_str());
        task.chain(cron).map(first_line).find(|line| !line.is_empty()).map(truncate)
    }

    /// Whether this is a `SubagentStart` or `SubagentStop` of one of Claude Code's own agents
    /// (compaction, prompt suggestions, `/btw`) rather than one the model spawned. Claude Code
    /// gives those the session's own agent name, empty when it runs without one; the model's
    /// always have a type.
    #[must_use]
    pub fn is_internal_subagent(&self) -> bool {
        matches!(self.event, HookEvent::SubagentStart | HookEvent::SubagentStop)
            && self.agent_type.as_deref().is_some_and(str::is_empty)
    }

    /// What the server's project tree takes from this hook in `session`: one of the model's
    /// own subagents starting or stopping, or an item of Claude Code's task list made or
    /// completed. Claude Code's internal subagents (compaction, suggestions) are no node.
    #[must_use]
    pub fn report(&self, session: SessionId) -> Option<AgentReport> {
        if self.is_internal_subagent() {
            return None;
        }
        match self.event {
            HookEvent::SubagentStart => Some(AgentReport::SubagentStarted {
                session,
                agent: self.agent_id.clone()?,
                kind: self.agent_type.clone().unwrap_or_default(),
            }),
            HookEvent::SubagentStop => Some(AgentReport::SubagentStopped {
                session,
                agent: self.agent_id.clone()?,
                transcript: self.agent_transcript_path.clone(),
                last: first_words(self.last_assistant_message.as_deref()),
            }),
            HookEvent::TaskCreated | HookEvent::TaskCompleted => {
                let task = NativeTask {
                    id: self.task_id.clone()?,
                    subject: first_words(self.task_subject.as_deref()).unwrap_or_default(),
                    done: self.event == HookEvent::TaskCompleted,
                };
                Some(AgentReport::NativeTask { session, task })
            }
            _ => None,
        }
    }

    /// The badge's line for a prompt: its first line, or for the turn a finished background
    /// task starts (its prompt a `<task-notification>`), the notification's summary.
    fn prompt_detail(&self) -> Option<String> {
        let prompt = self.prompt.as_deref()?;
        let summary = prompt
            .trim_start()
            .starts_with("<task-notification>")
            .then(|| prompt.split_once("<summary>")?.1.split_once("</summary>").map(|(s, _)| s))
            .flatten();
        Some(truncate(first_line(summary.unwrap_or(prompt))))
    }

    /// One line describing the tool call ("Bash: cargo test", "Edit src/main.rs").
    fn tool_detail(&self) -> Option<String> {
        let tool = self.tool_name.as_deref()?;
        let input = self.tool_input.as_ref();
        let arg = |key: &str| input.and_then(|v| v.get(key)).and_then(|v| v.as_str());
        let text = match tool {
            "Bash" => arg("command").map(|c| format!("$ {}", first_line(c))),
            "Edit" | "Write" | "Read" | "NotebookEdit" => {
                arg("file_path").map(|p| format!("{tool} {}", short_path(p)))
            }
            "Glob" | "Grep" => arg("pattern").map(|p| format!("{tool} {p}")),
            "Agent" | "Task" => arg("description").map(|d| format!("Agent: {d}")),
            "WebFetch" => arg("url").map(|u| format!("Fetch {u}")),
            "WebSearch" => arg("query").map(|q| format!("Search {q}")),
            "Skill" => arg("skill").map(|s| format!("/{s}")),
            _ => None,
        };
        Some(truncate(&text.unwrap_or_else(|| tool.to_owned())))
    }

    /// Whether the call is `AskUserQuestion`: a question to the person, whichever hook (its
    /// `PreToolUse`, or the `PermissionRequest` it is answered through) brings it.
    fn asks(&self) -> bool {
        self.tool_name.as_deref() == Some(ASK_USER_QUESTION)
    }

    /// The first question of an `AskUserQuestion` call.
    fn question(&self) -> Option<String> {
        let question = self
            .tool_input
            .as_ref()?
            .get("questions")?
            .as_array()?
            .first()?
            .get("question")?
            .as_str()?;
        Some(truncate(question))
    }

    /// The last line the assistant said, from a `Stop` payload.
    fn last_said(&self) -> Option<String> {
        transcript::last_line(self.last_assistant_message.as_deref()?).map(|l| truncate(&l))
    }
}

/// Whether an event's `detail` should be recovered from the transcript when the payload gave
/// none: a question or an elicitation raised by a notification that does not spell it out.
///
/// A `Stop` always carries `last_assistant_message`, so a finished turn never needs the read.
#[must_use]
pub const fn wants_transcript(event: &AgentEvent) -> bool {
    event.detail.is_none()
        && matches!(
            event.status,
            AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation)
        )
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// A text's first line, cut for a badge; `None` for no text or a blank first line.
fn first_words(text: Option<&str>) -> Option<String> {
    text.map(first_line).filter(|line| !line.is_empty()).map(truncate)
}

/// Last two path components.
fn short_path(p: &str) -> String {
    let mut parts: Vec<&str> = p.rsplit('/').take(2).collect();
    parts.reverse();
    parts.join("/")
}

/// Clip to [`DETAIL_MAX`] characters with an ellipsis.
#[must_use]
pub fn truncate(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= DETAIL_MAX {
        return s.to_owned();
    }
    let cut: String = s.chars().take(DETAIL_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Cut a string to `max` characters, marking the cut with an ellipsis.
fn clip_str(text: &mut String, max: usize) {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
        text.push('…');
    }
}

/// Cut a JSON value to about `budget` bytes of text: strings are cut, arrays shortened, and what
/// is left once the budget is spent becomes `null`. An edit's `originalFile` (the whole file
/// before it) goes first; its diff says the same in a fraction of the bytes.
fn clip_json(value: &mut serde_json::Value, budget: &mut usize) {
    use serde_json::Value;
    match value {
        Value::String(text) => {
            clip_str(text, conversation::OUTPUT.chars.min(*budget));
            *budget = budget.saturating_sub(text.len());
        }
        Value::Array(items) => {
            let mut kept = 0_usize;
            for item in items.iter_mut() {
                if *budget == 0 {
                    break;
                }
                clip_json(item, budget);
                kept = kept.saturating_add(1);
            }
            items.truncate(kept);
        }
        Value::Object(map) => {
            map.remove("originalFile");
            for (key, item) in map.iter_mut() {
                *budget = budget.saturating_sub(key.len());
                if *budget == 0 {
                    *item = Value::Null;
                } else {
                    clip_json(item, budget);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => *budget = budget.saturating_sub(8),
    }
}

/// Tool name from "Claude needs your permission to use Bash"-style messages.
fn tool_from_message(message: Option<&str>) -> Option<String> {
    let (_before, after) = message?.split_once("to use ")?;
    let tool = after.split(|c: char| c.is_whitespace() || c == '.').next()?;
    (!tool.is_empty()).then(|| tool.to_owned())
}

/// What the worker can see of a session without any help from the agent: the program in the
/// foreground of its tty, the title that program paints, and the directory it runs in.
///
/// Everything here is a fact about the terminal, gathered by the worker on a timer; nothing in
/// it requires the agent to cooperate. A `program` of `None` means the platform would not say
/// (the lookup failed), which is not the same as "no agent runs here" and never clears one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// The foreground process of the session's tty.
    pub program: Option<Program>,
    /// The terminal title (OSC 0/2), when the session set one.
    pub title: Option<String>,
    /// Where the agent runs: the foreground process's own working directory, else OSC 7.
    pub cwd: Option<PathBuf>,
    /// The foreground process's pid, when the platform said.
    pub pid: Option<i32>,
    /// When that process started, when the platform said. Together with `pid` this is the
    /// identity of the process: one `claude` replaced by another inside a tick is not the same
    /// agent, whatever the session it runs in.
    pub started: Option<SystemTime>,
}

/// Where to look for a session's transcript, and which file is being read now.
///
/// Claude Code starts a new file whenever the human runs `/clear` or resumes another
/// conversation, so a session that already has a `current` file still has to be looked up
/// again: the newest file in the project directory is the one it writes now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovery {
    /// The terminal session.
    pub session: SessionId,
    /// The directory the agent runs in ([`discover::project_dir`]).
    pub cwd: PathBuf,
    /// When the agent process was first seen; older files are other conversations.
    pub since: SystemTime,
    /// The transcript being read now, when one has been found already.
    pub current: Option<String>,
}

/// Consecutive probes without the agent in the foreground before a *hooked* session is ended
/// by the process table alone. The hook relay (`slopty hook`) is itself briefly the foreground
/// process of the terminal it reports on, so one probe never means the agent is gone.
const ABSENT_BEFORE_GONE: u8 = 4;

/// Probes in a row whose title says idle, after a prompt the transcript never took, before the
/// prompt is taken as gone back to Claude Code's input ([`Tracker::observe`]).
const TAKEN_BACK_PROBES: u8 = 4;

/// Per-session agent state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracker {
    status: AgentStatus,
    agent_session: Option<String>,
    detail: Option<String>,
    /// The conversation file, from the last hook that named one or from [`discover`].
    transcript_path: Option<String>,
    /// Which signal the current status came from.
    source: AgentSource,
    /// A hook has spoken for this session: weaker signals stop deciding the status, and the
    /// foreground process no longer clears the agent (the hooks say when it ends).
    hooked: bool,
    /// When an agent process was first seen here; the transcript is the newest file written
    /// at or after this.
    first_seen: Option<SystemTime>,
    /// The agent's working directory, for [`discover::transcript_for`].
    cwd: Option<PathBuf>,
    /// Consecutive probes whose foreground process was not the agent.
    absent: u8,
    /// The agent process this tracker follows (`pid`, start time), when the platform said.
    process: Option<(i32, Option<SystemTime>)>,
    /// Calls waiting on the human (a permission, a question) by `tool_use_id`: the agent
    /// fires calls in batches, so a result from beside one of these does not release it.
    blocks: BTreeSet<String>,
    /// When the status entered its [`phase`], what [`AgentEvent::since_ms`] says.
    since_ms: WallMs,
    /// The command line of the agent process this tracker follows, when the platform said.
    argv: Vec<String>,
    /// The permission mode the agent last said it runs in: a hook's `permission_mode`, or
    /// the transcript's `permissionMode` beside a prompt.
    permission_mode: Option<String>,
    /// When [`Self::permission_mode`] was last heard to change, by the worker's clock.
    mode_ms: WallMs,
    /// What in `argv` loosens the agent's permissions ([`loosening::loosening`]), once judged.
    loosened: Option<Vec<String>>,
    /// After a prompt (`UserPromptSubmit`) the transcript has not taken yet: how many probes in a
    /// row the title has said idle since.
    unwritten: Option<u8>,
    /// When the status line's full usage windows reset, as it last said: what a turn a limit
    /// stopped waits for.
    limit_resets: Option<WallMs>,
    /// The person stopped the last turn (Esc) and has not prompted since.
    interrupted: bool,
}

/// `found` cut to what one [`AgentReport::Loosened`] carries.
fn bounded_loosening(mut found: Vec<String>) -> Vec<String> {
    use slopty_proto::project::{LOOSENED_ITEM_MAX, LOOSENED_MAX};
    found.truncate(LOOSENED_MAX);
    for item in &mut found {
        if item.len() > LOOSENED_ITEM_MAX {
            let cut = (0..=LOOSENED_ITEM_MAX).rev().find(|&i| item.is_char_boundary(i));
            item.truncate(cut.unwrap_or(0));
        }
    }
    found
}

/// The part of a status an elapsed time runs across: a tool call inside a turn is still the
/// turn, and a second question while blocked is still the wait.
const fn phase(status: &AgentStatus) -> u8 {
    match status {
        AgentStatus::None => 0,
        AgentStatus::Idle => 1,
        AgentStatus::Working | AgentStatus::Tool { .. } => 2,
        AgentStatus::Blocked(_) => 3,
        AgentStatus::Done | AgentStatus::Failed { .. } => 4,
        AgentStatus::Waiting { .. } => 5,
    }
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            status: AgentStatus::None,
            agent_session: None,
            detail: None,
            transcript_path: None,
            source: AgentSource::Process,
            hooked: false,
            first_seen: None,
            cwd: None,
            absent: 0,
            process: None,
            blocks: BTreeSet::new(),
            since_ms: WallMs::ZERO,
            argv: Vec::new(),
            permission_mode: None,
            mode_ms: WallMs::ZERO,
            loosened: None,
            unwritten: None,
            limit_resets: None,
            interrupted: false,
        }
    }
}

impl Tracker {
    /// Current status.
    #[must_use]
    pub const fn status(&self) -> &AgentStatus {
        &self.status
    }

    /// Which signal the current status came from.
    #[must_use]
    pub const fn source(&self) -> AgentSource {
        self.source
    }

    /// The event describing the current state (for a joining client).
    #[must_use]
    pub fn event(&self, session: SessionId) -> AgentEvent {
        AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status: self.status.clone(),
            agent_session: self.agent_session.clone(),
            detail: self.detail.clone(),
            attention: false,
            source: self.source,
            since_ms: self.since_ms,
            mode: self
                .permission_mode
                .clone()
                .map(|name| HeardMode { name, heard_ms: self.mode_ms }),
        }
    }

    /// Take `status`, stamping the time when it starts a new phase.
    fn enter(&mut self, status: AgentStatus) {
        if phase(&status) != phase(&self.status) {
            self.since_ms = if status == AgentStatus::None { WallMs::ZERO } else { WallMs::now() };
        }
        self.status = status;
    }

    /// Apply one hook; the resulting event when the visible state changed.
    ///
    /// A hook from another agent session is dropped while this one is busy: a nested
    /// `claude -p` run by the agent's own Bash call inherits the terminal's session and would
    /// otherwise post its whole hook set here. It takes over only when this agent is at rest
    /// (a restart after a crash) or when the human started it (`/clear`, `/resume`).
    pub fn apply(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        // The status line's meters ride the hook path but say nothing about the turn, only when
        // a limit it stops on resets.
        if hook.event == HookEvent::Statusline {
            if let Some(meters) = &hook.meters {
                self.limit_resets = statusline::full_resets(meters);
            }
            return None;
        }
        if !self.owns(hook) {
            return None;
        }
        self.hooked = true;
        self.unwritten = (hook.event == HookEvent::UserPromptSubmit).then_some(0);
        // The person speaks again: a prompt, or a conversation they started or took up.
        if matches!(hook.event, HookEvent::UserPromptSubmit | HookEvent::SessionStart) {
            self.interrupted = false;
        }
        if hook.session_id.is_some() {
            self.agent_session.clone_from(&hook.session_id);
        }
        if hook.transcript_path.is_some() {
            self.transcript_path.clone_from(&hook.transcript_path);
        }
        let mode_moved = self.hear_mode(hook.permission_mode.as_deref());
        if self.cwd.is_none() {
            self.cwd = hook.cwd.as_ref().map(PathBuf::from);
        }
        self.ledger(hook);
        // A change of mode alone (Shift-Tab in the TUI, heard with the next hook) is news too,
        // quietly.
        self.apply_status(session, hook).or_else(|| mode_moved.then(|| self.event(session)))
    }

    /// The permission mode the agent says it runs in now, when it says one: whether that is
    /// a change.
    fn hear_mode(&mut self, mode: Option<&str>) -> bool {
        let Some(mode) = mode.filter(|m| !m.is_empty()) else { return false };
        if self.permission_mode.as_deref() == Some(mode) {
            return false;
        }
        self.permission_mode = Some(mode.to_owned());
        self.mode_ms = WallMs::now();
        true
    }

    /// What `hook` does to the status, once the tracker took in what it says of the agent.
    fn apply_status(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        let (status, detail) = self.next(hook)?;
        if !self.blocks.is_empty() && !matches!(status, AgentStatus::Blocked(_)) {
            // A call beside the blocked one started or finished; the human is still needed.
            return None;
        }
        let was_blocked = matches!(self.status, AgentStatus::Blocked(_));
        let attention = match status {
            AgentStatus::Blocked(_) => !was_blocked,
            AgentStatus::Done | AgentStatus::Failed { .. } => true,
            _ => false,
        };
        if status == self.status && detail == self.detail && self.source == AgentSource::Hook {
            return None;
        }
        self.enter(status);
        self.detail = detail;
        self.source = AgentSource::Hook;
        if self.status == AgentStatus::None {
            self.agent_session = None;
        }
        Some(AgentEvent { attention, ..self.event(session) })
    }

    /// Fold in what the worker can see of the session (see [`Observation`]).
    ///
    /// A foreground process that is not an agent clears the session. When a hook has spoken,
    /// the hooks decide when it ends and this only steps in for an agent the worker actually saw
    /// running that has now been gone for `ABSENT_BEFORE_GONE` probes — a `claude` killed
    /// without a `SessionEnd` would otherwise keep its pill until the terminal exits.
    /// Otherwise the title decides working from idle, and the mere presence of the process
    /// means [`AgentStatus`] `Idle`; neither ever overwrites what the transcript or a hook said.
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        let Some(program) = obs.program.as_ref() else {
            // The platform would not say; that is not evidence of anything.
            return None;
        };
        if !program.is_claude() {
            if self.status == AgentStatus::None {
                return None;
            }
            if self.hooked {
                // Hooks arrive from a relay, not from the session's foreground process: only
                // an agent we watched run there, gone for several probes, ends this way.
                self.first_seen?;
                self.absent = self.absent.saturating_add(1);
                if self.absent < ABSENT_BEFORE_GONE {
                    return None;
                }
            }
            *self = Self::default();
            return Some(self.event(session));
        }
        // A `claude` replaced by another one between two probes is a different agent: its
        // conversation, its status and the hooks that spoke for the old one describe nothing
        // here any more, so the session is attributed again from scratch.
        if let Some(pid) = obs.pid {
            let process = (pid, obs.started);
            if self.process.is_some_and(|known| known != process) {
                *self = Self::default();
            }
            self.process = Some(process);
        }
        if !program.argv.is_empty() && program.argv != self.argv {
            self.argv.clone_from(&program.argv);
            self.loosened = None;
        }
        self.absent = 0;
        if self.first_seen.is_none() {
            self.first_seen = Some(obs.started.unwrap_or_else(SystemTime::now));
        }
        if obs.cwd.is_some() {
            self.cwd.clone_from(&obs.cwd);
        }
        if let Some(event) = self.taken_back(session, obs.title.as_deref()) {
            return Some(event);
        }
        if self.source >= AgentSource::Transcript {
            return None;
        }
        let (status, source) = match obs.title.as_deref().and_then(title::signal) {
            Some(TitleSignal::Working) => (AgentStatus::Working, AgentSource::Title),
            Some(TitleSignal::Idle) => (AgentStatus::Idle, AgentSource::Title),
            // No title to read: the process being there is all we know.
            None => (AgentStatus::Idle, AgentSource::Process),
        };
        self.set(session, status, None, source)
    }

    /// Fold in what the session's transcript says the turn is doing. Ignored once a hook has
    /// spoken — the hooks report blocking, which the transcript never does — except for an
    /// interrupt: Esc ends the turn with no `Stop` hook, and the transcript's
    /// "[Request interrupted by user]" record is the only signal of it, so it takes a busy
    /// hooked agent to `Idle` (quietly: the human did it) and clears what it was waiting on.
    /// The stop is kept ([`Self::interrupted`]) until the person prompts again.
    pub fn observe_progress(
        &mut self,
        session: SessionId,
        progress: &Progress,
    ) -> Option<AgentEvent> {
        self.unwritten = None;
        if self.hooked {
            let interrupted = progress.status == AgentStatus::Idle
                && matches!(
                    self.status,
                    AgentStatus::Working | AgentStatus::Tool { .. } | AgentStatus::Blocked(_)
                );
            if !interrupted {
                return None;
            }
            self.blocks.clear();
            self.interrupted = progress.is_interrupt();
        } else if progress.is_interrupt() {
            self.interrupted = true;
        } else if progress.status != AgentStatus::Idle {
            // With no hooks, a prompt or a call in the transcript is the turn going on.
            self.interrupted = false;
        }
        self.set(session, progress.status.clone(), progress.detail.clone(), AgentSource::Transcript)
    }

    /// Whether the person stopped the agent's last turn (Esc) and has not prompted it since:
    /// nothing in their name started it again.
    #[must_use]
    pub const fn interrupted(&self) -> bool {
        self.interrupted
    }

    /// A prompt Claude Code took back: Esc right after Enter puts it back in its input, writes
    /// nothing to the transcript and fires no `Stop`, which would leave the agent working for
    /// ever. Its own title says it is at rest again, so once the title has said idle for
    /// [`TAKEN_BACK_PROBES`] probes in a row with the transcript still silent since the prompt,
    /// the agent is idle, by its title. The next hook speaks for it again.
    fn taken_back(&mut self, session: SessionId, title: Option<&str>) -> Option<AgentEvent> {
        let quiet = self.unwritten.as_mut()?;
        if self.status != AgentStatus::Working || self.source != AgentSource::Hook {
            self.unwritten = None;
            return None;
        }
        if title.and_then(title::signal) != Some(TitleSignal::Idle) {
            *quiet = 0;
            return None;
        }
        *quiet = quiet.saturating_add(1);
        if *quiet < TAKEN_BACK_PROBES {
            return None;
        }
        self.unwritten = None;
        self.set(session, AgentStatus::Idle, None, AgentSource::Title)
    }

    /// Move to a state a signal weaker than a hook reported; `None` when nothing visible
    /// changed.
    fn set(
        &mut self,
        session: SessionId,
        status: AgentStatus,
        detail: Option<String>,
        source: AgentSource,
    ) -> Option<AgentEvent> {
        if status == self.status && detail == self.detail && source == self.source {
            return None;
        }
        // Without hooks a finished turn is only news when we watched it run: a transcript read
        // for the first time is usually a conversation that ended hours ago.
        let attention = status == AgentStatus::Done
            && matches!(self.status, AgentStatus::Working | AgentStatus::Tool { .. });
        self.enter(status);
        self.detail = detail;
        self.source = source;
        Some(AgentEvent { attention, ..self.event(session) })
    }

    /// Where to look for this agent's transcript, and the file being read now.
    ///
    /// An agent keeps asking after its file has been found: `/clear` and `/resume` start a new
    /// one, and the tail has to move with it. A hook names the file itself, so a hooked
    /// session only asks while nothing has named one.
    fn discovery(&self, session: SessionId) -> Option<Discovery> {
        if self.status == AgentStatus::None || (self.hooked && self.transcript_path.is_some()) {
            return None;
        }
        Some(Discovery {
            session,
            cwd: self.cwd.clone()?,
            since: self.first_seen?,
            current: self.transcript_path.clone(),
        })
    }

    /// The conversation to bring back after a reboot ([`resume`]): the one a hook named, else
    /// the one whose transcript was found, in the directory the agent runs in, with the flags
    /// its command line keeps and the permission mode it was last in.
    #[must_use]
    pub fn resumable(&self) -> resume::Resumable {
        let invocation = resume::invocation(detect::agent_args(&self.argv));
        if self.status == AgentStatus::None || invocation.print {
            return resume::Resumable::No;
        }
        let session = self.agent_session.clone().or_else(|| {
            let path = Path::new(self.transcript_path.as_deref()?);
            Some(path.file_stem()?.to_str()?.to_owned())
        });
        let (Some(session), Some(cwd)) = (session, self.cwd.as_ref()) else {
            return resume::Resumable::Unknown;
        };
        if !resume::is_session_id(&session) {
            return resume::Resumable::Unknown;
        }
        resume::Resumable::Yes(resume::Resume {
            session,
            cwd: cwd.to_string_lossy().into_owned(),
            transcript: self.transcript_path.clone(),
            args: resume::with_mode(invocation.args, self.permission_mode.as_deref()),
            relay: invocation.relay,
            mcp: invocation.mcp,
            locked: invocation.locked,
            role: invocation.role,
        })
    }

    /// Take what Claude Code itself lists for the agent this tracker follows
    /// ([`roster::Listed`]): its conversation, and when hooks will keep the status from here
    /// on (`hooked`: the relay is in the user's settings, or its own command line carries it),
    /// its status as a hook would have said it.
    ///
    /// Nothing once a hook has spoken since the worker started: that is newer.
    fn recover(
        &mut self,
        session: SessionId,
        listed: &roster::Listed,
        hooked: bool,
    ) -> Option<AgentEvent> {
        if self.hooked || self.status == AgentStatus::None {
            return None;
        }
        if listed.session_id.is_some() {
            self.agent_session.clone_from(&listed.session_id);
        }
        let hooked = hooked || resume::invocation(detect::agent_args(&self.argv)).relay;
        let status = listed.status().filter(|_| hooked)?;
        self.hooked = true;
        if status == self.status && self.source == AgentSource::Hook {
            return None;
        }
        self.enter(status);
        self.detail = None;
        self.source = AgentSource::Hook;
        Some(self.event(session))
    }

    /// Replace the detail (something recovered after the hook, such as a transcript line).
    /// Returns whether it changed.
    pub fn set_detail(&mut self, detail: &str) -> bool {
        let detail = Some(truncate(detail)).filter(|d| !d.is_empty());
        if detail == self.detail {
            return false;
        }
        self.detail = detail;
        true
    }

    /// Whether a hook speaks for the agent this tracker follows (see [`Self::apply`]).
    fn owns(&self, hook: &Hook) -> bool {
        let (Some(mine), Some(theirs)) = (&self.agent_session, &hook.session_id) else {
            return true;
        };
        if mine == theirs {
            return true;
        }
        let at_rest = self.status.at_rest();
        let by_the_human =
            hook.event == HookEvent::SessionStart && hook.source.as_deref() != Some("startup");
        at_rest || by_the_human
    }

    /// Keep the set of calls waiting on the human: a permission request or a question opens
    /// one, the call starting (permitted), ending, failing or being denied closes it, and a
    /// turn boundary clears them all (an interrupted turn fires no `Stop`).
    fn ledger(&mut self, hook: &Hook) {
        let Some(id) = hook.tool_use_id.as_ref() else {
            if matches!(
                hook.event,
                HookEvent::SessionStart
                    | HookEvent::SessionEnd
                    | HookEvent::UserPromptSubmit
                    | HookEvent::Stop
                    | HookEvent::StopFailure
                    | HookEvent::Report
            ) {
                self.blocks.clear();
            }
            return;
        };
        match hook.event {
            HookEvent::PermissionRequest => {
                self.blocks.insert(id.clone());
            }
            HookEvent::PreToolUse if hook.asks() => {
                self.blocks.insert(id.clone());
            }
            HookEvent::PreToolUse
            | HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied => {
                self.blocks.remove(id);
            }
            _ => {}
        }
    }

    /// The transition for a hook, if it means anything to us.
    fn next(&self, hook: &Hook) -> Option<(AgentStatus, Option<String>)> {
        let tool = || hook.tool_name.clone().unwrap_or_default();
        Some(match hook.event {
            HookEvent::SessionStart => (AgentStatus::Idle, None),
            HookEvent::SessionEnd => (AgentStatus::None, None),
            HookEvent::UserPromptSubmit => (AgentStatus::Working, hook.prompt_detail()),
            HookEvent::PreToolUse | HookEvent::PermissionRequest if hook.asks() => {
                (AgentStatus::Blocked(BlockReason::Question), hook.question())
            }
            HookEvent::PreToolUse => (AgentStatus::Tool { tool: tool() }, hook.tool_detail()),
            HookEvent::PostToolUse
            | HookEvent::PostToolUseFailure
            | HookEvent::PermissionDenied
            | HookEvent::ElicitationResult => (AgentStatus::Working, None),
            HookEvent::PermissionRequest => {
                (AgentStatus::Blocked(BlockReason::Permission { tool: tool() }), hook.tool_detail())
            }
            HookEvent::Elicitation => (
                AgentStatus::Blocked(BlockReason::Elicitation),
                hook.message.as_deref().map(first_line).map(truncate),
            ),
            HookEvent::Notification => match hook.notification_type.as_deref()? {
                // Follows a `PermissionRequest` a few seconds later and, as of Claude Code
                // 2.1.261, says only "Claude needs your permission": when the request already
                // put the tool and its arguments (or its question) on the badge, keep them.
                // A question is answered through the permission hook, so its prompt is the
                // question's, never a permission for `AskUserQuestion`.
                "permission_prompt" => {
                    let tool = tool_from_message(hook.message.as_deref());
                    let asks = tool.as_deref() == Some(ASK_USER_QUESTION);
                    match &self.status {
                        AgentStatus::Blocked(BlockReason::Question) if asks || tool.is_none() => {
                            return None;
                        }
                        AgentStatus::Blocked(BlockReason::Permission { .. }) if tool.is_none() => {
                            return None;
                        }
                        _ if asks => (AgentStatus::Blocked(BlockReason::Question), None),
                        _ => (
                            AgentStatus::Blocked(BlockReason::Permission {
                                tool: tool.unwrap_or_default(),
                            }),
                            None,
                        ),
                    }
                }
                // Claude Code says its prompt has sat idle for a minute. A turn paused on
                // background work sits at that prompt by design; it stays paused.
                "idle_prompt" if matches!(self.status, AgentStatus::Waiting { .. }) => {
                    return None;
                }
                "idle_prompt" => (AgentStatus::Blocked(BlockReason::IdlePrompt), None),
                "agent_needs_input" => (AgentStatus::Blocked(BlockReason::Question), None),
                "elicitation_dialog" | "elicitation_url_dialog" => {
                    (AgentStatus::Blocked(BlockReason::Elicitation), None)
                }
                "elicitation_complete" | "elicitation_response" => (AgentStatus::Working, None),
                _ => return None,
            },
            // A turn that ended with work still out is paused until that work wakes it (a new
            // turn, whose prompt is the task's notification): not done, and nothing announced.
            HookEvent::Stop => match hook.pending_work() {
                Some((tasks, crons)) => {
                    (AgentStatus::Waiting { tasks, crons }, hook.pending_detail())
                }
                None => (AgentStatus::Done, hook.last_said()),
            },
            // A turn that ended on an API error ends as surely as one that finished, but failed:
            // the detail is the error as shown, and a limit waits for the full window to reset.
            HookEvent::StopFailure => {
                let error = hook.error.clone().filter(|e| !e.is_empty());
                let error = error.unwrap_or_else(|| "unknown".to_owned());
                let until_ms = (error == AgentStatus::RATE_LIMIT).then_some(self.limit_resets);
                (AgentStatus::Failed { error, until_ms: until_ms.flatten() }, hook.last_said())
            }
            // Any program's own word (`slopty hook report`): a wrapper around another agent
            // gets the same pill, badge and attention as Claude Code's hooks buy it.
            HookEvent::Report => {
                let said = hook.message.as_deref().map(first_line).map(truncate);
                match hook.status.as_deref()? {
                    "working" => (AgentStatus::Working, said),
                    "blocked" => (AgentStatus::Blocked(BlockReason::Question), said),
                    "done" => (AgentStatus::Done, said),
                    "idle" => (AgentStatus::Idle, said),
                    "gone" => (AgentStatus::None, None),
                    _ => return None,
                }
            }
            // The conversation face reads these; the status does not move.
            HookEvent::SubagentStart
            | HookEvent::SubagentStop
            | HookEvent::TaskCreated
            | HookEvent::TaskCompleted
            | HookEvent::PreCompact
            | HookEvent::PostCompact
            | HookEvent::Statusline
            | HookEvent::Other => return None,
        })
    }
}

/// All sessions' agents.
#[derive(Debug, Default)]
pub struct AgentTable {
    sessions: HashMap<SessionId, Tracker>,
    /// Conversations whose `SessionEnd` did not come from the person ([`Self::resumable`]),
    /// with the probes since that saw something else in the foreground.
    parked: HashMap<SessionId, (resume::Resume, u8)>,
    /// The pull request and worktree each session's status line last named, when it named
    /// either.
    branches: HashMap<SessionId, AgentBranch>,
    /// Sessions whose agent ended and where none has started since ([`Self::ended`]).
    ended: HashSet<SessionId>,
    /// The permission mode last reported for each session ([`Self::permission_mode_report`]).
    reported_modes: HashMap<SessionId, String>,
    /// What loosens each session's agent, as last reported ([`Self::loosening_report`]).
    reported_loosened: HashMap<SessionId, Vec<String>>,
    /// What the worker adds to the agents it starts, which loosens nothing
    /// ([`Self::set_own`]).
    own: loosening::Own,
}

impl AgentTable {
    /// Feed a hook for a session.
    pub fn apply(&mut self, session: SessionId, hook: &Hook) -> Option<AgentEvent> {
        let tracker = self.sessions.entry(session).or_default();
        let before = tracker.resumable();
        let had_agent = tracker.status() != &AgentStatus::None;
        let event = tracker.apply(session, hook);
        let has_agent = tracker.status() != &AgentStatus::None;
        let resumable = has_agent && matches!(tracker.resumable(), resume::Resumable::Yes(_));
        self.note_end(session, had_agent, has_agent);
        if !has_agent {
            self.sessions.remove(&session);
            self.branches.remove(&session);
            if hook.event == HookEvent::SessionEnd
                && !resume::ended_by_the_person(hook.reason.as_deref())
                && let resume::Resumable::Yes(conversation) = before
            {
                self.parked.insert(session, (conversation, 0));
            }
        } else if resumable {
            self.parked.remove(&session);
        }
        event
    }

    /// The conversation to bring back in `session` after a reboot.
    ///
    /// A conversation ended by a signal (`SessionEnd` with `other`, what a reboot gives) or
    /// followed by another (`clear`, `resume`) is kept until the next one is known, or until
    /// `ABSENT_BEFORE_GONE` probes in a row find no agent in the foreground: the person is
    /// back at the shell. One the person ended, or whose process went away, is not.
    #[must_use]
    pub fn resumable(&self, session: SessionId) -> resume::Resumable {
        let tracked = self.sessions.get(&session).map(Tracker::resumable);
        match (tracked, self.parked.get(&session)) {
            (Some(resume::Resumable::Unknown) | None, Some((conversation, _probes))) => {
                resume::Resumable::Yes(conversation.clone())
            }
            (Some(tracked), _) => tracked,
            (None, None) => resume::Resumable::No,
        }
    }

    /// Feed one round of what the worker can see of a session ([`Tracker::observe`]).
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        if let (Some(program), Some((_conversation, probes))) =
            (obs.program.as_ref(), self.parked.get_mut(&session))
        {
            if program.is_claude() {
                *probes = 0;
            } else {
                *probes = probes.saturating_add(1);
                if *probes >= ABSENT_BEFORE_GONE {
                    self.parked.remove(&session);
                }
            }
        }
        // A session with nothing agent-like in it must not grow an entry on every tick.
        if !self.sessions.contains_key(&session)
            && !obs.program.as_ref().is_some_and(Program::is_claude)
        {
            return None;
        }
        let tracker = self.sessions.entry(session).or_default();
        let had_agent = tracker.status() != &AgentStatus::None;
        let event = tracker.observe(session, obs);
        let has_agent = tracker.status() != &AgentStatus::None;
        self.note_end(session, had_agent, has_agent);
        if !has_agent {
            self.sessions.remove(&session);
            self.branches.remove(&session);
        }
        event
    }

    /// Keep [`Self::ended`] as a signal moved `session` from having an agent or not to having
    /// one or not.
    fn note_end(&mut self, session: SessionId, had_agent: bool, has_agent: bool) {
        if has_agent {
            self.ended.remove(&session);
        } else if had_agent {
            self.ended.insert(session);
        }
    }

    /// Whether the agent that ran in `session` has ended and none has started there since:
    /// the terminal's shell, if it has one, is what reads its input now.
    #[must_use]
    pub fn ended(&self, session: SessionId) -> bool {
        self.ended.contains(&session)
    }

    /// Whether the person stopped the last turn of the agent in `session` and has not prompted
    /// it since ([`Tracker::interrupted`]).
    #[must_use]
    pub fn interrupted(&self, session: SessionId) -> bool {
        self.sessions.get(&session).is_some_and(Tracker::interrupted)
    }

    /// The permission mode `session`'s agent is in, as the server's tree takes it, when it is
    /// not the one last reported for the session. Call it after [`Self::apply`]: only a hook
    /// the session's own agent sent moves the mode (a nested `claude -p` does not).
    pub fn permission_mode_report(&mut self, session: SessionId) -> Option<AgentReport> {
        let mode = self.sessions.get(&session)?.permission_mode.as_ref()?;
        if self.reported_modes.get(&session) == Some(mode) {
            return None;
        }
        self.reported_modes.insert(session, mode.clone());
        Some(AgentReport::PermissionMode { session, mode: mode.clone() })
    }

    /// Name what the worker adds to the agents it starts (its `slopty` for the hooks, the status
    /// line and the tools, its mod's plugin directory), so [`Self::loosening_report`] tells them
    /// from the same flags an agent passed itself.
    pub fn set_own(&mut self, own: loosening::Own) {
        self.own = own;
    }

    /// What loosens the permissions of the agent the worker sees in `session`'s foreground,
    /// when it is not what was last reported for the session. Call it after [`Self::observe`].
    /// A session whose agent is gone reports nothing more; one whose next agent loosens nothing
    /// reports that once.
    pub fn loosening_report(&mut self, session: SessionId) -> Option<AgentReport> {
        let own = &self.own;
        let tracker = self.sessions.get_mut(&session)?;
        if tracker.loosened.is_none() {
            // Judged once per command line: a `--settings` file is read only then.
            let cwd = tracker.cwd.as_deref().unwrap_or_else(|| Path::new("/"));
            let found = loosening::loosening(&tracker.argv, cwd, own);
            tracker.loosened = Some(bounded_loosening(found));
        }
        let found = tracker.loosened.clone().unwrap_or_default();
        let before = self.reported_loosened.get(&session);
        if before.map_or(found.is_empty(), |before| *before == found) {
            return None;
        }
        self.reported_loosened.insert(session, found.clone());
        Some(AgentReport::Loosened { session, found })
    }

    /// Take the pull request and worktree a status line named in `session`; what every client
    /// is told when either changed. Anything but a `Statusline` hook is passed over, and so is
    /// a session with no agent: the branch lives and goes with its agent, so apply the hook
    /// first.
    pub fn branch(&mut self, session: SessionId, hook: &Hook) -> Option<AgentBranch> {
        if hook.event != HookEvent::Statusline || !self.sessions.contains_key(&session) {
            return None;
        }
        let now = AgentBranch { session, pr: hook.pr.clone(), worktree: hook.worktree.clone() };
        let empty = now.pr.is_none() && now.worktree.is_none();
        if self.branches.get(&session).map_or(empty, |before| *before == now) {
            return None;
        }
        if empty {
            self.branches.remove(&session);
        } else {
            self.branches.insert(session, now.clone());
        }
        Some(now)
    }

    /// Every session's pull request and worktree, for a joining client.
    #[must_use]
    pub fn branches(&self) -> Vec<AgentBranch> {
        self.branches.values().cloned().collect()
    }

    /// Fold in Claude Code's own registry of its live sessions ([`roster::registered`]), read
    /// once after the worker started: each agent whose process it lists gets its conversation
    /// back, and when `hooked` (the relay is registered), the status the hooks it sent before
    /// the restart had said. The events to broadcast, all quiet.
    ///
    /// An agent's process is the one the registry names, or its parent: a managed launcher
    /// runs Claude Code as its child, so the terminal's foreground process is the launcher's.
    /// `children` lists a process's direct children.
    pub fn recover(
        &mut self,
        listed: &[roster::Listed],
        hooked: bool,
        children: impl Fn(i32) -> Vec<i32>,
    ) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        for (session, tracker) in &mut self.sessions {
            let Some((pid, _started)) = tracker.process else { continue };
            let named = |pid: i32| listed.iter().find(|l| l.pid == Some(pid));
            let Some(entry) = named(pid).or_else(|| children(pid).into_iter().find_map(named))
            else {
                continue;
            };
            events.extend(tracker.recover(*session, entry, hooked));
        }
        events
    }

    /// Feed what a session's transcript says the turn is doing
    /// ([`Tracker::observe_progress`]).
    pub fn observe_progress(
        &mut self,
        session: SessionId,
        progress: &Progress,
    ) -> Option<AgentEvent> {
        self.sessions.get_mut(&session)?.observe_progress(session, progress)
    }

    /// The permission mode `session`'s transcript noted beside its latest prompt, for an agent
    /// no hook speaks for: an event when that is a change.
    pub fn hear_mode(&mut self, session: SessionId, mode: &str) -> Option<AgentEvent> {
        let tracker = self.sessions.get_mut(&session)?;
        if tracker.hooked || !tracker.hear_mode(Some(mode)) {
            return None;
        }
        Some(tracker.event(session))
    }

    /// Every unhooked agent's transcript lookup: where to look, and what is being read now
    /// ([`Discovery`], [`discover::transcript_for`]).
    #[must_use]
    pub fn discoveries(&self) -> Vec<Discovery> {
        self.sessions.iter().filter_map(|(id, t)| t.discovery(*id)).collect()
    }

    /// Drop every agent whose session the worker no longer runs, and say so: a session that
    /// went away takes its agent with it whatever its last signal said.
    pub fn retain(&mut self, live: &[SessionId]) -> Vec<AgentEvent> {
        let mut gone = Vec::new();
        self.parked.retain(|session, _conversation| live.contains(session));
        self.branches.retain(|session, _branch| live.contains(session));
        self.ended.retain(|session| live.contains(session));
        self.reported_modes.retain(|session, _mode| live.contains(session));
        self.reported_loosened.retain(|session, _found| live.contains(session));
        self.sessions.retain(|session, _tracker| {
            if live.contains(session) {
                return true;
            }
            gone.push(Tracker::default().event(*session));
            false
        });
        gone
    }

    /// Whether an event still describes its session.
    ///
    /// The daemon computes events from a poll and broadcasts them; a hook arriving on the
    /// control socket in between has already told every client something newer, and the poll's
    /// event must not put the older state back (a permission badge would vanish until the next
    /// hook). Anything the table has moved past is dropped instead of sent.
    #[must_use]
    pub fn is_current(&self, event: &AgentEvent) -> bool {
        self.sessions.get(&event.session).map_or(event.status == AgentStatus::None, |tracker| {
            tracker.status == event.status && tracker.source == event.source
        })
    }

    /// Every session with an agent in it.
    #[must_use]
    pub fn sessions_with_agents(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// Record the transcript a session's agent writes, found without a hook. Returns whether
    /// the file moved: the caller reads a new conversation from its start.
    pub fn set_transcript_path(&mut self, session: SessionId, path: &Path) -> bool {
        let Some(tracker) = self.sessions.get_mut(&session) else { return false };
        let path = Some(path.to_string_lossy().into_owned());
        if tracker.transcript_path == path {
            return false;
        }
        tracker.transcript_path = path;
        true
    }

    /// Put a recovered detail on a session's agent (see [`wants_transcript`]); the next
    /// snapshot carries it. Returns whether anything changed.
    pub fn set_detail(&mut self, session: SessionId, detail: &str) -> bool {
        self.sessions.get_mut(&session).is_some_and(|t| t.set_detail(detail))
    }

    /// The session's terminal went away.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
        self.parked.remove(&session);
        self.branches.remove(&session);
        self.ended.remove(&session);
        self.reported_modes.remove(&session);
        self.reported_loosened.remove(&session);
    }

    /// The agent's process in `session`, once the worker has seen it in the foreground.
    #[must_use]
    pub fn pid(&self, session: SessionId) -> Option<i32> {
        self.sessions.get(&session)?.process.map(|(pid, _started)| pid)
    }

    /// Where the session's conversation is written, once a hook has said.
    #[must_use]
    pub fn transcript_path(&self, session: SessionId) -> Option<PathBuf> {
        self.sessions.get(&session)?.transcript_path.as_deref().map(PathBuf::from)
    }

    /// Current state of every session with an agent, for a joining client.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AgentEvent> {
        self.sessions.iter().map(|(id, t)| t.event(*id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(json: &str) -> Hook {
        Hook::parse(json).expect("hook json")
    }

    /// A turn that ended on an API error fails, with attention, by the error's kind; a limit
    /// waits for the full window the status line last showed to reset, and the agent is at rest
    /// for whatever comes next.
    #[test]
    fn a_stop_failure_fails_the_turn_until_a_limit_resets() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let statusline = hook(
            r#"{"hook_event_name":"Statusline","meters":{"model":null,"model_id":null,
            "context_used_pct":null,"context_window":null,
            "five_hour":{"used_pct":100.0,"resets_at":1790018000},
            "seven_day":{"used_pct":40.0,"resets_at":1790500000}}}"#,
        );
        assert_eq!(t.apply(sid, &statusline), None, "the meters say nothing of the turn");
        let prompt = hook(r#"{"hook_event_name":"UserPromptSubmit","session_id":"s1"}"#);
        t.apply(sid, &prompt);
        let failure = |error: &str| {
            hook(&format!(
                r#"{{"hook_event_name":"StopFailure","session_id":"s1","error":"{error}",
                "last_assistant_message":"You've hit your limit"}}"#
            ))
        };
        let e = t.apply(sid, &failure("rate_limit")).expect("failed");
        let until_ms = Some(WallMs::from_millis(1_790_018_000_000));
        assert_eq!(e.status, AgentStatus::Failed { error: "rate_limit".to_owned(), until_ms });
        assert!(e.attention);
        assert!(e.status.at_rest());
        t.apply(sid, &prompt);
        let e = t.apply(sid, &failure("overloaded")).expect("failed");
        assert_eq!(
            e.status,
            AgentStatus::Failed { error: "overloaded".to_owned(), until_ms: None }
        );
    }

    /// `slopty hook report` speaks for any program: the five words walk the pill, a block
    /// raises attention once, and "gone" ends the agent.
    #[test]
    fn a_report_from_any_program_drives_the_pill() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let report = |status: &str, msg: &str| {
            hook(&format!(
                r#"{{"hook_event_name":"Report","status":"{status}","message":"{msg}"}}"#
            ))
        };
        let e = t.apply(sid, &report("working", "planning")).expect("working");
        assert_eq!((e.status, e.detail.as_deref()), (AgentStatus::Working, Some("planning")));
        let e = t.apply(sid, &report("blocked", "approve rm -rf?")).expect("blocked");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert!(e.attention);
        assert_eq!(t.apply(sid, &report("blocked", "approve rm -rf?")), None, "same again");
        let e = t.apply(sid, &report("done", "all green")).expect("done");
        assert_eq!((e.status, e.detail.as_deref()), (AgentStatus::Done, Some("all green")));
        let e = t.apply(sid, &report("idle", "")).expect("idle");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(t.apply(sid, &report("dancing", "")), None, "not a status");
        let e = t.apply(sid, &report("gone", "")).expect("gone");
        assert_eq!(e.status, AgentStatus::None);
    }

    #[test]
    fn a_turn_walks_idle_working_tool_done() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"session_id":"abc","hook_event_name":"SessionStart","source":"startup"}"#,
                ),
            )
            .expect("start");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.agent_session.as_deref(), Some("abc"));
        assert!(!e.attention);

        let e = t
            .apply(
                sid,
                &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"fix the build\nplease"}"#),
            )
            .expect("prompt");
        assert_eq!(e.status, AgentStatus::Working);
        assert_eq!(e.detail.as_deref(), Some("fix the build"));

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"cargo test\n"},"tool_use_id":"t1"}"#,
                ),
            )
            .expect("tool");
        assert_eq!(e.status, AgentStatus::Tool { tool: "Bash".into() });
        assert_eq!(e.detail.as_deref(), Some("$ cargo test"));

        let e = t
            .apply(
                sid,
                &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"t1"}"#),
            )
            .expect("post");
        assert_eq!(e.status, AgentStatus::Working);
        assert_eq!(e.detail, None);

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"Stop","stop_hook_active":false}"#))
            .expect("stop");
        assert_eq!(e.status, AgentStatus::Done);
        assert!(e.attention);

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"SessionEnd","end_reason":"other"}"#))
            .expect("end");
        assert_eq!(e.status, AgentStatus::None);
        assert_eq!(e.agent_session, None);
    }

    /// A `claude -p` the agent runs from its own Bash call inherits the terminal's session
    /// and posts its own hooks: they are dropped while the agent is busy, so its start does
    /// not clear the tool and its stop does not mint a "finished". A new session id is taken
    /// once the agent is at rest (a restart), or at once when the human started it.
    /// The stamp moves when the status starts a new phase and holds across a tool call inside a
    /// turn, and the event a joining client is sent carries it: the elapsed time survives a
    /// reconnect. Stamps are backdated by hand so a change is visible without waiting.
    #[test]
    fn the_status_is_stamped_when_its_phase_changes() {
        const EARLIER: WallMs = WallMs::from_millis(1_000);
        let sid = SessionId::new();
        let mut t = Tracker::default();
        assert_eq!(t.event(sid).since_ms, WallMs::ZERO, "no agent, no stamp");
        let before = WallMs::now();
        let prompt = hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"go"}"#);
        let e = t.apply(sid, &prompt).expect("working");
        assert!(e.since_ms >= before, "stamped from the clock: {:?}", e.since_ms);
        t.since_ms = EARLIER;
        let tool = hook(
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        );
        let e = t.apply(sid, &tool).expect("tool");
        assert_eq!(e.since_ms, EARLIER, "a tool call is still the turn");
        let post = hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#);
        assert_eq!(t.apply(sid, &post).expect("working").since_ms, EARLIER);
        let e = t.apply(sid, &hook(r#"{"hook_event_name":"Stop"}"#)).expect("done");
        assert!(e.since_ms >= before, "done is a new phase");
        assert_eq!(t.event(sid).since_ms, e.since_ms, "a joining client reads the same stamp");
        let agent = status::SessionAgent::from(&t.event(sid));
        assert_eq!(agent.since_ms, e.since_ms, "and so does orchestration");
        let e = t.apply(sid, &hook(r#"{"hook_event_name":"SessionEnd"}"#)).expect("gone");
        assert_eq!(e.since_ms, WallMs::ZERO, "an ended agent has no stamp");
    }

    #[test]
    fn a_nested_run_does_not_take_over_a_busy_agent() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.apply(
            sid,
            &hook(r#"{"session_id":"abc","hook_event_name":"SessionStart","source":"startup"}"#),
        );
        t.apply(
            sid,
            &hook(r#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","prompt":"go"}"#),
        );
        t.apply(sid, &hook(r#"{"session_id":"abc","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"claude -p hi"},"tool_use_id":"t1"}"#));
        for nested in [
            r#"{"session_id":"def","hook_event_name":"SessionStart","source":"startup"}"#,
            r#"{"session_id":"def","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#,
            r#"{"session_id":"def","hook_event_name":"Stop","last_assistant_message":"hello"}"#,
            r#"{"session_id":"def","hook_event_name":"SessionEnd","end_reason":"other"}"#,
        ] {
            assert_eq!(t.apply(sid, &hook(nested)), None, "{nested}");
        }
        assert_eq!(t.status, AgentStatus::Tool { tool: "Bash".into() });
        assert_eq!(t.agent_session.as_deref(), Some("abc"));
        let e = t
            .apply(sid, &hook(r#"{"session_id":"abc","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"t1"}"#))
            .expect("the agent's own result");
        assert_eq!(e.status, AgentStatus::Working);
        t.apply(sid, &hook(r#"{"session_id":"abc","hook_event_name":"Stop"}"#));

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"session_id":"ghi","hook_event_name":"SessionStart","source":"startup"}"#,
                ),
            )
            .expect("a restart while done");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.agent_session.as_deref(), Some("ghi"));

        t.apply(
            sid,
            &hook(r#"{"session_id":"ghi","hook_event_name":"UserPromptSubmit","prompt":"go"}"#),
        );
        let e = t
            .apply(
                sid,
                &hook(r#"{"session_id":"jkl","hook_event_name":"SessionStart","source":"resume"}"#),
            )
            .expect("the human resumed another conversation");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.agent_session.as_deref(), Some("jkl"));
    }

    /// Calls come in batches: a permission or a question waits on the human while a call
    /// beside it runs, and that call's result does not release the block. The block ends
    /// when its own call is permitted (it starts), denied or answered.
    #[test]
    fn a_block_stands_while_a_call_beside_it_finishes() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"go"}"#));
        t.apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/a/b.rs"},"tool_use_id":"r1"}"#));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm x"},"tool_use_id":"b1"}"#))
            .expect("blocked");
        assert!(matches!(e.status, AgentStatus::Blocked(BlockReason::Permission { .. })));
        assert!(e.attention);
        assert_eq!(
            t.apply(
                sid,
                &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Read","tool_use_id":"r1"}"#)
            ),
            None,
            "the read finishing does not release the permission"
        );
        assert!(matches!(t.status, AgentStatus::Blocked(BlockReason::Permission { .. })));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"rm x"},"tool_use_id":"b1"}"#))
            .expect("permitted: the call starts");
        assert_eq!(e.status, AgentStatus::Tool { tool: "Bash".into() });

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/a/c.rs"},"tool_use_id":"e1"}"#))
            .expect("blocked again");
        assert!(matches!(e.status, AgentStatus::Blocked(BlockReason::Permission { .. })));
        assert_eq!(
            t.apply(
                sid,
                &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"b1"}"#)
            ),
            None
        );
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PermissionDenied","tool_name":"Edit","tool_use_id":"e1"}"#))
            .expect("denied: released");
        assert_eq!(e.status, AgentStatus::Working);

        t.apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"Grep","tool_input":{"pattern":"x"},"tool_use_id":"g1"}"#));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which?"}]},"tool_use_id":"q1"}"#))
            .expect("a question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert_eq!(
            t.apply(
                sid,
                &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Grep","tool_use_id":"g1"}"#)
            ),
            None
        );
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"AskUserQuestion","tool_use_id":"q1"}"#))
            .expect("answered");
        assert_eq!(e.status, AgentStatus::Working);

        // An interrupted turn fires no Stop; the next prompt clears what was left.
        t.apply(sid, &hook(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"b2"}"#));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"never mind"}"#))
            .expect("a new turn");
        assert_eq!(e.status, AgentStatus::Working);
        assert!(t.blocks.is_empty());
    }

    #[test]
    fn permission_blocks_once_with_attention() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/a/b/src/main.rs"}}"#,
                ),
            )
            .expect("permission");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: "Edit".into() }));
        assert_eq!(e.detail.as_deref(), Some("Edit src/main.rs"));
        assert!(e.attention);

        // The notification for the same prompt is a quiet correction, not a second alert.
        let e = t.apply(
            sid,
            &hook(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use Edit"}"#,
            ),
        );
        assert!(e.is_none_or(|e| !e.attention));

        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PostToolUse","tool_name":"Edit"}"#))
            .expect("resumes");
        assert_eq!(e.status, AgentStatus::Working);
        assert!(!e.attention);
    }

    /// `AskUserQuestion` is answered through the permission hook, so its `PermissionRequest`
    /// and the permission prompt after it are the question, never a permission to use it.
    #[test]
    fn a_question_asked_through_the_permission_hook_stays_a_question() {
        let ask = r#"{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which framework?"}]},"tool_use_id":"q1""#;
        let prompt = r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission to use AskUserQuestion"}"#;
        let bare = r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#;
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(sid, &hook(&format!(r#"{ask},"hook_event_name":"PreToolUse"}}"#)))
            .expect("a question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert!(e.attention);
        let e = t.apply(sid, &hook(&format!(r#"{ask},"hook_event_name":"PermissionRequest"}}"#)));
        assert!(
            e.is_none_or(
                |e| e.status == AgentStatus::Blocked(BlockReason::Question) && !e.attention
            ),
            "still the question, no second alert"
        );
        assert_eq!(t.apply(sid, &hook(prompt)), None);
        assert_eq!(t.apply(sid, &hook(bare)), None);
        assert_eq!(t.status(), &AgentStatus::Blocked(BlockReason::Question));

        let e = Tracker::default()
            .apply(sid, &hook(&format!(r#"{ask},"hook_event_name":"PermissionRequest"}}"#)))
            .expect("a question by its permission request alone");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert_eq!(e.detail.as_deref(), Some("Which framework?"));
        let e = Tracker::default().apply(sid, &hook(prompt)).expect("a prompt alone");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
    }

    #[test]
    fn questions_and_idle_prompts_block() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{}}"#))
            .expect("question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"waiting"}"#))
            .expect("idle");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::IdlePrompt));
        assert!(!e.attention, "already blocked: no second alert");
    }

    /// Every event reads and writes as its name; one a newer Claude Code adds, or none at all,
    /// reads as `Other`.
    #[test]
    fn an_event_is_spelled_as_its_name_and_a_new_one_reads_as_other() {
        let ours = [HookEvent::Report, HookEvent::Statusline, HookEvent::Other];
        for event in HOOK_EVENTS.into_iter().chain(ours) {
            let name = serde_json::json!(event.as_str());
            assert_eq!(serde_json::to_value(event).expect("json"), name, "{event}");
            assert_eq!(serde_json::from_value::<HookEvent>(name).expect("json"), event);
        }
        assert_eq!(hook(r#"{"hook_event_name":"MessageDisplay"}"#).event, HookEvent::Other);
        assert_eq!(hook("{}").event, HookEvent::Other);
    }

    #[test]
    fn unknown_events_and_duplicates_are_silent() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        assert!(
            t.apply(sid, &hook(r#"{"hook_event_name":"PreCompact","trigger_type":"auto"}"#))
                .is_none()
        );
        assert!(
            t.apply(
                sid,
                &hook(r#"{"hook_event_name":"Notification","notification_type":"auth_success"}"#)
            )
            .is_none()
        );
        assert!(t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).is_some());
        assert!(t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).is_none());
        assert!(t.apply(sid, &hook(r#"{"unrelated":true}"#)).is_none());
    }

    #[test]
    fn table_snapshots_live_sessions_and_drops_ended_ones() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.apply(a, &hook(r#"{"session_id":"x","hook_event_name":"SessionStart"}"#));
        table.apply(b, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#));
        assert_eq!(table.snapshot().len(), 2);
        table.apply(a, &hook(r#"{"hook_event_name":"SessionEnd"}"#));
        assert_eq!(table.snapshot().len(), 1);
        table.forget(b);
        assert_eq!(table.snapshot(), Vec::<AgentEvent>::new());
    }

    #[test]
    fn blocked_and_done_say_what_they_want() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which framework?","header":"Framework","options":[]}]}}"#,
                ),
            )
            .expect("question");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Question));
        assert_eq!(e.detail.as_deref(), Some("Which framework?"));
        assert!(!wants_transcript(&e), "the payload said it");

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Elicitation","mcp_server_name":"x","message":"Please provide your credentials","mode":"form"}"#,
                ),
            )
            .expect("elicitation");
        assert_eq!(e.detail.as_deref(), Some("Please provide your credentials"));

        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","stop_hook_active":false,"last_assistant_message":"All green.\n\nDone. `opened-enter` was created."}"#,
                ),
            )
            .expect("stop");
        assert_eq!(e.status, AgentStatus::Done);
        assert_eq!(e.detail.as_deref(), Some("Done. `opened-enter` was created."));

        // A stop always carries its message, so even one that said nothing is not looked up.
        let e = Tracker::default()
            .apply(sid, &hook(r#"{"hook_event_name":"Stop","transcript_path":"/x.jsonl"}"#))
            .expect("stop");
        assert!(!wants_transcript(&e));

        // A question the payload does not spell out: the daemon asks the transcript.
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Notification","notification_type":"agent_needs_input","transcript_path":"/x.jsonl"}"#,
                ),
            )
            .expect("question");
        assert_eq!(e.detail, None);
        assert!(wants_transcript(&e));
        assert!(t.set_detail("Which framework?"));
        assert!(!t.set_detail("Which framework?"));
        assert_eq!(t.event(sid).detail.as_deref(), Some("Which framework?"));
    }

    #[test]
    fn a_bare_permission_notification_keeps_the_request_detail() {
        // Observed with Claude Code 2.1.261: the notification that follows the request says
        // only "Claude needs your permission".
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}}"#,
                ),
            )
            .expect("permission");
        assert_eq!(e.detail.as_deref(), Some("$ touch x"));
        let e = t.apply(
            sid,
            &hook(
                r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#,
            ),
        );
        assert_eq!(e, None, "nothing to add: the request already said it all");
        assert_eq!(
            t.status(),
            &AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() })
        );
        // Without a preceding request the notification still blocks, tool unknown.
        let mut t = Tracker::default();
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Notification","notification_type":"permission_prompt","message":"Claude needs your permission"}"#,
                ),
            )
            .expect("blocks");
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: String::new() }));
    }

    /// What the worker sees of a terminal running `program` with `title`, always the same
    /// process ([`process`] for another one).
    fn seen(program: &str, title: Option<&str>) -> Observation {
        process(program, title, 4321)
    }

    /// [`seen`] for a named process: `pid`, started a second per pid after the epoch.
    fn process(program: &str, title: Option<&str>, pid: i32) -> Observation {
        Observation {
            program: Some(Program::named(program)),
            title: title.map(str::to_owned),
            cwd: Some(PathBuf::from("/tmp/project")),
            pid: Some(pid),
            started: Some(now()),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(1_000_000))
            .expect("a time in range")
    }

    /// A prompt Claude Code takes back (Esc right after Enter) leaves no transcript row and fires
    /// no `Stop`: once its title has said idle for [`TAKEN_BACK_PROBES`] probes in a row, the agent
    /// is idle. A spinner in between starts the count again, and a prompt the transcript took is
    /// never taken as gone back, however long its title rests.
    #[test]
    fn a_prompt_taken_back_leaves_the_agent_idle_by_its_title() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", Some("✳ Claude Code")));
        let prompt = hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"go"}"#);
        assert_eq!(t.apply(sid, &prompt).map(|e| e.status), Some(AgentStatus::Working));
        let idle = seen("claude", Some("✳ Claude Code"));
        for _probe in 1..TAKEN_BACK_PROBES {
            assert_eq!(t.observe(sid, &idle), None);
        }
        assert_eq!(t.observe(sid, &seen("claude", Some("✶ Claude Code"))), None, "it works");
        for _probe in 1..TAKEN_BACK_PROBES {
            assert_eq!(t.observe(sid, &idle), None, "counted again");
        }
        let e = t.observe(sid, &idle).expect("taken back");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Title));
        assert!(!e.attention, "the person did it");
        assert_eq!(t.apply(sid, &prompt).map(|e| e.status), Some(AgentStatus::Working));

        let written = Progress { status: AgentStatus::Working, detail: None };
        t.observe_progress(sid, &written);
        for _probe in 0..TAKEN_BACK_PROBES.saturating_mul(2) {
            assert_eq!(t.observe(sid, &idle), None, "the transcript took it");
        }
        assert_eq!(t.status(), &AgentStatus::Working);
    }

    #[test]
    fn a_hand_started_claude_is_attributed_from_the_process_and_the_title() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        // A shell is not an agent, and an empty tracker has nothing to clear.
        assert_eq!(t.observe(sid, &seen("zsh", Some("~/project"))), None);

        // The process alone: the agent is there, idle, and the pill can be drawn.
        let e = t.observe(sid, &seen("claude", None)).expect("the process");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.source, AgentSource::Process);
        assert!(!e.attention);
        assert_eq!(t.observe(sid, &seen("claude", None)), None, "nothing changed");

        // Its title says a turn is running; the sparkle and the summary say it ended.
        let e = t.observe(sid, &seen("claude", Some("◐ Claude Code"))).expect("the title");
        assert_eq!((e.status, e.source), (AgentStatus::Working, AgentSource::Title));
        let e = t.observe(sid, &seen("claude", Some("✳ fix the build"))).expect("back to idle");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Title));

        // The transcript outranks the title and says what the turn is doing.
        let tool = Progress {
            status: AgentStatus::Tool { tool: "Bash".to_owned() },
            detail: Some("cargo test".to_owned()),
        };
        let e = t.observe_progress(sid, &tool).expect("the transcript");
        assert_eq!(e.status, AgentStatus::Tool { tool: "Bash".to_owned() });
        assert_eq!(e.source, AgentSource::Transcript);
        assert_eq!(e.detail.as_deref(), Some("cargo test"));
        assert_eq!(
            t.observe(sid, &seen("claude", Some("claude"))),
            None,
            "a title never overwrites the transcript"
        );

        // A finished turn we watched run is worth the human's attention.
        let done = Progress { status: AgentStatus::Done, detail: Some("Fixed.".to_owned()) };
        let e = t.observe_progress(sid, &done).expect("done");
        assert!(e.attention);

        // The process goes away: so does the agent.
        let e = t.observe(sid, &seen("zsh", Some("~/project"))).expect("gone");
        assert_eq!(e.status, AgentStatus::None);
    }

    #[test]
    fn a_transcript_read_for_the_first_time_is_not_an_alert() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        let done = Progress { status: AgentStatus::Done, detail: Some("Hours ago.".to_owned()) };
        let e = t.observe_progress(sid, &done).expect("done");
        assert_eq!(e.status, AgentStatus::Done);
        assert!(!e.attention, "the turn ended before we ever looked");
    }

    #[test]
    fn hooks_outrank_everything_and_decide_when_the_agent_ends() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", Some("◐ Claude Code"))).expect("the title");
        let e = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"touch x"}}"#,
                ),
            )
            .expect("the hook");
        assert_eq!(e.source, AgentSource::Hook);
        assert_eq!(e.status, AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() }));

        // Neither weaker signal may talk over a blocked agent, and one probe without it in the
        // foreground may not end it: the hooks are relayed by a process that is itself briefly
        // the session's foreground one.
        assert_eq!(t.observe(sid, &seen("claude", Some("✳ touch x"))), None);
        let progress = Progress { status: AgentStatus::Working, detail: None };
        assert_eq!(t.observe_progress(sid, &progress), None);
        assert_eq!(t.observe(sid, &seen("slopty", Some("~/project"))), None);
        assert_eq!(
            t.status(),
            &AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() })
        );
    }

    /// Esc ends a turn with no hook at all: the transcript's interrupt record takes a hooked
    /// agent from working, a tool or a block to idle without an alert, and nothing else the
    /// transcript says gets past the hooks. The person's stop stands until they prompt again.
    #[test]
    fn an_interrupted_turn_goes_idle_from_the_transcript() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.apply(
            sid,
            &hook(r#"{"session_id":"abc","hook_event_name":"UserPromptSubmit","prompt":"go"}"#),
        );
        t.apply(sid, &hook(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ls"},"tool_use_id":"b1"}"#));
        assert!(!t.interrupted(), "working");
        let idle = Progress {
            status: AgentStatus::Idle,
            detail: Some(transcript::INTERRUPTED.to_owned()),
        };
        let e = t.observe_progress(sid, &idle).expect("interrupted");
        assert_eq!(e.status, AgentStatus::Idle);
        assert_eq!(e.detail.as_deref(), Some("interrupted"));
        assert!(!e.attention, "the human did it");
        assert!(t.blocks.is_empty(), "the permission it waited on is gone with the turn");
        assert!(t.interrupted(), "the person's stop stands");
        assert_eq!(t.observe_progress(sid, &idle), None, "already idle");

        let working = Progress { status: AgentStatus::Working, detail: Some("x".to_owned()) };
        assert_eq!(t.observe_progress(sid, &working), None, "the hooks still decide the rest");
        assert!(t.interrupted(), "the transcript alone does not lift it from a hooked agent");
        t.apply(
            sid,
            &hook(r#"{"hook_event_name":"Notification","notification_type":"idle_prompt"}"#),
        );
        assert!(t.interrupted(), "only the person's prompt lifts it");
        let e = t
            .apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"again"}"#))
            .expect("the next turn");
        assert_eq!(e.status, AgentStatus::Working);
        assert_eq!(e.source, AgentSource::Hook);
        assert!(!t.interrupted(), "they spoke again");
    }

    /// The table says the person stopped a session's agent until they prompt it again, and
    /// says no of a session it does not know; an agent no hook speaks for is lifted by the
    /// transcript's next prompt.
    #[test]
    fn the_table_keeps_the_persons_stop_until_their_next_prompt() {
        let (hooked, bare) = (SessionId::new(), SessionId::new());
        let mut table = AgentTable::default();
        table.apply(hooked, &hook(r#"{"session_id":"one","hook_event_name":"UserPromptSubmit"}"#));
        let stop = Progress {
            status: AgentStatus::Idle,
            detail: Some(transcript::INTERRUPTED.to_owned()),
        };
        table.observe_progress(hooked, &stop);
        assert!(table.interrupted(hooked));
        assert!(!table.interrupted(bare), "nothing known");
        table.apply(hooked, &hook(r#"{"session_id":"one","hook_event_name":"UserPromptSubmit"}"#));
        assert!(!table.interrupted(hooked));

        table.observe(bare, &seen("claude", None)).expect("the process");
        table.observe_progress(bare, &stop);
        assert!(table.interrupted(bare), "the transcript says it with no hooks");
        let prompt = Progress { status: AgentStatus::Working, detail: Some("go on".to_owned()) };
        table.observe_progress(bare, &prompt);
        assert!(!table.interrupted(bare));
    }

    #[test]
    fn a_hooked_agent_ends_when_the_process_it_ran_in_stays_gone() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        t.apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"hi"}"#))
            .expect("the hook");

        // `claude` killed with no `SessionEnd`: the pill stays through the probes a hook relay
        // could account for, then goes.
        let shell = seen("zsh", Some("~/project"));
        for probe in 1..ABSENT_BEFORE_GONE {
            assert_eq!(t.observe(sid, &shell), None, "probe {probe}");
        }
        let e = t.observe(sid, &shell).expect("gone");
        assert_eq!(e.status, AgentStatus::None);

        // An agent that only ever spoke through hooks is never ended this way: nothing of it
        // was ever in the foreground to disappear.
        let mut t = Tracker::default();
        t.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).expect("the hook");
        for _probe in 0..ABSENT_BEFORE_GONE.saturating_mul(2) {
            assert_eq!(t.observe(sid, &shell), None);
        }
        assert_eq!(t.status(), &AgentStatus::Idle);
    }

    #[test]
    fn a_second_claude_in_the_same_terminal_is_a_new_agent() {
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(sid, &process("claude", None, 100)).expect("the first agent");
        table.apply(sid, &hook(r#"{"session_id":"one","hook_event_name":"UserPromptSubmit"}"#));
        table.set_transcript_path(sid, Path::new("/tmp/one.jsonl"));

        // Between two probes the first `claude` exited and a second one started: nothing the
        // first said — its conversation, its status, its hooks — is about this one.
        let e = table.observe(sid, &process("claude", None, 101)).expect("the second agent");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Process));
        assert_eq!(table.transcript_path(sid), None, "the old conversation is not this one's");
        assert_eq!(
            table.discoveries().first().and_then(|d| d.current.clone()),
            None,
            "and it is looked up again"
        );
        // The same process seen again changes nothing.
        assert_eq!(table.observe(sid, &process("claude", None, 101)), None);
    }

    #[test]
    fn an_event_a_hook_has_overtaken_is_not_current_any_more() {
        // The daemon computes poll events, then reads files; a hook can arrive in between and
        // has already told every client something newer. `is_current` is what stops the poll's
        // event from putting the older state back.
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        let stale = table.observe(sid, &seen("claude", None)).expect("the process");
        assert!(table.is_current(&stale));
        table
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf /"}}"#,
                ),
            )
            .expect("the hook");
        assert!(!table.is_current(&stale), "the hook spoke after the poll was computed");

        // An event that says the agent ended is current exactly while there is no agent.
        let gone = Tracker::default().event(sid);
        assert!(!table.is_current(&gone));
        table.forget(sid);
        assert!(table.is_current(&gone));
    }

    #[test]
    fn a_lookup_that_says_nothing_changes_nothing() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.observe(sid, &seen("claude", None)).expect("the process");
        let blind = Observation::default();
        assert_eq!(t.observe(sid, &blind), None);
        assert_eq!(t.status(), &AgentStatus::Idle);
    }

    #[test]
    fn the_table_asks_for_a_transcript_only_for_agents() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        table.observe(b, &seen("zsh", None));
        assert_eq!(table.snapshot().len(), 1, "only the agent has an entry");
        assert_eq!(
            table.discoveries(),
            vec![Discovery {
                session: a,
                cwd: PathBuf::from("/tmp/project"),
                since: now(),
                current: None,
            }]
        );
        assert!(table.set_transcript_path(a, Path::new("/tmp/a.jsonl")), "the file was found");
        assert!(!table.set_transcript_path(a, Path::new("/tmp/a.jsonl")), "the same file");
        assert_eq!(table.transcript_path(a).as_deref(), Some(Path::new("/tmp/a.jsonl")));

        // The conversation the tail then reads drives the pill.
        let progress = Progress { status: AgentStatus::Working, detail: Some("fix it".to_owned()) };
        let e = table.observe_progress(a, &progress).expect("progress");
        assert_eq!(e.source, AgentSource::Transcript);
        assert_eq!(table.observe_progress(b, &progress), None, "no agent, no progress");

        // The agent exits and the table forgets it.
        table.observe(a, &seen("zsh", None));
        assert_eq!(table.snapshot(), Vec::<AgentEvent>::new());
    }

    #[test]
    fn a_cleared_conversation_is_looked_up_again_and_the_status_follows_the_new_file() {
        let a = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        assert!(table.set_transcript_path(a, Path::new("/tmp/first.jsonl")));
        let done = Progress { status: AgentStatus::Done, detail: Some("Fixed.".to_owned()) };
        table.observe_progress(a, &done).expect("the first conversation ends");

        // `/clear` writes a new file: the session keeps asking, and says what it reads now, so
        // the daemon can notice the answer moved and start the tail over.
        assert_eq!(
            table.discoveries().first().and_then(|d| d.current.clone()).as_deref(),
            Some("/tmp/first.jsonl")
        );
        assert!(table.set_transcript_path(a, Path::new("/tmp/second.jsonl")), "the file moved");
        let working = Progress { status: AgentStatus::Working, detail: Some("again".to_owned()) };
        let e = table.observe_progress(a, &working).expect("the new conversation");
        assert_eq!(e.status, AgentStatus::Working, "the pill is not stuck on the old file");

        // A hook names the file itself; that one is never looked up again.
        let b = SessionId::new();
        table.observe(b, &seen("claude", None));
        table.apply(b, &hook(r#"{"hook_event_name":"SessionStart","transcript_path":"/x.jsonl"}"#));
        assert!(table.discoveries().iter().all(|d| d.session != b));
    }

    #[test]
    fn a_session_the_worker_no_longer_runs_takes_its_agent_with_it() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(a, &seen("claude", None));
        table.apply(b, &hook(r#"{"hook_event_name":"SessionStart"}"#));
        assert_eq!(table.retain(&[a, b]), Vec::new(), "both still run");
        let gone = table.retain(&[b]);
        assert_eq!(gone.len(), 1, "{gone:#?}");
        assert_eq!((gone[0].session, gone[0].status.clone()), (a, AgentStatus::None));
        assert_eq!(table.snapshot().len(), 1);
        // Even a hooked agent goes when its terminal does; no `SessionEnd` ever arrives.
        assert_eq!(table.retain(&[]).len(), 1);
        assert_eq!(table.snapshot(), Vec::<AgentEvent>::new());
    }

    #[test]
    fn every_tool_is_described_by_the_argument_that_names_its_work() {
        let detail = |tool: &str, input: &str| {
            hook(&format!(
                r#"{{"hook_event_name":"PreToolUse","tool_name":"{tool}","tool_input":{input}}}"#
            ))
            .tool_detail()
        };
        assert_eq!(detail("Bash", r#"{"command":"ls\n-la"}"#).as_deref(), Some("$ ls"));
        assert_eq!(
            detail("Edit", r#"{"file_path":"/Users/x/proj/src/lib.rs"}"#).as_deref(),
            Some("Edit src/lib.rs")
        );
        assert_eq!(detail("Glob", r#"{"pattern":"**/*.rs"}"#).as_deref(), Some("Glob **/*.rs"));
        assert_eq!(detail("Grep", r#"{"pattern":"todo"}"#).as_deref(), Some("Grep todo"));
        assert_eq!(detail("Agent", r#"{"description":"scan"}"#).as_deref(), Some("Agent: scan"));
        assert_eq!(detail("Task", r#"{"description":"scan"}"#).as_deref(), Some("Agent: scan"));
        assert_eq!(
            detail("WebFetch", r#"{"url":"https://x.test"}"#).as_deref(),
            Some("Fetch https://x.test")
        );
        assert_eq!(detail("WebSearch", r#"{"query":"gpui"}"#).as_deref(), Some("Search gpui"));
        assert_eq!(detail("Skill", r#"{"skill":"commit"}"#).as_deref(), Some("/commit"));
        // A tool we do not know, or a known one without its argument, is named bare.
        assert_eq!(detail("Foo", r#"{"x":1}"#).as_deref(), Some("Foo"));
        assert_eq!(detail("Glob", "{}").as_deref(), Some("Glob"));
        // The tool in a permission message stops at a space or a full stop.
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Bash.")).as_deref(),
            Some("Bash")
        );
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Read tool")).as_deref(),
            Some("Read")
        );
        assert_eq!(tool_from_message(Some("Claude needs your permission to use ")), None);
    }

    #[test]
    fn the_other_notifications_block_on_the_human_and_elicitations_end() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        let note = |kind: &str| {
            hook(&format!(r#"{{"hook_event_name":"Notification","notification_type":"{kind}"}}"#))
        };
        let status = |t: &mut Tracker, kind: &str| t.apply(sid, &note(kind)).map(|e| e.status);
        assert_eq!(
            status(&mut t, "agent_needs_input"),
            Some(AgentStatus::Blocked(BlockReason::Question))
        );
        assert_eq!(
            status(&mut t, "elicitation_dialog"),
            Some(AgentStatus::Blocked(BlockReason::Elicitation))
        );
        assert_eq!(status(&mut t, "elicitation_complete"), Some(AgentStatus::Working));
        assert_eq!(
            status(&mut t, "elicitation_url_dialog"),
            Some(AgentStatus::Blocked(BlockReason::Elicitation))
        );
        assert_eq!(status(&mut t, "elicitation_response"), Some(AgentStatus::Working));
    }

    #[test]
    fn the_table_lists_its_agents_and_takes_a_detail_once() {
        let a = SessionId::new();
        let b = SessionId::new();
        let mut table = AgentTable::default();
        assert_eq!(table.sessions_with_agents(), Vec::<SessionId>::new());
        table.observe(a, &seen("claude", None));
        table.observe(b, &seen("zsh", None));
        assert_eq!(table.sessions_with_agents(), vec![a]);
        assert!(table.set_detail(a, "fix it"), "a new detail");
        assert!(!table.set_detail(a, "fix it"), "the same detail");
        assert!(!table.set_detail(b, "fix it"), "no agent, nothing to put it on");
        assert_eq!(table.snapshot()[0].detail.as_deref(), Some("fix it"));
    }

    #[test]
    fn an_event_from_another_source_is_not_current_even_with_the_same_status() {
        // The poll said idle from the process; a hook then said idle too. The poll's event
        // would put the weaker source back, so it is stale.
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        let stale = table.observe(sid, &seen("claude", None)).expect("the process");
        assert_eq!((&stale.status, stale.source), (&AgentStatus::Idle, AgentSource::Process));
        let hooked =
            table.apply(sid, &hook(r#"{"hook_event_name":"SessionStart"}"#)).expect("the hook");
        assert_eq!((&hooked.status, hooked.source), (&AgentStatus::Idle, AgentSource::Hook));
        assert!(!table.is_current(&stale));
        assert!(table.is_current(&hooked));
    }

    #[test]
    fn details_are_short() {
        let long = "x".repeat(200);
        let h = hook(&format!(
            r#"{{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{{"command":"{long}"}}}}"#
        ));
        let d = h.tool_detail().expect("detail");
        assert!(d.chars().count() <= DETAIL_MAX);
        assert!(d.ends_with('…'));
        assert_eq!(
            tool_from_message(Some("Claude needs your permission to use Bash")).as_deref(),
            Some("Bash")
        );
        assert_eq!(short_path("/Users/x/proj/src/lib.rs"), "src/lib.rs");
    }

    /// `claude <line>` running in `/tmp/project`, as the process table names it.
    fn claude(line: &str) -> Observation {
        let argv = std::iter::once("claude").chain(line.split_whitespace()).map(str::to_owned);
        Observation {
            program: Some(Program { name: "claude".into(), argv: argv.collect() }),
            ..seen("claude", None)
        }
    }

    fn resumed(table: &AgentTable, sid: SessionId) -> (String, Vec<String>) {
        match table.resumable(sid) {
            resume::Resumable::Yes(r) => (r.session, r.args),
            other => panic!("nothing to resume: {other:?}"),
        }
    }

    /// The conversation a hooked agent holds comes back with the flags of its command line and
    /// the mode it was last in; `/clear` moves it to the new conversation, and between the
    /// two hooks the old one still stands.
    #[test]
    fn the_newest_conversation_is_the_one_to_resume() {
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        assert_eq!(table.resumable(sid), resume::Resumable::No);
        table.observe(sid, &claude("--model opus --append-system-prompt hush-hush fix-it"));
        assert_eq!(table.resumable(sid), resume::Resumable::Unknown, "which one is not known");
        table.apply(
            sid,
            &hook(
                r#"{"session_id":"a1","hook_event_name":"SessionStart","source":"startup",
                "permission_mode":"plan","transcript_path":"/t/a1.jsonl"}"#,
            ),
        );
        assert_eq!(
            resumed(&table, sid),
            (
                "a1".into(),
                vec!["--model".into(), "opus".into(), "--permission-mode".into(), "plan".into()]
            )
        );
        let resume::Resumable::Yes(r) = table.resumable(sid) else { panic!("nothing to resume") };
        assert_eq!(
            (r.cwd.as_str(), r.transcript.as_deref()),
            ("/tmp/project", Some("/t/a1.jsonl"))
        );

        table.apply(
            sid,
            &hook(r#"{"session_id":"a1","hook_event_name":"SessionEnd","reason":"clear"}"#),
        );
        assert_eq!(resumed(&table, sid).0, "a1", "until the next one starts");
        table.apply(
            sid,
            &hook(r#"{"session_id":"b2","hook_event_name":"SessionStart","source":"clear","permission_mode":"default"}"#),
        );
        table.observe(sid, &claude("--model opus --append-system-prompt hush-hush fix-it"));
        assert_eq!(resumed(&table, sid), ("b2".into(), vec!["--model".into(), "opus".into()]));
    }

    /// An agent the person ended does not come back; one ended by a signal does, until the
    /// shell is seen back in the foreground for a while. A `--print` run never does.
    #[test]
    fn only_a_conversation_the_person_left_running_comes_back() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        let start = hook(r#"{"session_id":"a1","hook_event_name":"SessionStart","cwd":"/w"}"#);
        table.apply(sid, &start);
        table.apply(sid, &hook(r#"{"session_id":"a1","hook_event_name":"SessionEnd","reason":"prompt_input_exit"}"#));
        assert_eq!(table.resumable(sid), resume::Resumable::No);

        table.apply(sid, &start);
        table.apply(
            sid,
            &hook(r#"{"session_id":"a1","hook_event_name":"SessionEnd","reason":"other"}"#),
        );
        assert_eq!(resumed(&table, sid).0, "a1", "a reboot's signal");
        for _probe in 1..ABSENT_BEFORE_GONE {
            table.observe(sid, &seen("zsh", None));
        }
        assert_eq!(resumed(&table, sid).0, "a1", "the relay passes through the foreground");
        table.observe(sid, &seen("zsh", None));
        assert_eq!(table.resumable(sid), resume::Resumable::No, "the person is at the shell");

        let other = SessionId::new();
        table.observe(other, &claude("-p hello"));
        table.apply(other, &start);
        assert_eq!(table.resumable(other), resume::Resumable::No);
    }

    /// An agent no hook speaks for is resumed from the transcript found for it, whose name is
    /// the conversation's id; several agents each keep their own.
    #[test]
    fn an_unhooked_agent_is_resumed_from_its_transcript() {
        let (a, b, mut table) = (SessionId::new(), SessionId::new(), AgentTable::default());
        table.observe(a, &claude("--effort high"));
        table.observe(b, &claude(""));
        assert!(table.set_transcript_path(a, Path::new("/h/.claude/projects/p/aa-11.jsonl")));
        assert!(table.set_transcript_path(b, Path::new("/h/.claude/projects/p/bb-22.jsonl")));
        assert_eq!(resumed(&table, a), ("aa-11".into(), vec!["--effort".into(), "high".into()]));
        assert_eq!(resumed(&table, b), ("bb-22".into(), Vec::new()));
    }

    /// A turn that ends with a background command out is paused, not done: its own state, no
    /// attention, the task's description on the badge. The task's notification starts the next
    /// turn (named by its summary), and a turn that ends with nothing out is done, announced.
    #[test]
    fn a_stop_with_work_out_waits_and_the_last_one_is_done() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.apply(sid, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"build it"}"#));
        let paused = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","last_assistant_message":"Started it.",
                    "background_tasks":[{"id":"b1","type":"shell","status":"running",
                    "description":"Sleep then print a marker","command":"sleep 8"}],
                    "session_crons":[]}"#,
                ),
            )
            .expect("paused");
        assert_eq!(paused.status, AgentStatus::Waiting { tasks: 1, crons: 0 });
        assert_eq!(paused.detail.as_deref(), Some("Sleep then print a marker"));
        assert!(!paused.attention, "no done notification for a paused turn");
        let woke = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"UserPromptSubmit","prompt":"<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n<summary>Background command \"Sleep\" completed (exit code 0)</summary>\n</task-notification>"}"#,
                ),
            )
            .expect("woke");
        assert_eq!(woke.status, AgentStatus::Working);
        assert_eq!(
            woke.detail.as_deref(),
            Some(r#"Background command "Sleep" completed (exit code 0)"#)
        );
        let done = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","last_assistant_message":"All done.","background_tasks":[],"session_crons":[]}"#,
                ),
            )
            .expect("done");
        assert_eq!(done.status, AgentStatus::Done);
        assert!(done.attention);

        let paused_again = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","background_tasks":[{"id":"b2","type":"shell","status":"running","description":"cargo test"}]}"#,
                ),
            )
            .expect("paused again");
        assert_eq!(paused_again.status, AgentStatus::Waiting { tasks: 1, crons: 0 });
        let idle = hook(
            r#"{"hook_event_name":"Notification","notification_type":"idle_prompt","message":"Claude is waiting for your input"}"#,
        );
        assert_eq!(t.apply(sid, &idle), None, "a paused turn sits at its prompt by design");
        assert_eq!(t.status(), &AgentStatus::Waiting { tasks: 1, crons: 0 });

        let looped = t
            .apply(
                sid,
                &hook(
                    r#"{"hook_event_name":"Stop","session_crons":[{"id":"c1","schedule":"*/5 * * * *","recurring":true,"prompt":"check the build"}]}"#,
                ),
            )
            .expect("a loop");
        assert_eq!(looped.status, AgentStatus::Waiting { tasks: 0, crons: 1 });
        assert_eq!(looped.detail.as_deref(), Some("check the build"));
    }

    /// A paused agent is not at rest: a nested `claude -p` its background command runs posts
    /// hooks of another conversation, and they do not take the terminal over.
    #[test]
    fn a_paused_agent_keeps_its_conversation_against_a_nested_one() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        t.apply(sid, &hook(r#"{"session_id":"main","hook_event_name":"UserPromptSubmit"}"#));
        t.apply(
            sid,
            &hook(
                r#"{"session_id":"main","hook_event_name":"Stop","background_tasks":[{"id":"b1"}]}"#,
            ),
        );
        let nested = hook(r#"{"session_id":"nested","hook_event_name":"Stop"}"#);
        assert_eq!(t.apply(sid, &nested), None);
        assert_eq!(t.status(), &AgentStatus::Waiting { tasks: 1, crons: 0 });
    }

    /// Claude Code's own agents (compaction, prompt suggestions) stop with an empty type; the
    /// model's subagents, and payloads of builds that sent no type, are the model's.
    #[test]
    fn an_internal_subagent_is_told_by_its_empty_type() {
        let internal = hook(r#"{"hook_event_name":"SubagentStop","agent_type":""}"#);
        assert!(internal.is_internal_subagent());
        let spawned = hook(r#"{"hook_event_name":"SubagentStop","agent_type":"Explore"}"#);
        assert!(!spawned.is_internal_subagent());
        assert!(!hook(r#"{"hook_event_name":"SubagentStart"}"#).is_internal_subagent());
        assert!(!hook(r#"{"hook_event_name":"Stop","agent_type":""}"#).is_internal_subagent());
    }

    /// The model's subagents and task list become the project tree's leaves; Claude Code's
    /// own subagents, and hooks that say nothing of either, do not.
    #[test]
    fn subagent_and_task_hooks_report_the_tree_s_leaves() {
        let session = SessionId::new();
        let started =
            hook(r#"{"hook_event_name":"SubagentStart","agent_id":"ag1","agent_type":"Explore"}"#);
        assert_eq!(
            started.report(session),
            Some(AgentReport::SubagentStarted {
                session,
                agent: "ag1".to_owned(),
                kind: "Explore".to_owned()
            })
        );
        let stopped = hook(
            r#"{"hook_event_name":"SubagentStop","agent_id":"ag1","agent_type":"Explore",
            "agent_transcript_path":"/t/ag1.jsonl","last_assistant_message":"Found it.\nMore"}"#,
        );
        assert_eq!(
            stopped.report(session),
            Some(AgentReport::SubagentStopped {
                session,
                agent: "ag1".to_owned(),
                transcript: Some("/t/ag1.jsonl".to_owned()),
                last: Some("Found it.".to_owned()),
            })
        );
        let done = hook(
            r#"{"hook_event_name":"TaskCompleted","task_id":"3","task_subject":"Read the code"}"#,
        );
        let task =
            NativeTask { id: "3".to_owned(), subject: "Read the code".to_owned(), done: true };
        assert_eq!(done.report(session), Some(AgentReport::NativeTask { session, task }));
        let internal =
            hook(r#"{"hook_event_name":"SubagentStart","agent_id":"c","agent_type":""}"#);
        assert_eq!(internal.report(session), None, "compaction is no node");
        assert_eq!(hook(r#"{"hook_event_name":"SubagentStart"}"#).report(session), None);
        assert_eq!(hook(r#"{"hook_event_name":"Stop"}"#).report(session), None);
    }

    /// The relay keeps the count of work out, whatever it cuts of each entry's text.
    #[test]
    fn a_forwarded_stop_keeps_its_work_and_cuts_its_text() {
        let long = "x".repeat(5_000);
        let payload = serde_json::json!({
            "hook_event_name": "Stop",
            "background_tasks": [{ "id": "b1", "description": long }, { "id": "b2" }],
            "session_crons": [{ "id": "c1", "prompt": long }],
        });
        let hook = hook(&payload.to_string()).trimmed();
        assert_eq!(hook.pending_work(), Some((2, 1)));
        let text = &hook.background_tasks.as_ref().expect("tasks")[0].description;
        assert!(text.chars().count() <= PENDING_TEXT_MAX + 1 && text.ends_with('…'));
    }

    /// A status line's pull request and worktree are news when they change and only then, a
    /// joining client gets them, and they go with the agent.
    #[test]
    fn the_branch_is_told_when_it_changes_and_goes_with_the_agent() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        let line = |pr: Option<u32>| Hook {
            event: HookEvent::Statusline,
            pr: pr.map(|number| PullRequest {
                number,
                url: format!("https://github.com/o/r/pull/{number}"),
                review: None,
                merge_request: false,
            }),
            ..Hook::default()
        };
        assert_eq!(table.branch(sid, &line(Some(6))), None, "no agent to go with");
        table.observe(sid, &seen("claude", None));
        assert_eq!(table.branch(sid, &line(None)), None, "nothing to say");
        let opened = table.branch(sid, &line(Some(7))).expect("a pull request");
        assert_eq!(opened.pr.as_ref().map(|pr| pr.number), Some(7));
        assert_eq!(table.branch(sid, &line(Some(7))), None, "the same again");
        assert_eq!(table.branches(), [opened]);
        let closed = table.branch(sid, &line(None)).expect("merged");
        assert_eq!((closed.pr, closed.worktree), (None, None));
        assert_eq!(table.branches(), Vec::<AgentBranch>::new());
        let stop = hook(r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(table.branch(sid, &stop), None, "only a status line names one");

        assert!(table.branch(sid, &line(Some(8))).is_some());
        table.apply(sid, &hook(r#"{"hook_event_name":"SessionEnd"}"#));
        assert!(table.branches().is_empty(), "the agent went, its branch with it");
    }

    /// After a restart, Claude Code's own list puts back what only hooks had said (a
    /// permission prompt) for the agent it names by pid, quietly; not over a hook heard since,
    /// and not where no relay is registered to keep the status from there on.
    #[test]
    fn claude_codes_own_list_restores_what_the_hooks_had_said() {
        let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut table = AgentTable::default();
        table.observe(a, &process("claude", None, 11));
        table.observe(b, &process("claude", None, 22));
        table.observe(c, &process("claude", None, 33));
        table.apply(b, &hook(r#"{"hook_event_name":"UserPromptSubmit","prompt":"go"}"#));
        let listed: Vec<roster::Listed> = serde_json::from_str(
            r#"[{"pid":11,"sessionId":"s-a","status":"waiting","waitingFor":"permission prompt"},
               {"pid":22,"sessionId":"s-b","status":"idle"},
               {"pid":99,"sessionId":"s-x","status":"busy"}]"#,
        )
        .expect("json");
        let events = table.recover(&listed, true, |_| Vec::new());
        assert_eq!(events.len(), 1, "{events:?}");
        let event = &events[0];
        assert_eq!(event.session, a);
        assert_eq!(
            event.status,
            AgentStatus::Blocked(BlockReason::Permission { tool: String::new() })
        );
        assert_eq!((event.source, event.attention), (AgentSource::Hook, false));
        assert_eq!(event.agent_session.as_deref(), Some("s-a"));
        assert_eq!(
            table.snapshot().iter().find(|e| e.session == b).map(|e| &e.status),
            Some(&AgentStatus::Working)
        );

        let mut unhooked = AgentTable::default();
        unhooked.observe(a, &process("claude", None, 11));
        assert_eq!(unhooked.recover(&listed, false, |_| Vec::new()), Vec::<AgentEvent>::new());
        let kept = unhooked.snapshot();
        assert_eq!(kept[0].status, AgentStatus::Idle, "the process's word stands");
        assert_eq!(kept[0].agent_session.as_deref(), Some("s-a"), "the conversation is known");
    }

    /// A managed launcher runs Claude Code as its child: the registry names the child, and the
    /// terminal's agent (the launcher) is matched through it. A grandchild, or another
    /// process's child, is not its agent.
    #[test]
    fn a_launchers_agent_is_found_by_its_child() {
        let (launched, other, deep) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut table = AgentTable::default();
        table.observe(launched, &process("claude", None, 100));
        table.observe(other, &process("claude", None, 200));
        table.observe(deep, &process("claude", None, 300));
        let listed: Vec<roster::Listed> = serde_json::from_str(
            r#"[{"pid":101,"sessionId":"s-launched","status":"waiting","waitingFor":"input needed"},
               {"pid":302,"sessionId":"s-grandchild","status":"busy"}]"#,
        )
        .expect("json");
        let processes = |pid: i32| match pid {
            100 => vec![105, 101],
            300 => vec![301],
            301 => vec![302],
            _ => Vec::new(),
        };
        let events = table.recover(&listed, true, processes);
        let found: Vec<_> =
            events.iter().map(|e| (e.session, e.agent_session.as_deref())).collect();
        assert_eq!(found, [(launched, Some("s-launched"))]);
        assert_eq!(events[0].status, AgentStatus::Blocked(BlockReason::Question));
    }

    /// An agent that ends leaves its terminal marked until another starts there, whether its
    /// hooks said so or the process table did; a terminal that never had one is not marked.
    #[test]
    fn a_terminal_whose_agent_ended_is_marked_until_another_starts() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        assert!(!table.ended(sid));
        table.apply(sid, &hook(r#"{"hook_event_name":"SessionEnd","reason":"other"}"#));
        assert!(!table.ended(sid), "nothing ran here");
        table.apply(sid, &hook(r#"{"hook_event_name":"SessionStart","source":"startup"}"#));
        assert!(!table.ended(sid));
        table.apply(sid, &hook(r#"{"hook_event_name":"SessionEnd","reason":"prompt_input_exit"}"#));
        assert!(table.ended(sid), "ended by its hooks");
        table.observe(sid, &seen("claude", None)).expect("a new agent");
        assert!(!table.ended(sid), "another started");
        table.observe(sid, &seen("zsh", None)).expect("gone");
        assert!(table.ended(sid), "ended by the process table");
        table.forget(sid);
        assert!(!table.ended(sid));
    }

    /// The mode is reported when a hook of the session's agent first names it and each time it
    /// changes, never twice in a row; a nested `claude -p` in the terminal does not move it.
    #[test]
    fn a_permission_mode_is_reported_when_it_changes() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        let mut heard = |json: &str| {
            table.apply(sid, &hook(json));
            table.permission_mode_report(sid)
        };
        let mode =
            |mode: &str| Some(AgentReport::PermissionMode { session: sid, mode: mode.into() });
        assert_eq!(heard(r#"{"session_id":"a","hook_event_name":"SessionStart"}"#), None);
        let plan =
            r#"{"session_id":"a","hook_event_name":"UserPromptSubmit","permission_mode":"plan"}"#;
        assert_eq!(heard(plan), mode("plan"));
        assert_eq!(heard(plan), None, "the same mode again");
        let nested = r#"{"session_id":"b","hook_event_name":"PreToolUse","tool_name":"Bash","permission_mode":"bypassPermissions"}"#;
        assert_eq!(heard(nested), None, "another agent's hook, while this one works");
        let edits =
            r#"{"session_id":"a","hook_event_name":"Stop","permission_mode":"acceptEdits"}"#;
        assert_eq!(heard(edits), mode("acceptEdits"));
    }

    /// Shift-Tab in the TUI changes the mode without changing the turn: the next hook that
    /// names another mode is an event of its own, carrying the mode and when it was heard, and
    /// the same mode again is not.
    #[test]
    fn a_mode_change_alone_is_an_event_that_carries_it() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        let prompt = |mode: &str| {
            hook(&format!(
                r#"{{"session_id":"a","hook_event_name":"UserPromptSubmit","permission_mode":"{mode}"}}"#
            ))
        };
        let mode_of = |event: &AgentEvent| event.mode.as_ref().map(|m| m.name.clone());
        let working = table.apply(sid, &prompt("default")).expect("the turn starts");
        assert_eq!(working.status, AgentStatus::Working);
        assert_eq!(mode_of(&working).as_deref(), Some("default"));

        let before = WallMs::now();
        let planning = table.apply(sid, &prompt("plan")).expect("the mode alone moved");
        assert_eq!(planning.status, AgentStatus::Working, "the turn did not change");
        assert_eq!(mode_of(&planning).as_deref(), Some("plan"));
        assert!(planning.mode.as_ref().is_some_and(|m| m.heard_ms >= before));
        assert_eq!(table.apply(sid, &prompt("plan")), None, "nothing new");

        let kept = table.snapshot().into_iter().find(|e| e.session == sid).expect("an agent");
        assert_eq!(mode_of(&kept).as_deref(), Some("plan"), "and it is kept");
    }

    /// An agent no hook speaks for says its mode through the transcript's prompts; a hooked
    /// one's hooks outrank that.
    #[test]
    fn a_transcripts_mode_moves_only_an_unhooked_agent() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        table.observe(sid, &claude("")).expect("a new agent");
        let heard = table.hear_mode(sid, "acceptEdits").expect("a new mode");
        assert_eq!(heard.mode.map(|m| m.name).as_deref(), Some("acceptEdits"));
        assert_eq!(table.hear_mode(sid, "acceptEdits"), None, "the same again");

        table.apply(
            sid,
            &hook(r#"{"session_id":"a","hook_event_name":"Stop","permission_mode":"plan"}"#),
        );
        assert_eq!(table.hear_mode(sid, "default"), None, "the hooks speak for it now");
    }

    /// What loosens the agent in the foreground is reported when its command line first says
    /// so, however `claude` was wrapped, and again only when that changes: a later agent in
    /// the same terminal that loosens nothing clears it once.
    #[test]
    fn what_loosens_the_agent_in_the_foreground_is_reported_when_it_changes() {
        let (sid, mut table) = (SessionId::new(), AgentTable::default());
        let loosened = |found: &[&str]| {
            let found = found.iter().map(|f| (*f).to_owned()).collect();
            Some(AgentReport::Loosened { session: sid, found })
        };
        table.observe(sid, &claude("--model opus"));
        assert_eq!(table.loosening_report(sid), None, "nothing loosens, nothing to say");

        let wrapped = Observation {
            program: Some(Program {
                name: "bash".into(),
                argv: ["/bin/sh", "-c", "cd ~/w && claude --allowedTools Bash"]
                    .map(str::to_owned)
                    .to_vec(),
            }),
            ..process("bash", None, 99)
        };
        table.observe(sid, &wrapped);
        assert_eq!(table.loosening_report(sid), loosened(&["--allowedTools"]));
        table.observe(sid, &wrapped);
        assert_eq!(table.loosening_report(sid), None, "said once");

        table.observe(sid, &Observation { pid: Some(100), ..claude("-c") });
        assert_eq!(table.loosening_report(sid), loosened(&[]), "the next agent loosens nothing");
        assert_eq!(table.loosening_report(sid), None);
        let many = "--allowedTools ".repeat(40);
        table.observe(sid, &Observation { pid: Some(101), ..claude(&many) });
        let Some(AgentReport::Loosened { found, .. }) = table.loosening_report(sid) else {
            panic!("reported")
        };
        assert_eq!(found.len(), slopty_proto::project::LOOSENED_MAX, "bounded for the wire");
    }
}
