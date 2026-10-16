//! The thread host on the daemon (`slopty_worker::thread`).
//!
//! Every agent's thread is driven here through the agent's own door: Claude Code as `claude -p`
//! in ptyd's custody, Codex over the person's Codex daemon, pi over its RPC mode, any other
//! agent over ACP. They are served to clients: the table on the control stream, a stream per
//! followed thread, and intents answered once each.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use slopty_agent::codex::shared::Setting;
use slopty_core::{ClientId, SessionId};
use slopty_net::{Connection, WorkerMsg};
use slopty_proto::orchestration::ErrorCode;
use slopty_proto::thread::wire::{
    Expanded, Intent, IntentDone, Outcome, PastSessions, ReviewScope, Setup, Start, TableFrame,
    ThreadFrame, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{
    AgentId, Answerer, Cap, ContentRef, Cursor, Delivery, Drive, IntentId, Liveness, Phase,
    ThreadId, ThreadState, TreeRef, TurnId,
};
use slopty_proto::visual::{AppAnswer, AppAsk};
use slopty_worker::manager::Worker;
use slopty_worker::orchestrate::{self, Failure};
use slopty_worker::repo::worktrees;
use slopty_worker::thread::acp::{self, Acp};
use slopty_worker::thread::claude::{self, Claude};
use slopty_worker::thread::codex::{self, Codex};
use slopty_worker::thread::history::{self, History};
use slopty_worker::thread::host::UnderWay;
use slopty_worker::thread::pi::{self, Pi};
use slopty_worker::thread::review::Snapshots;
use slopty_worker::thread::screens::Screens;
use slopty_worker::thread::{Follower, Host, Seated, schedule};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};

use crate::Daemon;

pub mod awake;
mod starts;

/// The daemon's threads and what serves them: each agent's adapter, and the snapshots of their
/// turns. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Threads {
    host: Host,
    claude: Claude,
    codex: Codex,
    pi: Pi,
    acp: Acp,
    snapshots: Snapshots,
    history: History,
    worker: Worker,
    starts: starts::Starts,
}

/// Open the threads kept under `dir`, and what [`start`] serves them with.
///
/// Their snapshots' indexes are kept under `snapshots`, and `worker`'s terminals are where a
/// seat's variables come from. A host that cannot open is warned of, and the daemon goes on
/// without threads.
pub fn open(dir: &Path, snapshots: &Path, worker: Worker) -> Option<(Threads, Adapters)> {
    match Host::open(dir, slopty_worker::thread::log::Limits::default()) {
        Ok(host) => {
            let seats = worker.clone();
            host.set_seat_env(Arc::new(move |seat| seats.seat_env(seat)));
            let (claude, claude_asks) = Claude::channel();
            let (codex, codex_asks) = Codex::channel();
            let (pi, pi_asks) = Pi::channel();
            let (acp, acp_asks) = Acp::channel();
            let git = slopty_worker::changes::git().map(Path::to_path_buf);
            let snapshots = Snapshots::new(host.clone(), snapshots, git);
            // The threads live under the data directory, where pi's gate is written too.
            let data = dir.parent().unwrap_or(dir).to_path_buf();
            let history = History::new(history::Stores::here(), history::Limits::default());
            let adapters = Adapters {
                claude: claude_asks,
                codex: codex_asks,
                pi: pi_asks,
                acp: acp_asks,
                data,
                ptyd: worker.ptyd_socket().to_path_buf(),
            };
            let threads = Threads {
                host,
                claude,
                codex,
                pi,
                acp,
                snapshots,
                history,
                worker,
                starts: starts::Starts::default(),
            };
            Some((threads, adapters))
        }
        Err(e) => {
            tracing::warn!(dir = %dir.display(), "the thread host did not open: {e}");
            None
        }
    }
}

/// What the adapters take their asks from once they start.
#[derive(Debug)]
pub struct Adapters {
    claude: claude::Asks,
    codex: codex::Asks,
    pi: pi::Asks,
    acp: acp::Asks,
    /// The daemon's data directory.
    data: PathBuf,
    /// Where ptyd listens, which keeps each Claude Code.
    ptyd: PathBuf,
}

/// Serve the daemon's threads: drive Claude Code, pi and ACP agents, follow every Codex thread,
/// snapshot each turn, name the screens each agent drives, and watch each thread's pull request
/// where gh or glab is installed.
pub fn start(daemon: &Daemon, asks: Adapters) {
    let Some(threads) = &daemon.threads else { return };
    drop(threads.snapshots.spawn());
    drop(Screens::new(threads.host.clone()).spawn());
    let (host, snapshots) = (threads.host.clone(), threads.snapshots.clone());
    drop(tokio::spawn(async move {
        let programs = slopty_worker::repo::commit::Programs::here().await;
        if programs.has_forge() {
            drop(slopty_worker::thread::pulls::spawn(host, programs, snapshots));
        }
    }));
    let place = claude::Place {
        data_dir: asks.data.clone(),
        ptyd: asks.ptyd,
        path: None,
        home: slopty_platform::dirs::home(),
    };
    drop(claude::spawn(threads.host.clone(), place, asks.claude));
    if let Some(home) = codex::codex_home() {
        let launch = codex::Launch::new(None);
        drop(codex::spawn(threads.host.clone(), codex::socket_of(&home), Some(launch), asks.codex));
    }
    // The registry's agents and the person's own, `[worker.acp]`, read at each start.
    let settings = slopty_settings::path_in(&asks.data);
    let own: acp::Own =
        Arc::new(move || slopty_settings::Settings::load(&settings).settings.worker.acp);
    drop(tokio::spawn(rest(threads.host.clone())));
    drop(tokio::spawn(awake::watch(daemon.clone(), threads.host.clone())));
    drop(tokio::spawn(sweep_worktrees(threads.clone())));
    drop(pi::spawn(threads.host.clone(), asks.data, None, asks.pi));
    drop(acp::spawn(threads.host.clone(), None, own, asks.acp));
    // A scheduled message goes as the person's own, the way a client's does.
    let sending = threads.clone();
    let fire: schedule::Fire =
        Arc::new(move |thread, id, intent| act_as(ClientId::nil(), &sending, thread, id, &intent));
    drop(schedule::spawn(threads.host.clone(), fire));
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
    App { id: u64, ask: AppAsk },
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
            ThreadRequest::App { thread, id, ask } => {
                self.command(at, thread, Command::App { id, ask });
            }
            // Git takes its time: a keep or a revert is answered from a task of its own.
            ThreadRequest::Intent {
                id,
                thread,
                intent: intent @ (Intent::Keep(_) | Intent::Revert(_) | Intent::Unrewind { .. }),
            } => {
                let (snapshots, out) = (threads.snapshots, at.out.clone());
                at.tasks.spawn(async move {
                    if let Some(outcome) = snapshots.pick(thread, id, &intent).await {
                        let _gone =
                            out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                    }
                });
            }
            // The folder is taken first, so the rewind can be undone: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: intent @ Intent::Rewind { turn } } => {
                let (client, out) = (at.client, at.out.clone());
                at.tasks.spawn(async move {
                    let before = match threads.host.outcome(thread, id) {
                        Some(_) => None,
                        None => threads.snapshots.before_rewind(thread).await,
                    };
                    let outcome = act_as(client, &threads, thread, id, &intent);
                    if let (Outcome::Done, Some(tree)) = (&outcome, before) {
                        let at_ms = slopty_core::WallMs::now();
                        let rewound = slopty_proto::thread::Rewound { turn, tree, at_ms };
                        threads.host.apply(
                            thread,
                            vec![slopty_proto::thread::Action::Rewound(Some(rewound))],
                        );
                    }
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // An agent's own review names the change as commits first: on a task of its own.
            ThreadRequest::Intent { id, thread, intent: Intent::Review { from, to } } => {
                tracing::info!(client = %at.client, %id, %thread, "review by the agent");
                let (client, out) = (at.client, at.out.clone());
                at.tasks.spawn(async move {
                    let outcome = review(client, &threads, thread, id, &from, &to).await;
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
                tracing::info!(client = %at.client, %id, %thread, "close a thread for good");
                let out = at.out.clone();
                at.tasks.spawn(async move {
                    let outcome = discard(&threads, thread, id).await;
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                });
            }
            // The person's own marks, the same for every agent: on the worker alone.
            ThreadRequest::Intent {
                id,
                thread,
                intent:
                    intent @ (Intent::Seen { .. }
                    | Intent::Draft { .. }
                    | Intent::Name { .. }
                    | Intent::Said { .. }),
            } => {
                let now = slopty_core::WallMs::now();
                let marked = threads.host.intent(thread, id, |state| {
                    slopty_worker::thread::marks::act(state, &intent, now)
                });
                let outcome = marked.unwrap_or_else(|| refused("no such thread".to_owned()));
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            ThreadRequest::Intent { id, thread, intent: Intent::KeepAside } => {
                let outcome = keep_aside(&threads, thread, id);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            ThreadRequest::Intent { id, thread, intent } => {
                let outcome = act(at, &threads, thread, id, &intent);
                at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
            }
            // An agent is looked for and starts, in a terminal or not, in the worktree it
            // names once that is made and set up, the setup said as it goes: on the daemon's
            // own task, which outlives this link, and is followed from here. The same start
            // asked again, after a relink, follows the one under way or gets its outcome.
            ThreadRequest::Start { id, start } => {
                tracing::info!(client = %at.client, %id, agent = %start.agent.0, cwd = start.cwd, "start");
                if let Some(outcome) = threads.host.started(id) {
                    at.post(WorkerMsg::IntentDone(IntentDone { id, outcome }));
                    return;
                }
                let followed =
                    threads.starts.clone().take_up(id, |said| run_start(threads, id, start, said));
                at.tasks.spawn(starts::follow(id, followed, at.out.clone()));
            }
            // An agent is asked, or its session directory listed: on a task of its own.
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
            begin(&threads, id, start).await
        }
        Err(worktrees::Failed::Setup(failed)) => {
            Outcome::SetupFailed { setup: failed.setup, code: failed.code }
        }
        Err(failed) => refused(failed.to_string()),
    };
    threads.host.record_start(id, outcome)
}

/// Start a thread for intent `id` as `start` says, once, on its agent's adapter: Claude Code,
/// pi and an ACP agent are looked for and run here, Codex's daemon is asked.
async fn begin(threads: &Threads, id: IntentId, start: Box<Start>) -> Outcome {
    if start.agent.is(AgentId::PI) {
        threads.pi.start(id, start).await
    } else if start.agent.is(AgentId::CLAUDE_CODE) {
        threads.claude.start(id, start).await
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
        threads.claude.fork(thread, id, after).await
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

/// Close `thread` for good for intent `id`, once: an aside put away, or a run of a message the
/// person did not keep. Its agent ends, its session kept where the agent keeps it, and the
/// worker forgets the thread. Then the agent's worktree it worked in
/// goes ([`worktrees::discard`]) unless another thread or a live terminal works in it: an
/// aside shares its thread's folder, so it never takes it.
async fn discard(threads: &Threads, thread: ThreadId, id: IntentId) -> Outcome {
    // Answered once, though the thread is gone after: kept with the worker's starts.
    if let Some(first) = threads.host.started(id) {
        return first;
    }
    let Some((state, _)) = threads.host.state(thread) else {
        return refused("no such thread".to_owned());
    };
    if let Err(e) = threads.end(&state).await {
        tracing::warn!(%thread, "a closed thread's agent did not end: {e}");
    }
    threads.snapshots.forget(&state).await;
    if let Err(e) = threads.host.remove(thread) {
        tracing::warn!(%thread, "a closed thread's log stayed: {e}");
    }
    free_worktree(threads, &state).await;
    threads.host.record_start(id, Outcome::Done)
}

/// Take away the agent's worktree closed thread `state` worked in, when it worked in one and
/// nothing else does: no other thread held, and no live terminal. A folder that is no agent's
/// worktree is left as it is.
async fn free_worktree(threads: &Threads, state: &ThreadState) {
    let Some(git) = slopty_worker::changes::git() else { return };
    if state.meta.cwd.is_empty() {
        return;
    }
    let tree = slopty_worker::file::expand_home(Path::new(&state.meta.cwd));
    let cwds = worked_in(threads).await;
    match worktrees::discard(git, &tree, &cwds).await {
        Ok(removed) => tracing::info!(
            thread = %state.meta.id,
            tree = %tree.display(),
            branch = ?removed.branch,
            branch_removed = removed.branch_removed,
            "a closed thread's worktree went"
        ),
        Err(worktrees::Failed::NotOne(_)) => {}
        Err(kept) => {
            tracing::info!(thread = %state.meta.id, "a closed thread's worktree stayed: {kept}");
        }
    }
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
            }
            Err(e) => tracing::debug!(clone = %clone.display(), "no sweep: {e}"),
        }
    }
}

/// What Codex calls the change it reviews ([`Codex::review`]).
const REVIEW_TITLE: &str = "The changes on show in Slopty's review";

/// Ask `thread`'s agent for its own review of the change from `from` to `to`, for intent `id`,
/// once: the change is kept as two commits ([`Snapshots::review_range`]), then Codex's reviewer
/// takes the head commit, and Claude Code's `/code-review` the range, sent as the person's turn
/// as any command of theirs is.
async fn review(
    client: ClientId,
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
    let text = format!("/{} {}", slopty_agent::claude::REVIEW_COMMAND, range.dots());
    let delivery = state.meta.delivery_after_turn();
    act_as(client, threads, thread, id, &Intent::Send { text, delivery, attachments: Vec::new() })
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
        tokio::task::spawn_blocking(move || {
            slopty_agent::claude::sessions::sessions(&home, &dir, most)
        })
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
    act_as(at.client, threads, thread, id, intent)
}

/// [`act`], for `client`: the nil client for the worker's own and orchestration's.
fn act_as(
    client: ClientId,
    threads: &Threads,
    thread: ThreadId,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    // A scheduled message is the worker's to hold: no agent hears of it until its moment.
    if let Some(outcome) = schedule::act(&threads.host, thread, id, intent) {
        return outcome;
    }
    let decided =
        threads.host.intent(thread, id, |state| decide(client, threads, state, id, intent));
    decided.unwrap_or_else(|| refused("no such thread".to_owned()))
}

/// The daemon's threads as orchestration's verbs read and answer them
/// ([`slopty_worker::orchestrate::ThreadReads`]), by the nil client.
#[derive(Clone)]
pub struct Reads {
    threads: Threads,
}

impl Reads {
    /// `threads`, as orchestration reaches them.
    pub const fn new(threads: Threads) -> Self {
        Self { threads }
    }
}

impl orchestrate::ThreadReads for Reads {
    fn state(&self, thread: ThreadId) -> Option<ThreadState> {
        self.threads.host.state(thread).map(|(state, _)| state)
    }

    fn intent(&self, thread: ThreadId, id: IntentId, intent: Intent) -> Outcome {
        tracing::info!(%thread, %id, "an intent from orchestration");
        act_as(ClientId::nil(), &self.threads, thread, id, &intent)
    }

    fn seated_at(&self, session: SessionId) -> Option<ThreadRow> {
        let thread = self.threads.host.seated_at(session)?;
        let (state, _) = self.threads.host.state(thread)?;
        Some(state.row(slopty_core::WallMs::now()))
    }
}

/// What comes of `intent` on `state`'s thread, from `client`, done as it is decided.
fn decide(
    client: ClientId,
    threads: &Threads,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> (Outcome, Vec<slopty_proto::thread::Action>) {
    if intent.needs().is_some_and(|needs| !state.meta.can(needs)) {
        return (intent.unsupported(), Vec::new());
    }
    if let Some(refusal) = unattended(client, state, intent) {
        return (refusal, Vec::new());
    }
    let by = answerer(client);
    let outcome = if codex::is_shared(state) {
        shared(client, &threads.codex, state, id, intent)
    } else if claude::is_claude(state) {
        threads.claude.decide(state, id, intent, by)
    } else if pi::is_pi(state) {
        threads.pi.decide(state, id, intent, by)
    } else if acp::is_acp(state) {
        threads.acp.decide(state, id, intent, by)
    } else {
        refused(format!("{} threads are not driven here", state.meta.agent.0))
    };
    (outcome, Vec::new())
}

/// The refusal of an answer to a request only the person at its card may answer
/// ([`slopty_proto::thread::Request::needs_presence`]) when it comes from no client: from
/// orchestration's verbs, a notification's action, a page or a schedule, none with the person
/// at the card. A client's answer is its card's, the person's own click or key.
fn unattended(client: ClientId, state: &ThreadState, intent: &Intent) -> Option<Outcome> {
    let Intent::Answer { ask, .. } = intent else { return None };
    let present = client != ClientId::nil();
    let needs = state.requests.iter().any(|r| r.id == *ask && r.is_open() && r.needs_presence);
    (needs && !present)
        .then(|| refused(slopty_proto::thread::Request::NEEDS_PRESENCE_REFUSED.to_owned()))
}

/// What comes of `intent` on a Codex thread: it goes to the app-server as the Codex TUI's own
/// would. An answer to a request already settled, from the TUI or another client, is no
/// error: the card shows who settled it. A reason given with an answer goes as the person's
/// next words into the turn, since a Codex decision carries none.
fn shared(
    client: ClientId,
    codex: &Codex,
    state: &ThreadState,
    id: IntentId,
    intent: &Intent,
) -> Outcome {
    let thread = state.meta.id;
    if let Some(refusal) = not_while_exited(state, intent) {
        return refusal;
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
            codex.answer(thread, ask.clone(), choice.clone(), answerer(client), id);
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
        | Intent::Move { pending, .. }
            if !state.pending.iter().any(|p| p.intent == *pending) =>
        {
            refused("That message is not waiting".to_owned())
        }
        Intent::Move { pending, before } => {
            codex.move_queued(thread, *pending, *before);
            Outcome::Done
        }
        Intent::Withdraw { pending } => {
            codex.withdraw(thread, *pending);
            Outcome::Done
        }
        Intent::Promote { pending } => {
            codex.promote(thread, *pending);
            Outcome::Done
        }
        Intent::Edit { pending, text, attachments } => {
            if let Err(why) = slopty_worker::thread::attach::check(attachments) {
                return refused(why);
            }
            codex.edit(thread, *pending, text.clone(), attachments.clone());
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
        other => other.unsupported(),
    }
}

/// Why `intent` cannot go to the Codex thread `state` while Codex does not run it, if it cannot.
///
/// A thread Codex let go, or that was left exited when Codex went, takes a message or a change
/// to one waiting, a switch of its settings: the worker takes the thread up again for it, waits
/// for Codex to come back, or holds it in the list saying it did not go
/// (`slopty_worker::thread::codex`). An answer or a stop has nothing left to act on: the request
/// and the turn ended with Codex's run.
fn not_while_exited(state: &ThreadState, intent: &Intent) -> Option<Outcome> {
    let Liveness::Exited { resumable } = state.status.liveness else { return None };
    let waits = matches!(
        intent,
        Intent::Send { .. }
            | Intent::Withdraw { .. }
            | Intent::Edit { .. }
            | Intent::Promote { .. }
            | Intent::Move { .. }
            | Intent::SetModel { .. }
            | Intent::SetEffort { .. }
            | Intent::SetMode { .. }
    );
    (!(resumable && waits)).then(|| refused("Codex isn't running this thread".to_owned()))
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
                    let (snapshots, done) = (threads.snapshots.clone(), slow_tx.clone());
                    tokio::spawn(async move {
                        let id = content.blob_id().unwrap_or_default();
                        let body = snapshots.blob(thread, id).await;
                        let _gone = done.send(ThreadFrame::Expanded { content, body });
                    });
                    continue;
                }
                Some(Command::Expand { content }) => {
                    let body = threads.expand(thread, content.clone()).await;
                    ThreadFrame::Expanded { content, body }
                }
                // A page's ask waits on the agent: on a task of its own, so the thread's frames
                // go on meanwhile.
                Some(Command::App { id, ask }) => {
                    let (asked, done) = (threads.clone(), slow_tx.clone());
                    tokio::spawn(async move {
                        let answer = asked.app(thread, ask).await;
                        let _gone = done.send(ThreadFrame::App { id, answer });
                    });
                    continue;
                }
                // A review runs git for a while: on a task of its own, so the thread's frames
                // go on meanwhile.
                Some(Command::Review { scope }) => {
                    let (snapshots, done) = (threads.snapshots.clone(), slow_tx.clone());
                    reviewing.start(tokio::spawn(async move {
                        let review = snapshots.review(thread, scope).await;
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

    /// How many turns run now in threads whose agents are this daemon's children (pi, an ACP
    /// agent), so they end with it, where Claude Code goes on in ptyd's custody and Codex in its
    /// own daemon. An update says so before it ends them.
    pub fn driven_turns(&self) -> usize {
        let TableFrame::Snapshot { rows, .. } = self.host.table(None) else { return 0 };
        rows.iter().filter(|row| ends_with_daemon(row) && mid_turn(row.status.phase)).count()
    }

    /// The whole of `content`, clipped in `thread`, from the adapter that made it.
    async fn expand(&self, thread: ThreadId, content: ContentRef) -> Expanded {
        match self.host.state(thread) {
            // A visual's or a page's file, by its item and name, whichever agent showed it.
            Some((state, _)) if content.item_asset_of().is_some() => {
                slopty_worker::thread::assets::read(&state, &content).await
            }
            Some((state, _)) if claude::is_claude(&state) => {
                self.claude.expand(thread, content).await
            }
            // Every other agent's pictures are kept by the worker (`slopty_agent::media`).
            Some(_) => slopty_agent::media::read(&content).map_or(Expanded::Gone, Expanded::Bytes),
            None => Expanded::Gone,
        }
    }

    /// What a page shown in `thread` asks of its MCP App, through the agent that holds the
    /// connection: Codex's `mcpServer/resource/read` and `mcpServer/tool/call`, Claude Code's
    /// `mcp_read_resource` and `mcp_call`. An agent with no such door refuses.
    async fn app(&self, thread: ThreadId, ask: AppAsk) -> AppAnswer {
        if let AppAsk::Read { uri, .. } = &ask
            && !slopty_proto::visual::is_ui_uri(uri)
        {
            return AppAnswer::Refused(format!("{uri} is no app's page (ui://)"));
        }
        match self.host.state(thread) {
            Some((state, _)) if codex::is_shared(&state) => self.codex.app(thread, ask).await,
            Some((state, _)) if claude::is_claude(&state) => self.claude.app(thread, ask).await,
            Some(_) => AppAnswer::Refused("this agent has no door to its MCP servers".to_owned()),
            None => AppAnswer::Refused("the thread is gone".to_owned()),
        }
    }
}

/// The server starts seated threads here ([`slopty_proto::orchestration::Verb::StartThread`]),
/// each through its agent's own adapter, at the seat the server chose.
impl orchestrate::SeatedThreads for Threads {
    fn start(
        &self,
        thread: orchestrate::SeatedThread,
    ) -> orchestrate::BoxFuture<'_, Result<ThreadId, Failure>> {
        Box::pin(async move {
            let orchestrate::SeatedThread { start, seat, facts, system } = thread;
            let relay = slopty_agent::mcp::cli_beside_this_binary()
                .map(|relay| relay.to_string_lossy().into_owned());
            let seated = Seated { seat, facts, system, relay };
            let agent = start.agent.clone();
            tracing::info!(%seat, agent = %agent.0, cwd = start.cwd, "start a seated thread");
            let outcome = if agent.is(AgentId::PI) {
                self.pi.start_seated(Box::new(start), seated).await
            } else if agent.is(AgentId::CLAUDE_CODE) {
                self.claude.start_seated(Box::new(start), seated).await
            } else if agent.is(AgentId::CODEX) {
                self.codex.start_seated(start, seated).await
            } else if slopty_agent::acp::name_of(&agent).is_some() {
                self.acp.start_seated(Box::new(start), seated).await
            } else {
                let why = format!("{} is no agent this machine can start", agent.0);
                return Err(Failure::new(ErrorCode::Unsupported, why));
            };
            started(&agent, outcome)
        })
    }

    fn close(&self, seat: SessionId) -> orchestrate::BoxFuture<'_, Result<bool, Failure>> {
        Box::pin(async move {
            let Some(thread) = self.host.seated_at(seat) else { return Ok(false) };
            let Some((state, _)) = self.host.state(thread) else { return Ok(false) };
            tracing::info!(%seat, %thread, "end a seated thread");
            self.end(&state).await.map_err(|e| Failure::new(ErrorCode::Failed, e))?;
            Ok(true)
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

impl Threads {
    /// End `state`'s agent, its session kept to take up again: each adapter ends its own.
    async fn end(&self, state: &ThreadState) -> Result<(), String> {
        let thread = state.meta.id;
        if codex::is_shared(state) {
            self.codex.close(thread).await;
        } else if pi::is_pi(state) {
            self.pi.close(thread).await;
        } else if acp::is_acp(state) {
            self.acp.close(thread).await;
        } else if claude::is_claude(state) {
            self.claude.close(thread).await;
        } else {
            return Err(format!("{} threads are not driven here", state.meta.agent.0));
        }
        Ok(())
    }

    /// Send `text`, Slopty's reports, to the thread seated at `seat`, once for delivery
    /// `batch`: queued behind the turn under way where its agent queues, else steered into it.
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
            if intent.needs().is_some_and(|needs| !state.meta.can(needs)) {
                return (intent.unsupported(), Vec::new());
            }
            let by = Answerer { client: None, name: "Slopty".to_owned() };
            let outcome = if codex::is_shared(state) {
                self.codex.send(thread, text.to_owned(), Vec::new(), delivery, id);
                Outcome::Done
            } else if pi::is_pi(state) {
                self.pi.decide(state, id, &intent, by)
            } else if acp::is_acp(state) {
                self.acp.decide(state, id, &intent, by)
            } else if claude::is_claude(state) {
                self.claude.report(thread, text.to_owned())
            } else {
                refused("its agent takes no message here".to_owned())
            };
            (outcome, Vec::new())
        });
        decided.unwrap_or_else(|| refused("no such thread".to_owned()))
    }
}

/// Whether `row`'s thread runs as this daemon's child and so ends with it: a driven thread's
/// agent, live, counted once for its family (a subagent runs in its parent's process), and not
/// Claude Code, which ptyd keeps.
fn ends_with_daemon(row: &ThreadRow) -> bool {
    row.drive.is(Drive::DRIVEN)
        && !row.agent.is(AgentId::CLAUDE_CODE)
        && row.parent.is_none()
        && matches!(row.status.liveness, Liveness::Live)
}

/// Whether a thread at `phase` is in the middle of a turn: working, waiting on its own work, or
/// asking the person.
const fn mid_turn(phase: Phase) -> bool {
    matches!(phase, Phase::Working | Phase::Waiting | Phase::NeedsYou)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A thread of no agent's in particular, as a test starts it.
    fn a_thread() -> ThreadState {
        slopty_agent::claude::history::replay(&[])
    }

    /// A thread asking `ask`, which only the person at its card answers when `present`.
    fn asking(ask: &str, present: bool) -> ThreadState {
        let mut state = a_thread();
        state.requests.push(slopty_proto::thread::Request {
            id: slopty_proto::thread::AskId(ask.to_owned()),
            item: None,
            kind: slopty_proto::thread::Request::APPROVAL.to_owned(),
            title: "Run rm -rf build?".to_owned(),
            text: None,
            options: Vec::new(),
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            reason: None,
            state: slopty_proto::thread::RequestState::Open,
            opened_ms: slopty_core::WallMs::ZERO,
            until_ms: None,
            needs_presence: present,
        });
        state
    }

    /// An answer from no client, orchestration's verbs (the CLI, an agent's tool, a note's
    /// verdict through the server), is refused for a request only the person at its card
    /// answers; a client's, the card's own, goes on to its agent, as does any answer to a
    /// request that needs no one at its card.
    #[test]
    fn only_the_card_answers_a_request_that_needs_the_person_at_it() {
        let answer = |ask: &str| Intent::Answer {
            ask: slopty_proto::thread::AskId(ask.to_owned()),
            choice: "allow".to_owned(),
            message: None,
        };
        let present = asking("a", true);
        assert_eq!(
            unattended(ClientId::nil(), &present, &answer("a")),
            Some(refused(slopty_proto::thread::Request::NEEDS_PRESENCE_REFUSED.to_owned())),
            "an answer from orchestration"
        );
        assert_eq!(unattended(ClientId::new(), &present, &answer("a")), None, "the card's own");
        assert_eq!(unattended(ClientId::nil(), &present, &answer("b")), None, "another request");
        assert_eq!(unattended(ClientId::nil(), &asking("a", false), &answer("a")), None);
        assert_eq!(unattended(ClientId::nil(), &present, &Intent::Interrupt), None);
    }

    /// Only a live driven thread's turn ends with the daemon: Claude Code goes on in ptyd's
    /// custody and Codex (shared) in its own daemon, a subagent is its parent's, and a thread at
    /// rest or ended loses nothing.
    #[test]
    fn only_a_driven_thread_mid_turn_ends_with_the_daemon() {
        let mut row = a_thread().row(slopty_core::WallMs::now());
        row.agent = AgentId::named(AgentId::PI);
        row.drive = Drive::named(Drive::DRIVEN);
        row.status.liveness = Liveness::Live;
        assert!(ends_with_daemon(&row));
        let nested = ThreadRow {
            parent: Some(slopty_proto::thread::Link {
                thread: ThreadId::new(),
                item: slopty_proto::thread::ItemId("toolu_1".to_owned()),
            }),
            ..row.clone()
        };
        assert!(!ends_with_daemon(&nested), "a subagent is its parent's");
        let claude = ThreadRow { agent: AgentId::named(AgentId::CLAUDE_CODE), ..row.clone() };
        assert!(!ends_with_daemon(&claude), "ptyd keeps Claude Code");
        let shared = ThreadRow { drive: Drive::named(Drive::SHARED), ..row.clone() };
        assert!(!ends_with_daemon(&shared), "Codex runs in its own daemon");
        let mut ended = row;
        ended.status.liveness = Liveness::Exited { resumable: true };
        assert!(!ends_with_daemon(&ended));
        let mid = [Phase::Working, Phase::Waiting, Phase::NeedsYou];
        assert!(mid.into_iter().all(mid_turn));
        let resting = [Phase::Idle, Phase::Done, Phase::Failed, Phase::Stopped];
        assert!(!resting.into_iter().any(mid_turn));
    }

    /// A Codex thread left exited and resumable (let go, or Codex gone) still takes a message
    /// and the changes to one waiting, which the worker sends once Codex takes the thread up
    /// again; an answer or a stop has nothing left to act on, and a thread that cannot be taken
    /// up again takes nothing.
    #[test]
    fn an_exited_codex_thread_still_takes_a_message() {
        let mut state = a_thread();
        state.meta.agent = AgentId::named(AgentId::CODEX);
        state.meta.drive = Drive::named(Drive::SHARED);
        assert!(codex::is_shared(&state));
        state.status.liveness = Liveness::Exited { resumable: true };
        let send = Intent::Send {
            text: "Go on.".to_owned(),
            attachments: vec![],
            delivery: Delivery::Queue,
        };
        let withdraw = Intent::Withdraw { pending: IntentId::new() };
        assert_eq!(not_while_exited(&state, &send), None, "a message goes");
        assert_eq!(not_while_exited(&state, &withdraw), None, "a change to one waiting goes");
        assert!(not_while_exited(&state, &Intent::Interrupt).is_some(), "no turn to stop");
        state.status.liveness = Liveness::Exited { resumable: false };
        assert!(not_while_exited(&state, &send).is_some(), "not resumable");
        state.status.liveness = Liveness::Live;
        assert_eq!(not_while_exited(&state, &Intent::Interrupt), None);
    }

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
