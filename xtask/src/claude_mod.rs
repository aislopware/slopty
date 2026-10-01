//! `cargo xtask fixtures claude-mod`: check Slopty's Claude Code mod against the official build
//! and record what it sends (`crates/slopty-agent/tests/fixtures/mod`).
//!
//! The mod (`crates/slopty-agent/assets/claude-mod`) first goes through `claude plugin validate
//! --strict`. Then each scenario runs the official build ([`crate::claude::official`]) headless
//! with the mod loaded, the way the worker starts an agent: function hooks on, the
//! nonessential-traffic switch set empty, `SLOPTY_MOD_SOCKET` and `SLOPTY_SESSION` set. It runs
//! in a scratch home, against a canned Messages API on loopback ([`FakeApi`]) that answers each
//! scenario's prompt with scripted streams (text, thinking, a tool call, a subagent), so no
//! account and no model are involved. What the mod posts is taken on a Unix socket of the
//! recorder's own ([`sink`]); the transcripts come from the scratch home.
//!
//! Everything written is scrubbed as `fixtures claude` scrubs. Beside the scenarios go the
//! recorded Claude Code version (`recorded.json`) and a copy of the mod as recorded
//! (`plugin/`), which `slopty-agent`'s tests hold to `MOD_CLAUDE_VERSIONS` and to the embedded
//! files: a new Claude Code, or an edited mod, is trusted only after a new recording.

use std::fmt::Write as _;
use std::io::{BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Map, Value, json};

use crate::claude;
use crate::fixtures::Scrub;
use crate::tools::repo_root;

/// One scripted run: the prompt picks the canned answers.
struct Scenario {
    name: &'static str,
    prompt: &'static str,
}

const SCENARIOS: [Scenario; 3] = [
    // An answer, a Bash call, its result and a closing answer.
    Scenario { name: "bash", prompt: "scenario:bash run echo hi" },
    // Thinking before the answer.
    Scenario { name: "think", prompt: "scenario:think say hello" },
    // An Agent call whose subagent answers in its own thread, and the task notification after.
    Scenario { name: "agent", prompt: "scenario:agent look around" },
];

/// The terminal session the runs claim to be in.
const SESSION: &str = "5e55105e-0000-4000-8000-000000000001";

/// How long one scenario may run.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the sink waits for the mod's last request after `claude` exits.
const DRAIN: Duration = Duration::from_millis(500);

pub fn capture_all(only: Option<&str>) -> Result<()> {
    let claude = claude::official()?;
    println!("claude {} at {}", claude::VERSION, claude.display());
    let root = repo_root()?.into_std_path_buf();
    let plugin = root.join("crates/slopty-agent/assets/claude-mod");
    validate(&claude, &plugin)?;
    let out = root.join("crates/slopty-agent/tests/fixtures/mod");
    let api = FakeApi::start()?;
    let mut ran = 0_usize;
    for scenario in SCENARIOS.iter().filter(|s| only.is_none_or(|name| name == s.name)) {
        let started = Instant::now();
        println!("▶ {}", scenario.name);
        capture(&claude, &plugin, api.port, scenario, &out)
            .with_context(|| format!("scenario {}", scenario.name))?;
        println!("  ✓ {} ({:.1?})", scenario.name, started.elapsed());
        ran = ran.saturating_add(1);
    }
    ensure!(ran > 0, "no scenario is named {only:?}");
    record_plugin(&plugin, &out)?;
    let recorded = json!({ "claude": claude::VERSION });
    std::fs::write(out.join("recorded.json"), format!("{recorded:#}\n"))?;
    Ok(())
}

/// `claude plugin validate --strict` on the mod, in a scratch home.
fn validate(claude: &Path, plugin: &Path) -> Result<()> {
    let home = scratch("validate")?;
    let out = Command::new(claude)
        .args(["plugin", "validate", "--strict"])
        .arg(plugin)
        .env_clear()
        .envs([("PATH", "/usr/bin:/bin"), ("TMPDIR", "/tmp")])
        .env("HOME", &home)
        .output()
        .context("run claude plugin validate")?;
    let said = String::from_utf8_lossy(&out.stdout);
    ensure!(out.status.success(), "claude plugin validate --strict failed:\n{said}");
    println!("✓ claude plugin validate --strict");
    std::fs::remove_dir_all(&home)?;
    Ok(())
}

/// A fresh directory under `/tmp`.
fn scratch(name: &str) -> Result<PathBuf> {
    let dir = PathBuf::from(format!("/tmp/slopty-mod-fixture-{name}"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn capture(claude: &Path, plugin: &Path, port: u16, scenario: &Scenario, out: &Path) -> Result<()> {
    let work = scratch(scenario.name)?;
    let home = scratch(&format!("{}-home", scenario.name))?;
    let socket = home.join("mod.sock");
    let posted = sink(&socket)?;
    let mut child = Command::new(claude)
        .args(["-p", scenario.prompt, "--model", "haiku"])
        .arg(format!("--plugin-dir={}", plugin.display()))
        .args(["--allowedTools", "Bash Agent", "--setting-sources", ""])
        .current_dir(&work)
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", "/tmp"),
            ("ANTHROPIC_API_KEY", "sk-ant-fixture"),
            ("CLAUDE_CODE_ENABLE_FUNCTION_HOOKS", "1"),
            // As the worker starts an agent: set, and empty.
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", ""),
            ("DISABLE_TELEMETRY", "1"),
            ("DISABLE_ERROR_REPORTING", "1"),
            ("DISABLE_AUTOUPDATER", "1"),
            ("SLOPTY_SESSION", SESSION),
        ])
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("HOME", &home)
        .env("SLOPTY_MOD_SOCKET", &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn claude")?;
    let pid = child.id();
    let (exited, status) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = exited.send(child.wait());
    });
    let Ok(status) = status.recv_timeout(SCENARIO_TIMEOUT) else {
        let _killed = Command::new("/bin/kill").arg(pid.to_string()).status();
        bail!("timed out");
    };
    let status = status?;
    ensure!(status.success(), "claude exited {status}");
    let mut batches = Vec::new();
    while let Ok(batch) = posted.recv_timeout(DRAIN) {
        batches.push(batch);
    }
    ensure!(!batches.is_empty(), "the mod posted nothing");
    let kinds: Vec<&str> = batches
        .iter()
        .filter_map(|b| b.get("events").and_then(Value::as_array))
        .flatten()
        .filter_map(|e| e.get("kind").and_then(Value::as_str))
        .collect();
    ensure!(kinds.first() == Some(&"hello"), "the mod did not say hello first: {kinds:?}");
    ensure!(kinds.last() == Some(&"bye"), "the mod did not say bye last: {kinds:?}");
    let (main, subagents) = transcripts(&home)?;
    write(scenario, [plugin, &work, &home], &main, &subagents, batches, out)?;
    std::fs::remove_dir_all(&work)?;
    std::fs::remove_dir_all(&home)?;
    Ok(())
}

/// The session's transcript and its subagents', from the scratch home.
fn transcripts(home: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut dirs = vec![home.join(".claude/projects")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push(path);
            }
        }
    }
    let (mut subagents, main): (Vec<_>, Vec<_>) =
        files.into_iter().partition(|p| p.components().any(|c| c.as_os_str() == "subagents"));
    let [main] = <[PathBuf; 1]>::try_from(main)
        .map_err(|found| anyhow::anyhow!("expected one main transcript, found {found:?}"))?;
    subagents.sort();
    Ok((main, subagents))
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    text.lines().map(|line| Ok(serde_json::from_str(line)?)).collect()
}

/// `places` are the mod, the scratch directory and the scratch home, in that order.
fn write(
    scenario: &Scenario,
    [plugin, work, home]: [&Path; 3],
    main: &Path,
    subagents: &[PathBuf],
    batches: Vec<Value>,
    out: &Path,
) -> Result<()> {
    let mut scrub = Scrub::with_places(
        vec![(plugin.to_string_lossy().into_owned(), "/plugin".to_owned())],
        work,
        &home.to_string_lossy(),
    )?;
    let main = read_jsonl(main)?;
    let subagents: Vec<(PathBuf, Vec<Value>)> =
        subagents.iter().map(|p| Ok((p.clone(), read_jsonl(p)?))).collect::<Result<_>>()?;
    for record in main.iter().chain(subagents.iter().flat_map(|(_, r)| r)).chain(&batches) {
        scrub.collect(record);
    }
    let dir = out.join(scenario.name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let lines = |records: Vec<Value>, scrub: &mut Scrub, whole: bool| -> String {
        let lines: Vec<String> = records
            .into_iter()
            .map(|r| sorted(if whole { scrub.value(r, "") } else { scrub.record(r) }).to_string())
            .collect();
        format!("{}\n", lines.join("\n"))
    };
    std::fs::write(dir.join("transcript.jsonl"), lines(main, &mut scrub, false))?;
    for (path, records) in subagents {
        let name = path.file_name().context("a subagent file")?.to_string_lossy().into_owned();
        std::fs::create_dir_all(dir.join("subagents"))?;
        let text = lines(records, &mut scrub, false);
        std::fs::write(dir.join("subagents").join(scrub.text(&name)), text)?;
    }
    std::fs::write(dir.join("events.jsonl"), lines(batches, &mut scrub, true))?;
    Ok(())
}

/// Keys in order, whatever the map keeps: a recording diffs only where the run did.
fn sorted(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut pairs: Vec<(String, Value)> = map.into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(pairs.into_iter().map(|(k, v)| (k, sorted(v))).collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

/// The mod as recorded, beside the recording.
fn record_plugin(plugin: &Path, out: &Path) -> Result<()> {
    let copy = out.join("plugin");
    if copy.exists() {
        std::fs::remove_dir_all(&copy)?;
    }
    for path in [".claude-plugin/plugin.json", "hooks/hooks.json", "hooks/register.ts"] {
        let to = copy.join(path);
        std::fs::create_dir_all(to.parent().context("a parent")?)?;
        std::fs::copy(plugin.join(path), to)?;
    }
    Ok(())
}

/// Serve the mod's socket at `path`: every `POST` body, in order, on the channel returned,
/// each answered `204` on a kept-alive connection.
fn sink(path: &Path) -> Result<mpsc::Receiver<Value>> {
    let listener = UnixListener::bind(path).with_context(|| format!("bind {}", path.display()))?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let tx = tx.clone();
            std::thread::spawn(move || {
                let _served = serve_sink(stream, &tx);
            });
        }
    });
    Ok(rx)
}

fn serve_sink(stream: UnixStream, tx: &mpsc::Sender<Value>) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some((_, body)) = request(&mut reader)? {
        tx.send(serde_json::from_slice(&body)?)?;
        writer.write_all(b"HTTP/1.1 204 No Content\r\n\r\n")?;
    }
    Ok(())
}

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
    writer.write_all(payload.as_bytes())?;
    Ok(())
}

/// A content block the fake model writes, in pieces.
enum Block {
    Text(&'static [&'static str]),
    Thinking(&'static [&'static str]),
    Tool { id: &'static str, name: &'static str, input: &'static [&'static str] },
}

/// The fake model's answer to a request: its blocks and its stop reason.
fn answer(body: &Value) -> (Vec<Block>, &'static str) {
    let messages = body.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
    let text_of = |m: &Value| match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect()
        }
        _ => String::new(),
    };
    let first = messages.first().map(text_of).unwrap_or_default();
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
    } else if first.contains("scenario:think") {
        let thinking = Block::Thinking(&["The person ", "wants a greeting."]);
        (vec![thinking, Block::Text(&["Hello", " there."])], "end_turn")
    } else if first.contains("scenario:background") {
        let input: &[&str] = &[
            r#"{"command": "sleep 3; echo woke", "#,
            r#""description": "Sleep then print a marker", "#,
            r#""run_in_background": true}"#,
        ];
        (vec![Block::Tool { id: "toolu_fake3", name: "Bash", input }], "tool_use")
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

fn usage() -> Value {
    json!({ "input_tokens": 12, "output_tokens": 20 })
}

/// The answer as a stream of server-sent events.
fn sse(blocks: &[Block], stop: &str) -> String {
    let mut events = vec![json!({ "type": "message_start", "message": {
        "id": "msg_fake", "type": "message", "role": "assistant", "model": MODEL,
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
    json!({ "id": "msg_fake", "type": "message", "role": "assistant", "model": MODEL,
        "content": content, "stop_reason": stop, "usage": usage() })
}
