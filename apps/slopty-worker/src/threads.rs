//! The thread host on the daemon (`slopty_worker::thread`).
//!
//! Every agent runs in its own TUI in one of the worker's terminals (`docs/decisions/agents.md`,
//! "TUI-first"): a start opens it there ([`slopty_worker::thread::start`]), and every Claude
//! Code in any of them is read as threads from its transcript and its hooks
//! ([`slopty_worker::thread::claude`]). They are served to clients read-only: the table on the
//! control stream, a stream per followed thread, the person's own marks on them, and the
//! repository each works in for review.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use slopty_core::{ClientId, SessionId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::orchestration::ErrorCode;
use slopty_proto::terminal::OpenSession;
use slopty_proto::thread::wire::{
    Expanded, Intent, IntentDone, Outcome, PAST_RUNNING, PastSessions, ReviewScope, Setup, Start,
    TableFrame, ThreadFrame, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{
    AgentId, Cap, ContentRef, Cursor, IntentId, Liveness, Status, ThreadId, ThreadMeta,
    ThreadState, TurnId,
};
use slopty_proto::visual::AppAnswer;
use slopty_worker::ItemStore;
use slopty_worker::manager::Worker;
use slopty_worker::orchestrate::{self, Failure};
use slopty_worker::repo::worktrees;
use slopty_worker::thread::claude::{self, Claude, Place};
use slopty_worker::thread::history::{self, History};
use slopty_worker::thread::host::UnderWay;
use slopty_worker::thread::review::Reviews;
use slopty_worker::thread::screens::Screens;
use slopty_worker::thread::start::{Starter, Terminals};
use slopty_worker::thread::{Follower, Host, Seated};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::Daemon;

pub mod awake;
mod starts;

/// The daemon's threads and what serves them: the Claude Code observer, the starter, the
/// repositories for review and the past prompts. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Threads {
    host: Host,
    claude: Claude,
    starter: Starter,
    reviews: Reviews,
    history: History,
    worker: Worker,
    starts: starts::Starts,
}

/// Open the threads kept under `dir`, their agents' terminals opened on `worker` and told to
/// every client on `events` and in `items`, as any terminal is. A host that cannot open is
/// warned of, and the daemon goes on without threads.
pub fn open(
    dir: &Path,
    worker: Worker,
    events: broadcast::Sender<WorkerMsg>,
    items: ItemStore,
    machine: String,
) -> Option<Threads> {
    let host = match Host::open(dir, slopty_worker::thread::log::Limits::default()) {
        Ok(host) => host,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "the thread host did not open: {e}");
            return None;
        }
    };
    let cli = slopty_agent::mcp::cli_beside_this_binary();
    host.set_tools_cli(cli.map(|cli| cli.to_string_lossy().into_owned()));
    // The threads live under the data directory, where pi's tools extension is written too.
    let data = dir.parent().unwrap_or(dir).to_path_buf();
    let place = || Place { home: slopty_platform::dirs::home(), path: None, data: data.clone() };
    let (claude, _watching) = Claude::spawn(host.clone(), place());
    let terminals = Arc::new(Opener { worker: worker.clone(), events, items });
    let starter = Starter::new(host.clone(), claude.clone(), place(), terminals, machine);
    let git = slopty_worker::changes::git().map(Path::to_path_buf);
    Some(Threads {
        reviews: Reviews::new(host.clone(), git),
        history: History::new(history::Stores::here(), history::Limits::default()),
        host,
        claude,
        starter,
        worker,
        starts: starts::Starts::default(),
    })
}

/// The worker's terminals, as a start opens one: under the id it chose, told to every client
/// and put in the item registry, as the orchestrator's are.
#[derive(Debug)]
struct Opener {
    worker: Worker,
    events: broadcast::Sender<WorkerMsg>,
    items: ItemStore,
}

impl Terminals for Opener {
    fn open(
        &self,
        id: SessionId,
        req: OpenSession,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
        Box::pin(async move {
            self.worker.open_as(id, &req).await.map_err(|e| e.to_string())?;
            if let Some(summary) = self.worker.summary(id).await {
                let _nobody = self.events.send(WorkerMsg::SessionChanged(summary));
            }
            if let Some(delta) = self.items.ensure_terminal(id, ClientId::nil()) {
                let _nobody = self.events.send(WorkerMsg::Items(delta));
            }
            Ok(())
        })
    }

    fn env(&self, id: SessionId) -> Vec<(String, String)> {
        self.worker.seat_env(id)
    }
}

/// Serve the daemon's threads: name the screens each agent drives, rest the threads unused,
/// keep the machine awake for those at work, sweep the agents' landed worktrees, and bring the
/// starts a worker before this one left under way to an outcome.
pub fn start(daemon: &Daemon) {
    let Some(threads) = &daemon.threads else { return };
    drop(Screens::new(threads.host.clone()).spawn());
    drop(tokio::spawn(rest(threads.host.clone())));
    drop(tokio::spawn(awake::watch(daemon.clone(), threads.host.clone())));
    drop(tokio::spawn(sweep_worktrees(threads.clone())));
    resume_starts(threads);
}

/// How long a thread opened goes unused before it goes back to its head ([`Host::rest`]).
const REST: Duration = Duration::from_mins(10);

/// How often the threads opened are looked over for those that rested.
const REST_CHECK: Duration = Duration::from_mins(1);

/// Put the threads that rested back to their heads, every [`REST_CHECK`], for as long as the
/// worker runs: a worker up for weeks holds the threads in use, not every one ever opened.
async fn rest(host: Host) -> ! {
    let mut every = tokio::time::interval(REST_CHECK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let host = host.clone();
        match tokio::task::spawn_blocking(move || host.rest(REST)).await {
            Ok(0) => {}
            Ok(rested) => tracing::debug!(rested, "threads back to their heads"),
            Err(e) => tracing::warn!(error = %e, "the threads were not rested"),
        }
    }
}

/// Why a start whose agent was opening when the worker stopped is said to have failed.
const OPENING_LOST: &str = "The worker restarted while this start opened its agent. Look for \
    its thread before starting it again";

/// Bring each start a worker before this one left under way to an outcome.
///
/// No client then waits on one nobody holds. One still making or setting up its worktree is
/// taken up again from the top, as a client's repeat of it would be: the worktree is reopened
/// and its setup, which never finished, runs again, the one the old worker left running ended
/// first. One whose agent was opening may have opened it, so it is said to have failed rather
/// than open a second agent.
fn resume_starts(threads: &Threads) {
    for UnderWay { id, start, begun } in threads.host.starts_under_way() {
        if begun {
            tracing::warn!(%id, "a start was opening its agent when the worker stopped");
            let _said = threads.host.record_start(id, refused(OPENING_LOST.to_owned()));
            continue;
        }
        tracing::info!(%id, agent = %start.agent.0, "a start the worker stopped during goes on");
        let again = threads.clone();
        let _followed =
            threads.starts.clone().take_up(id, |said| run_start(again, id, Box::new(start), said));
    }
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
    /// The client.
    pub client: ClientId,
    /// The connection's tasks, which end with it.
    pub tasks: &'a mut JoinSet<()>,
}

impl Origin<'_> {
    /// Answer the client: at once while its control queue has room, else on a task of the
    /// connection's that waits for room ([`post_on`]).
    fn post(&mut self, msg: WorkerMsg) {
        post_on(self.out, self.tasks, msg);
    }
}

/// Queue `msg` on `out` without holding up the connection's reader: at once while there is
/// room, else on one of `tasks`, which waits for room and ends with the connection.
///
/// An intent's answer is never dropped. The client keeps the intent pending until it comes
/// (an interrupt, a model change, a fork), and nothing would send it again short of a relink.
fn post_on(out: &mpsc::Sender<WorkerMsg>, tasks: &mut JoinSet<()>, msg: WorkerMsg) {
    match out.try_send(msg) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
        Err(mpsc::error::TrySendError::Full(msg)) => {
            let out = out.clone();
            tasks.spawn(async move {
                let _gone = out.send(msg).await;
            });
        }
    }
}

/// What one connection follows of the threads.
#[derive(Debug, Default)]
pub struct Following {
    /// The task keeping the client's table current, replaced by its next ask.
    table: Option<AbortHandle>,
    /// Each followed thread's task, which takes its requests here and ends when the sender
    /// goes.
    threads: HashMap<ThreadId, mpsc::UnboundedSender<Command>>,
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
    App { id: u64 },
}

impl Following {
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
                if !threads.host.holds(thread) {
                    tracing::debug!(client = %at.client, %thread, "follow of a thread not here");
                    return;
                }
                tracing::info!(client = %at.client, %thread, ?have, "follow thread");
                let follower =
                    Follower::new(threads.host.clone(), thread, have, turns, max_latency_ms);
                let (commands, taken) = mpsc::unbounded_channel();
                at.tasks.spawn(stream(threads, at.conn.clone(), follower, taken));
                self.threads.insert(thread, commands);
            }
            ThreadRequest::Unfollow { thread } => {
                if self.threads.remove(&thread).is_some() {
                    tracing::info!(client = %at.client, %thread, "unfollow thread");
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
            ThreadRequest::App { thread, id, .. } => {
                self.command(at, thread, Command::App { id });
            }
            ThreadRequest::Intent { id, thread, intent } => {
                let outcome = mark(&threads, thread, id, &intent);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            // An agent starts in a terminal, in the worktree it names once that is made and set
            // up, the setup said as it goes: on the daemon's own task, which outlives this link,
            // and is followed from here. The same start asked again, after a relink, follows the
            // one under way or gets its outcome.
            ThreadRequest::Start { id, mut start } => {
                // A start's folder may be spelled from the home, as a client that knows no
                // better spells it.
                let cwd = slopty_worker::file::expand_home(Path::new(&start.cwd));
                start.cwd = cwd.to_string_lossy().into_owned();
                tracing::info!(client = %at.client, %id, agent = %start.agent.0, cwd = start.cwd, "start");
                if let Some(outcome) = threads.host.started(id) {
                    at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
                    return;
                }
                let followed =
                    threads.starts.clone().take_up(id, |said| run_start(threads, id, start, said));
                at.tasks.spawn(starts::follow(id, followed, at.out.clone()));
            }
            // An agent's sessions listed, or the past prompts searched: on a task of its own.
            ThreadRequest::Sessions { agent, cwd, query, limit } => {
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let listed = sessions(&threads, agent, cwd, query, limit).await;
                    let _gone = out.send(WorkerMsg::Sessions(listed)).await;
                });
            }
        }
    }

    fn command(&self, at: &Origin<'_>, thread: ThreadId, command: Command) {
        match self.threads.get(&thread) {
            Some(task) => {
                let _gone = task.send(command);
            }
            None => {
                tracing::debug!(client = %at.client, %thread, "a thread request while not following");
            }
        }
    }
}

/// Act on the person's own mark `intent` on `thread` once, for intent `id`: how far they have
/// seen it, or their name for it. Every other intent is a turn's or its agent's, which the
/// person does in the agent's own terminal (`docs/decisions/agents.md`, "TUI-first").
fn mark(threads: &Threads, thread: ThreadId, id: IntentId, intent: &Intent) -> Outcome {
    if !matches!(intent, Intent::Seen { .. } | Intent::Name { .. }) {
        let cap = intent.needs().unwrap_or(IN_ITS_TERMINAL);
        return Outcome::Unsupported { cap: Cap::named(cap) };
    }
    let now = slopty_core::WallMs::now();
    let marked = threads
        .host
        .intent(thread, id, |state| slopty_worker::thread::marks::act(state, intent, now));
    marked.unwrap_or_else(|| refused("no such thread".to_owned()))
}

/// What an intent no thread takes from Slopty is done in instead.
const IN_ITS_TERMINAL: &str = "take this from Slopty: do it in its terminal";

/// Start a thread for intent `id` as `start` says, in the worktree it names once that is made
/// and set up, telling `said` what the setup says as it goes. Its outcome is recorded with the
/// host's starts, so a repeat of the id long after gets it back and sets nothing up again.
/// Until then it is kept as under way ([`Host::start_under_way`]), for a worker that starts
/// after this one stopped ([`resume_starts`]).
async fn run_start(
    threads: Threads,
    id: IntentId,
    mut start: Box<Start>,
    said: watch::Sender<Option<Setup>>,
) -> Outcome {
    // What a setup says is drawn over again, so one lost to a full link is not missed.
    let say = |setup: &Setup| {
        said.send_replace(Some(setup.clone()));
    };
    threads.host.start_under_way(id, &start);
    let outcome = match worktrees::enter(&mut start, &say).await {
        Ok(_) => {
            threads.host.start_begins(id);
            threads.starter.start(id, &start, None).await
        }
        Err(worktrees::Failed::Setup(failed)) => {
            Outcome::SetupFailed { setup: failed.setup, code: failed.code }
        }
        Err(failed) => refused(failed.to_string()),
    };
    threads.host.record_start(id, outcome)
}

/// Every folder worked in now: each thread's the worker holds, and each live terminal's.
async fn worked_in(threads: &Threads) -> Vec<PathBuf> {
    let held = threads.host.visit(|other| {
        let cwd = &other.meta.cwd;
        (!cwd.is_empty()).then(|| slopty_worker::file::expand_home(Path::new(cwd)))
    });
    let terminals = threads
        .worker
        .summaries()
        .await
        .into_iter()
        .filter(|s| matches!(s.state, slopty_proto::terminal::SessionState::Running))
        .filter_map(|s| s.cwd)
        .map(|cwd| slopty_worker::file::expand_home(Path::new(&cwd)));
    held.into_iter().chain(terminals).collect()
}

/// How long after the worker starts it first sweeps the agents' worktrees: out of the way of
/// the start's own work.
const SWEEP_FIRST: Duration = Duration::from_mins(10);

/// How often the agents' worktrees whose work landed are swept away ([`worktrees::sweep`]).
const SWEEP_EVERY: Duration = Duration::from_hours(24);

/// Sweep the agents' worktrees every [`SWEEP_EVERY`], for as long as the worker runs.
async fn sweep_worktrees(threads: Threads) -> ! {
    tokio::time::sleep(SWEEP_FIRST).await;
    let mut every = tokio::time::interval(SWEEP_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        sweep_once(&threads).await;
    }
}

/// Sweep each clone a thread the worker holds works in, once ([`worktrees::sweep`]): what went
/// and what was kept is logged, and nothing a thread or a live terminal works in is touched.
async fn sweep_once(threads: &Threads) {
    let Some(git) = slopty_worker::changes::git() else { return };
    let programs = slopty_worker::repo::commit::Programs::here().await;
    let cwds = worked_in(threads).await;
    let clones = {
        let cwds = cwds.clone();
        tokio::task::spawn_blocking(move || {
            let mut clones: Vec<PathBuf> =
                cwds.iter().map(PathBuf::as_path).filter_map(worktrees::main_checkout).collect();
            clones.sort();
            clones.dedup();
            clones
        })
        .await
        .unwrap_or_default()
    };
    for clone in clones {
        match programs.scope(worktrees::sweep(git, &programs, &clone, &cwds)).await {
            Ok(swept) => {
                for (tree, removed) in &swept.removed {
                    tracing::info!(
                        tree = %tree.display(),
                        branch = ?removed.branch,
                        branch_removed = removed.branch_removed,
                        "a landed worktree swept"
                    );
                }
                for (_tree, _commits, words) in &swept.unpushed {
                    tracing::info!("a landed worktree kept: {words}");
                }
            }
            Err(e) => tracing::debug!(clone = %clone.display(), "no sweep: {e}"),
        }
    }
}

/// The past sessions [`ThreadRequest::Sessions`] asks for, at most `limit`: Claude Code's in
/// folder `cwd` as it lists them, with no `query`; else those the person's past prompts find
/// ([`History`]), the last prompted first for no words. Each is named by the thread held of it
/// here, if one is, and a Claude Code session another live Claude Code on the machine holds
/// says how that one runs ([`PAST_RUNNING`]); why there are none, when they could not be had.
async fn sessions(
    threads: &Threads,
    agent: Option<AgentId>,
    cwd: Option<String>,
    query: String,
    limit: u32,
) -> PastSessions {
    let most = usize::try_from(limit).unwrap_or(usize::MAX);
    let (sessions, absent, cut) = match (&agent, &cwd) {
        (Some(agent), Some(cwd)) if query.trim().is_empty() && agent.is(AgentId::CLAUDE_CODE) => {
            let (sessions, absent) = claude_sessions(cwd, most).await;
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
                    limit: most,
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
    let held = held_elsewhere(threads, &sessions).await;
    for past in &mut sessions {
        if past.agent.is(AgentId::CLAUDE_CODE)
            && let Some(holder) = held.get(&past.native)
        {
            past.facts.insert(PAST_RUNNING.to_owned(), holder.kind.clone());
        }
        if let Some((thread, title)) = threads.host.session(&past.agent, &past.native) {
            past.thread = Some(thread);
            if !title.trim().is_empty() && (past.title.is_none() || !past.prompts.is_empty()) {
                past.title = Some(title);
            }
        }
    }
    PastSessions { agent, cwd, query, sessions, absent, cut }
}

/// The Claude Code sessions among `sessions` that a live Claude Code outside Slopty's terminals
/// holds, by id: the threads this worker reads in its terminals are not.
async fn held_elsewhere(
    threads: &Threads,
    sessions: &[slopty_proto::thread::wire::PastSession],
) -> HashMap<String, claude::held::Holder> {
    if !sessions.iter().any(|past| past.agent.is(AgentId::CLAUDE_CODE)) {
        return HashMap::new();
    }
    let ours: HashSet<String> = threads
        .host
        .visit(|state| {
            (claude::is_claude(state) && state.status.liveness == Liveness::Live)
                .then(|| state.meta.native.clone())
        })
        .into_iter()
        .collect();
    let home = slopty_platform::dirs::home();
    tokio::task::spawn_blocking(move || claude::held::holders(&home, &ours))
        .await
        .unwrap_or_default()
}

/// Claude Code's past sessions in folder `cwd`, at most `most`, the last first, as it keeps
/// them; why there are none, when they could not be had.
async fn claude_sessions(
    cwd: &str,
    most: usize,
) -> (Vec<slopty_proto::thread::wire::PastSession>, Option<String>) {
    let dir = slopty_worker::file::expand_home(Path::new(cwd));
    let home = slopty_platform::dirs::home();
    let listed = tokio::task::spawn_blocking(move || {
        slopty_agent::claude::sessions::sessions(&home, &dir, most)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|listed| listed.map_err(|e| format!("Claude Code's sessions: {e}")));
    match listed {
        Ok(mut sessions) => {
            sessions.truncate(most);
            (sessions, None)
        }
        Err(why) => (Vec::new(), Some(why)),
    }
}

const fn refused(reason: String) -> Outcome {
    Outcome::Refused { reason }
}

/// The daemon's threads as orchestration's verbs read them
/// ([`slopty_worker::orchestrate::ThreadReads`]).
#[derive(Clone)]
pub struct Reads {
    host: Host,
}

impl Reads {
    /// `threads`, as orchestration reaches them.
    #[must_use]
    pub fn new(threads: &Threads) -> Self {
        Self { host: threads.host.clone() }
    }
}

impl orchestrate::ThreadReads for Reads {
    fn state(&self, thread: ThreadId) -> Option<ThreadState> {
        self.host.state(thread).map(|(state, _)| state)
    }

    fn standing(&self, thread: ThreadId) -> Option<(ThreadMeta, Status)> {
        self.host.meta(thread)
    }

    fn seated_at(&self, session: SessionId) -> Option<ThreadRow> {
        let thread = self.host.seated_at(session)?;
        let (state, _) = self.host.state(thread)?;
        Some(state.row(slopty_core::WallMs::now()))
    }
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

/// The review a thread's stream is reading, if any: aborted when another starts and when the
/// stream ends, its git children killed with it.
#[derive(Debug, Default)]
struct Reviewing(Option<tokio::task::JoinHandle<()>>);

impl Reviewing {
    /// Read `task` in place of the review before it.
    fn start(&mut self, task: tokio::task::JoinHandle<()>) {
        if let Some(before) = self.0.replace(task) {
            before.abort();
        }
    }
}

impl Drop for Reviewing {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Why a page shown in a thread gets no answer from its MCP App.
const NO_APP_DOOR: &str =
    "Its agent runs in its own terminal, which opens no door to its MCP servers here";

/// A followed thread's stream: what the client lacks from its cursor, then every change, with
/// the pages, expansions and reviews it asks for between them; finished when it unfollows or
/// the thread goes.
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
    // What runs git comes back here from a task of its own, so the thread's frames go on.
    let (slow_tx, mut slow) = mpsc::unbounded_channel();
    // The review being read, stopped when another is asked for or the stream ends: only the
    // latest is shown, and a big one need not run on for nobody.
    let mut reviewing = Reviewing::default();
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
                // A side of a file a review showed is read from git, on a task of its own.
                Some(Command::Expand { content }) if content.blob_id().is_some() => {
                    let (reviews, done) = (threads.reviews.clone(), slow_tx.clone());
                    tokio::spawn(async move {
                        let id = content.blob_id().unwrap_or_default();
                        let body = reviews.blob(thread, id).await;
                        let _gone = done.send(ThreadFrame::Expanded { content, body });
                    });
                    continue;
                }
                Some(Command::Expand { content }) => {
                    let body = threads.expand(thread, content.clone()).await;
                    ThreadFrame::Expanded { content, body }
                }
                Some(Command::App { id }) => {
                    ThreadFrame::App { id, answer: AppAnswer::Refused(NO_APP_DOOR.to_owned()) }
                }
                // A review runs git for a while: on a task of its own, so the thread's frames
                // go on meanwhile.
                Some(Command::Review { scope }) => {
                    let (reviews, done) = (threads.reviews.clone(), slow_tx.clone());
                    reviewing.start(tokio::spawn(async move {
                        let review = reviews.review(thread, scope).await;
                        let _gone = done.send(ThreadFrame::Review(Box::new(review)));
                    }));
                    continue;
                }
            },
            Some(frame) = slow.recv() => frame,
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

    /// The Claude Code observer, which the daemon's ticks and hooks feed.
    pub(crate) const fn claude(&self) -> &Claude {
        &self.claude
    }

    /// The whole of `content`, clipped in `thread`.
    async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        match self.host.state(thread) {
            // A visual's or a page's file, by its item and name, whichever agent showed it.
            Some((state, _)) if content.item_asset_of().is_some() => {
                slopty_worker::thread::assets::read(&state, &content).await
            }
            Some((state, _)) if claude::is_claude(&state) => {
                self.claude.expand(thread, content).await
            }
            Some(_) | None => Expanded::Gone,
        }
    }
}

/// The server starts seated threads here ([`slopty_proto::orchestration::Verb::StartThread`]),
/// each in a terminal opened under the seat the server chose.
impl orchestrate::SeatedThreads for Threads {
    fn start(
        &self,
        thread: orchestrate::SeatedThread,
    ) -> orchestrate::BoxFuture<'_, Result<ThreadId, Failure>> {
        Box::pin(async move {
            let orchestrate::SeatedThread { start, seat, facts, system } = thread;
            let seated = Seated { seat, facts, system };
            let agent = start.agent.clone();
            tracing::info!(%seat, agent = %agent.0, cwd = start.cwd, "start a seated thread");
            let outcome = self.starter.start(seated.intent(), &start, Some(&seated)).await;
            started(&agent, outcome)
        })
    }
}

/// The thread `agent`'s start answered with `outcome`, or why there is none.
fn started(agent: &AgentId, outcome: Outcome) -> Result<ThreadId, Failure> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A review asked for again stops the one still being read, and the stream's end stops the
    /// last: only the latest is read.
    #[tokio::test]
    async fn a_review_asked_again_stops_the_one_before() {
        let mut reviewing = Reviewing::default();
        let first = tokio::spawn(std::future::pending::<()>());
        let first_handle = first.abort_handle();
        reviewing.start(first);
        let second = tokio::spawn(std::future::pending::<()>());
        let second_handle = second.abort_handle();
        reviewing.start(second);
        tokio::task::yield_now().await;
        assert!(first_handle.is_finished(), "the earlier review stopped");
        assert!(!second_handle.is_finished(), "the latest goes on");
        drop(reviewing);
        tokio::task::yield_now().await;
        assert!(second_handle.is_finished(), "the stream's end stops it");
    }

    /// An intent's answer that finds the client's control queue full waits for room rather
    /// than being dropped, and comes after what was queued before it.
    #[tokio::test]
    async fn an_intent_s_answer_waits_for_room_in_a_full_queue() {
        let (out, mut rx) = mpsc::channel(1);
        let mut tasks = JoinSet::new();
        let first = IntentId::new();
        let done = |id| WorkerMsg::IntentDone(IntentDone { id, outcome: refused("no".to_owned()) });
        post_on(&out, &mut tasks, done(first));
        let second = IntentId::new();
        post_on(&out, &mut tasks, done(second));
        let ids = [rx.recv().await, rx.recv().await].map(|msg| match msg {
            Some(WorkerMsg::IntentDone(done)) => done.id,
            other => panic!("{other:?}"),
        });
        assert_eq!(ids, [first, second], "the full queue's answer came once there was room");
        drop(out);
        while tasks.join_next().await.is_some() {}
        assert!(rx.recv().await.is_none(), "each answer once");
    }
}
