//! `cargo xtask codex fixtures`: what the pinned Codex app-server says to two clients of one
//! thread, recorded into `crates/slopty-agent/tests/fixtures/codex/`.
//!
//! The official build ([`super::official`]) runs as `codex app-server --listen unix://…` on its
//! control socket, in a scratch `CODEX_HOME` with nothing signed in. Its model is a canned
//! Responses API on loopback, set as a custom provider the way Codex's own app-server tests set
//! one: it asks to run one shell command outside the sandbox, and once the command's output comes
//! back it answers in one line. Two clients join over the socket's WebSocket, as Slopty and a Codex
//! TUI would: the first starts the thread and the turn, the second resumes the thread to follow it
//! live. The approval the command needs then shows who is asked, what a second answer to one
//! request gets, and how each client hears that it was settled.
//!
//! Every frame either client sent or heard is kept in the order the recorder saw it, one JSON
//! line each (`{"client", "dir", "msg"}`), scrubbed: the scratch paths become `/work`,
//! `/codex-home` and `/home/user`, every UUID a stable placeholder numbered in order of
//! appearance, and every time and duration zero. A fixture is a scripted exchange with a canned
//! model, never a person's conversation.

use std::collections::HashMap;
use std::io::{BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::tools::repo_root;

/// Where the fixtures go.
const FIXTURES: &str = "crates/slopty-agent/tests/fixtures/codex";
/// The command the canned model asks to run, outside the read-only sandbox, so the
/// `on-request` approval policy asks the person first.
const COMMAND: &str = "touch made-by-codex";
/// How the canned model asks to leave the sandbox, and why.
const ESCALATED: &str = "require_escalated";
const WHY: &str = "It writes a file in the read-only workspace.";
/// What the person asks for.
const PROMPT: &str = "Make a file called made-by-codex.";
/// The first turn, which writes the thread's rollout so a second client can resume it, and
/// the canned model's answer to it.
const HELLO: &str = "Say hello.";
const GREETING: &str = "Hello.";
/// What the canned model answers once the command ran.
const ANSWER: &str = "Made it.";
/// How long the recorder waits on any one step before it gives up.
const PATIENCE: Duration = Duration::from_secs(60);
/// How long the recorder listens for what a client is not sent, before saying it was not.
const QUIET: Duration = Duration::from_secs(3);

/// The scratch directories of a run.
struct Scratch {
    root: PathBuf,
    home: PathBuf,
    codex_home: PathBuf,
    work: PathBuf,
}

impl Scratch {
    fn fresh() -> Result<Self> {
        // Outside the repository: Codex reads the `AGENTS.md` files and the git state above
        // its working directory, and a fixture must hold neither.
        let root = std::env::temp_dir().join(format!("slopty-codex-fixtures-{}", super::VERSION));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        let scratch = Self {
            home: root.join("home"),
            codex_home: root.join("codex-home"),
            work: root.join("work"),
            root,
        };
        for dir in [&scratch.home, &scratch.codex_home, &scratch.work] {
            std::fs::create_dir_all(dir)?;
        }
        // Scrubbing replaces the paths as Codex writes them, which is with symlinks resolved.
        Ok(Self {
            home: scratch.home.canonicalize()?,
            codex_home: scratch.codex_home.canonicalize()?,
            work: scratch.work.canonicalize()?,
            root: scratch.root.canonicalize()?,
        })
    }

    fn socket(&self) -> PathBuf {
        self.codex_home.join("app-server-control/app-server-control.sock")
    }
}

pub fn record() -> Result<()> {
    let codex = super::official()?;
    let scratch = Scratch::fresh()?;
    let api = CannedApi::start(&scratch.root.join("api.jsonl"), &scratch.work)?;
    write_config(&scratch.codex_home, api.port)?;
    let mut server = AppServer::start(&codex, &scratch)?;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let recorded = runtime.block_on(approval(&scratch));
    server.stop();
    let lines = recorded?;
    let dir = repo_root()?.join(FIXTURES).into_std_path_buf();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("approval.jsonl");
    println!("the run's API requests and app-server log are in {}", scratch.root.display());
    let mut scrub = Scrub::new(&scratch, &host()?);
    let mut out = String::new();
    for line in lines {
        out.push_str(&serde_json::to_string(&scrub.value(line))?);
        out.push('\n');
    }
    std::fs::write(&path, out).with_context(|| path.display().to_string())?;
    println!("wrote {} from Codex {}", path.display(), super::VERSION);
    Ok(())
}

/// The scratch `CODEX_HOME`'s config: the canned model as a custom provider, a read-only
/// sandbox, and an approval policy that asks before a command leaves it.
fn write_config(codex_home: &Path, port: u16) -> Result<()> {
    let config = format!(
        r#"model = "mock-model"
approval_policy = "on-request"
sandbox_mode = "read-only"
model_provider = "mock_provider"
check_for_update_on_startup = false

# Nothing is fetched from the network: no plugin marketplace, no connectors.
[features]
plugins = false
apps = false

[model_providers.mock_provider]
name = "Canned Responses API"
base_url = "http://127.0.0.1:{port}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false
"#
    );
    std::fs::write(codex_home.join("config.toml"), config)?;
    Ok(())
}

/// The app-server process, stopped when dropped.
struct AppServer {
    child: Child,
}

impl AppServer {
    fn start(codex: &Path, scratch: &Scratch) -> Result<Self> {
        let log = std::fs::File::create(scratch.root.join("app-server.log"))?;
        let listen = format!("unix://{}", scratch.socket().display());
        let child = Command::new(codex)
            .args(["app-server", "--listen", &listen])
            .current_dir(&scratch.work)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &scratch.home)
            .env("CODEX_HOME", &scratch.codex_home)
            .env("RUST_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("run {}", codex.display()))?;
        Ok(Self { child })
    }

    fn stop(&mut self) {
        let _gone = self.child.kill();
        let _reaped = self.child.wait();
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A canned Responses API: the first request of a turn is answered with a call to the shell
/// tool, the one carrying that call's output with [`ANSWER`]. Every request is kept in `log`.
struct CannedApi {
    port: u16,
}

impl CannedApi {
    fn start(log: &Path, work: &Path) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let log = std::fs::File::create(log)?;
        let work = work.to_owned();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let (Ok(log), work) = (log.try_clone(), work.clone()) else { break };
                std::thread::spawn(move || {
                    if let Err(e) = serve(stream, log, &work) {
                        eprintln!("canned API: {e:#}");
                    }
                });
            }
        });
        Ok(Self { port })
    }
}

fn serve(stream: TcpStream, mut log: std::fs::File, work: &Path) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some((line, body)) = crate::claude_mod::request(&mut reader)? {
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        writeln!(log, "{}", json!({ "line": line, "body": body }))?;
        let (status, kind, payload) = if line.starts_with("POST") && line.contains("/responses") {
            ("200 OK", "text/event-stream", sse(&answer(&body, work)?))
        } else {
            ("404 Not Found", "application/json", "{}".to_owned())
        };
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\r\n",
            payload.len()
        );
        writer.write_all(head.as_bytes())?;
        writer.write_all(payload.as_bytes())?;
    }
    Ok(())
}

/// The canned model's events for one request.
fn answer(body: &Value, work: &Path) -> Result<Vec<Value>> {
    let input = body.get("input").and_then(Value::as_array).cloned().unwrap_or_default();
    let last = input.last();
    let ran = last.and_then(|item| item.get("type")) == Some(&json!("function_call_output"));
    let asked = last.is_some_and(|item| item.to_string().contains(PROMPT));
    let created = json!({ "type": "response.created", "response": { "id": "resp-1" } });
    let completed = json!({
        "type": "response.completed",
        "response": {
            "id": "resp-1",
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": null,
                "output_tokens": 0,
                "output_tokens_details": null,
                "total_tokens": 0,
            },
        },
    });
    // Each message has an id of its own, as the Responses API gives one.
    let say = |id: &str, text: &str| {
        json!({
            "type": "message",
            "role": "assistant",
            "id": id,
            "content": [{ "type": "output_text", "text": text }],
        })
    };
    let item = if ran {
        say("msg-2", ANSWER)
    } else if !asked {
        say("msg-1", GREETING)
    } else {
        let tools: Vec<&str> = body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        let arguments = if tools.contains(&"exec_command") {
            (
                "exec_command",
                json!({ "cmd": COMMAND, "workdir": work, "sandbox_permissions": ESCALATED, "justification": WHY }),
            )
        } else if tools.contains(&"shell") {
            (
                "shell",
                json!({ "command": ["bash", "-lc", COMMAND], "workdir": work, "sandbox_permissions": ESCALATED, "justification": WHY }),
            )
        } else {
            bail!("the request offers no shell tool: {tools:?}");
        };
        json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": arguments.0,
            "arguments": arguments.1.to_string(),
        })
    };
    let done = json!({ "type": "response.output_item.done", "item": item });
    Ok(vec![created, done, completed])
}

/// `events` as a server-sent event stream.
fn sse(events: &[Value]) -> String {
    let mut out = String::new();
    for event in events {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
        out.push_str("event: ");
        out.push_str(kind);
        out.push_str("\ndata: ");
        out.push_str(&event.to_string());
        out.push_str("\n\n");
    }
    out
}

/// Which client a frame is of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Who {
    /// The client that starts the thread, as Slopty does.
    A,
    /// The one that resumes it to follow, as a Codex TUI does.
    B,
}

impl Who {
    const fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
        }
    }
}

type Socket = WebSocketStream<UnixStream>;

/// Both clients, with every frame either sent or heard, in order.
struct Pair {
    a: Socket,
    b: Socket,
    lines: Vec<Value>,
    next_id: HashMap<&'static str, u64>,
}

impl Pair {
    async fn connect(socket: &Path) -> Result<Self> {
        let a = connect(socket).await?;
        let b = connect(socket).await?;
        Ok(Self { a, b, lines: Vec::new(), next_id: HashMap::new() })
    }

    const fn socket(&mut self, who: Who) -> &mut Socket {
        match who {
            Who::A => &mut self.a,
            Who::B => &mut self.b,
        }
    }

    async fn send(&mut self, who: Who, msg: Value) -> Result<()> {
        self.lines.push(json!({ "client": who.name(), "dir": "sent", "msg": msg }));
        self.socket(who).send(Message::text(msg.to_string())).await?;
        Ok(())
    }

    /// Send request `method` from `who`, numbered per client.
    async fn request(&mut self, who: Who, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id.entry(who.name()).or_insert(0);
        *id = id.saturating_add(1);
        let id = *id;
        self.send(who, json!({ "id": id, "method": method, "params": params })).await?;
        Ok(id)
    }

    /// The next frame either client hears, or `None` after `wait` of quiet.
    async fn heard(&mut self, wait: Duration) -> Result<Option<(Who, Value)>> {
        let frame = tokio::time::timeout(wait, async {
            tokio::select! {
                frame = self.a.next() => (Who::A, frame),
                frame = self.b.next() => (Who::B, frame),
            }
        })
        .await;
        let Ok((who, frame)) = frame else { return Ok(None) };
        let frame = frame.with_context(|| format!("client {} was closed", who.name()))??;
        let text = match frame {
            Message::Text(text) => text.to_string(),
            Message::Ping(_) | Message::Pong(_) => return Box::pin(self.heard(wait)).await,
            other => bail!("client {} heard {other:?}", who.name()),
        };
        let msg: Value = serde_json::from_str(&text)?;
        self.lines.push(json!({ "client": who.name(), "dir": "heard", "msg": msg }));
        Ok(Some((who, msg)))
    }

    /// Listen until `done` takes a frame; what comes before it is kept and passed over.
    async fn until(
        &mut self,
        what: &str,
        mut done: impl FnMut(Who, &Value) -> bool,
    ) -> Result<(Who, Value)> {
        loop {
            let Some((who, msg)) = self.heard(PATIENCE).await? else {
                bail!("no {what} within {PATIENCE:?}");
            };
            if done(who, &msg) {
                return Ok((who, msg));
            }
        }
    }

    /// `who`'s answer to its request `id`.
    async fn answer(&mut self, who: Who, id: u64) -> Result<Value> {
        let (_, msg) = self
            .until(&format!("answer to request {id}"), |from, msg| {
                from == who && msg.get("id") == Some(&json!(id)) && msg.get("method").is_none()
            })
            .await?;
        ensure!(msg.get("error").is_none(), "request {id} failed: {msg}");
        Ok(msg)
    }

    /// Everything either client hears until `wait` passes quietly.
    async fn settle(&mut self, wait: Duration) -> Result<Vec<(Who, Value)>> {
        let mut heard = Vec::new();
        while let Some(frame) = self.heard(wait).await? {
            heard.push(frame);
        }
        Ok(heard)
    }
}

async fn connect(socket: &Path) -> Result<Socket> {
    let stream = UnixStream::connect(rendezvous(socket)?).await?;
    let (ws, _response) = tokio_tungstenite::client_async("ws://localhost/", stream)
        .await
        .context("the WebSocket handshake")?;
    Ok(ws)
}

/// Where the socket at `socket` really is. Codex makes its control socket a symlink to one in
/// a short shared directory, since a socket's own path must fit `sun_path` (104 bytes on
/// macOS); a client connects to the target for the same reason.
fn rendezvous(socket: &Path) -> Result<PathBuf> {
    socket.canonicalize().with_context(|| format!("resolve {}", socket.display()))
}

/// Wait for the app-server to listen.
async fn listening(socket: &Path) -> Result<()> {
    let wait = async {
        while match rendezvous(socket) {
            Ok(target) => UnixStream::connect(target).await.is_err(),
            Err(_) => true,
        } {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(PATIENCE, wait)
        .await
        .with_context(|| format!("the app-server never listened on {}", socket.display()))
}

/// The two-client approval: who is asked, what a second answer gets, how both hear it settled.
async fn approval(scratch: &Scratch) -> Result<Vec<Value>> {
    let socket = scratch.socket();
    listening(&socket).await?;
    let mut pair = Pair::connect(&socket).await?;
    for (who, name) in [(Who::A, "slopty"), (Who::B, "codex-tui")] {
        let params = json!({
            "clientInfo": { "name": name, "version": "1.0.0" },
            "capabilities": { "experimentalApi": true },
        });
        let id = pair.request(who, "initialize", params).await?;
        pair.answer(who, id).await?;
        pair.send(who, json!({ "method": "initialized" })).await?;
    }
    let start =
        json!({ "cwd": scratch.work, "approvalPolicy": "on-request", "sandbox": "read-only" });
    let id = pair.request(Who::A, "thread/start", start).await?;
    let started = pair.answer(Who::A, id).await?;
    let thread = started.pointer("/result/thread/id").and_then(Value::as_str);
    let thread = thread.context("thread/start named no thread")?.to_owned();
    // A thread is written down with its first turn; until then there is nothing to resume.
    let input = json!([{ "type": "text", "text": HELLO, "text_elements": [] }]);
    let id =
        pair.request(Who::A, "turn/start", json!({ "threadId": thread, "input": input })).await?;
    pair.answer(Who::A, id).await?;
    let done = |msg: &Value| msg.get("method") == Some(&json!("turn/completed"));
    pair.until("the first turn's end", |_, msg| done(msg)).await?;
    let id = pair.request(Who::B, "thread/resume", json!({ "threadId": thread })).await?;
    pair.answer(Who::B, id).await?;
    pair.settle(QUIET).await?;

    let input = json!([{ "type": "text", "text": PROMPT, "text_elements": [] }]);
    let turn = json!({ "threadId": thread, "input": input });
    let second = pair.lines.len();
    let id = pair.request(Who::A, "turn/start", turn).await?;
    pair.answer(Who::A, id).await?;
    let is_approval = |msg: &Value| {
        msg.get("method").and_then(Value::as_str).is_some_and(|m| m.ends_with("requestApproval"))
    };
    let mut asked: Vec<(Who, Value)> = Vec::new();
    asked.push(pair.until("approval request", |_, msg| is_approval(msg)).await?);
    for (who, msg) in pair.settle(QUIET).await? {
        if is_approval(&msg) {
            asked.push((who, msg));
        }
    }
    println!(
        "asked: {:?}",
        asked.iter().map(|(who, msg)| (who.name(), msg.get("id").cloned())).collect::<Vec<_>>()
    );
    // The follower answers first, then the starter answers the same request otherwise.
    let order = [(Who::B, "accept"), (Who::A, "decline")];
    for (who, decision) in order {
        let Some((_, request)) = asked.iter().find(|(asked_who, _)| *asked_who == who) else {
            continue;
        };
        let id = request.get("id").cloned().context("an approval with no id")?;
        pair.send(who, json!({ "id": id, "result": { "decision": decision } })).await?;
        pair.settle(QUIET).await?;
    }
    let mut completed: Vec<Who> = pair
        .lines
        .iter()
        .skip(second)
        .filter(|l| l.get("dir") == Some(&json!("heard")) && l.get("msg").is_some_and(done))
        .filter_map(|l| match l.get("client").and_then(Value::as_str) {
            Some("a") => Some(Who::A),
            Some("b") => Some(Who::B),
            _ => None,
        })
        .collect();
    while !(completed.contains(&Who::A) && completed.contains(&Who::B)) {
        let (who, _) = pair.until("turn/completed", |_, msg| done(msg)).await?;
        completed.push(who);
    }
    pair.settle(QUIET).await?;
    pair.a.close(None).await?;
    pair.b.close(None).await?;
    Ok(pair.lines)
}

/// What a fixture must not keep: scratch paths, ids and times.
struct Scrub {
    paths: Vec<(String, &'static str)>,
    ids: HashMap<String, String>,
}

impl Scrub {
    fn new(scratch: &Scratch, host: &str) -> Self {
        let paths = vec![
            (host.to_owned(), "host"),
            (scratch.work.display().to_string(), "/work"),
            (scratch.codex_home.display().to_string(), "/codex-home"),
            (scratch.home.display().to_string(), "/home/user"),
            (scratch.root.display().to_string(), "/scratch"),
        ];
        Self { paths, ids: HashMap::new() }
    }

    fn value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| {
                        let value = if is_time(&key) && value.is_number() {
                            json!(0)
                        } else if key == "userAgent" {
                            // It names this Mac's macOS build.
                            json!("user-agent")
                        } else {
                            self.value(value)
                        };
                        (key, value)
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    fn text(&mut self, text: &str) -> String {
        let mut out = text.to_owned();
        for (path, stand_in) in &self.paths {
            out = out.replace(path.as_str(), stand_in);
        }
        for template in DATED {
            out = undate(&out, template);
        }
        let mut scrubbed = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(at) = uuid_at(rest) {
            let (before, found) = rest.split_at(at);
            let (uuid, after) = found.split_at(36);
            scrubbed.push_str(before);
            let next = self.ids.len().saturating_add(1);
            let stand_in = self
                .ids
                .entry(uuid.to_owned())
                .or_insert_with(|| format!("00000000-0000-7000-8000-{next:012}"));
            scrubbed.push_str(stand_in);
            rest = after;
        }
        scrubbed.push_str(rest);
        scrubbed
    }
}

/// This machine's name, as Codex says it (`serverName`).
fn host() -> Result<String> {
    let out = Command::new("hostname").output().context("run hostname")?;
    let name = String::from_utf8(out.stdout)?.trim().to_owned();
    ensure!(!name.is_empty(), "this machine has no name");
    Ok(name)
}

/// Dates in the names Codex gives its rollouts
/// (`sessions/2026/10/02/rollout-2026-10-02T06-29-36-…`), `9` standing for a digit.
const DATED: [&str; 2] = ["/sessions/9999/99/99/", "rollout-9999-99-99T99-99-99-"];

/// `text` with every run that fits `template` written with zeros for its digits.
fn undate(text: &str, template: &str) -> String {
    let fits = |window: &[u8]| {
        window.iter().zip(template.as_bytes()).all(|(b, t)| match t {
            b'9' => b.is_ascii_digit(),
            t => b == t,
        })
    };
    let zeros = template.replace('9', "0");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.as_bytes().windows(template.len()).position(fits) {
        let (before, found) = rest.split_at(at);
        out.push_str(before);
        out.push_str(&zeros);
        rest = found.get(template.len()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// Whether a field named `key` holds a time or a duration.
fn is_time(key: &str) -> bool {
    key.ends_with("At")
        || key.ends_with("_at")
        || key.ends_with("Ms")
        || key.ends_with("_ms")
        || key == "timestamp"
}

/// Where the first UUID in `text` starts.
fn uuid_at(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(35)).find(|&at| {
        bytes.get(at..at.saturating_add(36)).is_some_and(|window| {
            window.iter().enumerate().all(|(i, b)| match i {
                8 | 13 | 18 | 23 => *b == b'-',
                _ => b.is_ascii_hexdigit(),
            })
        })
    })
}
