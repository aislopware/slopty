//! A `claude` that speaks the stream-json host protocol, for the driven-agent self-test.
//!
//! The host launches it (`SLOPTY_CLAUDE_BIN`) in place of Claude Code with the same flags,
//! and it answers each prompt from the shape of the prompt itself, so the test needs no real
//! model, network or key:
//!
//! * any prompt — three text deltas, the assistant record, a `success` result;
//! * a prompt starting with `linger` — the deltas, then nothing until an `interrupt` control
//!   request arrives, which it acknowledges before writing the interrupted user record and an
//!   `error_during_execution` result (the way Claude Code ends a stopped turn);
//! * a prompt starting with `write` — a `Write` tool call and a `can_use_tool` control request; an
//!   `allow` answer produces a tool result and a closing message, a `deny` a failed tool result
//!   with the host's message and a closing message that says so.
//!
//! Stdin closing ends it, as it ends Claude Code.

use std::io::{BufRead as _, Write};

use serde_json::{Value, json};

const SESSION: &str = "fake-session";
const MODEL: &str = "fake-model";
const DELTAS: [&str; 3] = ["Hello", " from", " the fake"];

fn emit(out: &mut impl Write, record: &Value) -> std::io::Result<()> {
    writeln!(out, "{record}")?;
    out.flush()
}

fn record(kind: &str, message: &Value) -> Value {
    json!({
        "type": kind,
        "message": message,
        "session_id": SESSION,
        "parent_tool_use_id": null,
        "uuid": "u",
        "timestamp": "2026-09-12T00:00:00.000Z",
    })
}

fn assistant(content: &Value) -> Value {
    record(
        "assistant",
        &json!({"model": MODEL, "id": "msg", "type": "message", "role": "assistant",
               "content": content, "stop_reason": null, "usage": {"input_tokens": 1, "output_tokens": 1}}),
    )
}

fn result(subtype: &str, text: &str) -> Value {
    json!({
        "type": "result", "subtype": subtype, "is_error": subtype != "success",
        "num_turns": 1, "result": text, "session_id": SESSION, "total_cost_usd": 0.01,
        "duration_ms": 1, "duration_api_ms": 1, "permission_denials": [], "uuid": "r",
    })
}

fn delta(text: &str) -> Value {
    json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0,
           "delta": {"type": "text_delta", "text": text}}, "session_id": SESSION})
}

fn text_of(line: &Value) -> Option<String> {
    let message = line.get("message")?;
    match message.get("content")? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// The answer to a `can_use_tool`: `Some(true)` allow, `Some(false)` deny with the message.
fn answer_of(line: &Value) -> Option<(bool, String)> {
    let response = line.get("response")?.get("response")?;
    let allow = response.get("behavior").and_then(Value::as_str)? == "allow";
    let message = response.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
    Some((allow, message))
}

fn is_interrupt(line: &Value) -> bool {
    line.get("type").and_then(Value::as_str) == Some("control_request")
        && line.get("request").and_then(|r| r.get("subtype")).and_then(Value::as_str)
            == Some("interrupt")
}

fn stream_deltas(out: &mut impl Write) -> std::io::Result<()> {
    for text in DELTAS {
        emit(out, &delta(text))?;
    }
    Ok(())
}

fn finish_text(out: &mut impl Write) -> std::io::Result<()> {
    let text: String = DELTAS.concat();
    emit(out, &json!({"type": "stream_event", "event": {"type": "message_stop"}}))?;
    emit(out, &assistant(&json!([{"type": "text", "text": text}])))?;
    emit(out, &result("success", &text))
}

fn main() -> std::io::Result<()> {
    match run() {
        // A broken pipe means the host is gone; nothing to report to.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other,
    }
}

fn run() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    emit(
        &mut out,
        &json!({"type": "system", "subtype": "init", "cwd": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
                "session_id": SESSION, "tools": ["Bash", "Edit", "Read", "Write"], "model": MODEL,
                "permissionMode": "default", "slash_commands": ["/compact"], "uuid": "i"}),
    )?;
    let mut requests = 0_u32;
    // What the turn in progress waits for.
    let mut waiting: Option<Wait> = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if is_interrupt(&value) {
            let id = value.get("request_id").and_then(Value::as_str).unwrap_or("");
            emit(
                &mut out,
                &json!({"type": "control_response", "response": {"subtype": "success",
                        "request_id": id, "response": {"still_queued": []}}}),
            )?;
            if waiting.take().is_some() {
                emit(
                    &mut out,
                    &record(
                        "user",
                        &json!({"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}),
                    ),
                )?;
                emit(&mut out, &result("error_during_execution", ""))?;
            }
            continue;
        }
        if let Some((allow, message)) = answer_of(&value) {
            let Some(Wait::Permission { tool_use }) = waiting.take() else { continue };
            let (content, closing) = if allow {
                ("wrote note.txt", "Done: the note is written.")
            } else {
                (message.as_str(), "Understood, I did not write it.")
            };
            emit(
                &mut out,
                &record(
                    "user",
                    &json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use,
                           "content": content, "is_error": !allow}]}),
                ),
            )?;
            emit(&mut out, &assistant(&json!([{"type": "text", "text": closing}])))?;
            emit(&mut out, &result("success", closing))?;
            continue;
        }
        if value.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(text) = text_of(&value) else { continue };
        // Claude Code replays the prompt as a user record first.
        emit(
            &mut out,
            &json!({"type": "user", "message": {"role": "user", "content": text}, "session_id": SESSION,
                    "parent_tool_use_id": null, "uuid": "p", "timestamp": "2026-09-12T00:00:00.000Z", "isReplay": true}),
        )?;
        if text.starts_with("write") {
            requests = requests.wrapping_add(1);
            let tool_use = format!("toolu_{requests}");
            let input = json!({"file_path": "note.txt", "content": "hi"});
            emit(
                &mut out,
                &assistant(
                    &json!([{"type": "tool_use", "id": tool_use, "name": "Write", "input": input}]),
                ),
            )?;
            emit(
                &mut out,
                &json!({"type": "control_request", "request_id": format!("req_{requests}"),
                        "request": {"subtype": "can_use_tool", "tool_name": "Write", "display_name": "Write",
                                    "input": input, "description": "Write note.txt", "tool_use_id": tool_use}}),
            )?;
            waiting = Some(Wait::Permission { tool_use });
        } else if text.starts_with("linger") {
            stream_deltas(&mut out)?;
            waiting = Some(Wait::Interrupt);
        } else {
            stream_deltas(&mut out)?;
            finish_text(&mut out)?;
        }
    }
    Ok(())
}

/// What a turn in progress waits for from the host.
enum Wait {
    /// The answer to a `can_use_tool` for this tool call.
    Permission {
        /// The call's id, echoed in the tool result.
        tool_use: String,
    },
    /// An interrupt.
    Interrupt,
}
