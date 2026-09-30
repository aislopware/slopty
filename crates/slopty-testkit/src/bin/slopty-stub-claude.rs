//! A stand-in for `claude`, for tests that start agents without spending anyone's plan
//! (`docs/decisions/projects.md`, "Tests use a stub agent").
//!
//! It does what Slopty needs of Claude Code and nothing more:
//!
//! - `--version` answers as Claude Code does, so a worker lists it as installed, and `agents
//!   --json` lists no live session.
//! - It fires the hooks its `--settings` registers, as Claude Code runs them: every command
//!   registered for the payload's event, with the payload on stdin and the session's environment,
//!   keeping what each printed. Which hooks, in order, is `STUB_HOOKS` (a JSON array of payloads);
//!   a `SessionStart` alone when it is unset. `STUB_LATER` is more of them, fired once its prompt
//!   shows and the file `STUB_LATER_AFTER` exists, as a later turn's.
//! - With `STUB_MCP_CALLS` (`[{"name": …, "arguments": …}, …]`), it calls those tools in order
//!   through the `slopty` MCP server its `--mcp-config` names, over stdio, as Claude Code starts
//!   it: the session's environment is the server's. With `STUB_MCP_AFTER` too, it first waits for
//!   that file to exist, so a test can change what the server knows of the session before.
//! - With `STUB_INBOX` (a file), it binds an inbox as Claude Code v2.1.224 and later do for
//!   cross-session messages: a Unix socket named to its hooks in `CLAUDE_CODE_MESSAGING_SOCKET`,
//!   with a token in `CLAUDE_CODE_MESSAGING_TOKEN`. Each user message posted there (one JSON
//!   document a line, an optional `{"type":"auth","token":…}` first) is appended to the file as one
//!   JSON line: its text, its priority, and whether its connection showed the token. Then, as an
//!   idle Claude Code does, it takes a turn: the message goes into its transcript
//!   (`STUB_TRANSCRIPT`, a JSONL file) and its `Stop` hooks fire, their outputs kept on the line as
//!   `turn`. With `STUB_INBOX_HOLD` it holds every message instead, as a session does under
//!   `crossSessionInbound: "hold"`, and the line says `held`.
//! - With `STUB_TITLE`, it paints that terminal title first (OSC 2), as Claude Code paints `✳
//!   Claude Code` at its prompt: what the worker reads as the agent's title.
//! - It shows a prompt and takes what is typed at it, a line at a time.
//!
//! What it was given and what it saw goes to `STUB_RECORD` as one JSON document, replaced whole
//! after each step: its arguments, its `SLOPTY_*` and `CLAUDE_*` environment, every variable's
//! name with a digest of its value (to tell what it inherited without writing any value), the
//! MCP configs, the hooks it fired, the tool's answer and the lines typed.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a terminal program: its prompt is its screen"
)]

use std::error::Error;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

/// What Claude Code answers to `--version`.
const VERSION: &str = "2.1.285 (Claude Code)";

/// How long a tool call through `slopty mcp` may take.
const MCP_PATIENCE: Duration = Duration::from_secs(30);

type Fallible<T> = Result<T, Box<dyn Error>>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("{VERSION}");
        return ExitCode::SUCCESS;
    }
    // The worker asks for Claude Code's live sessions when it starts: none run here.
    if args.first().is_some_and(|a| a == "agents") {
        println!("[]");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("stub claude: {e}");
            ExitCode::FAILURE
        }
    }
}

/// What the stub was given and what it saw, written to `STUB_RECORD` after each step.
struct Record {
    at: Option<PathBuf>,
    argv: Vec<String>,
    env: serde_json::Map<String, Value>,
    /// Every variable it was started with, by name, as a digest of its value
    /// (`slopty_testkit::env::digests`): no value is written.
    inherited: std::collections::BTreeMap<String, u64>,
    mcp_config: Vec<Value>,
    hooks: Vec<Value>,
    mcp: Vec<Value>,
    typed: Vec<String>,
    /// The variables Claude Code gives its hooks besides its own environment.
    hook_env: Vec<(String, String)>,
}

impl Record {
    fn save(&self) -> Fallible<()> {
        let Some(at) = &self.at else { return Ok(()) };
        let doc = json!({
            "argv": self.argv,
            "env": self.env,
            "inherited": self.inherited,
            "mcp_config": self.mcp_config,
            "hooks": self.hooks,
            "mcp": self.mcp,
            "typed": self.typed,
        });
        replace(at, &doc)
    }
}

fn run(args: &[String]) -> Fallible<()> {
    let mut record = Record {
        at: std::env::var_os("STUB_RECORD").map(PathBuf::from),
        argv: args.to_vec(),
        env: std::env::vars()
            .filter(|(k, _)| k.starts_with("SLOPTY_") || k.starts_with("CLAUDE_"))
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
        inherited: slopty_testkit::env::digests(),
        mcp_config: mcp_configs(args),
        hooks: Vec::new(),
        mcp: Vec::new(),
        typed: Vec::new(),
        hook_env: Vec::new(),
    };
    let settings = settings(args);
    if let Some(file) = std::env::var_os("STUB_INBOX") {
        record.hook_env = inbox(PathBuf::from(file), settings.clone())?;
    }

    let script = match std::env::var("STUB_HOOKS") {
        Ok(text) => serde_json::from_str(&text)?,
        Err(_) => json!([{ "hook_event_name": "SessionStart", "source": "startup" }]),
    };
    fire_all(&settings, &script, &mut record)?;
    record.save()?;

    if let Ok(calls) = std::env::var("STUB_MCP_CALLS") {
        let calls: Vec<Value> = serde_json::from_str(&calls)?;
        if let Some(gate) = std::env::var_os("STUB_MCP_AFTER") {
            wait_for(Path::new(&gate))?;
        }
        for call in &calls {
            let answer = match call_tool(&record.mcp_config, call) {
                Ok(answer) => answer,
                Err(e) => json!({ "error": e.to_string() }),
            };
            record.mcp.push(answer);
            record.save()?;
        }
    }

    let mut out = std::io::stdout();
    if let Ok(title) = std::env::var("STUB_TITLE") {
        write!(out, "\x1b]2;{title}\x07")?;
    }
    write!(out, "> ")?;
    out.flush()?;
    if let (Ok(later), Some(gate)) =
        (std::env::var("STUB_LATER"), std::env::var_os("STUB_LATER_AFTER"))
    {
        wait_for(Path::new(&gate))?;
        fire_all(&settings, &serde_json::from_str(&later)?, &mut record)?;
    }
    for line in std::io::stdin().lock().lines() {
        record.typed.push(line?);
        record.save()?;
        write!(out, "> ")?;
        out.flush()?;
    }
    Ok(())
}

/// What an inbox does with the messages posted to it.
struct Inbox {
    /// Where each message is noted.
    file: PathBuf,
    /// The token a connection shows to prove it is the session's own.
    token: String,
    /// Hold every message rather than take a turn with it.
    hold: bool,
    /// The transcript a message goes into when it is taken.
    transcript: Option<PathBuf>,
    /// The settings whose `Stop` hooks a turn fires, and the variables they are given.
    settings: Value,
    hook_env: Vec<(String, String)>,
}

/// Bind an inbox and serve it on a thread of its own ([`Inbox`]); the variables that name it to
/// the hooks.
fn inbox(file: PathBuf, settings: Value) -> Fallible<Vec<(String, String)>> {
    let socket = std::env::temp_dir().join(format!("stub-claude-{}.sock", std::process::id()));
    match std::fs::remove_file(&socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let token = format!("stub-{}-{}", std::process::id(), VERSION.len());
    let hook_env = vec![
        ("CLAUDE_CODE_MESSAGING_SOCKET".to_owned(), socket.to_string_lossy().into_owned()),
        ("CLAUDE_CODE_MESSAGING_TOKEN".to_owned(), token.clone()),
    ];
    let inbox = Inbox {
        file,
        token,
        hold: std::env::var_os("STUB_INBOX_HOLD").is_some(),
        transcript: std::env::var_os("STUB_TRANSCRIPT").map(PathBuf::from),
        settings,
        hook_env: hook_env.clone(),
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            if let Err(e) = read_inbox(stream, &inbox) {
                eprintln!("stub claude: inbox: {e}");
            }
        }
    });
    Ok(hook_env)
}

/// Read one connection to the inbox: an optional auth line, then user messages, each noted in
/// the inbox's file after it is held or taken as a turn.
fn read_inbox(stream: std::os::unix::net::UnixStream, inbox: &Inbox) -> Fallible<()> {
    let mut authed = false;
    for line in BufReader::new(stream).lines() {
        let doc: Value = serde_json::from_str(&line?)?;
        match doc.get("type").and_then(Value::as_str) {
            Some("auth") => authed = doc.get("token").and_then(Value::as_str) == Some(&inbox.token),
            Some("user") => {
                let content = doc.pointer("/message/content").and_then(Value::as_str);
                let content = content.filter(|c| !c.is_empty()).ok_or("a message with no text")?;
                let priority = doc.get("priority").and_then(Value::as_str).unwrap_or("next");
                let taken = if inbox.hold {
                    ("held", Value::Bool(true))
                } else {
                    ("turn", Value::Array(turn(inbox, content)?))
                };
                let mut got = serde_json::Map::new();
                got.insert("content".to_owned(), json!(content));
                got.insert("priority".to_owned(), json!(priority));
                got.insert("authed".to_owned(), json!(authed));
                got.insert(taken.0.to_owned(), taken.1);
                let got = Value::Object(got);
                let mut out =
                    std::fs::OpenOptions::new().create(true).append(true).open(&inbox.file)?;
                // One write per line, so a reader never sees half of one.
                out.write_all(format!("{got}\n").as_bytes())?;
            }
            _ => return Err(format!("not a line the inbox takes: {doc}").into()),
        }
    }
    Ok(())
}

/// Take a turn with a message: into the transcript as Claude Code writes a message from another
/// session, then the `Stop` hooks; what they printed.
fn turn(inbox: &Inbox, content: &str) -> Fallible<Vec<Value>> {
    let mut payload = serde_json::Map::new();
    payload.insert("hook_event_name".to_owned(), json!("Stop"));
    payload.insert("stop_hook_active".to_owned(), json!(false));
    if let Some(transcript) = &inbox.transcript {
        let entry = json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "origin": { "kind": "peer" },
        });
        let mut out = std::fs::OpenOptions::new().create(true).append(true).open(transcript)?;
        out.write_all(format!("{entry}\n").as_bytes())?;
        let path = transcript.to_string_lossy().into_owned();
        payload.insert("transcript_path".to_owned(), Value::String(path));
    }
    let payload = Value::Object(payload);
    let hooks = registered(&inbox.settings, "Stop");
    let printed =
        hooks.iter().filter_map(|(command, rest)| fire(command, rest, &inbox.hook_env, &payload));
    Ok(printed
        .filter(|out| !out.trim().is_empty())
        .map(|out| serde_json::from_str(&out).unwrap_or(Value::String(out)))
        .collect())
}

/// Wait for the file at `gate` to exist, up to [`MCP_PATIENCE`].
#[expect(
    clippy::disallowed_methods,
    reason = "a test program with no runtime, polling for a file the test writes"
)]
fn wait_for(gate: &Path) -> Fallible<()> {
    let since = std::time::Instant::now();
    while !gate.exists() {
        if since.elapsed() > MCP_PATIENCE {
            return Err(format!("{} never appeared", gate.display()).into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

/// Every `--mcp-config` document given as JSON, in the flag's `=` form or after it.
fn mcp_configs(args: &[String]) -> Vec<Value> {
    let mut docs = Vec::new();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let value = match word.strip_prefix("--mcp-config=") {
            Some(value) => Some(value.to_owned()),
            None if word == "--mcp-config" => words.next().cloned(),
            None => None,
        };
        if let Some(doc) = value.and_then(|v| serde_json::from_str::<Value>(&v).ok()) {
            docs.push(doc);
        }
    }
    docs
}

/// The `--settings` document, or an empty one.
fn settings(args: &[String]) -> Value {
    let at = args.iter().position(|a| a == "--settings");
    let doc = at.and_then(|at| args.get(at.saturating_add(1)));
    doc.and_then(|d| serde_json::from_str(d).ok()).unwrap_or_else(|| json!({}))
}

/// Fire every payload of `script` in order, recording each: whether every hook registered for
/// its event ran cleanly (none registered is not fired), and what they printed.
fn fire_all(settings: &Value, script: &Value, record: &mut Record) -> Fallible<()> {
    for payload in script.as_array().into_iter().flatten() {
        let event = payload.get("hook_event_name").and_then(Value::as_str).unwrap_or_default();
        // An event no hook is registered for (a `Statusline` a test posts) goes through the
        // relay, as `slopty hook statusline` posts it.
        let mut hooks = registered(settings, event);
        if hooks.is_empty() {
            hooks = registered(settings, "SessionStart");
            hooks.retain(|(_, rest)| rest == &["hook"]);
        }
        let env = &record.hook_env;
        let ran: Vec<Option<String>> =
            hooks.iter().map(|(command, rest)| fire(command, rest, env, payload)).collect();
        let fired = !ran.is_empty() && ran.iter().all(Option::is_some);
        let outputs: Vec<Value> = ran
            .into_iter()
            .flatten()
            .filter(|out| !out.trim().is_empty())
            .map(|out| serde_json::from_str(&out).unwrap_or(Value::String(out)))
            .collect();
        record.hooks.push(json!({ "event": event, "fired": fired, "outputs": outputs }));
        record.save()?;
    }
    Ok(())
}

/// The commands and their arguments the settings register for `event`, in order.
fn registered(settings: &Value, event: &str) -> Vec<(String, Vec<String>)> {
    let groups = settings.pointer(&format!("/hooks/{event}")).and_then(Value::as_array);
    let entries = groups.into_iter().flatten().filter_map(|g| g.get("hooks")?.as_array());
    entries
        .flatten()
        .filter(|h| h.get("type").and_then(Value::as_str) == Some("command"))
        .filter_map(|h| {
            let command = h.get("command")?.as_str()?.to_owned();
            let rest = h.get("args").and_then(Value::as_array).into_iter().flatten();
            Some((command, rest.filter_map(Value::as_str).map(str::to_owned).collect()))
        })
        .collect()
}

/// Run a hook as Claude Code does: the command with the payload on stdin. What it printed, when
/// it ran and exited cleanly.
fn fire(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    payload: &Value,
) -> Option<String> {
    let mut child = Command::new(command)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    stdin.write_all(payload.to_string().as_bytes()).ok()?;
    drop(stdin);
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Call `call` through the `slopty` server of the MCP configs, in the 2026-07-28 lifecycle
/// (no `initialize`; the version and capabilities in the request's `_meta`); its result.
fn call_tool(configs: &[Value], call: &Value) -> Fallible<Value> {
    let server = configs
        .iter()
        .find_map(|doc| doc.pointer("/mcpServers/slopty").filter(|s| s.is_object()))
        .ok_or("no --mcp-config names a slopty server")?;
    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or("the slopty server names no command")?;
    let args = server.get("args").and_then(Value::as_array).into_iter().flatten();
    let args = args.filter_map(Value::as_str);
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": call.get("name"),
            "arguments": call.get("arguments"),
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": { "name": "stub-claude", "version": VERSION },
            },
        },
    });
    let mut stdin = child.stdin.take().ok_or("the server's stdin")?;
    writeln!(stdin, "{request}")?;
    stdin.flush()?;
    let stdout = child.stdout.take().ok_or("the server's stdout")?;
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .find(|msg| msg.get("id").and_then(Value::as_u64) == Some(1));
        let _gone = answer.send(found);
    });
    let reply = answered.recv_timeout(MCP_PATIENCE);
    drop(stdin);
    let _killed = child.kill();
    let _reaped = child.wait();
    let reply = reply?.ok_or("the server closed without answering")?;
    Ok(reply.get("result").cloned().unwrap_or_default())
}

/// Replace the file at `at` with `record`, through a sibling, so a reader never sees half.
fn replace(at: &Path, record: &Value) -> Fallible<()> {
    let part = at.with_extension("part");
    std::fs::write(&part, serde_json::to_vec_pretty(record)?)?;
    std::fs::rename(&part, at)?;
    Ok(())
}
