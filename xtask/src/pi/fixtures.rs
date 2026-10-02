//! `cargo xtask pi fixtures`: what the pinned pi says over its RPC mode with Slopty's gate
//! loaded, recorded into `crates/slopty-agent/tests/fixtures/pi/`.
//!
//! The build ([`super::official`]) runs as the worker runs it: `--mode rpc`, the gate as an
//! extension, a session file of its own. It runs in a scratch agent directory with nothing signed
//! in, offline, with telemetry and the version check off, and an environment that holds nothing
//! of this one. Its model is a canned Messages API on loopback, set up in the scratch
//! `models.json` as a provider of its own. The recorder plays the person and the worker at once:
//!
//! 1. a first prompt, answered with thinking and text;
//! 2. a prompt whose answer runs a command: the gate asks, a steer is queued while it waits, and
//!    the call is allowed;
//! 3. a prompt whose command is denied, with the person's reason;
//! 4. a prompt whose command is interrupted while the gate waits;
//!
//! then the session's entries and stats. Every record each way is kept in the order the recorder
//! saw it, one JSON line each (`{"dir": "in" | "out", "msg"}`, `in` being what went to pi), then
//! scrubbed: the scratch paths become `/work`, `/pi-agent`, `/home/user` and `/scratch`, the
//! canned model's address `http://canned`, every
//! UUID a stable placeholder, every time and duration zero, and the system prompt is not kept.
//! A fixture is a scripted exchange with a canned model, never a person's conversation.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::claude_mod::{Block, request, sse};
use crate::scrub::{Scrub, host};
use crate::tools::repo_root;

/// Where the fixtures go.
const FIXTURES: &str = "crates/slopty-agent/tests/fixtures/pi";
/// Slopty's gate, as the worker embeds it.
const GATE: &str = "crates/slopty-agent/assets/pi-gate/gate.ts";
/// The canned model, as `models.json` names it.
const PROVIDER: &str = "canned";
const MODEL: &str = "canned-1";
/// What the person says, in order.
const HELLO: &str = "Say hello.";
const MAKE: &str = "Make a file called made-by-pi.";
const STEER: &str = "Also say done.";
const REMOVE: &str = "Remove it.";
const AGAIN: &str = "Remove it again.";
/// Why the person denies the removal.
const REASON: &str = "Keep the file.";
/// The session's id, as the worker gives pi the id of the intent that started the thread.
const SESSION: &str = "5e55105e-0000-4000-8000-000000000001";
/// How long the recorder waits on any one step before it gives up.
const PATIENCE: Duration = Duration::from_secs(60);

/// The scratch directories of a run.
struct Scratch {
    root: PathBuf,
    home: PathBuf,
    agent: PathBuf,
    work: PathBuf,
    tmp: PathBuf,
}

impl Scratch {
    fn fresh() -> Result<Self> {
        // Outside the repository: pi reads the `AGENTS.md` files above its working directory,
        // and a fixture must hold none.
        let root = std::env::temp_dir().join(format!("slopty-pi-fixtures-{}", super::VERSION));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let dirs = ["home", "agent", "work", "tmp"];
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir))?;
        }
        // Scrubbing replaces the paths as pi writes them, which is with symlinks resolved.
        let root = root.canonicalize()?;
        Ok(Self {
            home: root.join("home"),
            agent: root.join("agent"),
            work: root.join("work"),
            tmp: root.join("tmp"),
            root,
        })
    }
}

pub fn record() -> Result<()> {
    let pi = super::official()?;
    let scratch = Scratch::fresh()?;
    let api = CannedApi::start(&scratch.root.join("api.jsonl"))?;
    write_models(&scratch.agent, api.port)?;
    let gate = scratch.root.join("gate.ts");
    std::fs::copy(repo_root()?.join(GATE), &gate).context("copy the gate")?;
    let mut rpc = Rpc::start(&pi, &scratch, &gate, SESSION)?;
    let scripted = script(&mut rpc);
    let (lines, finished) = rpc.finish();
    let mut raw = String::new();
    for line in &lines {
        raw.push_str(&serde_json::to_string(line)?);
        raw.push('\n');
    }
    std::fs::write(scratch.root.join("rpc.jsonl"), raw)?;
    println!("the run's records, API requests and pi's stderr are in {}", scratch.root.display());
    scripted?;
    finished?;
    ensure!(scratch.work.join("made-by-pi").is_file(), "the allowed command did not run");
    let paths = vec![
        (host()?, "host"),
        (format!("http://127.0.0.1:{}", api.port), "http://canned"),
        (scratch.work.display().to_string(), "/work"),
        // pi names a project's session directory by its path, its separators dashes.
        (dashed(&scratch.work), "work"),
        (scratch.agent.display().to_string(), "/pi-agent"),
        (scratch.home.display().to_string(), "/home/user"),
        (scratch.tmp.display().to_string(), "/tmp"),
        (scratch.root.display().to_string(), "/scratch"),
    ];
    let mut scrub = Scrub::new(paths, &DATED);
    let entries = entry_ids(&lines);
    let mut out = String::new();
    for line in lines {
        let line = unraced(renamed(unprompted(line), &entries));
        out.push_str(&serde_json::to_string(&scrub.value(line))?);
        out.push('\n');
    }
    let dir = repo_root()?.join(FIXTURES).into_std_path_buf();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("gate.jsonl");
    std::fs::write(&path, out).with_context(|| path.display().to_string())?;
    let recorded = json!({ "pi": super::VERSION });
    std::fs::write(dir.join("recorded.json"), format!("{}\n", serde_json::to_string(&recorded)?))?;
    println!("wrote {} from pi {}", path.display(), super::VERSION);
    Ok(())
}

/// The person's side of the run.
fn script(rpc: &mut Rpc) -> Result<()> {
    rpc.command("state", json!({ "type": "get_state" }))?;

    rpc.send(json!({ "id": "hello", "type": "prompt", "message": HELLO }))?;
    rpc.until(settled)?;

    rpc.send(json!({ "id": "make", "type": "prompt", "message": MAKE }))?;
    let ask = asked(&rpc.until(is_gate)?)?;
    rpc.command("steer", json!({ "type": "steer", "message": STEER }))?;
    rpc.send(json!({ "type": "extension_ui_response", "id": ask, "value": "allow" }))?;
    rpc.until(settled)?;

    rpc.send(json!({ "id": "remove", "type": "prompt", "message": REMOVE }))?;
    let ask = asked(&rpc.until(is_gate)?)?;
    let denied = format!("deny\n{REASON}");
    rpc.send(json!({ "type": "extension_ui_response", "id": ask, "value": denied }))?;
    rpc.until(settled)?;

    rpc.send(json!({ "id": "again", "type": "prompt", "message": AGAIN }))?;
    rpc.until(is_gate)?;
    // An abort is answered once pi is idle, and no `agent_settled` follows it.
    rpc.command("abort", json!({ "type": "abort" }))?;

    rpc.command("entries", json!({ "type": "get_entries" }))?;
    rpc.command("stats", json!({ "type": "get_session_stats" }))?;
    Ok(())
}

/// `path` as pi names a project's session directory after it: without its leading separator,
/// every other one a dash.
fn dashed(path: &Path) -> String {
    let text = path.display().to_string();
    text.trim_start_matches('/').replace(['/', '\\', ':'], "-")
}

/// One record of the fixture: which way it went, and what it was.
fn line(dir: &str, msg: Value) -> Value {
    let mut line = serde_json::Map::new();
    line.insert("dir".to_owned(), json!(dir));
    line.insert("msg".to_owned(), msg);
    Value::Object(line)
}

/// `value`'s field `key`, or null.
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn settled(msg: &Value) -> bool {
    field(msg, "type") == "agent_settled"
}

fn is_response(msg: &Value, id: &str) -> bool {
    field(msg, "type") == "response" && field(msg, "id") == id
}

/// Whether `msg` is the gate asking about a call.
fn is_gate(msg: &Value) -> bool {
    field(msg, "type") == "extension_ui_request"
        && field(msg, "method") == "select"
        && field(msg, "title")
            .as_str()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .is_some_and(|t| field(&t, "gate") == "slopty-gate/1")
}

fn asked(msg: &Value) -> Result<String> {
    field(msg, "id").as_str().map(str::to_owned).context("a gate request with no id")
}

/// pi over RPC, every record each way kept.
struct Rpc {
    child: Child,
    stdin: Option<ChildStdin>,
    heard: mpsc::Receiver<Result<Value>>,
    lines: Vec<Value>,
}

impl Rpc {
    fn start(pi: &super::Pi, scratch: &Scratch, gate: &Path, session: &str) -> Result<Self> {
        let node_dir = pi.program.parent().map(Path::to_path_buf).unwrap_or_default();
        let path = format!("{}:/usr/bin:/bin", node_dir.display());
        let stderr = std::fs::File::create(scratch.root.join("pi.stderr"))?;
        let mut child = pi
            .command()
            .args(["--mode", "rpc", "--extension"])
            .arg(gate)
            .args(["--session-id", session])
            .args(["--provider", PROVIDER, "--model", MODEL])
            .current_dir(&scratch.work)
            .env("PATH", path)
            .env("HOME", &scratch.home)
            .env("TMPDIR", &scratch.tmp)
            .env("PI_CODING_AGENT_DIR", &scratch.agent)
            .env("PI_OFFLINE", "1")
            .env("PI_SKIP_VERSION_CHECK", "1")
            .env("PI_TELEMETRY", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .context("start pi")?;
        let stdout = child.stdout.take().context("pi's stdout")?;
        let (tx, heard) = mpsc::channel();
        std::thread::spawn(move || {
            // Split on LF alone, as pi's framing asks: U+2028 inside a string is no boundary.
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => return,
                    Ok(_) => {
                        let record = line.strip_suffix(b"\n").unwrap_or(&line);
                        let record = record.strip_suffix(b"\r").unwrap_or(record);
                        let parsed = serde_json::from_slice(record).with_context(|| {
                            format!("pi wrote {}", String::from_utf8_lossy(record))
                        });
                        if tx.send(parsed).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _gone = tx.send(Err(e.into()));
                        return;
                    }
                }
            }
        });
        Ok(Self { stdin: child.stdin.take(), child, heard, lines: Vec::new() })
    }

    fn send(&mut self, msg: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("pi's stdin is closed")?;
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        self.lines.push(line("in", msg));
        Ok(())
    }

    fn next(&mut self) -> Result<Value> {
        let msg = match self.heard.recv_timeout(PATIENCE) {
            Ok(msg) => msg?,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("pi said nothing for {PATIENCE:?}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("pi closed its stdout"),
        };
        self.lines.push(line("out", msg.clone()));
        Ok(msg)
    }

    fn until(&mut self, done: impl Fn(&Value) -> bool) -> Result<Value> {
        loop {
            let msg = self.next()?;
            if field(&msg, "type") == "response" && field(&msg, "success") == false {
                bail!("pi refused a command: {msg}");
            }
            if done(&msg) {
                return Ok(msg);
            }
        }
    }

    /// Send `msg` as command `id` and wait for its response.
    fn command(&mut self, id: &str, mut msg: Value) -> Result<Value> {
        if let Value::Object(map) = &mut msg {
            map.insert("id".to_owned(), json!(id));
        }
        self.send(msg)?;
        self.until(|msg| is_response(msg, id))
    }

    /// Close pi's stdin, as an orderly shutdown asks, and take what it said until it exits.
    fn finish(mut self) -> (Vec<Value>, Result<()>) {
        drop(self.stdin.take());
        let mut heard = Ok(());
        while let Ok(msg) = self.heard.recv_timeout(PATIENCE) {
            match msg {
                Ok(msg) => self.lines.push(line("out", msg)),
                Err(e) => heard = Err(e),
            }
        }
        let exited = self.child.wait().map_err(anyhow::Error::from).and_then(|status| {
            ensure!(status.success(), "pi exited with {status}");
            Ok(())
        });
        (std::mem::take(&mut self.lines), heard.and(exited))
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        let _gone = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// The scratch `models.json`: the canned model as a provider of its own.
fn write_models(agent: &Path, port: u16) -> Result<()> {
    let models = json!({ "providers": { PROVIDER: {
        "baseUrl": format!("http://127.0.0.1:{port}"),
        "api": "anthropic-messages",
        "apiKey": "canned",
        "models": [{
            "id": MODEL,
            "name": "Canned",
            "reasoning": true,
            "input": ["text"],
            "contextWindow": 200_000,
            "maxTokens": 8192,
        }],
    } } });
    std::fs::write(agent.join("models.json"), serde_json::to_string_pretty(&models)?)?;
    Ok(())
}

/// A canned Messages API on loopback, every request kept in a log.
struct CannedApi {
    port: u16,
}

impl CannedApi {
    fn start(log: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let log = log.to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let log = log.clone();
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, &log) {
                        eprintln!("canned API: {e:#}");
                    }
                });
            }
        });
        Ok(Self { port })
    }
}

fn serve(stream: TcpStream, log: &Path) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let Some((line, body)) = request(&mut reader)? else { return Ok(()) };
    let body: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body)? };
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    writeln!(file, "{}", json!({ "line": line, "body": body }))?;
    let (kind, payload) = if line.starts_with("POST") && line.contains("/v1/messages") {
        let (blocks, stop) = answer(&body);
        ("text/event-stream", sse(&blocks, stop))
    } else {
        ("application/json", "{}".to_owned())
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        payload.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(payload.as_bytes())?;
    Ok(())
}

/// The canned model's answer to a request, by its last message: its blocks and stop reason.
fn answer(body: &Value) -> (Vec<Block>, &'static str) {
    let tools = field(body, "tools").as_array().is_some_and(|t| !t.is_empty());
    let last =
        field(body, "messages").as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
    let parts = match field(&last, "content") {
        Value::Array(parts) => parts.clone(),
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        _ => Vec::new(),
    };
    let text: String = parts.iter().filter_map(|p| field(p, "text").as_str()).collect();
    let results: Vec<&Value> = parts.iter().filter(|p| field(p, "type") == "tool_result").collect();
    let failed = results.iter().any(|r| field(r, "is_error") == true);
    let touch: &[&str] = &[r#"{"command""#, r#": "touch made-by-pi"}"#];
    let remove: &[&str] = &[r#"{"command""#, r#": "rm made-by-pi"}"#];
    if !tools {
        // pi's own side requests, if any.
        (vec![Block::Text(&["Fake", " title"])], "end_turn")
    } else if text.contains(STEER) {
        (vec![Block::Text(&["Made it", ", and done."])], "end_turn")
    } else if !results.is_empty() && failed {
        (vec![Block::Text(&["I left", " it."])], "end_turn")
    } else if !results.is_empty() {
        (vec![Block::Text(&["Made", " it."])], "end_turn")
    } else if text.contains(AGAIN) {
        (vec![Block::Tool { id: "toolu_pi3", name: "bash", input: remove }], "tool_use")
    } else if text.contains(REMOVE) {
        let call = Block::Tool { id: "toolu_pi2", name: "bash", input: remove };
        (vec![Block::Text(&["Removing", " it."]), call], "tool_use")
    } else if text.contains(MAKE) {
        let call = Block::Tool { id: "toolu_pi1", name: "bash", input: touch };
        (vec![Block::Text(&["Let me ", "make it."]), call], "tool_use")
    } else if text.contains(HELLO) {
        let thinking = Block::Thinking(&["The person ", "wants a greeting."]);
        (vec![thinking, Block::Text(&["Hello", " there."])], "end_turn")
    } else {
        (vec![Block::Text(&["I have no answer for that."])], "end_turn")
    }
}

/// `record` without pi's system prompt and tool declarations, which hold this machine's date and
/// pi's own wording, and which Slopty does not read.
fn unprompted(record: Value) -> Value {
    match record {
        Value::Object(mut map) => {
            if map.get("role").and_then(Value::as_str) == Some("system") {
                if let Some(Value::Object(sections)) = map.get_mut("sections") {
                    for text in sections.values_mut() {
                        if text.is_string() {
                            *text = json!("(not kept)");
                        }
                    }
                }
                for key in ["toolsAdded", "toolsRemoved"] {
                    if let Some(Value::Array(tools)) = map.get_mut(key) {
                        for tool in tools.iter_mut() {
                            *tool = json!({ "name": field(tool, "name").clone() });
                        }
                    }
                }
            }
            Value::Object(map.into_iter().map(|(k, v)| (k, unprompted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(unprompted).collect()),
        other => other,
    }
}

/// A stand-in for each session entry's random id, numbered in the order the session holds them.
fn entry_ids(lines: &[Value]) -> HashMap<String, String> {
    let mut ids = HashMap::new();
    for line in lines {
        let msg = field(line, "msg");
        if field(msg, "command") != "get_entries" {
            continue;
        }
        let entries = field(field(msg, "data"), "entries").as_array().cloned().unwrap_or_default();
        for entry in entries {
            if let Some(id) = field(&entry, "id").as_str() {
                let next = ids.len().saturating_add(1);
                ids.entry(id.to_owned()).or_insert_with(|| format!("{next:08x}"));
            }
        }
    }
    ids
}

/// `value` with every string that is an entry's id given its stand-in.
fn renamed(value: Value, ids: &HashMap<String, String>) -> Value {
    match value {
        Value::String(text) => Value::String(ids.get(&text).cloned().unwrap_or(text)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| renamed(v, ids)).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, renamed(v, ids))).collect())
        }
        other => other,
    }
}

/// `line` without what pi raced into it.
///
/// pi writes `message_start` from the message it goes on filling, and each `message_update`'s
/// usage from the usage it goes on adding to, so those records hold none, some or all of what
/// came after them, by timing. Slopty builds a message from its deltas and its `message_end`,
/// and takes usage from the end alone, so the recording keeps the start as pi begins it and the
/// updates without their usage.
fn unraced(mut line: Value) -> Value {
    let Some(msg) = line.get_mut("msg") else { return line };
    if field(msg, "type") == "message_update"
        && let Value::Object(update) = msg
    {
        update.remove("usage");
        return line;
    }
    if field(msg, "type") != "message_start" {
        return line;
    }
    if let Some(Value::Object(message)) = msg.get_mut("message")
        && message.get("role").and_then(Value::as_str) == Some("assistant")
    {
        message.insert("content".to_owned(), json!([]));
        message.remove("responseId");
        message.remove("responseModel");
        message.insert("usage".to_owned(), json!(null));
    }
    line
}

/// The dates pi writes, `9` standing for a digit: entry timestamps
/// (`2026-10-02T06:29:36.123Z`) and session file names (`2026-10-02T04-13-49-083Z_<id>.jsonl`).
const DATED: [&str; 2] = ["9999-99-99T99:99:99.999Z", "9999-99-99T99-99-99-999Z_"];
