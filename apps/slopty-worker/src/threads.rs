//! The thread host on the daemon (`slopty_worker::thread`).
//!
//! Every Claude Code session the daemon sees is observed into the agent-neutral thread model,
//! beside today's conversation path, every Codex thread is followed, and a pi thread or the thread
//! of any ACP agent is started and driven here. A Claude Code thread is started in one of the
//! daemon's terminals and observed, and a Codex thread is started over the person's Codex daemon.
//! They are served to clients: the table on the control stream, a stream per followed thread, and
//! intents answered once each.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slopty_core::{ClientId, SessionId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::thread::wire::{
    Expanded, Intent, IntentDone, Outcome, PastSessions, ReviewScope, TableFrame, ThreadFrame,
    ThreadRequest,
};
use slopty_proto::thread::{
    Action, AgentId, AskId, Cap, ContentRef, Cursor, Delivery, IntentId, ThreadId, ThreadState,
    TurnId,
};
use slopty_worker::conversation::Seen;
use slopty_worker::manager::Worker;
use slopty_worker::orchestrate::{self, Agents, Conversations as _};
use slopty_worker::session::SessionHandle;
use slopty_worker::thread::acp::{self, Acp};
use slopty_worker::thread::claude::{self, Driver, Sources};
use slopty_worker::thread::codex::{self, Codex};
use slopty_worker::thread::compose::Terminals;
use slopty_worker::thread::pi::{self, Pi};
use slopty_worker::thread::review::Snapshots;
use slopty_worker::thread::terminals::{Pending, Terminals as AgentTerminalsTrait};
use slopty_worker::thread::{Composer, Follower, Host};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::Daemon;
use crate::follow::Orchestrated;

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
    fn status(&self, session: SessionId) -> Option<slopty_proto::agent::SessionAgent> {
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
            let (claude, asks) = Driver::channel();
            let (claude_start, claude_start_asks) = claude::start::Starter::channel();
            let (codex, codex_asks) = Codex::channel();
            let (pi, pi_asks) = Pi::channel();
            let (acp, acp_asks) = Acp::channel();
            let terminals = Arc::new(AgentTerminals(worker.clone()));
            let typing = Typing { worker, agents: crate::server::DaemonAgents(agents) };
            let composer = Composer::new(host.clone(), Arc::new(typing));
            let git = slopty_worker::changes::git().map(Path::to_path_buf);
            let snapshots = Snapshots::new(host.clone(), snapshots, git);
            // The threads live under the data directory, where pi's gate is written too.
            let data = dir.parent().unwrap_or(dir).to_path_buf();
            let observing = Observing {
                claude: asks,
                claude_start: claude_start_asks,
                codex: codex_asks,
                pi: pi_asks,
                acp: acp_asks,
                data,
                terminals,
            };
            let threads =
                Threads { host, claude, claude_start, codex, pi, acp, composer, snapshots };
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
            let program = command.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_owned());
            let open = slopty_proto::terminal::OpenSession {
                size: slopty_proto::terminal::TermSize::default(),
                cwd: Some(cwd),
                command,
                env,
                title: program,
                attach: false,
            };
            self.0.open(&open).await.map(|session| session.id()).map_err(|e| e.to_string())
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
/// and snapshot each turn.
pub fn start(daemon: &Daemon, asks: Observing) {
    let Some(threads) = &daemon.threads else { return };
    drop(threads.snapshots.spawn());
    threads.composer.resume();
    let sources: Arc<dyn Sources> = Arc::new(Observed(daemon.clone()));
    drop(claude::spawn(threads.host.clone(), daemon.events.subscribe(), sources, asks.claude));
    let terminals: Arc<dyn AgentTerminalsTrait> = asks.terminals;
    drop(claude::start::spawn(
        threads.host.clone(),
        threads.claude.clone(),
        Arc::clone(&terminals),
        None,
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
    pub fn follows_session(&self, session: SessionId) -> bool {
        self.threads.values().any(|(_, terminal)| *terminal == Some(session))
    }

    /// Take `req` from the client on `at`. `conversations` says whether the connection follows
    /// a session's conversation the old way, which holds its prompts too.
    pub fn handle(
        &mut self,
        at: &mut Origin<'_>,
        req: ThreadRequest,
        conversations: &dyn Fn(SessionId) -> bool,
    ) {
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
                if let Some(session) = terminal {
                    let mut follows = at.daemon.follows.lock();
                    let ids = follows.holds.follow(session, at.link);
                    crate::follow::show_held(&follows, ids, |msg| at.post(msg));
                    drop(follows);
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
                    && !conversations(session)
                {
                    let released = at.daemon.follows.lock().holds.unfollow(session, at.link);
                    crate::follow::release(at.daemon, released);
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
            ThreadRequest::Intent { id, thread, intent } => {
                let outcome = act(at, &threads, thread, id, &intent);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            // pi looks for its program and starts: on a task of its own.
            ThreadRequest::Start { id, start } if start.agent.is(AgentId::PI) => {
                tracing::info!(client = %at.client, %id, cwd = start.cwd, "start pi");
                let (pi, out) = (threads.pi, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = pi.start(id, start).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // Claude Code opens in a terminal, and Codex's daemon is asked: on tasks of their own.
            ThreadRequest::Start { id, start } if start.agent.is(AgentId::CLAUDE_CODE) => {
                tracing::info!(client = %at.client, %id, cwd = start.cwd, "start Claude Code");
                let (claude, out) = (threads.claude_start, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = claude.start(id, *start).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            ThreadRequest::Start { id, start } if start.agent.is(AgentId::CODEX) => {
                tracing::info!(client = %at.client, %id, cwd = start.cwd, "start Codex");
                let (codex, out) = (threads.codex, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = codex.start(id, *start).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // An ACP agent is looked for and starts: on a task of its own.
            ThreadRequest::Start { id, start }
                if slopty_agent::acp::name_of(&start.agent).is_some() =>
            {
                tracing::info!(client = %at.client, %id, agent = %start.agent.0, cwd = start.cwd, "start");
                let (acp, out) = (threads.acp, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = acp.start(id, start).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            ThreadRequest::Start { id, start } => {
                tracing::info!(client = %at.client, %id, agent = %start.agent.0, "start refused");
                let reason = format!("{} is no agent this machine can start", start.agent.0);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome: refused(reason) }));
            }
            // An agent is asked, or its session directory listed: on a task of its own.
            ThreadRequest::Sessions { agent, cwd, limit } => {
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let listed = sessions(&threads, agent, cwd, limit).await;
                    let _gone = out.send(WorkerMsg::Sessions(listed)).await;
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

/// Agent `agent`'s past sessions in folder `cwd`, at most `limit`, the last first, as the agent
/// keeps them: each named by the thread held of it here, if one is; why there are none, when
/// they could not be had.
async fn sessions(threads: &Threads, agent: AgentId, cwd: String, limit: u32) -> PastSessions {
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
    for past in &mut sessions {
        if let Some((thread, title)) = threads.host.session(&agent, &past.native) {
            past.thread = Some(thread);
            if past.title.is_none() && !title.trim().is_empty() {
                past.title = Some(title);
            }
        }
    }
    PastSessions { agent, cwd, sessions, absent }
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
    let decided = threads.host.intent(thread, id, |state| decide(at, threads, state, id, intent));
    decided.unwrap_or_else(|| refused("no such thread".to_owned()))
}

/// What comes of `intent` on `state`'s thread, done as it is decided.
fn decide(
    at: &Origin<'_>,
    threads: &Threads,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> (Outcome, Vec<Action>) {
    let needs = intent.needs();
    if !state.meta.can(needs) {
        return (Outcome::Unsupported { cap: Cap::named(needs) }, Vec::new());
    }
    if codex::is_shared(state) {
        return (shared(at, &threads.codex, state, id, intent), Vec::new());
    }
    if pi::is_pi(state) {
        let by = slopty_proto::thread::Answerer { client: Some(at.client), name: "Slopty".into() };
        return (threads.pi.decide(state, id, intent, by), Vec::new());
    }
    if acp::is_acp(state) {
        let by = slopty_proto::thread::Answerer { client: Some(at.client), name: "Slopty".into() };
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
                (Some(held), Some(verdict)) => answered(crate::follow::answer(
                    at.daemon, at.link, at.client, session, held, verdict,
                )),
            }
        }
        Intent::Release { ask } => match held(ask) {
            Some(held) => {
                answered(crate::follow::hand_back(at.daemon, at.link, at.client, session, held))
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
    at: &Origin<'_>,
    codex: &Codex,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    let thread = state.meta.id;
    if matches!(state.status.liveness, slopty_proto::thread::Liveness::Exited { .. }) {
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
            let by = slopty_proto::thread::Answerer {
                client: Some(at.client),
                name: "Slopty".to_owned(),
            };
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
        | Intent::Reorder { pending, .. }
            if !state.pending.iter().any(|p| p.intent == *pending) =>
        {
            refused("That message is not waiting".to_owned())
        }
        Intent::Reorder { before: Some(before), .. }
            if !state.pending.iter().any(|p| p.intent == *before) =>
        {
            refused("That message has already gone".to_owned())
        }
        Intent::Withdraw { pending } => {
            codex.withdraw(thread, *pending);
            Outcome::Done
        }
        Intent::Promote { pending } => {
            codex.promote(thread, *pending);
            Outcome::Done
        }
        Intent::Reorder { pending, before } => {
            codex.reorder(thread, *pending, *before);
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
    /// The whole of `content`, clipped in `thread`, from the adapter that made it.
    async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        let meta = self.host.state(thread).map(|(state, _)| state.meta);
        match meta.and_then(|m| m.terminal.filter(|_| m.agent.is(AgentId::CLAUDE_CODE))) {
            Some(session) => self.claude.expand(session, content).await,
            None => Expanded::Gone,
        }
    }
}
