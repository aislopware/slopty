//! `cargo xtask fixtures claude-stream`: record the official Claude Code build driven the way
//! the worker drives it (`crates/slopty-agent/tests/fixtures/claude-stream`).
//!
//! Each scenario runs the official build ([`crate::claude::official`]) as `claude -p` speaking
//! stream-json both ways with the control protocol, with the flags and variables the worker
//! gives it (`slopty_agent::claude::DRIVEN`, `slopty_agent::claude::ENV`), in a scratch home,
//! against the canned Messages API ([`crate::canned::FakeApi`]): no account and no model are
//! involved. The recorder plays the worker: it asks `initialize`, sends each message under an id
//! of its own, answers the permission it is asked, sets the mode and the model, asks for the
//! context, stops a turn as it streams, and runs `/compact`; it asks for the MCP servers' apps,
//! reads an app's page and calls its tools (`slopty-stub-mcp-apps`), lists the permission rules,
//! hands over a `!` command's output not to be answered, moves work to the background and stops
//! it, and asks for files to be put back. Every line Claude Code writes is kept, scrubbed as
//! `fixtures claude` scrubs.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};

use crate::fixtures::Scrub;
use crate::tools::repo_root;

/// The session every recording runs as.
const SESSION: &str = "11111111-2222-4333-8444-555555555555";

/// The flags the worker runs Claude Code with (`slopty_agent::claude::DRIVEN`).
const DRIVEN: [&str; 12] = [
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--permission-prompt-tool=stdio",
    "--replay-user-messages",
    "--include-partial-messages",
    "--forward-subagent-text",
    "--include-hook-events",
    "--prompt-suggestions",
];

/// The variables the worker gives it besides its own (`slopty_agent::claude::ENV`).
const ENV: [(&str, &str); 6] = [
    ("CLAUDE_CODE_EAGER_FLUSH", "1"),
    ("MCP_CONNECTION_NONBLOCKING", "true"),
    ("CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL", "true"),
    ("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1"),
    ("CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING", "1"),
    ("CLAUDE_CODE_MCP_APPS_HOST", "1"),
];

/// What the worker's `initialize` asks for beside the subtype (`Driven::initialize`).
fn initialize() -> Value {
    json!({
        "subtype": "initialize",
        "supportedDialogKinds": ["mcp_server_approval", "trust_folder"],
        "forwardNestedSubagentToolCalls": true,
        "agentProgressSummaries": true,
        "perTaskStopAffordance": true,
    })
}

/// How long one step may wait for what answers it.
const STEP: Duration = Duration::from_secs(60);

/// How long Claude Code has to end once its input closes.
const ENDING: Duration = Duration::from_secs(20);

/// Record every scenario with Claude Code `version`.
pub fn record(version: &str) -> Result<()> {
    let claude = crate::claude::official(version)?;
    println!("claude {version} at {}", claude.display());
    let out = repo_root()?.join("crates/slopty-agent/tests/fixtures/claude-stream");
    std::fs::create_dir_all(&out)?;
    let api = crate::canned::FakeApi::start()?;
    let apps = stub_mcp_apps()?;
    let servers = json!({ "mcpServers": { "apps": { "command": apps, "args": [] } } });
    let doors = vec![
        "--mcp-config".to_owned(),
        servers.to_string(),
        "--allowedTools".to_owned(),
        "Bash(echo:*)".to_owned(),
    ];
    let plays: [(&str, Play, Vec<String>); 3] = [
        ("session", session, Vec::new()),
        ("agent", agent, Vec::new()),
        ("doors", doors_play, doors),
    ];
    for (name, play, extra) in plays {
        let started = Instant::now();
        println!("▶ {name}");
        let lines = run(&claude, api.port, name, play, &extra)
            .with_context(|| format!("scenario {name}"))?;
        std::fs::write(out.join(format!("{name}.jsonl")).as_std_path(), lines)?;
        println!("  ✓ {name} ({:.1?})", started.elapsed());
    }
    Ok(())
}

/// What the recorder does with a running Claude Code.
type Play = fn(&mut Driver) -> Result<()>;

/// A running Claude Code, as the recorder plays the worker to it.
struct Driver {
    stdin: Option<ChildStdin>,
    heard: mpsc::Receiver<Value>,
    lines: Vec<Value>,
    sent: u32,
}

impl Driver {
    fn write(&mut self, line: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().context("Claude Code's input is closed")?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send `text` as the person's message, under an id of its own.
    fn say(&mut self, text: &str) -> Result<()> {
        self.sent = self.sent.saturating_add(1);
        let uuid = format!("aaaaaaaa-0000-4000-8000-{:012}", self.sent);
        self.write(&json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
            "parent_tool_use_id": null,
            "session_id": SESSION,
            "uuid": uuid,
        }))
    }

    /// Ask `request` under `id`, and wait for its answer.
    fn control(&mut self, id: &str, request: &Value) -> Result<Value> {
        self.write(&json!({ "type": "control_request", "request_id": id, "request": request }))?;
        self.until(&format!("the answer to {id}"), |line| {
            str_at(line, "/type") == Some("control_response")
                && str_at(line, "/response/request_id") == Some(id)
        })
    }

    /// The first line from now on that `wanted` takes, within [`STEP`].
    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now().checked_add(STEP).unwrap_or_else(Instant::now);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.heard.recv_timeout(left) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => bail!("no {what} in {STEP:?}"),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("Claude Code ended before {what}")
                }
            };
            self.lines.push(line.clone());
            if wanted(&line) {
                return Ok(line);
            }
        }
    }

    fn result(&mut self) -> Result<Value> {
        self.until("the turn's result", |line| str_at(line, "/type") == Some("result"))
    }
}

/// A turn that thinks and answers; one whose Bash call asks permission, allowed; the mode, the
/// model and the context asked for; a turn stopped as it streams; `/compact`.
fn session(driver: &mut Driver) -> Result<()> {
    driver.control("init1", &initialize())?;
    driver.say("scenario:think say hello")?;
    driver.result()?;
    driver.say("scenario:touch make a file")?;
    let asked = driver.until("a permission", |line| {
        str_at(line, "/type") == Some("control_request")
            && str_at(line, "/request/subtype") == Some("can_use_tool")
    })?;
    let id = asked.get("request_id").cloned().unwrap_or(Value::Null);
    let input = asked.pointer("/request/input").cloned().unwrap_or(Value::Null);
    driver.write(&json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id,
            "response": { "behavior": "allow", "updatedInput": input } },
    }))?;
    driver.result()?;
    driver.control("p1", &json!({ "subtype": "set_permission_mode", "mode": "plan" }))?;
    driver.control("m1", &json!({ "subtype": "set_model", "model": "sonnet" }))?;
    driver.control("c1", &json!({ "subtype": "get_context_usage" }))?;
    driver.say("scenario:slow one")?;
    driver.until("the answer streaming", |line| {
        str_at(line, "/type") == Some("stream_event")
            && str_at(line, "/event/type") == Some("content_block_delta")
    })?;
    driver.control("i1", &json!({ "subtype": "interrupt" }))?;
    driver.result()?;
    driver.say("/compact")?;
    driver.result()?;
    Ok(())
}

/// A subagent's run, its notice, and the turn the notice wakes.
fn agent(driver: &mut Driver) -> Result<()> {
    driver.control("init1", &initialize())?;
    driver.say("scenario:agent go")?;
    driver.result()?;
    driver.result()?;
    Ok(())
}

/// The doors beside messages: the MCP servers' apps, an app's page read and its tools called for
/// the page alone (one read refused), the permission rules, work moved to the background with
/// none running, the mods' surfaces, a background command stopped by its task id, a `!` command's
/// output handed over not to be answered, and files asked back to before the first message.
fn doors_play(driver: &mut Driver) -> Result<()> {
    driver.control("init1", &initialize())?;
    // The server connects without holding the first turn: its status names it once it did.
    let deadline = Instant::now().checked_add(STEP).unwrap_or_else(Instant::now);
    loop {
        let status = driver.control("s1", &json!({ "subtype": "mcp_status" }))?;
        let connected = status
            .pointer("/response/response/mcpServers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|s| str_at(s, "/status") == Some("connected"));
        if connected {
            break;
        }
        ensure!(Instant::now() < deadline, "the apps server did not connect in {STEP:?}");
        pause(Duration::from_millis(200));
    }
    let read =
        json!({ "subtype": "mcp_read_resource", "serverName": "apps", "uri": "ui://apps/echo" });
    driver.control("r1", &read)?;
    let missing =
        json!({ "subtype": "mcp_read_resource", "serverName": "apps", "uri": "ui://apps/none" });
    driver.control("r2", &missing)?;
    let call = json!({
        "subtype": "mcp_call", "tool": "mcp__apps__echo", "arguments": { "text": "hi" },
        "result_not_for_model": true,
    });
    driver.control("t1", &call)?;
    let refresh = json!({
        "subtype": "mcp_call", "tool": "mcp__apps__refresh", "arguments": {},
        "result_not_for_model": true,
    });
    driver.control("t2", &refresh)?;
    driver.control("l1", &json!({ "subtype": "list_permission_rules" }))?;
    driver.control("b1", &json!({ "subtype": "background_tasks" }))?;
    driver.control(
        "u1",
        &json!({ "subtype": "ui_attach", "surface": "desktop", "client_id": "slopty" }),
    )?;
    driver.control("u2", &json!({ "subtype": "ui_panes", "client_id": "slopty" }))?;
    driver.say("scenario:background start it")?;
    let started = driver.until("the background task", |line| {
        str_at(line, "/type") == Some("system") && str_at(line, "/subtype") == Some("task_started")
    })?;
    let task = str_at(&started, "/task_id").unwrap_or_default().to_owned();
    driver.result()?;
    driver.control("k1", &json!({ "subtype": "stop_task", "task_id": task }))?;
    // Its notice may come before the answer.
    let stopped = |line: &Value| {
        str_at(line, "/type") == Some("system")
            && str_at(line, "/subtype") == Some("task_notification")
    };
    if !driver.lines.iter().any(stopped) {
        driver.until("the task stopped", stopped)?;
    }
    // A `!` command's output, taken in without a turn.
    driver.sent = driver.sent.saturating_add(1);
    let bang = format!("aaaaaaaa-0000-4000-8000-{:012}", driver.sent);
    driver.write(&json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "text", "text": "<bash-input>pwd</bash-input>" },
            { "type": "text", "text": "<bash-stdout>/work</bash-stdout><bash-stderr></bash-stderr>" },
        ] },
        "parent_tool_use_id": null,
        "session_id": SESSION,
        "uuid": bang,
        "shouldQuery": false,
    }))?;
    driver.until("the output taken in", |line| {
        str_at(line, "/type") == Some("command_lifecycle")
            && str_at(line, "/command_uuid") == Some(bang.as_str())
            && matches!(str_at(line, "/state"), Some("completed" | "cancelled"))
    })?;
    let first = "aaaaaaaa-0000-4000-8000-000000000001";
    driver.control("w1", &json!({ "subtype": "rewind_files", "user_message_id": first }))?;
    driver.control("w2", &json!({ "subtype": "rewind_files", "user_message_id": "no-such" }))?;
    Ok(())
}

/// The stand-in MCP Apps server, built from this checkout.
fn stub_mcp_apps() -> Result<PathBuf> {
    let sh = xshell::Shell::new()?;
    sh.change_dir(repo_root()?);
    xshell::cmd!(sh, "cargo build -p slopty-testkit --bin slopty-stub-mcp-apps").run()?;
    Ok(crate::tools::target_dir(&sh)?.join("debug/slopty-stub-mcp-apps").into_std_path_buf())
}

/// Run scenario `name` by `play` against the canned API on `port`, with `extra` arguments:
/// every line Claude Code wrote, scrubbed.
fn run(claude: &Path, port: u16, name: &str, play: Play, extra: &[String]) -> Result<String> {
    let work = scratch(name)?;
    let home = scratch(&format!("{name}-home"))?;
    let mut child = Command::new(claude)
        .args(DRIVEN)
        .args(["--session-id", SESSION, "--model", "haiku", "--permission-mode", "default"])
        .args(["--setting-sources", ""])
        .args(extra)
        .current_dir(&work)
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin"),
            ("TMPDIR", "/tmp"),
            ("ANTHROPIC_API_KEY", "sk-ant-fixture"),
            ("DISABLE_TELEMETRY", "1"),
            ("DISABLE_ERROR_REPORTING", "1"),
            ("DISABLE_AUTOUPDATER", "1"),
        ])
        .envs(ENV)
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn claude")?;
    let stdout = child.stdout.take().context("claude's output")?;
    let (tx, heard) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
            if tx.send(value).is_err() {
                break;
            }
        }
    });
    let mut driver = Driver { stdin: child.stdin.take(), heard, lines: Vec::new(), sent: 0 };
    let played = play(&mut driver);
    // The input closed, the run ends, as the worker's would.
    drop(driver.stdin.take());
    let ended = ended(&mut child);
    if let Err(e) = played {
        let _killed = child.kill();
        return Err(e);
    }
    ended?;
    while let Ok(line) = driver.heard.try_recv() {
        driver.lines.push(line);
    }
    ensure!(!driver.lines.is_empty(), "Claude Code wrote nothing");
    // The stand-in MCP server runs from this checkout.
    let checkout = vec![(repo_root()?.to_string(), "/repo".to_owned())];
    let mut scrub = Scrub::with_places(checkout, &work, &home.to_string_lossy())?;
    for line in &driver.lines {
        scrub.collect(line);
    }
    let lines: Vec<String> =
        driver.lines.into_iter().map(|line| scrub.value(line, "").to_string()).collect();
    std::fs::remove_dir_all(&work)?;
    std::fs::remove_dir_all(&home)?;
    Ok(format!("{}\n", lines.join("\n")))
}

/// Wait for `child` to end within [`ENDING`].
fn ended(child: &mut Child) -> Result<()> {
    let deadline = Instant::now().checked_add(ENDING).unwrap_or_else(Instant::now);
    loop {
        if let Some(status) = child.try_wait()? {
            ensure!(status.success(), "claude exited {status}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _killed = child.kill();
            bail!("claude did not end in {ENDING:?}");
        }
        pause(Duration::from_millis(50));
    }
}

#[expect(clippy::disallowed_methods, reason = "a recorder's wait for its child, on its own thread")]
fn pause(wait: Duration) {
    std::thread::sleep(wait);
}

/// A fresh directory under `/tmp`.
fn scratch(name: &str) -> Result<PathBuf> {
    let dir = PathBuf::from(format!("/tmp/slopty-stream-fixture-{name}"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The string at JSON pointer `at` in `value`, if one is there.
fn str_at<'a>(value: &'a Value, at: &str) -> Option<&'a str> {
    value.pointer(at).and_then(Value::as_str)
}
