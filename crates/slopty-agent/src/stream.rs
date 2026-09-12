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
use slopty_proto::agent::{AgentStatus, BlockReason, PermissionRequest, TranscriptEntry};

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
    /// The streamed message ended; the `Record` that follows carries its final form.
    MessageStop,
    /// A tool waits on the human. The raw input the host echoes back on allow rides here.
    Permission {
        /// What the client is shown.
        request: PermissionRequest,
        /// The tool input, as sent, for the allow reply.
        input: Value,
    },
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
    /// Anything else: status, rate limits, hook bookkeeping, thinking token counts.
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
        ("system", Some("permission_denied")) => Event::Denied {
            tool: string(&record, "tool_name"),
            reason: record
                .get("message")
                .or_else(|| record.get("decision_reason"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        ("assistant" | "user", _) => Event::Record(record),
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
            })
        }
        _ => Event::Other,
    };
    Some(event)
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
    let pretty = serde_json::to_string_pretty(&input).unwrap_or_default();
    Some(Event::Permission {
        request: PermissionRequest {
            id: string(record, "request_id"),
            tool_use: string(request, "tool_use_id"),
            tool,
            summary,
            input: transcript::clip(&pretty),
        },
        input,
    })
}

/// The line that sends the human's words as the next prompt.
#[must_use]
pub fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}}).to_string()
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

fn allow_line(request_id: &str, input: &Value) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {"behavior": "allow", "updatedInput": input},
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
    /// A turn ended.
    Turn(TurnResult),
}

/// Folds the event stream of one conversation into [`Update`]s.
///
/// Keeps the tool names of the calls seen (so results are named), the inputs of the
/// permissions still open (so an allow echoes the input back), the text of the message being
/// streamed, and the last status (so nothing is repeated).
#[derive(Debug, Default)]
pub struct Fold {
    tools: ToolNames,
    pending: HashMap<String, Value>,
    partial: String,
    status: Option<(AgentStatus, Option<String>)>,
    init: Option<Init>,
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
                self.init = Some(init);
                self.status(AgentStatus::Idle, None).into_iter().collect()
            }
            Event::Record(record) => self.record(&record, now),
            Event::TextDelta(text) => {
                self.partial.push_str(&text);
                vec![Update::Partial(self.partial.clone())]
            }
            Event::Permission { request, input } => {
                self.pending.insert(request.id.clone(), input);
                let mut out = self
                    .status(
                        AgentStatus::Blocked(BlockReason::Permission {
                            tool: request.tool.clone(),
                        }),
                        Some(crate::truncate(&request.summary)),
                    )
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
    ) -> Option<String> {
        let input = self.pending.remove(request_id)?;
        self.status = Some((AgentStatus::Working, None));
        Some(if allowed {
            allow_line(request_id, &input)
        } else {
            deny_line(request_id, message.unwrap_or(DENIED))
        })
    }

    fn record(&mut self, record: &Value, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        let is_assistant = record.get("type").and_then(Value::as_str) == Some("assistant");
        if is_assistant && !self.partial.is_empty() {
            self.partial.clear();
            out.push(Update::Partial(String::new()));
        }
        let entries: Vec<TranscriptEntry> = transcript::record_entries(&mut self.tools, record)
            .into_iter()
            .map(|entry| TranscriptEntry { at: entry.at.or(Some(now)), body: entry.body })
            .collect();
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
    use slopty_proto::agent::TranscriptBody;

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
        for event in events(ONE_TURN) {
            for update in fold.apply(event, 1_000) {
                match update {
                    Update::Entries(e) => entries.extend(e),
                    Update::Status { status, detail } => statuses.push((status, detail)),
                    Update::Turn(t) => turns.push(t),
                    Update::Partial(_) | Update::Permission(_) => {}
                }
            }
        }
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
        assert!(request.input.text.contains("\"content\": \"hi\""), "{:?}", request.input);
        assert_eq!(fold.pending(), [request.id.as_str()]);

        let line = fold.answer(&request.id, true, None).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            sent,
            json!({"type":"control_response","response":{"subtype":"success","request_id":"e2f45975-aa92-4c0c-ad9b-05cd456d9b00","response":{"behavior":"allow","updatedInput":{"file_path":"/private/tmp/sj-probe/probe4.txt","content":"hi"}}}})
        );
        assert!(fold.pending().is_empty());
        assert!(fold.answer(&request.id, true, None).is_none(), "answered once");
    }

    #[test]
    fn a_denial_carries_the_message_the_agent_will_read() {
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let _updates = fold.apply(event, 0);
        let line = fold.answer("e2f45975-aa92-4c0c-ad9b-05cd456d9b00", false, None).expect("open");
        let sent: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(sent["response"]["response"]["behavior"], "deny");
        assert_eq!(sent["response"]["response"]["message"], DENIED);
        let mut fold = Fold::default();
        let Some(event) = parse(CAN_USE_TOOL) else { panic!("parses") };
        let _updates = fold.apply(event, 0);
        let line = fold
            .answer("e2f45975-aa92-4c0c-ad9b-05cd456d9b00", false, Some("not that file"))
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
            Some(Event::Other)
        );
        assert_eq!(parse(r#"{"type":"rate_limit_event"}"#), Some(Event::Other));
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"no":"type"}"#), None);
        assert_eq!(
            serde_json::from_str::<Value>(&user_message("fix it\nplease")).ok(),
            Some(json!({"type":"user","message":{"role":"user","content":"fix it\nplease"}}))
        );
    }
}
