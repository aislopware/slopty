//! The verbs that drive workers, for people and for AI agents alike.
//!
//! One vocabulary, three surfaces (`docs/decisions/topology.md`): the server's MCP endpoint,
//! the `slopty` CLI and the `slopty mcp` stdio shim all speak these [`Verb`]s to the server,
//! which answers what it knows (the directory) and forwards the rest down the owning worker's
//! connection. Handles are explicit: a terminal is a [`TermRef`] (worker + session), never an
//! index into some earlier listing, so every call stands on its own.
//!
//! Reads come from the worker's terminal engine, never from raw PTY bytes: the rendered screen,
//! scrollback by absolute line index, and OSC 133 command blocks with their exit codes.
//!
//! The server answers [`Verb::Events`] and [`Verb::ForgetWorker`] itself, from its registry:
//! events are what it heard from every worker, numbered in one sequence, so one long poll
//! watches the whole fleet.

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WorkerId};

use crate::agent::{AgentKind, AgentStatus, SessionAgent};
use crate::server::{Liveness, WorkerInfo};
use crate::terminal::SessionSummary;

/// A terminal on a worker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TermRef {
    /// The worker that runs it.
    pub worker: WorkerId,
    /// The session on that worker.
    pub session: SessionId,
}

/// A terminal's grid in character cells.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Size {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

/// What to type into a terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Input {
    /// Text typed as-is (newlines press Enter).
    Text(String),
    /// Text delivered as a paste (bracketed when the program asked for it).
    Paste(String),
    /// Named keys, each `[mods+]key` with mods `ctrl`, `alt`, `shift`, `cmd` and a key name as
    /// W3C `KeyboardEvent.code` spells it or a single character: `enter`, `ctrl+c`, `up`,
    /// `shift+tab`, `escape`.
    Keys(Vec<String>),
}

/// What [`Verb::WaitFor`] waits for.
///
/// Waits read the way `expect` does, from a per-session mark rather than from when the call
/// arrives: a command often finishes before a wait sent after it reaches the worker. The mark
/// starts at a terminal's first byte when a verb opened it, else where the cursor stood before
/// the first [`Verb::SendInput`] or wait. A met wait moves the mark past what met it, so no
/// line or command ends two waits.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum WaitUntil {
    /// A line of output at or after the mark matches this regular expression.
    Output(String),
    /// No output for this many milliseconds.
    Quiet {
        /// Milliseconds of silence.
        ms: u32,
    },
    /// A command ends (OSC 133 `D`) at or after the mark, including one that already has.
    CommandDone,
    /// The session's program exits.
    Exit,
    /// The agent in the session reaches a state that needs a human or is idle.
    AgentNeedsInput,
}

/// A request to the server.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verb {
    /// Every worker the server knows, live or not.
    ListWorkers,
    /// The terminals on one worker, or on all of them.
    ListTerminals {
        /// Only this worker.
        worker: Option<WorkerId>,
    },
    /// Start a terminal; answered with [`Outcome::Opened`].
    OpenTerminal {
        /// Where.
        worker: WorkerId,
        /// Working directory; the worker's home when absent.
        cwd: Option<String>,
        /// Program and arguments; the user's login shell when empty.
        command: Vec<String>,
        /// Extra environment.
        env: Vec<(String, String)>,
        /// A name for the terminal's tile.
        name: Option<String>,
        /// The grid until a client shows it and sizes it to its window; 120×36 when absent.
        size: Option<Size>,
    },
    /// Start an agent's TUI in a new terminal, optionally with a first prompt.
    SpawnAgent {
        /// Where.
        worker: WorkerId,
        /// Which agent.
        agent: AgentKind,
        /// Working directory (usually a repository).
        cwd: String,
        /// The first prompt, typed once the agent is ready.
        prompt: Option<String>,
        /// Arguments after the agent's program.
        args: Vec<String>,
        /// Extra environment.
        env: Vec<(String, String)>,
        /// The grid, as for [`Verb::OpenTerminal`].
        size: Option<Size>,
    },
    /// Type into a terminal.
    SendInput {
        /// Which.
        term: TermRef,
        /// What.
        input: Input,
    },
    /// The screen as it is drawn now.
    ReadScreen {
        /// Which.
        term: TermRef,
    },
    /// Scrollback and screen lines from an absolute line index on.
    ReadOutput {
        /// Which.
        term: TermRef,
        /// First line wanted; the oldest retained when absent.
        since: Option<u64>,
        /// At most this many lines.
        max_lines: u32,
    },
    /// Finished and running commands (OSC 133 blocks), oldest first.
    ListCommands {
        /// Which.
        term: TermRef,
        /// Only blocks that start at or after this absolute line.
        since: Option<u64>,
    },
    /// Block until a condition holds or the timeout passes; answered with [`Outcome::Waited`].
    WaitFor {
        /// Which.
        term: TermRef,
        /// The condition.
        until: WaitUntil,
        /// Give up after this long. The server caps it below the MCP client's idle abort.
        timeout_ms: u32,
    },
    /// The agent status of a terminal.
    AgentStatus {
        /// Which.
        term: TermRef,
    },
    /// Close a terminal (hang up its program).
    Close {
        /// Which.
        term: TermRef,
    },
    /// Read a file on a worker, whole or a range of it; answered with [`Outcome::File`].
    ReadFile {
        /// Where.
        worker: WorkerId,
        /// Absolute path, or `~/…`.
        path: String,
        /// First byte wanted.
        offset: u64,
        /// At most this many bytes, capped by the worker. The rest of the file when absent,
        /// which fails for a rest over the cap.
        length: Option<u64>,
    },
    /// Write a file on a worker, replacing it.
    WriteFile {
        /// Where.
        worker: WorkerId,
        /// Absolute path, or `~/…`.
        path: String,
        /// New contents.
        bytes: Vec<u8>,
    },
    /// TCP ports listening in a worker's terminals' process trees.
    ListPorts {
        /// Where.
        worker: WorkerId,
    },
    /// Resize a terminal no client shows (a client showing one sizes it to its window).
    ResizeTerminal {
        /// Which.
        term: TermRef,
        /// The new grid.
        size: Size,
    },
    /// A directory's entries by name; answered with [`Outcome::Dir`].
    ListDir {
        /// Where.
        worker: WorkerId,
        /// Absolute path, or `~/…`.
        path: String,
        /// At most this many entries, capped by the worker.
        max: u32,
    },
    /// What is at a path; answered with [`Outcome::Stat`].
    Stat {
        /// Where.
        worker: WorkerId,
        /// Absolute path, or `~/…`; a symbolic link is followed.
        path: String,
    },
    /// What happened on the fleet from cursor `since` on, waiting up to `timeout_ms` when
    /// nothing has yet; answered by the server with [`Outcome::Events`].
    Events {
        /// The first sequence number wanted: the `next` of the previous answer. From now on
        /// when absent. A cursor ahead of the server's (it restarted) reads from its oldest.
        since: Option<u64>,
        /// Wait this long for a first event that passes `filter`; the server caps it as it
        /// caps [`Verb::WaitFor`].
        timeout_ms: u32,
        /// Which events count.
        filter: EventFilter,
    },
    /// Remove a worker that is not online from the server's registry.
    ForgetWorker {
        /// Which.
        worker: WorkerId,
    },
}

/// Which [`HubEvent`]s a [`Verb::Events`] returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum EventFilter {
    /// Every event.
    #[default]
    All,
    /// An agent's move to a state that needs a human or is idle, as
    /// [`WaitUntil::AgentNeedsInput`] reads it, on any worker.
    AgentNeedsInput,
}

impl EventFilter {
    /// Whether `what` passes.
    #[must_use]
    pub const fn admits(self, what: &Happening) -> bool {
        match self {
            Self::All => true,
            Self::AgentNeedsInput => matches!(
                what,
                Happening::Agent {
                    status: AgentStatus::Blocked(_) | AgentStatus::Idle | AgentStatus::Done,
                    ..
                }
            ),
        }
    }
}

/// Something the server heard, numbered in the order it heard it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HubEvent {
    /// Its place in the server's one sequence. Each run starts it at the run's start time in
    /// microseconds, so a cursor from an earlier run never falls inside the new one's range.
    pub seq: u64,
    /// Milliseconds since the Unix epoch when the server heard it.
    pub at_ms: u64,
    /// What.
    pub what: Happening,
}

/// What a [`HubEvent`] reports.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Happening {
    /// A worker came online, turned unreachable or was presumed gone.
    Worker {
        /// Which.
        worker: WorkerId,
        /// Its name.
        name: String,
        /// Its new liveness.
        liveness: Liveness,
    },
    /// A worker left the registry: forgotten, or replaced by the same machine set up again.
    WorkerRemoved {
        /// Which.
        worker: WorkerId,
        /// Its name.
        name: String,
    },
    /// A terminal opened.
    SessionOpened {
        /// Where.
        worker: WorkerId,
        /// What.
        summary: SessionSummary,
    },
    /// A terminal ended.
    SessionClosed {
        /// Which.
        term: TermRef,
    },
    /// An agent's status changed.
    Agent {
        /// Where.
        term: TermRef,
        /// Which agent.
        kind: AgentKind,
        /// Its new status; `None` when it left.
        status: AgentStatus,
        /// What it says, for a human.
        detail: Option<String>,
    },
}

/// What kind of thing is at a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FileKind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link (in a listing; [`Verb::Stat`] follows links).
    Symlink,
    /// A pipe, socket or device.
    Other,
}

/// One entry of a directory.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DirEntry {
    /// Its name in the directory.
    pub name: String,
    /// What it is, the link itself for a symbolic link.
    pub kind: FileKind,
    /// Bytes.
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch.
    pub modified_ms: u64,
}

/// What is at a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileStat {
    /// What it is.
    pub kind: FileKind,
    /// Bytes.
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch.
    pub modified_ms: u64,
    /// Permission bits (`0o755`).
    pub mode: u32,
}

/// One line of terminal text.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Line {
    /// Absolute line index (stable across scrollback eviction).
    pub index: u64,
    /// The text, trailing blanks trimmed.
    pub text: String,
}

/// A terminal's screen.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Screen {
    /// Rows top to bottom.
    pub lines: Vec<Line>,
    /// Cursor row (0 = top of the screen) and column.
    pub cursor: (u16, u16),
    /// Title (OSC 0/2).
    pub title: String,
    /// Working directory (OSC 7).
    pub cwd: Option<String>,
    /// The alternate screen is on (a full-screen program runs).
    pub alternate: bool,
}

/// One command block (OSC 133).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Command {
    /// The command line as typed.
    pub line: String,
    /// Absolute line of its prompt.
    pub prompt_line: u64,
    /// Absolute lines of its output, `[start, end)`.
    pub output: (u64, u64),
    /// Exit code; `None` while it runs.
    pub exit: Option<i32>,
}

/// How a [`Verb::WaitFor`] ended.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Waited {
    /// The condition held; the line that matched, for `Output`.
    Met {
        /// The matching line, when the condition was an output pattern.
        line: Option<Line>,
    },
    /// The timeout passed first.
    TimedOut,
    /// The session ended first.
    Closed,
}

/// A listening TCP port.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Port {
    /// Port number.
    pub number: u16,
    /// The listening process id.
    pub pid: u32,
    /// Its command name.
    pub process: String,
    /// The terminal whose process tree holds it.
    pub session: Option<SessionId>,
}

/// Why a verb failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ErrorCode {
    /// No such worker.
    UnknownWorker,
    /// The worker is known but not connected.
    WorkerUnreachable,
    /// No such terminal.
    UnknownTerminal,
    /// A malformed argument (a bad key name, a bad pattern).
    Invalid,
    /// The worker could not do it (a file error, a spawn failure).
    Failed,
    /// The caller lost its link to the server before the answer came.
    ServerUnreachable,
}

/// The answer to a [`Verb`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Outcome {
    /// For [`Verb::ListWorkers`].
    Workers(Vec<WorkerInfo>),
    /// For [`Verb::ListTerminals`].
    Terminals(Vec<(WorkerId, SessionSummary)>),
    /// For [`Verb::OpenTerminal`] and [`Verb::SpawnAgent`].
    Opened(TermRef),
    /// For [`Verb::ReadScreen`].
    Screen(Screen),
    /// For [`Verb::ReadOutput`]: the lines, and the index to ask from next.
    Output {
        /// Lines in order.
        lines: Vec<Line>,
        /// One past the last line returned.
        next: u64,
    },
    /// For [`Verb::ListCommands`].
    Commands(Vec<Command>),
    /// For [`Verb::WaitFor`].
    Waited(Waited),
    /// For [`Verb::AgentStatus`]: the agent, if one runs, its status and where that came from.
    Agent(Option<SessionAgent>),
    /// For [`Verb::ReadFile`]: the bytes from `offset` on, and the file's whole size.
    File {
        /// What was read.
        bytes: Vec<u8>,
        /// Where they start in the file.
        offset: u64,
        /// The file's size in bytes.
        size: u64,
    },
    /// For [`Verb::ListPorts`].
    Ports(Vec<Port>),
    /// Done, nothing to report ([`Verb::SendInput`], [`Verb::Close`], [`Verb::WriteFile`],
    /// [`Verb::ResizeTerminal`], [`Verb::ForgetWorker`]).
    Done,
    /// It failed.
    Error {
        /// Why.
        code: ErrorCode,
        /// For a human or a model to read.
        message: String,
    },
    /// For [`Verb::ListDir`]: entries by name, and how many the directory holds.
    Dir {
        /// The first `max` entries by name.
        entries: Vec<DirEntry>,
        /// Every entry the directory holds.
        total: u32,
    },
    /// For [`Verb::Stat`]; `None` when nothing is at the path.
    Stat(Option<FileStat>),
    /// For [`Verb::Events`].
    Events {
        /// Oldest first.
        events: Vec<HubEvent>,
        /// The cursor to ask from next.
        next: u64,
        /// Events after `since` the server no longer holds.
        missed: u64,
    },
}
