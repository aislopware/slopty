//! The orchestration verbs, as a worker answers them (`docs/decisions/topology.md`).
//!
//! The server forwards every [`Verb`] naming this worker down the worker's link, and
//! [`Orchestrator::serve`] answers it. Terminals opened here go through the same path a
//! client's `OpenSession` takes and are announced on the same broadcast, so every attached
//! client sees them like any other. Reads come from the session's engine on its own thread
//! ([`SessionHandle::read`]); waits sleep on the session's [`Activity`](crate::session::Activity)
//! and the agent events, never on a timer that asks again.
//!
//! The per-session functions ([`send_input`], [`read_screen`], [`read_output`],
//! [`list_commands`], [`wait_for`]) take a [`SessionHandle`] alone, so they work on a session
//! actor without ptyd behind it.
//!
//! A verb that changes something and comes with an idempotency key is done once per key
//! ([`idempotency`]). A start under an id the caller chose is done once per id: asked again,
//! it answers the terminal that id already names.
//!
//! Nothing is typed into an agent's terminal while the agent cannot take it ([`may_type`]):
//! while it waits on a person, while a person has a line typed and unsent, before its first
//! hook, or after it ended. An agent's first prompt waits for its hooks to say it is at its
//! prompt, and is never typed blind.
//!
//! Any agent's thread is read and its requests answered through the daemon's [`ThreadReads`]
//! ([`thread_read`]), a Claude Code TUI's held prompts followed through its [`Conversations`]
//! ([`conversation`]); a still picture from ScreenCaptureKit ([`still`]); a
//! file too large for one reply goes up in parts ([`upload`]).

pub mod conversation;
pub mod idempotency;
pub mod keys;
pub mod still;
pub mod thread_read;
pub mod upload;
mod wait;

use std::collections::HashSet;
use std::io::{Read as _, Seek as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use conversation::{Conversations, Sources};
use slopty_agent::status::{AgentEvent, AgentSource, AgentStatus, BlockReason, SessionAgent};
use slopty_core::{ClientId, ItemId, SessionId, WorkerId};
use slopty_engine::ghostty::Position;
use slopty_proto::WorkerMsg;
use slopty_proto::folder::{FsOutcome, FsRefusal};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::orchestration::{
    BUNDLES, BranchBundle, Command, DirEntry, ErrorCode, FileStat, IdempotencyKey, Input, ItemRef,
    Line, Outcome, Screen, Size, TermAgent, TermRef, ThreadOf, Verb, WaitUntil,
};
use slopty_proto::project::VERIFY_PLACES;
use slopty_proto::screen::ScreenEvent;
use slopty_proto::terminal::{
    CloseReason, OpenSession, SessionState, SessionSummary, TermRequest, TermSize,
};
use slopty_proto::thread::wire::{self, Intent, ThreadRow};
use slopty_proto::thread::{Delivery, IntentId, ThreadId, ThreadState};
use tokio::sync::broadcast;
pub use wait::{AgentFeed, wait_for};

use crate::session::{Read, SessionHandle, Text};
use crate::{ItemStore, Worker, WorkerError, listing};

/// Who orchestration acts as, for the session actor (its errors go nowhere: the verb's own
/// outcome reports them) and for the item deltas it causes (nobody's echo).
const ORCHESTRATOR: ClientId = ClientId::nil();

/// How many clone progress steps wait for the server's link before the oldest are dropped.
const CLONE_PROGRESS: usize = 64;

/// Lines one `ReadOutput` returns at most, whatever it asks: a reply is one control-stream
/// frame, and a caller pages on with `next`.
pub const MAX_OUTPUT_LINES: u32 = 10_000;

/// Most bytes one `ReadFile` returns. A reply is one frame on the server's control stream,
/// and this leaves half of it for the envelope; a caller pages through a larger file with
/// `offset`.
pub const MAX_FILE_BYTES: u64 = 8 << 20;
const _: () = assert!(
    MAX_FILE_BYTES.saturating_mul(2) <= slopty_proto::codec::MAX_FRAME_BYTES as u64,
    "a whole read and its envelope fit in one frame"
);

/// The longest a `Search` walks.
///
/// Well under the minute the server waits for any verb's answer (`slopty_server::hub`), so a
/// search of a whole disk answers with what it found by then rather than walking on for a
/// caller that gave up.
pub const SEARCH_WITHIN: Duration = Duration::from_secs(30);

/// Most entries one `ListDir` returns: names of up to 255 bytes each keep the reply a few
/// megabytes, well inside a frame.
pub const MAX_DIR_ENTRIES: u32 = 10_000;

/// The grid a verb may ask for, inclusive: as small as a status line, as large as a wall of
/// displays in a small font.
const MIN_SIZE: Size = Size { cols: 10, rows: 2 };
const MAX_SIZE: Size = Size { cols: 1000, rows: 500 };

/// How long a spawned agent's first prompt waits for the agent's hooks to say it is at its
/// prompt.
///
/// A person may be answering a dialog of its own first (trusting a folder, an MCP server), so
/// this is long; past it the prompt is left unsent, never typed blind.
pub const PROMPT_READY_WITHIN: Duration = Duration::from_mins(10);

/// Between a pasted prompt and the Enter that submits it. Ink-based TUIs (Claude Code) read
/// a paste and an Enter that arrive in one read as one paste, and the Enter is swallowed.
const SUBMIT_PAUSE: Duration = Duration::from_millis(200);

/// How long after answering [`Verb::RestartWorker`] the daemon exits: time for the answer to
/// leave on the server's link, which goes with the process.
const RESTART_AFTER: Duration = Duration::from_millis(500);

/// Size of a terminal opened by a verb, until a client attaches and drives it.
const ORCHESTRATED_SIZE: TermSize = TermSize {
    cols: 120,
    rows: 36,
    metrics: slopty_proto::input::CellMetrics { cell_width: 8, cell_height: 16 },
};

/// Coding-agent status as the daemon tracks it (`slopty_agent::AgentTable` behind a lock).
pub trait Agents: Send + Sync {
    /// The agent in `session` now, if one runs.
    fn status(&self, session: SessionId) -> Option<SessionAgent>;
    /// `session` is gone; drop what was known about it.
    fn forget(&self, session: SessionId);
    /// The agent that ran in `session` has ended and none has started there since. A table
    /// that keeps no such history says no.
    fn ended(&self, _session: SessionId) -> bool {
        false
    }
    /// The person stopped the last turn of the agent in `session` (Esc) and has not prompted
    /// it since. A table that keeps no such word says no.
    fn interrupted(&self, _session: SessionId) -> bool {
        false
    }
}

/// A task's thread to start ([`TaskThreads::start`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TaskThread {
    /// What to start, in the folder it works in.
    pub start: wire::Start,
    /// The id it is known by: its row's [`slopty_proto::project::SEAT_FACT`], the session its
    /// Slopty tools speak as, and the terminal it runs in when its agent runs in one.
    pub seat: SessionId,
    /// Variables for the agent and its Slopty tools, from the server (its project and task).
    pub env: Vec<(String, String)>,
    /// What the agent is told it is for.
    pub role: Option<String>,
}

/// The worker's thread host, as the server's tasks start threads in it
/// ([`Verb::StartThread`]). The daemon gives it ([`Orchestrator::set_task_threads`]).
pub trait TaskThreads: Send + Sync {
    /// Start `thread`, answering its id once its row is in the table.
    ///
    /// Its row carries [`slopty_proto::project::SEAT_FACT`] naming the seat. Its Slopty tools
    /// get `env` with what every terminal of the worker gets for that seat (the server, the
    /// seat as its session, the token for it), through its agent's own door. The role goes
    /// through that door too: a system prompt where the agent takes one, else ahead of its
    /// first prompt. An agent that runs in a terminal runs in one opened under the seat. A
    /// seat started already answers its thread, starting nothing.
    ///
    /// # Errors
    /// [`ErrorCode::Unsupported`] for an agent it cannot start; any other failure as it is.
    fn start(&self, thread: TaskThread) -> BoxFuture<'_, Result<ThreadId, Failure>>;

    /// End the agent of the thread seated at `seat`, its session kept to take up again.
    /// `Ok(false)` when no thread is seated there.
    ///
    /// # Errors
    /// A thread there that could not be ended.
    fn close(&self, seat: SessionId) -> BoxFuture<'_, Result<bool, Failure>>;
}

/// The worker's threads as orchestration reads and answers them ([`Verb::ReadThread`],
/// [`Verb::AnswerRequest`]). The daemon gives it ([`Orchestrator::set_thread_reads`]).
pub trait ThreadReads: Send + Sync {
    /// `thread` as it stands; `None` for one this worker does not hold.
    fn state(&self, thread: ThreadId) -> Option<ThreadState>;

    /// Do `intent` on `thread` once under `id`, as a client's would be done, by orchestration:
    /// a prompt held in a terminal is answered as [`crate::conversation::ORCHESTRATION`].
    fn intent(&self, thread: ThreadId, id: IntentId, intent: Intent) -> wire::Outcome;

    /// The row of the thread whose agent runs in terminal `session`, hanging from no other:
    /// the latest to change, when several did.
    fn at_terminal(&self, session: SessionId) -> Option<ThreadRow>;
}

/// A boxed future a [`TaskThreads`] answers with.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A verb that failed: the code and the words for whoever asked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failure {
    /// Why, as a code.
    pub code: ErrorCode,
    /// Why, in words.
    pub message: String,
}

impl Failure {
    /// A failure.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    fn outcome(self) -> Outcome {
        Outcome::Error { code: self.code, message: self.message }
    }
}

impl From<WorkerError> for Failure {
    fn from(e: WorkerError) -> Self {
        let code = match e {
            WorkerError::NoSuchSession | WorkerError::SessionClosed => ErrorCode::UnknownTerminal,
            _ => ErrorCode::Failed,
        };
        Self::new(code, e.to_string())
    }
}

/// The queue's git work that failed, as the server is told: a conflict, or a target that
/// moved, is the work's to settle; anything else is not.
fn verify_failure(failed: &crate::repo::verify::Failed) -> Failure {
    use crate::repo::verify::Failed;
    let code = match failed {
        Failed::Conflict(_) | Failed::Moved(_) | Failed::Diverged(_) => ErrorCode::Conflict,
        Failed::Protected(_) => ErrorCode::Protected,
        Failed::Other(_) => ErrorCode::Failed,
    };
    Failure::new(code, failed.to_string())
}

/// A bundle that could not be made or fetched, as the server is told: a receiver that lacks
/// the fork point is a conflict, which a whole-branch bundle resolves.
fn bundle_failure(failed: crate::repo::bundle::Failed) -> Failure {
    match failed {
        crate::repo::bundle::Failed::Prerequisites(why) => Failure::new(ErrorCode::Conflict, why),
        crate::repo::bundle::Failed::NothingNew(why) => Failure::new(ErrorCode::NothingNew, why),
        crate::repo::bundle::Failed::Other(why) => Failure::new(ErrorCode::Failed, why),
    }
}

/// The settings file at `path` after `edits` under `root`, as the wire says it; an edit that
/// does not hold is [`ErrorCode::Invalid`] and writes nothing.
///
/// # Errors
/// An edit does not hold, or the file does not read or write.
pub fn settings_file(
    path: &Path,
    root: &str,
    edits: &[slopty_proto::settings::SettingEdit],
) -> Result<slopty_proto::settings::DaemonSettings, Failure> {
    use slopty_settings::daemon::{Edit, File, Refused, read_and_edit};
    let edits: Vec<Edit<'_>> = edits
        .iter()
        .map(|e| Edit {
            table: &e.table,
            key: &e.key,
            entry: e.entry.as_deref(),
            literal: e.literal.as_deref(),
        })
        .collect();
    match read_and_edit(path, root, &edits) {
        Ok(File { path, text, problems }) => Ok(slopty_proto::settings::DaemonSettings {
            path: path.to_string_lossy().into_owned(),
            text,
            tables: vec![root.to_owned()],
            problems,
        }),
        Err(Refused::Edit(why)) => Err(Failure::new(ErrorCode::Invalid, why)),
        Err(Refused::Io(why)) => Err(Failure::new(ErrorCode::Failed, why)),
    }
}

/// Answers the verbs the server forwards to this worker. Cheap to clone.
#[derive(Clone)]
pub struct Orchestrator {
    inner: Arc<Inner>,
}

struct Inner {
    id: WorkerId,
    worker: Worker,
    items: ItemStore,
    events: broadcast::Sender<WorkerMsg>,
    /// What the agents' hooks and the daemon's poll report, as the daemon's agent table took it.
    heard: broadcast::Sender<AgentEvent>,
    launch: Launch,
    conversations: Arc<dyn Conversations>,
    once: idempotency::Ledger,
    /// Terminals orchestration started as an agent's: an agent's before it is first seen, and
    /// after it ends.
    agent_terms: parking_lot::Mutex<HashSet<SessionId>>,
    /// Held while a start under a chosen id looks for it and opens it, so two starts under one
    /// id open one terminal.
    choosing: tokio::sync::Mutex<()>,
    /// The clones the server asked for, at most a few at a time.
    cloner: crate::repo::cloning::Cloner,
    /// How they go ([`Orchestrator::clone_progress`]).
    clone_progress: broadcast::Sender<(u64, crate::repo::cloning::Progress)>,
    /// The thread host the server's tasks start threads in, once the daemon gave it.
    task_threads: std::sync::OnceLock<Arc<dyn TaskThreads>>,
    /// The threads orchestration reads and answers, once the daemon gave them.
    thread_reads: std::sync::OnceLock<Arc<dyn ThreadReads>>,
    /// The `settings.toml` the daemon follows, whose `[worker]` another device reads and edits
    /// ([`Verb::Settings`]), once the daemon gave it.
    settings_file: std::sync::OnceLock<PathBuf>,
    /// What has the daemon exit for its service manager to start it again
    /// ([`Verb::RestartWorker`]), once the daemon gave it: only a daemon a service manager
    /// keeps alive gives one.
    restart: std::sync::OnceLock<Arc<tokio::sync::Notify>>,
}

/// What an agent the orchestrator starts is given.
#[derive(Clone, Debug, Default)]
pub struct Launch {
    /// The `slopty` binary beside the worker: the `slopty hook` relay it reports through,
    /// registered on its `--settings`, and `slopty mcp`, which serves it Slopty's tools on its
    /// `--mcp-config` (`docs/decisions/projects.md`).
    pub relay: Option<PathBuf>,
    /// Slopty's Claude Code mod, loaded with its flag and environment.
    pub claude_mod: Option<slopty_agent::claude_mod::Installed>,
}

impl std::fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrator").field("worker", &self.inner.id).finish_non_exhaustive()
    }
}

impl Orchestrator {
    /// An orchestrator for worker `id`, acting on its sessions, agent table, item registry,
    /// client broadcast, agent reports and followed conversations. The agents it starts get
    /// what `launch` says; the clones the server asks for share `cloner` with the person's own.
    #[must_use]
    pub fn new(
        id: WorkerId,
        worker: Worker,
        items: ItemStore,
        (events, heard): (broadcast::Sender<WorkerMsg>, broadcast::Sender<AgentEvent>),
        launch: Launch,
        (conversations, cloner): (Arc<dyn Conversations>, crate::repo::cloning::Cloner),
    ) -> Self {
        let inner = Inner {
            id,
            worker,
            items,
            events,
            heard,
            launch,
            conversations,
            once: idempotency::Ledger::default(),
            agent_terms: parking_lot::Mutex::default(),
            choosing: tokio::sync::Mutex::default(),
            cloner,
            clone_progress: broadcast::channel(CLONE_PROGRESS).0,
            task_threads: std::sync::OnceLock::new(),
            thread_reads: std::sync::OnceLock::new(),
            settings_file: std::sync::OnceLock::new(),
            restart: std::sync::OnceLock::new(),
        };
        Self { inner: Arc::new(inner) }
    }

    /// Start the server's tasks' threads in `threads` from now on. The first one given
    /// stays.
    pub fn set_task_threads(&self, threads: Arc<dyn TaskThreads>) {
        if self.inner.task_threads.set(threads).is_err() {
            tracing::warn!("the task threads were given twice; the first stay");
        }
    }

    /// Read and answer threads through `reads` from now on. The first one given stays.
    pub fn set_thread_reads(&self, reads: Arc<dyn ThreadReads>) {
        if self.inner.thread_reads.set(reads).is_err() {
            tracing::warn!("the thread reads were given twice; the first stay");
        }
    }

    /// Read and edit `[worker]` of the settings file at `path` for another device from now on
    /// ([`Verb::Settings`]). The first one given stays.
    pub fn set_settings_file(&self, path: PathBuf) {
        if self.inner.settings_file.set(path).is_err() {
            tracing::warn!("the settings file was given twice; the first stays");
        }
    }

    /// Have the daemon exit through `restart` when a client asks it to start again
    /// ([`Verb::RestartWorker`]), from now on: given only by a daemon that launchd or systemd
    /// keeps alive, which starts it again. The first one given stays.
    pub fn set_restart(&self, restart: Arc<tokio::sync::Notify>) {
        if self.inner.restart.set(restart).is_err() {
            tracing::warn!("the restart was given twice; the first stays");
        }
    }

    /// `[worker]` of this worker's settings file after `edits`.
    async fn settings(
        &self,
        of: Option<WorkerId>,
        edits: Vec<slopty_proto::settings::SettingEdit>,
    ) -> Result<Outcome, Failure> {
        let Some(worker) = of else {
            return Err(Failure::new(
                ErrorCode::Invalid,
                "the server answers for its own settings",
            ));
        };
        self.mine(worker)?;
        let path = self.inner.settings_file.get().cloned().ok_or_else(|| {
            Failure::new(ErrorCode::Unsupported, "this worker follows no settings file")
        })?;
        let read = tokio::task::spawn_blocking(move || settings_file(&path, "worker", &edits))
            .await
            .map_err(|e| Failure::new(ErrorCode::Failed, e.to_string()))??;
        Ok(Outcome::Settings(Box::new(read)))
    }

    /// The thread `of` names on this worker, and the threads to reach it through.
    fn thread_of(&self, of: &ThreadOf) -> Result<(ThreadId, Arc<dyn ThreadReads>), Failure> {
        let ThreadOf::On { worker, thread } = *of else {
            return Err(Failure::new(
                ErrorCode::Invalid,
                "the server finds which worker holds a thread; a worker reads one it names",
            ));
        };
        self.mine(worker)?;
        let reads = self.inner.thread_reads.get().cloned().ok_or_else(|| {
            Failure::new(ErrorCode::Unsupported, "this worker keeps no threads to read")
        })?;
        Ok((thread, reads))
    }

    /// Answer one verb, once per `key` when it changes something. Every failure is an
    /// [`Outcome::Error`].
    pub async fn serve(&self, key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
        match key {
            Some(key) if verb.changes() => {
                let this = self.clone();
                let once = verb.clone();
                self.inner.once.run(key, &verb, async move { this.answer(once).await }).await
            }
            _ => self.answer(verb).await,
        }
    }

    async fn answer(&self, verb: Verb) -> Outcome {
        self.dispatch(verb).await.unwrap_or_else(Failure::outcome)
    }

    async fn dispatch(&self, verb: Verb) -> Result<Outcome, Failure> {
        let inner = &self.inner;
        match verb {
            Verb::ListWorkers
            | Verb::ListTerminals { .. }
            | Verb::Events { .. }
            | Verb::ForgetWorker { .. }
            | Verb::Wake { .. }
            | Verb::ProjectCreate { .. }
            | Verb::ProjectSet { .. }
            | Verb::TaskMerge { .. }
            | Verb::TaskPush { .. }
            | Verb::ProjectDelete { .. }
            | Verb::TaskTell { .. }
            | Verb::ProjectList
            | Verb::ProjectStatus { .. }
            | Verb::TaskCreate { .. }
            | Verb::TaskUpdate { .. }
            | Verb::TaskSpawn { .. }
            | Verb::TaskRestart { .. }
            | Verb::WorkerFacts { .. }
            | Verb::TaskGet { .. }
            | Verb::TaskReport { .. }
            | Verb::WorkingOn { .. } => {
                Err(Failure::new(ErrorCode::Invalid, "the server answers this, not a worker"))
            }
            Verb::Settings { of, edits } => self.settings(of, edits).await,
            Verb::OpenTerminal { worker, cwd, command, env, name, size, session, worktree } => {
                self.mine(worker)?;
                let _choosing = self.choosing(session).await;
                if let Some(running) = self.running(session) {
                    return Ok(Outcome::Opened(TermRef { worker, session: running }));
                }
                let (cwd, made) = match (worktree, cwd) {
                    (Some(asked), Some(clone)) => {
                        let (at, made) = crate::repo::worktrees::open(&clone, asked, &|_| {})
                            .await
                            .map_err(|failed| worktree_failed(&failed))?;
                        (Some(at), Some(made))
                    }
                    (Some(_), None) => {
                        return Err(Failure::new(
                            ErrorCode::Invalid,
                            "a terminal opens in a worktree of the clone its cwd names",
                        ));
                    }
                    (None, cwd) => (cwd, None),
                };
                let req = OpenSession {
                    size: term_size(size)?,
                    cwd,
                    command,
                    env,
                    title: name,
                    attach: false,
                };
                let handle = self.open_as(session, &req, ORCHESTRATOR).await?;
                // A command that runs Claude Code is guarded as a spawned agent is from the
                // start: until its first hook a dialog of its own may be up, and typing would
                // answer it.
                if slopty_agent::detect::is_claude("", &req.command) {
                    inner.agent_terms.lock().insert(handle.id());
                }
                Ok(opened(TermRef { worker, session: handle.id() }, made))
            }
            Verb::SpawnAgent {
                worker,
                cwd,
                prompt,
                args,
                env,
                size,
                session,
                permission_flags,
                worktree,
            } => {
                self.mine(worker)?;
                let spawn = Spawn { cwd, args, env, size: term_size(size)?, permission_flags };
                let _choosing = self.choosing(session).await;
                if let Some(running) = self.running(session) {
                    return Ok(Outcome::Opened(TermRef { worker, session: running }));
                }
                // Made, or reopened as it is, for the agent's own `--worktree <name>` to open.
                let made = match worktree {
                    Some(asked) => Some(
                        crate::repo::worktrees::open(&spawn.cwd, asked, &|_| {})
                            .await
                            .map_err(|failed| worktree_failed(&failed))?
                            .1,
                    ),
                    None => None,
                };
                match self.spawn_agent(spawn, prompt, session).await? {
                    Outcome::Opened(term) => Ok(opened(term, made)),
                    other => Ok(other),
                }
            }
            Verb::SendInput { term, input } => {
                let handle = self.session(term)?;
                let agents = inner.worker.agents();
                let expects_agent = inner.agent_terms.lock().contains(&term.session);
                let guard = || may_type(&handle, agents, expects_agent);
                write_input(&handle, &input, guard).await?;
                Ok(Outcome::Done)
            }
            Verb::ReadScreen { term } => {
                Ok(Outcome::Screen(read_screen(&self.session(term)?).await?))
            }
            Verb::ReadOutput { term, since, max_lines } => {
                let (lines, next) = read_output(&self.session(term)?, since, max_lines).await?;
                Ok(Outcome::Output { lines, next })
            }
            Verb::ListCommands { term, since } => {
                Ok(Outcome::Commands(list_commands(&self.session(term)?, since).await?))
            }
            Verb::WaitFor { term, until, timeout_ms } => {
                let handle = self.session(term)?;
                let feed = matches!(until, WaitUntil::AgentNeedsInput).then(|| AgentFeed {
                    heard: inner.heard.subscribe(),
                    agents: inner.worker.shared_agents(),
                });
                let timeout = Duration::from_millis(u64::from(timeout_ms));
                Ok(Outcome::Waited(wait_for(&handle, &until, timeout, feed).await?))
            }
            Verb::AgentStatus { term } => {
                self.session(term)?;
                let row =
                    inner.thread_reads.get().and_then(|reads| reads.at_terminal(term.session));
                Ok(Outcome::Agent(row.as_ref().map(|r| Box::new(TermAgent::of(r)))))
            }
            Verb::Close { term } => {
                self.mine(term.worker)?;
                // A task's thread with no terminal of its own is closed by its seat.
                if inner.worker.get(term.session).is_err()
                    && let Some(threads) = inner.task_threads.get()
                    && threads.close(term.session).await?
                {
                    return Ok(Outcome::Done);
                }
                self.close(term.session).await?;
                Ok(Outcome::Done)
            }
            Verb::ReadFile { worker, path, offset, length } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                let (bytes, size) = blocking(move || read_file(&path, offset, length)).await?;
                Ok(Outcome::File { bytes, offset, size })
            }
            verb @ (Verb::CloneRepo { .. }
            | Verb::BundleBranch { .. }
            | Verb::FetchBundle { .. }
            | Verb::Verify { .. }
            | Verb::Rebase { .. }
            | Verb::TestDiff { .. }
            | Verb::FastForward { .. }
            | Verb::CatchUp { .. }
            | Verb::LandPull { .. }
            | Verb::RemoveWorktree { .. }
            | Verb::DropBranches { .. }) => Box::pin(self.repository(verb)).await,
            Verb::StartThread { worker, start, seat, env, role } => {
                self.mine(worker)?;
                let threads = inner.task_threads.get().cloned().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker starts no task's thread")
                })?;
                let mut start = *start;
                let made = crate::repo::worktrees::enter(&mut start, &|_| {})
                    .await
                    .map_err(|failed| worktree_failed(&failed))?;
                let thread = threads.start(TaskThread { start, seat, env, role }).await?;
                Ok(Outcome::ThreadStarted { thread, worktree: made.map(Box::new) })
            }
            Verb::ListPorts { worker } => {
                self.mine(worker)?;
                let roots = inner.worker.pids().await?;
                let ports = blocking(move || Ok(crate::ports::listening(&roots))).await?;
                Ok(Outcome::Ports(ports))
            }
            Verb::ResizeTerminal { term, size } => {
                let size = term_size(Some(size))?;
                resize(&self.session(term)?, size).await?;
                Ok(Outcome::Done)
            }
            Verb::ListDir { worker, path, max } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                let (entries, total) = blocking(move || list_dir(&path, max)).await?;
                Ok(Outcome::Dir { entries, total })
            }
            Verb::Stat { worker, path } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                blocking(move || stat(&path)).await.map(Outcome::Stat)
            }
            Verb::FsChange { worker, op } => {
                self.mine(worker)?;
                match blocking(move || Ok(crate::fsop::apply(&op))).await? {
                    FsOutcome::Done { path } => Ok(Outcome::FsDone { path }),
                    FsOutcome::Refused(refusal) => Err(refused(&refusal)),
                    FsOutcome::Failed { error } => Err(Failure::new(ErrorCode::Failed, error)),
                }
            }
            Verb::Search { worker, root, query, max_lines } => {
                self.mine(worker)?;
                let root = crate::file::expand_home(Path::new(&root));
                let (files, summary) = search(root, query, max_lines, SEARCH_WITHIN).await?;
                Ok(Outcome::Search { files, summary })
            }
            Verb::ListItems { worker } => {
                self.mine(worker)?;
                Ok(Outcome::Items(inner.items.items()))
            }
            Verb::OpenItem { worker, kind, name } => {
                self.mine(worker)?;
                if matches!(kind, ItemKind::Terminal { .. }) {
                    return Err(Failure::new(
                        ErrorCode::Invalid,
                        "a terminal comes with OpenTerminal, which starts its session",
                    ));
                }
                let item = Item {
                    id: ItemId::new(),
                    kind,
                    name,
                    facts: std::collections::BTreeMap::new(),
                };
                let id = item.id;
                self.change(ItemOp::Add(item))?;
                Ok(Outcome::Item(ItemRef { worker, item: id }))
            }
            Verb::RenameItem { item, name } => {
                self.item(item)?;
                self.change(ItemOp::Rename { id: item.item, name })?;
                Ok(Outcome::Done)
            }
            Verb::RemoveItem { item } => {
                if matches!(self.item(item)?.kind, ItemKind::Terminal { .. }) {
                    return Err(Failure::new(
                        ErrorCode::Invalid,
                        "a terminal's item goes when the terminal closes; use Close",
                    ));
                }
                self.change(ItemOp::Remove(item.item))?;
                Ok(Outcome::Done)
            }
            Verb::ListWindows { worker } => {
                self.mine(worker)?;
                match crate::screen::listing().await {
                    Ok(ScreenEvent::Listing { windows, displays }) => {
                        Ok(Outcome::Screens { windows, displays })
                    }
                    Ok(_other) => Err(unexpected()),
                    Err(e) => Err(Failure::new(ErrorCode::Failed, e.to_string())),
                }
            }
            Verb::ReadThread { of, view, after, hold } => {
                let (thread, reads) = self.thread_of(&of)?;
                let state = reads.state(thread).ok_or_else(|| no_thread(thread))?;
                // Only Claude Code's prompts wait in its terminal unless someone follows it.
                if hold && let Some(session) = state.meta.terminal {
                    inner.conversations.follow(session);
                }
                let read = thread_read::read(&state, inner.id, view, after);
                Ok(Outcome::Thread(Box::new(read)))
            }
            Verb::SendMessage { of, text } => {
                let (thread, reads) = self.thread_of(&of)?;
                let intent = Intent::Send { text, delivery: Delivery::Queue, attachments: vec![] };
                match reads.intent(thread, IntentId::new(), intent) {
                    wire::Outcome::Done | wire::Outcome::Accepted => Ok(Outcome::Done),
                    wire::Outcome::Refused { reason } => {
                        Err(Failure::new(ErrorCode::Failed, reason))
                    }
                    wire::Outcome::Unsupported { cap } => Err(Failure::new(
                        ErrorCode::Unsupported,
                        format!("this thread's agent takes no message from here ({})", cap.0),
                    )),
                    wire::Outcome::Started { .. } | wire::Outcome::SetupFailed { .. } => {
                        Err(unexpected())
                    }
                }
            }
            Verb::RestartWorker { worker } => {
                self.mine(worker)?;
                let restart = inner.restart.get().cloned().ok_or_else(|| {
                    Failure::new(
                        ErrorCode::Unsupported,
                        "this worker runs under no service manager that would start it again; \
                         start it again where it runs",
                    )
                })?;
                tracing::info!("restarting at a client's word");
                // The answer goes out first: the daemon exits a moment after it.
                tokio::spawn(async move {
                    tokio::time::sleep(RESTART_AFTER).await;
                    restart.notify_one();
                });
                Ok(Outcome::Done)
            }
            Verb::AnswerRequest { of, ask, choice, message } => {
                let (thread, reads) = self.thread_of(&of)?;
                let intent = Intent::Answer { ask, choice, message };
                match reads.intent(thread, IntentId::new(), intent) {
                    wire::Outcome::Done | wire::Outcome::Accepted => Ok(Outcome::Done),
                    wire::Outcome::Refused { reason } => {
                        Err(Failure::new(ErrorCode::Failed, reason))
                    }
                    wire::Outcome::Unsupported { cap } => Err(Failure::new(
                        ErrorCode::Unsupported,
                        format!("this thread's agent cannot be answered here ({})", cap.0),
                    )),
                    wire::Outcome::Started { .. } | wire::Outcome::SetupFailed { .. } => {
                        Err(unexpected())
                    }
                }
            }
            Verb::CaptureStill { worker, target } => {
                self.mine(worker)?;
                still::capture(target).await
            }
            Verb::Upload { worker, path, upload, part } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                // The bundle place is the server's to fill, and is made the first time it does.
                let bundles = crate::file::expand_home(Path::new(BUNDLES));
                let into_bundles = path.parent() == Some(bundles.as_path());
                blocking(move || {
                    if into_bundles {
                        std::fs::create_dir_all(&bundles)
                            .map_err(|e| Failure::new(ErrorCode::Failed, e.to_string()))?;
                    }
                    upload::apply(&path, upload, part)
                })
                .await
                .map(|()| Outcome::Done)
            }
            Verb::WakePeer { worker, peer } => {
                self.mine(worker)?;
                let own = blocking(|| Ok(slopty_tailnet::lan::ports())).await?;
                match slopty_tailnet::lan::wake_peer(&own, &peer).await {
                    Ok(_sent) => Ok(Outcome::Done),
                    Err(e) => Err(Failure::new(ErrorCode::Failed, e.to_string())),
                }
            }
        }
    }

    /// Apply an item change as orchestration's and announce it to every client.
    fn change(&self, op: ItemOp) -> Result<(), Failure> {
        let delta = self.inner.items.apply(op, ORCHESTRATOR).map_err(item_failure)?;
        let _sent = self.inner.events.send(WorkerMsg::Items(delta));
        Ok(())
    }

    /// The item `item` names, on this worker.
    fn item(&self, item: ItemRef) -> Result<Item, Failure> {
        self.mine(item.worker)?;
        self.inner.items.get(item.item).ok_or_else(|| {
            Failure::new(ErrorCode::UnknownItem, format!("no item {} on this worker", item.item))
        })
    }

    /// The session's summary as a client's list shows it, if the session runs.
    pub async fn summary(&self, session: SessionId) -> Option<SessionSummary> {
        self.inner.worker.summary(session).await
    }

    /// Open a session and announce it the way a client's open is announced: the summary to
    /// every client, and a terminal item made `by` the given client.
    ///
    /// # Errors
    ///
    /// ptyd refusing the spawn, or the session failing to start.
    pub async fn open(
        &self,
        req: &OpenSession,
        by: ClientId,
    ) -> Result<SessionHandle, WorkerError> {
        self.open_as(None, req, by).await
    }

    /// [`Self::open`], under the id `id` names when it names one.
    async fn open_as(
        &self,
        id: Option<SessionId>,
        req: &OpenSession,
        by: ClientId,
    ) -> Result<SessionHandle, WorkerError> {
        let inner = &self.inner;
        let handle = match id {
            Some(id) => inner.worker.open_as(id, req).await?,
            None => inner.worker.open(req).await?,
        };
        // A fresh terminal: output matching starts at its first byte, so a program's banner
        // printed before the first wait still counts.
        handle.mark_if_unset(Position { line: 0, col: 0, epoch: 0 });
        let session = handle.id();
        if let Some(summary) = inner.worker.summary(session).await {
            let _sent = inner.events.send(WorkerMsg::SessionChanged(summary));
        }
        if let Some(delta) = inner.items.ensure_terminal(session, by) {
            let _sent = inner.events.send(WorkerMsg::Items(delta));
        }
        Ok(handle)
    }

    /// Close a session and announce it the way a client's close is announced.
    async fn close(&self, session: SessionId) -> Result<(), WorkerError> {
        let inner = &self.inner;
        inner.worker.close(session).await?;
        inner.agent_terms.lock().remove(&session);
        inner.worker.agents().forget(session);
        inner.conversations.forget(session);
        let reason = CloseReason::Requested;
        let _sent = inner.events.send(WorkerMsg::SessionClosed { session, reason });
        for delta in inner.items.remove_session(session, ORCHESTRATOR) {
            let _sent = inner.events.send(WorkerMsg::Items(delta));
        }
        Ok(())
    }

    /// Start the agent's TUI, under `chosen` when the caller chose the id; with a prompt, type
    /// it once the agent's hooks say it is at its prompt ([`type_when_ready`]). It gets the
    /// hook relay, Slopty's tools and the mod ([`Launch`]), a conversation id of its own
    /// (`--session-id`, [`slopty_agent::resume::with_session_id`]), and unless the person
    /// allowed it flags that loosen permissions, a lock on the mode that asks none
    /// ([`slopty_agent::hooks::held_to_asking`]). The caller's own settings, MCP servers,
    /// arguments and variables are kept, and its variables win. The server it asks for tools
    /// is the session's own `SLOPTY_SERVER`.
    async fn spawn_agent(
        &self,
        spawn: Spawn,
        prompt: Option<String>,
        chosen: Option<SessionId>,
    ) -> Result<Outcome, Failure> {
        // Subscribed before the spawn: the agent may report itself ready before the open
        // returns.
        let heard = self.inner.heard.subscribe();
        let Spawn { cwd, args, env, size, permission_flags } = spawn;
        let launch = &self.inner.launch;
        let (args, conversation) = slopty_agent::resume::with_session_id(args);
        let relay = launch.relay.as_deref().map(|relay| relay.to_string_lossy().into_owned());
        let dir = crate::file::expand_home(Path::new(&cwd));
        let args = blocking({
            let relay = relay.clone();
            move || {
                let args = match &relay {
                    Some(relay) => slopty_agent::hooks::with_relay(args, relay, &dir),
                    None => args,
                };
                Ok(if permission_flags {
                    args
                } else {
                    slopty_agent::hooks::held_to_asking(args, &dir)
                })
            }
        })
        .await?;
        let args = match &relay {
            Some(relay) => slopty_agent::hooks::with_mcp(args, relay),
            None => args,
        };
        // The mod's variables go last, so a request's cannot silence or redirect it.
        let (args, env) = match &launch.claude_mod {
            Some(installed) => {
                (installed.args(args), env.into_iter().chain(installed.agent_env()).collect())
            }
            None => (args, env),
        };
        let req = OpenSession {
            size,
            cwd: Some(cwd),
            command: std::iter::once("claude".to_owned()).chain(args).collect(),
            env,
            title: None,
            attach: false,
        };
        let handle = self.open_as(chosen, &req, ORCHESTRATOR).await?;
        let session = handle.id();
        self.inner.agent_terms.lock().insert(session);
        tracing::info!(%session, conversation = ?conversation, "agent started");
        if let Some(prompt) = prompt {
            let agents = self.inner.worker.shared_agents();
            tokio::spawn(type_when_ready(handle, prompt, AgentFeed { heard, agents }));
        }
        Ok(Outcome::Opened(TermRef { worker: self.inner.id, session }))
    }

    /// The verbs on a repository the server asks for around a task: a clone, a branch
    /// bundled, a bundle fetched. Apart, and boxed, since their futures are large and rare.
    async fn repository(&self, verb: Verb) -> Result<Outcome, Failure> {
        match verb {
            Verb::CloneRepo { worker, url, clone } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let progress = self.inner.clone_progress.clone();
                let told = move |p: crate::repo::cloning::Progress| {
                    let _no_link = progress.send((clone, p));
                };
                let home = slopty_platform::dirs::home();
                let (path, repo) = self
                    .inner
                    .cloner
                    .clone_repo(git, &url, &home, told)
                    .await
                    .map_err(|why| Failure::new(ErrorCode::Failed, why))?;
                let at = path.clone();
                blocking(move || {
                    crate::repo::cloning::trust(&home, &at);
                    Ok(())
                })
                .await?;
                Ok(Outcome::Cloned { path: path.to_string_lossy().into_owned(), repo })
            }
            Verb::BundleBranch { worker, repo, branch, target } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let dir = crate::file::expand_home(Path::new(BUNDLES));
                let made = crate::repo::bundle::bundle_branch(
                    git,
                    &repo,
                    &branch,
                    target.as_deref(),
                    &dir,
                )
                .await
                .map_err(bundle_failure)?;
                Ok(Outcome::Bundle(Box::new(BranchBundle {
                    path: made.path.to_string_lossy().into_owned(),
                    name: made.name,
                    size: made.size,
                    digest: made.digest,
                    head: made.head,
                    base: made.base,
                })))
            }
            Verb::FetchBundle { worker, repo, bundle, branch, into, head } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let dir = crate::file::expand_home(Path::new(BUNDLES));
                let want = crate::repo::bundle::Fetch {
                    name: &bundle,
                    branch: &branch,
                    into: &into,
                    head: &head,
                };
                let head = crate::repo::bundle::fetch_bundle(git, &repo, &dir, want)
                    .await
                    .map_err(bundle_failure)?;
                Ok(Outcome::Fetched { branch: into, head })
            }
            Verb::Verify { worker, repo, worktree, head, target, command, session, title } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let places = crate::file::expand_home(Path::new(VERIFY_PLACES));
                let place = crate::repo::verify::place(&places, &worktree)
                    .map_err(|f| verify_failure(&f))?;
                let _choosing = self.choosing(Some(session)).await;
                let made = crate::repo::verify::checkout(git, &repo, &place, &head, &target)
                    .await
                    .map_err(|f| verify_failure(&f))?;
                let req = OpenSession {
                    size: ORCHESTRATED_SIZE,
                    cwd: Some(made.path.to_string_lossy().into_owned()),
                    command: crate::repo::verify::command_line(&command),
                    env: vec![
                        ("SLOPTY_VERIFY_HEAD".to_owned(), made.head.clone()),
                        ("SLOPTY_VERIFY_BASE".to_owned(), made.base.clone()),
                    ],
                    title: Some(title),
                    attach: false,
                };
                let handle = self.open_as(Some(session), &req, ORCHESTRATOR).await?;
                let term = TermRef { worker: self.inner.id, session: handle.id() };
                Ok(Outcome::Verifying { term, head: made.head, base: made.base })
            }
            Verb::Rebase { worker, repo, worktree, head, onto, trailers, verified } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let places = crate::file::expand_home(Path::new(VERIFY_PLACES));
                let place = crate::repo::verify::place(&places, &worktree)
                    .map_err(|f| verify_failure(&f))?;
                let commits = (head.as_str(), onto.as_str());
                let made = crate::repo::verify::rebase(
                    git,
                    &repo,
                    &place,
                    commits,
                    &trailers,
                    verified.as_deref(),
                )
                .await
                .map_err(|f| verify_failure(&f))?;
                let crate::repo::verify::Rebased { head, from, onto, verified } = made;
                Ok(Outcome::Rebased { head, from, onto, verified })
            }
            Verb::TestDiff { worker, repo, head, target, test_paths } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let tests = crate::repo::verify::test_diff(git, &repo, &head, &target, &test_paths)
                    .await
                    .map_err(|f| verify_failure(&f))?;
                Ok(Outcome::TestDiff(tests))
            }
            Verb::FastForward { worker, repo, target, from, to, push } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let moved =
                    crate::repo::verify::fast_forward(git, &repo, &target, &from, &to, push)
                        .await
                        .map_err(|f| verify_failure(&f))?;
                let crate::repo::verify::Moved { head, pushed, push_failed } = moved;
                Ok(Outcome::FastForwarded { head, pushed, push_failed })
            }
            Verb::CatchUp { worker, repo, target } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let moved = crate::repo::verify::catch_up(git, &repo, &target)
                    .await
                    .map_err(|f| verify_failure(&f))?;
                let crate::repo::verify::Moved { head, pushed, push_failed } = moved;
                Ok(Outcome::FastForwarded { head, pushed, push_failed })
            }
            Verb::LandPull { worker, repo, head, branch, target, title, body } => {
                self.mine(worker)?;
                let programs = crate::repo::commit::Programs::here().await;
                let repo = crate::file::expand_home(Path::new(&repo));
                let landing =
                    crate::repo::pull::Landing { head: &head, branch: &branch, target: &target };
                let landed = crate::repo::pull::land(&programs, &repo, landing, (&title, &body));
                let (number, url) = programs
                    .scope(landed)
                    .await
                    .map_err(|why| Failure::new(ErrorCode::Failed, why))?;
                Ok(Outcome::PullOpened { number, url })
            }
            Verb::RemoveWorktree { worker, worktree, landed } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let worktree = crate::file::expand_home(Path::new(&worktree));
                let places = crate::file::expand_home(Path::new(VERIFY_PLACES));
                // A project's verify checkout, the server's own: it goes by force.
                if let Some(name) = worktree
                    .strip_prefix(&places)
                    .ok()
                    .and_then(|rest| rest.to_str())
                    .filter(|name| crate::repo::verify::place(&places, name).is_ok())
                {
                    let place = places.join(name);
                    crate::repo::verify::drop_checkout(git, &place)
                        .await
                        .map_err(|failed| verify_failure(&failed))?;
                    return Ok(Outcome::WorktreeRemoved { branch: None, branch_removed: false });
                }
                let cwds: Vec<PathBuf> = self
                    .inner
                    .worker
                    .summaries()
                    .await
                    .into_iter()
                    .filter(|s| matches!(s.state, SessionState::Running))
                    .filter_map(|s| s.cwd)
                    .map(|cwd| crate::file::expand_home(Path::new(&cwd)))
                    .collect();
                let removed = crate::repo::worktrees::remove(git, &worktree, &landed, &cwds)
                    .await
                    .map_err(|failed| worktree_failed(&failed))?;
                let crate::repo::worktrees::Removed { branch, branch_removed } = removed;
                Ok(Outcome::WorktreeRemoved { branch, branch_removed })
            }
            Verb::DropBranches { worker, repo, branches } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                crate::repo::worktrees::drop_branches(git, &repo, &branches)
                    .await
                    .map_err(|failed| worktree_failed(&failed))?;
                Ok(Outcome::Done)
            }
            _ => Err(Failure::new(ErrorCode::Unsupported, "not a repository verb")),
        }
    }

    /// How the clones the server asked for go, as they move: the server's number for each,
    /// and its progress.
    #[must_use]
    pub fn clone_progress(&self) -> broadcast::Receiver<(u64, crate::repo::cloning::Progress)> {
        self.inner.clone_progress.subscribe()
    }

    /// `worker` is this one.
    fn mine(&self, worker: WorkerId) -> Result<(), Failure> {
        if worker == self.inner.id {
            Ok(())
        } else {
            Err(Failure::new(
                ErrorCode::UnknownWorker,
                format!("this is worker {}, not {worker}", self.inner.id),
            ))
        }
    }

    /// Held while a start under the id `chosen` names looks for it and opens it; nothing when
    /// the start chose none.
    async fn choosing(&self, chosen: Option<SessionId>) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        match chosen {
            Some(_) => Some(self.inner.choosing.lock().await),
            None => None,
        }
    }

    /// The chosen id, when this worker runs a session under it already.
    fn running(&self, chosen: Option<SessionId>) -> Option<SessionId> {
        chosen.filter(|id| self.inner.worker.get(*id).is_ok())
    }

    /// The live session `term` names, on this worker.
    fn session(&self, term: TermRef) -> Result<SessionHandle, Failure> {
        self.mine(term.worker)?;
        self.inner.worker.get(term.session).map_err(|_gone| {
            Failure::new(
                ErrorCode::UnknownTerminal,
                format!("no terminal {} on this worker", term.session),
            )
        })
    }
}

/// Where and how an agent starts.
struct Spawn {
    cwd: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    size: TermSize,
    /// It may be given flags and modes that loosen its permissions.
    permission_flags: bool,
}

/// A terminal opened, in the worktree `made` when it was asked one.
fn opened(term: TermRef, made: Option<slopty_proto::agent::Worktree>) -> Outcome {
    match made {
        Some(worktree) => Outcome::OpenedIn { term, worktree: Box::new(worktree) },
        None => Outcome::Opened(term),
    }
}

/// A worktree that could not be made or reopened, as the verb's failure.
fn worktree_failed(failed: &crate::repo::worktrees::Failed) -> Failure {
    use crate::repo::worktrees::Failed;
    let code = match failed {
        Failed::NotOne(_) => ErrorCode::Invalid,
        Failed::Busy(_) | Failed::Uncommitted(_) => ErrorCode::Conflict,
        Failed::Other(_) => ErrorCode::Failed,
        // The board and the orchestrator read why: the script's last lines go with it.
        Failed::Setup(setup) | Failed::Archive(setup) => {
            let said = setup.setup.tail.join("\n");
            return Failure::new(ErrorCode::Failed, format!("{failed}\n{said}"));
        }
    };
    Failure::new(code, failed.to_string())
}

/// The session size a verb asks for, [`ORCHESTRATED_SIZE`] when it names none.
fn term_size(size: Option<Size>) -> Result<TermSize, Failure> {
    let Some(size) = size else { return Ok(ORCHESTRATED_SIZE) };
    let fits = (MIN_SIZE.cols..=MAX_SIZE.cols).contains(&size.cols)
        && (MIN_SIZE.rows..=MAX_SIZE.rows).contains(&size.rows);
    if !fits {
        return Err(Failure::new(
            ErrorCode::Invalid,
            format!(
                "{}x{} is not a terminal size; cols {}..={}, rows {}..={}",
                size.cols, size.rows, MIN_SIZE.cols, MAX_SIZE.cols, MIN_SIZE.rows, MAX_SIZE.rows
            ),
        ));
    }
    Ok(TermSize { cols: size.cols, rows: size.rows, ..ORCHESTRATED_SIZE })
}

/// Resize a session no client shows.
///
/// A session's size is its driver's, and a client showing it drives it to fit its window, so a
/// session with viewers is refused rather than fought over. The check and the resize are one
/// step on the session's actor, so a client attaching meanwhile keeps its seat.
///
/// # Errors
///
/// [`ErrorCode::Failed`] while a client shows the session; [`ErrorCode::UnknownTerminal`] when
/// it is gone.
pub async fn resize(handle: &SessionHandle, size: TermSize) -> Result<(), Failure> {
    let viewers = handle.resize_unviewed(size).await?;
    if viewers > 0 {
        return Err(Failure::new(
            ErrorCode::Failed,
            format!(
                "{viewers} client(s) show this terminal and size it to their window; only a \
                 terminal no client shows can be resized"
            ),
        ));
    }
    Ok(())
}

/// Whether orchestration may type into the terminal `handle` reaches now, read at the moment
/// of the write.
///
/// A terminal with an agent in it (the table holds one, or orchestration started it as an
/// agent's: `expects_agent`) takes input only while the agent can:
///
/// - not while it waits on a person: a permission, a question, an elicitation
///   ([`ErrorCode::AwaitsPerson`]; the person answers, never an agent);
/// - not while a person has typed into it and not sent it ([`SessionHandle::draft_pending`];
///   [`ErrorCode::AwaitsPerson`]), which would be merged into their line;
/// - not before a hook of its session has spoken ([`ErrorCode::AgentNotReady`]): until then a
///   dialog of its own may be up, and typing would answer it;
/// - not once it has ended ([`ErrorCode::AgentExited`]): the shell below it would run the text.
///
/// A terminal with no agent takes anything.
///
/// # Errors
///
/// The refusal, as above.
pub fn may_type(
    handle: &SessionHandle,
    agents: &dyn Agents,
    expects_agent: bool,
) -> Result<(), Failure> {
    let session = handle.id();
    let agent = agents.status(session).filter(|a| a.status != AgentStatus::None);
    let ended = agents.ended(session);
    if agent.is_none() && !expects_agent && !ended {
        return Ok(());
    }
    // A program that exited may leave its last status behind: nothing reads the input now.
    let exited = handle.activity().borrow().exited;
    let Some(agent) = agent.filter(|_| !exited) else {
        if exited || ended {
            return Err(Failure::new(
                ErrorCode::AgentExited,
                "the agent in this terminal has exited, so its shell would get the input; \
                 start the agent again or close the terminal",
            ));
        }
        return Err(not_ready());
    };
    if let AgentStatus::Blocked(why) = &agent.status {
        let what = match why {
            BlockReason::Permission { .. } => Some("a permission prompt"),
            BlockReason::Question => Some("a question"),
            BlockReason::Elicitation => Some("an MCP server's question"),
            BlockReason::IdlePrompt => None,
        };
        if let Some(what) = what {
            return Err(Failure::new(
                ErrorCode::AwaitsPerson,
                format!(
                    "the agent waits on {what}, which is the person's to answer; wait for them"
                ),
            ));
        }
    }
    if agent.source != AgentSource::Hook {
        return Err(not_ready());
    }
    if handle.draft_pending() {
        return Err(Failure::new(
            ErrorCode::AwaitsPerson,
            "a person is typing into this agent's prompt; wait until they send it",
        ));
    }
    Ok(())
}

fn not_ready() -> Failure {
    Failure::new(
        ErrorCode::AgentNotReady,
        "the agent has not reported through its hooks yet, so a dialog of its own may be up; \
         wait for it (wait_for agent_needs_input) and try again",
    )
}

/// Whether what is kept for the agent in `handle`'s terminal may be posted to wake it now.
///
/// What is kept is reports, and the person's or the orchestrator's words through the server. It
/// may go as [`may_type`] says, and not while the person's own stop of its last turn stands. A
/// post starts a turn in an idle agent, so one the person just stopped would be started again
/// in nobody's name; what is kept waits for its next hook, which is the person's next prompt,
/// and rides with their turn.
///
/// # Errors
///
/// [`may_type`]'s refusals, and [`ErrorCode::AwaitsPerson`] after the person's stop.
pub fn may_deliver(handle: &SessionHandle, agents: &dyn Agents) -> Result<(), Failure> {
    may_type(handle, agents, true)?;
    if agents.interrupted(handle.id()) {
        return Err(Failure::new(
            ErrorCode::AwaitsPerson,
            "the person stopped this agent; what is kept for it goes with their next prompt",
        ));
    }
    Ok(())
}

/// Whether an agent is at its prompt, by its hooks' word: at rest after its `SessionStart` or
/// a turn. A weaker signal (its process, its title, its transcript) may come before its TUI
/// takes input, or while a dialog of its own is up.
fn at_its_prompt(agent: &SessionAgent) -> bool {
    agent.source == AgentSource::Hook
        && matches!(
            agent.status,
            AgentStatus::Idle
                | AgentStatus::Done
                | AgentStatus::Failed { .. }
                | AgentStatus::Waiting { .. }
                | AgentStatus::Blocked(BlockReason::IdlePrompt)
        )
}

/// Type a spawned agent's first prompt once its hooks say it is at its prompt and nothing
/// stands in the way ([`may_type`]), trying again at each report of the agent until
/// [`PROMPT_READY_WITHIN`] has passed. Never typed blind: when the agent never gets there, or
/// has ended, the prompt is left unsent and the log says why.
async fn type_when_ready(handle: SessionHandle, prompt: String, mut feed: AgentFeed) {
    let session = handle.id();
    let Some(deadline) = tokio::time::Instant::now().checked_add(PROMPT_READY_WITHIN) else {
        return;
    };
    let mut activity = handle.activity();
    let mut held = not_ready();
    loop {
        if feed.agents.status(session).is_some_and(|a| at_its_prompt(&a)) {
            match may_type(&handle, &*feed.agents, true) {
                Ok(()) => break,
                Err(refused) if refused.code == ErrorCode::AgentExited => {
                    tracing::warn!(%session, why = %refused.message, "first prompt left unsent");
                    return;
                }
                Err(refused) => held = refused,
            }
        }
        let reported = async {
            loop {
                match feed.heard.recv().await {
                    Ok(ev) if ev.session == session => return true,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => return true,
                    Err(broadcast::error::RecvError::Closed) => return false,
                }
            }
        };
        let exited = async {
            while !activity.borrow_and_update().exited {
                if activity.changed().await.is_err() {
                    return;
                }
            }
        };
        let woke = tokio::select! {
            reported = tokio::time::timeout_at(deadline, reported) => reported,
            () = exited => Ok(false),
        };
        match woke {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(%session, "first prompt left unsent: the agent's terminal ended");
                return;
            }
            Err(_elapsed) => {
                tracing::warn!(
                    %session, within = ?PROMPT_READY_WITHIN, why = %held.message,
                    "first prompt left unsent: the agent never became ready for it"
                );
                return;
            }
        }
    }
    tracing::info!(%session, "typing the agent's first prompt");
    if let Err(e) =
        handle.request(ORCHESTRATOR, TermRequest::Paste { text: prompt, confirmed: true })
    {
        tracing::warn!(%session, error = %e, "first prompt not typed");
        return;
    }
    tokio::time::sleep(SUBMIT_PAUSE).await;
    let enter = Input::Keys(vec!["enter".to_owned()]);
    let guard = || may_type(&handle, &*feed.agents, true);
    if let Err(e) = write_input(&handle, &enter, guard).await {
        tracing::warn!(%session, error = %e.message, "first prompt typed but not submitted");
    }
}

/// Type into a session the way a client's input path does.
///
/// Text goes as the committed text a keyboard's input method sends (raw bytes) with each newline an
/// Enter press through the key encoder; a paste through the paste encoder (bracketed when the
/// program asked); named keys through the key encoder, all of them checked before any is sent. The
/// session's mark is set where the cursor stands before the input, if orchestration never set it,
/// so a wait for the input's output finds it however soon it came.
///
/// # Errors
///
/// [`ErrorCode::Invalid`] for a key name that does not parse; [`ErrorCode::UnknownTerminal`]
/// when the session is gone.
pub async fn send_input(handle: &SessionHandle, input: &Input) -> Result<(), Failure> {
    write_input(handle, input, || Ok(())).await
}

/// [`send_input`], with `guard` asked right before the first byte is queued, after everything
/// the write waits for: what it refuses is refused as the input would have landed.
///
/// # Errors
///
/// As [`send_input`], and whatever `guard` refuses.
pub async fn write_input(
    handle: &SessionHandle,
    input: &Input,
    guard: impl Fn() -> Result<(), Failure>,
) -> Result<(), Failure> {
    let requests: Vec<TermRequest> = match input {
        Input::Text(text) => text_requests(text),
        Input::Paste(text) => vec![TermRequest::Paste { text: text.clone(), confirmed: true }],
        Input::Keys(names) => names
            .iter()
            .map(|name| keys::parse(name, 0).map(TermRequest::Key))
            .collect::<Result<_, _>>()
            .map_err(|message| Failure::new(ErrorCode::Invalid, message))?,
    };
    guard()?;
    if handle.mark().is_none() {
        handle.mark_if_unset(position(handle).await?);
    }
    guard()?;
    for req in requests {
        handle.request(ORCHESTRATOR, req)?;
    }
    Ok(())
}

/// Where the session's cursor is.
async fn position(handle: &SessionHandle) -> Result<Position, Failure> {
    match handle.read(Read::Position).await? {
        Text::Position(at) => Ok(at),
        _other => Err(unexpected()),
    }
}

/// Text as raw runs and Enter presses: `\n`, `\r` and `\r\n` are each one Enter.
fn text_requests(text: &str) -> Vec<TermRequest> {
    let enter = || keys::parse("enter", 0).map(TermRequest::Key).ok();
    let mut out = Vec::new();
    let mut run = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' || c == '\n' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if !run.is_empty() {
                out.push(TermRequest::Raw(std::mem::take(&mut run).into_bytes()));
            }
            out.extend(enter());
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        out.push(TermRequest::Raw(run.into_bytes()));
    }
    out
}

/// The screen as drawn now.
///
/// # Errors
///
/// [`ErrorCode::UnknownTerminal`] when the session is gone; [`ErrorCode::Failed`] when the
/// engine fails.
pub async fn read_screen(handle: &SessionHandle) -> Result<Screen, Failure> {
    let Text::Screen { screen, title, cwd } = handle.read(Read::Screen).await? else {
        return Err(unexpected());
    };
    let lines =
        (screen.first..).zip(screen.rows).map(|(index, text)| Line { index, text }).collect();
    Ok(Screen {
        lines,
        cursor: screen.cursor,
        title: title.unwrap_or_default(),
        cwd,
        alternate: screen.alternate,
    })
}

/// At most `max_lines` (and [`MAX_OUTPUT_LINES`]) lines from `since` on, and where to ask
/// from next.
///
/// # Errors
///
/// As [`read_screen`].
pub async fn read_output(
    handle: &SessionHandle,
    since: Option<u64>,
    max_lines: u32,
) -> Result<(Vec<Line>, u64), Failure> {
    let read = Read::Output { since, max: max_lines.min(MAX_OUTPUT_LINES) };
    let Text::Output(text) = handle.read(read).await? else { return Err(unexpected()) };
    let next = text.next();
    let lines = (text.first..).zip(text.lines).map(|(index, text)| Line { index, text }).collect();
    Ok((lines, next))
}

/// The OSC 133 command blocks from `since` on.
///
/// # Errors
///
/// As [`read_screen`].
pub async fn list_commands(
    handle: &SessionHandle,
    since: Option<u64>,
) -> Result<Vec<Command>, Failure> {
    let Text::Commands(blocks) = handle.read(Read::Commands { since }).await? else {
        return Err(unexpected());
    };
    Ok(blocks
        .into_iter()
        .map(|b| Command {
            line: b.command,
            prompt_line: b.prompt_line,
            output: b.output,
            exit: b.exit.filter(|_| b.finished).map(i32::from),
        })
        .collect())
}

/// An item change the registry refused: a missing item, or a bad name, path, address or note.
fn item_failure(e: WorkerError) -> Failure {
    match e {
        WorkerError::NoSuchItem => Failure::new(ErrorCode::UnknownItem, e.to_string()),
        other => Failure::new(ErrorCode::Invalid, other.to_string()),
    }
}

fn unexpected() -> Failure {
    Failure::new(ErrorCode::Failed, "the session answered a different read")
}

/// Search the files under `root` on the blocking pool, for `within` at most. Dropping the
/// future (the server's link went, or its task was aborted) stops the walk at its next file.
///
/// # Errors
///
/// `Invalid` when the root is not a folder or the query does not parse.
pub async fn search(
    root: PathBuf,
    query: slopty_proto::search::SearchQuery,
    max_lines: u32,
    within: Duration,
) -> Result<(Vec<slopty_proto::search::FileHits>, slopty_proto::search::SearchSummary), Failure> {
    let stop = crate::search::StopOnDrop::default();
    let cancel = stop.flag();
    let found = blocking(move || {
        crate::search::collect(&root, &query, max_lines, &cancel, within)
            .map_err(|e| Failure::new(ErrorCode::Invalid, e))
    })
    .await;
    drop(stop);
    found
}

/// Run file work on the blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| Failure::new(ErrorCode::Failed, e.to_string()))?
}

/// An op on the worker's files that was not tried, said plainly for an agent or a person to
/// act on: what stood in the way, and that nothing was touched.
fn refused(refusal: &FsRefusal) -> Failure {
    let (code, message) = match refusal {
        FsRefusal::NotAbsolute { path } => (
            ErrorCode::Invalid,
            format!("{path} is not an absolute path (nor ~/…), or climbs with `..`"),
        ),
        FsRefusal::BadName { name } => (
            ErrorCode::Invalid,
            format!("{name:?} is not one plain name: no `/`, not `.` or `..`, not empty"),
        ),
        FsRefusal::Protected { path } => (
            ErrorCode::Forbidden,
            format!(
                "{path} holds others' work (a root, a volume, the home or a folder above it), so                  it is never moved or trashed"
            ),
        ),
        FsRefusal::Clash { path } => (
            ErrorCode::Conflict,
            format!("something is already at {path}; nothing is ever replaced, so pick another"),
        ),
        FsRefusal::Missing { path } => (ErrorCode::Invalid, format!("nothing is at {path}")),
        FsRefusal::IntoItself => (
            ErrorCode::Invalid,
            "a folder cannot move into itself or a folder inside it".to_owned(),
        ),
        FsRefusal::OtherVolume => (
            ErrorCode::Unsupported,
            "the destination is on another volume, and a move only renames; copy it instead"
                .to_owned(),
        ),
        FsRefusal::NoTrash => (
            ErrorCode::Unsupported,
            "its volume keeps no trash the worker can use, so it was left where it is".to_owned(),
        ),
        FsRefusal::Changed { now } => (
            ErrorCode::Conflict,
            format!(
                "the file changed since the version the new contents were made from (it is {} \
                 bytes now, written at {} ms); read it again and make the change over that",
                now.size,
                now.modified_ms.as_millis()
            ),
        ),
    };
    Failure::new(code, format!("refused, nothing was touched: {message}"))
}

/// A thread this worker does not hold.
fn no_thread(thread: ThreadId) -> Failure {
    Failure::new(ErrorCode::Invalid, format!("this worker holds no thread {thread}"))
}

fn io_failure(path: &Path, e: &std::io::Error) -> Failure {
    Failure::new(ErrorCode::Failed, format!("{}: {e}", path.display()))
}

/// The bytes of a file from `offset` on, `length` of them or the rest, [`MAX_FILE_BYTES`] at
/// most; and the file's size.
fn read_file(path: &Path, offset: u64, length: Option<u64>) -> Result<(Vec<u8>, u64), Failure> {
    // Non-blocking, so opening a named pipe answers at once instead of waiting for a writer;
    // what was opened is then looked at, not the path, which could change in between.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| io_failure(path, &e))?;
    let meta = file.metadata().map_err(|e| io_failure(path, &e))?;
    if meta.is_dir() {
        return Err(Failure::new(ErrorCode::Failed, format!("{} is a directory", path.display())));
    }
    if !meta.is_file() {
        return Err(Failure::new(
            ErrorCode::Failed,
            format!("{} is not a regular file", path.display()),
        ));
    }
    let size = meta.len();
    let want = if let Some(length) = length {
        length.min(MAX_FILE_BYTES)
    } else {
        let rest = size.saturating_sub(offset);
        if rest > MAX_FILE_BYTES {
            return Err(Failure::new(
                ErrorCode::Failed,
                format!(
                    "{} is {size} bytes, and one read returns at most {MAX_FILE_BYTES} (8 MiB); \
                     read it in parts with offset and length",
                    path.display()
                ),
            ));
        }
        rest
    };
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset)).map_err(|e| io_failure(path, &e))?;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(want.min(size)).unwrap_or(0));
    // Capped here too: the file may have grown since the stat.
    (&mut file).take(want).read_to_end(&mut bytes).map_err(|e| io_failure(path, &e))?;
    Ok((bytes, size))
}

/// The first `max` entries of a directory by name ([`MAX_DIR_ENTRIES`] at most), and how many
/// it holds.
fn list_dir(path: &Path, max: u32) -> Result<(Vec<DirEntry>, u32), Failure> {
    first_entries(path, max, |entry| std::fs::symlink_metadata(entry))
}

/// [`list_dir`], with `look` reading an entry's metadata: only the names are gathered from the
/// whole directory ([`listing::first`]), the first `max` of them kept, and only those looked at.
fn first_entries(
    path: &Path,
    max: u32,
    mut look: impl FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
) -> Result<(Vec<DirEntry>, u32), Failure> {
    let keep = usize::try_from(max.min(MAX_DIR_ENTRIES)).unwrap_or(usize::MAX);
    let (names, total) = listing::first(path, keep, |_| ()).map_err(|e| io_failure(path, &e))?;
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        // Gone between the listing and the look: it is not in the directory any more.
        let Ok(meta) = look(&path.join(&name)) else { continue };
        entries.push(DirEntry {
            name: name.to_string_lossy().into_owned(),
            kind: listing::kind(meta.file_type()),
            size: meta.len(),
            modified_ms: listing::modified_ms(&meta),
        });
    }
    Ok((entries, total))
}

/// What is at `path`, following a symbolic link; `None` when nothing is.
fn stat(path: &Path) -> Result<Option<FileStat>, Failure> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_failure(path, &e)),
    };
    Ok(Some(FileStat {
        kind: listing::kind(meta.file_type()),
        size: meta.len(),
        modified_ms: listing::modified_ms(&meta),
        mode: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777,
    }))
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::orchestration::FileKind;

    use super::*;

    #[test]
    fn text_is_raw_runs_and_enter_presses() {
        let reqs = text_requests("echo hi\nls\r\n\rx");
        let shape: Vec<String> = reqs
            .iter()
            .map(|r| match r {
                TermRequest::Raw(b) => String::from_utf8_lossy(b).into_owned(),
                TermRequest::Key(k) => format!("<{:?}>", k.code),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(shape, ["echo hi", "<Enter>", "ls", "<Enter>", "<Enter>", "x"]);
    }

    fn whole(path: &Path) -> Result<Vec<u8>, Failure> {
        read_file(path, 0, None).map(|(bytes, _size)| bytes)
    }

    #[test]
    fn files_are_read_whole_up_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"one").unwrap();
        assert_eq!(whole(&path).unwrap(), b"one");

        let big = dir.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = whole(&big).unwrap_err();
        assert!(err.message.contains("offset and length"), "{err:?}");
        assert_eq!(whole(dir.path()).unwrap_err().code, ErrorCode::Failed);
    }

    /// A range reads from its offset, a length past the end stops at the end, and every read
    /// reports the whole file's size; a file over the cap is read in parts, each at most the
    /// cap.
    #[test]
    fn a_file_is_read_in_ranges_with_its_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("digits");
        std::fs::write(&path, b"0123456789").unwrap();
        assert_eq!(read_file(&path, 3, Some(4)).unwrap(), (b"3456".to_vec(), 10));
        assert_eq!(read_file(&path, 8, None).unwrap(), (b"89".to_vec(), 10));
        assert_eq!(read_file(&path, 8, Some(100)).unwrap(), (b"89".to_vec(), 10));
        assert_eq!(read_file(&path, 20, Some(5)).unwrap(), (Vec::new(), 10), "past the end");

        let big = dir.path().join("big.bin");
        std::fs::File::create(&big).unwrap().set_len(MAX_FILE_BYTES + 3).unwrap();
        let (first, size) = read_file(&big, 0, Some(u64::MAX)).unwrap();
        assert_eq!((first.len() as u64, size), (MAX_FILE_BYTES, MAX_FILE_BYTES + 3));
        let (rest, _size) = read_file(&big, MAX_FILE_BYTES, None).unwrap();
        assert_eq!(rest.len(), 3, "the rest fits");
    }

    /// Entries come by name with their kind, size and time, up to `max`, with the count of all
    /// of them; a missing path is an error, a missing stat is `None`.
    #[test]
    fn a_directory_lists_by_name_and_a_path_stats() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), b"hello").unwrap();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::os::unix::fs::symlink("b.txt", dir.path().join("c")).unwrap();
        let (entries, total) = list_dir(dir.path(), 10).unwrap();
        let shape: Vec<(&str, FileKind)> =
            entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            shape,
            [("a", FileKind::Dir), ("b.txt", FileKind::File), ("c", FileKind::Symlink)]
        );
        assert_eq!(total, 3);
        assert_eq!(entries[1].size, 5);
        assert!(
            entries[1].modified_ms > WallMs::from_millis(1_700_000_000_000),
            "{:?}",
            entries[1]
        );
        let (first, total) = list_dir(dir.path(), 1).unwrap();
        assert_eq!((first.len(), total), (1, 3), "bounded, with the whole count");
        assert_eq!(list_dir(&dir.path().join("nope"), 10).unwrap_err().code, ErrorCode::Failed);

        let linked = stat(&dir.path().join("c")).unwrap().unwrap();
        assert_eq!((linked.kind, linked.size), (FileKind::File, 5), "a link is followed");
        std::fs::set_permissions(
            dir.path().join("a"),
            std::os::unix::fs::PermissionsExt::from_mode(0o750),
        )
        .unwrap();
        let a = stat(&dir.path().join("a")).unwrap().unwrap();
        assert_eq!((a.kind, a.mode), (FileKind::Dir, 0o750));
        assert_eq!(stat(&dir.path().join("nope")).unwrap(), None);
    }

    /// A named pipe (or any other file that is not a regular one) is refused at once: opening
    /// one for reading waits for a writer, and would hold a blocking thread for good.
    #[test]
    fn a_named_pipe_is_refused_not_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let (done, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(read_file(&fifo, 0, None)));
        let read = answer.recv_timeout(Duration::from_secs(5)).expect("answered, not blocked");
        let refused = read.unwrap_err();
        assert!(refused.message.contains("not a regular file"), "{refused:?}");
    }

    /// Only the entries answered are looked at: a huge directory costs its names, not a stat
    /// of each, and its count is still whole.
    #[test]
    fn a_directory_is_stat_only_for_the_entries_it_answers() {
        let dir = tempfile::tempdir().unwrap();
        for i in (0..50).rev() {
            std::fs::write(dir.path().join(format!("f{i:02}")), b"").unwrap();
        }
        let looked = std::cell::Cell::new(0);
        let (entries, total) = first_entries(dir.path(), 3, |p| {
            looked.set(looked.get() + 1);
            std::fs::symlink_metadata(p)
        })
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!((names, total), (vec!["f00", "f01", "f02"], 50));
        assert_eq!(looked.get(), 3, "a stat for each entry answered, none for the rest");
    }

    #[test]
    fn a_size_is_checked_and_defaults() {
        assert_eq!(term_size(None).unwrap(), ORCHESTRATED_SIZE);
        let wide = term_size(Some(Size { cols: 200, rows: 50 })).unwrap();
        assert_eq!((wide.cols, wide.rows, wide.metrics), (200, 50, ORCHESTRATED_SIZE.metrics));
        let err = term_size(Some(Size { cols: 0, rows: 50 })).unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        term_size(Some(Size { cols: 80, rows: 5000 })).unwrap_err();
    }
}
