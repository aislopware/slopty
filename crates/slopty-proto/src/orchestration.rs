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
//! The server answers [`Verb::Events`], [`Verb::ForgetWorker`] and [`Verb::Wake`] itself, from
//! its registry: events are what it heard from every worker, numbered in one sequence, so one
//! long poll watches the whole fleet, and a sleeping worker is woken by whichever machine it
//! knows on the same LAN.
//!
//! A verb that [changes](Verb::changes) something may carry an [`IdempotencyKey`], so a caller
//! whose answer was lost sends it again without doing it twice.
//!
//! Any agent's thread is read through the agent-neutral thread model ([`Verb::ReadThread`]), a
//! bounded page of whole turns at a time, and its open requests are answered as a client's
//! would be ([`Verb::AnswerRequest`]). A file too large for one frame goes up in parts
//! ([`Verb::Upload`]) and comes down in [`Verb::ReadFile`] ranges.
//!
//! Agents start agents ([`Verb::AgentStart`]) on the machine they name, tell them things, wait
//! for them and read them, at any depth: the server records who started whom, and a child coming
//! to rest is news to its parent, as a message of its own (`docs/decisions/projects.md`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId, XferId};

use crate::folder::FsOp;
use crate::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use crate::search::{FileHits, SearchQuery, SearchSummary};
use crate::server::{Liveness, WorkerFacts, WorkerInfo};
use crate::terminal::SessionSummary;
use crate::thread::attention::Rung;
use crate::thread::wire::ThreadRow;
use crate::thread::{AgentId, AskId, Choice, Phase, ThreadId, ToolState, TurnId, TurnState, Wait};
use crate::transfer::Hash;

/// How long after its answer a key is still honoured: longer than any caller keeps retrying
/// one call (a `WaitFor` runs at most four minutes), short enough that the tables stay small.
pub const KEY_LIFETIME: Duration = Duration::from_mins(10);

/// The caller's name for the effect of one verb, a UUID or any 1 to
/// [`IdempotencyKey::MAX_LEN`] bytes of printable ASCII.
///
/// The component that does the verb (the worker, or the server for [`Verb::ForgetWorker`])
/// answers a repeat of it under the same key for [`KEY_LIFETIME`] with the first answer, and
/// does nothing again: a repeat that arrives while the first still runs waits for its answer.
/// The same key with other arguments is [`ErrorCode::Invalid`]. A verb that only reads ignores
/// the key and is answered afresh.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);

/// A string that is not an [`IdempotencyKey`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("an idempotency key is 1 to 128 printable ASCII characters")]
pub struct BadKey;

impl IdempotencyKey {
    /// The longest key, in bytes.
    pub const MAX_LEN: usize = 128;

    /// `key`, if it is 1 to [`Self::MAX_LEN`] bytes of printable ASCII.
    ///
    /// # Errors
    /// [`BadKey`] otherwise.
    pub fn new(key: impl Into<String>) -> Result<Self, BadKey> {
        let key = key.into();
        let fits = (1..=Self::MAX_LEN).contains(&key.len());
        if fits && key.bytes().all(|b| b.is_ascii_graphic()) { Ok(Self(key)) } else { Err(BadKey) }
    }

    /// A key spelled as the 32 hex digits of `id`: a fresh UUID's bits make a fresh key.
    #[must_use]
    pub fn from_id(id: u128) -> Self {
        Self(format!("{id:032x}"))
    }

    /// The key as the caller spelled it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The key of the step `name` of a call made under this key: the same for every retry of
    /// the call, and another for each step, so a call that clones and then starts an agent
    /// starts one however often it is sent.
    #[must_use]
    pub fn part(&self, name: &str) -> Self {
        let whole = format!("{name}.{}", self.0);
        if whole.len() <= Self::MAX_LEN && whole.bytes().all(|b| b.is_ascii_graphic()) {
            return Self(whole);
        }
        // FNV-1a over the whole key keeps long keys apart once cut to fit.
        let hash = whole.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        let named: String = name.chars().filter(char::is_ascii_graphic).take(32).collect();
        let mut short = format!("{named}.{hash:016x}.");
        let room = Self::MAX_LEN.saturating_sub(short.len());
        short.extend(self.0.chars().take(room));
        Self(short)
    }

    /// The answer to a verb whose key an earlier verb with other arguments holds.
    #[must_use]
    pub fn reused(&self) -> Outcome {
        Outcome::Error {
            code: ErrorCode::Invalid,
            message: format!(
                "idempotency key {:?} was used with other arguments; a new call takes a new key",
                self.0
            ),
        }
    }
}

impl TryFrom<String> for IdempotencyKey {
    type Error = BadKey;

    fn try_from(key: String) -> Result<Self, BadKey> {
        Self::new(key)
    }
}

impl From<IdempotencyKey> for String {
    fn from(key: IdempotencyKey) -> Self {
        key.0
    }
}

impl std::str::FromStr for IdempotencyKey {
    type Err = BadKey;

    fn from_str(s: &str) -> Result<Self, BadKey> {
        Self::new(s)
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The agent at work in a terminal, as its thread's row says: what a listing and a status
/// query answer, and what [`Happening::Rung`] carries.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TermAgent {
    /// Its thread.
    pub thread: ThreadId,
    /// Which agent.
    pub agent: AgentId,
    /// The thread's title.
    pub title: String,
    /// Where it stands among the fleet's threads.
    pub rung: Rung,
    /// Its phase.
    pub phase: Phase,
    /// What it waits on, while it waits.
    pub wait: Option<Wait>,
    /// What it asks the person: its first open request's title.
    pub asks: Option<String>,
    /// When it entered its phase, by its worker's clock.
    pub since_ms: WallMs,
}

impl TermAgent {
    /// Where `row`'s thread stands.
    #[must_use]
    pub fn of(row: &ThreadRow) -> Self {
        Self {
            thread: row.id,
            agent: row.agent.clone(),
            title: row.title.clone(),
            rung: Rung::of(row),
            phase: row.status.phase,
            wait: row.status.wait.clone(),
            asks: row.requests.first().map(|r| r.title.clone()),
            since_ms: row.status.since_ms,
        }
    }

    /// Whether it waits on the person or rests: what [`EventFilter::AgentNeedsInput`] keeps.
    #[must_use]
    pub const fn needs_input(&self) -> bool {
        matches!(self.rung, Rung::NeedsYou)
            || matches!(self.phase, Phase::Idle | Phase::Done | Phase::Failed | Phase::Stopped)
    }
}

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
        /// The id the terminal takes, when the caller chooses it: a start whose answer was lost
        /// is found by it, and a start again under an id the worker already runs answers that
        /// terminal instead of opening a second.
        session: Option<SessionId>,
        /// A git worktree the worker makes, or reopens as it is, from the clone `cwd` is in at
        /// its base fetched current ([`crate::thread::wire::NewWorktree`]); the terminal opens
        /// in it, where `cwd` stood in the clone.
        worktree: Option<crate::thread::wire::NewWorktree>,
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
    /// Where the agent at work in a terminal stands, as its thread's row says.
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
    /// The windows and displays a worker can stream; answered with [`Outcome::Screens`].
    ListWindows {
        /// Where.
        worker: WorkerId,
    },
    /// What a thread did after turn `after`, read through the thread model whatever its agent;
    /// answered with [`Outcome::Thread`].
    ///
    /// A read is bounded ([`ThreadRead::truncated`]) and ends on a whole turn, so reading again
    /// from [`ThreadRead::next`] goes on where it stopped; a turn still under way is read again
    /// until it ends. The thread's open requests come with it, answered with
    /// [`Verb::AnswerRequest`].
    ReadThread {
        /// Which thread; the server finds where it is and sends [`ThreadOf::On`].
        of: ThreadOf,
        /// How much of each turn.
        view: ThreadView,
        /// The last turn already read; from the first held when absent.
        after: Option<TurnId>,
    },
    /// Answer a thread's open request ([`ThreadRead::requests`]) as a client's would be: by
    /// one of the choices it offers. The person's alone; an agent is
    /// [`ErrorCode::Forbidden`].
    AnswerRequest {
        /// Which thread; the server finds where it is and sends [`ThreadOf::On`].
        of: ThreadOf,
        /// The request.
        ask: AskId,
        /// The choice taken ([`Choice::id`]), or the answers to its questions.
        choice: String,
        /// Words to go with it, where the agent takes them.
        message: Option<String>,
    },
    /// One still picture of a window or a display; answered with [`Outcome::Still`], or
    /// [`ErrorCode::Unsupported`] by a worker that may not capture its screen.
    CaptureStill {
        /// Where.
        worker: WorkerId,
        /// What, by an id from [`Verb::ListWindows`].
        target: CaptureTarget,
    },
    /// One step of a file sent up in parts, of any size. The parts land beside the file under the
    /// upload's name, and only a finished upload that adds up replaces it.
    Upload {
        /// Where.
        worker: WorkerId,
        /// Absolute path, or `~/…`.
        path: String,
        /// The caller's name for this upload; a retried upload under the same name writes into
        /// the same parts.
        upload: XferId,
        /// What this step does.
        part: UploadPart,
    },
    /// Wake a worker that sleeps: the server, or an online worker on the same subnet, sends
    /// the magic packet to its LAN ports ([`crate::server::WorkerCaps::lan`]). Answered by the
    /// server with [`Outcome::WakeSent`]; the worker registering again is the directory's news.
    Wake {
        /// Which.
        worker: WorkerId,
    },
    /// For a worker, from the server: send the magic packet for each of `peer`'s ports from
    /// this worker's port on the same subnet. Answered with [`Outcome::Done`].
    WakePeer {
        /// The worker that sends.
        worker: WorkerId,
        /// The sleeping worker's ports that this one shares a subnet with.
        peer: Vec<crate::lan::LanPort>,
    },
    /// The lines matching a query in the files under a directory, as search in files finds
    /// them; answered with [`Outcome::Search`].
    Search {
        /// Where.
        worker: WorkerId,
        /// An absolute directory, or `~/…`.
        root: String,
        /// What to look for.
        query: SearchQuery,
        /// At most this many matching lines, capped by the worker at
        /// [`crate::search::MAX_LINES`].
        max_lines: u32,
    },
    /// What the workers are and have; answered with [`Outcome::Facts`].
    ///
    /// The server forwards it to each worker it has not asked yet on its registration, naming
    /// that worker: a worker gathers its own facts only once asked.
    WorkerFacts {
        /// This worker only; every one when absent.
        worker: Option<WorkerId>,
    },
    /// Clone a repository onto a worker from `url`, with the worker's own git credentials,
    /// into the place it keeps its clones; one there already is answered as it is. How it goes
    /// comes as [`crate::server::ToServer::Cloning`]. Answered with [`Outcome::Cloned`].
    CloneRepo {
        /// Where.
        worker: WorkerId,
        /// What to clone: a remote's address, never a path on the worker's own disk.
        url: String,
        /// The server's number for it, which its progress names.
        clone: u64,
    },
    /// Put a branch's commits beyond where it left `target` in a git bundle on its worker, for
    /// another worker to fetch. Answered with [`Outcome::Bundle`].
    BundleBranch {
        /// Where.
        worker: WorkerId,
        /// The clone or worktree the branch is in.
        repo: String,
        /// The branch.
        branch: String,
        /// The branch work lands on, whose fork point the bundle starts after; the whole
        /// branch when `None`.
        target: Option<String>,
    },
    /// Fetch a branch from a bundle put in the worker's bundle place into a repository on it,
    /// as the branch `into`; the bundle goes after. A repository that lacks the commit the
    /// bundle starts after fetches its own `origin` once first. Answered with
    /// [`Outcome::Fetched`].
    FetchBundle {
        /// Where.
        worker: WorkerId,
        /// The repository to fetch into.
        repo: String,
        /// The bundle's name in the worker's bundle place.
        bundle: String,
        /// The branch it holds.
        branch: String,
        /// The branch it lands as, which is set to it whatever it held.
        into: String,
        /// The commit the branch is at, which the bundle must hold.
        head: String,
    },
    /// Start an agent as a thread of the worker's thread host, seated at `seat`: its row
    /// carries [`crate::agent::SEAT_FACT`] and `facts`, its Slopty tools speak as `seat` with
    /// the worker's token for it, and `system` reaches it through its agent's own door (a
    /// system prompt where it takes one, else ahead of its first prompt). An agent whose
    /// thread runs in a terminal runs in one opened under `seat`. A start that names a
    /// worktree ([`crate::thread::wire::Start::worktree`]) works in it, made or reopened, and
    /// the answer says where. A repeat with a `seat` already started answers that thread.
    /// Answered with [`Outcome::ThreadStarted`]; [`ErrorCode::Unsupported`] for an agent the
    /// worker cannot start. The server sends it for [`Verb::AgentStart`].
    StartThread {
        /// Where.
        worker: WorkerId,
        /// What to start.
        start: Box<crate::thread::wire::Start>,
        /// The id it is known by.
        seat: SessionId,
        /// Open facts its row carries from the start ([`crate::agent::PARENT_FACT`]).
        facts: Vec<(String, String)>,
        /// Extra system text, where its agent takes some.
        system: Option<String>,
    },
    /// Make a folder, move or rename an entry, or put one in the OS's trash, as a folder tile
    /// does ([`FsOp`]): nothing is replaced and nothing unlinked. Answered with
    /// [`Outcome::FsDone`], or an error that says plainly why it was refused or failed.
    FsChange {
        /// Where.
        worker: WorkerId,
        /// What to do.
        op: FsOp,
    },
    /// Read a daemon's settings file, after making `edits` to it: the keys a worker or the
    /// server reads itself (`[worker]`, `[server]`), which a settings page on another device
    /// edits for it. Edits go in order, as the page writes them on its own device, and only to
    /// the daemon's own tables; the daemon takes the file up again as it does after a hand
    /// edit. Answered with [`Outcome::Settings`]; [`ErrorCode::Invalid`] for an edit outside
    /// its tables or one that leaves the file unreadable, [`ErrorCode::Forbidden`] for an
    /// agent.
    Settings {
        /// Whose: a worker's, or the server's own when `None`.
        of: Option<WorkerId>,
        /// What to change first; none only reads.
        edits: Vec<crate::settings::SettingEdit>,
    },
    /// Send a thread a message as its composer would, to go when the agent next takes one
    /// (what a phone's note replies with, with no window open). The person's alone; an agent
    /// is [`ErrorCode::Forbidden`] (an agent speaks to another with [`Verb::AgentTell`]).
    /// Answered with [`Outcome::Done`] once the thread took it.
    SendMessage {
        /// Which thread; the server finds where it is and sends [`ThreadOf::On`].
        of: ThreadOf,
        /// What it says.
        text: String,
    },
    /// Have a worker's daemon exit for its service manager to start it again (launchd's
    /// `KeepAlive`, systemd's `Restart=always`), so a grant made at its desk (Screen Recording)
    /// is seen. Its terminals stay with ptyd and come back; its agents' turns that run in the
    /// worker itself end as an update ends them. The person's alone; an agent is
    /// [`ErrorCode::Forbidden`]. Answered with [`Outcome::Done`] just before it exits.
    RestartWorker {
        /// Which.
        worker: WorkerId,
    },
    /// Start an agent on a machine (`docs/decisions/projects.md`). The server builds what
    /// its harness is started with from the person's autonomy setting, gives it Slopty's
    /// tools, and records the caller's agent as its parent. Answered with
    /// [`Outcome::ThreadStarted`] once it runs.
    AgentStart(Box<crate::agent::AgentStart>),
    /// Words for an agent, marked as the caller's: they reach it as a message through its
    /// thread, never typed into a terminal. Answered with [`Outcome::Done`].
    AgentTell {
        /// Which agent.
        agent: ThreadId,
        /// What to say, at most [`crate::agent::TELL_MAX`] bytes.
        text: String,
    },
    /// Wait until one, or each, of `agents` needs someone or comes to rest, for at most
    /// `timeout_ms` (capped as [`Verb::WaitFor`] is). Answered with [`Outcome::Agents`], every
    /// agent named as it stands then.
    AgentWait {
        /// Which, at most [`crate::agent::WAIT_MAX`].
        agents: Vec<ThreadId>,
        /// Wait for every one; the first one when `false`.
        all: bool,
        /// How long, in milliseconds.
        timeout_ms: u32,
    },
    /// End an agent, its conversation kept. Answered with [`Outcome::Done`].
    AgentStop {
        /// Which agent.
        agent: ThreadId,
    },
    /// Every agent on the fleet, each with its parent, so they read as a tree. Answered with
    /// [`Outcome::Agents`].
    Agents,
    /// Bring a branch from a clone on one machine into a clone on another, as a git bundle
    /// through the server. Answered with [`Outcome::Fetched`].
    BranchBring {
        /// The machine the branch is on.
        from: WorkerId,
        /// The clone or worktree it is in there.
        repo: String,
        /// The branch.
        branch: String,
        /// The machine it goes to.
        to: WorkerId,
        /// The clone it lands in there; the clone of the same repository there when absent.
        into: Option<String>,
        /// The name it lands as; its own when absent.
        name: Option<String>,
    },
}

/// Where a worker keeps the git bundles it makes and is sent ([`Verb::BundleBranch`],
/// [`Verb::FetchBundle`]): an upload there is a bundle to fetch. One left an hour is swept.
pub const BUNDLES: &str = "~/.cache/slopty/bundles";

/// A branch put in a git bundle on its worker ([`Verb::BundleBranch`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BranchBundle {
    /// Where it is on the worker.
    pub path: String,
    /// Its name, for [`Verb::FetchBundle`] once it is in another worker's [`BUNDLES`].
    pub name: String,
    /// Its size.
    pub size: u64,
    /// Its BLAKE3 digest.
    pub digest: Hash,
    /// The commit the branch is at.
    pub head: String,
    /// The commit it starts after, when it holds only the branch's own.
    pub base: Option<String>,
}

/// One step of a [`Verb::Upload`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum UploadPart {
    /// These bytes at `offset` of the upload. Parts may come in any order, and a part sent
    /// again writes the same bytes again.
    Bytes {
        /// Where they go in the file.
        offset: u64,
        /// At most the worker's cap on one read.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    /// Every part is sent: the upload holds `size` bytes whose BLAKE3 digest is `digest`, and
    /// replaces the file at the path. One that does not add up is dropped.
    Finish {
        /// The file's size.
        size: u64,
        /// Its BLAKE3 digest.
        digest: Hash,
        /// Permission bits for a new file, as `scp` gives them: a file replaced keeps its own.
        mode: Option<u32>,
    },
    /// Drop what the upload holds.
    Abort,
}

impl Verb {
    /// Whether doing it twice differs from doing it once, so a repeat under the same
    /// [`IdempotencyKey`] answers the first outcome. A [`Verb::WaitFor`] counts: a met wait
    /// moves the session's mark past what met it, and a repeat would wait for the next match.
    #[must_use]
    pub const fn changes(&self) -> bool {
        match self {
            Self::OpenTerminal { .. }
            | Self::SendInput { .. }
            | Self::WaitFor { .. }
            | Self::Close { .. }
            | Self::ResizeTerminal { .. }
            | Self::ForgetWorker { .. }
            | Self::AnswerRequest { .. }
            | Self::SendMessage { .. }
            | Self::RestartWorker { .. }
            | Self::BundleBranch { .. }
            | Self::FetchBundle { .. }
            | Self::StartThread { .. }
            | Self::AgentStart(..)
            | Self::AgentTell { .. }
            | Self::AgentStop { .. }
            | Self::BranchBring { .. }
            | Self::FsChange { .. } => true,
            // Reading the file again is the same; an edit writes it.
            Self::Settings { edits, .. } => !edits.is_empty(),
            // A part rewrites the same bytes and an abort finds nothing the second time; only
            // the finish replaces the file.
            Self::Upload { part, .. } => matches!(part, UploadPart::Finish { .. }),
            // A clone there already is answered as it is.
            Self::CloneRepo { .. }
            | Self::ListWorkers
            | Self::ListTerminals { .. }
            | Self::ReadScreen { .. }
            | Self::ReadOutput { .. }
            | Self::ListCommands { .. }
            | Self::AgentStatus { .. }
            | Self::ReadFile { .. }
            | Self::ListPorts { .. }
            | Self::ListDir { .. }
            | Self::Stat { .. }
            | Self::Search { .. }
            | Self::Events { .. }
            | Self::ListWindows { .. }
            | Self::ReadThread { .. }
            | Self::CaptureStill { .. }
            | Self::WorkerFacts { .. }
            | Self::AgentWait { .. }
            | Self::Agents
            // A second magic packet wakes nothing that the first did not.
            | Self::Wake { .. }
            | Self::WakePeer { .. } => false,
        }
    }
}

/// Which [`HubEvent`]s a [`Verb::Events`] returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum EventFilter {
    /// Every event.
    #[default]
    All,
    /// A driven agent's move to a state that needs a human or is idle, on any worker.
    AgentNeedsInput,
}

impl EventFilter {
    /// Whether `what` passes.
    #[must_use]
    pub const fn admits(self, what: &Happening) -> bool {
        match self {
            Self::All => true,
            Self::AgentNeedsInput => {
                matches!(what, Happening::Rung { agent, .. } if agent.needs_input())
            }
        }
    }
}

/// Something the server heard, numbered in the order it heard it: what [`Verb::Events`]
/// returns and what a client or agent link is pushed as it happens, one log for both.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HubEvent {
    /// Its place in the server's one sequence. Each run starts it at the run's start time in
    /// microseconds, so a cursor from an earlier run never falls inside the new one's range.
    pub seq: u64,
    /// When the server heard it.
    pub at_ms: WallMs,
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
        /// What; boxed, being the largest.
        summary: Box<SessionSummary>,
    },
    /// A terminal ended.
    SessionClosed {
        /// Which.
        term: TermRef,
    },
    /// A thread that hangs from no other moved on the ladder, or what it asks changed: its
    /// phase, its rung, or its first open request, as its worker's table says.
    Rung {
        /// Where.
        worker: WorkerId,
        /// The terminal it is seated at, when it is.
        terminal: Option<SessionId>,
        /// Where it stands now.
        agent: TermAgent,
    },
    /// A terminal's program exited; the terminal stays, its last screen readable, until it is
    /// closed.
    SessionExited {
        /// Which.
        term: TermRef,
        /// Exit status, or the signal number negated; none when its worker did not see it end.
        status: Option<i32>,
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
    /// Last modification.
    pub modified_ms: WallMs,
}

/// What is at a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileStat {
    /// What it is.
    pub kind: FileKind,
    /// Bytes.
    pub size: u64,
    /// Last modification.
    pub modified_ms: WallMs,
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
    /// No such agent.
    UnknownAgent,
    /// A malformed argument (a bad key name, a bad pattern).
    Invalid,
    /// The worker could not do it (a file error, a spawn failure).
    Failed,
    /// The caller lost its link to the server before the answer came.
    ServerUnreachable,
    /// A link dropped after the verb went out and before its answer came back: it may have
    /// been done. Sent again under the same [`IdempotencyKey`], it is not done twice.
    Interrupted,
    /// The worker cannot do this at all: it may not capture its screen, or has none.
    Unsupported,
    /// It would take what another holds: a branch moved on, a session another writes.
    Conflict,
    /// The caller may not do this: an agent answering a permission, changing what only the
    /// person changes, or typing into a terminal a coding agent runs in.
    Forbidden,
    /// There is nothing new to carry: the branch has no commit beyond the one it would start
    /// after, so the other side has it from the same forge.
    NothingNew,
    /// The server runs another build than the caller, which this build cannot talk to: one of
    /// them is to be updated. A caller's own answer, never the server's.
    OtherBuild,
}

/// The answer to a [`Verb`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Outcome {
    /// For [`Verb::ListWorkers`].
    Workers(Vec<WorkerInfo>),
    /// For [`Verb::ListTerminals`]: the terminals, and the agent at work in each that has one.
    Terminals {
        /// The terminals, by worker.
        terminals: Vec<(WorkerId, SessionSummary)>,
        /// The agent in each terminal that has one, as its thread's row says.
        agents: Vec<(TermRef, TermAgent)>,
    },
    /// For [`Verb::OpenTerminal`].
    Opened(TermRef),
    /// For [`Verb::OpenTerminal`] that named a worktree: the terminal,
    /// and the worktree it opened in, made or reopened, so whoever started it can free it.
    OpenedIn {
        /// The terminal.
        term: TermRef,
        /// The worktree.
        worktree: Box<crate::agent::Worktree>,
    },
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
    /// For [`Verb::AgentStatus`]: the agent at work in the terminal, if one is. Boxed: it is
    /// several times the size of every other outcome.
    Agent(Option<Box<TermAgent>>),
    /// For [`Verb::ReadFile`]: the bytes from `offset` on, and the file's whole size.
    File {
        /// What was read.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        /// Where they start in the file.
        offset: u64,
        /// The file's size in bytes.
        size: u64,
    },
    /// For [`Verb::ListPorts`].
    Ports(Vec<Port>),
    /// Done, nothing to report ([`Verb::SendInput`], [`Verb::Close`],
    /// [`Verb::ResizeTerminal`], [`Verb::ForgetWorker`], [`Verb::AnswerRequest`],
    /// [`Verb::Upload`], [`Verb::AgentTell`], [`Verb::AgentStop`]).
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
    /// For [`Verb::ListWindows`].
    Screens {
        /// Windows, front to back.
        windows: Vec<WindowInfo>,
        /// Displays.
        displays: Vec<DisplayInfo>,
    },
    /// For [`Verb::ReadThread`].
    Thread(Box<ThreadRead>),
    /// For [`Verb::CaptureStill`]: a PNG, at the target's native pixel size or halved until it
    /// fits one reply.
    Still {
        /// The PNG file's bytes.
        #[serde(with = "serde_bytes")]
        png: Vec<u8>,
        /// Pixels across.
        width: u32,
        /// Pixels down.
        height: u32,
    },
    /// For [`Verb::Wake`]: the magic packet went out. Whether the worker wakes shows when it
    /// registers again.
    WakeSent {
        /// The machine that sent it: the server's name or a worker's.
        by: String,
        /// The interfaces of the sleeping worker it was sent for (`en0`).
        to: Vec<String>,
    },
    /// For [`Verb::Search`]: the files with matches in path order, and how the search ended.
    Search {
        /// The files.
        files: Vec<FileHits>,
        /// What it found; `capped` when there were more lines than it returned.
        summary: SearchSummary,
    },
    /// For [`Verb::WorkerFacts`].
    Facts(Vec<WorkerFacts>),
    /// For [`Verb::CloneRepo`]: where the clone is, and which repository it is.
    Cloned {
        /// Its root on the worker.
        path: String,
        /// Its identity, as the worker reads it.
        repo: crate::terminal::RepoId,
    },
    /// For [`Verb::BundleBranch`]: the bundle, to read with [`Verb::ReadFile`]; boxed, being
    /// rare and the largest.
    Bundle(Box<BranchBundle>),
    /// For [`Verb::FetchBundle`]: the branch is in the repository at `head`.
    Fetched {
        /// The branch it landed as.
        branch: String,
        /// Its commit.
        head: String,
    },
    /// For [`Verb::StartThread`] and [`Verb::AgentStart`]: the thread runs.
    ThreadStarted {
        /// The thread.
        thread: ThreadId,
        /// The worktree it works in, when it was given one.
        worktree: Option<Box<crate::agent::Worktree>>,
    },
    /// For [`Verb::FsChange`]: where the entry now is (the new folder, the moved entry, or its
    /// place in the trash), absolute with `~` spelled out.
    FsDone {
        /// The path.
        path: String,
    },
    /// For [`Verb::Settings`]: the daemon's settings file as it stands.
    Settings(Box<crate::settings::DaemonSettings>),
    /// For [`Verb::Agents`] and [`Verb::AgentWait`]: each agent as it stands.
    Agents {
        /// The agents.
        agents: Vec<crate::agent::AgentEntry>,
        /// A wait ran out before what it waited for came.
        timed_out: bool,
    },
}

/// Which thread a [`Verb::ReadThread`], a [`Verb::AnswerRequest`] or a [`Verb::SendMessage`] is
/// about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ThreadOf {
    /// The thread of the agent in a terminal, or of a thread seated there.
    Term(TermRef),
    /// A thread by its id, a subagent's too, on whichever worker holds it.
    Thread(ThreadId),
    /// A thread on a worker: what the server sends that worker once it found the thread.
    On {
        /// The worker that holds it.
        worker: WorkerId,
        /// The thread.
        thread: ThreadId,
    },
}

/// How much of each turn a [`Verb::ReadThread`] gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum ThreadView {
    /// What was said: the person's messages and the agent's answers.
    #[default]
    Messages,
    /// Those, and what the agent did between: each tool call with its outcome and the end of
    /// its output, and what the agent itself noted.
    Activity,
}

/// Turns of a thread, as [`Verb::ReadThread`] reads them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadRead {
    /// The worker that holds it.
    pub worker: WorkerId,
    /// The thread.
    pub thread: ThreadId,
    /// Its agent.
    pub agent: AgentId,
    /// What it is about.
    pub title: String,
    /// For a subagent's thread: its parent's.
    pub parent: Option<ThreadId>,
    /// Where it is.
    pub phase: Phase,
    /// What it waits on, in words, when it waits.
    pub wait: Option<String>,
    /// The turns after the one asked from, oldest first; the last may be under way.
    pub turns: Vec<TurnRead>,
    /// The requests open now, oldest first.
    pub requests: Vec<RequestRead>,
    /// The turn to read after next: the last whole turn given, or the one asked from when
    /// none ended.
    pub next: TurnId,
    /// Something was left out to keep the read small: a text cut short (it ends in
    /// [`ThreadRead::CUT`]), or turns that did not fit, which the next read from `next` gives.
    pub truncated: bool,
    /// Turns after the one asked from are no longer held: the read starts at the first held.
    pub skipped: bool,
}

impl ThreadRead {
    /// What a text cut short ends in.
    pub const CUT: &str = " [\u{2026}]";
}

/// One turn of a [`ThreadRead`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TurnRead {
    /// Its number.
    pub id: TurnId,
    /// How it stands.
    pub state: TurnState,
    /// When it began.
    pub started_ms: WallMs,
    /// When it ended.
    pub ended_ms: Option<WallMs>,
    /// What happened in it, in order, as the view asked.
    pub entries: Vec<ReadEntry>,
}

/// One thing in a turn of a [`ThreadRead`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReadEntry {
    /// What the person (or an automation) sent.
    User(String),
    /// The agent's answer.
    Text(String),
    /// A tool call ([`ThreadView::Activity`]).
    Tool {
        /// What kind of call ([`crate::thread::kind`]), open.
        kind: String,
        /// What it does, in words.
        title: String,
        /// Where it is.
        state: ToolState,
        /// The end of what it printed or returned.
        output: Option<String>,
        /// The thread of the subagent it started, to read in turn.
        child: Option<ThreadId>,
    },
    /// Something the agent itself said ([`ThreadView::Activity`]): an API error, a hook's word.
    Notice {
        /// Its kind ([`crate::thread::Notice::kind`]).
        kind: String,
        /// What it says.
        text: String,
    },
}

/// An open request of a [`ThreadRead`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RequestRead {
    /// Its id, for [`Verb::AnswerRequest`].
    pub ask: AskId,
    /// What it asks ([`crate::thread::Request::kind`]), open.
    pub kind: String,
    /// What it asks, in a line.
    pub title: String,
    /// The answers it offers, in its order: each `id` is a choice to answer with.
    pub choices: Vec<Choice>,
    /// Its questions, for a question.
    pub questions: Vec<String>,
    /// The answers a note offers as buttons of their own
    /// ([`NoteChoice::of`](crate::thread::wire::NoteChoice::of)): a note's pick, pressed,
    /// answers with the choice at its place here.
    pub picks: Vec<crate::thread::wire::NoteChoice>,
    /// Only the person at its card answers it ([`crate::thread::Request::needs_presence`]): an
    /// answer from here is refused, so a caller says so without asking
    /// ([`crate::thread::Request::NEEDS_PRESENCE_REFUSED`]).
    #[serde(default)]
    pub needs_presence: bool,
}
