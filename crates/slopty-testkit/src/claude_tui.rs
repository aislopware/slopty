//! The stand-in `claude`: Claude Code's TUI as Slopty meets it from outside, in a terminal.
//!
//! A test starts and follows a session with it without spending anyone's plan
//! (`docs/decisions/agents.md`, "TUI-first"). It keeps the contract the worker reads and nothing of
//! Claude Code's screen:
//!
//! - **Its record of itself**, `<config>/sessions/<pid>.json` (`pid`, `sessionId`, `cwd`, `kind`
//!   `interactive`, `version`), where `<config>` is `CLAUDE_CONFIG_DIR` or `~/.claude`, written as
//!   it starts and again for each session it moves to, and removed as it ends. As 2.1.312 does, it
//!   says there what it does (`status`: `idle`, `busy` through a turn, `waiting` while a question
//!   waits on the person, `waitingFor` `input needed`) and since when (`statusUpdatedAt`,
//!   `updatedAt`, milliseconds since the epoch), written again as that changes.
//! - **The session's transcript**, `<config>/projects/<cwd with every character but letters and
//!   digits made '-'>/<session>.jsonl`: each turn's prompt, its answer, its tool calls and their
//!   results, and its end (`system`/`turn_duration`), as Claude Code writes them. A session is the
//!   one `--session-id` names, or the one `--resume` takes up (its file goes on), else a new one.
//! - **Its hooks**, the `command` entries its `--settings` registers, each run with the event's
//!   JSON on its standard input (`hook_event_name`, `session_id`, `transcript_path`, `cwd`, and the
//!   event's own fields), to its end before the next, as a model's own pace keeps them apart.
//! - **Its terminal**: raw, with bracketed paste turned on, so a paste arrives whole between its
//!   markers and the Enter that sends it is a key of its own.
//! - **Its prompt box, through Slopty's mod**: given `--plugin-dir=<dir>`, it runs the relay
//!   `<dir>/hooks/register.ts` names (`const RELAY = [<program>, "hook"]`, as the worker writes it)
//!   after each read that changed the line being typed, with the `PromptDraft` payload the mod
//!   hands it (`session_id`, `prompt`, and `tmpdir` when `CLAUDE_CODE_TMPDIR` is set). A paste that
//!   is the path of a PNG, JPEG, GIF or WebP file is a picture, as Claude Code takes one: its bytes
//!   stored at `<CLAUDE_CODE_TMPDIR or /tmp>/claude-<uid>/<cwd's key>/<session>/images/<N>.<ext>`
//!   and `[Image #N]` put in the line, numbered from 1 in each session.
//!
//! What it is typed is a line, sent by Enter. The turn is chosen by the first word of the line's
//! last line (an agent's words come framed, the words last):
//!
//! - `ask <question>`: an `AskUserQuestion` call, its `PreToolUse` hook run, waiting on the person:
//!   the next line typed answers it, and the turn ends.
//! - `hold <anything>`: a turn that goes on until Esc interrupts it, as Claude Code records an
//!   interrupt, and no hook runs.
//! - a line `STUB_TURNS` names (`{"<line>": [{"name": …, "arguments": …}, …], …}`): those tools
//!   called in order through the `slopty` MCP server its `--mcp-config` names, over stdio, each an
//!   `mcp__slopty__<name>` call with its hooks and its result. In a call's arguments `{started}` is
//!   the agent the turn's last `agent_start` answered; a call marked `"until_ok": true` is made
//!   again every 100 ms for up to 30 s while its result is an error.
//! - `/clear`: the session ends (`SessionEnd`, `clear`) and a new one begins (`SessionStart`,
//!   `clear`), its record saying so. `/exit` ends it, as does ^D on an empty line or its terminal
//!   closing.
//! - anything else: answered at once, `Done: <line>`.
//!
//! Each line typed while a held turn runs waits for it to end, queued as Claude Code queues it: a
//! `queue-operation` record in the transcript, `enqueue` with the line as it is typed and
//! `dequeue` as it goes to the agent.
//!
//! What it did goes to `STUB_RECORD` as one JSON document, replaced whole as it changes: its pid,
//! when it started (`started_us`, microseconds since the epoch), its arguments and directory, the
//! Slopty seat it runs in (`SLOPTY_SESSION`), every variable's name with a digest of its value
//! (`crate::env::digests`), each read of its terminal with when it was made (`reads`), each line
//! it took (`prompts`), each hook it ran (`hooks`), each draft it relayed (`drafts`), the sessions
//! it held (`sessions`), when each
//! transcript record was written (`wrote`) and its tools' results (`mcp`).
//! In `STUB_RECORD`, `{session}` is the seat (`none` outside one) and `{pid}` its pid.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// What it says its version is.
pub const VERSION: &str = "2.1.312";

/// The longest a hook is waited for when its entry names no timeout, in seconds.
const HOOK_TIMEOUT_S: u64 = 60;

/// How long Slopty's MCP server may take to answer a call: an `agent_wait` waits on it.
const MCP_PATIENCE: Duration = Duration::from_secs(90);

/// How long a call marked `until_ok` is made again, and how often.
const UNTIL_OK: (Duration, Duration) = (Duration::from_secs(30), Duration::from_millis(100));

/// Bracketed paste's markers, around what a terminal pastes.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The words Claude Code records an interrupt with.
const INTERRUPTED: &str = "[Request interrupted by user]";

/// What its record says it does: at rest, at work, waiting on the person.
const IDLE: &str = "idle";
const BUSY: &str = "busy";
const WAITING: &str = "waiting";

/// What its record says a question waits for.
const INPUT_NEEDED: &str = "input needed";

/// The line of the mod's module that names its relay, before the relay itself.
const RELAY_LINE: &str = "const RELAY = ";

/// The variable that moves Claude Code's temporary root, where pasted pictures are kept.
const TMPDIR_ENV: &str = "CLAUDE_CODE_TMPDIR";

/// Run the TUI as `args` ask, until its session ends.
#[must_use]
pub fn run(args: &[String]) -> ExitCode {
    Tui::start(args).serve()
}

/// What the person typed, as the TUI takes it.
#[derive(Debug, PartialEq, Eq)]
enum Key {
    /// A line, sent by Enter.
    Line(String),
    /// Esc alone.
    Escape,
    /// ^D on an empty line.
    End,
}

/// The line being typed: what came of the terminal, read as keys and pastes.
#[derive(Debug, Default)]
struct Input {
    line: Vec<u8>,
    pending: Vec<u8>,
    pasting: bool,
    /// The paste under way, taken whole at its end.
    paste: Vec<u8>,
    /// Where the session keeps pasted pictures, and how many it has.
    pictures: Option<(PathBuf, u32)>,
}

impl Input {
    /// Keep pasted pictures in `dir` from now on: numbered from 1 again in another session's.
    fn keep_pictures(&mut self, dir: PathBuf) {
        if self.pictures.as_ref().is_none_or(|(had, _)| *had != dir) {
            self.pictures = Some((dir, 0));
        }
    }

    /// A paste of `bytes` that is the path of a picture, stored as Claude Code stores it: the
    /// placeholder that stands for it in the line.
    fn picture(&mut self, bytes: &[u8]) -> Option<String> {
        let path = std::str::from_utf8(bytes).ok()?.trim();
        let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        let ext = match ext.as_str() {
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "gif" => "gif",
            "webp" => "webp",
            _ => return None,
        };
        let picture = std::fs::read(path).ok()?;
        let (dir, count) = self.pictures.as_mut()?;
        let n = count.saturating_add(1);
        std::fs::create_dir_all(&*dir).ok()?;
        std::fs::write(dir.join(format!("{n}.{ext}")), picture).ok()?;
        *count = n;
        Some(format!("[Image #{n}]"))
    }

    /// Take the first `n` bytes of what is pending.
    fn cut(&mut self, n: usize) -> Vec<u8> {
        let rest = self.pending.split_off(n.min(self.pending.len()));
        std::mem::replace(&mut self.pending, rest)
    }

    /// Add a whole paste to the line: a picture as its placeholder, words as they are with a
    /// return in them a new line.
    fn pasted(&mut self, bytes: &[u8]) {
        if let Some(placeholder) = self.picture(bytes) {
            self.line.extend_from_slice(placeholder.as_bytes());
            return;
        }
        self.line.extend(bytes.iter().map(|b| if *b == b'\r' { b'\n' } else { *b }));
    }

    /// The keys `bytes` complete, with what came before them.
    fn take(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        loop {
            if self.pasting {
                if let Some(at) = find(&self.pending, PASTE_END) {
                    let pasted = self.cut(at);
                    self.paste.extend_from_slice(&pasted);
                    drop(self.cut(PASTE_END.len()));
                    self.pasting = false;
                    let whole = std::mem::take(&mut self.paste);
                    self.pasted(&whole);
                    continue;
                }
                // All but what may begin the end marker, which the next read completes.
                let keep = (1..PASTE_END.len())
                    .rev()
                    .find(|n| self.pending.ends_with(PASTE_END.get(..*n).unwrap_or_default()))
                    .unwrap_or(0);
                let pasted = self.cut(self.pending.len().saturating_sub(keep));
                self.paste.extend_from_slice(&pasted);
                break;
            }
            let Some(&byte) = self.pending.first() else { break };
            match byte {
                0x1b => {
                    if self.pending.starts_with(PASTE_START) {
                        drop(self.cut(PASTE_START.len()));
                        self.pasting = true;
                    } else if self.pending.len() == 1 {
                        self.pending.clear();
                        keys.push(Key::Escape);
                    } else if self.pending.get(1) == Some(&b'[') {
                        // Another sequence (an arrow, a focus report): passed over whole.
                        let end = self
                            .pending
                            .iter()
                            .skip(2)
                            .position(|b| (0x40..=0x7e).contains(b))
                            .map(|at| at.saturating_add(3));
                        let Some(end) = end else { break };
                        drop(self.cut(end));
                    } else {
                        drop(self.cut(1));
                        keys.push(Key::Escape);
                    }
                }
                b'\r' | b'\n' => {
                    drop(self.cut(1));
                    let line = std::mem::take(&mut self.line);
                    keys.push(Key::Line(String::from_utf8_lossy(&line).into_owned()));
                }
                0x04 => {
                    drop(self.cut(1));
                    if self.line.is_empty() {
                        keys.push(Key::End);
                    }
                }
                // ^C clears the line.
                0x03 => {
                    drop(self.cut(1));
                    self.line.clear();
                }
                0x7f | 0x08 => {
                    drop(self.cut(1));
                    self.line.pop();
                }
                other => {
                    drop(self.cut(1));
                    self.line.push(other);
                }
            }
        }
        keys
    }
}

/// Where `needle` first is in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What the TUI is doing.
#[derive(Debug)]
enum State {
    /// Waiting for a line.
    Idle,
    /// A turn that goes on until Esc.
    Holding,
    /// A question asked of the person: its tool call, and when the turn began.
    Asking { call: String, since: Instant },
}

/// One Claude Code TUI.
struct Tui {
    args: Vec<String>,
    config: PathBuf,
    cwd: String,
    session: String,
    resumed: bool,
    hooks: Value,
    mcp: Vec<Value>,
    turns: Value,
    /// The last record written, which the next one follows.
    last: Option<String>,
    state: State,
    /// Lines typed while a turn ran, each run once it ends.
    waiting: Vec<String>,
    record: Option<PathBuf>,
    doc: Value,
    /// What its record says it does, what for while it waits, and since when (milliseconds
    /// since the epoch).
    doing: (&'static str, Option<&'static str>, u64),
    /// The relay Slopty's mod names, when one was loaded, and the draft it was last handed.
    relay: Option<Vec<String>>,
    drafted: String,
}

impl Tui {
    fn start(args: &[String]) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let config = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map_or_else(|| home.join(".claude"), PathBuf::from);
        let resume = flag(args, "--resume");
        let session = resume.clone().or_else(|| flag(args, "--session-id")).unwrap_or_else(uuid);
        let cwd = here();
        let seat = std::env::var("SLOPTY_SESSION").ok();
        let record = std::env::var("STUB_RECORD").ok().map(|at| {
            let at = at
                .replace(concat!("{", "session", "}"), seat.as_deref().unwrap_or("none"))
                .replace(concat!("{", "pid", "}"), &std::process::id().to_string());
            PathBuf::from(at)
        });
        let doc = json!({
            "pid": std::process::id(),
            "started_us": now_us(),
            "args": args,
            "cwd": cwd,
            "seat": seat,
            "env": crate::env::digests(),
            "sessions": [session],
            "reads": [], "prompts": [], "hooks": [], "drafts": [], "wrote": [], "mcp": [],
        });
        let mut tui = Self {
            args: args.to_vec(),
            config,
            cwd,
            session,
            resumed: resume.is_some(),
            hooks: settings(args).get("hooks").cloned().unwrap_or(Value::Null),
            mcp: mcp_configs(args),
            turns: std::env::var("STUB_TURNS")
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or(Value::Null),
            last: None,
            state: State::Idle,
            waiting: Vec::new(),
            record,
            doc,
            doing: (IDLE, None, now_us() / 1000),
            relay: flag(args, "--plugin-dir").and_then(|dir| relay_of(Path::new(&dir))),
            drafted: String::new(),
        };
        if tui.resumed {
            tui.last = std::fs::read_to_string(tui.transcript()).ok().and_then(|text| {
                text.lines()
                    .rev()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .find_map(|r| r.get("uuid").and_then(Value::as_str).map(str::to_owned))
            });
        }
        tui
    }

    /// Run until the session ends.
    fn serve(mut self) -> ExitCode {
        let terminal = Raw::enter();
        screen("\x1b[?2004h");
        self.register();
        let source = if self.resumed { "resume" } else { "startup" };
        self.hook("SessionStart", &json!({ "source": source, "model": "claude-stub" }));
        self.save();
        let first = self
            .args
            .iter()
            .position(|a| a == "--")
            .and_then(|at| self.args.get(at.saturating_add(1)..).map(|rest| rest.join(" ")));
        match first.filter(|p| !p.trim().is_empty()) {
            // A first prompt's turn draws the input line after its answer.
            Some(first) => {
                if self.line(first) {
                    return self.end("prompt_input_exit", terminal);
                }
            }
            None => screen("> "),
        }
        let mut ended = "other";
        let mut input = Input::default();
        let mut buf = [0_u8; 4096];
        'reading: loop {
            let read = std::io::stdin().lock().read(&mut buf);
            let read = match read {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let bytes = buf.get(..read).unwrap_or_default();
            self.note(
                "reads",
                json!({ "at_us": now_us(), "bytes": String::from_utf8_lossy(bytes) }),
            );
            input.keep_pictures(self.pictures());
            for key in input.take(bytes) {
                let done = match key {
                    Key::Line(line) => self.line(line),
                    Key::Escape => self.interrupt(),
                    Key::End => true,
                };
                if done {
                    ended = "prompt_input_exit";
                    break 'reading;
                }
            }
            self.draft(&input.line);
            self.save();
        }
        self.end(ended, terminal)
    }

    /// Where the session keeps the pictures pasted into its box, as Claude Code keeps them.
    fn pictures(&self) -> PathBuf {
        let root = std::env::var_os(TMPDIR_ENV)
            .filter(|t| !t.is_empty())
            .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let uid = rustix::process::getuid().as_raw();
        root.join(format!("claude-{uid}"))
            .join(escaped(&self.cwd))
            .join(&self.session)
            .join("images")
    }

    /// Hand the mod's relay the box's `line`, when one was loaded and the line changed, as the
    /// mod does once typing pauses.
    fn draft(&mut self, line: &[u8]) {
        let Some((program, args)) = self.relay.as_ref().and_then(|r| r.split_first()) else {
            return;
        };
        let text = String::from_utf8_lossy(line).into_owned();
        if text == self.drafted {
            return;
        }
        let entry = json!({ "type": "command", "command": program, "args": args });
        let mut payload = json!({
            "hook_event_name": "PromptDraft", "session_id": self.session, "prompt": text,
        });
        if let Some(tmpdir) = std::env::var(TMPDIR_ENV).ok().filter(|t| !t.is_empty()) {
            merge(&mut payload, &json!({ "tmpdir": tmpdir }));
        }
        let ran = run_hook(&entry, &payload);
        let noted = json!({ "session": self.session, "text": text, "at_us": now_us(), "ran": ran });
        self.note("drafts", noted);
        self.drafted = text;
    }

    /// The session ends as `reason` says: its hook run, its record of itself removed.
    fn end(mut self, reason: &str, terminal: Option<Raw>) -> ExitCode {
        self.hook("SessionEnd", &json!({ "reason": reason }));
        drop(std::fs::remove_file(self.registry()));
        self.save();
        screen("\x1b[?2004l");
        drop(terminal);
        ExitCode::SUCCESS
    }

    /// Take line `line`; whether the session ends with it.
    fn line(&mut self, line: String) -> bool {
        self.note("prompts", json!({ "session": self.session, "text": line, "at_us": now_us() }));
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Asking { call, since } => {
                self.answered(&call, &line, since);
                return self.after_turn();
            }
            State::Holding => {
                self.state = State::Holding;
                self.queue("enqueue", Some(&line));
                self.waiting.push(line);
                return false;
            }
            State::Idle => {}
        }
        match line.trim() {
            "/exit" | "/quit" => return true,
            "/clear" => {
                self.clear();
                return false;
            }
            _ => {}
        }
        let last_line = line.lines().last().unwrap_or_default().trim().to_owned();
        let word = last_line.split_whitespace().next().unwrap_or_default();
        self.doing(BUSY, None);
        self.hook("UserPromptSubmit", &json!({ "prompt": line }));
        let prompt = self.write("user", json!({ "role": "user", "content": line }));
        let since = Instant::now();
        let calls = self.turns.get(&line).and_then(Value::as_array).cloned();
        if let Some(calls) = calls {
            self.call_tools(&calls);
        } else if word == "ask" {
            let call = format!("toolu_ask_{}", short(&prompt));
            let question = last_line.strip_prefix("ask").unwrap_or_default().trim().to_owned();
            let input = json!({ "questions": [{
                "question": question, "header": "Ask", "multiSelect": false,
                "options": [{ "label": "Red", "description": "" }, { "label": "Blue", "description": "" }],
            }] });
            self.write(
                "assistant",
                assistant(&json!([{
                    "type": "tool_use", "id": call, "name": "AskUserQuestion", "input": input,
                }])),
            );
            self.hook(
                "PreToolUse",
                &json!({
                    "tool_name": "AskUserQuestion", "tool_input": input, "tool_use_id": call,
                }),
            );
            screen(&format!("\r\n? {question}\r\n"));
            self.state = State::Asking { call, since };
            self.doing(WAITING, Some(INPUT_NEEDED));
            return false;
        } else if word == "hold" {
            screen("\r\n… holding until Esc\r\n");
            self.state = State::Holding;
            return false;
        }
        self.answer(&format!("Done: {line}"), since);
        self.after_turn()
    }

    /// The turn's answer, its `Stop` hook and its end.
    fn answer(&mut self, text: &str, since: Instant) {
        self.write("assistant", assistant(&json!([{ "type": "text", "text": text }])));
        self.hook("Stop", &json!({ "stop_hook_active": false, "last_assistant_message": text }));
        self.turn_end(since);
        self.doing(IDLE, None);
        screen(&format!("\r\n● {text}\r\n> "));
    }

    fn turn_end(&mut self, since: Instant) {
        let ms = u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut record = self.base("system");
        merge(
            &mut record,
            &json!({ "subtype": "turn_duration", "durationMs": ms, "isMeta": false }),
        );
        self.put(&record);
    }

    /// A question asked by call `call` answered `answer`.
    fn answered(&mut self, call: &str, answer: &str, since: Instant) {
        self.doing(BUSY, None);
        let result = format!("User has answered your questions: \"{answer}\"");
        self.write(
            "user",
            json!({ "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call, "content": result,
        }] }),
        );
        self.hook("PostToolUse", &json!({ "tool_name": "AskUserQuestion", "tool_use_id": call }));
        self.answer(&format!("Done: {answer}"), since);
    }

    /// Run what waited behind a turn; whether the session ends with it.
    fn after_turn(&mut self) -> bool {
        while matches!(self.state, State::Idle) && !self.waiting.is_empty() {
            let next = self.waiting.remove(0);
            self.queue("dequeue", None);
            if self.line(next) {
                return true;
            }
        }
        false
    }

    /// Esc: the turn under way stops, as Claude Code records an interrupt, and no hook runs;
    /// whether the session ends with what waited behind it.
    fn interrupt(&mut self) -> bool {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => return false,
            State::Asking { call, .. } => {
                self.write(
                    "user",
                    json!({ "role": "user", "content": [{
                    "type": "tool_result", "tool_use_id": call, "is_error": true,
                    "content": "The user doesn't want to proceed with this tool use.",
                }] }),
                );
            }
            State::Holding => {}
        }
        let text = json!([{ "type": "text", "text": INTERRUPTED }]);
        self.write("user", json!({ "role": "user", "content": text }));
        self.doing(IDLE, None);
        screen("\r\n⎿ Interrupted\r\n> ");
        self.after_turn()
    }

    /// `/clear`: this session ends, and a new one begins in the same process.
    fn clear(&mut self) {
        self.hook("SessionEnd", &json!({ "reason": "clear" }));
        self.session = uuid();
        self.last = None;
        if let Some(sessions) = self.doc.get_mut("sessions").and_then(Value::as_array_mut) {
            sessions.push(Value::String(self.session.clone()));
        }
        self.register();
        self.hook("SessionStart", &json!({ "source": "clear", "model": "claude-stub" }));
        screen("\r\n(no content)\r\n> ");
    }

    /// Call each of `calls` through the `slopty` server, in order.
    fn call_tools(&mut self, calls: &[Value]) {
        let mut started: Option<String> = None;
        for (n, call) in calls.iter().enumerate() {
            let name = call.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            let mut call = call.clone();
            if let (Some(agent), Some(arguments)) = (&started, call.get_mut("arguments")) {
                let text = arguments.to_string().replace(concat!("{", "started", "}"), agent);
                *arguments = serde_json::from_str(&text).unwrap_or(Value::Null);
            }
            let id = format!("toolu_mcp_{n}_{}", short(&uuid()));
            let tool = format!("mcp__slopty__{name}");
            let input = call.get("arguments").cloned().unwrap_or(Value::Null);
            self.write(
                "assistant",
                assistant(&json!([{
                    "type": "tool_use", "id": id, "name": tool, "input": input,
                }])),
            );
            let pre = json!({ "tool_name": tool, "tool_input": input, "tool_use_id": id });
            self.hook("PreToolUse", &pre);
            let until_ok = call.get("until_ok").and_then(Value::as_bool).unwrap_or(false);
            let answer = call_until(&self.mcp, &call, until_ok);
            if name == "agent_start" {
                started = answered_agent(&answer).or(started);
            }
            let text =
                answer.pointer("/content/0/text").and_then(Value::as_str).unwrap_or_default();
            self.write(
                "user",
                json!({ "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": id, "content": text,
            }] }),
            );
            self.hook("PostToolUse", &json!({ "tool_name": tool, "tool_use_id": id }));
            self.note("mcp", answer);
            self.save();
        }
    }

    /// Run event `event`'s hooks, `fields` beside the fields every hook gets.
    fn hook(&mut self, event: &str, fields: &Value) {
        let mut payload = json!({
            "hook_event_name": event,
            "session_id": self.session,
            "transcript_path": self.transcript().to_string_lossy(),
            "cwd": self.cwd,
            "permission_mode": "default",
        });
        merge(&mut payload, fields);
        let entries: Vec<Value> = self
            .hooks
            .get(event)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|group| group.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("command"))
            .cloned()
            .collect();
        let ran = entries.iter().filter(|entry| run_hook(entry, &payload)).count();
        let noted =
            json!({ "event": event, "session": self.session, "at_us": now_us(), "ran": ran });
        self.note("hooks", noted);
        self.save();
    }

    /// Write a record of `kind` with `message`: its id.
    fn write(&mut self, kind: &str, message: Value) -> String {
        let mut record = self.base(kind);
        if let Some(fields) = record.as_object_mut() {
            fields.insert("message".to_owned(), message);
        }
        self.put(&record)
    }

    /// Record a queue operation on the prompts typed while a turn runs, as Claude Code records
    /// one: no link in the chain of messages, the prompt itself on an `enqueue`.
    fn queue(&mut self, operation: &str, content: Option<&str>) {
        let mut record = json!({
            "type": "queue-operation", "operation": operation, "timestamp": timestamp(),
            "sessionId": self.session,
        });
        if let (Some(fields), Some(content)) = (record.as_object_mut(), content) {
            fields.insert("content".to_owned(), Value::from(content));
        }
        append(&self.transcript(), &record);
        let noted =
            json!({ "at_us": now_us(), "type": "queue-operation", "session": self.session });
        self.note("wrote", noted);
    }

    /// The fields every record of `kind` has.
    fn base(&self, kind: &str) -> Value {
        json!({
            "parentUuid": self.last, "isSidechain": false, "userType": "external",
            "cwd": self.cwd, "sessionId": self.session, "version": VERSION, "gitBranch": "",
            "type": kind, "uuid": uuid(), "timestamp": timestamp(),
        })
    }

    /// Append `record` to the transcript: its id.
    fn put(&mut self, record: &Value) -> String {
        let id = record.get("uuid").and_then(Value::as_str).unwrap_or_default().to_owned();
        append(&self.transcript(), record);
        let kind = record.get("type").cloned().unwrap_or(Value::Null);
        self.note(
            "wrote",
            json!({ "at_us": now_us(), "uuid": id, "type": kind, "session": self.session }),
        );
        self.last = Some(id.clone());
        id
    }

    fn transcript(&self) -> PathBuf {
        self.config
            .join("projects")
            .join(escaped(&self.cwd))
            .join(format!("{}.jsonl", self.session))
    }

    fn registry(&self) -> PathBuf {
        self.config.join("sessions").join(format!("{}.json", std::process::id()))
    }

    /// Its record of itself, naming the session it holds now and saying what it does.
    fn register(&self) {
        let at = self.registry();
        let (status, waiting_for, since) = self.doing;
        let started = self.doc.get("started_us").and_then(Value::as_u64).unwrap_or_default();
        let mut record = json!({
            "pid": std::process::id(), "sessionId": self.session, "cwd": self.cwd,
            "startedAt": started / 1000, "kind": "interactive", "entrypoint": "cli",
            "version": VERSION, "status": status, "updatedAt": since, "statusUpdatedAt": since,
        });
        if let (Some(fields), Some(waiting_for)) = (record.as_object_mut(), waiting_for) {
            fields.insert("waitingFor".to_owned(), Value::from(waiting_for));
        }
        replace(&at, &record);
    }

    /// Its record says it does `status` from now, waiting for `waiting_for`, when that changed.
    fn doing(&mut self, status: &'static str, waiting_for: Option<&'static str>) {
        if (self.doing.0, self.doing.1) != (status, waiting_for) {
            self.doing = (status, waiting_for, now_us() / 1000);
            self.register();
        }
    }

    /// Add `value` to the record's list `list`.
    fn note(&mut self, list: &str, value: Value) {
        if let Some(items) = self.doc.get_mut(list).and_then(Value::as_array_mut) {
            items.push(value);
        }
    }

    /// Write the record whole.
    fn save(&self) {
        if let Some(at) = &self.record {
            replace(at, &self.doc);
        }
    }
}

/// An assistant message of `content`.
fn assistant(content: &Value) -> Value {
    json!({
        "id": format!("msg_{}", short(&uuid())), "type": "message", "role": "assistant",
        "model": "claude-stub", "content": content, "stop_reason": "end_turn",
        "stop_sequence": null, "usage": { "input_tokens": 1, "output_tokens": 1 },
    })
}

/// `call`'s result through the `slopty` server of `configs`, made again while it fails when
/// `until_ok`; an error result when the server would not answer.
fn call_until(configs: &[Value], call: &Value, until_ok: bool) -> Value {
    let begun = Instant::now();
    loop {
        let answer = call_tool(configs, call).unwrap_or_else(
            |e| json!({ "isError": true, "content": [{ "type": "text", "text": e }] }),
        );
        let failed = answer.get("isError").and_then(Value::as_bool).unwrap_or(false);
        if !failed || !until_ok || begun.elapsed() > UNTIL_OK.0 {
            return answer;
        }
        pause(UNTIL_OK.1);
    }
}

/// The relay the mod at `dir` names in its module, as the worker writes it: a JSON array of the
/// program and its arguments.
fn relay_of(dir: &Path) -> Option<Vec<String>> {
    let module = std::fs::read_to_string(dir.join("hooks").join("register.ts")).ok()?;
    let named = module.lines().find_map(|line| line.trim().strip_prefix(RELAY_LINE))?;
    serde_json::from_str::<Vec<String>>(named.trim().trim_end_matches(';')).ok()
}

/// Run hook `entry` on `payload`, to its end or its timeout; whether it ran.
fn run_hook(entry: &Value, payload: &Value) -> bool {
    let Some(command) = entry.get("command").and_then(Value::as_str) else { return false };
    let args: Option<Vec<&str>> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect());
    let mut run = if let Some(args) = args {
        let mut run = Command::new(command);
        run.args(args);
        run
    } else {
        let mut run = Command::new("/bin/sh");
        run.args(["-c", command]);
        run
    };
    let spawned = run.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let Ok(mut child) = spawned else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        drop(writeln!(stdin, "{payload}"));
    }
    let timeout = entry.get("timeout").and_then(Value::as_u64).unwrap_or(HOOK_TIMEOUT_S);
    let (done, ended) = mpsc::channel();
    let waiting = std::thread::spawn(move || {
        let status = child.wait();
        let _told = done.send(());
        status
    });
    if ended.recv_timeout(Duration::from_secs(timeout)).is_ok() {
        drop(waiting.join());
    }
    true
}

/// The terminal in raw mode, as it was put back when dropped; `None` when stdin is no terminal.
struct Raw(rustix::termios::Termios);

impl Raw {
    fn enter() -> Option<Self> {
        let stdin = std::io::stdin();
        let was = rustix::termios::tcgetattr(&stdin).ok()?;
        let mut raw = was.clone();
        raw.make_raw();
        rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw).ok()?;
        Some(Self(was))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let stdin = std::io::stdin();
        let _put_back =
            rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &self.0);
    }
}

/// Show `text` on its screen.
fn screen(text: &str) {
    let mut out = std::io::stdout().lock();
    drop(out.write_all(text.as_bytes()));
    drop(out.flush());
}

/// The `--settings` it was given: JSON, or a file of it.
fn settings(args: &[String]) -> Value {
    let Some(value) = flag(args, "--settings") else { return Value::Null };
    let text = if value.trim_start().starts_with('{') {
        value
    } else {
        std::fs::read_to_string(&value).unwrap_or_default()
    };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// The `--mcp-config` documents in `args`.
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

/// Call `call` through the `slopty` server of `configs`, in the 2026-07-28 lifecycle (no
/// `initialize`; the version and capabilities in the request's `_meta`): its result.
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
        .stderr(Stdio::null())
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

/// The value after `name` in `args` before any `--`, as `--name value` or `--name=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    let before = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let args = args.get(..before).unwrap_or_default();
    args.iter().enumerate().find_map(|(at, arg)| {
        if arg == name {
            args.get(at.saturating_add(1)).cloned()
        } else {
            arg.strip_prefix(&prefix).map(str::to_owned)
        }
    })
}

/// Copy `fields`' entries into `into`, both objects.
fn merge(into: &mut Value, fields: &Value) {
    if let (Some(into), Some(fields)) = (into.as_object_mut(), fields.as_object()) {
        for (k, v) in fields {
            into.insert(k.clone(), v.clone());
        }
    }
}

/// Its working directory as the process sees it: resolved, as Claude Code names its projects.
fn here() -> String {
    std::env::current_dir().map(|dir| dir.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The project directory's name Claude Code keeps a directory's conversations under.
fn escaped(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Append `value` to the file at `at` as one line, making its directory.
fn append(at: &Path, value: &Value) {
    if let Some(dir) = at.parent() {
        drop(std::fs::create_dir_all(dir));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(at) {
        drop(file.write_all(format!("{value}\n").as_bytes()));
    }
}

/// Replace the file at `at` with `value` whole, so a reader never sees half of it.
fn replace(at: &Path, value: &Value) {
    if let Some(dir) = at.parent() {
        drop(std::fs::create_dir_all(dir));
    }
    let staged = at.with_extension(format!("partial-{}", std::process::id()));
    if std::fs::write(&staged, value.to_string()).is_ok() {
        drop(std::fs::rename(&staged, at));
    }
}

/// Wait `for_` without a timer of the runtime's: a test's binary has none.
fn pause(for_: Duration) {
    let (_kept, never) = mpsc::channel::<()>();
    let _waited = never.recv_timeout(for_);
}

fn now_us() -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(now.as_micros()).unwrap_or(u64::MAX)
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

/// The first eight characters of `id`.
fn short(id: &str) -> String {
    id.chars().filter(char::is_ascii_alphanumeric).take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A paste arrives whole between its markers, over reads that cut them, its returns kept as
    /// new lines; the Enter after it sends the line, an Esc alone is a key, another sequence is
    /// passed over, and ^D ends only an empty line.
    #[test]
    fn keys_and_pastes_are_read_as_a_terminal_sends_them() {
        let mut input = Input::default();
        assert_eq!(input.take(b"\x1b[200~two\rli"), Vec::new());
        assert_eq!(input.take(b"nes\x1b[20"), Vec::new());
        assert_eq!(input.take(b"1~"), Vec::new(), "the paste is whole, not sent");
        assert_eq!(input.take(b"\r"), vec![Key::Line("two\nlines".to_owned())]);
        assert_eq!(input.take(b"\x1b"), vec![Key::Escape]);
        assert_eq!(input.take(b"\x1b[Ahi\x7fo\r"), vec![Key::Line("ho".to_owned())]);
        assert_eq!(input.take(b"x\x04"), Vec::new(), "^D on a line that has words");
        assert_eq!(input.take(b"\x7f\x04"), vec![Key::End]);
    }

    /// The names Claude Code keeps a project's sessions under, the dates its records carry, a
    /// session's id, and a flag read only before the prompt.
    #[test]
    fn records_are_named_as_claude_code_names_them() {
        assert_eq!(escaped("/private/tmp/sj/work"), "-private-tmp-sj-work");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_742), (2026, 10, 16));
        let id = uuid();
        assert_eq!(id.len(), 36, "{id}");
        let args: Vec<String> =
            ["--session-id", "s1", "--", "--session-id"].iter().map(|a| (*a).to_owned()).collect();
        assert_eq!(flag(&args, "--session-id").as_deref(), Some("s1"));
    }
}
