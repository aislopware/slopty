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
//! Projects ([`crate::project`]) are the server's own: it answers the project and task verbs
//! from its store, starts what runs for a task on the worker its orchestrator names or one with
//! room ([`Verb::TaskSpawn`]), and logs every change as a [`Happening::Project`].

use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_core::{ItemId, SessionId, WallMs, WorkerId, XferId};

use crate::agent::{AgentEvent, AgentKind, AgentStatus, SessionAgent};
use crate::folder::FsOp;
use crate::items::{Item, ItemKind};
use crate::project::{
    LimitsChange, Project, ProjectId, ProjectStatus, ProjectUpdate, Report, TaskChange, TaskId,
    TaskLaunch, TaskSpec, WorkerFacts,
};
use crate::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use crate::search::{FileHits, SearchQuery, SearchSummary};
use crate::server::{Liveness, WorkerInfo};
use crate::terminal::SessionSummary;
use crate::thread::{AgentId, AskId, Choice, Phase, ToolState, TurnId, TurnState};
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
    /// the call, and another for each step, so a call that makes a task and then starts it
    /// makes one task however often it is sent.
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

/// A terminal on a worker.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TermRef {
    /// The worker that runs it.
    pub worker: WorkerId,
    /// The session on that worker.
    pub session: SessionId,
}

/// An item on a worker's workspace.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ItemRef {
    /// The worker whose registry holds it.
    pub worker: WorkerId,
    /// The item.
    pub item: ItemId,
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
        /// The id the terminal takes, when the caller chooses it: a start whose answer was lost
        /// is found by it, and a start again under an id the worker already runs answers that
        /// terminal instead of opening a second.
        session: Option<SessionId>,
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
        /// The id the terminal takes, as for [`Verb::OpenTerminal`].
        session: Option<SessionId>,
        /// The person allowed this agent flags and modes that loosen Claude Code's
        /// permissions. Without it the worker locks bypass mode off in the agent's settings.
        /// The server sets it from the person's policy on every start it forwards.
        permission_flags: bool,
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
        #[serde(with = "serde_bytes")]
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
    /// The items on a worker's workspace; answered with [`Outcome::Items`].
    ListItems {
        /// Where.
        worker: WorkerId,
    },
    /// Put an item on a worker's workspace, where every client shows it; answered with
    /// [`Outcome::Item`]. A terminal comes with [`Verb::OpenTerminal`] instead.
    OpenItem {
        /// Where.
        worker: WorkerId,
        /// What it shows.
        kind: ItemKind,
        /// A name for its tile.
        name: Option<String>,
    },
    /// Name an item, or take its name away.
    RenameItem {
        /// Which.
        item: ItemRef,
        /// The new name; none shows what the item's content says.
        name: Option<String>,
    },
    /// Take an item off the workspace; a terminal goes with [`Verb::Close`] instead.
    RemoveItem {
        /// Which.
        item: ItemRef,
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
        /// Hold the agent's prompts that only its terminal shows for an answer here from then
        /// on, as a person following the thread does. The server sets it, for the person alone:
        /// an agent's read leaves every prompt where it is.
        hold: bool,
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
    /// One step of a file sent up in parts, for a file too large for [`Verb::WriteFile`]. The
    /// parts land beside the file under the upload's name, and only a finished upload that adds
    /// up replaces it.
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
    /// Make a project; answered by the server with [`Outcome::Project`].
    ProjectCreate {
        /// Its name, unique on the server.
        project: ProjectId,
        /// What it is for, in a line.
        title: String,
        /// What else is in it ([`crate::project::Project::members`]).
        members: Vec<crate::project::Matcher>,
        /// The repository its tasks work in.
        repo: String,
        /// The branch finished work lands on.
        target: String,
        /// The command that says a task's work is right.
        verifier: Option<String>,
        /// Push the target to its clone's `origin` after each merge ([`Project::push`]); only
        /// the person turns it on.
        push: bool,
        /// The orchestrator's terminal.
        orchestrator: Option<TermRef>,
        /// Its limits over [`crate::project::Limits::default`]; only the person sets them.
        limits: LimitsChange,
        /// Anything its agents keep with it: the text of a JSON object.
        metadata: Option<String>,
    },
    /// Change a project's members, orchestrator, verifier, pushing, limits or metadata; what is
    /// absent stays. Answered with [`Outcome::Project`].
    ProjectSet {
        /// Which.
        project: ProjectId,
        /// Its members, in place of the old ([`crate::project::Project::members`]).
        members: Option<Vec<crate::project::Matcher>>,
        /// The orchestrator's terminal.
        orchestrator: Option<TermRef>,
        /// The verifier command; empty for none.
        verifier: Option<String>,
        /// Whether to push the target after each merge ([`Project::push`]); only the person
        /// sets it.
        push: Option<bool>,
        /// Its limits; only the person sets them.
        limits: LimitsChange,
        /// New metadata, in place of the old.
        metadata: Option<String>,
    },
    /// Every project; answered with [`Outcome::Projects`].
    ProjectList,
    /// A project whole, its timeline from `since`, waiting up to `timeout_ms` for an entry
    /// past it when there is none yet; answered with [`Outcome::Project`].
    ProjectStatus {
        /// Which.
        project: ProjectId,
        /// The first timeline entry wanted: the `next` of the previous answer. The last few
        /// when absent.
        since: Option<u64>,
        /// Wait this long for an entry past `since`; capped as [`Verb::WaitFor`] is.
        timeout_ms: u32,
    },
    /// Make a task under the project's orchestrator; answered with [`Outcome::Task`]. A
    /// dependency that leads back to it is refused.
    TaskCreate {
        /// In which project.
        project: ProjectId,
        /// What it is.
        spec: Box<TaskSpec>,
    },
    /// Move a task, record its branch or its verifier's word, or note something on its
    /// timeline; answered with [`Outcome::Task`].
    TaskUpdate {
        /// In which project.
        project: ProjectId,
        /// Which.
        task: TaskId,
        /// What changes.
        change: Box<TaskChange>,
    },
    /// Start what runs for a task on the worker it is pinned to, else one with room, with the
    /// project and the task in its environment; answered with [`Outcome::Task`] once it runs,
    /// or [`ErrorCode::Unplaced`] saying why each worker was passed over, or
    /// [`ErrorCode::Limit`] naming the limit reached: for an agent's start, among them, as
    /// many of the project's tasks waiting on the person as its [`crate::project::Limits::review`].
    TaskSpawn {
        /// In which project.
        project: ProjectId,
        /// Which.
        task: TaskId,
        /// How to start it.
        launch: TaskLaunch,
    },
    /// What the workers are and have; answered with [`Outcome::Facts`].
    WorkerFacts {
        /// This worker only; every one when absent.
        worker: Option<WorkerId>,
    },
    /// One node of a project's tree in full: a task with its brief, pin and metadata, or the
    /// orchestrator's node, each with the natives Claude Code keeps in it. Answered with
    /// [`Outcome::Node`].
    TaskGet {
        /// In which project.
        project: ProjectId,
        /// Which task; the orchestrator's node when absent.
        task: Option<TaskId>,
    },
    /// A task's agent reports on its work to the project's orchestrator, delivered through
    /// its hooks when the report's kind says; answered with [`Outcome::Task`].
    TaskReport {
        /// In which project.
        project: ProjectId,
        /// Which.
        task: TaskId,
        /// What it says.
        report: Report,
    },
    /// What a terminal works on by the server's record, in any project: answered with
    /// [`Outcome::WorkingOn`]. How a tool finds its caller's own project and task before the
    /// caller's environment, which may be stale or inherited.
    WorkingOn {
        /// The terminal, by its session.
        session: SessionId,
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
    /// Check out `head` in the project's verify checkout of the clone at `repo`
    /// ([`crate::project::VERIFY_PLACES`]), made on first use and reused, and run `command`
    /// there through the person's login shell in a new terminal under the id `session`, which
    /// every client shows. Answered with [`Outcome::Verifying`] once it runs; its end is the
    /// terminal's own.
    Verify {
        /// Where.
        worker: WorkerId,
        /// The clone the commit is in.
        repo: String,
        /// The checkout's name in [`crate::project::VERIFY_PLACES`]: the project's.
        worktree: String,
        /// The commit or branch to verify.
        head: String,
        /// The branch work lands on, whose fork point from `head` is the run's base.
        target: String,
        /// The verifier, a shell command line.
        command: String,
        /// The terminal's id, which the server chooses.
        session: SessionId,
        /// A name for its tile.
        title: String,
    },
    /// Rebase `head` onto the branch `onto` in the project's verify checkout of the clone at
    /// `repo`, as the merge queue does before it verifies again, with `trailers` added to each
    /// commit's message as `git interpret-trailers` adds them (a trailer already there is not
    /// added twice). A head that already holds `onto` and every trailer is answered as it is.
    /// Answered with [`Outcome::Rebased`], or [`ErrorCode::Conflict`] naming the paths that
    /// conflict.
    Rebase {
        /// Where.
        worker: WorkerId,
        /// The clone.
        repo: String,
        /// The checkout's name in [`crate::project::VERIFY_PLACES`].
        worktree: String,
        /// The commit to rebase.
        head: String,
        /// The branch it goes on top of.
        onto: String,
        /// Each `(token, value)` to add to every commit rebased: the task and the thread its
        /// work came from. A token is letters, digits and `-`; a value has no line break.
        trailers: Vec<(String, String)>,
        /// The commit last verified, whose tree the rebased one is compared with
        /// ([`Outcome::Rebased::verified`]).
        verified: Option<String>,
    },
    /// What `head` did to the tests since it left the branch `target`, in the clone at `repo`
    /// ([`crate::project::TestDiff`]): every test file it deleted, changed or added, by
    /// [`crate::project::is_test_path`] with `test_paths`. Answered with [`Outcome::TestDiff`].
    TestDiff {
        /// Where.
        worker: WorkerId,
        /// The clone.
        repo: String,
        /// The commit or branch the work is at.
        head: String,
        /// The branch it left.
        target: String,
        /// The project's own test paths, beside the usual ones.
        test_paths: Vec<String>,
    },
    /// Move the branch `target` of the clone at `repo` from `from` to `to`, a commit that
    /// descends from it, and nothing else: in the checkout that has it checked out, as `git
    /// merge --ff-only` does, so nothing of the person's there is overwritten. Then push it to
    /// `origin` when asked. Answered with [`Outcome::FastForwarded`], or
    /// [`ErrorCode::Conflict`] when the branch is no longer at `from`.
    FastForward {
        /// Where.
        worker: WorkerId,
        /// The clone.
        repo: String,
        /// The branch.
        target: String,
        /// The commit it must be at now.
        from: String,
        /// The commit it moves to.
        to: String,
        /// Push it to `origin` after.
        push: bool,
    },
    /// Tell a task's agent, or the project's orchestrator, something: the person's own words,
    /// or the orchestrator's to one of its tasks, marked as the orchestrator's. They reach the
    /// agent through its hooks as reports do (its inbox wakes it when idle), never typed into
    /// its terminal. The board's next steps (fix CI, address the review's comments, resolve
    /// conflicts) and its line to the orchestrator are said this way. Answered with
    /// [`Outcome::Done`]; a node with no agent running is [`ErrorCode::Invalid`]. Any agent but
    /// the orchestrator is [`ErrorCode::Forbidden`], and the orchestrator telling a task that
    /// waits on the person is [`ErrorCode::Conflict`].
    TaskTell {
        /// In which project.
        project: ProjectId,
        /// Which task's agent; the orchestrator when absent.
        task: Option<TaskId>,
        /// What the person says, at most [`crate::project::NOTE_MAX`] bytes.
        text: String,
    },
    /// Put a task in its project's merge queue, the person's word, and the only way work
    /// merges: a task its checks passed waits in Ready to merge until then. Work not checked
    /// yet (one given back, or done without a report) is verified first when the project or the
    /// task has a verifier. Answered with [`Outcome::Task`]; an agent is
    /// [`ErrorCode::Forbidden`].
    TaskMerge {
        /// In which project.
        project: ProjectId,
        /// Which.
        task: TaskId,
    },
    /// The person lets a project go: its board, its tasks and its queue. The terminals that
    /// worked in it stay, as terminals. Answered with [`Outcome::Done`].
    ProjectDelete {
        /// Which.
        project: ProjectId,
    },
    /// Read a pull request's own checks from its forge, with the forge's own command line
    /// (`gh`, `glab`) in a checkout of its repository, as the worker's user is signed in to it:
    /// nothing of the sign-in is read. The server's own, for a task's card. Answered with
    /// [`Outcome::Checks`]; [`ErrorCode::Unsupported`] when the worker has no such command.
    PullChecks {
        /// Where.
        worker: WorkerId,
        /// A checkout of the repository the pull request is in.
        cwd: String,
        /// Its number.
        number: u32,
        /// A GitLab merge request rather than a GitHub pull request.
        merge_request: bool,
    },
    /// Push a merged task's target to its clone's `origin` again after the push that went
    /// with its merge failed, the person's word. The target goes as the merge left it, so a
    /// target moved on since is pushed from where it is instead. Answered with
    /// [`Outcome::Task`], the card saying how the push went; a task whose merge was pushed is
    /// answered as it is, one not merged is [`ErrorCode::Invalid`], and an agent is
    /// [`ErrorCode::Forbidden`].
    TaskPush {
        /// In which project.
        project: ProjectId,
        /// Which.
        task: TaskId,
    },
    /// Remove a finished task's worktree from its worker: one an agent made under its clone's
    /// `.claude/worktrees/`, with nothing uncommitted in it and no terminal on the worker
    /// working in it. The branch it had checked out goes too only when every commit on it is
    /// in one of `landed` by patch (`git cherry`), so work the merge queue rebased counts as
    /// landed; otherwise the branch is kept. Answered with [`Outcome::WorktreeRemoved`];
    /// [`ErrorCode::Conflict`] while a terminal works in it or something in it is not
    /// committed, [`ErrorCode::Invalid`] for a path that is no such worktree.
    RemoveWorktree {
        /// Where.
        worker: WorkerId,
        /// The worktree, as its agent reported it.
        worktree: String,
        /// Where the task's work landed, as commits or branches of the clone (the merge's
        /// head, the target, the target on `origin`); none keeps the branch.
        landed: Vec<String>,
    },
    /// Start a task's agent as a thread of the worker's thread host, seated at `seat`: its
    /// row carries [`crate::project::SEAT_FACT`], its Slopty tools speak as `seat` with the
    /// worker's token for it and `env`, and `role` reaches it through its agent's own door (a
    /// system prompt where it takes one, else ahead of its first prompt). An agent whose
    /// thread runs in a terminal runs in one opened under `seat`. With `worktree`, it works in
    /// a git worktree of that name the worker makes under the clone at the start's `cwd`
    /// (`.claude/worktrees/<name>`, on branch `worktree-<name>`, from `origin`'s default
    /// branch, else `HEAD`), or reopens when it is there. A repeat with a `seat` already
    /// started answers that thread. Answered with [`Outcome::ThreadStarted`];
    /// [`ErrorCode::Unsupported`] for an agent the worker cannot start.
    StartThread {
        /// Where.
        worker: WorkerId,
        /// What to start.
        start: Box<crate::thread::wire::Start>,
        /// The id it is known by: the task's [`crate::project::Assignment::term`].
        seat: SessionId,
        /// Variables for the agent and its Slopty tools (the server, project and task).
        env: Vec<(String, String)>,
        /// What the agent is told it is for.
        role: Option<String>,
        /// The name of the worktree of its own it works in, for an agent that writes.
        worktree: Option<String>,
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
    /// Keep `script` in `project`, in place of one of its name. Answered with
    /// [`Outcome::Project`]. The person's alone.
    ScriptSet {
        /// The project.
        project: ProjectId,
        /// The script.
        script: crate::project::Script,
    },
    /// Take script `name` away from `project`. Answered with [`Outcome::Project`]. The
    /// person's alone.
    ScriptDelete {
        /// The project.
        project: ProjectId,
        /// The script's name.
        name: String,
    },
    /// Run script `name` of `project` in a terminal of the person's: in `task`'s worktree on
    /// its worker, else in the project's folder on `worker` (its orchestrator's when absent).
    /// Answered with [`Outcome::Opened`]. The person's alone.
    ScriptRun {
        /// The project.
        project: ProjectId,
        /// The script's name.
        name: String,
        /// The worker; the task's, or the orchestrator's, when absent.
        worker: Option<WorkerId>,
        /// The task whose worktree it runs in.
        task: Option<TaskId>,
    },
    /// Server → worker, for [`Verb::ScriptRun`]: open a terminal in `cwd` running `line`
    /// through the person's login shell, then leave them that shell. Answered with
    /// [`Outcome::Opened`].
    RunScript {
        /// Where.
        worker: WorkerId,
        /// The folder it runs in.
        cwd: String,
        /// The command line.
        line: String,
        /// The terminal's title.
        name: String,
        /// The terminal's id, chosen by the server.
        session: SessionId,
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
            | Self::SpawnAgent { .. }
            | Self::SendInput { .. }
            | Self::WaitFor { .. }
            | Self::Close { .. }
            | Self::WriteFile { .. }
            | Self::ResizeTerminal { .. }
            | Self::ForgetWorker { .. }
            | Self::OpenItem { .. }
            | Self::RenameItem { .. }
            | Self::RemoveItem { .. }
            | Self::AnswerRequest { .. }
            | Self::ProjectCreate { .. }
            | Self::ProjectSet { .. }
            | Self::TaskCreate { .. }
            | Self::TaskUpdate { .. }
            | Self::TaskSpawn { .. }
            | Self::TaskTell { .. }
            | Self::TaskReport { .. }
            | Self::BundleBranch { .. }
            | Self::FetchBundle { .. }
            | Self::TaskMerge { .. }
            | Self::TaskPush { .. }
            | Self::ProjectDelete { .. }
            | Self::Verify { .. }
            | Self::Rebase { .. }
            | Self::FastForward { .. }
            | Self::RemoveWorktree { .. }
            | Self::StartThread { .. }
            | Self::FsChange { .. }
            | Self::ScriptSet { .. }
            | Self::ScriptDelete { .. }
            | Self::ScriptRun { .. }
            | Self::RunScript { .. } => true,
            // A part rewrites the same bytes and an abort finds nothing the second time; only
            // the finish replaces the file.
            Self::Upload { part, .. } => matches!(part, UploadPart::Finish { .. }),
            // A clone there already is answered as it is.
            Self::CloneRepo { .. }
            | Self::PullChecks { .. }
            | Self::TestDiff { .. }
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
            | Self::ListItems { .. }
            | Self::ListWindows { .. }
            | Self::ReadThread { .. }
            | Self::CaptureStill { .. }
            | Self::ProjectList
            | Self::ProjectStatus { .. }
            | Self::WorkerFacts { .. }
            | Self::TaskGet { .. }
            | Self::WorkingOn { .. }
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
                Happening::Agent { event, .. }
                    if matches!(
                        event.status,
                        AgentStatus::Blocked(_)
                            | AgentStatus::Idle
                            | AgentStatus::Done
                            | AgentStatus::Failed { .. }
                    )
            ),
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
        /// What; boxed, being the largest but for a project's.
        summary: Box<SessionSummary>,
    },
    /// A terminal ended.
    SessionClosed {
        /// Which.
        term: TermRef,
    },
    /// An agent's status changed, or where its status came from did; its status is
    /// [`AgentStatus::None`] when it left.
    Agent {
        /// Where.
        worker: WorkerId,
        /// What, as the worker reported it.
        event: AgentEvent,
    },
    /// A terminal's program exited; the terminal stays, its last screen readable, until it is
    /// closed.
    SessionExited {
        /// Which.
        term: TermRef,
        /// Exit status, or the signal number negated.
        status: i32,
    },
    /// A project or one of its tasks changed; boxed, being the largest by far.
    Project(Box<ProjectUpdate>),
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
    /// No such item.
    UnknownItem,
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
    /// No such project.
    UnknownProject,
    /// No such task in the project.
    UnknownTask,
    /// It would take what another holds: a task another agent works on, a branch moved on.
    Conflict,
    /// No worker may run the task now; the message says why each was passed over.
    Unplaced,
    /// A limit the project or the person set is reached, or would be passed; the message
    /// names it and who may raise it.
    Limit,
    /// A metadata document does not parse or check.
    BadExpression,
    /// Nothing may type into the agent now: it waits on a person (a permission, a question),
    /// or a person has typed into its composer and not sent it. The message says which.
    AwaitsPerson,
    /// The agent has not said it is ready: no hook of its session has arrived yet, so a
    /// dialog of its own (trusting a folder, an MCP server) may be up.
    AgentNotReady,
    /// The agent's program has ended; what is typed would reach the shell below it.
    AgentExited,
    /// The caller may not do this: an agent answering a permission, or moving a task where
    /// only the person or the merge queue may.
    Forbidden,
    /// There is nothing new to carry: the branch has no commit beyond the one it would start
    /// after, so the other side has it from the same forge.
    NothingNew,
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
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
        /// Where they start in the file.
        offset: u64,
        /// The file's size in bytes.
        size: u64,
    },
    /// For [`Verb::ListPorts`].
    Ports(Vec<Port>),
    /// Done, nothing to report ([`Verb::SendInput`], [`Verb::Close`], [`Verb::WriteFile`],
    /// [`Verb::ResizeTerminal`], [`Verb::ForgetWorker`], [`Verb::RenameItem`],
    /// [`Verb::RemoveItem`], [`Verb::AnswerRequest`], [`Verb::Upload`]).
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
    /// For [`Verb::ListItems`].
    Items(Vec<Item>),
    /// For [`Verb::OpenItem`].
    Item(ItemRef),
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
    /// For [`Verb::ProjectCreate`], [`Verb::ProjectSet`] and [`Verb::ProjectStatus`].
    Project(Box<ProjectStatus>),
    /// For [`Verb::ProjectList`]: every project, by name.
    Projects(Vec<Project>),
    /// For the task verbs: the task as it is now.
    Task(Box<crate::project::Task>),
    /// For [`Verb::WorkerFacts`].
    Facts(Vec<WorkerFacts>),
    /// For [`Verb::TaskGet`].
    Node(Box<crate::project::NodeDetail>),
    /// For [`Verb::WorkingOn`]: the project and the task, none for its orchestrator; none when
    /// the terminal is on nothing.
    WorkingOn(Option<(ProjectId, Option<TaskId>)>),
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
    /// For [`Verb::Verify`]: the verifier runs in `term`, on these commits.
    Verifying {
        /// Its terminal.
        term: TermRef,
        /// The commit checked out, in hex.
        head: String,
        /// Where it left the target branch, in hex.
        base: String,
    },
    /// For [`Verb::Rebase`]: what the rebase made, on top of the target at `onto`.
    Rebased {
        /// The rebased commit, in hex; the head given when it already held `onto` and every
        /// trailer.
        head: String,
        /// The target's commit it is on top of, in hex.
        onto: String,
        /// Its tree is the very tree of the commit last verified: only messages changed, so
        /// what was verified holds for it.
        verified: bool,
    },
    /// For [`Verb::TestDiff`].
    TestDiff(crate::project::TestDiff),
    /// For [`Verb::FastForward`]: the branch is at `head`.
    FastForwarded {
        /// Its commit now, in hex.
        head: String,
        /// It was pushed to `origin`.
        pushed: bool,
        /// Why a push asked for did not happen; the branch moved all the same.
        push_failed: Option<String>,
    },
    /// For [`Verb::PullChecks`]: what the pull request's checks say.
    Checks(crate::project::Checks),
    /// For [`Verb::RemoveWorktree`]: the worktree is gone.
    WorktreeRemoved {
        /// The branch it had checked out, if one.
        branch: Option<String>,
        /// Whether that branch went too, its work all landed.
        branch_removed: bool,
    },
    /// For [`Verb::StartThread`]: the thread runs.
    ThreadStarted {
        /// The thread.
        thread: crate::thread::ThreadId,
        /// The worktree it works in, when it was given one.
        worktree: Option<Box<crate::agent::Worktree>>,
    },
    /// For [`Verb::FsChange`]: where the entry now is (the new folder, the moved entry, or its
    /// place in the trash), absolute with `~` spelled out.
    FsDone {
        /// The path.
        path: String,
    },
}

/// Which thread a [`Verb::ReadThread`] or a [`Verb::AnswerRequest`] is about.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ThreadOf {
    /// The thread working on a task now: its assignment's.
    Task {
        /// The project.
        project: ProjectId,
        /// The task.
        task: TaskId,
    },
    /// The thread of the agent in a terminal, or of a task's thread seated there.
    Term(TermRef),
    /// A thread by its id, a subagent's too, on whichever worker holds it.
    Thread(crate::thread::ThreadId),
    /// A thread on a worker: what the server sends that worker once it found the thread.
    On {
        /// The worker that holds it.
        worker: WorkerId,
        /// The thread.
        thread: crate::thread::ThreadId,
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
    pub thread: crate::thread::ThreadId,
    /// Its agent.
    pub agent: AgentId,
    /// What it is about.
    pub title: String,
    /// For a subagent's thread: its parent's.
    pub parent: Option<crate::thread::ThreadId>,
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
        child: Option<crate::thread::ThreadId>,
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
}
