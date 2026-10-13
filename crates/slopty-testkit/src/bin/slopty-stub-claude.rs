//! A stand-in for `claude`, for tests that start agents without spending anyone's plan
//! (`docs/decisions/projects.md`, "Tests use a stub agent").
//!
//! It does what Slopty needs of Claude Code and nothing more:
//!
//! - `--version` answers as Claude Code does, so a worker lists it as installed, and
//!   `--managed-help` is refused as Claude Code refuses it: it is no managed launcher.
//! - It fires the hooks its `--settings` registers, as Claude Code runs them: every command
//!   registered for the payload's event, with the payload on stdin (naming the session it was
//!   started on with `--session-id`, unless the payload names one) and the session's environment,
//!   keeping what each printed. Which hooks, in order, is `STUB_HOOKS` (a JSON array of payloads);
//!   a `SessionStart` alone when it is unset (its source `resume` when started with `--resume`,
//!   whose session the hooks then name). With `STUB_TRANSCRIPT`, that file is made at once and
//!   every payload names it as its `transcript_path`, as Claude Code's do: what a worker resumes
//!   the conversation by after a reboot. `STUB_LATER` is more of them, fired once its prompt shows
//!   and the file `STUB_LATER_AFTER` exists, as a later turn's.
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
//! - With `STUB_TURNS` (`{"<prompt>": [{"name": …, "arguments": …}, …], …}`), the prompt it was
//!   started with (after `--`) takes a turn as Claude Code's does once the start's hooks fired:
//!   `UserPromptSubmit`, the prompt into the transcript, the tool calls the object lists for that
//!   prompt (none when it lists none) through the `slopty` MCP server as above, its reply (`Done:
//!   <prompt>`) into the transcript, then `Stop`. In a call's arguments, `{started}` is the agent
//!   the turn's last `agent_start` answered. A message the inbox takes gets the same reply.
//! - With `STUB_TITLE`, it paints that terminal title first (OSC 2), as Claude Code paints `✳
//!   Claude Code` at its prompt: what the worker reads as the agent's title.
//! - It shows a prompt and takes what is typed at it, a line at a time.
//!
//! What it was given and what it saw goes to `STUB_RECORD` as one JSON document, replaced whole
//! after each step: its pid, its arguments, its `SLOPTY_*` and `CLAUDE_*` environment, every
//! variable's name with a digest of its value (to tell what it inherited without writing any
//! value), the MCP configs, the hooks it fired, the tool's answer and the lines typed.
//!
//! In `STUB_RECORD`, `STUB_MCP_CALLS`, `STUB_MCP_AFTER`, `STUB_INBOX` and `STUB_TRANSCRIPT`,
//! `{session}` is the Slopty terminal it runs in (`SLOPTY_SESSION`, `none` outside one), and
//! `{conversation}` the conversation it was started on (`--session-id` or `--resume`, `none`
//! otherwise), so one script serves every agent a machine runs, each with its own files and its
//! own record; a transcript under `~/.claude/projects/` named for its conversation is where
//! Claude Code keeps it.

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
    if args.first().is_some_and(|a| a == "--managed-help") {
        eprintln!("error: unknown option '--managed-help'");
        return ExitCode::FAILURE;
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
    /// The directory it was started in.
    cwd: Option<String>,
    /// The directory the session is in now: where it was started, until a payload names
    /// another, as Claude Code moves into a worktree it enters.
    here: Option<String>,
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
            "pid": std::process::id(),
            "argv": self.argv,
            "cwd": self.cwd,
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

/// Where a knob names the terminal the agent runs in.
const SESSION_PLACE: &str = concat!("{", "session", "}");

/// Where a tool call's arguments name the agent the turn last started.
const STARTED_PLACE: &str = concat!("{", "started", "}");

/// Where a knob names the conversation the agent was started on.
const CONVERSATION_PLACE: &str = concat!("{", "conversation", "}");

/// The variable `name`, with [`SESSION_PLACE`] the terminal the agent runs in and
/// [`CONVERSATION_PLACE`] the conversation it was started on.
fn knob(name: &str) -> Option<String> {
    let session = std::env::var("SLOPTY_SESSION").unwrap_or_else(|_| "none".to_owned());
    let args: Vec<String> = std::env::args().collect();
    let at = args.iter().position(|a| a == "--session-id" || a == "--resume");
    let conversation =
        at.and_then(|at| args.get(at.saturating_add(1))).map_or("none", String::as_str);
    std::env::var(name).ok().map(|value| {
        value.replace(SESSION_PLACE, &session).replace(CONVERSATION_PLACE, conversation)
    })
}

fn run(args: &[String]) -> Fallible<()> {
    let mut record = Record {
        at: knob("STUB_RECORD").map(PathBuf::from),
        argv: args.to_vec(),
        cwd: std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned()),
        here: None,
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
    // Claude Code writes its transcript from its first moment: one resumed finds it there.
    if let Some(transcript) = knob("STUB_TRANSCRIPT") {
        drop(std::fs::OpenOptions::new().create(true).append(true).open(transcript)?);
    }
    if let Some(file) = knob("STUB_INBOX") {
        record.hook_env = inbox(PathBuf::from(file), settings.clone())?;
    }

    let script = if let Ok(text) = std::env::var("STUB_HOOKS") {
        serde_json::from_str(&text)?
    } else {
        let source = if args.iter().any(|a| a == "--resume") { "resume" } else { "startup" };
        json!([{ "hook_event_name": "SessionStart", "source": source }])
    };
    fire_all(&settings, &script, &mut record)?;
    record.save()?;

    if let Some(calls) = knob("STUB_MCP_CALLS") {
        let calls: Vec<Value> = serde_json::from_str(&calls)?;
        if let Some(gate) = knob("STUB_MCP_AFTER") {
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
    if let Ok(turns) = std::env::var("STUB_TURNS") {
        let turns: Value = serde_json::from_str(&turns)?;
        if let Some(prompt) = first_prompt(args) {
            let calls = turns.get(&prompt).and_then(Value::as_array).cloned().unwrap_or_default();
            take_turn(&settings, &prompt, &calls, &mut record)?;
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

/// The prompt Claude Code was started with: the words after `--`.
fn first_prompt(args: &[String]) -> Option<String> {
    let at = args.iter().position(|a| a == "--")?;
    let words = args.get(at.saturating_add(1)..)?;
    Some(words.join(" ")).filter(|p| !p.trim().is_empty())
}

/// What the stub answers a prompt with: `Done:` and the prompt's last line of words, so a
/// message wrapped in tags is answered by what it says.
fn reply_to(prompt: &str) -> String {
    let words = prompt.lines().rev().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('<'));
    format!("Done: {}", words.unwrap_or_else(|| prompt.trim()))
}

/// Take a turn with `prompt` as Claude Code does ([`STUB_TURNS`](self)): its hooks, its
/// transcript, and `calls` in order.
fn take_turn(settings: &Value, prompt: &str, calls: &[Value], record: &mut Record) -> Fallible<()> {
    let submitted = json!([{ "hook_event_name": "UserPromptSubmit", "prompt": prompt }]);
    fire_all(settings, &submitted, record)?;
    let transcript = knob("STUB_TRANSCRIPT").map(PathBuf::from);
    if let Some(transcript) = &transcript {
        said(transcript, "user", &json!(prompt))?;
    }
    let mut started: Option<String> = None;
    for call in calls {
        let call = match &started {
            Some(agent) => serde_json::from_str(&call.to_string().replace(STARTED_PLACE, agent))?,
            None => call.clone(),
        };
        let answer = match call_tool(&record.mcp_config, &call) {
            Ok(answer) => answer,
            Err(e) => json!({ "error": e.to_string() }),
        };
        if call.get("name").and_then(Value::as_str) == Some("agent_start") {
            started = answered_agent(&answer).or(started);
        }
        record.mcp.push(answer);
        record.save()?;
    }
    let reply = reply_to(prompt);
    if let Some(transcript) = &transcript {
        said(transcript, "assistant", &json!([{ "type": "text", "text": reply }]))?;
    }
    let stopped = json!([{
        "hook_event_name": "Stop",
        "stop_hook_active": false,
        "last_assistant_message": reply,
    }]);
    fire_all(settings, &stopped, record)
}

/// The agent an `agent_start`'s result names.
fn answered_agent(result: &Value) -> Option<String> {
    let text = result.pointer("/content/0/text")?.as_str()?;
    let answer: Value = serde_json::from_str(text).ok()?;
    answer.get("agent")?.as_str().map(str::to_owned)
}

/// Append one message to the transcript at `at` as Claude Code writes one: `role`'s, with
/// `content`, chained to the line before it and stamped with the time. An answer ends its turn,
/// as Claude Code's last message of a turn says and the `turn_duration` record after it marks.
fn said(at: &Path, role: &str, content: &Value) -> Fallible<()> {
    let before = std::fs::read_to_string(at).unwrap_or_default();
    let parent = before
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok()?.get("uuid").cloned());
    let n = before.lines().count();
    let uuid = format!("stub-{}-{n}", std::process::id());
    let timestamp = stamp(std::time::SystemTime::now());
    let text = if role == "assistant" {
        let message = json!({ "role": role, "content": content, "stop_reason": "end_turn" });
        let line = json!({
            "type": role, "uuid": uuid, "parentUuid": parent, "timestamp": timestamp,
            "message": message,
        });
        let ended = format!("stub-{}-{}", std::process::id(), n.saturating_add(1));
        let end = json!({
            "type": "system", "subtype": "turn_duration", "uuid": ended, "parentUuid": uuid,
            "timestamp": timestamp, "durationMs": 1,
        });
        format!("{line}\n{end}\n")
    } else {
        let message = json!({ "role": role, "content": content });
        let line = json!({
            "type": role, "uuid": uuid, "parentUuid": parent, "timestamp": timestamp,
            "message": message,
        });
        format!("{line}\n")
    };
    let mut out = std::fs::OpenOptions::new().create(true).append(true).open(at)?;
    // One write, so a reader never sees half of a turn.
    out.write_all(text.as_bytes())?;
    Ok(())
}

/// `at` as Claude Code stamps a record, in UTC to the millisecond (`2026-10-13T05:46:04.748Z`);
/// `None` before 1970.
fn stamp(at: std::time::SystemTime) -> Option<String> {
    let ms = u64::try_from(at.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()?;
    let (days, of_day) = (ms / 86_400_000, ms % 86_400_000);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's `civil_from_days`).
    let z = days.checked_add(719_468)?;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe =
        doe.checked_sub(doe / 1460)?.checked_add(doe / 36_524)?.checked_sub(doe / 146_096)? / 365;
    let doy =
        doe.checked_sub(yoe.checked_mul(365)?.checked_add(yoe / 4)?.checked_sub(yoe / 100)?)?;
    let mp = doy.checked_mul(5)?.checked_add(2)? / 153;
    let day = doy.checked_sub(mp.checked_mul(153)?.checked_add(2)? / 5)?.checked_add(1)?;
    let month = if mp < 10 { mp.checked_add(3)? } else { mp.checked_sub(9)? };
    let year = era.checked_mul(400)?.checked_add(yoe)?.checked_add(u64::from(month <= 2))?;
    let (hour, minute) = (of_day / 3_600_000, of_day / 60_000 % 60);
    let (second, milli) = (of_day / 1000 % 60, of_day % 1000);
    Some(format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z"))
}

/// What an inbox does with the messages posted to it.
struct Inbox {
    /// Where each message is noted.
    file: PathBuf,
    /// The token a connection shows to prove it is the session's own.
    token: String,
    /// Hold every message rather than take a turn with it.
    hold: bool,
    /// Answer a message taken, as a turn under `STUB_TURNS` does.
    replies: bool,
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
        replies: std::env::var_os("STUB_TURNS").is_some(),
        transcript: knob("STUB_TRANSCRIPT").map(PathBuf::from),
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
        if inbox.replies {
            said(transcript, "user", &json!(content))?;
        } else {
            let entry = json!({
                "type": "user",
                "message": { "role": "user", "content": content },
                "origin": { "kind": "peer" },
            });
            let mut out = std::fs::OpenOptions::new().create(true).append(true).open(transcript)?;
            out.write_all(format!("{entry}\n").as_bytes())?;
        }
        if inbox.replies {
            let reply = reply_to(content);
            said(transcript, "assistant", &json!([{ "type": "text", "text": reply }]))?;
            payload.insert("last_assistant_message".to_owned(), Value::String(reply));
        }
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
/// its event ran cleanly (none registered is not fired), and what they printed. A payload that
/// names no session carries the one the stub was started on (`--session-id`), and one that names
/// no directory the session's (the last one named, else the stub's own), as each of Claude
/// Code's does.
fn fire_all(settings: &Value, script: &Value, record: &mut Record) -> Fallible<()> {
    let at = record.argv.iter().position(|a| a == "--session-id" || a == "--resume");
    let session = at.and_then(|at| record.argv.get(at.saturating_add(1))).cloned();
    let transcript = knob("STUB_TRANSCRIPT");
    for payload in script.as_array().into_iter().flatten() {
        let mut payload = payload.clone();
        if let (Some(session), Some(fields)) = (&session, payload.as_object_mut()) {
            fields.entry("session_id").or_insert_with(|| json!(session));
        }
        if let (Some(transcript), Some(fields)) = (&transcript, payload.as_object_mut()) {
            fields.entry("transcript_path").or_insert_with(|| json!(transcript));
        }
        in_session_dir(&mut payload, record.cwd.as_ref(), &mut record.here);
        let payload = &payload;
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

/// Give `payload` the session's directory unless it names one: each of Claude Code's names
/// where the session is now, the last directory a payload named (`here`, kept), else where it
/// was `started`.
fn in_session_dir(payload: &mut Value, started: Option<&String>, here: &mut Option<String>) {
    if let Some(named) = payload.get("cwd").and_then(Value::as_str) {
        *here = Some(named.to_owned());
    }
    let now = here.as_ref().or(started);
    if let (Some(cwd), Some(fields)) = (now, payload.as_object_mut()) {
        fields.entry("cwd").or_insert_with(|| json!(cwd));
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::in_session_dir;

    /// A payload that names no directory carries the last one a payload named, as Claude Code's
    /// do once the session entered a worktree, and where the stub started before any did.
    #[test]
    fn a_payload_carries_the_directory_the_session_is_in() {
        let started = "/repo".to_owned();
        let mut here = None;
        let mut first = json!({ "hook_event_name": "SessionStart" });
        in_session_dir(&mut first, Some(&started), &mut here);
        assert_eq!(first["cwd"], "/repo");
        let mut entered = json!({ "hook_event_name": "UserPromptSubmit", "cwd": "/repo/tree" });
        in_session_dir(&mut entered, Some(&started), &mut here);
        assert_eq!(entered["cwd"], "/repo/tree");
        let mut status = json!({ "hook_event_name": "Statusline" });
        in_session_dir(&mut status, Some(&started), &mut here);
        assert_eq!(status["cwd"], "/repo/tree", "the worktree, not where it started");
    }
}
