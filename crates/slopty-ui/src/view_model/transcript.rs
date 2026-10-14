//! A thread as the shell's transcript: the person's messages as user entries, each turn's answer as
//! one assistant entry whose parts are its text, reasoning, tool calls and notices.

use slopty_proto::thread::{
    IntentId, ItemBody, ThreadId, ThreadState, ToolCall, ToolDetail, ToolState, Turn, TurnId,
    TurnState, kind,
};
use slopty_view_doc::{MessagePart, MessageRole, MessageStatus, SessionMessageEntry, ToolDiffStat};

/// `state`'s transcript, its entries written on `device`.
///
/// `child_running` says whether a subagent's thread still runs, for its spawn chip; `named`
/// gives the id the shell gave a message it sent, by the intent that sent it.
#[must_use]
pub fn entries(
    state: &ThreadState,
    device: &str,
    child_running: impl Fn(ThreadId) -> Option<bool>,
    named: impl Fn(IntentId) -> Option<String>,
) -> Vec<SessionMessageEntry> {
    let mut out: Vec<SessionMessageEntry> = Vec::new();
    // The answer being gathered: its turn, and its entry.
    let mut answer: Option<(TurnId, SessionMessageEntry)> = None;
    for item in &state.items {
        let id = item.id.0.clone();
        let at = i64::try_from(item.at_ms.as_millis()).unwrap_or(i64::MAX);
        if let ItemBody::User(message) = &item.body {
            // The id the shell gave the message, where it sent it, so its echo gives way.
            let id = message.intent.and_then(&named).unwrap_or(id);
            if let Some((_, entry)) = answer.take() {
                out.push(entry);
            }
            out.push(SessionMessageEntry {
                parts: vec![MessagePart::Text {
                    id: format!("{id}:text"),
                    text: message.text.text.clone(),
                }],
                id,
                role: MessageRole::User,
                created_at: at,
                device_id: device.to_owned(),
                status: None,
                continuation_of: None,
                duration_ms: None,
            });
            continue;
        }
        let Some(part) = part(&id, &item.body, &child_running) else { continue };
        match &mut answer {
            Some((turn, entry)) if *turn == item.turn => entry.parts.push(part),
            _ => {
                if let Some((_, entry)) = answer.take() {
                    out.push(entry);
                }
                let turn = state.turns.iter().find(|t| t.id == item.turn);
                answer = Some((item.turn, assistant(&id, at, device, turn, part)));
            }
        }
    }
    if let Some((_, entry)) = answer {
        out.push(entry);
    }
    out
}

/// A turn's answer, begun with its first part.
fn assistant(
    id: &str,
    at: i64,
    device: &str,
    turn: Option<&Turn>,
    first: MessagePart,
) -> SessionMessageEntry {
    let mut parts = vec![first];
    let status = turn.map(|turn| match &turn.state {
        TurnState::Active => MessageStatus::Streaming,
        TurnState::Complete => MessageStatus::Complete,
        TurnState::Interrupted => MessageStatus::Aborted,
        TurnState::Failed { error, .. } => {
            parts.push(MessagePart::Error { id: format!("{id}:error"), message: error.clone() });
            MessageStatus::Complete
        }
    });
    let duration_ms = turn.and_then(|turn| {
        let ended = turn.ended_ms?;
        i64::try_from(ended.as_millis().saturating_sub(turn.started_ms.as_millis())).ok()
    });
    SessionMessageEntry {
        id: format!("answer:{id}"),
        role: MessageRole::Assistant,
        parts,
        created_at: at,
        device_id: device.to_owned(),
        status,
        continuation_of: None,
        duration_ms,
    }
}

/// An item of the agent's as a part of its answer; nothing for what the shell draws no part of.
fn part(
    id: &str,
    body: &ItemBody,
    child_running: &impl Fn(ThreadId) -> Option<bool>,
) -> Option<MessagePart> {
    let id = id.to_owned();
    Some(match body {
        ItemBody::Text(text) => MessagePart::Text { id, text: text.text.clone() },
        ItemBody::Reasoning(text) => MessagePart::Reasoning { id, text: text.text.clone() },
        ItemBody::Tool(call) => tool(id, call, child_running),
        ItemBody::Notice(notice) => MessagePart::Error { id, message: notice.text.text.clone() },
        ItemBody::User(_)
        | ItemBody::Compaction(_)
        | ItemBody::Review { .. }
        | ItemBody::Extra { .. } => return None,
    })
}

/// A tool call as the shell's tool part.
fn tool(
    id: String,
    call: &ToolCall,
    child_running: &impl Fn(ThreadId) -> Option<bool>,
) -> MessagePart {
    let input: Option<serde_json::Value> = serde_json::from_str(&call.input.text).ok();
    let text = |key: &str| {
        input
            .as_ref()
            .and_then(|i| i.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let mut diff_stats = None;
    let decoded = match (&call.detail, call.kind.as_str()) {
        (Some(ToolDetail::Exec(exec)), _) => {
            slopty_view_proto::ToolCall::Exec { command: exec.command.text.clone() }
        }
        (Some(ToolDetail::Read(read)), _) => {
            slopty_view_proto::ToolCall::ReadFile { path: read.path.clone() }
        }
        (Some(ToolDetail::Edit(edit)), _) => {
            diff_stats = Some(vec![ToolDiffStat {
                path: edit.path.clone(),
                additions: u64::from(edit.patch.added),
                deletions: u64::from(edit.patch.removed),
            }]);
            slopty_view_proto::ToolCall::EditFile {
                path: edit.path.clone(),
                old_string: text("old_string"),
                new_string: text("new_string"),
            }
        }
        (Some(ToolDetail::Write(write)), _) => {
            diff_stats = Some(vec![ToolDiffStat {
                path: write.path.clone(),
                additions: u64::from(write.patch.added),
                deletions: u64::from(write.patch.removed),
            }]);
            slopty_view_proto::ToolCall::WriteFile { path: write.path.clone(), content: None }
        }
        (Some(ToolDetail::Search(search)), _) => slopty_view_proto::ToolCall::Search {
            pattern: search.pattern.clone(),
            path: search.path.clone(),
        },
        (Some(ToolDetail::Fetch(fetch)), _) => slopty_view_proto::ToolCall::WebFetch {
            url: fetch.url.clone(),
            prompt: fetch.prompt.clone(),
        },
        (Some(ToolDetail::WebSearch(search)), _) => {
            slopty_view_proto::ToolCall::WebSearch { query: search.query.clone() }
        }
        (Some(ToolDetail::Mcp(mcp)), _) => slopty_view_proto::ToolCall::Mcp {
            server: mcp.server.clone(),
            tool: mcp.tool.clone(),
            input: input.clone(),
        },
        (Some(ToolDetail::Agent(agent)), _) => slopty_view_proto::ToolCall::Unknown {
            name: agent
                .description
                .as_ref()
                .map_or_else(|| "Agent".to_owned(), |about| format!("Agent: {about}")),
            input: input.clone(),
        },
        (_, kind::AGENT) => slopty_view_proto::ToolCall::Unknown {
            name: format!("Agent: {}", call.title),
            input: input.clone(),
        },
        _ => slopty_view_proto::ToolCall::Unknown { name: call.name.clone(), input: input.clone() },
    };
    let child = call.child.filter(|_| decoded.is_subagent_spawn());
    let subagent_status = child.map(|child| match child_running(child) {
        Some(true) => slopty_view_doc::SubagentStatus::Running,
        _ if call.state == ToolState::Failed => slopty_view_doc::SubagentStatus::Failed,
        _ => slopty_view_doc::SubagentStatus::Done,
    });
    MessagePart::Tool {
        id,
        call: decoded,
        is_error: matches!(call.state, ToolState::Failed | ToolState::Rejected),
        resolved: call.state.is_final(),
        output: call
            .output
            .as_ref()
            .and_then(|out| slopty_view_doc::summarize_tool_output(&out.text)),
        diff: None,
        output_ref: None,
        output_bytes: call.output.as_ref().map(|out| u64::from(out.chars)),
        diff_ref: None,
        diff_stats,
        subagent_ref: child.map(|child| child.to_string()),
        subagent_status,
        subagent_tail: None,
    }
}
