//! Claude Code driven as an SDK host: its stream-json protocol over stdio.
//!
//! `claude -p --verbose --input-format stream-json --output-format stream-json
//! --permission-prompts host --permission-prompt-tool stdio --include-partial-messages
//! --replay-user-messages` writes one JSON record per line to stdout and reads the host's
//! records from stdin, for as many turns as the host cares to send (DECISIONS "Claude Code",
//! structured driving). This module is the pure half of that: [`parse`] turns a stdout line
//! into an [`Event`], [`Fold`] turns events into what a client is shown (conversation
//! entries, status, a permission to answer), and [`user_message`], [`Fold::answer`] and
//! [`interrupt`] build the lines the host writes back. No process, no I/O.
//!
//! The `assistant` and `user` records on the stream have the same shape as the JSONL
//! transcript file, so [`crate::transcript`] maps them. Two differences matter: streamed
//! assistant records carry no `stop_reason` (the `result` record is what ends a turn), and a
//! record may carry no timestamp (the fold stamps those with the clock it is given).

use std::collections::HashMap;

use serde_json::{Value, json};
use slopty_proto::agent::{
    AgentStatus, AgentTask, BlockReason, Context, PermissionRequest, QuestionAnswer, ToolDetail,
    TranscriptBody, TranscriptEntry, Usage, UsageWindow,
};

use crate::transcript::{self, ToolNames};

/// What `system/init` says about the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Init {
    /// Claude Code's session id, the one `--resume` takes.
    pub session_id: String,
    /// The model in use.
    pub model: String,
    /// The permission mode in force (`default`, `acceptEdits`, `auto`, …).
    pub permission_mode: String,
    /// Tools available to the agent.
    pub tools: Vec<String>,
    /// Slash commands the agent accepts as messages (`/compact`, `/clear`, …).
    pub slash_commands: Vec<String>,
}

/// What a `result` record says about the turn that just ended.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnResult {
    /// The turn completed (`subtype: success`, not `is_error`).
    pub ok: bool,
    /// The record's `subtype` (`success`, `error_during_execution`, …).
    pub kind: String,
    /// Turns in the conversation so far.
    pub turns: u32,
    /// Claude Code's own cost estimate for the conversation.
    pub cost_usd: f64,
    /// The final text of the turn, when there was one.
    pub text: Option<String>,
    /// Claude Code's session id.
    pub session_id: String,
    /// The widest context window among the models the turn used (`modelUsage.*.contextWindow`).
    pub context_window: Option<u64>,
}

/// The kind of content block a streamed message opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Extended thinking: nothing to show until the text starts.
    Thinking,
    /// The answer's text: the deltas that follow fill the partial.
    Text,
    /// A tool call being composed, by name.
    Tool(String),
}

impl Block {
    /// The Working detail while this block streams; `None` for text (the partial says it).
    #[must_use]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Thinking => Some("thinking…".to_owned()),
            Self::Text => None,
            Self::Tool(name) => Some(format!("calling {name}…")),
        }
    }
}

/// One line Claude Code wrote to stdout, decoded as far as Slopty needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// `system/init`: the session is up.
    Init(Init),
    /// A conversation record (`assistant` or `user`), the transcript file's shape.
    Record(Value),
    /// A text delta of the message being streamed (`stream_event`, `text_delta`).
    TextDelta(String),
    /// A content block of the streamed message opened (`stream_event`, `content_block_start`):
    /// what the agent is doing before any record says.
    BlockStart(Block),
    /// The streamed message ended; the `Record` that follows carries its final form.
    MessageStop,
    /// A tool waits on the human. The raw input the host echoes back on allow rides here.
    Permission {
        /// What the client is shown.
        request: PermissionRequest,
        /// What the allow reply echoes, boxed to keep the event small.
        pending: Box<Pending>,
    },
    /// A subagent started or progressed (`system/task_started`, `task_progress`).
    Task(AgentTask),
    /// Claude Code answered a control request the host sent (an interrupt).
    ControlAck {
        /// The host's request id.
        request_id: String,
        /// `None` on success, the error text otherwise.
        error: Option<String>,
    },
    /// The turn ended.
    Result(TurnResult),
    /// A tool was refused by policy without asking (`system/permission_denied`).
    Denied {
        /// Tool name.
        tool: String,
        /// Why.
        reason: String,
    },
    /// `system/status`: the permission mode changed (the answer to `set_permission_mode`
    /// arrives this way too).
    Status {
        /// The mode now in force, when the record names one.
        permission_mode: Option<String>,
        /// What the agent is doing, when the record says (`requesting`, `compacting`).
        doing: Option<String>,
    },
    /// `rate_limit_event`: the subscription's usage windows.
    RateLimit(Usage),
    /// Anything else: hook bookkeeping, thinking token counts.
    Other,
}

/// Decode one stdout line; `None` when it is not JSON.
#[must_use]
pub fn parse(line: &str) -> Option<Event> {
    let record: Value = serde_json::from_str(line.trim()).ok()?;
    let kind = record.get("type").and_then(Value::as_str)?;
    let subtype = record.get("subtype").and_then(Value::as_str);
    let event = match (kind, subtype) {
        ("system", Some("init")) => Event::Init(Init {
            session_id: string(&record, "session_id"),
            model: string(&record, "model"),
            permission_mode: string(&record, "permissionMode"),
            tools: strings(&record, "tools"),
            slash_commands: strings(&record, "slash_commands"),
        }),
        ("system", Some("status")) => Event::Status {
            permission_mode: record
                .get("permissionMode")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned),
            doing: record
                .get("status")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned),
        },
        ("rate_limit_event", _) => record.get("rate_limit_info").map_or(Event::Other, |info| {
            let window = |name: &str| {
                let w = info.get("unifiedWindows")?.get(name)?;
                let utilization = w.get("utilization").and_then(Value::as_f64)?;
                let spent = (utilization * 100.0).round().clamp(0.0, 100.0);
                // The first whole percent at or above the fraction; no float cast needed.
                let percent = (0..=100_u8).find(|&n| f64::from(n) >= spent).unwrap_or(100);
                Some(UsageWindow {
                    percent,
                    resets_at: w.get("resetsAt").and_then(Value::as_u64).unwrap_or(0),
                })
            };
            Event::RateLimit(Usage {
                limited: info.get("status").and_then(Value::as_str).is_some_and(|s| s != "allowed"),
                five_hour: window("five_hour"),
                seven_day: window("seven_day"),
            })
        }),
        ("system", Some("task_started" | "task_progress")) => Event::Task(AgentTask {
            call: string(&record, "tool_use_id"),
            description: string(&record, "description"),
            kind: record.get("subagent_type").and_then(Value::as_str).map(str::to_owned),
            tool_uses: record
                .get("usage")
                .and_then(|u| u.get("tool_uses"))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            duration_ms: record
                .get("usage")
                .and_then(|u| u.get("duration_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
            last_tool: record.get("last_tool_name").and_then(Value::as_str).map(str::to_owned),
            done: false,
        }),
        ("system", Some("permission_denied")) => Event::Denied {
            tool: string(&record, "tool_name"),
            reason: record
                .get("message")
                .or_else(|| record.get("decision_reason"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        ("assistant" | "user", _) | ("system", Some("compact_boundary")) => Event::Record(record),
        ("stream_event", _) => stream_event(record.get("event")),
        ("control_request", _) => control_request(&record).unwrap_or(Event::Other),
        ("control_response", _) => {
            let response = record.get("response").unwrap_or(&Value::Null);
            Event::ControlAck {
                request_id: string(response, "request_id"),
                error: (response.get("subtype").and_then(Value::as_str) != Some("success")).then(
                    || {
                        response
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("control request failed")
                            .to_owned()
                    },
                ),
            }
        }
        ("result", _) => {
            let subtype = subtype.unwrap_or_default().to_owned();
            let is_error = record.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            Event::Result(TurnResult {
                ok: subtype == "success" && !is_error,
                kind: subtype,
                turns: record
                    .get("num_turns")
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                cost_usd: record.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
                text: record
                    .get("result")
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
                    .map(str::to_owned),
                session_id: string(&record, "session_id"),
                context_window: record.get("modelUsage").and_then(Value::as_object).and_then(
                    |models| {
                        models
                            .values()
                            .filter_map(|m| m.get("contextWindow").and_then(Value::as_u64))
                            .max()
                    },
                ),
            })
        }
        _ => Event::Other,
    };
    Some(event)
}

/// What the request behind an assistant record carried: its `usage` input tokens, cache
/// reads and cache writes together, which is the context the model saw.
fn context_tokens(message: &Value) -> Option<u64> {
    let usage = message.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let tokens = count("input_tokens")
        .saturating_add(count("cache_creation_input_tokens"))
        .saturating_add(count("cache_read_input_tokens"));
    (tokens > 0).then_some(tokens)
}

fn string(record: &Value, key: &str) -> String {
    record.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn strings(record: &Value, key: &str) -> Vec<String> {
    record
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The API's own streaming events: only text deltas and the end of a message matter; tool
/// input deltas and thinking deltas wait for the record.
fn stream_event(event: Option<&Value>) -> Event {
    let Some(event) = event else { return Event::Other };
    match event.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => event
            .get("delta")
            .filter(|d| d.get("type").and_then(Value::as_str) == Some("text_delta"))
            .and_then(|d| d.get("text").and_then(Value::as_str))
            .map_or(Event::Other, |text| Event::TextDelta(text.to_owned())),
        Some("content_block_start") => event
            .get("content_block")
            .and_then(|block| match block.get("type").and_then(Value::as_str)? {
                "thinking" => Some(Block::Thinking),
                "text" => Some(Block::Text),
                "tool_use" => Some(Block::Tool(string(block, "name"))),
                _ => None,
            })
            .map_or(Event::Other, Event::BlockStart),
        Some("message_stop") => Event::MessageStop,
        _ => Event::Other,
    }
}

/// `can_use_tool` is the one control request Claude Code sends the host.
fn control_request(record: &Value) -> Option<Event> {
    let request = record.get("request")?;
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return None;
    }
    let tool = string(request, "tool_name");
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let summary = {
        let line = transcript::tool_summary(&tool, Some(&input));
        if line.is_empty() { string(request, "description") } else { line }
    };
    let detail = transcript::tool_detail(&tool, Some(&input));
    let suggestions = request.get("permission_suggestions").cloned().unwrap_or(Value::Null);
    Some(Event::Permission {
        request: PermissionRequest {
            id: string(record, "request_id"),
            tool_use: string(request, "tool_use_id"),
            tool,
            summary,
            detail,
            always: always_label(&suggestions),
        },
        pending: Box::new(Pending { input, suggestions }),
    })
}

/// What a `can_use_tool` is answered with: the input echoed on an allow, and the agent's
/// `permission_suggestions` echoed as `updatedPermissions` on an "always" allow.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Pending {
    /// The tool input, as sent.
    pub input: Value,
    /// The suggestions, as sent; `Null` when the agent made none.
    pub suggestions: Value,
}

/// What taking the agent's `permission_suggestions` would do, in the human's words.
///
/// `None` when there are none. Probed on CLI 2.1.269: a `Write` suggests
/// `[{type: "setMode", mode: "acceptEdits", destination: "session"}]`; a rule suggestion is
/// `{type: "addRules", rules: [{toolName, ruleContent}], behavior, destination}`.
#[must_use]
pub fn always_label(suggestions: &Value) -> Option<String> {
    let list = suggestions.as_array().filter(|l| !l.is_empty())?;
    let where_ = |s: &Value| match s.get("destination").and_then(Value::as_str) {
        Some("session") => " for this session",
        Some("localSettings" | "projectSettings") => " in this project",
        Some("userSettings") => " for you",
        _ => "",
    };
    let parts: Vec<String> = list
        .iter()
        .filter_map(|s| match s.get("type").and_then(Value::as_str)? {
            "setMode" => {
                let mode = match s.get("mode").and_then(Value::as_str)? {
                    "acceptEdits" => "accept edits",
                    "plan" => "plan",
                    "bypassPermissions" => "bypass permissions",
                    "default" => "ask",
                    other => other,
                };
                Some(format!("{mode}{}", where_(s)))
            }
            "addRules" => {
                let rules: Vec<String> = s
                    .get("rules")?
                    .as_array()?
                    .iter()
                    .filter_map(|r| {
                        let tool = r.get("toolName")?.as_str()?;
                        Some(match r.get("ruleContent").and_then(Value::as_str) {
                            Some(content) => format!("{tool}({content})"),
                            None => tool.to_owned(),
                        })
                    })
                    .collect();
                let what = if rules.is_empty() { "this".to_owned() } else { rules.join(", ") };
                let verb = if s.get("behavior").and_then(Value::as_str) == Some("deny") {
                    "always deny"
                } else {
                    "always allow"
                };
                Some(format!("{verb} {what}{}", where_(s)))
            }
            _ => None,
        })
        .collect();
    if parts.is_empty() { Some("remember this".to_owned()) } else { Some(parts.join("; ")) }
}

/// The arguments that put Claude Code into the protocol this module speaks. `resume` reopens
/// a conversation by session id; `model` overrides the default model.
#[must_use]
pub fn arguments(resume: Option<&str>, model: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--permission-prompts",
        "host",
        "--permission-prompt-tool",
        "stdio",
        "--include-partial-messages",
        "--replay-user-messages",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(id) = resume {
        args.push("--resume".to_owned());
        args.push(id.to_owned());
    }
    if let Some(model) = model {
        args.push("--model".to_owned());
        args.push(model.to_owned());
    }
    args
}

/// `words` as one line for `$SHELL -lic`.
///
/// Each word is single-quoted where a POSIX or fish shell would otherwise read it. The
/// interactive login shell is what finds a `claude` that is an alias or lives in
/// `~/.claude/local`, as it does for the "+ agent" terminal.
#[must_use]
pub fn shell_line<'a>(words: impl IntoIterator<Item = &'a str>) -> String {
    words.into_iter().map(shell_quote).collect::<Vec<_>>().join(" ")
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&b))
    {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The line that sends the human's words as the next prompt.
#[must_use]
pub fn user_message(text: &str, images: &[slopty_proto::agent::Image]) -> String {
    if images.is_empty() {
        return json!({"type": "user", "message": {"role": "user", "content": text}}).to_string();
    }
    // With pictures the content is blocks: each picture as the API takes it (base64 with its
    // media type; probed on CLI 2.1.269, the model read a pasted PNG), then the text.
    let mut blocks: Vec<Value> = images
        .iter()
        .map(|image| {
            json!({"type": "image", "source": {"type": "base64", "media_type": image.media_type,
                   "data": data_encoding::BASE64.encode(&image.data)}})
        })
        .collect();
    if !text.trim().is_empty() {
        blocks.push(json!({"type": "text", "text": text}));
    }
    json!({"type": "user", "message": {"role": "user", "content": blocks}}).to_string()
}

/// The line that asks Claude Code to stop the running turn. `request_id` is the host's own;
/// the [`Event::ControlAck`] that follows carries it back.
#[must_use]
pub fn interrupt(request_id: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
    .to_string()
}

/// The line that switches the agent's model in place (`set_model`); `model` is an alias
/// (`fable`, `opus`, `sonnet`, `haiku`) or a full name.
#[must_use]
pub fn set_model(request_id: &str, model: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "set_model", "model": model},
    })
    .to_string()
}

/// The line that switches the agent's permission mode in place (`set_permission_mode`);
/// the agent confirms with a `system/status` record naming the mode.
#[must_use]
pub fn set_permission_mode(request_id: &str, mode: &str) -> String {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "set_permission_mode", "mode": mode},
    })
    .to_string()
}

/// The allow; with `permissions` (the agent's own suggestions, echoed) it also stops the
/// agent asking again, the way the TUI's "always allow" does.
fn allow_line(request_id: &str, input: &Value, permissions: Option<&Value>) -> String {
    let mut response = json!({"behavior": "allow", "updatedInput": input});
    if let (Some(permissions), Value::Object(map)) = (permissions, &mut response) {
        map.insert("updatedPermissions".to_owned(), permissions.clone());
    }
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response,
        },
    })
    .to_string()
}

fn deny_line(request_id: &str, message: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {"behavior": "deny", "message": message},
        },
    })
    .to_string()
}

/// What the client denies with when it gives no reason of its own; the agent sees it as the
/// tool's error text.
pub const DENIED: &str = "The user denied this tool call.";

/// What a client is shown after one event.
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// Conversation entries to append.
    Entries(Vec<TranscriptEntry>),
    /// The text of the message being streamed, so far; empty once the message lands as an
    /// entry.
    Partial(String),
    /// The agent's status changed.
    Status {
        /// The status.
        status: AgentStatus,
        /// One line about it.
        detail: Option<String>,
    },
    /// A tool waits on the human; answer with [`Fold::answer`].
    Permission(PermissionRequest),
    /// A subagent started, progressed or finished; the client shows it under its call.
    Task(AgentTask),
    /// The subscription's usage windows changed.
    Usage(Usage),
    /// The context window's fill changed (an assistant record's `usage`, or a turn result
    /// naming the window).
    Context(Context),
    /// A turn ended.
    Turn(TurnResult),
    /// The session is up and said what it is.
    Init(Init),
    /// The model in use changed (an assistant record named another one).
    Model(String),
    /// The permission mode changed.
    PermissionMode(String),
}

/// Folds the event stream of one conversation into [`Update`]s.
///
/// Keeps the tool names of the calls seen (so results are named), the inputs of the
/// permissions still open (so an allow echoes the input back), the text of the message being
/// streamed, and the last status (so nothing is repeated).
#[derive(Debug, Default)]
pub struct Fold {
    tools: ToolNames,
    pending: HashMap<String, Pending>,
    /// The subagents seen, by the call that spawned them, for marking them done.
    tasks: HashMap<String, AgentTask>,
    partial: String,
    status: Option<(AgentStatus, Option<String>)>,
    init: Option<Init>,
    model: Option<String>,
    context: Option<Context>,
}

impl Fold {
    /// What `system/init` said, once it has.
    #[must_use]
    pub const fn init(&self) -> Option<&Init> {
        self.init.as_ref()
    }

    /// The permissions waiting on the human, oldest first is not guaranteed.
    #[must_use]
    pub fn pending(&self) -> Vec<&str> {
        self.pending.keys().map(String::as_str).collect()
    }

    /// Apply one event. `now` (milliseconds since the Unix epoch) stamps the entries whose
    /// record carries no timestamp.
    pub fn apply(&mut self, event: Event, now: u64) -> Vec<Update> {
        match event {
            Event::Init(init) => {
                self.model = Some(init.model.clone()).filter(|m| !m.is_empty());
                self.init = Some(init.clone());
                let mut out = vec![Update::Init(init)];
                out.extend(self.status(AgentStatus::Idle, None));
                out
            }
            Event::Status { permission_mode, doing } => {
                let mut out: Vec<Update> =
                    permission_mode.map(Update::PermissionMode).into_iter().collect();
                // "requesting" is the agent waiting on the model, "compacting" its own
                // housekeeping: both are the turn alive with nothing yet to show.
                let detail = match doing.as_deref() {
                    Some("requesting") => Some("waiting for the model…"),
                    Some("compacting") => Some("compacting the conversation…"),
                    _ => None,
                };
                if let Some(detail) = detail
                    && matches!(self.status.as_ref(), Some((AgentStatus::Working, _)) | None)
                {
                    out.extend(self.status(AgentStatus::Working, Some(detail.to_owned())));
                }
                out
            }
            Event::RateLimit(usage) => vec![Update::Usage(usage)],
            Event::Record(record) => self.record(&record, now),
            Event::TextDelta(text) => {
                self.partial.push_str(&text);
                vec![Update::Partial(self.partial.clone())]
            }
            // A block opening names what the agent is doing between records; like a status
            // record it never overwrites a permission, a question or a finished turn.
            Event::BlockStart(block) => {
                if matches!(self.status.as_ref(), Some((AgentStatus::Working, _)) | None) {
                    self.status(AgentStatus::Working, block.detail()).into_iter().collect()
                } else {
                    Vec::new()
                }
            }
            Event::Task(task) => {
                let mut task = task;
                if let Some(seen) = self.tasks.get(&task.call) {
                    // A start after progress, or progress without counts, keeps what is known.
                    task.tool_uses = task.tool_uses.max(seen.tool_uses);
                    task.duration_ms = task.duration_ms.max(seen.duration_ms);
                    if task.last_tool.is_none() {
                        task.last_tool.clone_from(&seen.last_tool);
                    }
                    if task.kind.is_none() {
                        task.kind.clone_from(&seen.kind);
                    }
                    task.done = seen.done;
                }
                self.tasks.insert(task.call.clone(), task.clone());
                vec![Update::Task(task)]
            }
            Event::Permission { request, pending } => {
                self.pending.insert(request.id.clone(), *pending);
                // A question is a `can_use_tool` on the wire but not a permission to the
                // human: it blocks as a question, the way the hook path reports one.
                let reason = if matches!(request.detail, ToolDetail::Question { .. }) {
                    BlockReason::Question
                } else {
                    BlockReason::Permission { tool: request.tool.clone() }
                };
                let mut out = self
                    .status(AgentStatus::Blocked(reason), Some(crate::truncate(&request.summary)))
                    .into_iter()
                    .collect::<Vec<_>>();
                out.push(Update::Permission(request));
                out
            }
            Event::Result(result) => {
                let detail = result.text.as_deref().and_then(transcript::last_line).map_or_else(
                    || (!result.ok).then(|| crate::truncate(&result.kind.replace('_', " "))),
                    |line| Some(crate::truncate(&line)),
                );
                let mut out = Vec::new();
                if !self.partial.is_empty() {
                    self.partial.clear();
                    out.push(Update::Partial(String::new()));
                }
                out.extend(self.status(AgentStatus::Done, detail));
                if let Some(window) = result.context_window
                    && let Some(context) = self.context
                    && context.window != Some(window)
                {
                    let context = Context { window: Some(window), ..context };
                    self.context = Some(context);
                    out.push(Update::Context(context));
                }
                out.push(Update::Turn(result));
                out
            }
            Event::MessageStop | Event::ControlAck { .. } | Event::Denied { .. } | Event::Other => {
                Vec::new()
            }
        }
    }

    /// Answer an open permission: the line to write, or `None` when no such request is open
    /// (already answered, or never seen). `message` is what a denial tells the agent;
    /// [`DENIED`] when the client gave none.
    pub fn answer(
        &mut self,
        request_id: &str,
        allowed: bool,
        message: Option<&str>,
        answers: &[QuestionAnswer],
        always: bool,
    ) -> Option<String> {
        let Pending { mut input, suggestions } = self.pending.remove(request_id)?;
        self.status = Some((AgentStatus::Working, None));
        if allowed && !answers.is_empty() {
            // `AskUserQuestion` is allowed with the answers filed into its input, keyed by
            // the question text; Claude Code turns them into the tool's result.
            let filed: serde_json::Map<String, Value> = answers
                .iter()
                .map(|a| (a.question.clone(), Value::String(a.answer.clone())))
                .collect();
            if let Value::Object(map) = &mut input {
                map.insert("answers".to_owned(), Value::Object(filed));
            }
        }
        let permissions = (allowed && always && !suggestions.is_null()).then_some(suggestions);
        Some(if allowed {
            allow_line(request_id, &input, permissions.as_ref())
        } else {
            deny_line(request_id, message.unwrap_or(DENIED))
        })
    }

    fn record(&mut self, record: &Value, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        if transcript::is_subagent(record) {
            // A subagent talking to itself: not the conversation, not the model, not the
            // status. Its progress arrives as task records.
            return out;
        }
        // A result for a call that spawned a subagent ends that subagent.
        if let Some(blocks) =
            record.get("message").and_then(|m| m.get("content")).and_then(Value::as_array)
        {
            for id in blocks.iter().filter_map(|b| {
                (b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .then(|| b.get("tool_use_id").and_then(Value::as_str))
                    .flatten()
            }) {
                if let Some(task) = self.tasks.get_mut(id)
                    && !task.done
                {
                    task.done = true;
                    out.push(Update::Task(task.clone()));
                }
            }
        }
        let is_assistant = record.get("type").and_then(Value::as_str) == Some("assistant");
        if is_assistant && !self.partial.is_empty() {
            self.partial.clear();
            out.push(Update::Partial(String::new()));
        }
        if is_assistant
            && let Some(model) =
                record.get("message").and_then(|m| m.get("model")).and_then(Value::as_str)
            && !model.is_empty()
            && self.model.as_deref() != Some(model)
        {
            self.model = Some(model.to_owned());
            out.push(Update::Model(model.to_owned()));
        }
        if is_assistant
            && let Some(tokens) = record.get("message").and_then(context_tokens)
            && self.context.is_none_or(|c| c.tokens != tokens)
        {
            let context = Context { tokens, window: self.context.and_then(|c| c.window) };
            self.context = Some(context);
            out.push(Update::Context(context));
        }
        let entries: Vec<TranscriptEntry> = transcript::record_entries(&mut self.tools, record)
            .into_iter()
            .map(|entry| TranscriptEntry { at: entry.at.or(Some(now)), body: entry.body })
            .collect();
        // A compaction says what the context shrank to; the chip need not wait for the next
        // assistant record.
        if let Some(TranscriptBody::Compacted { post_tokens: Some(tokens), .. }) =
            entries.first().map(|e| &e.body)
            && self.context.is_some_and(|c| c.tokens != *tokens)
        {
            let context = Context { tokens: *tokens, window: self.context.and_then(|c| c.window) };
            self.context = Some(context);
            out.push(Update::Context(context));
        }
        if !entries.is_empty() {
            out.push(Update::Entries(entries));
        }
        if let Some(progress) = transcript::record_progress(record) {
            // A streamed assistant record never says `end_turn`; the result does.
            let status = match progress.status {
                AgentStatus::Done => AgentStatus::Working,
                other => other,
            };
            out.extend(self.status(status, progress.detail));
        }
        out
    }

    fn status(&mut self, status: AgentStatus, detail: Option<String>) -> Option<Update> {
        let next = (status, detail);
        if self.status.as_ref() == Some(&next) {
            return None;
        }
        self.status = Some(next.clone());
        Some(Update::Status { status: next.0, detail: next.1 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_TURN: &str = include_str!("../tests/fixtures/stream_one_turn.jsonl");

    const CAN_USE_TOOL: &str = r#"{"type":"control_request","request_id":"e2f45975-aa92-4c0c-ad9b-05cd456d9b00","request":{"subtype":"can_use_tool","tool_name":"Write","display_name":"Write","input":{"file_path":"/private/tmp/sj-probe/probe4.txt","content":"hi"},"description":"probe4.txt","permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"tool_use_id":"toolu_01F6WsndtGMAv3yQPYeTTxMc"}}"#;

    fn events(jsonl: &str) -> Vec<Event> {
        jsonl.lines().filter_map(parse).collect()
    }

    #[test]
    fn a_one_turn_run_parses_into_init_records_and_a_result() {
        let kinds: Vec<&str> = events(ONE_TURN)
            .iter()
            .map(|e| match e {
                Event::Init(_) => "init",
                Event::Record(_) => "record",
                Event::Result(_) => "result",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["init", "record", "record", "record", "result"]);
        let Some(Event::Init(init)) = events(ONE_TURN).into_iter().next() else {
            panic!("init first")
        };
        assert_eq!(init.model, "claude-haiku-4-5-20251001");
        assert_eq!(init.permission_mode, "default");
        assert!(init.tools.iter().any(|t| t == "Bash"));
        assert!(init.slash_commands.iter().any(|c| c == "/compact"));
        assert_eq!(init.session_id.len(), 36, "a uuid");
    }

    #[test]
    fn the_fold_shows_the_prompt_the_thinking_the_answer_and_the_statuses() {
        let mut fold = Fold::default();
        let mut entries = Vec::new();
        let mut statuses = Vec::new();
        let mut turns = Vec::new();
        let mut inits = 0;
        for event in events(ONE_TURN) {
            for update in fold.apply(event, 1_000) {
                match update {
                    Update::Entries(e) => entries.extend(e),
                    Update::Status { status, detail } => statuses.push((status, detail)),
                    Update::Turn(t) => turns.push(t),
                    Update::Init(init) => {
                        assert_eq!(init.model, "claude-haiku-4-5-20251001");
                        assert!(init.slash_commands.iter().any(|c| c == "/compact"));
                        inits += 1;
                    }
                    Update::Partial(_)
                    | Update::Permission(_)
                    | Update::Task(_)
                    | Update::Usage(_)
                    | Update::Context(_)
                    | Update::Model(_)
                    | Update::PermissionMode(_) => {}
                }
            }
        }
        assert_eq!(inits, 1, "init is reported once, before the first status");
        let bodies: Vec<&str> = entries
            .iter()
            .map(|e| match &e.body {
                TranscriptBody::User { .. } => "user",
                TranscriptBody::Thinking { .. } => "thinking",
                TranscriptBody::Assistant { .. } => "assistant",
                _ => "other",
            })
            .collect();
        assert_eq!(bodies, ["user", "thinking", "assistant"]);
        assert!(
            entries.iter().all(|e| e.at.is_some_and(|at| at > 1_000)),
            "records that carry a timestamp keep it: {entries:?}"
        );
        assert_eq!(
            statuses,
            [
                (AgentStatus::Idle, None),
                (AgentStatus::Working, Some("Reply with the single word pong.".to_owned())),
                (AgentStatus::Working, Some("pong".to_owned())),
                (AgentStatus::Done, Some("pong".to_owned())),
            ]
        );
        assert_eq!(turns.len(), 1);
        assert!(turns[0].ok);
        assert_eq!(turns[0].turns, 1);
        assert!(turns[0].cost_usd > 0.0);
        assert_eq!(fold.init().map(|i| i.model.as_str()), Some("claude-haiku-4-5-20251001"));
    }

    /// Probed on CLI 2.1.269: `AskUserQuestion` arrives as a `can_use_tool` (with
    /// `requires_user_interaction`), and the answer is an allow whose `updatedInput` files
    /// the answers under the question text.
    #[test]
    fn a_question_blocks_as_a_question_and_the_answers_are_filed_into_the_input() {
        let mut fold = Fold::default();
        let line = r#"{"type":"control_request","request_id":"07dd1978-3d77-4b8d-8f39-d1ed20e653ba","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","display_name":"AskUserQuestion","input":{"questions":[{"question":"Which colour do you prefer?","header":"Colour","options":[{"label":"Red","description":"The colour red"},{"label":"Blue","description":"The colour blue"}],"multiSelect":false}]},"tool_use_id":"toolu_01UMP2qYNcjojzv7BrqSiRzT","requires_user_interaction":true}}"#;
        let Some(event) = parse(line) else { panic!("parses") };
        let updates = fold.apply(event, 0);
        let Some(Update::Status { status, detail }) = updates.first() else {
            panic!("blocked first: {updates:?}")
        };
        assert_eq!(*status, AgentStatus::Blocked(BlockReason::Question));
        assert_eq!(detail.as_deref(), Some("Which colour do you prefer?"));
        let Some(Update::Permission(request)) = updates.get(1) else { panic!("then the request") };
        let ToolDetail::Question { questions } = &request.detail else {
            panic!("a question: {request:?}")
        };
        assert_eq!(questions.len(), 1);
        assert_eq!(
            questions[0].options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            ["Red", "Blue"]
        );
        let answers = [QuestionAnswer {
            question: "Which colour do you prefer?".to_owned(),
            answer: "Blue".to_owned(),
        }];
        let line = fold.answer(&request.id, true, None, &answers, false).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            sent["response"]["response"]["updatedInput"]["answers"],
            json!({"Which colour do you prefer?": "Blue"})
        );
        assert_eq!(sent["response"]["response"]["behavior"], "allow");
        assert!(sent["response"]["response"]["updatedInput"]["questions"].is_array());
    }

    /// Probed on CLI 2.1.269: a `Write` suggests `setMode acceptEdits` for the session, an
    /// allow with `updatedPermissions` echoing it stops the next `Write` from asking, and a
    /// `system/status` naming the mode follows.
    #[test]
    fn an_always_allow_echoes_the_agents_suggestion_and_a_plain_one_does_not() {
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let updates = fold.apply(event, 0);
        let Some(Update::Permission(request)) = updates.get(1) else { panic!("the request") };
        assert_eq!(request.always.as_deref(), Some("accept edits for this session"));
        let line = fold.answer(&request.id, true, None, &[], true).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            sent["response"]["response"]["updatedPermissions"],
            json!([{"type":"setMode","mode":"acceptEdits","destination":"session"}])
        );
        assert_eq!(sent["response"]["response"]["behavior"], "allow");

        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let updates = fold.apply(event, 0);
        let Some(Update::Permission(request)) = updates.get(1) else { panic!("the request") };
        let line = fold.answer(&request.id, true, None, &[], false).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert!(sent["response"]["response"].get("updatedPermissions").is_none(), "{sent}");

        // A request without suggestions has no "always", and asking for it changes nothing.
        let bare = CAN_USE_TOOL.replace(
            r#""permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"#,
            "",
        );
        let mut fold = Fold::default();
        let Some(event) = parse(&bare) else { panic!("parses") };
        let updates = fold.apply(event, 0);
        let Some(Update::Permission(request)) = updates.get(1) else { panic!("the request") };
        assert_eq!(request.always, None);
        let line = fold.answer(&request.id, true, None, &[], true).expect("open");
        assert!(!line.contains("updatedPermissions"), "{line}");

        // Rules read as what they allow, and where.
        let rules = json!([{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"cargo test:*"}],"behavior":"allow","destination":"localSettings"}]);
        assert_eq!(
            always_label(&rules).as_deref(),
            Some("always allow Bash(cargo test:*) in this project")
        );
        assert_eq!(always_label(&json!([])), None);
        assert_eq!(always_label(&json!([{"type":"mystery"}])).as_deref(), Some("remember this"));
    }

    /// Probed on CLI 2.1.269: a subagent's own records carry `parent_tool_use_id`, and
    /// `system/task_started` / `task_progress` follow it; the call's result ends it.
    #[test]
    fn a_subagent_is_followed_by_its_task_records_and_its_own_are_hidden() {
        let mut fold = Fold::default();
        let lines = [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_p","name":"Agent","input":{"description":"List files","prompt":"ls","subagent_type":"Explore"}}]},"parent_tool_use_id":null}"#,
            r#"{"type":"system","subtype":"task_started","task_id":"t","tool_use_id":"toolu_p","description":"List files","subagent_type":"Explore","is_backgrounded":true,"spawn_depth":1,"task_type":"local_agent","prompt":"ls"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_c","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":"toolu_p"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_c","content":"a.txt"}]},"parent_tool_use_id":"toolu_p"}"#,
            r#"{"type":"system","subtype":"task_progress","task_id":"t","tool_use_id":"toolu_p","description":"Running List files","subagent_type":"Explore","usage":{"total_tokens":11283,"tool_uses":1,"duration_ms":2525},"last_tool_name":"Bash"}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_p","content":"a.txt is there"}]},"parent_tool_use_id":null}"#,
        ];
        let mut updates = Vec::new();
        for line in lines {
            let Some(event) = parse(line) else { panic!("parses: {line}") };
            updates.extend(fold.apply(event, 0));
        }
        let tasks: Vec<&AgentTask> = updates
            .iter()
            .filter_map(|u| if let Update::Task(t) = u { Some(t) } else { None })
            .collect();
        assert_eq!(tasks.len(), 3, "{updates:?}");
        assert_eq!(
            (tasks[0].tool_uses, tasks[0].done, tasks[0].kind.as_deref()),
            (0, false, Some("Explore"))
        );
        assert_eq!(
            (
                tasks[1].tool_uses,
                tasks[1].duration_ms,
                tasks[1].last_tool.as_deref(),
                tasks[1].done
            ),
            (1, 2525, Some("Bash"), false)
        );
        assert_eq!(tasks[1].description, "Running List files");
        assert!(tasks[2].done && tasks[2].tool_uses == 1, "{:?}", tasks[2]);
        let entries: Vec<&TranscriptEntry> = updates
            .iter()
            .filter_map(|u| if let Update::Entries(e) = u { Some(e) } else { None })
            .flatten()
            .collect();
        let names: Vec<String> = entries
            .iter()
            .map(|e| match &e.body {
                TranscriptBody::ToolUse { name, call, .. } => {
                    format!("{name}:{call}")
                }
                TranscriptBody::ToolResult { tool, .. } => {
                    format!("result:{}", tool.as_deref().unwrap_or("?"))
                }
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["Agent:toolu_p", "result:Agent"], "the subagent's Bash is not shown");
    }

    #[test]
    fn a_permission_blocks_the_agent_and_the_answer_echoes_the_input() {
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let updates = fold.apply(event, 0);
        let Some(Update::Status { status, detail }) = updates.first() else {
            panic!("blocked first: {updates:?}")
        };
        assert_eq!(*status, AgentStatus::Blocked(BlockReason::Permission { tool: "Write".into() }));
        assert_eq!(detail.as_deref(), Some("/private/tmp/sj-probe/probe4.txt"));
        let Some(Update::Permission(request)) = updates.get(1) else { panic!("then the request") };
        assert_eq!(request.id, "e2f45975-aa92-4c0c-ad9b-05cd456d9b00");
        assert_eq!(request.tool_use, "toolu_01F6WsndtGMAv3yQPYeTTxMc");
        assert_eq!(request.summary, "/private/tmp/sj-probe/probe4.txt");
        assert_eq!(
            request.detail,
            ToolDetail::Write {
                path: "/private/tmp/sj-probe/probe4.txt".to_owned(),
                content: slopty_proto::agent::Clipped::whole("hi".to_owned()),
            }
        );
        assert_eq!(fold.pending(), [request.id.as_str()]);

        let line = fold.answer(&request.id, true, None, &[], false).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            sent,
            json!({"type":"control_response","response":{"subtype":"success","request_id":"e2f45975-aa92-4c0c-ad9b-05cd456d9b00","response":{"behavior":"allow","updatedInput":{"file_path":"/private/tmp/sj-probe/probe4.txt","content":"hi"}}}})
        );
        assert!(fold.pending().is_empty());
        assert!(fold.answer(&request.id, true, None, &[], false).is_none(), "answered once");
    }

    #[test]
    fn a_denial_carries_the_message_the_agent_will_read() {
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let _updates = fold.apply(event, 0);
        let line = fold
            .answer("e2f45975-aa92-4c0c-ad9b-05cd456d9b00", false, None, &[], false)
            .expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(sent["response"]["response"]["behavior"], "deny");
        assert_eq!(sent["response"]["response"]["message"], DENIED);
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let _updates = fold.apply(event, 0);
        let line = fold
            .answer(
                "e2f45975-aa92-4c0c-ad9b-05cd456d9b00",
                false,
                Some("not that file"),
                &[],
                false,
            )
            .expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(sent["response"]["response"]["message"], "not that file");
    }

    #[test]
    fn text_deltas_stream_the_partial_and_the_record_clears_it() {
        let mut fold = Fold::default();
        let delta = |t: &str| {
            format!(
                r#"{{"type":"stream_event","event":{{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{t}"}}}}}}"#
            )
        };
        // The blocks opening say what the agent is doing until the text arrives.
        let start = |block: &str| {
            format!(
                r#"{{"type":"stream_event","event":{{"type":"content_block_start","index":0,"content_block":{block}}}}}"#
            )
        };
        let Some(thinking) = parse(&start(r#"{"type":"thinking","thinking":"","signature":""}"#))
        else {
            panic!("parses")
        };
        assert_eq!(thinking, Event::BlockStart(Block::Thinking));
        assert_eq!(
            fold.apply(thinking, 0),
            [Update::Status { status: AgentStatus::Working, detail: Some("thinking…".into()) }]
        );
        let Some(text) = parse(&start(r#"{"type":"text","text":""}"#)) else { panic!("parses") };
        assert_eq!(
            fold.apply(text, 0),
            [Update::Status { status: AgentStatus::Working, detail: None }]
        );
        let Some(first) = parse(&delta("Loo")) else { panic!("parses") };
        assert_eq!(fold.apply(first, 0), [Update::Partial("Loo".into())]);
        let Some(second) = parse(&delta("king.")) else { panic!("parses") };
        assert_eq!(fold.apply(second, 0), [Update::Partial("Looking.".into())]);
        let Some(stop) = parse(r#"{"type":"stream_event","event":{"type":"message_stop"}}"#) else {
            panic!("parses")
        };
        assert_eq!(stop, Event::MessageStop);
        assert!(fold.apply(stop, 0).is_empty());
        let Some(record) = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."}]}}"#,
        ) else {
            panic!("parses")
        };
        let updates = fold.apply(record, 7);
        assert_eq!(updates.first(), Some(&Update::Partial(String::new())), "cleared first");
        assert!(
            matches!(updates.get(1), Some(Update::Entries(e)) if e.len() == 1 && e[0].at == Some(7))
        );
    }

    #[test]
    fn the_context_fill_follows_the_usage_and_the_result_names_the_window() {
        let mut fold = Fold::default();
        let assistant = |tokens: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":10,"cache_creation_input_tokens":{tokens},"cache_read_input_tokens":990,"output_tokens":7}}}}}}"#
            )
        };
        let Some(first) = parse(&assistant(30_000)) else { panic!("parses") };
        let context = |updates: &[Update]| {
            updates.iter().find_map(|u| match u {
                Update::Context(c) => Some(*c),
                _ => None,
            })
        };
        assert_eq!(
            context(&fold.apply(first, 0)),
            Some(Context { tokens: 31_000, window: None }),
            "input, cache writes and cache reads together; the window unknown"
        );
        let Some(again) = parse(&assistant(30_000)) else { panic!("parses") };
        assert_eq!(context(&fold.apply(again, 0)), None, "unchanged, not repeated");
        let Some(result) = parse(
            r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"total_cost_usd":0.01,"result":"ok","session_id":"s","modelUsage":{"claude-haiku-4-5-20251001":{"contextWindow":200000},"claude-fable-5-1":{"contextWindow":1000000}}}"#,
        ) else {
            panic!("parses")
        };
        let Event::Result(turn) = &result else { panic!("a result") };
        assert_eq!(turn.context_window, Some(1_000_000), "the widest window");
        let updates = fold.apply(result, 0);
        assert_eq!(
            context(&updates),
            Some(Context { tokens: 31_000, window: Some(1_000_000) }),
            "the result names the window"
        );
        assert!(matches!(updates.last(), Some(Update::Turn(_))));
        let Some(less) = parse(&assistant(4_000)) else { panic!("parses") };
        assert_eq!(
            context(&fold.apply(less, 0)),
            Some(Context { tokens: 5_000, window: Some(1_000_000) }),
            "after a compaction the fill drops and keeps the window"
        );
        let Some(no_usage) = parse(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
        ) else {
            panic!("parses")
        };
        assert_eq!(context(&fold.apply(no_usage, 0)), None, "a record without usage says nothing");
        // A compaction boundary is an entry and lowers the chip at once.
        let Some(boundary) = parse(
            r#"{"type":"system","subtype":"compact_boundary","compact_metadata":{"trigger":"manual","pre_tokens":5000,"post_tokens":700}}"#,
        ) else {
            panic!("parses")
        };
        let updates = fold.apply(boundary, 9);
        assert_eq!(context(&updates), Some(Context { tokens: 700, window: Some(1_000_000) }));
        assert!(
            matches!(
                updates.last(),
                Some(Update::Entries(e)) if e.len() == 1 && e[0].at == Some(9) && e[0].body
                    == TranscriptBody::Compacted {
                        trigger: "manual".to_owned(),
                        pre_tokens: 5000,
                        post_tokens: Some(700)
                    }
            ),
            "{updates:?}"
        );
    }

    #[test]
    fn an_interrupt_is_acked_and_ends_the_turn_as_an_error() {
        assert_eq!(
            serde_json::from_str::<Value>(&interrupt("slopty-int-1")).ok(),
            Some(
                json!({"type":"control_request","request_id":"slopty-int-1","request":{"subtype":"interrupt"}})
            )
        );
        let ack = parse(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"slopty-int-1","response":{"still_queued":[]}}}"#,
        );
        assert_eq!(ack, Some(Event::ControlAck { request_id: "slopty-int-1".into(), error: None }));
        let failed = parse(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"x","error":"no turn"}}"#,
        );
        assert_eq!(
            failed,
            Some(Event::ControlAck { request_id: "x".into(), error: Some("no turn".into()) })
        );
        let mut fold = Fold::default();
        let Some(result) = parse(
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"session_id":"s","total_cost_usd":0.001}"#,
        ) else {
            panic!("parses")
        };
        let updates = fold.apply(result, 0);
        assert_eq!(
            updates.first(),
            Some(&Update::Status {
                status: AgentStatus::Done,
                detail: Some("error during execution".into())
            })
        );
        assert!(
            matches!(updates.get(1), Some(Update::Turn(t)) if !t.ok && t.kind == "error_during_execution")
        );
    }

    #[test]
    fn the_launch_line_quotes_only_what_a_shell_would_read() {
        let args = arguments(Some("19146b4d"), Some("opus"));
        assert_eq!(&args[..2], ["-p", "--verbose"]);
        assert_eq!(&args[args.len() - 4..], ["--resume", "19146b4d", "--model", "opus"]);
        assert_eq!(arguments(None, None).len(), 12);
        let words = ["exec", "claude", "-p", "it's", "a b", ""];
        assert_eq!(shell_line(words), "exec claude -p 'it'\\''s' 'a b' ''");
    }

    #[test]
    fn a_policy_denial_and_the_rest_are_named_or_ignored() {
        assert_eq!(
            parse(
                r#"{"type":"system","subtype":"permission_denied","tool_name":"Write","decision_reason":"x","message":"outside the working directory"}"#
            ),
            Some(Event::Denied {
                tool: "Write".into(),
                reason: "outside the working directory".into()
            })
        );
        assert_eq!(
            parse(r#"{"type":"system","subtype":"status","status":"requesting"}"#),
            Some(Event::Status { permission_mode: None, doing: Some("requesting".into()) })
        );
        assert_eq!(
            parse(
                r#"{"type":"system","subtype":"status","status":null,"permissionMode":"acceptEdits"}"#
            ),
            Some(Event::Status { permission_mode: Some("acceptEdits".into()), doing: None })
        );
        assert_eq!(parse(r#"{"type":"rate_limit_event"}"#), Some(Event::Other));
        // Probed on CLI 2.1.269: the windows ride in `unifiedWindows`, spent as a fraction.
        let event = parse(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1789195800,"rateLimitType":"five_hour","unifiedWindows":{"five_hour":{"utilization":0.22999999999999998,"resetsAt":1789195800},"seven_day":{"utilization":0.735,"resetsAt":1789257600}}}}"#,
        );
        assert_eq!(
            event,
            Some(Event::RateLimit(Usage {
                limited: false,
                five_hour: Some(UsageWindow { percent: 23, resets_at: 1_789_195_800 }),
                seven_day: Some(UsageWindow { percent: 74, resets_at: 1_789_257_600 }),
            }))
        );
        let bare = parse(
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rate_limited","resetsAt":1}}"#,
        );
        assert_eq!(
            bare,
            Some(Event::RateLimit(Usage { limited: true, five_hour: None, seven_day: None }))
        );
        // A status record while working names what the agent is doing; a permission or a
        // finished turn is not overwritten by it.
        let mut fold = Fold::default();
        let Some(event) = parse(r#"{"type":"system","subtype":"status","status":"requesting"}"#)
        else {
            panic!()
        };
        assert_eq!(
            fold.apply(event, 0),
            [Update::Status {
                status: AgentStatus::Working,
                detail: Some("waiting for the model…".to_owned())
            }]
        );
        let Some(event) = parse(CAN_USE_TOOL) else { panic!() };
        let _blocked = fold.apply(event, 0);
        let Some(event) = parse(r#"{"type":"system","subtype":"status","status":"compacting"}"#)
        else {
            panic!()
        };
        assert!(fold.apply(event, 0).is_empty(), "a blocked agent keeps its reason");
        let Some(event) = parse(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Write","input":{}}}}"#,
        ) else {
            panic!()
        };
        assert_eq!(event, Event::BlockStart(Block::Tool("Write".to_owned())));
        assert!(fold.apply(event, 0).is_empty(), "a blocked agent keeps its reason");
        assert_eq!(
            Fold::default().apply(Event::BlockStart(Block::Tool("Write".to_owned())), 0),
            [Update::Status {
                status: AgentStatus::Working,
                detail: Some("calling Write…".into())
            }]
        );
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"no":"type"}"#), None);
        assert_eq!(
            serde_json::from_str::<Value>(&user_message("fix it\nplease", &[])).ok(),
            Some(json!({"type":"user","message":{"role":"user","content":"fix it\nplease"}}))
        );
        // With a picture the content is blocks, the picture first as the API takes it.
        let picture = slopty_proto::agent::Image {
            media_type: "image/png".to_owned(),
            data: vec![0x89, b'P', b'N', b'G'],
        };
        assert_eq!(
            serde_json::from_str::<Value>(&user_message(
                "what colour?",
                std::slice::from_ref(&picture)
            ))
            .ok(),
            Some(json!({"type":"user","message":{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw=="}},
                {"type":"text","text":"what colour?"}]}}))
        );
        assert_eq!(
            serde_json::from_str::<Value>(&user_message("  ", std::slice::from_ref(&picture))).ok(),
            Some(json!({"type":"user","message":{"role":"user","content":[
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw=="}}]}})),
            "nothing typed: no empty text block"
        );
    }

    /// `set_model` / `set_permission_mode` are control requests the agent acknowledges; the
    /// model actually in use is read off the next assistant record, the mode off the status
    /// record the agent writes when it switches.
    #[test]
    fn a_retune_is_asked_in_protocol_and_confirmed_by_what_the_agent_says_next() {
        assert_eq!(
            serde_json::from_str::<Value>(&set_model("slopty-1", "opus")).ok(),
            Some(json!({"type":"control_request","request_id":"slopty-1",
                        "request":{"subtype":"set_model","model":"opus"}}))
        );
        assert_eq!(
            serde_json::from_str::<Value>(&set_permission_mode("slopty-2", "plan")).ok(),
            Some(json!({"type":"control_request","request_id":"slopty-2",
                        "request":{"subtype":"set_permission_mode","mode":"plan"}}))
        );
        let mut fold = Fold::default();
        let init = r#"{"type":"system","subtype":"init","session_id":"s","model":"claude-sonnet-5","permissionMode":"default","tools":[],"slash_commands":[]}"#;
        let Some(init) = parse(init) else { panic!("parses") };
        let updates = fold.apply(init, 0);
        assert!(matches!(updates.first(), Some(Update::Init(i)) if i.model == "claude-sonnet-5"));
        let Some(ack) = parse(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"slopty-1"}}"#,
        ) else {
            panic!("parses")
        };
        assert_eq!(ack, Event::ControlAck { request_id: "slopty-1".into(), error: None });
        assert!(fold.apply(ack, 0).is_empty(), "the ack alone changes nothing shown");
        let same = r#"{"type":"assistant","message":{"model":"claude-sonnet-5","role":"assistant","content":[{"type":"text","text":"hi"}]},"session_id":"s"}"#;
        let Some(same) = parse(same) else { panic!("parses") };
        assert!(
            !fold.apply(same, 0).iter().any(|u| matches!(u, Update::Model(_))),
            "the same model is not news"
        );
        let switched = r#"{"type":"assistant","message":{"model":"claude-opus-5","role":"assistant","content":[{"type":"text","text":"hi"}]},"session_id":"s"}"#;
        let Some(switched) = parse(switched) else { panic!("parses") };
        let models: Vec<String> = fold
            .apply(switched, 0)
            .into_iter()
            .filter_map(|u| if let Update::Model(m) = u { Some(m) } else { None })
            .collect();
        assert_eq!(models, ["claude-opus-5"]);
        let Some(status) =
            parse(r#"{"type":"system","subtype":"status","status":null,"permissionMode":"plan"}"#)
        else {
            panic!("parses")
        };
        assert_eq!(fold.apply(status, 0), [Update::PermissionMode("plan".into())]);
    }
}
