//! What the verbs answer, twice: JSON with stable field names for scripts and models (the same
//! shapes from `--json` and from `slopty mcp`), and text for a person.
//!
//! A terminal is always named by its `term`, `worker/session` with both ids in full, which
//! every verb accepts back. Text output shortens it to `name/prefix`, which verbs accept too.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;
use slopty_core::{SessionId, WorkerId};
use slopty_proto::agent::{AgentKind, AgentStatus, BlockReason};
use slopty_proto::orchestration::{Command, Line, Port, Screen, TermRef, Waited};
use slopty_proto::server::{Liveness, Os, WorkerInfo};
use slopty_proto::terminal::{SessionState, SessionSummary};

/// The shortest session-id prefix text output uses. `UUIDv7`s start with their creation time,
/// so ids minted close together share more than this and get longer prefixes.
const MIN_PREFIX: usize = 8;

/// A terminal's full handle.
pub fn term_string(term: TermRef) -> String {
    format!("{}/{}", term.worker, term.session)
}

/// Everything `slopty workers` shows: the directory, the terminals, and each agent's status.
#[derive(Debug, Default)]
pub struct Overview {
    /// The directory.
    pub workers: Vec<WorkerInfo>,
    /// Every terminal on every worker.
    pub terminals: Vec<(WorkerId, SessionSummary)>,
    /// The agent in a terminal, where one runs.
    pub agents: HashMap<TermRef, (AgentKind, AgentStatus)>,
}

/// A worker, for JSON.
#[derive(Debug, Serialize)]
pub struct WorkerView {
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
    last_seen_ms: u64,
    terminals: usize,
    waiting: Vec<WaitingView>,
}

/// An agent that needs a human, for JSON.
#[derive(Debug, Serialize)]
pub struct WaitingView {
    term: String,
    agent: &'static str,
    reason: &'static str,
    tool: Option<String>,
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
}

/// An agent's status, for JSON.
#[derive(Debug, Serialize)]
pub struct AgentView {
    agent: Option<&'static str>,
    status: &'static str,
    tool: Option<String>,
    reason: Option<&'static str>,
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

/// A file's contents, for JSON.
#[derive(Debug, Serialize)]
pub struct FileView<'a> {
    path: &'a str,
    size: usize,
    text: &'a str,
}

/// A new terminal, for JSON.
#[derive(Debug, Serialize)]
pub struct OpenedView {
    term: String,
    worker: WorkerId,
    session: SessionId,
}

/// Nothing to report, for JSON.
#[derive(Debug, Serialize)]
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

const fn agent_key(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "claude_code",
    }
}

const fn agent_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "Claude Code",
    }
}

const fn reason_key(reason: &BlockReason) -> &'static str {
    match reason {
        BlockReason::Permission { .. } => "permission",
        BlockReason::Question => "question",
        BlockReason::Elicitation => "elicitation",
        BlockReason::IdlePrompt => "idle_prompt",
    }
}

/// The reason an agent is blocked, when it is.
pub const fn blocked(status: &AgentStatus) -> Option<&BlockReason> {
    match status {
        AgentStatus::Blocked(reason) => Some(reason),
        _ => None,
    }
}

/// An agent's status, for JSON.
pub fn agent(agent: Option<&(AgentKind, AgentStatus)>) -> AgentView {
    let Some((kind, status)) = agent else {
        return AgentView {
            agent: None,
            status: "none",
            tool: None,
            reason: None,
            needs_human: false,
        };
    };
    let (status_key, tool, reason) = match status {
        AgentStatus::None => ("none", None, None),
        AgentStatus::Idle => ("idle", None, None),
        AgentStatus::Working => ("working", None, None),
        AgentStatus::Tool { tool } => ("tool", Some(tool.clone()), None),
        AgentStatus::Blocked(reason) => {
            let tool = match reason {
                BlockReason::Permission { tool } => Some(tool.clone()),
                BlockReason::Question | BlockReason::Elicitation | BlockReason::IdlePrompt => None,
            };
            ("blocked", tool, Some(reason_key(reason)))
        }
        AgentStatus::Done => ("done", None, None),
    };
    AgentView {
        agent: Some(agent_key(*kind)),
        status: status_key,
        tool,
        reason,
        needs_human: blocked(status).is_some(),
    }
}

/// An agent's status, for a person.
pub fn agent_text(agent: Option<&(AgentKind, AgentStatus)>) -> String {
    let Some((kind, status)) = agent else { return "no agent".to_owned() };
    let what = match status {
        AgentStatus::None => "not detected".to_owned(),
        AgentStatus::Idle => "idle".to_owned(),
        AgentStatus::Working => "working".to_owned(),
        AgentStatus::Tool { tool } => format!("running {tool}"),
        AgentStatus::Blocked(reason) => format!("waiting: {}", reason_text(reason)),
        AgentStatus::Done => "done".to_owned(),
    };
    format!("{}  {what}", agent_name(*kind))
}

fn reason_text(reason: &BlockReason) -> String {
    match reason {
        BlockReason::Permission { tool } => format!("permission for {tool}"),
        BlockReason::Question => "a question".to_owned(),
        BlockReason::Elicitation => "an answer to a form".to_owned(),
        BlockReason::IdlePrompt => "the next prompt".to_owned(),
    }
}

impl Overview {
    fn waiting(&self, worker: WorkerId) -> Vec<(TermRef, AgentKind, &BlockReason)> {
        let mut waiting: Vec<_> = self
            .terminals
            .iter()
            .filter(|(w, _)| *w == worker)
            .filter_map(|(w, s)| {
                let term = TermRef { worker: *w, session: s.id };
                let (kind, status) = self.agents.get(&term)?;
                blocked(status).map(|reason| (term, *kind, reason))
            })
            .collect();
        waiting.sort_by_key(|(term, ..)| term.session);
        waiting
    }

    fn terminal_count(&self, worker: WorkerId) -> usize {
        self.terminals.iter().filter(|(w, _)| *w == worker).count()
    }

    /// The workers, for JSON.
    pub fn json(&self) -> Vec<WorkerView> {
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
                load: w.caps.load,
                version: w.caps.version.clone(),
                can_capture: w.caps.can_capture,
                can_inject: w.caps.can_inject,
                last_seen_ms: w.last_seen_ms,
                terminals: self.terminal_count(w.worker),
                waiting: self
                    .waiting(w.worker)
                    .into_iter()
                    .map(|(term, kind, reason)| WaitingView {
                        term: term_string(term),
                        agent: agent_key(kind),
                        reason: reason_key(reason),
                        tool: match reason {
                            BlockReason::Permission { tool } => Some(tool.clone()),
                            _ => None,
                        },
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
            .map(|(term, kind, reason)| {
                let title = self
                    .terminals
                    .iter()
                    .find(|(_, s)| s.id == term.session)
                    .map(|(_, s)| s.title.clone())
                    .unwrap_or_default();
                vec![short.get(term), agent_name(kind).to_owned(), reason_text(reason), title]
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

/// Terminals, for JSON.
pub fn terminals_json(
    workers: &[WorkerInfo],
    terminals: &[(WorkerId, SessionSummary)],
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
            }
        })
        .collect()
}

/// Terminals, for a person.
pub fn terminals_text(workers: &[WorkerInfo], terminals: &[(WorkerId, SessionSummary)]) -> String {
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
            ]
        })
        .collect();
    table(&["TERM", "STATE", "SIZE", "VIEWERS", "TITLE", "CWD"], rows)
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

/// A file's text, for JSON.
pub const fn file<'a>(path: &'a str, text: &'a str) -> FileView<'a> {
    FileView { path, size: text.len(), text }
}

/// A new terminal, for JSON.
pub fn opened(term: TermRef) -> OpenedView {
    OpenedView { term: term_string(term), worker: term.worker, session: term.session }
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
    use slopty_proto::agent::AgentKind;
    use slopty_proto::server::WorkerCaps;
    use slopty_proto::terminal::SessionKind;

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
            cpus: 24,
            memory: 64 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            load: 1.5,
            version: "0.1.0".to_owned(),
        }
    }

    fn summary(n: u8, title: &str, cwd: &str, state: SessionState) -> SessionSummary {
        SessionSummary {
            id: session(n),
            kind: SessionKind::Terminal,
            title: title.to_owned(),
            cwd: Some(cwd.to_owned()),
            repo: None,
            cols: 120,
            rows: 40,
            state,
            viewers: 1,
            command: vec!["zsh".to_owned()],
        }
    }

    pub fn overview() -> Overview {
        let workers = vec![
            WorkerInfo {
                worker: worker(1),
                name: "mac-studio".to_owned(),
                address: "100.64.0.3:45550".to_owned(),
                liveness: Liveness::Online,
                caps: caps("26.5"),
                last_seen_ms: 1_790_000_000_000,
            },
            WorkerInfo {
                worker: worker(2),
                name: "macbook-pro".to_owned(),
                address: "100.64.0.7:45550".to_owned(),
                liveness: Liveness::Unreachable,
                caps: caps("26.5.1"),
                last_seen_ms: 1_789_999_990_000,
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
        let mut agents = HashMap::new();
        agents.insert(
            TermRef { worker: worker(1), session: session(2) },
            (
                AgentKind::ClaudeCode,
                AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            ),
        );
        agents.insert(
            TermRef { worker: worker(1), session: session(1) },
            (AgentKind::ClaudeCode, AgentStatus::Working),
        );
        Overview { workers, terminals, agents }
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
        insta::assert_snapshot!(terminals_text(&o.workers, &o.terminals));
    }

    #[test]
    fn terminals_as_json() {
        let o = overview();
        insta::assert_json_snapshot!(terminals_json(&o.workers, &o.terminals));
    }

    #[test]
    fn nothing_to_list_reads_as_such() {
        assert_eq!(Overview::default().text(), "no workers have registered with the server\n");
        assert_eq!(terminals_text(&[], &[]), "no terminals\n");
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

    #[test]
    fn an_agent_reads_the_same_in_json_and_text() {
        let blocked = (AgentKind::ClaudeCode, AgentStatus::Blocked(BlockReason::Question));
        let json = serde_json::to_value(agent(Some(&blocked))).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "agent": "claude_code", "status": "blocked", "tool": null,
                "reason": "question", "needs_human": true
            })
        );
        assert_eq!(agent_text(Some(&blocked)), "Claude Code  waiting: a question");
        let none = serde_json::to_value(agent(None)).unwrap();
        assert_eq!(none["status"], "none");
        assert_eq!(none["needs_human"], false);
        assert_eq!(agent_text(None), "no agent");
    }
}
