//! The stand-in `claude` driven over stream-json, speaking Claude Code's control protocol.
//!
//! `slopty-stub-claude -p --input-format stream-json --output-format stream-json ...` reads and
//! writes it on stdin and stdout as the driven adapter runs the real one, so a test drives a
//! Claude Code thread without spending anyone's plan.
//!
//! It answers `initialize` (commands, models, mode, session state, and every permission prompt it
//! issued and not yet heard answered as `pending_permission_requests`, so a second initialize from
//! a worker that reattached gets them back), and runs one turn per user message, in order: a
//! message is `queued`, then `started`, echoed with its own `uuid` and `isReplay`, and the turn's
//! output ends in a `result` and a `completed` (or `cancelled`) lifecycle record.
//!
//! The turn is chosen by the prompt's first word, after an optional `scenario:` prefix:
//!
//! - `bash <cmd>`: a `Bash` tool call, asked about with `can_use_tool` as an ask rule wants
//!   (`decision_reason_type: rule`); allowed it says `ran <cmd>` with the file it made
//!   (`bashEditDiff`), denied its result is the deny message, as an error.
//! - `ask`: an `AskUserQuestion` (`Which color?`, Red or Blue) asked about with `can_use_tool`; the
//!   `answers` of an allow come back in its result.
//! - `agent`: an `Agent` call whose subagent (`task_started`, a message of its own with
//!   `parent_tool_use_id`, `task_notification`) waits `STUB_AGENT_HOLD_MS` (0 by default), so a
//!   test can `stop_task` it.
//! - `slow <n>`: `n` text deltas 200 ms apart (10 by default), for an interrupt to land in.
//! - `retry <n>`: the request fails and is tried again `n` times (3 by default), each a
//!   `system/api_retry` (overloaded, attempt `i` of 10, `retry_delay_ms` growing), then it goes
//!   through and the turn answers as below.
//! - `limit`: a `rate_limit_event` rejected until `STUB_LIMIT_RESET_S` (3 by default) from now, and
//!   an error result; every message until then meets the same, one after it runs.
//! - `/compact`: compaction (`status: compacting`, `compact_boundary`), a local command.
//! - any other `/<name>`: a local command's output.
//! - a prompt `STUB_TURNS` (`{"<prompt>": [{"name": …, "arguments": …}, …], …}`) names: those tools
//!   called in order through the `slopty` MCP server its `--mcp-config` (or the folder's
//!   `.mcp.json`) names, over stdio, as Claude Code starts it (the stand-in's environment is the
//!   server's), each an `mcp__slopty__<name>` call with its result, then `Done: <prompt>`. In a
//!   call's arguments, `{started}` is the agent the turn's last `agent_start` answered. Every
//!   result goes to the record's `mcp`.
//! - a prompt `STUB_REPLAY` names: a recorded Claude Code turn played as its own
//!   ([`crate::replay`]). `STUB_REPLAY` is a conversation fixture of `slopty-agent` by name
//!   (`tools`, `permission`, …) or folder, played for any prompt, or an object naming one for each
//!   prompt (`{"<prompt>": "<fixture>"}`). The recording's messages stream as Claude Code streams
//!   them, a subagent's under the `Agent` call that started it, each recorded permission request is
//!   a `can_use_tool` the turn waits on, and every record goes to the conversation's file (a
//!   subagent's to its own beside it). `STUB_REPLAY_GAP_MS` (20 by default) paces its steps.
//! - `background <cmd>`, `wait <secs>`, `monitor`, `signals`, `dialog <kind>`, `elicit`, `write
//!   <path> <words>` and `plan`: the doors beside messages (`doors`).
//! - anything else: a thinking block, then `Done: <prompt>`; for a batch of Slopty's reports
//!   (`<slopty-reports>`), `Done: <its last line>`.
//!
//! A user message with `shouldQuery: false` (a `!` command's output not to be answered) is kept
//! and never echoed, and its result comes at once, with no turn.
//!
//! Control requests: `interrupt` (answered after `STUB_SLOW_INTERRUPT_MS`, 0 by default; it ends
//! the turn under way as Claude Code does), `set_model`, `set_permission_mode`,
//! `apply_flag_settings` (`effortLevel`, `fastMode`, shown in the next `init`), `stop_task` (a
//! subagent or a background command), `get_context_usage`, and the doors' (`background_tasks`,
//! `rewind_files`, `list_permission_rules`, `mcp_status`, `mcp_read_resource`, `mcp_call`,
//! `side_question`, `ui_*`); any other subtype is answered with an error.
//!
//! The conversation goes where Claude Code keeps it, `~/.claude/projects/<cwd with every
//! character but letters and digits made '-'>/<session>.jsonl`, so `--resume <id>` (with
//! `--fork-session`, into a new one, through `--resume-session-at`'s record when given) goes on
//! from it; a `--resume` of a session with no file ends
//! at once with an error result and exit status 1. With `CLAUDE_CODE_SDK_READS_SESSION_STATE` or
//! `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` set it says `session_state_changed`.
//!
//! Failures, for the adapter's tests: `STUB_CRASH_MID_TURN` exits 3 right after the first
//! assistant output of its first turn; `STUB_NO_ECHO` never says `command_lifecycle` nor echoes a
//! message (the turn still runs); `STUB_MALFORMED` writes a line that is no JSON and one cut short
//! first; `STUB_NEVER_ANSWER_INIT` leaves `initialize` unanswered; `STUB_EXIT_ON_START=<code>` says
//! so on stderr and exits with `code` at once. `STUB_TURN_GAP_MS` (20 by default) paces a turn's
//! steps. Orca's faults: `STUB_ACK_WITHOUT_ENDING` answers an `interrupt` at once and goes on
//! with the turn; `STUB_IGNORE_SIGTERM` runs with `SIGTERM` ignored (it starts itself again
//! under a shell that ignores it, keeping its pid); `STUB_GRANDCHILD=<secs>` leaves a process of
//! its own group behind that holds its standard output for that long after it ends;
//! `STUB_DEAF` stops reading its standard input once it heard its first message, and goes on.
//!
//! What it was started with goes to `STUB_RECORD` as one JSON document, replaced whole as it
//! hears each line: its pid, arguments, directory, `SLOPTY_*` and `CLAUDE_*` environment, every
//! variable's name with a digest of its value, every line it was sent (`received`), and its tools'
//! results (`mcp`). In `STUB_RECORD`, `{session}` is the Slopty seat it runs in (`SLOPTY_SESSION`,
//! `none` outside one), `{conversation}` the session it was started on and `{pid}` its own pid, so
//! one setting serves every agent a machine runs, a fork of a conversation too.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

mod doors;

/// What it says its version is.
pub const VERSION: &str = "2.1.302-stub";

/// The pause between two deltas of a `slow` turn.
const SLOW_STEP: Duration = Duration::from_millis(200);

/// Whether `args` ask for stream-json input: the driven mode this module serves.
#[must_use]
pub fn wanted(args: &[String]) -> bool {
    args.iter().enumerate().any(|(at, arg)| {
        arg == "--input-format=stream-json"
            || (arg == "--input-format"
                && args.get(at.saturating_add(1)).is_some_and(|next| next == "stream-json"))
    })
}

/// Run the stand-in on this process's stdin and stdout until stdin ends.
#[must_use]
pub fn run(args: &[String]) -> ExitCode {
    if let Some(code) = knob("STUB_EXIT_ON_START", args) {
        let mut err = std::io::stderr().lock();
        drop(writeln!(err, "claude: this install is broken (STUB_EXIT_ON_START)"));
        return ExitCode::from(code.parse::<u8>().unwrap_or(1));
    }
    if knob("STUB_IGNORE_SIGTERM", args).is_some() && std::env::var_os(SIGTERM_IGNORED).is_none() {
        return ignoring_sigterm(args);
    }
    if let Some(secs) = knob("STUB_GRANDCHILD", args) {
        hold_stdout(&secs);
    }
    let mut stub = match Stub::new(args) {
        Ok(stub) => stub,
        Err(code) => return code,
    };
    stub.serve()
}

/// How long a tool call through the `slopty` MCP server may take.
const MCP_PATIENCE: Duration = Duration::from_secs(60);

/// Where a tool call's arguments name the agent the turn last started.
const STARTED_PLACE: &str = concat!("{", "started", "}");

/// Set once the stand-in runs again with `SIGTERM` ignored ([`ignoring_sigterm`]).
const SIGTERM_IGNORED: &str = "STUB_SIGTERM_IGNORED";

/// Run again as this same process (`exec`, the pid kept) under a shell that ignores `SIGTERM`:
/// an ignored signal stays ignored across `exec`, so the stand-in that comes back cannot be
/// ended by it. Returns only when the `exec` failed.
fn ignoring_sigterm(args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt as _;
    let Ok(me) = std::env::current_exe() else { return ExitCode::from(2) };
    let failed = Command::new("/bin/sh")
        .arg("-c")
        .arg("trap '' TERM; exec \"$0\" \"$@\"")
        .arg(me)
        .args(args)
        .env(SIGTERM_IGNORED, "1")
        .exec();
    let mut err = std::io::stderr().lock();
    drop(writeln!(err, "claude: could not ignore SIGTERM: {failed}"));
    ExitCode::from(2)
}

/// Leave behind a `sleep` of `secs` seconds in a process group of its own, holding this
/// process's standard output: its reader sees no end of it until the sleep ends, whatever
/// becomes of the stand-in.
fn hold_stdout(secs: &str) {
    use std::os::unix::process::CommandExt as _;
    let held = Command::new("sleep")
        .arg(secs)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(e) = held {
        let mut err = std::io::stderr().lock();
        drop(writeln!(err, "claude: no grandchild: {e}"));
    }
}

/// The variable `name`, with `{conversation}` the session the stand-in was started on and
/// `{session}` the Slopty seat it runs in.
fn knob(name: &str, args: &[String]) -> Option<String> {
    let conversation = flag(args, "--session-id").or_else(|| flag(args, "--resume"));
    let seat = std::env::var("SLOPTY_SESSION").unwrap_or_else(|_| "none".to_owned());
    std::env::var(name).ok().map(|value| {
        value
            .replace(concat!("{", "conversation", "}"), conversation.as_deref().unwrap_or("none"))
            .replace(concat!("{", "session", "}"), &seat)
            .replace(concat!("{", "pid", "}"), &std::process::id().to_string())
    })
}

/// The value after `name` in `args`, as `--name value` or `--name=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.iter().enumerate().find_map(|(at, arg)| {
        if arg == name {
            args.get(at.saturating_add(1)).cloned()
        } else {
            arg.strip_prefix(&prefix).map(str::to_owned)
        }
    })
}

/// A number of milliseconds from the variable `name`, else `default`.
fn millis(name: &str, default: u64) -> Duration {
    Duration::from_millis(
        std::env::var(name).ok().and_then(|ms| ms.parse().ok()).unwrap_or(default),
    )
}

/// One line heard on stdin.
enum Heard {
    Line(Value),
    End,
}

/// How a turn ended.
enum Ended {
    /// It ran to its result, with this text.
    Done(String),
    /// An interrupt ended it.
    Interrupted,
    /// It met a usage limit.
    Limited,
    /// A local command ran, by this name.
    Local(String, String),
}

/// Why a wait inside a turn stopped early.
enum Cut {
    /// An interrupt came due.
    Interrupt,
}

/// The stand-in's state.
#[expect(clippy::struct_excessive_bools, reason = "the stand-in's knobs, each a plain switch")]
struct Stub {
    args: Vec<String>,
    heard: Receiver<Heard>,
    session: String,
    cwd: String,
    transcript: PathBuf,
    /// The last record of the conversation, which the next one follows.
    last: Option<String>,
    model: String,
    mode: String,
    effort: Option<String>,
    fast: bool,
    partial: bool,
    states: bool,
    no_echo: bool,
    crash: bool,
    never_init: bool,
    /// An `interrupt` is answered and the turn goes on (`STUB_ACK_WITHOUT_ENDING`).
    ack_only: bool,
    gap: Duration,
    /// User messages waiting for their turn.
    queue: VecDeque<Value>,
    /// Every `can_use_tool` asked and not yet answered, by request id.
    asked: BTreeMap<String, Value>,
    /// Answers heard for requests, by request id.
    answers: BTreeMap<String, Value>,
    /// An interrupt heard during a turn: when it is answered, and its request id.
    interrupt: Option<(Instant, String)>,
    /// The subagent waiting, by task id, and whether a `stop_task` named it.
    task: Option<(String, bool)>,
    /// Until when (epoch seconds) a usage limit holds.
    limited_until: Option<u64>,
    running: bool,
    turns: u64,
    record: Option<PathBuf>,
    received: Vec<Value>,
    /// The `--mcp-config` documents it was started with.
    mcp_configs: Vec<Value>,
    /// What `STUB_TURNS` scripts.
    tool_turns: Value,
    /// Every tool result, in order.
    mcp: Vec<Value>,
    /// Every dialog asked and not yet answered, by request id.
    dialogs: BTreeMap<String, Value>,
    /// What the doors beside messages keep.
    doors: doors::Doors,
}

impl Stub {
    fn new(args: &[String]) -> Result<Self, ExitCode> {
        let cwd = std::env::current_dir()
            .map(|dir| dir.to_string_lossy().into_owned())
            .unwrap_or_default();
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let project = home.join(".claude").join("projects").join(escaped(&cwd));
        let resume = flag(args, "--resume");
        let fork = args.iter().any(|a| a == "--fork-session");
        let resume_at = flag(args, "--resume-session-at");
        let session = match (&resume, fork) {
            (Some(id), false) => id.clone(),
            _ => flag(args, "--session-id").unwrap_or_else(uuid),
        };
        let transcript = project.join(format!("{session}.jsonl"));
        let mut last = None;
        if let Some(id) = &resume {
            let from = project.join(format!("{id}.jsonl"));
            let Ok(text) = std::fs::read_to_string(&from) else {
                let mut out = std::io::stdout().lock();
                let failed = json!({
                    "type": "result", "subtype": "error_during_execution", "is_error": true,
                    "errors": [format!("No conversation found with session ID: {id}")],
                    "session_id": id, "uuid": uuid(),
                });
                drop(writeln!(out, "{failed}"));
                return Err(ExitCode::FAILURE);
            };
            // A fork through a record keeps the conversation up to it, and no further.
            let text = match (&resume_at, fork) {
                (Some(at), true) => through(&text, at),
                _ => text,
            };
            last = text
                .lines()
                .rev()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find_map(|record| record.get("uuid").and_then(Value::as_str).map(str::to_owned));
            if fork {
                drop(std::fs::create_dir_all(&project));
                drop(std::fs::write(&transcript, text.replace(id.as_str(), &session)));
            }
        }
        let (tx, heard) = mpsc::channel();
        let deaf = std::env::var_os("STUB_DEAF").is_some();
        drop(std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                let prompt = value.get("type").and_then(Value::as_str) == Some("user");
                if tx.send(Heard::Line(value)).is_err() {
                    return;
                }
                if deaf && prompt {
                    // Reads nothing more, and never says its input ended.
                    let _kept = Box::leak(Box::new(tx));
                    return;
                }
            }
            drop(tx.send(Heard::End));
        }));
        let set = |name: &str| std::env::var_os(name).is_some();
        let configs = mcp_configs(args, &cwd);
        let stub = Self {
            args: args.to_vec(),
            heard,
            session,
            cwd,
            transcript,
            last,
            model: flag(args, "--model").unwrap_or_else(|| "default".to_owned()),
            mode: flag(args, "--permission-mode").unwrap_or_else(|| "default".to_owned()),
            effort: flag(args, "--effort"),
            fast: false,
            partial: args.iter().any(|a| a == "--include-partial-messages"),
            states: set("CLAUDE_CODE_SDK_READS_SESSION_STATE")
                || set("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS"),
            no_echo: set("STUB_NO_ECHO"),
            crash: set("STUB_CRASH_MID_TURN"),
            never_init: set("STUB_NEVER_ANSWER_INIT"),
            ack_only: set("STUB_ACK_WITHOUT_ENDING"),
            gap: millis("STUB_TURN_GAP_MS", 20),
            queue: VecDeque::new(),
            asked: BTreeMap::new(),
            answers: BTreeMap::new(),
            interrupt: None,
            task: None,
            limited_until: None,
            running: false,
            turns: 0,
            record: knob("STUB_RECORD", args).map(PathBuf::from),
            received: Vec::new(),
            mcp_configs: configs,
            tool_turns: std::env::var("STUB_TURNS")
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Value::Null),
            mcp: Vec::new(),
            dialogs: BTreeMap::new(),
            doors: doors::Doors::new(),
        };
        stub.save();
        if set("STUB_MALFORMED") {
            let mut out = std::io::stdout().lock();
            drop(writeln!(out, "this is no JSON at all"));
            drop(writeln!(out, "{{\"type\":\"assistant\",\"message\":{{\"id\":"));
            drop(out.flush());
        }
        Ok(stub)
    }

    /// Write [`Self::record`] whole.
    fn save(&self) {
        let Some(at) = &self.record else { return };
        let env: Map<String, Value> = std::env::vars()
            .filter(|(k, _)| k.starts_with("SLOPTY_") || k.starts_with("CLAUDE_"))
            .map(|(k, v)| (k, Value::String(v)))
            .collect();
        let doc = json!({
            "pid": std::process::id(),
            "argv": self.args,
            "cwd": self.cwd,
            "env": env,
            "inherited": crate::env::digests(),
            "received": self.received,
            "mcp": self.mcp,
        });
        let staged = at.with_extension("partial");
        if std::fs::write(&staged, doc.to_string()).is_ok() {
            drop(std::fs::rename(&staged, at));
        }
    }

    /// Hear lines and run turns until stdin ends.
    fn serve(&mut self) -> ExitCode {
        loop {
            if let Some(message) = self.queue.pop_front() {
                self.turn(&message);
                continue;
            }
            match self.heard.recv() {
                Ok(Heard::Line(line)) => self.hear(line),
                Ok(Heard::End) | Err(_) => return ExitCode::SUCCESS,
            }
        }
    }

    /// Write a `system` record of `subtype` with `fields`.
    fn system(&self, subtype: &str, fields: &Value) {
        let mut record = json!({
            "type": "system", "subtype": subtype, "session_id": self.session, "uuid": uuid(),
        });
        merge(&mut record, fields);
        say(&record);
    }

    fn state(&self, state: &str) {
        if self.states {
            self.system("session_state_changed", &json!({ "state": state }));
        }
    }

    /// Append `record` to the conversation's file, following the last one.
    fn keep(&mut self, record: &Value) {
        let mut kept = record.clone();
        let id = kept.get("uuid").and_then(Value::as_str).map_or_else(uuid, str::to_owned);
        merge(
            &mut kept,
            &json!({
                "uuid": id, "parentUuid": self.last, "sessionId": self.session,
                "cwd": self.cwd, "timestamp": timestamp(), "isSidechain": false,
                "userType": "external", "entrypoint": "sdk-cli", "version": VERSION,
            }),
        );
        if let Some(object) = kept.as_object_mut() {
            object.remove("session_id");
            object.remove("isReplay");
        }
        if let Some(dir) = self.transcript.parent() {
            drop(std::fs::create_dir_all(dir));
        }
        if let Ok(mut file) =
            std::fs::OpenOptions::new().create(true).append(true).open(&self.transcript)
        {
            drop(writeln!(file, "{kept}"));
        }
        self.last = Some(id);
    }

    fn lifecycle(&self, command: &str, state: &str) {
        if !self.no_echo {
            say(&json!({
                "type": "command_lifecycle", "command_uuid": command, "state": state,
                "uuid": uuid(), "session_id": self.session,
            }));
        }
    }

    /// Act on one line from the host.
    fn hear(&mut self, line: Value) {
        self.received.push(line.clone());
        self.save();
        match line.get("type").and_then(Value::as_str) {
            Some("user") => {
                let id = line.get("uuid").and_then(Value::as_str).map_or_else(uuid, str::to_owned);
                self.lifecycle(&id, "queued");
                let mut message = line;
                merge(&mut message, &json!({ "uuid": id }));
                self.queue.push_back(message);
            }
            Some("control_request") => self.control(&line),
            Some("control_response") => {
                let response = line.get("response").cloned().unwrap_or(Value::Null);
                if let Some(id) = response.get("request_id").and_then(Value::as_str) {
                    self.asked.remove(id);
                    self.answers.insert(id.to_owned(), response.clone());
                }
            }
            _ => {}
        }
    }

    /// Answer a control request.
    fn control(&mut self, line: &Value) {
        let id = line.get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let request = line.get("request").cloned().unwrap_or(Value::Null);
        let field = |name: &str| request.get(name).and_then(Value::as_str).map(str::to_owned);
        match field("subtype").as_deref() {
            Some("initialize") if self.never_init => {}
            Some("initialize") => self.initialize(&id),
            Some("interrupt") if self.running && self.ack_only => {
                reply(&id, Some(json!({ "still_queued": [] })));
            }
            Some("interrupt") if self.running => {
                let due = Instant::now()
                    .checked_add(millis("STUB_SLOW_INTERRUPT_MS", 0))
                    .unwrap_or_else(Instant::now);
                self.interrupt = Some((due, id));
            }
            Some("interrupt") => reply(&id, Some(json!({ "still_queued": [] }))),
            Some("set_model") => {
                let model = field("model").unwrap_or_else(|| "default".to_owned());
                reply(&id, None);
                let said =
                    format!("<local-command-stdout>Set model to `{model}`</local-command-stdout>");
                say(&json!({
                    "type": "user", "message": { "role": "user", "content": said },
                    "session_id": self.session, "parent_tool_use_id": null, "uuid": uuid(),
                    "timestamp": timestamp(), "isReplay": true,
                }));
                self.model = model;
            }
            Some("set_permission_mode") => {
                let mode = field("mode").unwrap_or_else(|| "default".to_owned());
                reply(&id, Some(json!({ "mode": mode })));
                self.system("status", &json!({ "status": null, "permissionMode": mode }));
                self.mode = mode;
            }
            Some("apply_flag_settings") => {
                let settings = request.get("settings").cloned().unwrap_or(Value::Null);
                if let Some(effort) = settings.get("effortLevel").and_then(Value::as_str) {
                    self.effort = Some(effort.to_owned());
                }
                if let Some(fast) = settings.get("fastMode").and_then(Value::as_bool) {
                    self.fast = fast;
                }
                reply(&id, None);
            }
            Some("stop_task") => {
                let named = field("task_id").unwrap_or_default();
                if let Some((task, stopped)) = &mut self.task
                    && *task == named
                {
                    *stopped = true;
                    reply(&id, None);
                } else if !self.stop_running(&id, &named) {
                    refuse(&id, "No running task with that id");
                }
            }
            Some("get_context_usage") => reply(
                &id,
                Some(json!({
                    "categories": [
                        { "name": "System prompt", "tokens": 1100, "kind": "used" },
                        { "name": "Messages", "tokens": 262, "kind": "used" },
                        { "name": "Free space", "tokens": 198_638, "kind": "free" },
                    ],
                    "totalTokens": 1362, "maxTokens": 200_000, "rawMaxTokens": 200_000,
                    "percentage": 1, "autoCompactThreshold": 167_000, "isAutoCompactEnabled": true,
                })),
            ),
            Some(other) if self.door(&id, other, &request) => {}
            Some(other) => refuse(&id, &format!("Unsupported control request: {other}")),
            None => refuse(&id, "A control request names no subtype"),
        }
    }

    fn initialize(&self, id: &str) {
        let state = if !self.asked.is_empty() {
            "requires_action"
        } else if self.running {
            "running"
        } else {
            "idle"
        };
        let models: Vec<Value> = ["default", "opus", "sonnet", "haiku"]
            .iter()
            .map(|value| {
                json!({
                    "value": value, "resolvedModel": resolved(value),
                    "displayName": display(value), "description": format!("{} (stand-in)", display(value)),
                    "supportsEffort": true,
                    "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"],
                    "supportsFastMode": *value != "haiku",
                })
            })
            .collect();
        let response = json!({
            "commands": commands(),
            "models": models,
            "current_permission_mode": self.mode,
            "session_state": state,
            "fast_mode_state": if self.fast { "on" } else { "off" },
            "claude_code_version": VERSION,
            "pid": std::process::id(),
            "account": { "tokenSource": "none", "apiKeySource": "none", "apiProvider": "firstParty" },
        });
        say(&json!({
            "type": "control_response",
            "response": {
                "subtype": "success", "request_id": id, "response": response,
                "pending_permission_requests": self.asked.values().collect::<Vec<_>>(),
                "pending_user_dialog_requests": self.dialogs.values().collect::<Vec<_>>(),
            },
        }));
    }

    /// Wait `pause`, hearing what comes meanwhile; cut short by an interrupt come due.
    fn pause(&mut self, pause: Duration) -> Result<(), Cut> {
        let until = Instant::now().checked_add(pause).unwrap_or_else(Instant::now);
        loop {
            if let Some((due, _)) = &self.interrupt
                && *due <= Instant::now()
            {
                return Err(Cut::Interrupt);
            }
            let now = Instant::now();
            if now >= until {
                return Ok(());
            }
            let mut wait = until.saturating_duration_since(now);
            if let Some((due, _)) = &self.interrupt {
                wait = wait.min(due.saturating_duration_since(now));
            }
            match self.heard.recv_timeout(wait) {
                Ok(Heard::Line(line)) => self.hear(line),
                Ok(Heard::End) | Err(RecvTimeoutError::Disconnected) => end(),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Ask the host about a tool call and wait for its answer.
    fn ask(
        &mut self,
        tool: &str,
        input: &Value,
        tool_use: &str,
        extra: &Value,
    ) -> Result<Value, Cut> {
        let mut request = json!({
            "subtype": "can_use_tool", "tool_name": tool, "display_name": tool,
            "input": input, "tool_use_id": tool_use,
        });
        merge(&mut request, extra);
        self.request(&request, false)
    }

    /// One assistant message of one content block, streamed first when asked to.
    fn assistant(&mut self, block: &Value, parent: Option<&str>, agent: Option<&str>) {
        if self.partial && parent.is_none() {
            let event = |event: Value| {
                json!({
                    "type": "stream_event", "event": event, "session_id": self.session,
                    "parent_tool_use_id": null, "uuid": uuid(),
                })
            };
            let empty = match block.get("type").and_then(Value::as_str) {
                Some("text") => json!({ "type": "text", "text": "" }),
                Some("thinking") => json!({ "type": "thinking", "thinking": "", "signature": "" }),
                _ => block.clone(),
            };
            say(&event(
                json!({ "type": "content_block_start", "index": 0, "content_block": empty }),
            ));
            if let Some(text) = block.get("text").and_then(Value::as_str) {
                let delta = json!({ "type": "text_delta", "text": text });
                say(&event(json!({ "type": "content_block_delta", "index": 0, "delta": delta })));
            }
            if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                let delta = json!({ "type": "thinking_delta", "thinking": thinking });
                say(&event(json!({ "type": "content_block_delta", "index": 0, "delta": delta })));
            }
            say(&event(json!({ "type": "content_block_stop", "index": 0 })));
        }
        let mut message = json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_{}", self.turns), "type": "message", "role": "assistant",
                "model": resolved(&self.model), "content": [block], "stop_reason": null,
                "stop_sequence": null, "usage": { "input_tokens": 12, "output_tokens": 8 },
            },
            "parent_tool_use_id": parent, "session_id": self.session, "uuid": uuid(),
            "timestamp": timestamp(),
        });
        if let Some(agent) = agent {
            merge(&mut message, &json!({ "agent_id": agent }));
        }
        say(&message);
        if parent.is_none() {
            self.keep(&message);
        }
        if self.crash {
            crash();
        }
    }

    /// A user record carrying a tool's result.
    fn tool_result(&mut self, tool_use: &str, content: &str, error: bool) {
        self.tool_result_with(tool_use, content, error, &Value::Null);
    }

    /// A user record carrying a tool's result, and what the tool said of it besides the text
    /// (`tool_use_result` as streamed, `toolUseResult` as kept), when it says anything.
    fn tool_result_with(&mut self, tool_use: &str, content: &str, error: bool, structured: &Value) {
        let mut record = json!({
            "type": "user",
            "message": { "role": "user", "content": [{
                "tool_use_id": tool_use, "type": "tool_result", "content": content, "is_error": error,
            }]},
            "parent_tool_use_id": null, "session_id": self.session, "uuid": uuid(),
            "timestamp": timestamp(),
        });
        if structured.is_null() {
            say(&record);
        } else {
            let mut streamed = record.clone();
            merge(&mut streamed, &json!({ "tool_use_result": structured }));
            say(&streamed);
            merge(&mut record, &json!({ "toolUseResult": structured }));
        }
        self.keep(&record);
        self.system("status", &json!({ "status": "requesting" }));
    }

    /// Say a local command's output, as Claude Code echoes it.
    fn local(&mut self, said: &str) {
        let record = json!({
            "type": "user", "message": { "role": "user", "content": said },
            "session_id": self.session, "parent_tool_use_id": null, "uuid": uuid(),
            "timestamp": timestamp(), "isReplay": true,
        });
        say(&record);
        self.keep(&record);
    }

    /// Run one user message's turn.
    fn turn(&mut self, message: &Value) {
        let id = message.get("uuid").and_then(Value::as_str).map_or_else(uuid, str::to_owned);
        self.doors.prompted(id.clone());
        if message.get("shouldQuery") == Some(&Value::Bool(false)) {
            self.taken_in(message, &id);
            return;
        }
        let text = prompt(message);
        self.running = true;
        self.turns = self.turns.saturating_add(1);
        self.lifecycle(&id, "started");
        self.state("running");
        let mut capabilities = vec!["interrupt_receipt_v1", "msg_lifecycle_v1"];
        if self.states {
            capabilities.push("session_state_v1");
        }
        self.system(
            "init",
            &json!({
                "cwd": self.cwd, "model": resolved(&self.model), "permissionMode": self.mode,
                "tools": ["Agent", "AskUserQuestion", "Bash", "Edit", "Read", "Write"],
                "slash_commands": commands().iter().filter_map(|c| c.get("name").cloned()).collect::<Vec<_>>(),
                "capabilities": capabilities, "claude_code_version": VERSION,
                "effort": self.effort, "fast_mode_state": if self.fast { "on" } else { "off" },
                "user_message_uuids": [id],
            }),
        );
        self.system("status", &json!({ "status": "requesting", "user_message_uuids": [id] }));
        let mut echo = message.clone();
        merge(
            &mut echo,
            &json!({ "session_id": self.session, "parent_tool_use_id": null, "timestamp": timestamp() }),
        );
        if !self.no_echo {
            let mut replayed = echo.clone();
            merge(&mut replayed, &json!({ "isReplay": true }));
            say(&replayed);
        }
        self.keep(&echo);
        let ended = self.scripted(&text);
        self.running = false;
        let (subtype, error, result, extra, lifecycle) = match ended {
            Ended::Done(said) => (
                "success",
                false,
                said,
                json!({ "terminal_reason": "completed", "stop_reason": "end_turn" }),
                "completed",
            ),
            Ended::Limited => {
                let said = limit_text(self.limited_until.unwrap_or_default());
                (
                    "success",
                    true,
                    said,
                    json!({ "api_error_status": 429, "terminal_reason": "completed" }),
                    "completed",
                )
            }
            Ended::Local(name, said) => (
                "success",
                false,
                said,
                json!({ "local_command": name, "num_turns": 0 }),
                "completed",
            ),
            Ended::Interrupted => {
                if let Some((_, request)) = self.interrupt.take() {
                    reply(&request, Some(json!({ "still_queued": [] })));
                }
                let record = json!({
                    "type": "user",
                    "message": { "role": "user", "content": [{ "type": "text", "text": "[Request interrupted by user]" }] },
                    "parent_tool_use_id": null, "session_id": self.session, "uuid": uuid(),
                    "timestamp": timestamp(),
                });
                say(&record);
                self.keep(&record);
                let extra = json!({
                    "terminal_reason": "aborted_streaming", "stop_reason": null,
                    "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=null"],
                });
                ("error_during_execution", true, String::new(), extra, "cancelled")
            }
        };
        let mut result = json!({
            "type": "result", "subtype": subtype, "is_error": error, "result": result,
            "duration_ms": 40, "duration_api_ms": 30, "num_turns": 1,
            "session_id": self.session, "total_cost_usd": 0.0001,
            "usage": { "input_tokens": 12, "output_tokens": 20, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0 },
            "modelUsage": {}, "permission_denials": [], "uuid": uuid(),
            "user_message_uuid": id, "user_message_uuids": [id],
        });
        merge(&mut result, &extra);
        if subtype == "error_during_execution"
            && let Some(object) = result.as_object_mut()
        {
            object.remove("result");
        }
        say(&result);
        self.lifecycle(&id, lifecycle);
        self.state("idle");
    }

    /// The turn `text` names, up to how it ended.
    fn scripted(&mut self, text: &str) -> Ended {
        let body = text.trim().strip_prefix("scenario:").unwrap_or(text).trim().to_owned();
        let (word, rest) = body.split_once(char::is_whitespace).unwrap_or((body.as_str(), ""));
        let rest = rest.trim().to_owned();
        let now = epoch_s();
        if self.limited_until.is_some_and(|until| until > now) {
            return self.limited();
        }
        self.limited_until = None;
        let calls = self.tool_turns.get(body.as_str()).and_then(Value::as_array).cloned();
        let replay = replay_of(&body);
        let steps = match word {
            _ if replay.is_some() => self.replayed(&body, replay.as_deref().unwrap_or(&body)),
            _ if calls.is_some() => self.tools(&body, &calls.unwrap_or_default()),
            "bash" => self.bash(&rest),
            "ask" => self.question(),
            "agent" => self.agent(),
            "slow" => self.slow(rest.parse().unwrap_or(10)),
            "retry" => self.retried(rest.parse().unwrap_or(3), &body),
            "background" => self.background_command(&rest),
            "wait" => self.waited(rest.parse().unwrap_or(10)),
            "monitor" => self.monitor(),
            "signals" => self.signals(),
            "dialog" => self.dialog(&rest),
            "elicit" => self.elicit(),
            "write" => self.write_file(&rest),
            "plan" => self.plan(),
            "limit" => {
                let reset = std::env::var("STUB_LIMIT_RESET_S").ok().and_then(|s| s.parse().ok());
                self.limited_until = Some(now.saturating_add(reset.unwrap_or(3)));
                return self.limited();
            }
            "/compact" => return self.compact(),
            command if command.starts_with('/') => {
                let name = command.trim_start_matches('/').to_owned();
                let said = format!("<local-command-stdout>Ran /{name}</local-command-stdout>");
                self.local(&said);
                return Ended::Local(name, String::new());
            }
            _ => self.plain(&body),
        };
        match steps {
            Ok(said) => Ended::Done(said),
            Err(Cut::Interrupt) => Ended::Interrupted,
        }
    }

    /// Call `calls` in order through the `slopty` MCP server, each a tool call with its result,
    /// then answer as [`Self::plain`] does.
    fn tools(&mut self, body: &str, calls: &[Value]) -> Result<String, Cut> {
        let mut started: Option<String> = None;
        for (n, call) in calls.iter().enumerate() {
            let call = match &started {
                Some(agent) => {
                    serde_json::from_str(&call.to_string().replace(STARTED_PLACE, agent))
                        .unwrap_or_else(|_| call.clone())
                }
                None => call.clone(),
            };
            let name = call.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            let tool_use = format!("toolu_mcp_{}_{n}", self.turns);
            let input = call.get("arguments").cloned().unwrap_or(Value::Null);
            let block = json!({
                "type": "tool_use", "id": tool_use, "name": format!("mcp__slopty__{name}"),
                "input": input,
            });
            self.assistant(&block, None, None);
            let answer = call_tool(&self.mcp_configs, &call)
                .unwrap_or_else(|why| json!({ "isError": true, "error": why }));
            if name == "agent_start" {
                started = answered_agent(&answer).or(started);
            }
            let text =
                answer.pointer("/content/0/text").and_then(Value::as_str).unwrap_or_default();
            let failed = answer.get("isError").and_then(Value::as_bool).unwrap_or(false);
            self.tool_result(&tool_use, text, failed);
            self.mcp.push(answer);
            self.save();
            self.pause(self.gap)?;
        }
        let said = format!("Done: {body}");
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    /// Play the recording `fixture` names as this turn ([`crate::replay`]), its prompt
    /// `prompt`.
    fn replayed(&mut self, prompt: &str, fixture: &str) -> Result<String, Cut> {
        let recording = match crate::replay::Recording::load(&crate::replay::fixture(fixture)) {
            Ok(recording) => recording,
            Err(e) => {
                let said = format!("No recording {fixture}: {e}");
                self.assistant(&json!({ "type": "text", "text": said }), None, None);
                return Ok(said);
            }
        };
        let to = crate::replay::Session {
            id: self.session.clone(),
            transcript: self.transcript.clone(),
            cwd: self.cwd.clone(),
            prompt: prompt.to_owned(),
            now_ms: epoch_s().saturating_mul(1000),
        };
        let gap = millis("STUB_REPLAY_GAP_MS", 20);
        // The `Agent` call each subagent's file answers, found by the prompt it was given.
        let mut parents: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut calls: Vec<(String, String)> = Vec::new();
        let mut said = String::new();
        let mut prompted = false;
        for step in recording.schedule(&to) {
            match step {
                crate::replay::Step::Write { to: file, lines } => {
                    let main = file == self.transcript;
                    for line in lines {
                        let Ok(record) = serde_json::from_str::<Value>(&line) else { continue };
                        if main && !prompted && is_prompt(&record) {
                            // The stand-in echoed and kept the person's own already.
                            prompted = true;
                            continue;
                        }
                        append(&file, &line);
                        if main {
                            calls.extend(agent_calls(&record));
                            if let Some(text) = text_of(&record) {
                                said = text;
                            }
                        } else if !parents.contains_key(&file)
                            && let Some(asked) =
                                record.pointer("/message/content").and_then(Value::as_str)
                            && let Some((id, _)) = calls.iter().find(|(_, p)| p == asked)
                        {
                            parents.insert(file.clone(), id.clone());
                        }
                        if let Some(id) = record.get("uuid").and_then(Value::as_str)
                            && main
                        {
                            self.last = Some(id.to_owned());
                        }
                        let parent = if main { None } else { parents.get(&file).cloned() };
                        if let Some(streamed) = streamed(&record, &self.session, parent.as_deref())
                        {
                            say(&streamed);
                        }
                    }
                }
                crate::replay::Step::Hook { payload, waits: true } => {
                    let field = |name: &str| payload.get(name).cloned().unwrap_or(Value::Null);
                    let tool = field("tool_name");
                    let tool_use = field("tool_use_id");
                    let extra =
                        json!({ "permission_suggestions": field("permission_suggestions") });
                    self.ask(
                        tool.as_str().unwrap_or_default(),
                        &field("tool_input"),
                        tool_use.as_str().unwrap_or_default(),
                        &extra,
                    )?;
                }
                crate::replay::Step::Hook { .. } => {}
            }
            self.pause(gap)?;
        }
        Ok(said)
    }

    /// The request fails `times` times, each tried again, then the turn answers.
    fn retried(&mut self, times: u64, body: &str) -> Result<String, Cut> {
        for attempt in 1..=times {
            self.pause(self.gap.saturating_mul(5))?;
            self.system(
                "api_retry",
                &json!({
                    "attempt": attempt, "max_retries": 10,
                    "retry_delay_ms": attempt.saturating_mul(100), "error_status": 529,
                    "error": { "type": "overloaded_error", "message": "Overloaded" },
                }),
            );
        }
        self.plain(body)
    }

    fn plain(&mut self, body: &str) -> Result<String, Cut> {
        let thinking = json!({ "type": "thinking", "thinking": "The person wants an answer.", "signature": "c2ln" });
        self.assistant(&thinking, None, None);
        self.pause(self.gap)?;
        // A batch of Slopty's reports is answered by its last word, as one line.
        let words = if body.trim_start().starts_with("<slopty-reports>") {
            body.lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty() && !line.contains("slopty-reports>"))
                .unwrap_or(body)
        } else {
            body
        };
        let said = format!("Done: {words}");
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    fn bash(&mut self, command: &str) -> Result<String, Cut> {
        self.assistant(&json!({ "type": "text", "text": "Let me run it." }), None, None);
        self.pause(self.gap)?;
        let tool_use = format!("toolu_b{}", self.turns);
        let input = json!({ "command": command, "description": "Run a command" });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "Bash", "input": input }),
            None,
            None,
        );
        let extra = json!({
            "description": "Run a command",
            "decision_reason_type": "rule",
            "matched_ask_rule": "Bash",
            "decision_reason_rule_source": "localSettings",
            "permission_suggestions": [{
                "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": command }],
                "behavior": "allow", "destination": "localSettings",
            }],
        });
        let answer = self.ask("Bash", &input, &tool_use, &extra)?;
        let (allowed, message) = verdict(&answer);
        if allowed {
            // What it changed in the folder, as Claude Code takes it while a command runs.
            let made = std::path::Path::new(&self.cwd).join("made");
            let structured = json!({
                "stdout": format!("ran {command}"), "stderr": "", "interrupted": false,
                "bashEditDiff": {
                    "changedFiles": [made],
                    "files": [{ "filePath": made, "created": true, "hunks": [{
                        "oldStart": 0, "oldLines": 0, "newStart": 1, "newLines": 1,
                        "lines": ["+made"],
                    }] }],
                    "moreFiles": 0,
                },
            });
            self.tool_result_with(&tool_use, &format!("ran {command}"), false, &structured);
        } else {
            self.tool_result(&tool_use, &message, true);
        }
        self.pause(self.gap)?;
        let said = if allowed { "Done: ran it." } else { "Done: it was not allowed." };
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    fn question(&mut self) -> Result<String, Cut> {
        let tool_use = format!("toolu_q{}", self.turns);
        let input = json!({ "questions": [{
            "question": "Which color?", "header": "Color", "multiSelect": false,
            "options": [
                { "label": "Red", "description": "warm" },
                { "label": "Blue", "description": "cool" },
            ],
        }]});
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "AskUserQuestion", "input": input }),
            None,
            None,
        );
        let answer = self.ask(
            "AskUserQuestion",
            &input,
            &tool_use,
            &json!({ "requires_user_interaction": true }),
        )?;
        let (allowed, message) = verdict(&answer);
        if allowed {
            let answers = answer
                .pointer("/response/updatedInput/answers")
                .and_then(Value::as_object)
                .map(|answers| {
                    answers
                        .iter()
                        .map(|(q, a)| format!("\"{q}\"=\"{}\"", a.as_str().unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let said = format!(
                "Your questions have been answered: {answers}. You can now continue with these answers in mind."
            );
            self.tool_result(&tool_use, &said, false);
        } else {
            self.tool_result(&tool_use, &message, true);
        }
        self.pause(self.gap)?;
        let said = "Done: noted.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    fn agent(&mut self) -> Result<String, Cut> {
        let tool_use = format!("toolu_a{}", self.turns);
        let task = format!("a{:016x}", self.turns);
        let input = json!({ "description": "Look", "prompt": "Look around", "subagent_type": "general-purpose" });
        self.assistant(
            &json!({ "type": "tool_use", "id": tool_use, "name": "Agent", "input": input }),
            None,
            None,
        );
        self.system(
            "task_started",
            &json!({
                "task_id": task, "tool_use_id": tool_use, "description": "Look",
                "subagent_type": "general-purpose", "prompt": "Look around",
                "task_type": "local_agent", "is_backgrounded": false,
            }),
        );
        self.task = Some((task.clone(), false));
        let hold = millis("STUB_AGENT_HOLD_MS", 0);
        let until = Instant::now().checked_add(hold).unwrap_or_else(Instant::now);
        while Instant::now() < until && !self.task.as_ref().is_some_and(|(_, stopped)| *stopped) {
            let left = until.saturating_duration_since(Instant::now()).min(self.gap);
            if let Err(cut) = self.pause(left) {
                self.task = None;
                return Err(cut);
            }
        }
        let stopped = self.task.take().is_some_and(|(_, stopped)| stopped);
        let summary = if stopped {
            self.system(
                "task_notification",
                &json!({ "task_id": task, "tool_use_id": tool_use, "status": "stopped", "summary": "Stopped" }),
            );
            "The agent was stopped."
        } else {
            self.assistant(
                &json!({ "type": "text", "text": "Subagent found it." }),
                Some(&tool_use),
                Some(&task),
            );
            self.system(
                "task_notification",
                &json!({
                    "task_id": task, "tool_use_id": tool_use, "status": "completed",
                    "summary": "Subagent found it.",
                }),
            );
            "Subagent found it."
        };
        self.tool_result(&tool_use, summary, stopped);
        self.pause(self.gap)?;
        let said = "Done: the agent is back.";
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said.to_owned())
    }

    fn slow(&mut self, steps: u32) -> Result<String, Cut> {
        let mut said = String::new();
        let event = |stub: &Self, event: Value| {
            json!({
                "type": "stream_event", "event": event, "session_id": stub.session,
                "parent_tool_use_id": null, "uuid": uuid(),
            })
        };
        let start = json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } });
        say(&event(self, start));
        for _ in 0..steps {
            self.pause(SLOW_STEP)?;
            said.push_str("Slowly ");
            let delta = json!({ "type": "text_delta", "text": "Slowly " });
            say(&event(self, json!({ "type": "content_block_delta", "index": 0, "delta": delta })));
            if self.crash {
                crash();
            }
        }
        say(&event(self, json!({ "type": "content_block_stop", "index": 0 })));
        let said = said.trim_end().to_owned();
        self.assistant(&json!({ "type": "text", "text": said }), None, None);
        Ok(said)
    }

    fn limited(&mut self) -> Ended {
        let until = self.limited_until.unwrap_or_default();
        say(&json!({
            "type": "rate_limit_event",
            "rate_limit_info": { "status": "rejected", "resetsAt": until, "rateLimitType": "five_hour" },
            "uuid": uuid(), "session_id": self.session,
        }));
        let said = limit_text(until);
        let mut message = json!({
            "type": "assistant",
            "message": {
                "id": format!("msg_{}", self.turns), "type": "message", "role": "assistant",
                "model": "<synthetic>", "content": [{ "type": "text", "text": said }],
                "stop_reason": "stop_sequence", "stop_sequence": "",
                "usage": { "input_tokens": 0, "output_tokens": 0 },
            },
            "parent_tool_use_id": null, "session_id": self.session, "uuid": uuid(),
            "timestamp": timestamp(), "error": "rate_limit", "isApiErrorMessage": true,
        });
        say(&message);
        if let Some(object) = message.as_object_mut() {
            object.remove("error");
        }
        self.keep(&message);
        Ended::Limited
    }

    fn compact(&mut self) -> Ended {
        self.system("status", &json!({ "status": "compacting" }));
        if self.pause(self.gap).is_err() {
            return Ended::Interrupted;
        }
        self.system("status", &json!({ "status": null, "compact_result": "success" }));
        self.system(
            "compact_boundary",
            &json!({ "compact_metadata": { "trigger": "manual", "pre_tokens": 1683, "post_tokens": 352 } }),
        );
        self.local("<local-command-stdout>Compacted </local-command-stdout>");
        Ended::Local("compact".to_owned(), String::new())
    }
}

/// The lines of conversation `text` up to the record `at`, that one too; all of them when no
/// record is `at`.
fn through(text: &str, at: &str) -> String {
    let mut kept = String::new();
    for line in text.lines() {
        kept.push_str(line);
        kept.push('\n');
        let id = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|record| record.get("uuid").and_then(Value::as_str).map(str::to_owned));
        if id.as_deref() == Some(at) {
            return kept;
        }
    }
    text.to_owned()
}

/// The recording `STUB_REPLAY` names for `prompt`, if any ([`Stub::replayed`]).
fn replay_of(prompt: &str) -> Option<String> {
    let named = std::env::var("STUB_REPLAY").ok()?;
    match serde_json::from_str::<Value>(&named) {
        Ok(Value::Object(by_prompt)) => by_prompt.get(prompt)?.as_str().map(str::to_owned),
        _ => Some(named),
    }
}

/// The person's prompt: a user record whose content is words, not tool results nor Claude
/// Code's own notes.
fn is_prompt(record: &Value) -> bool {
    record.get("type").and_then(Value::as_str) == Some("user")
        && record.pointer("/message/content").is_some_and(Value::is_string)
        && !record.get("isMeta").and_then(Value::as_bool).unwrap_or(false)
}

/// Each `Agent` call `record` makes: its id and the prompt it gives the subagent.
fn agent_calls(record: &Value) -> Vec<(String, String)> {
    let parts = record.pointer("/message/content").and_then(Value::as_array);
    parts
        .into_iter()
        .flatten()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter(|p| matches!(p.get("name").and_then(Value::as_str), Some("Agent" | "Task")))
        .filter_map(|p| {
            let id = p.get("id")?.as_str()?.to_owned();
            Some((id, p.pointer("/input/prompt")?.as_str()?.to_owned()))
        })
        .collect()
}

/// The words of an assistant record, if it has any.
fn text_of(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let parts = record.pointer("/message/content")?.as_array()?;
    let words: Vec<&str> = parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("text")?.as_str())
        .collect();
    (!words.is_empty()).then(|| words.join("\n"))
}

/// A recorded message as Claude Code streams it, under `parent` for a subagent's; `None` for a
/// record it keeps but does not stream.
fn streamed(record: &Value, session: &str, parent: Option<&str>) -> Option<Value> {
    let kind = record.get("type").and_then(Value::as_str)?;
    if !matches!(kind, "user" | "assistant")
        || record.get("isMeta").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let mut message = record.clone();
    merge(&mut message, &json!({ "session_id": session, "parent_tool_use_id": parent }));
    Some(message)
}

/// Append `line` to the file at `at`, made with its folder when it is not there.
fn append(at: &std::path::Path, line: &str) {
    if let Some(dir) = at.parent() {
        drop(std::fs::create_dir_all(dir));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(at) {
        drop(writeln!(file, "{line}"));
    }
}

/// The `--mcp-config` documents in `args`, then the project's own (`.mcp.json` in `cwd`).
fn mcp_configs(args: &[String], cwd: &str) -> Vec<Value> {
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
    let project = std::fs::read_to_string(std::path::Path::new(cwd).join(".mcp.json"));
    if let Some(doc) = project.ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        docs.push(doc);
    }
    docs
}

/// Call `call` through the `slopty` server of `configs`, in the 2026-07-28 lifecycle (no
/// `initialize`; the version and capabilities in the request's `_meta`); its result.
fn call_tool(configs: &[Value], call: &Value) -> Result<Value, String> {
    let server = configs
        .iter()
        .find_map(|doc| doc.pointer("/mcpServers/slopty").filter(|s| s.is_object()))
        .ok_or("no --mcp-config names a slopty server")?;
    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or("the slopty server names no command")?;
    let args = server.get("args").and_then(Value::as_array).into_iter().flatten();
    let mut child = Command::new(command)
        .args(args.filter_map(Value::as_str))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("the slopty server: {e}"))?;
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": call.get("name"), "arguments": call.get("arguments"),
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": { "name": "stub-claude", "version": VERSION },
            },
        },
    });
    let mut stdin = child.stdin.take().ok_or("the server's stdin")?;
    writeln!(stdin, "{request}").map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("the server's stdout")?;
    let (answer, answered) = mpsc::channel();
    drop(std::thread::spawn(move || {
        let found = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .find(|msg| msg.get("id").and_then(Value::as_u64) == Some(1));
        drop(answer.send(found));
    }));
    let reply = answered.recv_timeout(MCP_PATIENCE);
    drop(stdin);
    drop(child.kill());
    drop(child.wait());
    let reply = reply.map_err(|late| format!("the server did not answer in time: {late}"))?;
    let reply = reply.ok_or("the server closed without answering")?;
    Ok(reply.get("result").cloned().unwrap_or_default())
}

/// The agent an `agent_start` result names.
fn answered_agent(result: &Value) -> Option<String> {
    let text = result.pointer("/content/0/text")?.as_str()?;
    let answer: Value = serde_json::from_str(text).ok()?;
    answer.get("agent")?.as_str().map(str::to_owned)
}

/// Write one line.
fn say(value: &Value) {
    let mut out = std::io::stdout().lock();
    drop(writeln!(out, "{value}"));
    drop(out.flush());
}

/// Answer the control request `id` with success.
fn reply(id: &str, response: Option<Value>) {
    let mut answer = json!({ "subtype": "success", "request_id": id });
    if let Some(response) = response {
        merge(&mut answer, &json!({ "response": response }));
    }
    say(&json!({ "type": "control_response", "response": answer }));
}

/// Answer the control request `id` with an error.
fn refuse(id: &str, error: &str) {
    say(&json!({
        "type": "control_response",
        "response": { "subtype": "error", "request_id": id, "error": error },
    }));
}

/// What an answer to `can_use_tool` decided, and the deny's message.
fn verdict(answer: &Value) -> (bool, String) {
    let behavior = answer.pointer("/response/behavior").and_then(Value::as_str);
    let message = answer
        .pointer("/response/message")
        .or_else(|| answer.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("The person said no.")
        .to_owned();
    (
        answer.get("subtype").and_then(Value::as_str) == Some("success")
            && behavior == Some("allow"),
        message,
    )
}

/// The text of a user message.
fn prompt(message: &Value) -> String {
    match message.pointer("/message/content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The commands it offers, as `initialize` lists them.
fn commands() -> Vec<Value> {
    [
        ("compact", "Clear conversation history but keep a summary in context", "<optional instructions>"),
        ("review", "Review a pull request", ""),
        ("code-review", "Review the current diff", "[low|medium|high]"),
        ("deep-research", "Research a question across sources (skill)", "<question>"),
    ]
    .iter()
    .map(|(name, description, hint)| {
        json!({ "name": name, "description": description, "argumentHint": hint, "builtin": *name != "deep-research" })
    })
    .collect()
}

fn resolved(model: &str) -> String {
    match model {
        "default" | "opus" => "claude-opus-5-5".to_owned(),
        "sonnet" => "claude-sonnet-5-5".to_owned(),
        "haiku" => "claude-haiku-5-5".to_owned(),
        other => other.to_owned(),
    }
}

fn display(model: &str) -> &str {
    match model {
        "default" => "Default (recommended)",
        "opus" => "Opus",
        "sonnet" => "Sonnet",
        "haiku" => "Haiku",
        other => other,
    }
}

fn limit_text(until: u64) -> String {
    format!("You've hit your limit · resets at {until} (epoch seconds)")
}

/// Copy `fields`' entries into `into`, both objects.
fn merge(into: &mut Value, fields: &Value) {
    if let (Some(into), Some(fields)) = (into.as_object_mut(), fields.as_object()) {
        for (k, v) in fields {
            into.insert(k.clone(), v.clone());
        }
    }
}

/// The project directory's name Claude Code keeps a directory's conversations under.
fn escaped(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn epoch_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Now, as ISO 8601 in UTC with milliseconds.
fn timestamp() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil(i64::try_from(days).unwrap_or_default());
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        now.subsec_millis()
    )
}

/// The civil date `days` after 1970-01-01 (Howard Hinnant's algorithm).
#[expect(clippy::arithmetic_side_effects, reason = "dates far inside i64's range")]
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A random version-4 UUID.
fn uuid() -> String {
    let mut bytes = [0_u8; 16];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes));
    if random.is_err() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        bytes = nanos.to_le_bytes();
    }
    let hex = bytes.iter().fold(String::new(), |mut hex, b| {
        hex.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        hex.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
        hex
    });
    let part = |from: usize, to: usize| hex.get(from..to).unwrap_or_default().to_owned();
    format!("{}-{}-4{}-8{}-{}", part(0, 8), part(8, 12), part(13, 16), part(17, 20), part(20, 32))
}

/// Stdin ended mid-turn: nothing more will be asked.
#[expect(clippy::exit, reason = "the stand-in ends as Claude Code does when its input ends")]
fn end() -> ! {
    std::process::exit(0)
}

/// `STUB_CRASH_MID_TURN`: die as a crashing Claude Code does.
#[expect(clippy::exit, reason = "the stand-in crashing on purpose")]
fn crash() -> ! {
    let mut err = std::io::stderr().lock();
    drop(writeln!(err, "claude: crashed mid-turn (STUB_CRASH_MID_TURN)"));
    std::process::exit(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_directory_is_named_as_claude_code_names_it() {
        assert_eq!(escaped("/private/tmp/sj/work"), "-private-tmp-sj-work");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_742), (2026, 10, 16));
        let id = uuid();
        assert_eq!(id.len(), 36, "{id}");
        assert!(wanted(&["-p".to_owned(), "--input-format".to_owned(), "stream-json".to_owned()]));
        assert!(wanted(&["--input-format=stream-json".to_owned()]));
        assert!(!wanted(&["--input-format".to_owned(), "text".to_owned()]));
    }
}
