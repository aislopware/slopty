//! `cargo xtask fixtures claude-stream`: record the official Claude Code build driven the way
//! the worker drives it (`crates/slopty-agent/tests/fixtures/claude-stream`).
//!
//! Each scenario runs the official build ([`crate::claude::official`]) as `claude -p` speaking
//! stream-json both ways with the control protocol, with the flags and variables the worker
//! gives it (`slopty_agent::claude::DRIVEN`, `slopty_agent::claude::ENV`), in a scratch home,
//! against the canned Messages API ([`crate::canned::FakeApi`]): no account and no model are
//! involved. The recorder plays the worker: it asks `initialize`, sends each message under an id
//! of its own, answers the permission it is asked, sets the mode and the model, asks for the
//! context, stops a turn as it streams, and runs `/compact`. Every line Claude Code writes is
//! kept, scrubbed as `fixtures claude` scrubs.

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
const DRIVEN: [&str; 9] = [
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--permission-prompt-tool=stdio",
    "--replay-user-messages",
    "--include-partial-messages",
];

/// The variables the worker gives it besides its own (`slopty_agent::claude::ENV`).
const ENV: [(&str, &str); 3] = [
    ("CLAUDE_CODE_EAGER_FLUSH", "1"),
    ("MCP_CONNECTION_NONBLOCKING", "true"),
    ("CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL", "true"),
];

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
    let plays: [(&str, Play); 2] = [("session", session), ("agent", agent)];
    for (name, play) in plays {
        let started = Instant::now();
        println!("▶ {name}");
        let lines =
            run(&claude, api.port, name, play).with_context(|| format!("scenario {name}"))?;
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
    driver.control("init1", &json!({ "subtype": "initialize" }))?;
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
    driver.control("init1", &json!({ "subtype": "initialize" }))?;
    driver.say("scenario:agent go")?;
    driver.result()?;
    driver.result()?;
    Ok(())
}

/// Run scenario `name` by `play` against the canned API on `port`: every line Claude Code wrote,
/// scrubbed.
fn run(claude: &Path, port: u16, name: &str, play: Play) -> Result<String> {
    let work = scratch(name)?;
    let home = scratch(&format!("{name}-home"))?;
    let mut child = Command::new(claude)
        .args(DRIVEN)
        .args(["--session-id", SESSION, "--model", "haiku", "--permission-mode", "default"])
        .args(["--setting-sources", ""])
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
    let mut scrub = Scrub::with_places(Vec::new(), &work, &home.to_string_lossy())?;
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
