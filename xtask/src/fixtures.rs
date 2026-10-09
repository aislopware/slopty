//! `cargo xtask fixtures claude`: capture the Claude Code transcripts and hook payloads that pin
//! `slopty-agent`'s conversation decoder (`crates/slopty-agent/tests/fixtures/conversation`).
//!
//! Each scenario runs the official Claude Code build ([`crate::claude::official`], never
//! whatever `claude` is on `PATH`) headless on haiku, in a scratch directory of its own, with a
//! cheap scripted prompt. The transcript is taken from the `transcript_mirror` frames
//! `--session-mirror` writes on stdout, never from the files under `~/.claude`. Hook payloads
//! come from this binary, registered for the run as the hook (`fixtures hook-sink`), which also
//! answers permission requests so the capture proves the decision output Claude Code accepts.
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
    /// Validate Slopty's Claude Code mod and record what it sends, with the official build
    /// against a canned API: no account, no model.
    ClaudeMod {
        /// Record only this scenario.
        #[arg(long)]
        only: Option<String>,
        /// The Claude Code release to record on; the mod is then verified on it alone
        /// (`slopty_agent::claude_mod::MOD_CLAUDE_VERSIONS`).
        #[arg(long, default_value = crate::claude::VERSION)]
        version: String,
    },
    /// The hook a capture registers: saves the payload on stdin and answers permission
    /// requests. Not for people.
    #[command(hide = true)]
    HookSink {
        /// Where the payloads go, one file each.
        dir: PathBuf,
    },
}

pub fn run(cmd: &FixturesCmd) -> Result<()> {
    match cmd {
        FixturesCmd::Claude { only, version } => capture_all(only.as_deref(), version),
        FixturesCmd::ClaudeMod { only, version } => {
            crate::claude_mod::capture_all(only.as_deref(), version)
        }
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
    /// Interrupt the turn when the assistant calls this tool.
    interrupt_on: Option<&'static str>,
    /// Files the run must leave in the scratch directory, and files it must not.
    expect: &'static [&'static str],
    absent: &'static [&'static str],
    /// After the first turn's `result`, record the run's session registry file
    /// (`sessions/<pid>.json`): the session is alive, its background work still out.
    roster: bool,
    /// Turns the session starts on its own after the last prompt's (a finished background task
    /// wakes it), whose `result` is waited for too.
    wakes: usize,
    /// Answered by the canned Messages API (`claude_mod::FakeApi`) in a scratch home, not by a
    /// model: no account involved, and the model's part is scripted.
    canned: bool,
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
        roster: false,
        wakes: 0,
        canned: false,
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
        roster: false,
        wakes: 0,
        canned: false,
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
        roster: false,
        wakes: 0,
        canned: false,
    },
    Scenario {
        name: "compact",
        files: &[],
        turns: &[
            "Reply with the single word \"ready\".",
            "/compact Summarize only the user's requests, in one sentence.",
        ],
        allowed: "",
        interrupt_on: None,
        expect: &[],
        absent: &[],
        roster: false,
        wakes: 0,
        canned: false,
    },
    Scenario {
        name: "permission",
        files: &[],
        turns: &["Run these three Bash commands one at a time, in order, each as its own call, \
                  then reply \"done\": `touch refused.txt`, `touch allowed.txt`, \
                  `touch always.txt`. If one is refused, go on with the next."],
        allowed: "",
        interrupt_on: None,
        expect: &["allowed.txt", "always.txt"],
        absent: &["refused.txt"],
        roster: false,
        wakes: 0,
        canned: false,
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
        roster: true,
        wakes: 1,
        canned: true,
    },
];

/// Events the sink is registered for.
const SINK_EVENTS: [&str; 14] = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionDenied",
    "SubagentStart",
    "SubagentStop",
    "TaskCreated",
    "TaskCompleted",
    "PreCompact",
    "PostCompact",
    "Stop",
    "StopFailure",
];

/// The pid a recorded session registry file is named and says, whatever the run's was.
const REGISTRY_PID: u32 = 4321;

/// How long one scenario may run.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(300);

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
    /// The run's own session registry file, mid-run ([`Scenario::roster`]).
    roster: Option<Value>,
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
    let hook = json!([{ "hooks": [{
        "type": "command",
        "command": exe.to_string_lossy(),
        "args": ["fixtures", "hook-sink", sink.to_string_lossy()],
    }] }]);
    // Thinking is written to the transcript only as summaries, and only when asked for.
    let settings = json!({
        "showThinkingSummaries": true,
        "hooks": SINK_EVENTS.iter().map(|e| ((*e).to_owned(), hook.clone())).collect::<Map<_, _>>(),
    });
    // A canned run gets a scratch home and the fake API, nothing of the person's.
    let canned = if scenario.canned {
        let home = PathBuf::from(format!("/tmp/slopty-fixture-{}-home", scenario.name));
        if home.exists() {
            std::fs::remove_dir_all(&home)?;
        }
        std::fs::create_dir_all(&home)?;
        Some((home, crate::claude_mod::FakeApi::start()?))
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
        // what the scenarios' allowed tools already settle; the fixtures record `default`.
        .args(["--permission-mode", "default"])
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
    let registered = canned
        .as_ref()
        .map(|(home, _api)| home.join(".claude/sessions").join(format!("{}.json", child.id())));
    let roster = || match &registered {
        Some(file) => registry_file(file),
        None => bail!("a roster is read only from a canned run's scratch home"),
    };
    let captured = drive(scenario, &roster, &mut child);
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

/// Feed the turns and collect the mirrored records until `claude` exits.
fn drive(
    scenario: &Scenario,
    roster: &dyn Fn() -> Result<Value>,
    child: &mut Child,
) -> Result<Captured> {
    let stdout = child.stdout.take().context("stdout")?;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
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
    let mut wakes = scenario.wakes;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = match rx.recv_timeout(left) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("timed out"),
        };
        let Ok(frame) = serde_json::from_str::<Value>(&line) else { continue };
        match frame.get("type").and_then(Value::as_str) {
            Some("transcript_mirror") => {
                let path = frame.get("filePath").and_then(Value::as_str).unwrap_or_default();
                let entries = frame.get("entries").and_then(Value::as_array).cloned();
                captured.push(path, entries.unwrap_or_default());
            }
            Some("assistant") if !interrupted && calls(&frame, scenario.interrupt_on) => {
                if let Some(input) = stdin.as_mut() {
                    let request = json!({
                        "type": "control_request",
                        "request_id": "fixture-interrupt",
                        "request": { "subtype": "interrupt" },
                    });
                    writeln!(input, "{request}")?;
                    input.flush()?;
                }
                interrupted = true;
            }
            Some("control_request") => bail!("claude asked the host: {line}"),
            Some("result") => {
                if scenario.roster && captured.roster.is_none() {
                    captured.roster = Some(roster()?);
                }
                match (stdin.as_mut(), turns.next()) {
                    (Some(input), Some(turn)) => send_prompt(input, turn)?,
                    // A turn the session starts on its own is still to come: keep it open.
                    (Some(_), None) if wakes > 0 => wakes = wakes.saturating_sub(1),
                    _ => drop(stdin.take()),
                }
            }
            _ => {}
        }
    }
    Ok(captured)
}

/// The run's session registry file (`<home>/.claude/sessions/<pid>.json`), as `claude agents`
/// and the worker read it. Only ever a scratch home's.
fn registry_file(file: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("claude registered no session at {}", file.display()))?;
    serde_json::from_str(&text).context("the session registry file is no JSON")
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

/// The hook: save the payload, and answer a permission request by its command, the way the
/// `permission` scenario expects (allow once, allow always with the suggested rule, deny).
fn hook_sink(dir: &Path) -> Result<()> {
    let mut payload = String::new();
    std::io::stdin().read_to_string(&mut payload)?;
    let input: Value = serde_json::from_str(&payload)?;
    let output = (input.get("hook_event_name").and_then(Value::as_str)
        == Some("PermissionRequest"))
    .then(|| permission_answer(&input));
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

fn permission_answer(input: &Value) -> Value {
    let command = input.pointer("/tool_input/command").and_then(Value::as_str).unwrap_or_default();
    let decision = if command.contains("refused") {
        json!({ "behavior": "deny", "message": "Refused by the fixture." })
    } else if command.contains("always") {
        let suggested = input.get("permission_suggestions").cloned().unwrap_or_else(|| json!([]));
        json!({ "behavior": "allow", "updatedPermissions": suggested })
    } else {
        json!({ "behavior": "allow" })
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

/// The sink's records, in the order the hooks ran.
fn sink_payloads(dir: &Path) -> Result<Vec<Value>> {
    let mut names: Vec<PathBuf> =
        std::fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    names.sort();
    names.iter().map(|p| Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?)).collect()
}

/// `paths` are the hook sink, the scratch directory and this binary, in that order.
fn write_fixture(
    scenario: &Scenario,
    paths: [&Path; 3],
    home: &str,
    captured: Captured,
    hooks: Vec<Value>,
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
    for hook in &hooks {
        scrub.collect(hook);
    }
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
        std::fs::write(dir.join(name), format!("{}\n", lines.join("\n")))?;
    }
    if let Some(mut session) = captured.roster {
        // The process, the clocks, the socket and the generated name differ each run: fixed,
        // so the file only moves with Claude Code's shape.
        let clock = json!(1_790_000_000_000_u64);
        let fixed = [
            ("pid", json!(REGISTRY_PID)),
            ("procStart", json!("fixed")),
            ("startedAt", clock.clone()),
            ("updatedAt", clock.clone()),
            ("statusUpdatedAt", clock.clone()),
            ("nameSince", clock),
            ("messagingSocketPath", json!("/tmp/claude.sock")),
            ("name", json!(scenario.name)),
        ];
        if let Some(entry) = session.as_object_mut() {
            for (key, value) in &fixed {
                if entry.contains_key(*key) {
                    entry.insert((*key).to_owned(), value.clone());
                }
            }
        }
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        let file = sessions.join(format!("{REGISTRY_PID}.json"));
        std::fs::write(file, format!("{}\n", scrub.value(session, "")))?;
    }
    let hooks: Vec<String> = hooks.into_iter().map(|h| scrub.value(h, "").to_string()).collect();
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
