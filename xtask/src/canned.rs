//! A canned Messages API on loopback: what the fixture recorders run Claude Code against, so no
//! account and no model are involved. Each request is answered by the scenario its messages
//! name (`scenario:<name>` in the person's words), with scripted streams: text, thinking, a tool
//! call, a subagent.

use std::fmt::Write as _;
use std::io::{BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

/// One HTTP/1.1 request, `(request line, body)`; `None` at the end of the connection.
pub fn request(reader: &mut impl std::io::BufRead) -> Result<Option<(String, Vec<u8>)>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut length = 0_usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse()?;
        }
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some((line.trim_end().to_owned(), body)))
}

/// A canned Messages API: each request is answered by the scenario its first message names.
pub struct FakeApi {
    pub port: u16,
}

impl FakeApi {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                std::thread::spawn(move || {
                    if let Err(e) = serve_api(stream) {
                        eprintln!("fake API: {e:#}");
                    }
                });
            }
        });
        Ok(Self { port })
    }
}

fn serve_api(stream: TcpStream) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let Some((line, body)) = request(&mut reader)? else { return Ok(()) };
    let body: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body)? };
    let slow = latest(&body).contains("scenario:slow")
        && body.get("tools").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
    let (kind, payload) = if line.contains("count_tokens") {
        ("application/json", json!({ "input_tokens": 100 }).to_string())
    } else if line.starts_with("POST") && line.contains("/v1/messages") {
        let (blocks, stop) = answer(&body);
        if body.get("stream").and_then(Value::as_bool) == Some(true) {
            ("text/event-stream", sse(&blocks, stop))
        } else {
            ("application/json", whole(&blocks, stop).to_string())
        }
    } else {
        ("application/json", "{}".to_owned())
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    writer.write_all(head.as_bytes())?;
    if !slow || kind != "text/event-stream" {
        writer.write_all(payload.as_bytes())?;
        return Ok(());
    }
    // An answer a stop lands in the middle of: each event on its own, [`SLOW`] apart.
    for event in payload.split_inclusive("\n\n") {
        // A stop closes the request: the rest of the answer goes nowhere.
        if writer.write_all(event.as_bytes()).and_then(|()| writer.flush()).is_err() {
            break;
        }
        pause(SLOW);
    }
    Ok(())
}

/// How long the canned model takes between two pieces of a slow answer.
const SLOW: Duration = Duration::from_millis(250);

#[expect(clippy::disallowed_methods, reason = "the canned model's own pace, on its own thread")]
fn pause(at: Duration) {
    std::thread::sleep(at);
}

/// A content block the fake model writes, in pieces.
pub enum Block {
    Text(&'static [&'static str]),
    Thinking(&'static [&'static str]),
    Tool { id: &'static str, name: &'static str, input: &'static [&'static str] },
}

/// The text of a message, its text parts joined.
fn text_of(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect()
        }
        _ => String::new(),
    }
}

/// The person's latest words that name a scenario: what a later turn of a driven run answers.
fn latest(body: &Value) -> String {
    body.get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .rev()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .map(text_of)
        .find(|t| t.contains("scenario:"))
        .unwrap_or_default()
}

/// The fake model's answer to a request: its blocks and its stop reason.
fn answer(body: &Value) -> (Vec<Block>, &'static str) {
    // Claude Code 2.1.295 appends `system` entries (context reminders) after the turn, so the
    // conversation is read without them.
    let messages: Vec<Value> = body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("role").and_then(Value::as_str) != Some("system"))
        .cloned()
        .collect();
    let first = messages.first().map(text_of).unwrap_or_default();
    let latest = latest(body);
    let tool_result =
        messages.last().and_then(|m| m.get("content")).and_then(Value::as_array).is_some_and(
            |parts| {
                parts.iter().any(|p| p.get("type").and_then(Value::as_str) == Some("tool_result"))
            },
        );
    let tools = body.get("tools").and_then(Value::as_array).is_some_and(|t| !t.is_empty());
    let woken = messages.last().map(text_of).is_some_and(|t| t.contains("<task-notification>"));
    if !tools {
        // Claude Code's own side requests (a title, a summary).
        (vec![Block::Text(&["Fake", " title"])], "end_turn")
    } else if first.contains("SUBTASK") {
        (vec![Block::Text(&["Sub", "agent ", "found it."])], "end_turn")
    } else if woken {
        // A background task finished and woke the session.
        (vec![Block::Text(&["It ", "finished."])], "end_turn")
    } else if tool_result {
        (vec![Block::Text(&["Done", ": the ", "command said hi."])], "end_turn")
    } else if latest.contains("scenario:slow") {
        // Long enough for a stop to land mid-answer ([`SLOW`]).
        (vec![Block::Text(&["Slowly"; 40])], "end_turn")
    } else if latest.contains("scenario:think") {
        let thinking = Block::Thinking(&["The person ", "wants a greeting."]);
        (vec![thinking, Block::Text(&["Hello", " there."])], "end_turn")
    } else if first.contains("scenario:background") {
        let input: &[&str] = &[
            r#"{"command": "sleep 3; echo woke", "#,
            r#""description": "Sleep then print a marker", "#,
            r#""run_in_background": true}"#,
        ];
        (vec![Block::Tool { id: "toolu_fake3", name: "Bash", input }], "tool_use")
    } else if latest.contains("scenario:touch") {
        // A call the default mode asks the person about.
        let input: &[&str] =
            &[r#"{"command": "touch made.txt", "#, r#""description": "Make a file"}"#];
        let call = Block::Tool { id: "toolu_fake4", name: "Bash", input };
        (vec![Block::Text(&["Let me ", "make it."]), call], "tool_use")
    } else if first.contains("scenario:agent") {
        let input: &[&str] = &[
            r#"{"description": "Look", "#,
            r#""prompt": "SUBTASK look around", "#,
            r#""subagent_type": "general-purpose"}"#,
        ];
        (vec![Block::Tool { id: "toolu_fake2", name: "Agent", input }], "tool_use")
    } else {
        let input: &[&str] = &[r#"{"command""#, r#": "echo hi""#, r#", "description": "Say hi"}"#];
        let call = Block::Tool { id: "toolu_fake1", name: "Bash", input };
        (vec![Block::Text(&["Let me ", "run it."]), call], "tool_use")
    }
}

const MODEL: &str = "claude-haiku-4-5-20251001";

/// A message id of its own for each answer, as the API gives: a client tells two answers
/// apart by it.
fn message_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("msg_fake{:08}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn usage() -> Value {
    json!({ "input_tokens": 12, "output_tokens": 20 })
}

/// The answer as a stream of server-sent events.
pub fn sse(blocks: &[Block], stop: &str) -> String {
    let mut events = vec![json!({ "type": "message_start", "message": {
        "id": message_id(), "type": "message", "role": "assistant", "model": MODEL,
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": { "input_tokens": 12, "output_tokens": 1 },
    } })];
    for (index, block) in blocks.iter().enumerate() {
        let (start, deltas): (Value, Vec<Value>) = match block {
            Block::Text(pieces) => (
                json!({ "type": "text", "text": "" }),
                pieces.iter().map(|t| json!({ "type": "text_delta", "text": t })).collect(),
            ),
            Block::Thinking(pieces) => (
                json!({ "type": "thinking", "thinking": "", "signature": "" }),
                pieces
                    .iter()
                    .map(|t| json!({ "type": "thinking_delta", "thinking": t }))
                    .chain(std::iter::once(
                        json!({ "type": "signature_delta", "signature": "c2lnbmF0dXJl" }),
                    ))
                    .collect(),
            ),
            Block::Tool { id, name, input } => (
                json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
                input
                    .iter()
                    .map(|j| json!({ "type": "input_json_delta", "partial_json": j }))
                    .collect(),
            ),
        };
        events
            .push(json!({ "type": "content_block_start", "index": index, "content_block": start }));
        for delta in deltas {
            events.push(json!({ "type": "content_block_delta", "index": index, "delta": delta }));
        }
        events.push(json!({ "type": "content_block_stop", "index": index }));
    }
    events.push(json!({ "type": "message_delta",
        "delta": { "stop_reason": stop, "stop_sequence": null }, "usage": { "output_tokens": 20 } }));
    events.push(json!({ "type": "message_stop" }));
    let mut out = String::new();
    for event in &events {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        let _written = write!(out, "event: {kind}\ndata: {event}\n\n");
    }
    out
}

/// The answer in one piece, for a request that does not stream.
fn whole(blocks: &[Block], stop: &str) -> Value {
    let content: Vec<Value> = blocks
        .iter()
        .map(|block| match block {
            Block::Text(pieces) => json!({ "type": "text", "text": pieces.concat() }),
            Block::Thinking(pieces) => {
                json!({ "type": "thinking", "thinking": pieces.concat(), "signature": "c2lnbmF0dXJl" })
            }
            Block::Tool { id, name, input } => {
                let input: Value = serde_json::from_str(&input.concat()).unwrap_or(Value::Null);
                json!({ "type": "tool_use", "id": id, "name": name, "input": input })
            }
        })
        .collect();
    json!({ "id": message_id(), "type": "message", "role": "assistant", "model": MODEL,
        "content": content, "stop_reason": stop, "usage": usage() })
}
