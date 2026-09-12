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
//!   with the host's message and a closing message that says so;
//! * `/cost` — one line naming the cost, the way a slash command answers.
//!
//! It also does what a retune asks: `set_model` is acknowledged and the assistant records
//! that follow name the new model; `set_permission_mode` is acknowledged and followed by the
//! `system/status` record Claude Code writes. `--resume <id>` makes `<id>` the session id
//! (and the `init` says so), `--model <m>` the starting model. Every prompt and text reply is
//! appended to `$HOME/.claude/projects/<escaped cwd>/<session>.jsonl` as Claude Code writes its
//! transcript, so the host's resume list finds the conversation afterwards and a resumed
//! card shows its past.
//!
//! Stdin closing ends it, as it ends Claude Code.

use std::io::{BufRead as _, Write};

use serde_json::{Value, json};

const DEFAULT_SESSION: &str = "fake-session";
const DEFAULT_MODEL: &str = "fake-model";
const DELTAS: [&str; 3] = ["Hello", " from", " the fake"];

/// What this run is: the session (fresh or resumed), the model, the turn count.
struct Fake {
    session: String,
    model: String,
    turns: u32,
    cwd: String,
    transcript: Option<std::path::PathBuf>,
}

impl Fake {
    /// From the arguments the host passes: `--resume <id>` and `--model <m>` are read, the
    /// protocol flags are what they are.
    fn from_args() -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let after = |flag: &str| {
            args.iter().position(|a| a == flag).and_then(|i| args.get(i.wrapping_add(1))).cloned()
        };
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let session = after("--resume").unwrap_or_else(|| DEFAULT_SESSION.to_owned());
        let transcript = std::env::var_os("HOME").map(|home| {
            let escaped: String =
                cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            std::path::PathBuf::from(home)
                .join(".claude")
                .join("projects")
                .join(escaped)
                .join(format!("{session}.jsonl"))
        });
        Self {
            session,
            model: after("--model").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            turns: 0,
            cwd,
            transcript,
        }
    }

    fn record(&self, kind: &str, message: &Value) -> Value {
        json!({
            "type": kind,
            "message": message,
            "session_id": self.session,
            "parent_tool_use_id": null,
            "uuid": "u",
            "timestamp": "2026-09-12T00:00:00.000Z",
        })
    }

    fn assistant(&self, content: &Value) -> Value {
        self.record(
            "assistant",
            &json!({"model": self.model, "id": "msg", "type": "message", "role": "assistant",
                   "content": content, "stop_reason": null, "usage": {"input_tokens": 1, "output_tokens": 1}}),
        )
    }

    fn result(&self, subtype: &str, text: &str) -> Value {
        json!({
            "type": "result", "subtype": subtype, "is_error": subtype != "success",
            "num_turns": self.turns, "result": text, "session_id": self.session,
            "total_cost_usd": 0.01 * f64::from(self.turns),
            "duration_ms": 1, "duration_api_ms": 1, "permission_denials": [], "uuid": "r",
        })
    }

    fn delta(&self, text: &str) -> Value {
        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0,
               "delta": {"type": "text_delta", "text": text}}, "session_id": self.session})
    }

    /// Append a prompt to the transcript file as Claude Code writes it.
    fn note_prompt(&self, text: &str) {
        self.note(&json!({"type": "user", "cwd": self.cwd, "sessionId": self.session,
                          "message": {"role": "user", "content": text}}));
    }

    /// Append a reply to the transcript file as Claude Code writes it.
    fn note_reply(&self, text: &str) {
        self.note(&json!({"type": "assistant", "cwd": self.cwd, "sessionId": self.session,
                          "message": {"role": "assistant", "model": self.model,
                                      "content": [{"type": "text", "text": text}]}}));
    }

    fn note(&self, line: &Value) {
        let Some(path) = &self.transcript else { return };
        if let Some(dir) = path.parent() {
            let _made = std::fs::create_dir_all(dir);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _written = writeln!(file, "{line}");
        }
    }
}

fn emit(out: &mut impl Write, record: &Value) -> std::io::Result<()> {
    writeln!(out, "{record}")?;
    out.flush()
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

/// The answer to a `can_use_tool`: allow or deny with the message, and the `answers` the
/// allow filed into the input (an `AskUserQuestion`), as `"question"="answer"` pairs.
fn answer_of(line: &Value) -> Option<(bool, String, Vec<String>)> {
    let response = line.get("response")?.get("response")?;
    let allow = response.get("behavior").and_then(Value::as_str)? == "allow";
    let message = response.get("message").and_then(Value::as_str).unwrap_or("").to_owned();
    let answers = response
        .get("updatedInput")
        .and_then(|i| i.get("answers"))
        .and_then(Value::as_object)
        .map(|map| {
            map.iter().map(|(q, a)| format!("\"{q}\"=\"{}\"", a.as_str().unwrap_or(""))).collect()
        })
        .unwrap_or_default();
    Some((allow, message, answers))
}

/// A control request's subtype and id, when the line is one.
fn control_request(line: &Value) -> Option<(&str, &str)> {
    if line.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }
    let subtype = line.get("request")?.get("subtype")?.as_str()?;
    let id = line.get("request_id").and_then(Value::as_str).unwrap_or("");
    Some((subtype, id))
}

fn ack(id: &str, response: &Value) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": id,
            "response": response}})
}

fn stream_deltas(fake: &Fake, out: &mut impl Write) -> std::io::Result<()> {
    for text in DELTAS {
        emit(out, &fake.delta(text))?;
    }
    Ok(())
}

fn finish_text(fake: &Fake, out: &mut impl Write) -> std::io::Result<()> {
    let text: String = DELTAS.concat();
    fake.note_reply(&text);
    emit(out, &json!({"type": "stream_event", "event": {"type": "message_stop"}}))?;
    emit(out, &fake.assistant(&json!([{"type": "text", "text": text}])))?;
    emit(out, &fake.result("success", &text))
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
    let mut fake = Fake::from_args();
    emit(
        &mut out,
        &json!({"type": "system", "subtype": "init", "cwd": fake.cwd,
                "session_id": fake.session, "tools": ["Bash", "Edit", "Read", "Write"], "model": fake.model,
                "permissionMode": "default", "slash_commands": ["/compact", "/clear", "/cost"], "uuid": "i"}),
    )?;
    let mut requests = 0_u32;
    // What the turn in progress waits for.
    let mut waiting: Option<Wait> = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some((subtype, id)) = control_request(&value) {
            match subtype {
                "interrupt" => {
                    emit(&mut out, &ack(id, &json!({"still_queued": []})))?;
                    if waiting.take().is_some() {
                        emit(
                            &mut out,
                            &fake.record(
                                "user",
                                &json!({"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]}),
                            ),
                        )?;
                        emit(&mut out, &fake.result("error_during_execution", ""))?;
                    }
                }
                "set_model" => {
                    if let Some(model) =
                        value.get("request").and_then(|r| r.get("model")).and_then(Value::as_str)
                    {
                        model.clone_into(&mut fake.model);
                    }
                    emit(&mut out, &ack(id, &Value::Null))?;
                }
                "set_permission_mode" => {
                    let mode = value
                        .get("request")
                        .and_then(|r| r.get("mode"))
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_owned();
                    emit(&mut out, &ack(id, &json!({"mode": mode})))?;
                    emit(
                        &mut out,
                        &json!({"type": "system", "subtype": "status", "status": null,
                                "permissionMode": mode, "session_id": fake.session, "uuid": "s"}),
                    )?;
                }
                _other => {
                    emit(
                        &mut out,
                        &json!({"type": "control_response", "response": {"subtype": "error",
                                "request_id": id, "error": format!("Unsupported control request subtype: {subtype}")}}),
                    )?;
                }
            }
            continue;
        }
        if let Some((allow, message, answers)) = answer_of(&value) {
            let Some(Wait::Permission { tool_use }) = waiting.take() else { continue };
            // A question's answer comes back the way Claude Code words it, and the closing
            // line is the choice itself.
            let filed = format!(
                "Your questions have been answered: {}. You can now continue with these answers in mind.",
                answers.join(", ")
            );
            let choice = answers
                .first()
                .and_then(|a| a.rsplit_once('=').map(|(_, v)| v.trim_matches('"').to_owned()))
                .unwrap_or_default();
            let (content, closing) = if !answers.is_empty() {
                (filed.as_str(), choice.as_str())
            } else if allow {
                ("wrote note.txt", "Done: the note is written.")
            } else {
                (message.as_str(), "Understood, I did not write it.")
            };
            emit(
                &mut out,
                &fake.record(
                    "user",
                    &json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": tool_use,
                           "content": content, "is_error": !allow}]}),
                ),
            )?;
            emit(&mut out, &fake.assistant(&json!([{"type": "text", "text": closing}])))?;
            emit(&mut out, &fake.result("success", closing))?;
            continue;
        }
        if value.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(text) = text_of(&value) else { continue };
        fake.turns = fake.turns.wrapping_add(1);
        fake.note_prompt(&text);
        // Claude Code replays the prompt as a user record first.
        emit(
            &mut out,
            &json!({"type": "user", "message": {"role": "user", "content": text}, "session_id": fake.session,
                    "parent_tool_use_id": null, "uuid": "p", "timestamp": "2026-09-12T00:00:00.000Z", "isReplay": true}),
        )?;
        if text.starts_with("write") {
            requests = requests.wrapping_add(1);
            let tool_use = format!("toolu_{requests}");
            let input = json!({"file_path": "note.txt", "content": "hi"});
            emit(
                &mut out,
                &fake.assistant(
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
        } else if text.starts_with("edit") {
            // An edit the settings already allow: the call, its result and a todo list, no
            // permission asked. What the card makes of them is the point.
            requests = requests.wrapping_add(1);
            let edit = json!({"file_path": "note.txt", "old_string": "hi\nthere", "new_string": "hello\nthere"});
            let todos = json!({"todos": [
                {"content": "Edit the note", "status": "completed", "activeForm": "Editing the note"},
                {"content": "Tell the user", "status": "in_progress", "activeForm": "Telling the user"}
            ]});
            emit(
                &mut out,
                &fake.assistant(&json!([
                    {"type": "tool_use", "id": format!("toolu_{requests}"), "name": "Edit", "input": edit},
                    {"type": "tool_use", "id": format!("toolu_{requests}_todo"), "name": "TodoWrite", "input": todos}
                ])),
            )?;
            emit(
                &mut out,
                &fake.record(
                    "user",
                    &json!({"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": format!("toolu_{requests}"), "content": "The file note.txt has been updated."},
                        {"type": "tool_result", "tool_use_id": format!("toolu_{requests}_todo"), "content": "Todos have been modified successfully."}
                    ]}),
                ),
            )?;
            let closing = "Edited: hi is now hello.";
            fake.note_reply(closing);
            emit(&mut out, &fake.assistant(&json!([{"type": "text", "text": closing}])))?;
            emit(&mut out, &fake.result("success", closing))?;
        } else if text.starts_with("ask") {
            // A question to the human: a `can_use_tool` for AskUserQuestion, answered with
            // the answers filed into the input (probed on CLI 2.1.269).
            requests = requests.wrapping_add(1);
            let tool_use = format!("toolu_{requests}");
            let input = json!({"questions": [{"question": "Which colour do you prefer?", "header": "Colour",
                "options": [{"label": "Red", "description": "The colour red"}, {"label": "Blue", "description": "The colour blue"}],
                "multiSelect": false}]});
            emit(
                &mut out,
                &fake.assistant(
                    &json!([{"type": "tool_use", "id": tool_use, "name": "AskUserQuestion", "input": input}]),
                ),
            )?;
            emit(
                &mut out,
                &json!({"type": "control_request", "request_id": format!("req_{requests}"),
                        "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion", "display_name": "AskUserQuestion",
                                    "input": input, "tool_use_id": tool_use, "requires_user_interaction": true}}),
            )?;
            waiting = Some(Wait::Permission { tool_use });
        } else if text.starts_with("linger") {
            stream_deltas(&fake, &mut out)?;
            waiting = Some(Wait::Interrupt);
        } else if text.trim() == "/cost" {
            let line = format!("Total cost: ${:.2}", 0.01 * f64::from(fake.turns));
            fake.note_reply(&line);
            emit(&mut out, &fake.assistant(&json!([{"type": "text", "text": line}])))?;
            emit(&mut out, &fake.result("success", &line))?;
        } else {
            stream_deltas(&fake, &mut out)?;
            finish_text(&fake, &mut out)?;
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
