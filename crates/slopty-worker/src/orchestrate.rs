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
//! An agent's conversation and its held permission prompts come through the daemon's
//! [`Conversations`] ([`conversation`]); a still picture from ScreenCaptureKit ([`still`]); a
//! file too large for one reply goes up in parts ([`upload`]).

pub mod conversation;
pub mod idempotency;
pub mod keys;
pub mod still;
pub mod upload;
mod wait;

use std::collections::HashSet;
use std::io::{Read as _, Seek as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use conversation::{Conversations, Sources};
use slopty_core::{ClientId, ItemId, SessionId, WorkerId};
use slopty_engine::ghostty::Position;
use slopty_proto::WorkerMsg;
use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent};
use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};
use slopty_proto::orchestration::{
    BUNDLES, BranchBundle, Command, DirEntry, ErrorCode, FileStat, IdempotencyKey, Input, ItemRef,
    Line, Outcome, Screen, Size, TermRef, Verb, WaitUntil,
};
use slopty_proto::project::VERIFY_PLACES;
use slopty_proto::screen::ScreenEvent;
use slopty_proto::terminal::{CloseReason, OpenSession, SessionSummary, TermRequest, TermSize};
use tokio::sync::broadcast;
pub use wait::{AgentFeed, wait_for};

use crate::session::{Read, SessionHandle, Text};
use crate::{ItemStore, Worker, WorkerError, listing};

/// Who orchestration acts as, for the session actor (its errors go nowhere: the verb's own
/// outcome reports them) and for the item deltas it causes (nobody's echo).
const ORCHESTRATOR: ClientId = ClientId::nil();

/// Who a [`Verb::PointAt`] says pointed, as a client's toast names it.
const POINTER_NAME: &str = "Orchestration";
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
}

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
        Failed::Conflict(_) | Failed::Moved(_) => ErrorCode::Conflict,
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
    /// client broadcast and followed conversations. The agents it starts get what `launch`
    /// says.
    #[must_use]
    pub fn new(
        id: WorkerId,
        worker: Worker,
        items: ItemStore,
        events: broadcast::Sender<WorkerMsg>,
        launch: Launch,
        conversations: Arc<dyn Conversations>,
    ) -> Self {
        let inner = Inner {
            id,
            worker,
            items,
            events,
            launch,
            conversations,
            once: idempotency::Ledger::default(),
            agent_terms: parking_lot::Mutex::default(),
            choosing: tokio::sync::Mutex::default(),
            cloner: crate::repo::cloning::Cloner::default(),
            clone_progress: broadcast::channel(CLONE_PROGRESS).0,
        };
        Self { inner: Arc::new(inner) }
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
            | Verb::TaskReview { .. }
            | Verb::ProjectDelete { .. }
            | Verb::TaskStart { .. }
            | Verb::TaskTell { .. }
            | Verb::ProjectNeeds { .. }
            | Verb::ProjectList
            | Verb::ProjectStatus { .. }
            | Verb::TaskCreate { .. }
            | Verb::TaskClaim { .. }
            | Verb::TaskUpdate { .. }
            | Verb::TaskAssign { .. }
            | Verb::TaskSpawn { .. }
            | Verb::PlacementSuggest { .. }
            | Verb::WorkerFacts { .. }
            | Verb::TaskGet { .. }
            | Verb::TaskReport { .. }
            | Verb::WorkingOn { .. } => {
                Err(Failure::new(ErrorCode::Invalid, "the server answers this, not a worker"))
            }
            Verb::OpenTerminal { worker, cwd, command, env, name, size, session } => {
                self.mine(worker)?;
                let req = OpenSession {
                    size: term_size(size)?,
                    cwd,
                    command,
                    env,
                    title: name,
                    attach: false,
                };
                let _choosing = self.choosing(session).await;
                if let Some(running) = self.running(session) {
                    return Ok(Outcome::Opened(TermRef { worker, session: running }));
                }
                let handle = self.open_as(session, &req, ORCHESTRATOR).await?;
                // A command that runs Claude Code is guarded as a spawned agent is from the
                // start: until its first hook a dialog of its own may be up, and typing would
                // answer it.
                if slopty_agent::detect::is_claude("", &req.command) {
                    inner.agent_terms.lock().insert(handle.id());
                }
                Ok(Outcome::Opened(TermRef { worker, session: handle.id() }))
            }
            Verb::SpawnAgent {
                worker,
                agent,
                cwd,
                prompt,
                args,
                env,
                size,
                session,
                permission_flags,
            } => {
                self.mine(worker)?;
                let spawn = Spawn { cwd, args, env, size: term_size(size)?, permission_flags };
                let _choosing = self.choosing(session).await;
                if let Some(running) = self.running(session) {
                    return Ok(Outcome::Opened(TermRef { worker, session: running }));
                }
                self.spawn_agent(agent, spawn, prompt, session).await
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
                    events: inner.events.subscribe(),
                    agents: inner.worker.shared_agents(),
                });
                let timeout = Duration::from_millis(u64::from(timeout_ms));
                Ok(Outcome::Waited(wait_for(&handle, &until, timeout, feed).await?))
            }
            Verb::AgentStatus { term } => {
                self.session(term)?;
                Ok(Outcome::Agent(inner.worker.agents().status(term.session)))
            }
            Verb::Close { term } => {
                self.mine(term.worker)?;
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
            | Verb::ReviewCheckout { .. }
            | Verb::Rebase { .. }
            | Verb::FastForward { .. }
            | Verb::PullChecks { .. }) => Box::pin(self.repository(verb)).await,
            Verb::WriteFile { worker, path, bytes } => {
                self.mine(worker)?;
                let path = crate::file::expand_home(Path::new(&path));
                blocking(move || write_file(&path, &bytes)).await.map(|()| Outcome::Done)
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
                    sleeping: false,
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
            Verb::PointAt { item } => {
                self.item(item)?;
                let pointed = ItemSync::Pointed {
                    client: ORCHESTRATOR,
                    name: POINTER_NAME.to_owned(),
                    item: item.item,
                };
                let _sent = inner.events.send(WorkerMsg::Items(pointed));
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
            Verb::ReadConversation { term, thread, since, max, hold } => {
                self.session(term)?;
                let held = if hold { inner.conversations.follow(term.session) } else { Vec::new() };
                let sources = inner.conversations.sources(term.session);
                let mut page =
                    blocking(move || conversation::read_page(&sources, &thread, since, max))
                        .await?;
                page.held = held;
                Ok(Outcome::Conversation(Box::new(page)))
            }
            Verb::AnswerPermission { term, ask, verdict } => {
                self.session(term)?;
                if inner.conversations.answer(term.session, ask, verdict) {
                    Ok(Outcome::Done)
                } else {
                    Err(Failure::new(
                        ErrorCode::Failed,
                        format!(
                            "no prompt {ask} waits for orchestration in this terminal: it was \
                             answered, handed back to the TUI or withdrawn, or asked before \
                             orchestration read this conversation"
                        ),
                    ))
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
    /// ([`slopty_agent::hooks::without_bypass`]). The caller's own settings, MCP servers,
    /// arguments and variables are kept, and its variables win. The server it asks for tools
    /// is the session's own `SLOPTY_SERVER`.
    async fn spawn_agent(
        &self,
        agent: AgentKind,
        spawn: Spawn,
        prompt: Option<String>,
        chosen: Option<SessionId>,
    ) -> Result<Outcome, Failure> {
        let program = match agent {
            AgentKind::ClaudeCode => "claude",
        };
        // Subscribed before the spawn: the agent may report itself ready before the open
        // returns.
        let events = self.inner.events.subscribe();
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
                    slopty_agent::hooks::without_bypass(args, &dir)
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
            command: std::iter::once(program.to_owned()).chain(args).collect(),
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
            tokio::spawn(type_when_ready(handle, prompt, AgentFeed { events, agents }));
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
            Verb::ReviewCheckout { worker, repo, worktree, head, target } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let places = crate::file::expand_home(Path::new(VERIFY_PLACES));
                let place = crate::repo::verify::place(&places, &worktree)
                    .map_err(|f| verify_failure(&f))?;
                let made = crate::repo::review::checkout(git, &repo, &place, &head, &target)
                    .await
                    .map_err(|f| verify_failure(&f))?;
                let path = made.path.to_string_lossy().into_owned();
                Ok(Outcome::CheckedOut { path, head: made.head, base: made.base })
            }
            Verb::Rebase { worker, repo, worktree, head, onto } => {
                self.mine(worker)?;
                let git = crate::changes::git().ok_or_else(|| {
                    Failure::new(ErrorCode::Unsupported, "this worker has no git")
                })?;
                let repo = crate::file::expand_home(Path::new(&repo));
                let places = crate::file::expand_home(Path::new(VERIFY_PLACES));
                let place = crate::repo::verify::place(&places, &worktree)
                    .map_err(|f| verify_failure(&f))?;
                let made = crate::repo::verify::rebase(git, &repo, &place, &head, &onto)
                    .await
                    .map_err(|f| verify_failure(&f))?;
                Ok(Outcome::Rebased { head: made.head, onto: made.onto })
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
            Verb::PullChecks { worker, cwd, number, merge_request } => {
                self.mine(worker)?;
                let cwd = crate::file::expand_home(Path::new(&cwd));
                crate::repo::checks::read(&cwd, number, merge_request)
                    .await
                    .map(Outcome::Checks)
                    .map_err(|failed| match failed {
                        crate::repo::checks::Failed::Missing(program) => Failure::new(
                            ErrorCode::Unsupported,
                            format!("this worker has no {program}"),
                        ),
                        crate::repo::checks::Failed::Said(why) => {
                            Failure::new(ErrorCode::Failed, why)
                        }
                    })
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

/// Whether an agent is at its prompt, by its hooks' word: at rest after its `SessionStart` or
/// a turn. A weaker signal (its process, its title, its transcript) may come before its TUI
/// takes input, or while a dialog of its own is up.
fn at_its_prompt(agent: &SessionAgent) -> bool {
    agent.source == AgentSource::Hook
        && matches!(
            agent.status,
            AgentStatus::Idle
                | AgentStatus::Done
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
                match feed.events.recv().await {
                    Ok(WorkerMsg::Agent(ev)) if ev.session == session => return true,
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

/// Replace a file whole (`slopty_platform::fs::replace`).
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    slopty_platform::fs::replace(path, bytes).map_err(|e| io_failure(path, &e))
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
    fn files_are_read_capped_and_written_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        write_file(&path, b"one").unwrap();
        assert_eq!(whole(&path).unwrap(), b"one");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        write_file(&path, b"two").unwrap();
        assert_eq!(whole(&path).unwrap(), b"two");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&path).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o755, "the mode carries over");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");

        let big = dir.path().join("big.bin");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = whole(&big).unwrap_err();
        assert!(err.message.contains("offset and length"), "{err:?}");
        assert_eq!(whole(dir.path()).unwrap_err().code, ErrorCode::Failed);
        let missing = write_file(&dir.path().join("no/such/dir/x"), b"").unwrap_err();
        assert_eq!(missing.code, ErrorCode::Failed);
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
