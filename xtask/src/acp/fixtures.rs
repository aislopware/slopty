//! `cargo xtask acp fixtures`: what the pinned `OpenCode` says over the Agent Client Protocol,
//! driven as Slopty's ACP adapter drives an agent, recorded into
//! `crates/slopty-agent/tests/fixtures/acp/`.
//!
//! The build ([`super::official`]) runs as the worker runs an ACP agent: `opencode acp` on stdio,
//! in its working directory. It runs in a scratch home with nothing signed in, its model list not
//! fetched, its own providers off, and an environment that holds nothing of this one. Its model
//! is a canned Messages API on loopback, set in the scratch `opencode.json` as a provider of its
//! own, and every tool asks first. The recorder plays the worker, sending what the adapter sends
//! and refusing what it refuses (the agent's file system requests), and the person:
//!
//! 1. `turns.jsonl`, one process: a first prompt answered with thinking and text; a prompt whose
//!    answer writes a file, allowed once; a prompt whose command is rejected; a prompt whose
//!    command is cancelled while it is asked about;
//! 2. `load.jsonl`, a second process: the session loaded again, replayed by the agent, and one more
//!    prompt.
//!
//! Every message each way is kept in the order the recorder saw it, one JSON line each
//! (`{"dir": "in" | "out", "msg"}`, `in` being what went to the agent), then scrubbed: the scratch
//! paths become `/work`, `/home/user` and `/scratch` (and `work` where `OpenCode` writes a path
//! without its leading separator), `OpenCode`'s time-ordered ids (`ses_`, `msg_`, `prt_`) stable
//! placeholders numbered in order of appearance across both files, every UUID a placeholder and
//! every time zero. `auth.jsonl` is not recorded: it is an agent that wants a sign-in, which a
//! recording with nothing signed in cannot reach, written by hand to the schema. A fixture is a
//! scripted exchange with a canned model, never a person's conversation.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::claude_mod::{Block, request, sse};
use crate::scrub::{Scrub, host};
use crate::tools::repo_root;

/// Where the fixtures go.
const FIXTURES: &str = "crates/slopty-agent/tests/fixtures/acp";
/// The canned model, as `opencode.json` names it.
const PROVIDER: &str = "canned";
const MODEL: &str = "canned-1";
/// What the person says, in order.
const HELLO: &str = "Say hello.";
const MAKE: &str = "Make a file called made-by-acp.";
const REMOVE: &str = "Remove it.";
const AGAIN: &str = "Remove it again.";
const HELLO_AGAIN: &str = "Say hello again.";
/// The file the allowed call writes.
const MADE: &str = "made-by-acp";
/// `OpenCode`'s ids, each a prefix and 26 letters and digits that begin with the time.
const NATIVE: [&str; 3] = ["ses_", "msg_", "prt_"];
const NATIVE_LEN: usize = 26;
/// How long the recorder waits on any one step before it gives up.
const PATIENCE: Duration = Duration::from_secs(60);

/// The scratch directories of a run.
struct Scratch {
    root: PathBuf,
    home: PathBuf,
    work: PathBuf,
    tmp: PathBuf,
}

impl Scratch {
    fn fresh() -> Result<Self> {
        // Outside the repository: OpenCode reads the `AGENTS.md` files and the git state above
        // its working directory, and a fixture must hold neither.
        let root = std::env::temp_dir().join(format!("slopty-acp-fixtures-{}", super::VERSION));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        for dir in ["home", "work", "tmp"] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        // Scrubbing replaces the paths as OpenCode writes them, which is with symlinks resolved.
        let root = root.canonicalize()?;
        Ok(Self { home: root.join("home"), work: root.join("work"), tmp: root.join("tmp"), root })
    }
}

/// How the person answers the call a turn asks about.
#[derive(Clone, Copy)]
enum Choice {
    /// No call is asked about.
    Unasked,
    /// The option of `kind` (`allow_once`, `reject_once`, …).
    Pick(&'static str),
    /// The turn is cancelled while the call is asked about.
    Cancel,
}

pub fn record() -> Result<()> {
    let opencode = super::official()?;
    let scratch = Scratch::fresh()?;
    let api = CannedApi::start(&scratch.root.join("api.jsonl"))?;
    write_config(&scratch.home, api.port)?;

    let mut acp = Acp::start(&opencode, &scratch, "opencode.stderr")?;
    let opened = turns(&mut acp, &scratch.work);
    let (turns, finished) = acp.finish();
    keep(&scratch.root.join("turns.raw.jsonl"), &turns)?;
    let session = opened?;
    finished?;
    ensure!(scratch.work.join(MADE).is_file(), "the allowed call did not write {MADE}");

    let mut acp = Acp::start(&opencode, &scratch, "opencode-load.stderr")?;
    let loaded = load(&mut acp, &scratch.work, &session);
    let (load, finished) = acp.finish();
    keep(&scratch.root.join("load.raw.jsonl"), &load)?;
    println!(
        "the run's messages, API requests and OpenCode's stderr are in {}",
        scratch.root.display()
    );
    loaded?;
    finished?;
    ensure!(scratch.work.join(MADE).is_file(), "a refused call removed {MADE}");

    let work = scratch.work.display().to_string();
    let paths = vec![
        (host()?, "host"),
        (format!("http://127.0.0.1:{}", api.port), "http://canned"),
        (work.clone(), "/work"),
        // OpenCode titles a write by its path without the leading separator.
        (work.trim_start_matches('/').to_owned(), "work"),
        (scratch.home.display().to_string(), "/home/user"),
        (scratch.tmp.display().to_string(), "/tmp"),
        (scratch.root.display().to_string(), "/scratch"),
    ];
    let mut scrub = Scrub::new(paths, &[]);
    let mut ids = HashMap::new();
    let dir = repo_root()?.join(FIXTURES).into_std_path_buf();
    std::fs::create_dir_all(&dir)?;
    for (name, lines) in [("turns.jsonl", turns), ("load.jsonl", load)] {
        let mut out = String::new();
        for line in lines {
            let line = scrub.value(renamed(line, &mut ids));
            out.push_str(&serde_json::to_string(&line)?);
            out.push('\n');
        }
        let path = dir.join(name);
        std::fs::write(&path, out).with_context(|| path.display().to_string())?;
        println!("wrote {} from OpenCode {}", path.display(), super::VERSION);
    }
    let recorded = json!({ "opencode": super::VERSION });
    std::fs::write(dir.join("recorded.json"), format!("{}\n", serde_json::to_string(&recorded)?))?;
    Ok(())
}

/// The first process: a new session and its four turns. Its session id.
fn turns(acp: &mut Acp, work: &Path) -> Result<String> {
    acp.call("initialize", &initialize(), Choice::Unasked)?;
    let new = json!({ "cwd": work.display().to_string(), "mcpServers": [] });
    let opened = acp.call("session/new", &new, Choice::Unasked)?;
    let session =
        field(&opened, "sessionId").as_str().context("a new session with no id")?.to_owned();
    acp.session.clone_from(&session);
    acp.prompt(HELLO, Choice::Unasked)?;
    acp.prompt(MAKE, Choice::Pick("allow_once"))?;
    acp.prompt(REMOVE, Choice::Pick("reject_once"))?;
    let stopped = acp.prompt(AGAIN, Choice::Cancel)?;
    ensure!(field(&stopped, "stopReason") == "cancelled", "a cancelled turn ended {stopped}");
    Ok(session)
}

/// The second process: the session loaded and prompted again.
fn load(acp: &mut Acp, work: &Path, session: &str) -> Result<()> {
    acp.call("initialize", &initialize(), Choice::Unasked)?;
    let load = json!({ "sessionId": session, "cwd": work.display().to_string(), "mcpServers": [] });
    acp.call("session/load", &load, Choice::Unasked)?;
    session.clone_into(&mut acp.session);
    acp.prompt(HELLO_AGAIN, Choice::Unasked)?;
    Ok(())
}

/// What Slopty's adapter says it is and can do: no file system, no terminal.
fn initialize() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "auth": { "terminal": false },
        },
        "clientInfo": { "name": "slopty", "title": "Slopty", "version": env!("CARGO_PKG_VERSION") },
    })
}

fn keep(path: &Path, lines: &[Value]) -> Result<()> {
    let mut raw = String::new();
    for line in lines {
        raw.push_str(&serde_json::to_string(line)?);
        raw.push('\n');
    }
    std::fs::write(path, raw)?;
    Ok(())
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

/// `opencode acp` over stdio, every message each way kept.
struct Acp {
    child: Child,
    stdin: Option<ChildStdin>,
    heard: mpsc::Receiver<Result<Value>>,
    lines: Vec<Value>,
    next_id: u64,
    session: String,
}

impl Acp {
    fn start(opencode: &Path, scratch: &Scratch, stderr: &str) -> Result<Self> {
        let stderr = std::fs::File::create(scratch.root.join(stderr))?;
        let home = &scratch.home;
        let mut child = Command::new(opencode)
            .arg("acp")
            .current_dir(&scratch.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("TMPDIR", &scratch.tmp)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .env("OPENCODE_DISABLE_MODELS_FETCH", "1")
            .env("OPENCODE_DISABLE_LSP_DOWNLOAD", "1")
            .env("OPENCODE_DISABLE_SHARE", "1")
            .env("OPENCODE_DISABLE_DEFAULT_PLUGINS", "1")
            .env("OPENCODE_DISABLE_CLAUDE_CODE", "1")
            .env("OPENCODE_DISABLE_EXTERNAL_SKILLS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .context("start opencode acp")?;
        let stdout = child.stdout.take().context("OpenCode's stdout")?;
        let (tx, heard) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let parsed = line.map_err(anyhow::Error::from).and_then(|line| {
                    serde_json::from_str(&line).with_context(|| format!("OpenCode wrote {line}"))
                });
                if tx.send(parsed).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            stdin: child.stdin.take(),
            child,
            heard,
            lines: Vec::new(),
            next_id: 0,
            session: String::new(),
        })
    }

    fn send(&mut self, msg: Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("OpenCode's stdin is closed")?;
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        self.lines.push(line("in", msg));
        Ok(())
    }

    fn next(&mut self) -> Result<Value> {
        let msg = match self.heard.recv_timeout(PATIENCE) {
            Ok(msg) => msg?,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("OpenCode said nothing for {PATIENCE:?}"),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("OpenCode closed its stdout"),
        };
        self.lines.push(line("out", msg.clone()));
        Ok(msg)
    }

    fn prompt(&mut self, text: &str, choice: Choice) -> Result<Value> {
        let prompt =
            json!({ "sessionId": self.session, "prompt": [{ "type": "text", "text": text }] });
        self.call("session/prompt", &prompt, choice)
    }

    /// Ask `method` and wait for its answer, answering what the agent asks meanwhile: a
    /// permission by `choice`, anything else refused as the worker refuses it.
    fn call(&mut self, method: &str, params: &Value, choice: Choice) -> Result<Value> {
        self.next_id = self.next_id.saturating_add(1);
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let msg = self.next()?;
            match (field(&msg, "method").as_str(), msg.get("id")) {
                (Some("session/request_permission"), Some(asked)) => {
                    let asked = asked.clone();
                    self.permission(&msg, &asked, choice)?;
                }
                (Some(_), Some(asked)) => {
                    let refused = json!({ "jsonrpc": "2.0", "id": asked,
                        "error": { "code": -32_601, "message": "Method not found" } });
                    self.send(refused)?;
                }
                (None, Some(answered)) if *answered == json!(id) => {
                    if let Some(error) = msg.get("error") {
                        bail!("OpenCode refused {method}: {error}");
                    }
                    return Ok(field(&msg, "result").clone());
                }
                _ => {}
            }
        }
    }

    fn permission(&mut self, asked: &Value, id: &Value, choice: Choice) -> Result<()> {
        let kind = match choice {
            Choice::Unasked => bail!("OpenCode asked about a call no one meant: {asked}"),
            Choice::Cancel => {
                // As the adapter cancels: the turn first, then the open request answered.
                let session = json!({ "sessionId": self.session });
                self.send(
                    json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": session }),
                )?;
                let cancelled = json!({ "outcome": { "outcome": "cancelled" } });
                return self.send(json!({ "jsonrpc": "2.0", "id": id, "result": cancelled }));
            }
            Choice::Pick(kind) => kind,
        };
        let options =
            field(field(asked, "params"), "options").as_array().cloned().unwrap_or_default();
        let option = options
            .iter()
            .find(|o| field(o, "kind") == kind)
            .and_then(|o| field(o, "optionId").as_str())
            .with_context(|| format!("no {kind} option in {asked}"))?;
        let picked = json!({ "outcome": { "outcome": "selected", "optionId": option } });
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": picked }))
    }

    /// Close `OpenCode`'s stdin, as an orderly shutdown asks, and take what it said until it exits.
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
            ensure!(status.success(), "OpenCode exited with {status}");
            Ok(())
        });
        (std::mem::take(&mut self.lines), heard.and(exited))
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        let _gone = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// The scratch `opencode.json`: the canned model as a provider of its own and the only one, no
/// sharing or updates, and every tool asking first.
fn write_config(home: &Path, port: u16) -> Result<()> {
    let config = json!({
        "$schema": "https://opencode.ai/config.json",
        "autoupdate": false,
        "share": "disabled",
        "disabled_providers": ["opencode"],
        "provider": { PROVIDER: {
            "npm": "@ai-sdk/anthropic",
            "name": "Canned",
            "options": { "baseURL": format!("http://127.0.0.1:{port}/v1"), "apiKey": "canned" },
            "models": { MODEL: {
                "name": "Canned",
                "cost": { "input": 1000, "output": 1000 },
                "limit": { "context": 200_000, "output": 8192 },
            } },
        } },
        "model": format!("{PROVIDER}/{MODEL}"),
        "permission": { "edit": "ask", "bash": "ask", "webfetch": "ask" },
    });
    let dir = home.join(".config/opencode");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("opencode.json"), serde_json::to_string_pretty(&config)?)?;
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
    while let Some((line, body)) = request(&mut reader)? {
        let body: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(&body)? };
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        writeln!(file, "{}", json!({ "line": line, "body": body }))?;
        let (kind, payload) = if line.starts_with("POST") && line.contains("/messages") {
            let (blocks, stop) = answer(&body);
            ("text/event-stream", sse(&blocks, stop))
        } else {
            ("application/json", "{}".to_owned())
        };
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\r\n",
            payload.len()
        );
        writer.write_all(head.as_bytes())?;
        writer.write_all(payload.as_bytes())?;
    }
    Ok(())
}

/// The canned model's answer to a request, by its last message: its blocks and stop reason. The
/// person's words come first, since `OpenCode` may send them beside a call's result.
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
    let result = parts
        .iter()
        .rev()
        .find(|p| field(p, "type") == "tool_result")
        .and_then(|p| field(p, "tool_use_id").as_str());
    let write: &[&str] = &[r#"{"filePath": "made-by-acp", "#, r#""content": "made by acp\n"}"#];
    let remove: &[&str] = &[r#"{"command": "rm made-by-acp", "#, r#""description": "Remove it"}"#];
    if !tools {
        // OpenCode's own side requests: a session's title.
        (vec![Block::Text(&["A greeting"])], "end_turn")
    } else if text.contains(HELLO_AGAIN) {
        (vec![Block::Text(&["Hello", " again."])], "end_turn")
    } else if text.contains(HELLO) {
        let thinking = Block::Thinking(&["The person ", "greets me."]);
        (vec![thinking, Block::Text(&["Hello", ", there."])], "end_turn")
    } else if text.contains(MAKE) {
        let call = Block::Tool { id: "toolu_1", name: "write", input: write };
        (vec![Block::Text(&["I will ", "write it."]), call], "tool_use")
    } else if text.contains(AGAIN) {
        (vec![Block::Tool { id: "toolu_3", name: "bash", input: remove }], "tool_use")
    } else if text.contains(REMOVE) {
        (vec![Block::Tool { id: "toolu_2", name: "bash", input: remove }], "tool_use")
    } else if result == Some("toolu_1") {
        (vec![Block::Text(&["Made", " it."])], "end_turn")
    } else if result.is_some() {
        (vec![Block::Text(&["I left", " it."])], "end_turn")
    } else {
        (vec![Block::Text(&["I have no answer for that."])], "end_turn")
    }
}

/// `value` with each of `OpenCode`'s ids given a stand-in of its kind, numbered in order of first
/// appearance.
fn renamed(value: Value, ids: &mut HashMap<String, String>) -> Value {
    match value {
        Value::String(text) => Value::String(renamed_text(&text, ids)),
        Value::Array(items) => Value::Array(items.into_iter().map(|v| renamed(v, ids)).collect()),
        Value::Object(map) => {
            Value::Object(map.into_iter().map(|(k, v)| (k, renamed(v, ids))).collect())
        }
        other => other,
    }
}

fn renamed_text(text: &str, ids: &mut HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((at, prefix)) = native_at(rest) {
        let (before, found) = rest.split_at(at);
        let end = prefix.len().saturating_add(NATIVE_LEN);
        let (id, after) = found.split_at(end);
        out.push_str(before);
        let next = ids.keys().filter(|k| k.starts_with(prefix)).count().saturating_add(1);
        let stand_in =
            ids.entry(id.to_owned()).or_insert_with(|| format!("{prefix}{next:0NATIVE_LEN$}"));
        out.push_str(stand_in);
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Where the first of `OpenCode`'s ids in `text` starts, and its prefix.
fn native_at(text: &str) -> Option<(usize, &'static str)> {
    let bytes = text.as_bytes();
    NATIVE
        .iter()
        .filter_map(|prefix| {
            text.match_indices(prefix)
                .map(|(at, _)| at)
                .find(|&at| {
                    let start = at.saturating_add(prefix.len());
                    let id = bytes.get(start..start.saturating_add(NATIVE_LEN));
                    let ends = bytes
                        .get(start.saturating_add(NATIVE_LEN))
                        .is_none_or(|b| !b.is_ascii_alphanumeric());
                    id.is_some_and(|id| id.iter().all(u8::is_ascii_alphanumeric)) && ends
                })
                .map(|at| (at, *prefix))
        })
        .min_by_key(|(at, _)| *at)
}
