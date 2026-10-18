//! The work Claude Code does beside the conversation, as its TUI shows it: the background tasks
//! (commands, monitors, subagents, workflows, an MCP server's long task) from the edge events that
//! start and end them and the level that lists those still running, each call's timer, the
//! spinner's figures, its notifications, the goal, the prompt it suggests next, and the rows its
//! TUI derives from events of their own ("Allowed cargo test", "Saved 2 memories", "All
//! background agents stopped", a fallback model, a hook that failed).
//!
//! - **Tasks.** `task_started` adds a task to the thread's list ([`BackgroundTask`]), its kind by
//!   Claude Code's `task_type` (a `Monitor` call's shell is a monitor), its call noted with the
//!   task's id; `task_progress` and `task_updated` change it; `background_tasks_changed` is the
//!   level, replacing what runs (a task gone from it ended, whatever its notice says once it
//!   comes); `task_notification` is the row it ends with, rebuilt as the transcript records it, its
//!   figures (tokens, tool uses, time) and the resources it returned kept.
//! - **Timers.** `tool_progress` notes a running call's elapsed time ([`CallNote`]) when it first
//!   comes and then every [`PROGRESS_EVERY`], and at once when a subagent's request is tried again:
//!   a client counts the seconds between, as the TUI does.
//! - **The spinner.** The tokens the turn's answers have taken so far, how long the model has been
//!   thinking, and the phrase Claude Code gives the turn (`task_summary`), told at most once a
//!   second ([`Activity`]); gone when the turn ends.
//! - **Tails.** A background command's output is read from the file Claude Code writes it to, every
//!   [`TAIL_EVERY`] while one runs ([`Outputs`](crate::conversation::output::Outputs)).

use std::fmt::Write as _;
use std::time::Duration;

use serde_json::Value;
use slopty_core::WallMs;
use slopty_proto::thread::{
    Action, Activity, AgentNote, BackgroundTask, CallNote, Goal, ItemId, Notice, Retry,
    SubagentKind, TaskUsage,
};

use super::{Driven, bool_at, ms, str_at, u64_at};
use crate::claude::history::Out;
use crate::conversation as conv;

/// How often a running call's timer is noted after its first note.
pub const PROGRESS_EVERY: Duration = Duration::from_secs(10);

/// How often a running background command's output is read.
pub const TAIL_EVERY: Duration = Duration::from_secs(5);

/// How close together background agents ended for a stop of them all to name them.
const STOPPED_TOGETHER: Duration = Duration::from_secs(5);

/// Tokens of subagent descriptions past which the TUI warns they crowd every request.
const AGENT_DESCRIPTION_TOKENS: usize = 15_000;

/// How often the spinner's figures are told at most.
const SPIN_EVERY: Duration = Duration::from_secs(1);

/// The spinner's figures as they stand, and as last told.
#[derive(Debug, Default)]
pub(super) struct Spin {
    /// Tokens the turn's finished answers took.
    base: u64,
    /// Tokens the answer under way has taken.
    current: u64,
    /// Tokens Claude Code estimates the thinking under way has taken.
    thinking_tokens: u64,
    thinking_since: Option<WallMs>,
    thought_ms: Option<u64>,
    phrase: Option<String>,
    /// Since when the reply under way, while one streams, has said nothing new.
    quiet_since: Option<WallMs>,
    told: Option<Activity>,
    told_at: Option<WallMs>,
    /// A suggestion shows.
    suggested: bool,
}

impl Spin {
    fn now(&self) -> Activity {
        Activity {
            tokens: self.base.saturating_add(self.current.max(self.thinking_tokens)),
            thinking_since: self.thinking_since,
            thought_ms: self.thought_ms,
            phrase: self.phrase.clone(),
            quiet_since: self.quiet_since,
        }
    }
}

/// The kind of task Claude Code's `task_type` names, `tool` the call that started it.
pub(super) fn task_kind(task_type: &str, tool: Option<&str>) -> String {
    match task_type {
        "local_bash" if tool == Some("Monitor") => BackgroundTask::MONITOR,
        "local_bash" => BackgroundTask::SHELL,
        "local_agent" => BackgroundTask::AGENT,
        "local_workflow" => BackgroundTask::WORKFLOW,
        "mcp_task" => BackgroundTask::MCP,
        "remote_agent" => BackgroundTask::REMOTE,
        "in_process_teammate" => BackgroundTask::TEAMMATE,
        "monitor_mcp" | "monitor_ws" => BackgroundTask::MONITOR,
        other => other,
    }
    .to_owned()
}

/// The figures a task's `usage` gives: tokens, tool uses, how long it ran.
fn usage_of(message: &Value) -> Option<TaskUsage> {
    let usage = message.get("usage")?;
    Some(TaskUsage {
        tokens: u64_at(usage, "total_tokens").unwrap_or_default(),
        tool_uses: u64_at(usage, "tool_uses").unwrap_or_default(),
        duration_ms: u64_at(usage, "duration_ms").unwrap_or_default(),
    })
}

/// Whether `subtype` is a system message a row of the TUI's own is derived from.
pub(super) fn derived(subtype: &str) -> bool {
    matches!(
        subtype,
        "memory_saved"
            | "memory_recall"
            | "agents_killed"
            | "permission_retry"
            | "model_fallback"
            | "model_refusal_fallback"
            | "model_consent_fallback"
            | "away_summary"
            | "hook_response"
            | "stop_hook_summary"
            | "local_command_output"
            | "plugin_install"
            | "instruction_size_warning"
            | "scheduled_task_fire"
            | "peer_message_hold"
    )
}

/// `n` of `one`, as people count them ("1 memory", "3 memories").
fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// A time as the TUI says it, its parts of nothing left out: "12s", "5m 3s", "5m", "1h 51m 50s",
/// "1h 3s".
pub(super) fn span(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let parts: Vec<String> = [(h, "h"), (m, "m"), (s, "s")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, unit)| format!("{n}{unit}"))
        .collect();
    if parts.is_empty() { "0s".to_owned() } else { parts.join(" ") }
}

impl Driven {
    /// Each call an assistant `record` makes, kept by its id with its tool and input.
    pub(super) fn note_calls(&mut self, record: &Value) {
        let blocks = record.pointer("/message/content").and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            if str_at(block, "type") != Some("tool_use") {
                continue;
            }
            let (Some(id), Some(name)) = (str_at(block, "id"), str_at(block, "name")) else {
                continue;
            };
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            self.last_call = Some(call_words(name, &input));
            self.calls.insert(id.to_owned(), (name.to_owned(), input));
        }
    }

    /// A running call's timer (`tool_progress`), noted when it first comes, then every
    /// [`PROGRESS_EVERY`], and at once with a retry.
    pub(super) fn tool_progress(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(call) = str_at(message, "tool_use_id") else { return Vec::new() };
        let retry = message.get("subagent_retry").map(|r| {
            let count = |name: &str| u64_at(r, name).and_then(|n| u32::try_from(n).ok());
            Retry {
                attempt: count("attempt").unwrap_or(1),
                max: count("max_retries"),
                in_ms: u64_at(r, "retry_delay_ms"),
            }
        });
        let due = self
            .noted
            .get(call)
            .is_none_or(|at| now.as_millis().saturating_sub(at.as_millis()) >= ms(PROGRESS_EVERY));
        if !due && retry.is_none() {
            return Vec::new();
        }
        self.noted.insert(call.to_owned(), now);
        let note = CallNote {
            elapsed_ms: u64_at(message, "elapsed_time_seconds").map(|s| s.saturating_mul(1000)),
            at_ms: now,
            task: str_at(message, "task_id").map(str::to_owned),
            retry,
            said: None,
            title: None,
        };
        self.history.call_noted(call, note)
    }

    /// The prompt Claude Code suggests next.
    pub(super) fn suggested(&mut self, message: &Value) -> Vec<Out> {
        let suggestion = str_at(message, "suggestion").map(str::trim).filter(|s| !s.is_empty());
        self.activity.suggested = suggestion.is_some();
        self.history.tell(Action::Suggested(suggestion.map(str::to_owned)))
    }

    /// Whatever was suggested goes: the person said something.
    pub(super) fn unsuggest(&mut self) -> Vec<Out> {
        if !std::mem::take(&mut self.activity.suggested) {
            return Vec::new();
        }
        self.history.tell(Action::Suggested(None))
    }

    /// The person's goal (`active_goal`): set, checked again, or met and cleared.
    pub(super) fn goal(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let value = message.get("value").filter(|v| v.is_object());
        let goal = value.map(|v| Goal {
            condition: str_at(v, "condition").unwrap_or_default().to_owned(),
            iterations: u64_at(v, "iterations").and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
            set_ms: WallMs::from_millis(u64_at(v, "set_at").unwrap_or_default()),
            tokens_at_start: u64_at(v, "tokens_at_start").unwrap_or_default(),
            last_reason: str_at(v, "last_reason").map(str::to_owned),
        });
        let mut outs = Vec::new();
        // The TUI's goal rows: its first line, then what it says more ("Goal: …", "Reason: …"),
        // which opens under the row.
        match (&self.goal, &goal) {
            // Cleared by the person: Claude Code says so itself ("Goal cleared: …").
            (Some(_), None) if std::mem::take(&mut self.goal_clearing) => {}
            (Some(had), None) => {
                let took = span(now.as_millis().saturating_sub(had.set_ms.as_millis()));
                let turns = counted(usize::try_from(had.iterations).unwrap_or(0), "turn", "turns");
                let text = format!("Goal achieved ({took} · {turns})\nGoal: {}", had.condition);
                outs.extend(self.history.notice(Notice::GOAL, &text, now));
            }
            (Some(had), Some(now_goal)) if now_goal.iterations > had.iterations => {
                let reason =
                    now_goal.last_reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
                let text = match reason {
                    Some(reason) => format!("Goal not yet met… continuing\nReason: {reason}"),
                    None => "Goal not yet met… continuing".to_owned(),
                };
                outs.extend(self.history.notice(Notice::GOAL, &text, now));
            }
            _ => {}
        }
        if goal.is_none() {
            self.goal_clearing = false;
        }
        self.goal.clone_from(&goal);
        outs.extend(self.history.tell(Action::GoalSet(goal)));
        outs
    }

    /// A task Claude Code started (`task_started`).
    pub(super) fn task_started(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(task) = str_at(message, "task_id").map(str::to_owned) else { return Vec::new() };
        let call = str_at(message, "tool_use_id").map(str::to_owned);
        if let Some(call) = &call {
            self.agents.insert(call.clone(), task.clone());
        }
        let called = call.as_ref().and_then(|c| self.calls.get(c));
        let tool = called.map(|(name, _)| name.as_str());
        let kind = task_kind(str_at(message, "task_type").unwrap_or_default(), tool);
        let title = str_at(message, "description")
            .or_else(|| str_at(message, "workflow_name"))
            .unwrap_or_default();
        let input = called.map(|(_, input)| input);
        let mut started = BackgroundTask {
            item: call.clone().map(ItemId),
            agent_type: str_at(message, "subagent_type")
                .map(|t| crate::conversation::agent_kind(t.to_owned())),
            parent: str_at(message, "parent_task_id").map(str::to_owned),
            ambient: bool_at(message, "ambient") || bool_at(message, "skip_transcript"),
            awaited: bool_at(message, "awaited"),
            backgrounded: message.get("is_backgrounded").and_then(Value::as_bool).unwrap_or(true),
            timeout_ms: input.and_then(|i| u64_at(i, "timeout_ms")),
            persistent: input.is_some_and(|i| bool_at(i, "persistent")),
            ..BackgroundTask::new(&task, &kind, title, now)
        };
        if started.persistent {
            started.timeout_ms = None;
        }
        let mut outs = self.history.task_started(started);
        if let Some(call) = call {
            let note = CallNote { at_ms: now, task: Some(task), ..CallNote::default() };
            outs.extend(self.history.call_noted(&call, note));
        }
        outs
    }

    /// How a task goes (`task_progress`): its figures, and what it does now.
    pub(super) fn task_progress(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(task) = str_at(message, "task_id").map(str::to_owned) else { return Vec::new() };
        let mut outs = Vec::new();
        if !self.history.tasks_now().iter().any(|t| t.id == task) {
            outs.extend(self.task_started(message, now));
        }
        let usage = usage_of(message);
        let step = str_at(message, "summary")
            .or_else(|| str_at(message, "last_tool_name"))
            .map(str::to_owned);
        outs.extend(self.history.task_changed(&task, |t| {
            t.usage = usage.or(t.usage);
            if step.is_some() {
                t.step = step;
            }
        }));
        outs
    }

    /// What changed of a task (`task_updated`), merged into what is known of it.
    pub(super) fn task_updated(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(task) = str_at(message, "task_id").map(str::to_owned) else { return Vec::new() };
        let Some(patch) = message.get("patch") else { return Vec::new() };
        let state = str_at(patch, "status").map(str::to_owned);
        let title = str_at(patch, "description").map(str::to_owned);
        let ended = u64_at(patch, "end_time").map(WallMs::from_millis);
        let error = str_at(patch, "error").map(|e| format!("failed: {e}"));
        let backgrounded = patch.get("is_backgrounded").and_then(Value::as_bool);
        self.history.task_changed(&task, |t| {
            if let Some(state) = state {
                t.state = state;
            }
            if let Some(title) = title {
                t.title = title;
            }
            if !t.is_live() {
                t.ended_ms = ended.or(t.ended_ms).or(Some(now));
            }
            if error.is_some() {
                t.outcome = error;
            }
            if let Some(backgrounded) = backgrounded {
                t.backgrounded = backgrounded;
            }
        })
    }

    /// A task ended (`task_notification`): its row, as the transcript records the notice, with
    /// its figures and the resources it returned.
    pub(super) fn task_notification(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let field = |name: &str| str_at(message, name).unwrap_or_default();
        let mut content = format!("<task-notification>\n<task-id>{}</task-id>\n", field("task_id"));
        if let Some(call) = str_at(message, "tool_use_id").filter(|c| !c.is_empty()) {
            let _written = writeln!(content, "<tool-use-id>{call}</tool-use-id>");
        }
        let _written = writeln!(
            content,
            "<output-file>{}</output-file>\n<status>{}</status>\n<summary>{}</summary>",
            field("output_file"),
            field("status"),
            field("summary"),
        );
        if let Some(usage) = usage_of(message) {
            let _written = writeln!(
                content,
                "<usage><total_tokens>{}</total_tokens><tool_uses>{}</tool_uses>\
                 <duration_ms>{}</duration_ms></usage>",
                usage.tokens, usage.tool_uses, usage.duration_ms,
            );
        }
        content.push_str("</task-notification>");
        if let Some(links) = message.get("resource_links").and_then(Value::as_array) {
            let links = crate::shown::mcp_content(links).links;
            self.history.task_links(field("task_id"), links);
        }
        let record = serde_json::json!({
            "type": "queue-operation",
            "operation": "enqueue",
            "timestamp": super::stamp(now),
            "content": content,
        });
        let changes = self.decoder.ingest(&conv::ThreadId::Main, &record);
        self.history.transcript(&changes, &[])
    }

    /// The tasks that run now (`background_tasks_changed`), which replace those that ran.
    pub(super) fn tasks_changed(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let tasks: Vec<Value> =
            message.get("tasks").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut outs = Vec::new();
        for task in &tasks {
            let Some(id) = str_at(task, "task_id") else { continue };
            let ambient = bool_at(task, "ambient");
            let parent = str_at(task, "parent_task_id").map(str::to_owned);
            if self.history.tasks_now().iter().any(|t| t.id == id) {
                outs.extend(self.history.task_changed(id, |t| {
                    t.ambient = ambient;
                    t.parent.clone_from(&parent);
                    t.backgrounded = true;
                }));
            } else {
                outs.extend(self.task_started(task, now));
            }
        }
        let live: Vec<String> =
            tasks.iter().filter_map(|t| str_at(t, "task_id").map(str::to_owned)).collect();
        outs.extend(self.history.tasks_live(&live, now));
        let active: Vec<&Value> = tasks.iter().filter(|t| !bool_at(t, "ambient")).collect();
        let named = active.first().and_then(|t| str_at(t, "description")).map(str::to_owned);
        let count = u32::try_from(active.len()).unwrap_or(u32::MAX);
        outs.extend(self.history.background(count, 0, named));
        outs
    }

    /// The phrase Claude Code gives the turn under way (`task_summary`); `null` clears it.
    pub(super) fn phrase(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        self.activity.phrase = str_at(message, "detail").map(str::to_owned);
        self.tell_spin(now, true)
    }

    /// Claude Code's estimate of the thinking under way (`thinking_tokens`).
    pub(super) fn thinking(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        self.activity.thinking_tokens = u64_at(message, "estimated_tokens").unwrap_or_default();
        self.activity.thinking_since.get_or_insert(now);
        self.tell_spin(now, false)
    }

    /// What a streamed `event` of the main thread says of the spinner's figures.
    pub(super) fn spin(&mut self, event: &Value, now: WallMs) -> Vec<Out> {
        let spin = &mut self.activity;
        let changed = match str_at(event, "type").unwrap_or_default() {
            "message_start" => {
                spin.quiet_since = Some(now);
                spin.base = spin.base.saturating_add(spin.current.max(spin.thinking_tokens));
                spin.current = event
                    .pointer("/message/usage/output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                spin.thinking_tokens = 0;
                false
            }
            "message_delta" => {
                let out = event.pointer("/usage/output_tokens").and_then(Value::as_u64);
                spin.current = out.unwrap_or(spin.current);
                spin.quiet_since = spin.quiet_since.map(|_| now);
                false
            }
            // A token came: the reply is not quiet.
            "content_block_delta" => {
                spin.quiet_since = spin.quiet_since.map(|_| now);
                false
            }
            // The reply is whole: a tool may run now, which is no silence of the model's.
            "message_stop" => spin.quiet_since.take().is_some(),
            "content_block_start" => {
                let thinking = matches!(
                    event.pointer("/content_block/type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                );
                if thinking {
                    spin.thinking_since = Some(now);
                }
                thinking
            }
            "content_block_stop" => match spin.thinking_since.take() {
                Some(since) => {
                    let took = now.as_millis().saturating_sub(since.as_millis());
                    spin.thought_ms = Some(spin.thought_ms.unwrap_or(0).saturating_add(took));
                    true
                }
                None => false,
            },
            _ => false,
        };
        self.tell_spin(now, changed)
    }

    /// The spinner's figures, told when they changed and a second has passed, or at once when
    /// `now_too`.
    fn tell_spin(&mut self, now: WallMs, now_too: bool) -> Vec<Out> {
        if !self.running {
            return Vec::new();
        }
        let activity = self.activity.now();
        if self.activity.told.as_ref() == Some(&activity) {
            return Vec::new();
        }
        let due = self
            .activity
            .told_at
            .is_none_or(|at| now.as_millis().saturating_sub(at.as_millis()) >= ms(SPIN_EVERY));
        if !due && !now_too {
            return Vec::new();
        }
        self.activity.told = Some(activity.clone());
        self.activity.told_at = Some(now);
        self.history.tell(Action::ActivitySet(Some(activity)))
    }

    /// The turn is over: no spinner, no timers.
    pub(super) fn turn_over(&mut self, _now: WallMs) -> Vec<Out> {
        self.noted.clear();
        let suggested = self.activity.suggested;
        let told = self.activity.told.is_some();
        self.activity = Spin { suggested, ..Spin::default() };
        if told { self.history.tell(Action::ActivitySet(None)) } else { Vec::new() }
    }

    /// One of Claude Code's notifications (`system/notification`).
    pub(super) fn notification(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(text) = str_at(message, "text").filter(|t| !t.trim().is_empty()) else {
            return Vec::new();
        };
        let until = u64_at(message, "timeout_ms")
            .map(|t| WallMs::from_millis(now.as_millis().saturating_add(t)));
        let note = AgentNote {
            key: str_at(message, "key").unwrap_or(text).to_owned(),
            text: text.to_owned(),
            priority: str_at(message, "priority").unwrap_or("medium").to_owned(),
            color: str_at(message, "color").map(str::to_owned),
            at_ms: now,
            until_ms: until,
        };
        self.history.tell(Action::Notified(note))
    }

    /// A call Claude Code's permission system turned down (`permission_denied`): why, under the
    /// call.
    pub(super) fn denied(&mut self, message: &Value, now: WallMs) -> Vec<Out> {
        let Some(call) = str_at(message, "tool_use_id") else { return Vec::new() };
        let reason = str_at(message, "decision_reason")
            .or_else(|| str_at(message, "message"))
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let by = match str_at(message, "decision_reason_type") {
            Some("classifier") => "Denied by the auto-mode classifier",
            Some("rule") => "Denied by a permission rule",
            Some("mode") => "Denied by the permission mode",
            Some("hook") => "Denied by a hook",
            _ => "Denied",
        };
        let said = reason.map_or_else(|| by.to_owned(), |r| format!("{by}: {r}"));
        let note = CallNote { at_ms: now, said: Some(said), ..CallNote::default() };
        self.history.call_noted(call, note)
    }

    /// A row the TUI derives from system message `subtype`.
    pub(super) fn derived_row(&mut self, subtype: &str, message: &Value, now: WallMs) -> Vec<Out> {
        let text = |name: &str| str_at(message, name).map(str::trim).filter(|t| !t.is_empty());
        let list = |name: &str| -> Vec<String> {
            message
                .get(name)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let row = match subtype {
            "memory_saved" => {
                let verb = text("verb").unwrap_or("Saved");
                let n = list("written_paths").len();
                Some((Notice::MEMORY, format!("{verb} {}", counted(n, "memory", "memories"))))
            }
            "memory_recall" => {
                let n = message.get("memories").and_then(Value::as_array).map_or(0, Vec::len);
                Some((Notice::MEMORY, format!("Recalled {}", counted(n, "memory", "memories"))))
            }
            "agents_killed" => Some((Notice::STOPPED, self.agents_stopped(now))),
            "permission_retry" => {
                let commands = list("commands");
                let words = if commands.is_empty() {
                    text("content").unwrap_or("Allowed").to_owned()
                } else {
                    format!("Allowed {}", commands.join(", "))
                };
                Some((Notice::ALLOWED, words))
            }
            "model_fallback" | "model_refusal_fallback" | "model_consent_fallback" => {
                let words = text("content")
                    .map(str::to_owned)
                    .or_else(|| text("fallback_model").map(|model| format!("Switched to {model}")));
                words.map(|w| (Notice::FALLBACK, w))
            }
            "away_summary" | "scheduled_task_fire" | "local_command_output" => {
                let kind =
                    if subtype == "local_command_output" { Notice::COMMAND } else { Notice::INFO };
                text("content").map(|c| (kind, c.to_owned()))
            }
            "hook_response" => {
                let name = text("hook_name").or_else(|| text("hook_event")).unwrap_or("A hook");
                let code = message.get("exit_code").and_then(Value::as_i64);
                let first = |name: &str| text(name).and_then(|t| t.lines().next()).map(str::trim);
                let why = first("stderr").or_else(|| first("output")).or_else(|| first("stdout"));
                let said = |head: String| match why {
                    Some(why) => format!("{head}: {why}"),
                    None => head,
                };
                let event = text("hook_event").unwrap_or(name);
                match (text("outcome"), code) {
                    // A hook that decided a call's permission says so, as the TUI's
                    // "Allowed by PreToolUse hook" (a deny is said under the call it stopped).
                    (Some("success") | None, Some(0) | None) if hook_allowed(message) => {
                        Some((Notice::ALLOWED, format!("Allowed by {event} hook")))
                    }
                    // It said nothing past success: quiet, as the TUI keeps it.
                    (Some("success") | None, Some(0) | None) => first("stdout")
                        .filter(|_| text("outcome") == Some("success"))
                        .map(|out| (Notice::HOOK, format!("{name} hook: {out}"))),
                    (Some("cancelled"), _) => Some((
                        Notice::HOOK,
                        format!("{name} hook timed out; its output was dropped"),
                    )),
                    (_, Some(2)) => {
                        Some((Notice::HOOK, said(format!("{name} hook returned blocking error"))))
                    }
                    _ => Some((Notice::HOOK, said(format!("{name} hook failed")))),
                }
            }
            "stop_hook_summary" => {
                let label = text("hook_label").unwrap_or("Stop");
                if bool_at(message, "prevented_continuation") {
                    let why = text("stop_reason").unwrap_or("no reason given");
                    Some((Notice::HOOK, format!("{label} hook stopped the turn: {why}")))
                } else {
                    let errors = list("hook_errors");
                    (!errors.is_empty()).then(|| {
                        (Notice::HOOK, format!("{label} hook error: {}", errors.join("; ")))
                    })
                }
            }
            "plugin_install" => match text("status") {
                Some("installed") => {
                    text("name").map(|n| (Notice::INFO, format!("Installed plugin {n}")))
                }
                Some("failed") => {
                    let name = text("name").unwrap_or("A plugin");
                    let why = text("error").unwrap_or("no reason given");
                    Some((Notice::INFO, format!("{name} failed to install: {why}")))
                }
                _ => None,
            },
            // A message from another session held before it reached the model: informational,
            // as Claude Code takes no answer to it from a host.
            "peer_message_hold" => {
                let from = text("from_name").or_else(|| text("from")).unwrap_or("another session");
                let cause = text("cause").unwrap_or_default();
                let why = if cause.contains("mode") {
                    "its permission mode does not match this session's"
                } else if cause.contains("setting") || cause.contains("policy") {
                    "managed settings hold messages from other sessions"
                } else {
                    cause
                };
                let words = match text("outcome").or_else(|| text("state")) {
                    Some("delivered" | "released") => {
                        format!("Delivered the held message from {from}")
                    }
                    Some("dropped" | "denied" | "declined" | "expired") => {
                        format!("Dropped the held message from {from}")
                    }
                    _ if why.is_empty() => format!("Held a message from another session ({from})"),
                    _ => format!("Held a message from another session ({from}): {why}"),
                };
                Some((Notice::INFO, words))
            }
            "instruction_size_warning" => {
                let total = u64_at(message, "total_chars").unwrap_or_default();
                let limit = u64_at(message, "total_limit_chars").unwrap_or_default();
                Some((
                    Notice::INFO,
                    format!("The instruction files are long: {total} characters, over {limit}"),
                ))
            }
            _ => None,
        };
        row.map(|(kind, words)| self.history.notice(kind, &words, now)).unwrap_or_default()
    }

    /// The background agents just stopped at once (`agents_killed`), named as the TUI names them:
    /// those still live, which the stop ends, and those it ended a moment ago.
    fn agents_stopped(&self, now: WallMs) -> String {
        let just = |t: &BackgroundTask| {
            t.state == BackgroundTask::KILLED
                && t.ended_ms.is_some_and(|at| {
                    now.as_millis().saturating_sub(at.as_millis()) <= ms(STOPPED_TOGETHER)
                })
        };
        let named: Vec<String> = self
            .history
            .tasks_now()
            .iter()
            .filter(|t| t.kind == BackgroundTask::AGENT && !t.ambient)
            .filter(|t| t.is_live() || just(t))
            .map(|t| format!("\"{}\"", t.title.trim()))
            .collect();
        match named.as_slice() {
            [] => "All background agents stopped".to_owned(),
            [one] => format!("Background agent {one} was stopped"),
            many => format!("{} background agents were stopped: {}", many.len(), many.join(", ")),
        }
    }

    /// At a turn's end, what it leaves running, as the TUI's turn footer says it: the background
    /// agents and workflows it waits for ("Waiting for 2 background agents and 1 dynamic
    /// workflow to finish"), and the other background tasks still running ("3 background tasks
    /// still running"). The client puts it in place of, or after, the turn's own time.
    pub(super) fn waiting_on_agents(&mut self, now: WallMs) -> Vec<Out> {
        let live: Vec<&BackgroundTask> =
            self.history.tasks_now().iter().filter(|t| t.is_live() && !t.ambient).collect();
        let agents =
            live.iter().filter(|t| t.kind == BackgroundTask::AGENT && t.backgrounded).count();
        let workflows = live.iter().filter(|t| t.kind == BackgroundTask::WORKFLOW).count();
        let others = live
            .iter()
            .filter(|t| t.kind != BackgroundTask::AGENT && t.kind != BackgroundTask::WORKFLOW)
            .count();
        let waited: Vec<String> = [
            (agents > 0).then(|| counted(agents, "background agent", "background agents")),
            (workflows > 0).then(|| counted(workflows, "dynamic workflow", "dynamic workflows")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut parts = Vec::new();
        if !waited.is_empty() {
            parts.push(format!("Waiting for {} to finish", waited.join(" and ")));
        }
        if others > 0 {
            parts.push(format!(
                "{} still running",
                counted(others, "background task", "background tasks")
            ));
        }
        if parts.is_empty() {
            return Vec::new();
        }
        self.history.notice(Notice::TURN, &parts.join(" · "), now)
    }

    /// The usage limit stopped the work while a goal is set: the goal waits for the reset, as the
    /// TUI says.
    pub(super) fn goal_paused(&mut self, now: WallMs) -> Vec<Out> {
        if self.goal.is_none() {
            return Vec::new();
        }
        let text = "Goal paused · usage limit reached · continues automatically when it resets";
        self.history.notice(Notice::GOAL, text, now)
    }

    /// The subagents the session can start (`initialize`'s `agents`), for the person to name.
    pub(super) fn agents_listed(&mut self, agents: &[Value]) -> Vec<Out> {
        let kinds = agents
            .iter()
            .filter_map(|a| {
                Some(SubagentKind {
                    name: str_at(a, "name")?.to_owned(),
                    description: str_at(a, "description").unwrap_or_default().to_owned(),
                    model: str_at(a, "model").map(str::to_owned),
                })
            })
            .collect();
        self.history.tell(Action::AgentsSet(kinds))
    }

    /// The subagents' descriptions, which ride every request, past the TUI's limit: said once,
    /// as its status notice says it.
    pub(super) fn agents_too_long(&mut self, agents: &[Value], now: WallMs) -> Vec<Out> {
        let chars: usize = agents
            .iter()
            .filter_map(|a| str_at(a, "description"))
            .map(str::len)
            .fold(0, usize::saturating_add);
        // About four characters a token, as Claude Code estimates text it has not sent.
        let tokens = chars / 4;
        if tokens <= AGENT_DESCRIPTION_TOKENS || !self.said_once.0.insert("agents-long".to_owned())
        {
            return Vec::new();
        }
        let words = format!(
            "Agent descriptions are over the {}k-token limit (~{}k tokens) · ask Claude to trim \
             agent descriptions in .claude/agents/",
            AGENT_DESCRIPTION_TOKENS / 1_000,
            tokens / 1_000
        );
        self.passing("agents-long", &words, now)
    }

    /// When the background commands' output was last read, while one runs.
    pub(super) fn tails_due(&self) -> Option<WallMs> {
        let running = self.history.tasks_now().iter().any(|t| {
            t.is_live()
                && matches!(t.kind.as_str(), BackgroundTask::SHELL | BackgroundTask::MONITOR)
        });
        running.then(|| self.tailed.unwrap_or(WallMs::ZERO))
    }

    /// What the background commands printed since they were last read.
    pub(super) fn tails(&mut self, now: WallMs) -> Vec<Out> {
        self.tailed = Some(now);
        let outputs = self.outputs.read(&self.decoder);
        if outputs.is_empty() {
            return Vec::new();
        }
        self.history.transcript(&[], &outputs)
    }
}

/// Whether a hook's answer (`hook_response`'s JSON `stdout` or `output`) allowed a call: a
/// `PreToolUse` hook's `permissionDecision`, a `PermissionRequest` hook's `decision.behavior`.
fn hook_allowed(message: &Value) -> bool {
    ["stdout", "output"].iter().any(|key| {
        let Some(said) = message.get(*key).and_then(Value::as_str) else { return false };
        let Ok(said) = serde_json::from_str::<Value>(said.trim()) else { return false };
        let specific = said.get("hookSpecificOutput");
        let decided = specific
            .and_then(|s| s.get("permissionDecision"))
            .or_else(|| specific.and_then(|s| s.get("decision")).and_then(|d| d.get("behavior")));
        decided.and_then(Value::as_str) == Some("allow")
    })
}

/// The most characters of a call's argument a stall names.
const CALL_WORDS: usize = 60;

/// A tool call in a few words, as the TUI heads it: `Bash(cargo test)`, `Edit(src/main.rs)`, or
/// the tool's name alone.
fn call_words(name: &str, input: &Value) -> String {
    let argument = ["command", "file_path", "path", "pattern", "url", "description"]
        .iter()
        .find_map(|key| str_at(input, key))
        .map(|said| said.lines().next().unwrap_or_default().trim());
    match argument.filter(|said| !said.is_empty()) {
        Some(said) if said.chars().count() > CALL_WORDS => {
            let cut: String = said.chars().take(CALL_WORDS).collect();
            format!("{name}({cut}…)")
        }
        Some(said) => format!("{name}({said})"),
        None => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Times read as the TUI says them; each task type is the kind a client groups it by.
    #[test]
    fn times_and_kinds_read_as_the_tui_says_them() {
        assert_eq!(span(12_400), "12s");
        assert_eq!(span(303_000), "5m 3s");
        assert_eq!(span(6_710_000), "1h 51m 50s");
        assert_eq!(span(300_000), "5m", "a part of nothing is left out");
        assert_eq!(span(3_603_000), "1h 3s");
        assert_eq!(task_kind("local_bash", Some("Monitor")), BackgroundTask::MONITOR);
        assert_eq!(task_kind("local_bash", Some("Bash")), BackgroundTask::SHELL);
        assert_eq!(task_kind("local_agent", None), BackgroundTask::AGENT);
        assert_eq!(task_kind("in_process_teammate", None), BackgroundTask::TEAMMATE);
        assert_eq!(task_kind("dream", None), "dream", "an unknown type keeps its own word");
        assert_eq!(counted(1, "memory", "memories"), "1 memory");
    }
}
