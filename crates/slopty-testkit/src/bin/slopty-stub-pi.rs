//! A stand-in for `pi`, for tests that drive pi over its RPC mode with no model and nobody's
//! account (`docs/decisions/agents.md`, "pi on the worker").
//!
//! It replays a recording of pi's RPC mode (`cargo xtask pi fixtures`: one `{"dir", "msg"}` a
//! line, `in` what went to pi, `out` what pi wrote) against what it is sent:
//!
//! - `--version` answers as pi 1.0.0 does.
//! - The recording is cut into steps: a command that went to pi, and what pi wrote after it up to
//!   the next command.
//! - A command that is the next step's gets that step's records, the step's response under the
//!   command's own id. A message is the step's by its words, however it was sent (a prompt, a
//!   steer, a follow-up); a dialog's answer by its id and what it says; any other command by its
//!   type.
//! - A query (`get_…`) that is not the next step's gets the recording's own response to it, or an
//!   empty success when the recording has none, and the steps stay where they were.
//! - Anything else is unexpected: it is noted and answered with a failure.
//!
//! - With `session_file`, the session's file is said to be that one wherever pi names it.
//! - With `writer_lock`, it holds that file while it runs, in either mode, made only if it is not
//!   there; finding it there already means two wrote the session at once, which its record says
//!   (`clash`).
//!
//! Started without `--mode rpc` it is pi's TUI on the session: it appends the session the
//! recording ends with (its `get_entries`) to `session_file` as pi's TUI writes it, a header
//! first when the file is new, in two writes a moment apart. Then it waits for its stdin to
//! close, as a TUI waits on its terminal, or with `tui_exits` it exits, as when the person ends
//! it. What it was started with goes to `tui_record`.
//!
//! It reads what to replay from `stub-pi.json` beside the path it was started as (a test puts it
//! on the worker's `PATH` as `pi`, with the file beside it): `{"fixture": …, "record": …}`. No
//! environment variable carries it, since the worker that starts it may be the test itself.
//!
//! What it was given and what it heard goes to `record` as one JSON document, replaced whole
//! after each command: its arguments, its directory, whether the extension it was given is
//! Slopty's gate, every command, and the unexpected ones.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "stdout is its RPC; stderr its log")]

use std::error::Error;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

/// What pi 1.0.0 answers to `--version`.
const VERSION: &str = "1.0.0";

/// What the gate names itself in each dialog (`slopty_agent::pi::GATE_PROTOCOL`).
const GATE: &str = "\"slopty-gate/1\"";

type Fallible<T> = Result<T, Box<dyn Error>>;

fn main() -> ExitCode {
    let mut argv = std::env::args();
    let at = argv.next().map(PathBuf::from).unwrap_or_default();
    let args: Vec<String> = argv.collect();
    if args.iter().any(|a| a == "--version") {
        println!("{VERSION}");
        return ExitCode::SUCCESS;
    }
    match run(&at, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stub pi: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One command that went to pi, and what pi wrote after it.
struct Step {
    sent: Value,
    wrote: Vec<Value>,
}

fn run(at: &Path, args: &[String]) -> Fallible<()> {
    let config = at.with_file_name("stub-pi.json");
    let config: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
    let path = |key: &str| config.get(key).and_then(Value::as_str).map(PathBuf::from);
    let fixture = path("fixture").ok_or("stub-pi.json names no fixture")?;
    let (before, steps) = steps(&std::fs::read_to_string(fixture)?)?;
    let session_file = path("session_file");
    let lock = path("writer_lock");
    let clash = lock.as_deref().is_some_and(|lock| {
        std::fs::OpenOptions::new().write(true).create_new(true).open(lock).is_err()
    });
    let ran = if args.windows(2).any(|pair| pair == ["--mode", "rpc"]) {
        rpc(&config, args, &before, &steps, session_file.as_deref(), clash)
    } else {
        tui(&config, args, &steps, session_file.as_deref(), clash)
    };
    if let Some(lock) = lock.filter(|_| !clash) {
        std::fs::remove_file(lock)?;
    }
    ran
}

/// pi in RPC mode: the recording replayed against what it is sent.
fn rpc(
    config: &Value,
    args: &[String],
    before: &[Value],
    steps: &[Step],
    session_file: Option<&Path>,
    clash: bool,
) -> Fallible<()> {
    let path = |key: &str| config.get(key).and_then(Value::as_str).map(PathBuf::from);
    let gate = args
        .iter()
        .position(|a| a == "--extension")
        .and_then(|at| args.get(at.saturating_add(1)))
        .and_then(|file| std::fs::read_to_string(file).ok())
        .is_some_and(|text| text.contains(GATE));
    let mut record = json!({
        "argv": args,
        "cwd": std::env::current_dir()?.to_string_lossy(),
        "gate": gate,
        "heard": [],
        "unexpected": [],
        "clash": clash,
    });
    let record_at = path("record");
    save(record_at.as_deref(), &record)?;

    let mut out = std::io::stdout().lock();
    for line in before {
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    let mut next = 0_usize;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let sent: Value = serde_json::from_str(&line)?;
        push(&mut record, "heard", sent.clone());
        let id = sent.get("id").cloned();
        let kind = text(&sent, "type").to_owned();
        if let Some(step) = steps.get(next).filter(|step| same(&step.sent, &sent)) {
            next = next.saturating_add(1);
            for wrote in &step.wrote {
                let wrote = answering(wrote, &step.sent, id.as_ref(), &kind);
                writeln!(out, "{}", filed(wrote, session_file))?;
            }
        } else if kind.starts_with("get_") {
            let recorded = steps.iter().find(|s| text(&s.sent, "type") == kind).and_then(|s| {
                s.wrote.iter().find(|w| is_response(w) && w.get("id") == s.sent.get("id"))
            });
            let answer = recorded.map_or_else(
                || {
                    let empty =
                        json!({ "type": "response", "command": kind, "success": true, "data": {} });
                    with_id(empty, id.as_ref())
                },
                |wrote| answering(wrote, wrote, id.as_ref(), &kind),
            );
            writeln!(out, "{}", filed(answer, session_file))?;
        } else if kind != "extension_ui_response" {
            push(&mut record, "unexpected", sent.clone());
            let error = "the stand-in pi has no record of this";
            let answer =
                json!({ "type": "response", "command": kind, "success": false, "error": error });
            writeln!(out, "{}", with_id(answer, id.as_ref()))?;
        } else {
            push(&mut record, "unexpected", sent.clone());
        }
        out.flush()?;
        save(record_at.as_deref(), &record)?;
    }
    Ok(())
}

/// pi's TUI on the session: the recorded session appended to `file`, then a wait on stdin.
#[expect(
    clippy::disallowed_methods,
    reason = "a test program with no runtime, pausing between two writes as a TUI's turns do"
)]
fn tui(
    config: &Value,
    args: &[String],
    steps: &[Step],
    file: Option<&Path>,
    clash: bool,
) -> Fallible<()> {
    let cwd = std::env::current_dir()?;
    let record = json!({ "argv": args, "cwd": cwd.to_string_lossy(), "clash": clash });
    let at = config.get("tui_record").and_then(Value::as_str).map(PathBuf::from);
    save(at.as_deref(), &record)?;
    let file = file.ok_or("stub-pi.json names no session_file")?;
    let entries: Vec<Value> = steps
        .iter()
        .filter(|s| text(&s.sent, "type") == "get_entries")
        .flat_map(|s| s.wrote.iter())
        .find(|w| is_response(w))
        .and_then(|w| w.get("data")?.get("entries")?.as_array().cloned())
        .ok_or("the recording has no session to write")?;
    let mut lines = Vec::new();
    if !file.exists() {
        let id = args
            .iter()
            .position(|a| a == "--session-id")
            .and_then(|at| args.get(at.saturating_add(1)))
            .cloned()
            .unwrap_or_default();
        let header =
            json!({ "type": "session", "version": 3, "id": id, "cwd": cwd.to_string_lossy() });
        lines.push(header);
    }
    lines.extend(entries);
    let half = lines.len() / 2;
    let mut session = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    for (n, line) in lines.iter().enumerate() {
        if n == half {
            session.flush()?;
            std::thread::sleep(Duration::from_millis(300));
        }
        writeln!(session, "{line}")?;
    }
    session.flush()?;
    if config.get("tui_exits").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    for line in std::io::stdin().lock().lines() {
        drop(line?);
    }
    Ok(())
}

/// `record` with the session's file said to be `file`, when it names one.
fn filed(mut record: Value, file: Option<&Path>) -> Value {
    let Some(file) = file else { return record };
    if let Some(data) = record.get_mut("data").and_then(Value::as_object_mut)
        && data.contains_key("sessionFile")
    {
        data.insert("sessionFile".to_owned(), json!(file.to_string_lossy()));
    }
    record
}

/// The recording cut into what pi wrote before any command, and the steps.
fn steps(fixture: &str) -> Fallible<(Vec<Value>, Vec<Step>)> {
    let mut before = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
        let line: Value = serde_json::from_str(line)?;
        let msg = line.get("msg").cloned().ok_or("a fixture line with no msg")?;
        match (line.get("dir").and_then(Value::as_str), steps.last_mut()) {
            (Some("in"), _) => steps.push(Step { sent: msg, wrote: Vec::new() }),
            (Some("out"), Some(step)) => step.wrote.push(msg),
            (Some("out"), None) => before.push(msg),
            (dir, _) => return Err(format!("a fixture line going {dir:?}").into()),
        }
    }
    Ok((before, steps))
}

/// Whether `sent` is the command `recorded` was.
fn same(recorded: &Value, sent: &Value) -> bool {
    const MESSAGES: [&str; 3] = ["prompt", "steer", "follow_up"];
    let (was, is) = (text(recorded, "type"), text(sent, "type"));
    if MESSAGES.contains(&was) && MESSAGES.contains(&is) {
        return recorded.get("message") == sent.get("message");
    }
    match is {
        "extension_ui_response" => recorded == sent,
        _ => was == is,
    }
}

/// `wrote`, and when it is the response to `recorded`, as the response to the command sent
/// under `id` as `kind`.
fn answering(wrote: &Value, recorded: &Value, id: Option<&Value>, kind: &str) -> Value {
    let response = is_response(wrote) && wrote.get("id") == recorded.get("id");
    if !response {
        return wrote.clone();
    }
    let mut answer = wrote.clone();
    if let Some(map) = answer.as_object_mut() {
        map.insert("command".to_owned(), json!(kind));
    }
    with_id(answer, id)
}

/// The text at `key` of `value`; empty when there is none.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_response(value: &Value) -> bool {
    text(value, "type") == "response"
}

fn with_id(mut answer: Value, id: Option<&Value>) -> Value {
    if let Some(map) = answer.as_object_mut() {
        match id {
            Some(id) => map.insert("id".to_owned(), id.clone()),
            None => map.remove("id"),
        };
    }
    answer
}

fn push(record: &mut Value, key: &str, value: Value) {
    if let Some(list) = record.get_mut(key).and_then(Value::as_array_mut) {
        list.push(value);
    }
}

/// `record` written whole to `at`, through a sibling renamed into place, so a reader never
/// sees half of it.
fn save(at: Option<&Path>, record: &Value) -> Fallible<()> {
    let Some(at) = at else { return Ok(()) };
    let part = at.with_extension("part");
    std::fs::write(&part, serde_json::to_vec_pretty(record)?)?;
    std::fs::rename(&part, at)?;
    Ok(())
}
