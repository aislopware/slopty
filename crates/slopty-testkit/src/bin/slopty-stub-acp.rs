//! A stand-in for an ACP agent (`<agent> acp` and the like), for tests that drive an agent over
//! the Agent Client Protocol with no model and nobody's account (`docs/decisions/agents.md`,
//! "Any ACP agent, driven").
//!
//! It replays a recording of an ACP session (one `{"dir", "msg"}` a line, `in` what went to the
//! agent, `out` what the agent wrote) against what it is sent:
//!
//! - `--version` answers as the recorded agent does.
//! - The recording is cut into steps: a message that went to the agent, and what the agent wrote
//!   after it up to the next message.
//! - A message that is the next step's gets that step's lines. A request is the step's by its
//!   method (a prompt by its words too), a notification by its method, and an answer to one of the
//!   agent's own requests by that request's id and what it answers.
//! - The client numbers its requests as it likes: an answer the agent wrote to one of them goes out
//!   under the id it was sent with.
//! - Anything else is unexpected: it is noted, and a request is answered with an error.
//!
//! It reads what to replay from `stub-acp.json` beside the path it was started as (a test puts it
//! on the worker's `PATH` under the agent's program name, with the file beside it):
//! `{"fixture": …, "record": …}`. No environment variable carries it, since the worker that
//! starts it may be the test itself.
//!
//! What it was given and what it heard goes to `record` as one JSON document, replaced whole
//! after each message: how many times it was started on that record, its arguments, its
//! directory, every message, and the unexpected ones.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "stdout is its ACP; stderr its log")]

use std::collections::HashMap;
use std::error::Error;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

/// What the recorded agent answers to `--version`.
const VERSION: &str = "1.18.34";

/// JSON-RPC's code for an internal error.
const INTERNAL_ERROR: i64 = -32_603;

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
            eprintln!("stub acp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// One message that went to the agent, and what the agent wrote after it.
struct Step {
    sent: Value,
    wrote: Vec<Value>,
}

fn run(at: &Path, args: &[String]) -> Fallible<()> {
    let config = at.with_file_name("stub-acp.json");
    let config: Value = serde_json::from_slice(&std::fs::read(&config)?)?;
    let path = |key: &str| config.get(key).and_then(Value::as_str).map(PathBuf::from);
    let fixture = path("fixture").ok_or("stub-acp.json names no fixture")?;
    let steps = steps(&std::fs::read_to_string(fixture)?)?;
    let record_at = path("record");
    // How many times it was started on this record, the one before counted.
    let before = record_at.as_deref().and_then(|at| std::fs::read(at).ok());
    let starts = before
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|before| before.get("starts").and_then(Value::as_u64))
        .unwrap_or(0)
        .saturating_add(1);
    let mut record = json!({
        "starts": starts,
        "argv": args,
        "cwd": std::env::current_dir()?.to_string_lossy(),
        "heard": [],
        "unexpected": [],
    });
    save(record_at.as_deref(), &record)?;

    // The recording's ids of the client's requests, to the ids they were sent with.
    let mut ids: HashMap<String, Value> = HashMap::new();
    let mut out = std::io::stdout().lock();
    let mut next = 0_usize;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let sent: Value = serde_json::from_str(&line)?;
        push(&mut record, "heard", sent.clone());
        let step = steps.get(next).filter(|step| same(&step.sent, &sent));
        if step.is_none() {
            push(&mut record, "unexpected", sent.clone());
        }
        // Saved before the answer goes, so what the answer sets off finds it said.
        save(record_at.as_deref(), &record)?;
        if let Some(step) = step {
            next = next.saturating_add(1);
            if let (Some(_), Some(was), Some(is)) =
                (sent.get("method"), step.sent.get("id"), sent.get("id"))
            {
                ids.insert(was.to_string(), is.clone());
            }
            for wrote in &step.wrote {
                writeln!(out, "{}", renumbered(wrote, &ids))?;
            }
        } else if let (Some(method), Some(id)) = (sent.get("method"), sent.get("id")) {
            let error = json!({
                "code": INTERNAL_ERROR,
                "message": format!("the stand-in agent has no record of {method}"),
            });
            writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": id, "error": error }))?;
        }
        out.flush()?;
    }
    Ok(())
}

/// The recording cut into steps.
fn steps(fixture: &str) -> Fallible<Vec<Step>> {
    let mut steps: Vec<Step> = Vec::new();
    for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
        let line: Value = serde_json::from_str(line)?;
        let msg = line.get("msg").cloned().ok_or("a fixture line with no msg")?;
        match (line.get("dir").and_then(Value::as_str), steps.last_mut()) {
            (Some("in"), _) => steps.push(Step { sent: msg, wrote: Vec::new() }),
            (Some("out"), Some(step)) => step.wrote.push(msg),
            (Some("out"), None) => return Err("the agent wrote before it was asked".into()),
            (dir, _) => return Err(format!("a fixture line going {dir:?}").into()),
        }
    }
    Ok(steps)
}

/// Whether `sent` is the message `recorded` was.
fn same(recorded: &Value, sent: &Value) -> bool {
    match (recorded.get("method"), sent.get("method")) {
        (Some(was), Some(is)) => {
            was == is
                && recorded.get("id").is_some() == sent.get("id").is_some()
                && (is != "session/prompt" || prompt(recorded) == prompt(sent))
        }
        // An answer to one of the agent's own requests, whose ids are the recording's.
        (None, None) => {
            recorded.get("id") == sent.get("id")
                && recorded.get("result") == sent.get("result")
                && recorded.get("error").is_some() == sent.get("error").is_some()
        }
        _ => false,
    }
}

fn prompt(message: &Value) -> Option<&Value> {
    message.get("params")?.get("prompt")
}

/// `wrote`, and when it answers one of the client's requests, under the id that was sent.
fn renumbered(wrote: &Value, ids: &HashMap<String, Value>) -> Value {
    let answer = wrote.get("method").is_none();
    let sent = wrote.get("id").and_then(|id| ids.get(&id.to_string()));
    match (answer, sent, wrote.clone()) {
        (true, Some(id), Value::Object(mut map)) => {
            map.insert("id".to_owned(), id.clone());
            Value::Object(map)
        }
        (_, _, wrote) => wrote,
    }
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
