//! The thread host on the daemon (`slopty_worker::thread`).
//!
//! Every Claude Code session the daemon sees is observed into the agent-neutral thread model,
//! its permission prompts held for the thread's followers ([`hold`]), every Codex thread is
//! followed, and a pi thread or the thread
//! of any ACP agent is started and driven here. A Claude Code thread is started in one of the
//! daemon's terminals and observed, and a Codex thread is started over the person's Codex daemon.
//! They are served to clients: the table on the control stream, a stream per followed thread, and
//! intents answered once each.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hold::Orchestrated;
use slopty_agent::codex::shared::Setting;
use slopty_core::{ClientId, SessionId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::orchestration::ErrorCode;
use slopty_proto::thread::wire::{
    Expanded, Intent, IntentDone, Outcome, PastSessions, ReviewScope, Setup, Start, TableFrame,
    ThreadFrame, ThreadHits, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{
    Action, AgentId, Answerer, AskId, Cap, ContentRef, Cursor, Delivery, IntentId, Liveness,
    ThreadId, ThreadState, TreeRef, TurnId,
};
use slopty_worker::conversation::{ORCHESTRATION, Seen};
use slopty_worker::manager::Worker;
use slopty_worker::orchestrate::{self, Agents, Conversations as _, Failure};
use slopty_worker::repo::worktrees;
use slopty_worker::session::SessionHandle;
use slopty_worker::thread::acp::{self, Acp};
use slopty_worker::thread::authors::Authorship;
use slopty_worker::thread::carry::carry;
use slopty_worker::thread::claude::{self, Driver, Sources};
use slopty_worker::thread::codex::{self, Codex};
use slopty_worker::thread::compose::Terminals;
use slopty_worker::thread::history::{self, History};
use slopty_worker::thread::pi::{self, Pi};
use slopty_worker::thread::review::Snapshots;
use slopty_worker::thread::screens::Screens;
use slopty_worker::thread::terminals::{Pending, Terminals as AgentTerminalsTrait};
use slopty_worker::thread::{Composer, Follower, Host, Seated, schedule};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::Daemon;

pub mod hold;

/// What an observed session needs of the daemon.
struct Observed(Daemon);

impl Sources for Observed {
    fn sources(&self, session: SessionId) -> orchestrate::Sources {
        Orchestrated(self.0.clone()).sources(session)
    }

    fn seen(&self, session: SessionId) -> watch::Receiver<Seen> {
        self.0.follows.lock().board.watch(session)
    }
}

/// The daemon's terminals and agents, as the composer types into them.
struct Typing {
    worker: Worker,
    agents: crate::server::DaemonAgents,
}

impl Agents for Typing {
    fn status(&self, session: SessionId) -> Option<slopty_agent::status::SessionAgent> {
        self.agents.status(session)
    }

    fn forget(&self, session: SessionId) {
        self.agents.forget(session);
    }

    fn ended(&self, session: SessionId) -> bool {
        self.agents.ended(session)
    }
}

impl Terminals for Typing {
    fn terminal(&self, session: SessionId) -> Option<SessionHandle> {
        self.worker.get(session).ok()
    }
}

/// The daemon's threads and what serves them.
///
/// The adapter answers for them, the composer types what is sent to them, and their turns
/// are snapshotted. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Threads {
    host: Host,
    claude: Driver,
    claude_start: claude::start::Starter,
    codex: Codex,
    pi: Pi,
    acp: Acp,
    composer: Composer,
    snapshots: Snapshots,
    authors: Authorship,
    history: History,
    worker: Worker,
}

/// Open the threads kept under `dir`, and what [`start`] needs to observe into them.
///
/// They are typed into through `worker`'s terminals as `agents` says of them, and their
/// snapshots' indexes are kept under `snapshots`. A host that cannot open is warned of, and
/// the daemon goes on without threads.
pub fn open(
    dir: &Path,
    snapshots: &Path,
    worker: Worker,
    agents: Arc<parking_lot::Mutex<slopty_agent::AgentTable>>,
) -> Option<(Threads, Observing)> {
    match Host::open(dir, slopty_worker::thread::log::Limits::default()) {
        Ok(host) => {
            let seats = worker.clone();
            host.set_seat_env(Arc::new(move |seat, extra| seats.seat_env(seat, extra)));
            let (claude, asks) = Driver::channel();
            let (claude_start, claude_start_asks) = claude::start::Starter::channel();
            let (codex, codex_asks) = Codex::channel();
            let (pi, pi_asks) = Pi::channel();
            let (acp, acp_asks) = Acp::channel();
            let terminals = Arc::new(AgentTerminals(worker.clone()));
            let typing =
                Typing { worker: worker.clone(), agents: crate::server::DaemonAgents(agents) };
            let composer = Composer::new(host.clone(), Arc::new(typing));
            let git = slopty_worker::changes::git().map(Path::to_path_buf);
            let authors = Authorship::new(host.clone(), git.clone());
            let snapshots = Snapshots::new(host.clone(), snapshots, git);
            // The threads live under the data directory, where pi's gate is written too.
            let data = dir.parent().unwrap_or(dir).to_path_buf();
            let history = History::new(history::Stores::here(), history::Limits::default());
            let observing = Observing {
                claude: asks,
                claude_start: claude_start_asks,
                codex: codex_asks,
                pi: pi_asks,
                acp: acp_asks,
                data,
                terminals,
            };
            let threads = Threads {
                host,
                claude,
                claude_start,
                codex,
                pi,
                acp,
                composer,
                snapshots,
                authors,
                history,
                worker,
            };
            Some((threads, observing))
        }
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "the thread host did not open: {e}");
            None
        }
    }
}

/// The daemon's terminals, as an agent's own TUI runs in them: Claude Code started from a
/// client, pi's TUI when a session is handed to it. Each is titled by its program until the
/// program titles it.
#[derive(Debug)]
struct AgentTerminals(Worker);

impl AgentTerminalsTrait for AgentTerminals {
    fn open(
        &self,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) -> Pending<'_, Result<SessionId, String>> {
        Box::pin(async move {
            let open = opening(command, cwd, env);
            self.0.open(&open).await.map(|session| session.id()).map_err(|e| e.to_string())
        })
    }

    fn open_at(
        &self,
        seat: SessionId,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) -> Pending<'_, Result<SessionId, String>> {
        Box::pin(async move {
            let open = opening(command, cwd, env);
            self.0.open_as(seat, &open).await.map(|session| session.id()).map_err(|e| e.to_string())
        })
    }

    fn exited(&self, session: SessionId) -> Pending<'static, ()> {
        let handle = self.0.get(session).ok();
        Box::pin(async move {
            let Some(handle) = handle else { return };
            let mut activity = handle.activity();
            while !activity.borrow_and_update().exited {
                if activity.changed().await.is_err() {
                    return;
                }
            }
        })
    }

    fn close(&self, session: SessionId) -> Pending<'_, ()> {
        Box::pin(async move {
            if let Err(e) = self.0.close(session).await {
                tracing::debug!(%session, "an agent's terminal did not close: {e}");
            }
        })
    }
}

/// The agents' terminals, each one opened told to every client as a client's own is: the
/// thread names the terminal its agent runs in, and a client shows the agent there only once it
/// knows that terminal is live.
#[derive(Debug)]
struct Announced {
    terminals: Arc<AgentTerminals>,
    daemon: Daemon,
}

impl Announced {
    /// Tell every client of `session`, just opened.
    async fn announce(&self, session: SessionId) {
        if let Some(summary) = self.daemon.worker.summary(session).await {
            let _sent = self.daemon.events.send(WorkerMsg::SessionChanged(summary));
        }
    }
}

impl AgentTerminalsTrait for Announced {
    fn open(
        &self,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) -> Pending<'_, Result<SessionId, String>> {
        Box::pin(async move {
            let session = self.terminals.open(command, cwd, env).await?;
            self.announce(session).await;
            Ok(session)
        })
    }

    fn open_at(
        &self,
        seat: SessionId,
        command: Vec<String>,
        cwd: String,
        env: Vec<(String, String)>,
    ) -> Pending<'_, Result<SessionId, String>> {
        Box::pin(async move {
            let session = self.terminals.open_at(seat, command, cwd, env).await?;
            self.announce(session).await;
            Ok(session)
        })
    }

    fn exited(&self, session: SessionId) -> Pending<'static, ()> {
        self.terminals.exited(session)
    }

    fn close(&self, session: SessionId) -> Pending<'_, ()> {
        self.terminals.close(session)
    }
}

/// What the adapters take their asks from once they start.
#[derive(Debug)]
pub struct Observing {
    claude: claude::Asks,
    claude_start: claude::start::Asks,
    codex: codex::Asks,
    pi: pi::Asks,
    acp: acp::Asks,
    /// The daemon's data directory.
    data: PathBuf,
    /// The terminals Claude Code and pi's TUI run in.
    terminals: Arc<AgentTerminals>,
}

/// Observe every Claude Code session and follow every Codex thread into the daemon's
/// threads, start Claude Code threads in its terminals, serve the pi and ACP threads it starts,
/// snapshot each turn, name the screens each agent drives, and watch each thread's pull
/// request where gh is installed.
pub fn start(daemon: &Daemon, asks: Observing) {
    let Some(threads) = &daemon.threads else { return };
    drop(threads.snapshots.spawn());
    drop(Screens::new(threads.host.clone()).spawn());
    if let Some(gh) = slopty_worker::repo::commit::Programs::here().gh {
        drop(slopty_worker::thread::pulls::spawn(threads.host.clone(), gh));
    }
    threads.composer.resume();
    let sources: Arc<dyn Sources> = Arc::new(Observed(daemon.clone()));
    let (events, heard) = (daemon.events.subscribe(), daemon.heard.subscribe());
    drop(claude::spawn(threads.host.clone(), events, heard, sources, asks.claude));
    let terminals: Arc<dyn AgentTerminalsTrait> =
        Arc::new(Announced { terminals: asks.terminals, daemon: daemon.clone() });
    drop(claude::start::spawn(
        threads.host.clone(),
        threads.claude.clone(),
        Arc::clone(&terminals),
        None,
        slopty_agent::roster::sessions_dir(&slopty_platform::dirs::home()),
        asks.claude_start,
    ));
    if let Some(home) = codex::codex_home() {
        let launch = codex::Launch::new(None).with_terminals(Arc::clone(&terminals));
        drop(codex::spawn(threads.host.clone(), codex::socket_of(&home), Some(launch), asks.codex));
    }
    // The registry's agents and the person's own, `[worker.acp]`, read at each start.
    let settings = slopty_settings::path_in(&asks.data);
    let own: acp::Own =
        Arc::new(move || slopty_settings::Settings::load(&settings).settings.worker.acp);
    drop(pi::spawn(threads.host.clone(), asks.data, None, terminals, asks.pi));
    drop(acp::spawn(threads.host.clone(), None, own, asks.acp));
    // A scheduled message goes as the person's own, the way a client's does.
    let (sending, at) = (threads.clone(), daemon.clone());
    let fire: schedule::Fire = Arc::new(move |thread, id, intent| {
        let who = Who { daemon: &at, link: ORCHESTRATION, client: ClientId::nil() };
        act_as(&who, &sending, thread, id, &intent)
    });
    drop(schedule::spawn(threads.host.clone(), fire));
}

/// What a request needs of the connection it came on.
#[derive(Debug)]
pub struct Origin<'a> {
    /// The daemon.
    pub daemon: &'a Daemon,
    /// The connection, which a followed thread's stream opens on.
    pub conn: &'a Connection,
    /// Its control stream.
    pub out: &'a mpsc::Sender<WorkerMsg>,
    /// The connection, as the held prompts tell clients apart.
    pub link: slopty_worker::clip::Link,
    /// The client.
    pub client: ClientId,
    /// The connection's tasks, which end with it.
    pub tasks: &'a mut JoinSet<()>,
}

impl Origin<'_> {
    /// Who acts through this connection.
    const fn who(&self) -> Who<'_> {
        Who { daemon: self.daemon, link: self.link, client: self.client }
    }

    /// Queue `msg` for the client without waiting, as the connection's own answers are: a
    /// client that has not read a full queue loses it, and an intent's answer is had again by
    /// sending the intent again.
    fn post(&self, msg: WorkerMsg) {
        if let Err(mpsc::error::TrySendError::Full(msg)) = self.out.try_send(msg) {
            tracing::warn!(client = %self.client, ?msg, "control queue full; answer dropped");
        }
    }
}

/// What one connection follows of the threads.
#[derive(Debug, Default)]
pub struct Following {
    /// The task keeping the client's table current, replaced by its next ask.
    table: Option<AbortHandle>,
    /// Each followed thread's task, which takes its requests here and ends when the sender
    /// goes, and the terminal the thread's agent runs in.
    threads: HashMap<ThreadId, (mpsc::UnboundedSender<Command>, Option<SessionId>)>,
}

impl Drop for Following {
    fn drop(&mut self) {
        if let Some(table) = &self.table {
            table.abort();
        }
    }
}

/// What a followed thread's task is asked between frames.
#[derive(Debug)]
enum Command {
    Page { before: TurnId, turns: u32 },
    Expand { content: ContentRef },
    Review { scope: ReviewScope },
}

impl Following {
    /// Whether a followed thread's agent runs in `session`: the prompts held there are held
    /// for this connection while it is.
    fn follows_session(&self, session: SessionId) -> bool {
        self.threads.values().any(|(_, terminal)| *terminal == Some(session))
    }

    /// Take `req` from the client on `at`.
    pub fn handle(&mut self, at: &mut Origin<'_>, req: ThreadRequest) {
        let Some(threads) = at.daemon.threads.clone() else {
            tracing::debug!(client = %at.client, "a thread request, with no threads here");
            if let ThreadRequest::Intent { id, .. } | ThreadRequest::Start { id, .. } = req {
                let reason = "this machine keeps no threads".to_owned();
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome: refused(reason) }));
            }
            return;
        };
        // A start's folder may be spelled from the home, as a client that knows no better
        // spells it: every adapter takes it whole.
        let req = match req {
            ThreadRequest::Start { id, mut start } => {
                let cwd = slopty_worker::file::expand_home(Path::new(&start.cwd));
                start.cwd = cwd.to_string_lossy().into_owned();
                ThreadRequest::Start { id, start }
            }
            other => other,
        };
        match req {
            ThreadRequest::Table { have } => {
                if let Some(old) = self.table.take() {
                    old.abort();
                }
                // Every thread's requests show in the table, so its holder answers approvals.
                at.daemon.follows.lock().holds.approve(at.link);
                self.table = Some(at.tasks.spawn(table(threads.host, have, at.out.clone())));
            }
            ThreadRequest::Follow { thread, have, turns, max_latency_ms } => {
                if self.threads.contains_key(&thread) {
                    return;
                }
                let Some((state, _cursor)) = threads.host.state(thread) else {
                    tracing::debug!(client = %at.client, %thread, "follow of a thread not here");
                    return;
                };
                // A Codex thread let go while it rested is taken up again.
                if codex::is_shared(&state) {
                    threads.codex.wake(thread);
                }
                let terminal = state.meta.terminal;
                // Its prompts are held for this client while it follows, and shown on the thread.
                if let Some(session) = terminal {
                    at.daemon.follows.lock().holds.follow(session, at.link);
                }
                tracing::info!(client = %at.client, %thread, ?have, "follow thread");
                let follower =
                    Follower::new(threads.host.clone(), thread, have, turns, max_latency_ms);
                let (commands, taken) = mpsc::unbounded_channel();
                at.tasks.spawn(stream(threads, at.conn.clone(), follower, taken));
                self.threads.insert(thread, (commands, terminal));
            }
            ThreadRequest::Unfollow { thread } => {
                let Some((_task, terminal)) = self.threads.remove(&thread) else { return };
                tracing::info!(client = %at.client, %thread, "unfollow thread");
                if let Some(session) = terminal
                    && !self.follows_session(session)
                {
                    let released = at.daemon.follows.lock().holds.unfollow(session, at.link);
                    hold::release(at.daemon, released);
                }
            }
            ThreadRequest::Page { thread, before, turns } => {
                self.command(at, thread, Command::Page { before, turns });
            }
            ThreadRequest::Expand { thread, content } => {
                self.command(at, thread, Command::Expand { content });
            }
            ThreadRequest::Review { thread, scope } => {
                self.command(at, thread, Command::Review { scope });
            }
            // Blame runs git over every turn: on a task of its own.
            ThreadRequest::Authors { thread, path } => {
                let (authors, out) = (threads.authors, at.out.clone());
                at.tasks.spawn(async move {
                    let _gone =
                        out.send(WorkerMsg::Authors(authors.authors(thread, &path).await)).await;
                });
            }
            // Git takes its time: a keep or a revert is answered from a task of its own.
            ThreadRequest::Intent {
                id,
                thread,
                intent: intent @ (Intent::Keep(_) | Intent::Revert(_)),
            } => {
                let (snapshots, out) = (threads.snapshots, at.out.clone());
                at.tasks.spawn(async move {
                    if let Some(outcome) = snapshots.pick(thread, id, &intent).await {
                        let _gone =
                            out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                    }
                });
            }
            // An agent's own review names the change as commits first: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Review { from, to } } => {
                tracing::info!(client = %at.client, %id, %thread, "review by the agent");
                let (daemon, link, client, out) =
                    (at.daemon.clone(), at.link, at.client, at.out.clone());
                at.tasks.spawn(async move {
                    let who = Who { daemon: &daemon, link, client };
                    let outcome = review(&who, &threads, thread, id, &from, &to).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // Codex's own TUI is found and opened on the thread: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Release { .. } }
                if threads
                    .host
                    .state(thread)
                    .is_some_and(|(state, _)| codex::is_shared(&state)) =>
            {
                let (codex, out) = (threads.codex, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = codex.release(thread, id).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // A fork starts a thread: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Fork { after } } => {
                tracing::info!(client = %at.client, %id, %thread, "fork");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let outcome = fork(&threads, thread, id, after).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // An aside starts a thread, and closing one ends an agent: on tasks of their own.
            ThreadRequest::Intent { id, thread, intent: Intent::Aside } => {
                tracing::info!(client = %at.client, %id, %thread, "aside");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let outcome = aside(&threads, thread, id).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            ThreadRequest::Intent { id, thread, intent: Intent::Discard } => {
                tracing::info!(client = %at.client, %id, %thread, "discard an aside");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let outcome = discard(&threads, thread, id).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            ThreadRequest::Intent { id, thread, intent: Intent::KeepAside } => {
                let outcome = keep_aside(&threads, thread, id);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            // An edit from a turn branches the thread and may put files back: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Rewind { turn, files } } => {
                tracing::info!(client = %at.client, %id, %thread, turn = turn.0, files, "rewind");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let outcome = rewind(&threads, thread, id, turn, files).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // Going on in a new thread starts one: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Continue { agent } } => {
                tracing::info!(client = %at.client, %id, %thread, agent = %agent.0, "continue");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let host = threads.host.clone();
                    let begun = |id, start| begin(&threads, id, Box::new(start));
                    let outcome = carry(&host, thread, id, agent, begun).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            ThreadRequest::Intent { id, thread, intent } => {
                let outcome = act(at, &threads, thread, id, &intent);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            // An agent is looked for and starts, in a terminal or not, in the worktree it
            // names once that is made and set up, the setup said as it goes: on a task of its
            // own.
            ThreadRequest::Start { id, mut start } => {
                tracing::info!(client = %at.client, %id, agent = %start.agent.0, cwd = start.cwd, "start");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    // What a setup says is drawn over again, so one lost to a full link is not
                    // missed.
                    let said = |setup: &Setup| {
                        let msg = WorkerMsg::SettingUp { id, setup: setup.clone() };
                        let _full = out.try_send(msg);
                    };
                    let outcome = match worktrees::enter(&mut start, &said).await {
                        Ok(_) => begin(&threads, id, start).await,
                        Err(worktrees::Failed::Setup(failed)) => {
                            Outcome::SetupFailed { setup: failed.setup, code: failed.code }
                        }
                        Err(failed) => refused(failed.to_string()),
                    };
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // An agent is asked, or its session directory listed: on a task of its own.
            ThreadRequest::Sessions { agent, cwd, query, limit } => {
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let listed = sessions(&threads, agent, cwd, query, limit).await;
                    let _gone = out.send(WorkerMsg::Sessions(listed)).await;
                });
            }
            // Every thread held is read: on the blocking pool.
            ThreadRequest::Search { query, limit } => {
                let (host, out) = (threads.host, at.out.clone());
                at.tasks.spawn(async move {
                    let asked = query.clone();
                    let found = tokio::task::spawn_blocking(move || {
                        slopty_worker::thread::search::search(&host, &query, limit)
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("a thread search failed: {e}");
                        ThreadHits { query: asked, threads: Vec::new(), more: 0 }
                    });
                    let _gone = out.send(WorkerMsg::ThreadHits(found)).await;
                });
            }
        }
    }

    fn command(&self, at: &Origin<'_>, thread: ThreadId, command: Command) {
        match self.threads.get(&thread) {
            Some((task, _)) => {
                let _gone = task.send(command);
            }
            None => {
                tracing::debug!(client = %at.client, %thread, "a thread request while not following");
            }
        }
    }
}

/// Start a thread for intent `id` as `start` says, once, on its agent's adapter: pi and an
/// ACP agent are looked for and run here, Claude Code opens in a terminal, Codex's daemon is
/// asked.
async fn begin(threads: &Threads, id: IntentId, start: Box<Start>) -> Outcome {
    if start.agent.is(AgentId::PI) {
        threads.pi.start(id, start).await
    } else if start.agent.is(AgentId::CLAUDE_CODE) {
        threads.claude_start.start(id, *start).await
    } else if start.agent.is(AgentId::CODEX) {
        threads.codex.start(id, *start).await
    } else if slopty_agent::acp::name_of(&start.agent).is_some() {
        threads.acp.start(id, start).await
    } else {
        refused(format!("{} is no agent this machine can start", start.agent.0))
    }
}

/// Branch a new thread off `thread` through turn `after`, or all of it, for intent `id`, once,
/// as its agent forks: Codex `thread/fork`, an ACP agent `session/fork`, pi `--fork`, Claude
/// Code `--fork-session`.
async fn fork(threads: &Threads, thread: ThreadId, id: IntentId, after: Option<TurnId>) -> Outcome {
    if let Some(outcome) = threads.host.started(id) {
        return outcome;
    }
    let Some((state, _)) = threads.host.state(thread) else {
        return threads.host.record_start(id, refused("no such thread".to_owned()));
    };
    if !state.meta.can(Cap::FORK) {
        return threads.host.record_start(id, Outcome::Unsupported { cap: Cap::named(Cap::FORK) });
    }
    if codex::is_shared(&state) {
        threads.codex.fork(thread, id, after).await
    } else if pi::is_pi(&state) {
        threads.pi.fork(thread, id, after).await
    } else if acp::is_acp(&state) {
        threads.acp.fork(thread, id, after).await
    } else if state.meta.agent.is(AgentId::CLAUDE_CODE) {
        threads.claude_start.fork(thread, id, after).await
    } else {
        let reason = format!("{} threads are not forked here", state.meta.agent.0);
        threads.host.record_start(id, refused(reason))
    }
}

/// Ask aside of `thread` for intent `id`, once: a fork of the whole thread, marked as its aside
/// so lists and attention pass over it until it is kept.
async fn aside(threads: &Threads, thread: ThreadId, id: IntentId) -> Outcome {
    let outcome = fork(threads, thread, id, None).await;
    if let Outcome::Started { thread: aside } = outcome
        && !threads.host.aside(aside, Some(thread))
    {
        tracing::warn!(%thread, %aside, "an aside left before it was marked");
    }
    outcome
}

/// Close aside `thread` for intent `id`, once: its agent ends as a settled task's does, its
/// session kept where the agent keeps it, and the worker forgets the thread. A thread that is
/// no aside is refused.
async fn discard(threads: &Threads, thread: ThreadId, id: IntentId) -> Outcome {
    // Answered once, though the thread is gone after: kept with the worker's starts.
    if let Some(first) = threads.host.started(id) {
        return first;
    }
    let Some((state, _)) = threads.host.state(thread) else {
        return refused("no such thread".to_owned());
    };
    let outcome = if state.meta.aside_of().is_none() {
        refused("Only an aside is closed for good".to_owned())
    } else {
        if let Err(e) = threads.end(&state).await {
            tracing::warn!(%thread, "an aside's agent did not end: {e}");
        }
        threads.snapshots.forget(&state).await;
        if let Err(e) = threads.host.remove(thread) {
            tracing::warn!(%thread, "an aside's log stayed: {e}");
        }
        Outcome::Done
    };
    threads.host.record_start(id, outcome)
}

/// What Codex calls the change it reviews ([`Codex::review`]).
const REVIEW_TITLE: &str = "The changes on show in Slopty's review";

/// Ask `thread`'s agent for its own review of the change from `from` to `to`, for intent `id`,
/// once: the change is kept as two commits ([`Snapshots::review_range`]), then Codex's reviewer
/// takes the head commit, and Claude Code's `/code-review` the range, sent as the person's turn
/// through its composer as any command of theirs is.
async fn review(
    who: &Who<'_>,
    threads: &Threads,
    thread: ThreadId,
    id: IntentId,
    from: &TreeRef,
    to: &TreeRef,
) -> Outcome {
    if let Some(first) = threads.host.outcome(thread, id) {
        return first;
    }
    let once = |outcome: Outcome| {
        threads
            .host
            .intent(thread, id, |_state| (outcome, Vec::new()))
            .unwrap_or_else(|| refused("no such thread".to_owned()))
    };
    let Some((state, _)) = threads.host.state(thread) else {
        return refused("no such thread".to_owned());
    };
    if !state.meta.can(Cap::REVIEW) {
        return once(Outcome::Unsupported { cap: Cap::named(Cap::REVIEW) });
    }
    let range = match threads.snapshots.review_range(thread, from, to).await {
        Ok(range) => range,
        Err(why) => return once(refused(why)),
    };
    if codex::is_shared(&state) {
        let outcome = threads.codex.review(thread, range.head, REVIEW_TITLE.to_owned()).await;
        return once(outcome);
    }
    let text = format!("/{} {}", slopty_agent::observed::REVIEW_COMMAND, range.dots());
    let delivery = state.meta.delivery_after_turn();
    act_as(who, threads, thread, id, &Intent::Send { text, delivery, attachments: Vec::new() })
}

/// Keep aside `thread` for intent `id`, once, as an ordinary thread of its own.
fn keep_aside(threads: &Threads, thread: ThreadId, id: IntentId) -> Outcome {
    if let Some(first) = threads.host.outcome(thread, id) {
        return first;
    }
    let Some((state, _)) = threads.host.state(thread) else {
        return refused("no such thread".to_owned());
    };
    if state.meta.aside_of().is_some() {
        threads.host.aside(thread, None);
    }
    threads.host.intent(thread, id, |_| (Outcome::Done, Vec::new())).unwrap_or(Outcome::Done)
}

/// Edit `thread` from turn `turn` for intent `id`, once, its files going back too with `files`
/// ([`slopty_worker::thread::rewind`]): only Codex branches a session before a turn, and every
/// other agent is refused in words.
async fn rewind(
    threads: &Threads,
    thread: ThreadId,
    id: IntentId,
    turn: TurnId,
    files: bool,
) -> Outcome {
    let shared = threads.host.state(thread).is_some_and(|(state, _)| codex::is_shared(&state));
    let branch = async || {
        if shared {
            threads.codex.rewind(thread, id, turn).await
        } else {
            refused("Only Codex goes back to before a turn through its own door".to_owned())
        }
    };
    let (host, snapshots) = (&threads.host, &threads.snapshots);
    slopty_worker::thread::rewind::rewind(host, snapshots, (thread, id), (turn, files), branch)
        .await
}

/// Who answers for a client: Slopty, on its behalf.
fn answerer(client: ClientId) -> Answerer {
    Answerer { client: Some(client), name: "Slopty".to_owned() }
}

/// The past sessions [`ThreadRequest::Sessions`] asks for, at most `limit`: agent `agent`'s in
/// folder `cwd` as the agent lists them, with no `query`; else those the person's past prompts
/// find ([`History`]). Each is named by the thread held of it here, if one is; why there are
/// none, when they could not be had.
async fn sessions(
    threads: &Threads,
    agent: Option<AgentId>,
    cwd: Option<String>,
    query: String,
    limit: u32,
) -> PastSessions {
    let (sessions, absent, cut) = match (&agent, &cwd) {
        (Some(agent), Some(cwd)) if query.trim().is_empty() => {
            let (sessions, absent) = listed(threads, agent.clone(), cwd.clone(), limit).await;
            (sessions, absent, None)
        }
        _ => {
            let history = threads.history.clone();
            let (agent, cwd, query) = (agent.clone(), cwd.clone(), query.clone());
            let found = tokio::task::spawn_blocking(move || {
                history.search(&history::Ask {
                    agent: agent.as_ref(),
                    cwd: cwd.as_deref(),
                    query: &query,
                    limit: usize::try_from(limit).unwrap_or(usize::MAX),
                })
            })
            .await;
            match found {
                Ok(found) => (found.sessions, found.absent, found.cut),
                Err(e) => (Vec::new(), Some(format!("The search stopped: {e}")), None),
            }
        }
    };
    let mut sessions = sessions;
    mark_running(&mut sessions).await;
    for past in &mut sessions {
        if let Some((thread, title)) = threads.host.session(&past.agent, &past.native) {
            past.thread = Some(thread);
            if !title.trim().is_empty() && (past.title.is_none() || !past.prompts.is_empty()) {
                past.title = Some(title);
            }
        }
    }
    PastSessions { agent, cwd, query, sessions, absent, cut }
}

/// Mark each Claude Code session a live Claude Code holds, as Claude Code's own registry of its
/// live sessions says it now ([`slopty_proto::thread::wire::PAST_RUNNING`]): the person sees it
/// runs before they pick it, and a pick of it is refused.
async fn mark_running(sessions: &mut [slopty_proto::thread::wire::PastSession]) {
    if !sessions.iter().any(|p| p.agent.is(AgentId::CLAUDE_CODE)) {
        return;
    }
    let listed = tokio::task::spawn_blocking(|| {
        let registry = slopty_agent::roster::sessions_dir(&slopty_platform::dirs::home());
        slopty_agent::roster::registered(&registry, slopty_worker::ports::alive)
    })
    .await
    .unwrap_or_default();
    for past in sessions.iter_mut().filter(|p| p.agent.is(AgentId::CLAUDE_CODE)) {
        if let Some(live) = slopty_agent::roster::holder(&listed, &past.native) {
            let how = live.kind.clone().unwrap_or_else(|| "interactive".to_owned());
            past.facts.insert(slopty_proto::thread::wire::PAST_RUNNING.to_owned(), how);
        }
    }
}

/// Agent `agent`'s past sessions in folder `cwd`, at most `limit`, the last first, as the agent
/// keeps them; why there are none, when they could not be had.
async fn listed(
    threads: &Threads,
    agent: AgentId,
    cwd: String,
    limit: u32,
) -> (Vec<slopty_proto::thread::wire::PastSession>, Option<String>) {
    let cwd = slopty_worker::file::expand_home(Path::new(&cwd)).to_string_lossy().into_owned();
    let listed = if agent.is(AgentId::CLAUDE_CODE) {
        let (home, dir) = (slopty_platform::dirs::home(), PathBuf::from(&cwd));
        let most = usize::try_from(limit).unwrap_or(usize::MAX);
        tokio::task::spawn_blocking(move || slopty_agent::discover::sessions(&home, &dir, most))
            .await
            .map_err(|e| e.to_string())
            .and_then(|listed| listed.map_err(|e| format!("Claude Code's sessions: {e}")))
    } else if agent.is(AgentId::CODEX) {
        threads.codex.sessions(cwd.clone(), limit).await
    } else if agent.is(AgentId::PI) {
        match slopty_agent::pi::sessions::agent_dir() {
            Some(dir) => pi::sessions(&dir, &cwd, limit).await,
            None => Err("pi's directory is not known here".to_owned()),
        }
    } else if slopty_agent::acp::name_of(&agent).is_some() {
        threads.acp.sessions(agent.clone(), cwd.clone(), limit).await
    } else {
        Err(format!("{} keeps no sessions this machine can list", agent.0))
    };
    let (mut sessions, absent) = match listed {
        Ok(sessions) => (sessions, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    sessions.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    (sessions, absent)
}

const fn refused(reason: String) -> Outcome {
    Outcome::Refused { reason }
}

/// Act on intent `id` for `thread` once ([`Host::intent`]): a repeat, after a reconnect or
/// from another client, gets the first outcome back and acts on nothing.
fn act(
    at: &Origin<'_>,
    threads: &Threads,
    thread: ThreadId,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    act_as(&at.who(), threads, thread, id, intent)
}

/// [`act`], for `who`.
fn act_as(
    who: &Who<'_>,
    threads: &Threads,
    thread: ThreadId,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    // A scheduled message is the worker's to hold: no agent hears of it until its moment.
    if let Some(outcome) = schedule::act(&threads.host, thread, id, intent) {
        return outcome;
    }
    let decided = threads.host.intent(thread, id, |state| decide(who, threads, state, id, intent));
    decided.unwrap_or_else(|| refused("no such thread".to_owned()))
}

/// Who acts on a thread: a client's connection, or orchestration's verbs.
struct Who<'a> {
    /// The daemon.
    daemon: &'a Daemon,
    /// The link the held prompts know it by.
    link: slopty_worker::clip::Link,
    /// The client; the nil client for orchestration.
    client: ClientId,
}

/// The daemon's threads as orchestration's verbs read and answer them
/// ([`slopty_worker::orchestrate::ThreadReads`]): as [`ORCHESTRATION`], by the nil client,
/// so a Claude Code prompt held for orchestration's read is answered here.
#[derive(Clone)]
pub struct Reads {
    threads: Threads,
    daemon: Daemon,
}

impl Reads {
    /// `threads`, as `daemon`'s orchestration reaches them.
    pub const fn new(threads: Threads, daemon: Daemon) -> Self {
        Self { threads, daemon }
    }
}

impl orchestrate::ThreadReads for Reads {
    fn state(&self, thread: ThreadId) -> Option<ThreadState> {
        self.threads.host.state(thread).map(|(state, _)| state)
    }

    fn intent(&self, thread: ThreadId, id: IntentId, intent: Intent) -> Outcome {
        let who = Who { daemon: &self.daemon, link: ORCHESTRATION, client: ClientId::nil() };
        tracing::info!(%thread, %id, "an intent from orchestration");
        act_as(&who, &self.threads, thread, id, &intent)
    }

    fn at_terminal(&self, session: SessionId) -> Option<ThreadRow> {
        self.threads.host.at_terminal(session)
    }
}

/// What comes of `intent` on `state`'s thread, done as it is decided.
fn decide(
    who: &Who<'_>,
    threads: &Threads,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> (Outcome, Vec<Action>) {
    let needs = intent.needs();
    if !state.meta.can(needs) {
        return (Outcome::Unsupported { cap: Cap::named(needs) }, Vec::new());
    }
    if let Intent::Send { text, attachments, delivery: Delivery::Interrupt } = intent {
        // "Now" goes by the agent's own steer where it has one. Only an agent without one is
        // interrupted, and its adapter puts the message first in its queue.
        if state.meta.can(Cap::STEER) {
            let (text, attachments) = (text.clone(), attachments.clone());
            let steer = Intent::Send { text, attachments, delivery: Delivery::Steer };
            return decide(who, threads, state, id, &steer);
        }
        if !state.meta.can(Cap::QUEUE) {
            return (Outcome::Unsupported { cap: Cap::named(Cap::QUEUE) }, Vec::new());
        }
    }
    if codex::is_shared(state) {
        return (shared(who, &threads.codex, state, id, intent), Vec::new());
    }
    if pi::is_pi(state) {
        let by = answerer(who.client);
        return (threads.pi.decide(state, id, intent, by), Vec::new());
    }
    if acp::is_acp(state) {
        let by = answerer(who.client);
        return (threads.acp.decide(state, id, intent, by), Vec::new());
    }
    let Some(session) = state.meta.terminal else {
        let reason = "the thread's agent runs in no terminal here".to_owned();
        return (refused(reason), Vec::new());
    };
    if let Some(decided) = threads.composer.decide(state, id, intent) {
        return decided;
    }
    let outcome = match intent {
        Intent::Answer { ask, choice, message } => {
            let verdict = slopty_agent::observed::verdict(choice, message.as_deref());
            match (held(ask), verdict) {
                (None, _) => refused(format!("no request {}", ask.0)),
                (_, None) => refused(format!("no choice {choice}")),
                (Some(held), Some(verdict)) => {
                    answered(hold::answer(who.daemon, who.link, who.client, session, held, verdict))
                }
            }
        }
        Intent::Release { ask } => match held(ask) {
            Some(held) => {
                answered(hold::hand_back(who.daemon, who.link, who.client, session, held))
            }
            // Asked in the agent's own terminal with nothing held here: it is there already.
            None if asked_in_terminal(state, ask) => Outcome::Done,
            None => refused(format!("no request {}", ask.0)),
        },
        _ => Outcome::Unsupported { cap: Cap::named(needs) },
    };
    (outcome, Vec::new())
}

/// What comes of `intent` on a Codex thread: it goes to the app-server as the Codex TUI's own
/// would. An answer to a request already settled, from the TUI or another client, is no
/// error: the card shows who settled it. A reason given with an answer goes as the person's
/// next words into the turn, since a Codex decision carries none. Nothing goes while Codex does
/// not run the thread: it would be lost.
fn shared(
    who: &Who<'_>,
    codex: &Codex,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    let thread = state.meta.id;
    if matches!(state.status.liveness, Liveness::Exited { .. }) {
        return refused("Codex isn't running this thread".to_owned());
    }
    match intent {
        Intent::Answer { ask, choice, message } => {
            let Some(request) = state.requests.iter().find(|r| r.id == *ask) else {
                return refused(format!("no request {}", ask.0));
            };
            if !request.is_open() {
                return Outcome::Done;
            }
            let questions = &request.questions;
            let offered = request.options.iter().any(|o| o.id == *choice);
            if !offered && questions.is_empty() {
                return refused(format!("no choice {choice}"));
            }
            if !offered && slopty_proto::thread::detail::Answer::read(questions, choice).is_none() {
                return refused("the answer does not answer each question".to_owned());
            }
            let by = Answerer { client: Some(who.client), name: "Slopty".to_owned() };
            codex.answer(thread, ask.clone(), choice.clone(), by);
            if let Some(why) = message.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
                codex.send(thread, why.to_owned(), Vec::new(), Delivery::Steer, id);
            }
            Outcome::Done
        }
        Intent::Send { text, attachments, .. }
            if text.trim().is_empty() && attachments.is_empty() =>
        {
            refused("There is nothing to send".to_owned())
        }
        Intent::Send { text, attachments, delivery } => {
            if let Err(why) = slopty_worker::thread::attach::check(attachments) {
                return refused(why);
            }
            codex.send(thread, text.clone(), attachments.clone(), *delivery, id);
            Outcome::Done
        }
        Intent::Withdraw { pending }
        | Intent::Edit { pending, .. }
        | Intent::Promote { pending }
            if !state.pending.iter().any(|p| p.intent == *pending) =>
        {
            refused("That message is not waiting".to_owned())
        }
        Intent::Withdraw { pending } => {
            codex.withdraw(thread, *pending);
            Outcome::Done
        }
        Intent::Promote { pending } => {
            codex.promote(thread, *pending);
            Outcome::Done
        }
        Intent::Edit { pending, text } => {
            codex.edit(thread, *pending, text.clone());
            Outcome::Done
        }
        Intent::Interrupt => {
            codex.interrupt(thread);
            Outcome::Done
        }
        Intent::SetModel { model } if !state.meta.models.iter().any(|m| m.id == *model) => {
            refused(format!("Codex offers no model {model} here"))
        }
        Intent::SetEffort { effort } if !state.meta.efforts.iter().any(|e| e.id == *effort) => {
            refused(format!("The model has no reasoning effort {effort}"))
        }
        Intent::SetMode { mode } if !state.meta.modes.iter().any(|m| m.id == *mode) => {
            refused(format!("Codex has no approval policy {mode}"))
        }
        Intent::SetModel { model } => {
            codex.set(thread, Setting::Model(model.clone()));
            Outcome::Done
        }
        Intent::SetEffort { effort } => {
            codex.set(thread, Setting::Effort(effort.clone()));
            Outcome::Done
        }
        Intent::SetMode { mode } => {
            codex.set(thread, Setting::Mode(mode.clone()));
            Outcome::Done
        }
        other => Outcome::Unsupported { cap: Cap::named(other.needs()) },
    }
}

fn answered(taken: bool) -> Outcome {
    if taken {
        Outcome::Done
    } else {
        refused("the request is no longer open to this client".to_owned())
    }
}

/// The held prompt a request made from it names.
fn held(ask: &AskId) -> Option<u64> {
    ask.0.parse().ok()
}

/// Whether `ask` is open on the thread and answered only in the agent's own terminal: it
/// offers nothing here.
fn asked_in_terminal(state: &ThreadState, ask: &AskId) -> bool {
    state
        .requests
        .iter()
        .any(|r| r.id == *ask && r.is_open() && r.options.is_empty() && r.questions.is_empty())
}

/// The thread table as the server is told of it: every row first, then what changed. The
/// server ranks the rows for the whole fleet (`slopty_server::hub`'s ladder).
#[derive(Debug)]
pub struct Publish {
    host: Host,
    have: Option<Cursor>,
    changed: watch::Receiver<Cursor>,
}

impl Publish {
    /// The table of `threads`, from the start.
    #[must_use]
    pub fn of(threads: &Threads) -> Self {
        let changed = threads.host.table_watch();
        Self { host: threads.host.clone(), have: None, changed }
    }

    /// The next frame to send: the whole table the first time, then each change that moved a
    /// row. Cancel safe. `None` once the host is gone.
    pub async fn next(&mut self) -> Option<TableFrame> {
        loop {
            if self.have.is_some() {
                self.changed.changed().await.ok()?;
            }
            self.changed.borrow_and_update();
            let frame = self.host.table(self.have);
            let (cursor, moved) = match &frame {
                TableFrame::Snapshot { cursor, .. } => (*cursor, true),
                TableFrame::Delta { cursor, rows, removed } => {
                    (*cursor, !rows.is_empty() || !removed.is_empty())
                }
            };
            self.have = Some(cursor);
            if moved {
                return Some(frame);
            }
        }
    }
}

/// Keep the client's table current from `have`: what it lacks now, then each change.
async fn table(host: Host, mut have: Option<Cursor>, out: mpsc::Sender<WorkerMsg>) {
    let mut changed = host.table_watch();
    loop {
        changed.borrow_and_update();
        let frame = host.table(have);
        have = Some(match &frame {
            TableFrame::Snapshot { cursor, .. } | TableFrame::Delta { cursor, .. } => *cursor,
        });
        if out.send(WorkerMsg::Threads(frame)).await.is_err() || changed.changed().await.is_err() {
            return;
        }
    }
}

/// A followed thread's stream: what the client lacks from its cursor, then every change, with
/// the pages and expansions it asks for between them; finished when it unfollows or the
/// thread goes.
async fn stream(
    threads: Threads,
    conn: Connection,
    mut follower: Follower,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let thread = follower.thread();
    let wait = slopty_net::streams::SESSION_STREAM_WAIT;
    let mut out = match slopty_net::streams::open_thread(&conn, thread, wait).await {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!(%thread, error = %e, "no thread stream");
            return;
        }
    };
    let (reviewed_tx, mut reviewed) = mpsc::unbounded_channel();
    let why = loop {
        let frame = tokio::select! {
            frame = follower.next() => match frame {
                Some(frame) => frame,
                None => break "the thread is gone",
            },
            command = commands.recv() => match command {
                None => break "unfollowed",
                Some(Command::Page { before, turns }) => {
                    let Some(page) = threads.host.page(thread, before, turns) else {
                        break "the thread is gone";
                    };
                    ThreadFrame::Page(page)
                }
                Some(Command::Expand { content }) => {
                    let body = threads.expand(thread, content.clone()).await;
                    ThreadFrame::Expanded { content, body }
                }
                // A review runs git for a while: on a task of its own, so the thread's frames
                // go on meanwhile.
                Some(Command::Review { scope }) => {
                    let (snapshots, reviewed) = (threads.snapshots.clone(), reviewed_tx.clone());
                    tokio::spawn(async move {
                        let _gone = reviewed.send(snapshots.review(thread, scope).await);
                    });
                    continue;
                }
            },
            Some(review) = reviewed.recv() => ThreadFrame::Review(Box::new(review)),
        };
        if let Err(e) = out.send(&frame).await {
            tracing::debug!(%thread, error = %e, "thread stream ended");
            return;
        }
    };
    tracing::debug!(%thread, why, "thread stream finished");
    let _finished = out.finish();
}

impl Threads {
    /// The threads held.
    pub(crate) const fn host(&self) -> &Host {
        &self.host
    }

    /// The whole of `content`, clipped in `thread`, from the adapter that made it.
    async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        let meta = self.host.state(thread).map(|(state, _)| state.meta);
        match meta.and_then(|m| m.terminal.filter(|_| m.agent.is(AgentId::CLAUDE_CODE))) {
            Some(session) => self.claude.expand(session, content).await,
            None => Expanded::Gone,
        }
    }
}

/// A terminal for an agent's own TUI: `command` in `cwd`, with `env`, titled by its program
/// until the program titles it.
fn opening(
    command: Vec<String>,
    cwd: String,
    env: Vec<(String, String)>,
) -> slopty_proto::terminal::OpenSession {
    let program = command.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned());
    slopty_proto::terminal::OpenSession {
        size: slopty_proto::terminal::TermSize::default(),
        cwd: Some(cwd),
        command,
        env,
        title: program,
        attach: false,
    }
}

/// A server's tasks start their threads here ([`slopty_proto::orchestration::Verb::StartThread`]),
/// each through its agent's own adapter, at the seat the server chose.
impl orchestrate::TaskThreads for Threads {
    fn start(
        &self,
        thread: orchestrate::TaskThread,
    ) -> orchestrate::BoxFuture<'_, Result<ThreadId, Failure>> {
        Box::pin(async move {
            let orchestrate::TaskThread { start, seat, env, role } = thread;
            let relay = slopty_agent::hooks::relay_beside_this_binary()
                .map(|relay| relay.to_string_lossy().into_owned());
            let seated = Seated { seat, env, role, relay };
            let agent = start.agent.clone();
            tracing::info!(%seat, agent = %agent.0, cwd = start.cwd, "start a task's thread");
            let outcome = if agent.is(AgentId::PI) {
                self.pi.start_seated(Box::new(start), seated).await
            } else if agent.is(AgentId::CLAUDE_CODE) {
                self.claude_start.start_seated(start, seated).await
            } else if agent.is(AgentId::CODEX) {
                self.codex.start_seated(start, seated).await
            } else if slopty_agent::acp::name_of(&agent).is_some() {
                self.acp.start_seated(Box::new(start), seated).await
            } else {
                let why = format!("{} is no agent this machine can start", agent.0);
                return Err(Failure::new(ErrorCode::Unsupported, why));
            };
            match outcome {
                Outcome::Started { thread } => Ok(thread),
                Outcome::Unsupported { cap } => {
                    let why = format!("{} cannot {} through Slopty", agent.0, cap.0);
                    Err(Failure::new(ErrorCode::Unsupported, why))
                }
                Outcome::Refused { reason } => Err(Failure::new(ErrorCode::Failed, reason)),
                Outcome::Done | Outcome::Accepted | Outcome::SetupFailed { .. } => {
                    let why = format!("{} started no thread", agent.0);
                    Err(Failure::new(ErrorCode::Failed, why))
                }
            }
        })
    }

    fn close(&self, seat: SessionId) -> orchestrate::BoxFuture<'_, Result<bool, Failure>> {
        Box::pin(async move {
            let Some(thread) = self.host.seated_at(seat) else { return Ok(false) };
            let Some((state, _)) = self.host.state(thread) else { return Ok(false) };
            tracing::info!(%seat, %thread, "end a task's thread");
            self.end(&state).await.map_err(|e| Failure::new(ErrorCode::Failed, e))?;
            Ok(true)
        })
    }
}

impl Threads {
    /// End `state`'s agent, its session kept to take up again: each adapter ends its own, and
    /// an agent in a terminal ends with it, as a seat's terminal is closed.
    async fn end(&self, state: &ThreadState) -> Result<(), String> {
        let thread = state.meta.id;
        if codex::is_shared(state) {
            self.codex.close(thread).await;
        } else if pi::is_pi(state) {
            self.pi.close(thread).await;
        } else if acp::is_acp(state) {
            self.acp.close(thread).await;
        } else if let Some(terminal) = state.meta.terminal {
            self.worker.close(terminal).await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// The thread a server task started at `seat`, when one runs in no terminal of its own:
    /// what the server delivers to the seat goes to it as a message.
    #[must_use]
    pub fn seated_without_terminal(&self, seat: SessionId) -> Option<ThreadId> {
        let thread = self.host.seated_at(seat)?;
        let (state, _) = self.host.state(thread)?;
        state.meta.terminal.is_none().then_some(thread)
    }

    /// Send `text` to the thread seated at `seat`, once for delivery `batch`: queued behind the
    /// turn under way where its agent queues, else steered into it. The words are the reports
    /// block a terminal agent's hooks hand it.
    pub fn deliver(&self, seat: SessionId, batch: u64, text: &str) -> Outcome {
        let Some(thread) = self.host.seated_at(seat) else {
            return refused("no thread is seated there".to_owned());
        };
        let id = IntentId::from_uuid(
            *ThreadId::derived(&["seat delivery", &seat.to_string(), &batch.to_string()]).as_uuid(),
        );
        let decided = self.host.intent(thread, id, |state| {
            let delivery = state.meta.delivery_after_turn();
            let intent = Intent::Send { text: text.to_owned(), delivery, attachments: Vec::new() };
            if !state.meta.can(intent.needs()) {
                return (Outcome::Unsupported { cap: Cap::named(intent.needs()) }, Vec::new());
            }
            let by = Answerer { client: None, name: "Slopty".to_owned() };
            let outcome = if codex::is_shared(state) {
                self.codex.send(thread, text.to_owned(), Vec::new(), delivery, id);
                Outcome::Done
            } else if pi::is_pi(state) {
                self.pi.decide(state, id, &intent, by)
            } else if acp::is_acp(state) {
                self.acp.decide(state, id, &intent, by)
            } else {
                return self.composer.decide(state, id, &intent).unwrap_or_else(|| {
                    (refused("its agent takes no message here".to_owned()), Vec::new())
                });
            };
            (outcome, Vec::new())
        });
        decided.unwrap_or_else(|| refused("no such thread".to_owned()))
    }
}
