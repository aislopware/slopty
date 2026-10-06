//! What the verbs answer, twice: JSON with stable field names for scripts and models (the same
//! shapes from `slopty … --json`, `slopty mcp` and the server's MCP endpoint), and text for a
//! person.
//!
//! A terminal is always named by its `term`, `worker/session` with both ids in full, which
//! every verb accepts back. Text output shortens it to `name/prefix`, which verbs accept too.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde::Serialize;
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::orchestration::{
    Command, DirEntry, FileKind, FileStat, Happening, HubEvent, ItemRef, Line, Port, Screen,
    TermAgent, TermRef, Waited,
};
use slopty_proto::project::WorkerFacts;
use slopty_proto::screen::{DisplayInfo, WindowInfo};
use slopty_proto::search::{FileHits, SearchSummary};
use slopty_proto::server::{Liveness, Os, WorkerInfo};
use slopty_proto::terminal::{SessionState, SessionSummary};
use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::{AgentId, ThreadId};

use crate::ops::{Chunk, EventPage};

mod agents;
pub mod projects;

pub use agents::{
    ChoiceView, MovedView, ReadEntryView, RequestView, StillView, ThreadReadView, TurnView, moved,
    phase_key, still, thread_read, thread_read_text,
};

/// The shortest session-id prefix text output uses. `UUIDv7`s start with their creation time,
/// so ids minted close together share more than this and get longer prefixes.
const MIN_PREFIX: usize = 8;

/// A terminal's full handle.
pub fn term_string(term: TermRef) -> String {
    format!("{}/{}", term.worker, term.session)
}

/// An item's full handle.
pub fn item_string(item: ItemRef) -> String {
    format!("{}/{}", item.worker, item.item)
}

/// Everything `slopty workers` shows: the directory and the terminals, each with its agent.
#[derive(Debug, Default)]
pub struct Overview {
    /// The directory.
    pub workers: Vec<WorkerInfo>,
    /// Every terminal on every worker.
    pub terminals: Vec<(WorkerId, SessionSummary)>,
    /// The agent at work in each terminal that has one, as its thread's row says.
    pub agents: Vec<(TermRef, TermAgent)>,
    /// What each worker is and has; none from a worker reached without a server.
    pub facts: Vec<WorkerFacts>,
}

/// A worker, for JSON.
#[derive(Debug, Serialize)]
pub struct WorkerView<'a> {
    worker: WorkerId,
    name: String,
    liveness: &'static str,
    address: String,
    os: &'static str,
    os_version: String,
    arch: String,
    cpus: u16,
    memory: u64,
    load: f32,
    version: String,
    can_capture: bool,
    can_inject: bool,
    last_seen_ms: WallMs,
    terminals: usize,
    waiting: Vec<WaitingView>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    facts: BTreeMap<&'a str, serde_json::Value>,
}

/// An agent that needs a human, for JSON.
#[derive(Debug, Serialize)]
pub struct WaitingView {
    term: String,
    /// The agent, by its id (`claude-code`, `codex`, `pi`, `acp:<name>`).
    agent: String,
    /// What it asks.
    asks: Option<String>,
}

/// A terminal, for JSON.
#[derive(Debug, Serialize)]
pub struct TerminalView {
    term: String,
    worker: WorkerId,
    worker_name: Option<String>,
    session: SessionId,
    title: String,
    cwd: Option<String>,
    repo: Option<String>,
    cols: u16,
    rows: u16,
    running: bool,
    exit_status: Option<i32>,
    viewers: u16,
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentView>,
}

/// An agent's status, as its thread's row says, for JSON.
#[derive(Debug, Serialize)]
pub struct AgentView {
    /// The agent, by its id (`claude-code`, `codex`, `pi`, `acp:<name>`); none with no agent.
    agent: Option<String>,
    /// Its thread, the handle the thread verbs take.
    thread: Option<ThreadId>,
    /// Its phase: `idle`, `working`, `waiting`, `needs_you`, `done`, `failed`, `stopped`, or
    /// `none` with no agent.
    status: &'static str,
    /// What it waits on, while it waits: its adapter's words.
    waits: Option<String>,
    /// What it asks the person, while it asks.
    asks: Option<String>,
    needs_human: bool,
}

/// A line, for JSON.
#[derive(Debug, Serialize)]
pub struct LineView<'a> {
    index: u64,
    text: &'a str,
}

/// The screen, for JSON.
#[derive(Debug, Serialize)]
pub struct ScreenView<'a> {
    lines: Vec<LineView<'a>>,
    cursor_row: u16,
    cursor_col: u16,
    title: &'a str,
    cwd: Option<&'a str>,
    alternate: bool,
}

/// Scrollback, for JSON.
#[derive(Debug, Serialize)]
pub struct OutputView<'a> {
    lines: Vec<LineView<'a>>,
    next: u64,
}

/// A command block, for JSON.
#[derive(Debug, Serialize)]
pub struct CommandView<'a> {
    command: &'a str,
    prompt_line: u64,
    output_start: u64,
    output_end: u64,
    exit: Option<i32>,
    running: bool,
}

/// How a wait ended, for JSON.
#[derive(Debug, Serialize)]
pub struct WaitedView<'a> {
    result: &'static str,
    line: Option<LineView<'a>>,
}

/// A listening port, for JSON.
#[derive(Debug, Serialize)]
pub struct PortView {
    port: u16,
    pid: u32,
    process: String,
    term: Option<String>,
}

/// How a file's contents are spelled in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// The bytes are UTF-8 text, as they are.
    #[default]
    Utf8,
    /// The bytes in standard base64, for anything that is not UTF-8.
    Base64,
}

/// A file's contents, for JSON: UTF-8 text as it is, anything else in base64.
#[derive(Debug, Serialize)]
pub struct FileView<'a> {
    path: &'a str,
    /// The whole file's size.
    size: u64,
    /// Where `content` starts in the file.
    offset: u64,
    /// How many bytes `content` holds.
    length: usize,
    /// The file goes on past `content`.
    more: bool,
    encoding: Encoding,
    content: Cow<'a, str>,
}

/// A directory, for JSON.
#[derive(Debug, Serialize)]
pub struct DirView<'a> {
    path: &'a str,
    entries: Vec<EntryView<'a>>,
    total: u32,
    truncated: bool,
}

/// One entry of a directory, for JSON.
#[derive(Debug, Serialize)]
pub struct EntryView<'a> {
    name: &'a str,
    kind: &'static str,
    size: u64,
    modified_ms: WallMs,
}

/// A search in files, for JSON.
#[derive(Debug, Serialize)]
pub struct SearchView<'a> {
    root: &'a str,
    files: Vec<SearchFileView<'a>>,
    /// Matching lines returned.
    lines: u32,
    /// Files the search read.
    searched: u32,
    /// There were more matching lines than returned.
    capped: bool,
    elapsed_ms: u32,
}

/// One file of a search, for JSON.
#[derive(Debug, Serialize)]
pub struct SearchFileView<'a> {
    /// Relative to the search's root.
    path: &'a str,
    lines: Vec<SearchLineView<'a>>,
}

/// A matching line or a line of context round one, for JSON.
#[derive(Debug, Serialize)]
pub struct SearchLineView<'a> {
    line: u32,
    /// Without its indentation, cut round its first match when it is long.
    text: &'a str,
    /// Byte ranges of the matches in `text`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    matches: Vec<[u32; 2]>,
    /// A line round a match, not one.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    context: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    cut_before: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    cut_after: bool,
}

/// What is at a path, for JSON.
#[derive(Debug, Serialize)]
pub struct StatView<'a> {
    path: &'a str,
    exists: bool,
    kind: Option<&'static str>,
    size: Option<u64>,
    modified_ms: Option<WallMs>,
    /// Permission bits in octal, `"755"`.
    mode: Option<String>,
}

/// A page of the server's events, for JSON.
#[derive(Debug, Serialize)]
pub struct EventsView<'a> {
    events: Vec<EventView<'a>>,
    next: u64,
    missed: u64,
}

/// One event, for JSON: the fields its `kind` has.
#[derive(Debug, Serialize)]
pub struct EventView<'a> {
    seq: u64,
    at_ms: WallMs,
    kind: &'static str,
    worker: WorkerId,
    #[serde(skip_serializing_if = "Option::is_none")]
    term: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    liveness: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Cow<'a, str>>,
    /// A program's exit status, or its signal number negated.
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_status: Option<i32>,
    /// A project's change: its name.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a str>,
    /// A project's change: the task it concerns.
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<u32>,
    /// A project's change: the task's state now.
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<&'static str>,
}

/// A new terminal, for JSON.
#[derive(Debug, Serialize)]
pub struct OpenedView {
    term: String,
    worker: WorkerId,
    session: SessionId,
}

/// Where an entry a change to the files left now is, for JSON.
#[derive(Debug, Serialize)]
pub struct PlacedView<'a> {
    path: &'a str,
}

/// Where an entry made, moved or trashed now is.
#[must_use]
pub const fn placed(path: &str) -> PlacedView<'_> {
    PlacedView { path }
}

/// Nothing to report, for JSON.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DoneView {
    ok: bool,
}

/// The `Done` answer.
pub const DONE: DoneView = DoneView { ok: true };

const fn liveness(l: Liveness) -> &'static str {
    match l {
        Liveness::Online => "online",
        Liveness::Unreachable => "unreachable",
        Liveness::Gone => "gone",
    }
}

const fn os_key(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macos",
        Os::Linux => "linux",
    }
}

const fn os_name(os: Os) -> &'static str {
    match os {
        Os::MacOs => "macOS",
        Os::Linux => "Linux",
    }
}

/// An agent's name, for a person.
fn agent_name(agent: &AgentId) -> String {
    match agent.0.as_str() {
        AgentId::CLAUDE_CODE => "Claude Code".to_owned(),
        AgentId::CODEX => "Codex".to_owned(),
        AgentId::PI => "pi".to_owned(),
        _ => agent.acp_name().unwrap_or(&agent.0).to_owned(),
    }
}

/// What an agent is doing, for a person: its phase, and what it waits on or asks.
fn status_text(agent: &TermAgent) -> String {
    let phase = phase_key(agent.phase).replace('_', " ");
    let said = agent.asks.as_ref().or_else(|| agent.wait.as_ref().map(|w| &w.text));
    match said {
        Some(said) => format!("{phase}: {said}"),
        None => phase,
    }
}

/// An agent's status, for JSON.
pub fn agent(agent: Option<&TermAgent>) -> AgentView {
    let Some(a) = agent else {
        return AgentView {
            agent: None,
            thread: None,
            status: "none",
            waits: None,
            asks: None,
            needs_human: false,
        };
    };
    AgentView {
        agent: Some(a.agent.0.clone()),
        thread: Some(a.thread),
        status: phase_key(a.phase),
        waits: a.wait.as_ref().map(|w| w.text.clone()),
        asks: a.asks.clone(),
        needs_human: a.rung == Rung::NeedsYou,
    }
}

/// An agent's status, for a person: "Claude Code  needs you: Which branch?".
pub fn agent_text(agent: Option<&TermAgent>) -> String {
    let Some(a) = agent else { return "no agent".to_owned() };
    format!("{}  {}", agent_name(&a.agent), status_text(a))
}

impl Overview {
    /// The agents on `worker` waiting on a human. A worker that is not online has none: what
    /// it last reported may have been answered since.
    fn waiting(&self, worker: WorkerId) -> Vec<(TermRef, &TermAgent)> {
        let online =
            self.workers.iter().any(|w| w.worker == worker && w.liveness == Liveness::Online);
        if !online {
            return Vec::new();
        }
        let mut waiting: Vec<_> = self
            .agents
            .iter()
            .filter(|(term, a)| term.worker == worker && a.rung == Rung::NeedsYou)
            .map(|(term, a)| (*term, a))
            .collect();
        waiting.sort_by_key(|(term, _)| term.session);
        waiting
    }

    fn terminal_count(&self, worker: WorkerId) -> usize {
        self.terminals.iter().filter(|(w, _)| *w == worker).count()
    }

    /// The workers, for JSON.
    pub fn json(&self) -> Vec<WorkerView<'_>> {
        self.workers
            .iter()
            .map(|w| WorkerView {
                worker: w.worker,
                name: w.name.clone(),
                liveness: liveness(w.liveness),
                address: w.address.clone(),
                os: os_key(w.caps.os),
                os_version: w.caps.os_version.clone(),
                arch: w.caps.arch.clone(),
                cpus: w.caps.cpus,
                memory: w.caps.memory,
                load: w.load,
                version: w.caps.version.clone(),
                can_capture: w.caps.can_capture,
                can_inject: w.caps.can_inject,
                last_seen_ms: w.last_seen_ms,
                terminals: self.terminal_count(w.worker),
                facts: self
                    .facts
                    .iter()
                    .find(|f| f.worker == w.worker)
                    .map(|f| projects::facts(&f.facts))
                    .unwrap_or_default(),
                waiting: self
                    .waiting(w.worker)
                    .into_iter()
                    .map(|(term, a)| WaitingView {
                        term: term_string(term),
                        agent: a.agent.0.clone(),
                        asks: a.asks.clone().or_else(|| a.wait.as_ref().map(|w| w.text.clone())),
                    })
                    .collect(),
            })
            .collect()
    }

    /// The workers, for a person: one row each, then the agents waiting on a human.
    pub fn text(&self) -> String {
        if self.workers.is_empty() {
            return "no workers have registered with the server\n".to_owned();
        }
        let short = ShortTerms::new(&self.workers, &self.terminals);
        let rows = self
            .workers
            .iter()
            .map(|w| {
                let waiting = self.waiting(w.worker).len();
                vec![
                    w.name.clone(),
                    liveness(w.liveness).to_owned(),
                    w.address.clone(),
                    format!("{} {}", os_name(w.caps.os), w.caps.os_version),
                    self.terminal_count(w.worker).to_string(),
                    if waiting == 0 { "-".to_owned() } else { waiting.to_string() },
                ]
            })
            .collect();
        let mut out = table(&["NAME", "STATE", "ADDRESS", "OS", "TERMINALS", "WAITING"], rows);
        let waiting: Vec<Vec<String>> = self
            .workers
            .iter()
            .flat_map(|w| self.waiting(w.worker))
            .map(|(term, a)| {
                let asks = a.asks.clone().or_else(|| a.wait.as_ref().map(|w| w.text.clone()));
                vec![
                    short.get(term),
                    agent_name(&a.agent),
                    asks.unwrap_or_default(),
                    a.title.clone(),
                ]
            })
            .collect();
        if !waiting.is_empty() {
            out.push_str("\nwaiting on a human:\n");
            for line in table(&["TERM", "AGENT", "FOR", "TITLE"], waiting).lines() {
                let _infallible = writeln!(out, "  {line}");
            }
        }
        out
    }
}

/// The agent at work in `term`, among `agents`.
fn agent_in(agents: &[(TermRef, TermAgent)], term: TermRef) -> Option<&TermAgent> {
    agents.iter().find(|(t, _)| *t == term).map(|(_, a)| a)
}

/// Terminals, for JSON, each with its agent from `agents`.
pub fn terminals_json(
    workers: &[WorkerInfo],
    terminals: &[(WorkerId, SessionSummary)],
    agents: &[(TermRef, TermAgent)],
) -> Vec<TerminalView> {
    terminals
        .iter()
        .map(|(worker, s)| {
            let (running, exit_status) = match s.state {
                SessionState::Running => (true, None),
                SessionState::Exited { status } => (false, Some(status)),
            };
            TerminalView {
                term: term_string(TermRef { worker: *worker, session: s.id }),
                worker: *worker,
                worker_name: workers.iter().find(|w| w.worker == *worker).map(|w| w.name.clone()),
                session: s.id,
                title: s.title.clone(),
                cwd: s.cwd.clone(),
                repo: s.repo.clone(),
                cols: s.cols,
                rows: s.rows,
                running,
                exit_status,
                viewers: s.viewers,
                command: s.command.clone(),
                agent: agent_in(agents, TermRef { worker: *worker, session: s.id })
                    .map(|a| agent(Some(a))),
            }
        })
        .collect()
}

/// Terminals, for a person, each with its agent from `agents`.
pub fn terminals_text(
    workers: &[WorkerInfo],
    terminals: &[(WorkerId, SessionSummary)],
    agents: &[(TermRef, TermAgent)],
) -> String {
    if terminals.is_empty() {
        return "no terminals\n".to_owned();
    }
    let short = ShortTerms::new(workers, terminals);
    let rows = terminals
        .iter()
        .map(|(worker, s)| {
            let state = match s.state {
                SessionState::Running => "running".to_owned(),
                SessionState::Exited { status } => format!("exited {status}"),
            };
            vec![
                short.get(TermRef { worker: *worker, session: s.id }),
                state,
                format!("{}x{}", s.cols, s.rows),
                s.viewers.to_string(),
                s.title.clone(),
                s.cwd.clone().unwrap_or_default(),
                agent_in(agents, TermRef { worker: *worker, session: s.id })
                    .map_or_else(|| "-".to_owned(), status_text),
            ]
        })
        .collect();
    table(&["TERM", "STATE", "SIZE", "VIEWERS", "TITLE", "CWD", "AGENT"], rows)
}

/// Lines, for JSON.
pub fn lines(lines: &[Line]) -> Vec<LineView<'_>> {
    lines.iter().map(|l| LineView { index: l.index, text: &l.text }).collect()
}

/// The screen, for JSON.
pub fn screen(screen: &Screen) -> ScreenView<'_> {
    ScreenView {
        lines: lines(&screen.lines),
        cursor_row: screen.cursor.0,
        cursor_col: screen.cursor.1,
        title: &screen.title,
        cwd: screen.cwd.as_deref(),
        alternate: screen.alternate,
    }
}

/// Scrollback, for JSON.
pub fn output(output: &[Line], next: u64) -> OutputView<'_> {
    OutputView { lines: lines(output), next }
}

/// Command blocks, for JSON.
pub fn commands(commands: &[Command]) -> Vec<CommandView<'_>> {
    commands
        .iter()
        .map(|c| CommandView {
            command: &c.line,
            prompt_line: c.prompt_line,
            output_start: c.output.0,
            output_end: c.output.1,
            exit: c.exit,
            running: c.exit.is_none(),
        })
        .collect()
}

/// Command blocks, for a person.
pub fn commands_text(commands: &[Command]) -> String {
    if commands.is_empty() {
        return "no commands (the shell may lack OSC 133 integration)\n".to_owned();
    }
    let rows = commands
        .iter()
        .map(|c| {
            vec![
                c.exit.map_or_else(|| "…".to_owned(), |e| e.to_string()),
                format!("{}-{}", c.output.0, c.output.1),
                c.line.clone(),
            ]
        })
        .collect();
    table(&["EXIT", "LINES", "COMMAND"], rows)
}

/// How a wait ended, for JSON.
pub fn waited(waited: &Waited) -> WaitedView<'_> {
    match waited {
        Waited::Met { line } => WaitedView {
            result: "met",
            line: line.as_ref().map(|l| LineView { index: l.index, text: &l.text }),
        },
        Waited::TimedOut => WaitedView { result: "timed_out", line: None },
        Waited::Closed => WaitedView { result: "closed", line: None },
    }
}

/// Listening ports, for JSON.
pub fn ports(worker: WorkerId, ports: &[Port]) -> Vec<PortView> {
    ports
        .iter()
        .map(|p| PortView {
            port: p.number,
            pid: p.pid,
            process: p.process.clone(),
            term: p.session.map(|session| term_string(TermRef { worker, session })),
        })
        .collect()
}

/// Listening ports, for a person.
pub fn ports_text(worker: WorkerId, ports: &[Port]) -> String {
    if ports.is_empty() {
        return "no listening ports\n".to_owned();
    }
    let rows = ports
        .iter()
        .map(|p| {
            vec![
                p.number.to_string(),
                p.pid.to_string(),
                p.process.clone(),
                p.session
                    .map(|session| term_string(TermRef { worker, session }))
                    .unwrap_or_default(),
            ]
        })
        .collect();
    table(&["PORT", "PID", "PROCESS", "TERM"], rows)
}

/// A file's contents, for JSON.
pub fn file<'a>(path: &'a str, chunk: &'a Chunk) -> FileView<'a> {
    let bytes = &chunk.bytes;
    let (encoding, content) = match std::str::from_utf8(bytes) {
        Ok(text) => (Encoding::Utf8, Cow::Borrowed(text)),
        Err(_binary) => (Encoding::Base64, Cow::Owned(data_encoding::BASE64.encode(bytes))),
    };
    let end = chunk.offset.saturating_add(bytes.len() as u64);
    FileView {
        path,
        size: chunk.size,
        offset: chunk.offset,
        length: bytes.len(),
        more: end < chunk.size,
        encoding,
        content,
    }
}

const fn kind_key(kind: FileKind) -> &'static str {
    match kind {
        FileKind::File => "file",
        FileKind::Dir => "dir",
        FileKind::Symlink => "symlink",
        FileKind::Other => "other",
    }
}

/// A directory, for JSON.
pub fn dir<'a>(path: &'a str, entries: &'a [DirEntry], total: u32) -> DirView<'a> {
    let listed = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    DirView {
        path,
        entries: entries
            .iter()
            .map(|e| EntryView {
                name: &e.name,
                kind: kind_key(e.kind),
                size: e.size,
                modified_ms: e.modified_ms,
            })
            .collect(),
        total,
        truncated: listed < total,
    }
}

/// A directory, for a person: `ls -p` style, a directory's name ending in `/` and a link's
/// in `@`.
pub fn dir_text(entries: &[DirEntry], total: u32) -> String {
    let rows = entries
        .iter()
        .map(|e| {
            let mark = match e.kind {
                FileKind::Dir => "/",
                FileKind::Symlink => "@",
                FileKind::File | FileKind::Other => "",
            };
            vec![e.size.to_string(), format!("{}{mark}", e.name)]
        })
        .collect();
    let mut out = table(&["SIZE", "NAME"], rows);
    let listed = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    if listed < total {
        let _infallible = writeln!(out, "… {} more", total.saturating_sub(listed));
    }
    out
}

/// What is at a path, for JSON.
pub fn stat<'a>(path: &'a str, stat: Option<&FileStat>) -> StatView<'a> {
    StatView {
        path,
        exists: stat.is_some(),
        kind: stat.map(|s| kind_key(s.kind)),
        size: stat.map(|s| s.size),
        modified_ms: stat.map(|s| s.modified_ms),
        mode: stat.map(|s| format!("{:o}", s.mode)),
    }
}

/// What is at a path, for a person.
pub fn stat_text(path: &str, stat: Option<&FileStat>) -> String {
    stat.map_or_else(
        || format!("{path}: nothing there\n"),
        |s| format!("{path}: {} {} bytes mode {:o}\n", kind_key(s.kind), s.size, s.mode),
    )
}

/// A file's matching lines and the context round them, top down: each line once.
fn search_lines(file: &FileHits) -> Vec<SearchLineView<'_>> {
    let mut lines: Vec<SearchLineView<'_>> = file
        .lines
        .iter()
        .map(|l| SearchLineView {
            line: l.line,
            text: &l.text,
            matches: l.spans.iter().map(|s| [s.start, s.end]).collect(),
            context: false,
            cut_before: l.cut_before,
            cut_after: l.cut_after,
        })
        .chain(file.context.iter().map(|c| SearchLineView {
            line: c.line,
            text: &c.text,
            matches: Vec::new(),
            context: true,
            cut_before: false,
            cut_after: c.cut_after,
        }))
        .collect();
    lines.sort_by_key(|l| l.line);
    lines
}

/// A search in files, for JSON.
pub fn search<'a>(root: &'a str, files: &'a [FileHits], summary: &SearchSummary) -> SearchView<'a> {
    SearchView {
        root,
        files: files
            .iter()
            .map(|f| SearchFileView { path: &f.path, lines: search_lines(f) })
            .collect(),
        lines: summary.lines,
        searched: summary.searched,
        capped: summary.capped,
        elapsed_ms: summary.elapsed_ms,
    }
}

/// A search in files, for a person: as ripgrep prints it with `--heading`, a match's number
/// followed by `:` and a context line's by `-`, then what it came to.
pub fn search_text(files: &[FileHits], summary: &SearchSummary) -> String {
    let mut out = String::new();
    for file in files {
        let _infallible = writeln!(out, "{}", file.path);
        for line in search_lines(file) {
            let mark = if line.context { '-' } else { ':' };
            let (before, after) =
                (if line.cut_before { "…" } else { "" }, if line.cut_after { "…" } else { "" });
            let _infallible = writeln!(out, "{}{mark}{before}{}{after}", line.line, line.text);
        }
        out.push('\n');
    }
    let noun = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let _infallible = writeln!(
        out,
        "{} in {}, {} searched in {} ms",
        noun(summary.lines, "matching line", "matching lines"),
        noun(u32::try_from(files.len()).unwrap_or(u32::MAX), "file", "files"),
        noun(summary.searched, "file", "files"),
        summary.elapsed_ms,
    );
    if summary.capped {
        out.push_str("There are more: narrow the search with --glob or a sharper pattern.\n");
    }
    out
}

/// A page of the server's events, for JSON.
pub fn events(page: &EventPage) -> EventsView<'_> {
    EventsView {
        events: page.events.iter().map(event).collect(),
        next: page.next,
        missed: page.missed,
    }
}

/// One event, for JSON.
pub fn event(e: &HubEvent) -> EventView<'_> {
    let mut view = EventView {
        seq: e.seq,
        at_ms: e.at_ms,
        kind: "",
        worker: WorkerId::nil(),
        term: None,
        name: None,
        liveness: None,
        title: None,
        cwd: None,
        command: None,
        agent: None,
        detail: None,
        exit_status: None,
        project: None,
        task: None,
        state: None,
    };
    match &e.what {
        Happening::Worker { worker, name, liveness: l } => {
            view.kind = "worker";
            view.worker = *worker;
            view.name = Some(name);
            view.liveness = Some(liveness(*l));
        }
        Happening::WorkerRemoved { worker, name } => {
            view.kind = "worker_removed";
            view.worker = *worker;
            view.name = Some(name);
        }
        Happening::SessionOpened { worker, summary } => {
            view.kind = "session_opened";
            view.worker = *worker;
            view.term = Some(term_string(TermRef { worker: *worker, session: summary.id }));
            view.title = Some(&summary.title);
            view.cwd = summary.cwd.as_deref();
            view.command = Some(&summary.command);
        }
        Happening::SessionClosed { term } => {
            view.kind = "session_closed";
            view.worker = term.worker;
            view.term = Some(term_string(*term));
        }
        Happening::SessionExited { term, status } => {
            view.kind = "session_exited";
            view.worker = term.worker;
            view.term = Some(term_string(*term));
            view.exit_status = Some(*status);
        }
        Happening::Rung { worker, terminal, agent: a } => {
            view.kind = "agent";
            view.worker = *worker;
            view.term = terminal.map(|session| term_string(TermRef { worker: *worker, session }));
            view.agent = Some(agent(Some(a)));
            view.title = Some(&a.title);
        }
        Happening::Project(update) => {
            view.kind = "project";
            view.project = Some(update.project.as_str());
            let task = update.task.as_ref();
            view.task = update
                .entry
                .as_ref()
                .and_then(|e| e.task)
                .or_else(|| task.map(|t| t.id))
                .or_else(|| update.native.as_ref().and_then(|n| n.task))
                .map(|t| t.0);
            view.state = task.map(|t| projects::state_word(t.state));
            let term = task.and_then(|t| t.assignment.as_ref()).map(|a| a.term);
            let term = term.or_else(|| update.record.as_ref().and_then(|p| p.orchestrator));
            if let Some(term) = term {
                view.worker = term.worker;
                view.term = Some(term_string(term));
            }
            view.title = task.map(|t| t.title.as_str());
            view.detail = update.entry.as_ref().map(|e| Cow::Owned(projects::moment(&e.what).1));
        }
    }
    view
}

/// One event, for a person, on one line; workers by name where `names` knows them.
pub fn event_text<S: std::hash::BuildHasher>(
    e: &HubEvent,
    names: &HashMap<WorkerId, String, S>,
) -> String {
    let worker = |id: &WorkerId| names.get(id).cloned().unwrap_or_else(|| id.to_string());
    let term = |t: &TermRef| format!("{}/{}", worker(&t.worker), t.session);
    let what = match &e.what {
        Happening::Worker { name, liveness: l, .. } => format!("worker  {name} {}", liveness(*l)),
        Happening::WorkerRemoved { name, .. } => format!("worker  {name} removed"),
        Happening::SessionOpened { worker: w, summary } => {
            let t = TermRef { worker: *w, session: summary.id };
            format!("opened  {}  {}", term(&t), summary.title)
        }
        Happening::SessionClosed { term: t } => format!("closed  {}", term(t)),
        Happening::SessionExited { term: t, status } => format!("exited  {}  {status}", term(t)),
        Happening::Rung { worker: w, terminal, agent: a } => {
            let at = terminal.map_or_else(
                || format!("{}/{}", worker(w), a.thread),
                |session| term(&TermRef { worker: *w, session }),
            );
            format!("agent   {at}  {}", agent_text(Some(a)))
        }
        Happening::Project(update) => {
            let task = update
                .entry
                .as_ref()
                .and_then(|e| e.task)
                .or_else(|| update.task.as_ref().map(|t| t.id))
                .or_else(|| update.native.as_ref().and_then(|n| n.task));
            let task = task.map_or_else(String::new, |t| format!(" #{t}"));
            let what = update.entry.as_ref().map_or_else(
                || {
                    update
                        .task
                        .as_ref()
                        .map_or("changed", |t| projects::state_word(t.state))
                        .to_owned()
                },
                |e| projects::moment(&e.what).1,
            );
            format!("project {}{task}  {what}", update.project)
        }
    };
    format!("{:>6}  {what}", e.seq)
}

/// A new terminal, for JSON.
pub fn opened(term: TermRef) -> OpenedView {
    OpenedView { term: term_string(term), worker: term.worker, session: term.session }
}

/// A wake sent to a sleeping worker, for JSON.
#[derive(Debug, Serialize)]
pub struct WokenView<'a> {
    worker: WorkerId,
    /// The machine that sent the magic packet.
    by: &'a str,
    /// The sleeping worker's interfaces it went to.
    to: &'a [String],
    /// The worker said it sleeps through a magic packet (Wake for network access is off).
    wake_on_lan_off: bool,
}

/// The wake [`crate::ops::wake`] sent.
pub fn woken(woken: &crate::ops::Woken) -> WokenView<'_> {
    WokenView {
        worker: woken.worker,
        by: &woken.by,
        to: &woken.to,
        wake_on_lan_off: woken.wake_on_lan_off,
    }
}

/// Short handles for text output: the worker's name where it is unique (its id otherwise), and
/// the shortest session-id prefix no other listed session shares.
struct ShortTerms {
    workers: HashMap<WorkerId, String>,
    prefix: usize,
}

impl ShortTerms {
    fn new(workers: &[WorkerInfo], terminals: &[(WorkerId, SessionSummary)]) -> Self {
        let named = workers
            .iter()
            .map(|w| {
                let unique = workers.iter().filter(|o| o.name == w.name).count() == 1;
                (w.worker, if unique { w.name.clone() } else { w.worker.to_string() })
            })
            .collect();
        let ids: Vec<String> = terminals.iter().map(|(_, s)| s.id.to_string()).collect();
        let mut prefix = MIN_PREFIX;
        for (i, a) in ids.iter().enumerate() {
            for b in ids.iter().skip(i.saturating_add(1)) {
                let shared = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
                prefix = prefix.max(shared.saturating_add(1));
            }
        }
        Self { workers: named, prefix }
    }

    fn get(&self, term: TermRef) -> String {
        let worker =
            self.workers.get(&term.worker).cloned().unwrap_or_else(|| term.worker.to_string());
        let session = term.session.to_string();
        let prefix = session.get(..self.prefix).unwrap_or(&session);
        format!("{worker}/{prefix}")
    }
}

/// An item on a workspace, for JSON: its handle, its kind and the one field that says what it
/// shows.
#[derive(Debug, Serialize)]
pub struct ItemView<'a> {
    item: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    term: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<String>,
}

/// A new item, for JSON.
#[derive(Debug, Serialize)]
pub struct OpenedItemView {
    item: String,
}

/// A window a worker can stream, for JSON.
#[derive(Debug, Serialize)]
pub struct WindowView<'a> {
    window: u32,
    app: &'a str,
    title: &'a str,
    display: u32,
    on_screen: bool,
    width: f32,
    height: f32,
}

/// A display a worker can stream, for JSON.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DisplayView {
    display: u32,
    width: f32,
    height: f32,
    scale: f32,
    hz: f32,
}

/// What a worker can stream, for JSON.
#[derive(Debug, Serialize)]
pub struct ScreensView<'a> {
    windows: Vec<WindowView<'a>>,
    displays: Vec<DisplayView>,
}

const fn kind_word(kind: &ItemKind) -> &'static str {
    match kind {
        ItemKind::Terminal { .. } => "terminal",
        ItemKind::Window { .. } => "window",
        ItemKind::Display { .. } => "display",
        ItemKind::File { .. } => "file",
        ItemKind::Folder { .. } => "folder",
        ItemKind::Browser { .. } => "browser",
        ItemKind::Review { .. } => "review",
        ItemKind::Thread { .. } => "thread",
        ItemKind::Changes { .. } => "changes",
    }
}

/// A workspace's items, for JSON.
pub fn items(worker: WorkerId, items: &[Item]) -> Vec<ItemView<'_>> {
    items
        .iter()
        .map(|i| {
            let mut view = ItemView {
                item: item_string(ItemRef { worker, item: i.id }),
                kind: kind_word(&i.kind),
                name: i.name.as_deref(),
                term: None,
                window: None,
                display: None,
                path: None,
                url: None,
                thread: None,
            };
            match &i.kind {
                ItemKind::Terminal { session } => {
                    view.term = Some(term_string(TermRef { worker, session: *session }));
                }
                ItemKind::Window { window } => view.window = Some(window.0),
                ItemKind::Display { display } => view.display = Some(display.0),
                ItemKind::File { path }
                | ItemKind::Folder { path }
                | ItemKind::Changes { path, .. } => view.path = Some(path),
                ItemKind::Browser { url } => view.url = Some(url),
                ItemKind::Review { thread } | ItemKind::Thread { thread } => {
                    view.thread = Some(thread.to_string());
                }
            }
            view
        })
        .collect()
}

/// A workspace's items, for a person.
pub fn items_text(worker: WorkerId, items: &[Item]) -> String {
    if items.is_empty() {
        return "no items\n".to_owned();
    }
    let rows = items
        .iter()
        .map(|i| {
            let shows = match &i.kind {
                ItemKind::Terminal { session } => {
                    term_string(TermRef { worker, session: *session })
                }
                ItemKind::Window { window } => window.0.to_string(),
                ItemKind::Display { display } => display.to_string(),
                ItemKind::File { path }
                | ItemKind::Folder { path }
                | ItemKind::Changes { path, .. } => path.clone(),
                ItemKind::Browser { url } => url.clone(),
                ItemKind::Review { thread } | ItemKind::Thread { thread } => thread.to_string(),
            };
            vec![
                i.id.to_string(),
                kind_word(&i.kind).to_owned(),
                i.name.clone().unwrap_or_default(),
                shows,
            ]
        })
        .collect();
    table(&["ITEM", "KIND", "NAME", "SHOWS"], rows)
}

/// A new item, for JSON.
pub fn opened_item(item: ItemRef) -> OpenedItemView {
    OpenedItemView { item: item_string(item) }
}

/// What a worker can stream, for JSON.
pub fn screens<'a>(windows: &'a [WindowInfo], displays: &[DisplayInfo]) -> ScreensView<'a> {
    ScreensView {
        windows: windows
            .iter()
            .map(|w| WindowView {
                window: w.id.0,
                app: &w.app,
                title: &w.title,
                display: w.display.0,
                on_screen: w.on_screen,
                width: w.w,
                height: w.h,
            })
            .collect(),
        displays: displays
            .iter()
            .map(|d| DisplayView {
                display: d.id.0,
                width: d.w,
                height: d.h,
                scale: d.scale,
                hz: d.hz,
            })
            .collect(),
    }
}

/// What a worker can stream, for a person.
pub fn screens_text(windows: &[WindowInfo], displays: &[DisplayInfo]) -> String {
    let size = |w: f32, h: f32| format!("{w:.0}x{h:.0}");
    let windows = windows
        .iter()
        .map(|w| {
            vec![
                w.id.0.to_string(),
                w.app.clone(),
                w.title.clone(),
                w.display.to_string(),
                size(w.w, w.h),
            ]
        })
        .collect();
    let displays = displays
        .iter()
        .map(|d| vec![d.id.to_string(), size(d.w, d.h), format!("{}x", d.scale), d.hz.to_string()])
        .collect();
    let mut out = table(&["WINDOW", "APP", "TITLE", "DISPLAY", "SIZE"], windows);
    out.push('\n');
    out.push_str(&table(&["DISPLAY", "SIZE", "SCALE", "HZ"], displays));
    out
}

/// Left-aligned columns two spaces apart, a header row first, no trailing blanks.
fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let header = headers.iter().map(|h| (*h).to_owned()).collect();
    let mut out = String::new();
    for row in std::iter::once(header).chain(rows) {
        let mut line = String::new();
        for (cell, width) in row.iter().zip(&widths) {
            let _infallible = write!(line, "{cell:<width$}  ");
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use slopty_proto::server::WorkerCaps;
    use slopty_proto::thread::{Phase, Wait};

    use super::*;

    fn worker(n: u8) -> WorkerId {
        format!("0199a000-0000-7000-8000-0000000000{n:02x}").parse().unwrap()
    }

    /// Ids that differ in their first eight characters, as sessions opened minutes apart do.
    fn session(n: u8) -> SessionId {
        format!("0199a1b{n:x}-c3d4-7000-8000-00000000abcd").parse().unwrap()
    }

    fn caps(version: &str) -> WorkerCaps {
        WorkerCaps {
            os: Os::MacOs,
            os_version: version.to_owned(),
            arch: "aarch64".to_owned(),
            form: slopty_proto::server::Form::Desktop,
            cpus: 24,
            memory: 64 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            virtual_displays: false,
            version: "0.1.0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
            writes_failing: None,
            stops_at_logout: None,
        }
    }

    fn summary(n: u8, title: &str, cwd: &str, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: session(n),
            title: title.to_owned(),
            cwd: Some(cwd.to_owned()),
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 120,
            rows: 40,
            state,
            viewers: 1,
            command: vec!["zsh".to_owned()],
            progress: None,
            restored: None,
            repo_id: None,
        }
    }

    /// Claude Code in thread `n`, in `phase`, asking `asks`.
    fn at(n: u8, phase: Phase, asks: Option<&str>) -> TermAgent {
        TermAgent {
            thread: format!("0199a000-0000-7000-8000-0000000071{n:02x}").parse().unwrap(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: format!("Thread {n}"),
            rung: if asks.is_some() { Rung::NeedsYou } else { Rung::Working },
            phase,
            wait: None,
            asks: asks.map(str::to_owned),
            since_ms: WallMs::ZERO,
        }
    }

    fn overview() -> Overview {
        let workers = vec![
            WorkerInfo {
                worker: worker(1),
                name: "mac-studio".to_owned(),
                address: "100.64.0.3:45550".to_owned(),
                liveness: Liveness::Online,
                caps: caps("26.5"),
                load: 1.5,
                last_seen_ms: WallMs::from_millis(1_790_000_000_000),
            },
            WorkerInfo {
                worker: worker(2),
                name: "macbook-pro".to_owned(),
                address: "100.64.0.7:45550".to_owned(),
                liveness: Liveness::Unreachable,
                caps: caps("26.5.1"),
                load: 1.5,
                last_seen_ms: WallMs::from_millis(1_789_999_990_000),
            },
        ];
        let terminals = vec![
            (worker(1), summary(1, "zsh", "~/src/slopty", SessionState::Running)),
            (worker(1), summary(2, "claude", "~/src/slopty", SessionState::Running)),
            (
                worker(2),
                summary(3, "cargo test", "~/src/web", SessionState::Exited { status: 101 }),
            ),
        ];
        let agents = vec![
            (TermRef { worker: worker(1), session: session(1) }, at(1, Phase::Working, None)),
            (
                TermRef { worker: worker(1), session: session(2) },
                at(2, Phase::NeedsYou, Some("Run cargo test")),
            ),
            (
                TermRef { worker: worker(2), session: session(3) },
                at(3, Phase::NeedsYou, Some("Which branch?")),
            ),
        ];
        Overview { workers, terminals, agents, facts: Vec::new() }
    }

    #[test]
    fn workers_as_text() {
        insta::assert_snapshot!(overview().text());
    }

    #[test]
    fn workers_as_json() {
        insta::assert_json_snapshot!(overview().json());
    }

    #[test]
    fn terminals_as_text() {
        let o = overview();
        insta::assert_snapshot!(terminals_text(&o.workers, &o.terminals, &o.agents));
    }

    #[test]
    fn terminals_as_json() {
        let o = overview();
        insta::assert_json_snapshot!(terminals_json(&o.workers, &o.terminals, &o.agents));
    }

    #[test]
    fn nothing_to_list_reads_as_such() {
        assert_eq!(Overview::default().text(), "no workers have registered with the server\n");
        assert_eq!(terminals_text(&[], &[], &[]), "no terminals\n");
    }

    #[test]
    fn a_shared_name_falls_back_to_the_worker_id() {
        let mut o = overview();
        o.workers[1].name = "mac-studio".to_owned();
        let short = ShortTerms::new(&o.workers, &o.terminals);
        let t = TermRef { worker: worker(2), session: session(3) };
        assert!(short.get(t).starts_with(&worker(2).to_string()), "{}", short.get(t));
    }

    #[test]
    fn session_prefixes_grow_until_they_are_unique() {
        let o = overview();
        assert_eq!(ShortTerms::new(&o.workers, &o.terminals).prefix, MIN_PREFIX);
        let close = |id: &str| {
            let mut s = o.terminals[0].1.clone();
            s.id = id.parse().unwrap();
            (worker(1), s)
        };
        let same_second = [
            close("0199a1b2-c3d4-7000-8000-00000000abcd"),
            close("0199a1b2-c3d5-7000-8000-00000000abcd"),
        ];
        let short = ShortTerms::new(&o.workers, &same_second);
        assert_eq!(short.prefix, 13, "one past the twelve characters they share");
        let t = TermRef { worker: worker(1), session: same_second[1].1.id };
        assert_eq!(short.get(t), "mac-studio/0199a1b2-c3d5");
    }

    /// An agent reads as its thread's row says, alike in JSON and in text: its phase, and what
    /// it asks or waits on.
    #[test]
    fn an_agent_reads_the_same_in_json_and_text() {
        let asking = at(1, Phase::NeedsYou, Some("Which branch?"));
        let json = serde_json::to_value(agent(Some(&asking))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "agent": "claude-code", "thread": asking.thread, "status": "needs_you",
                "waits": null, "asks": "Which branch?", "needs_human": true
            })
        );
        assert_eq!(agent_text(Some(&asking)), "Claude Code  needs you: Which branch?");
        let paused = TermAgent {
            wait: Some(Wait { kind: Wait::TASK.to_owned(), text: "2 background tasks".to_owned() }),
            ..at(1, Phase::Waiting, None)
        };
        assert_eq!(agent_text(Some(&paused)), "Claude Code  waiting: 2 background tasks");
        let none = serde_json::to_value(agent(None)).unwrap();
        assert_eq!(none["status"], "none");
        assert_eq!(none["needs_human"], false);
        assert_eq!(agent_text(None), "no agent");
    }

    fn chunk(bytes: &[u8], offset: u64, size: u64) -> Chunk {
        Chunk { bytes: bytes.to_vec(), offset, size }
    }

    #[test]
    fn a_file_is_text_when_it_can_be_and_base64_otherwise() {
        let whole = chunk("héllo\n".as_bytes(), 0, 7);
        let text = serde_json::to_value(file("/tmp/a", &whole)).unwrap();
        assert_eq!(
            text,
            serde_json::json!({
                "path": "/tmp/a", "size": 7, "offset": 0, "length": 7, "more": false,
                "encoding": "utf8", "content": "héllo\n"
            })
        );
        let part = chunk(&[0xff, 0], 10, 40);
        let binary = serde_json::to_value(file("/tmp/b", &part)).unwrap();
        assert_eq!(binary["encoding"], "base64");
        assert_eq!(binary["content"], "/wA=");
        assert_eq!(binary["length"], 2, "the bytes read, not their spelling");
        assert_eq!((binary["size"].as_u64(), binary["more"].as_bool()), (Some(40), Some(true)));
    }

    #[test]
    fn a_directory_and_a_stat_read_as_views() {
        let entries = vec![
            DirEntry {
                name: "src".to_owned(),
                kind: FileKind::Dir,
                size: 96,
                modified_ms: WallMs::from_millis(5),
            },
            DirEntry {
                name: "a.rs".to_owned(),
                kind: FileKind::File,
                size: 12,
                modified_ms: WallMs::from_millis(6),
            },
        ];
        let json = serde_json::to_value(dir("/r", &entries, 3)).unwrap();
        assert_eq!(
            json["entries"][0],
            serde_json::json!({
                "name": "src", "kind": "dir", "size": 96, "modified_ms": 5
            })
        );
        assert_eq!((json["total"].as_u64(), json["truncated"].as_bool()), (Some(3), Some(true)));
        assert_eq!(dir_text(&entries, 3), "SIZE  NAME\n96    src/\n12    a.rs\n… 1 more\n");
        let found = FileStat {
            kind: FileKind::File,
            size: 4,
            modified_ms: WallMs::from_millis(9),
            mode: 0o644,
        };
        let json = serde_json::to_value(stat("/r/a", Some(&found))).unwrap();
        assert_eq!(json["mode"], "644");
        assert_eq!(json["exists"], true);
        let json = serde_json::to_value(stat("/r/b", None)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "path": "/r/b", "exists": false, "kind": null, "size": null,
                "modified_ms": null, "mode": null
            })
        );
    }

    #[test]
    fn events_carry_the_fields_of_their_kind() {
        let term = TermRef { worker: worker(1), session: session(1) };
        let page = EventPage {
            events: vec![
                HubEvent {
                    seq: 7,
                    at_ms: WallMs::from_millis(100),
                    what: Happening::Worker {
                        worker: worker(1),
                        name: "mac-studio".to_owned(),
                        liveness: Liveness::Unreachable,
                    },
                },
                HubEvent {
                    seq: 8,
                    at_ms: WallMs::from_millis(101),
                    what: Happening::Rung {
                        worker: term.worker,
                        terminal: Some(term.session),
                        agent: at(1, Phase::NeedsYou, Some("Which branch?")),
                    },
                },
            ],
            next: 9,
            missed: 0,
        };
        insta::assert_json_snapshot!(events(&page));
        let names = HashMap::from([(worker(1), "mac-studio".to_owned())]);
        assert_eq!(event_text(&page.events[0], &names), "     7  worker  mac-studio unreachable");
        assert_eq!(
            event_text(&page.events[1], &names),
            format!(
                "     8  agent   mac-studio/{}  Claude Code  needs you: Which branch?",
                session(1)
            )
        );
    }
}
