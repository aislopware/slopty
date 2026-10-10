//! The registry: every worker the server knows, the lease each live one holds, and the verbs
//! routed to them.
//!
//! A worker's connection is its lease ([`Lease`]): registering takes it, dropping it (the link
//! ended, cleanly or by the idle timeout) marks the worker [`Liveness::Unreachable`], and
//! [`GONE_AFTER`] later, with no reconnect, [`Liveness::Gone`]. Every change goes out to every
//! client and agent link as [`FromServer::Worker`], [`FromServer::Load`] or
//! [`FromServer::Event`].
//!
//! A worker set up again (a new data directory) registers under a new id with its old name,
//! from its old address. Nothing else can be listening there, so the entries it replaces are
//! dropped and every link gets the directory again without them.
//!
//! [`Hub::dispatch`] is the one verb dispatch both front ends call (QUIC links and MCP): the
//! directory verbs, [`Verb::Events`], [`Verb::ForgetWorker`] and [`Verb::Wake`] are answered
//! here, the rest go down the owning worker's link.
//!
//! A sleeping worker is woken from its own LAN ([`Verb::Wake`]): by the server when it shares a
//! subnet with the worker's last reported ports, else by an online worker that does
//! ([`Verb::WakePeer`]).
//!
//! An [`IdempotencyKey`] goes down with the verb it came with: the worker, which does the verb,
//! keeps the table that does it once per key. The hub keeps its own only for the one effect it
//! owns, forgetting a worker.
//!
//! Every change of liveness, terminal and agent status goes into a bounded log of [`HubEvent`]s
//! under one sequence, and the same event goes out to every link as it is logged: a link pushed
//! it and a [`Verb::Events`] reading from a cursor see one vocabulary in one order, and one call
//! watches the whole fleet.
//!
//! Projects are the hub's own too ([`crate::project`], `projects`): their verbs are answered
//! here from the store's records, what runs for a task goes to the worker its start names or
//! one with room and is started with the project and task in its environment
//! ([`Verb::TaskSpawn`]), every agent's start is counted against the person's bounds, and what
//! the workers report of their agents (status, branches, Claude Code's own subagents) moves the
//! tasks they work on. Every change is a [`Happening::Project`] in the one log, and the projects
//! file is written after it.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_agent::vouch::SessionKey;
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::RequestId;
use slopty_proto::agent::AgentBranch;
use slopty_proto::lan::{LanPort, MacAddr};
use slopty_proto::orchestration::{
    ErrorCode, EventFilter, Happening, HubEvent, IdempotencyKey, KEY_LIFETIME, Outcome, TermRef,
    ThreadOf, Verb,
};
use slopty_proto::project::{AgentReport, Facts, ProjectId, ProjectStatus, ProjectsPart, TaskId};
use slopty_proto::server::{FromServer, Liveness, Refusal, Registration, ToServer, WorkerInfo};
use slopty_proto::terminal::{SessionState, SessionSummary};
use tokio::sync::{Notify, broadcast, mpsc, oneshot, watch};

use crate::deliver::Deliveries;
use crate::project::{
    Caller, Change, Drove, First, KEYS_REMEMBERED, Keep, KeptKey, Projects, ProjectsFile,
    Remembered, Seen, StartKept, Starting, Watched,
};

mod awake;
mod codex;
mod ladder;
mod outcomes;
mod projects;
mod queue;
mod settle;
mod steps;

pub use awake::{Hold, Policy as KeepAwake};
pub use ladder::{Devices, PushKept, Seated};

/// How long an unreachable worker has to reconnect before it is presumed gone (Nomad's TTL plus
/// grace; `docs/decisions/topology.md`).
pub const GONE_AFTER: Duration = Duration::from_secs(20);
/// The longest [`Verb::WaitFor`] the server forwards, in milliseconds: under Claude Code's
/// five-minute idle abort of an MCP call, with room for the answer to travel back.
pub const WAIT_CAP_MS: u32 = 240_000;
/// What a forwarded [`Verb::WaitFor`] gets beyond its own timeout before the server gives up on
/// the worker's answer.
const WAIT_GRACE: Duration = Duration::from_secs(15);
/// How long any other forwarded verb may take. Every verb but `WaitFor` is one step on the
/// worker; this only bounds a worker that stopped answering while its link stays up.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a clone the server asked for may take: the worker's own limit, and a minute.
const CLONE_TIMEOUT: Duration = Duration::from_mins(31);
/// How long making or fetching a bundle may take: the worker's own limit, and a minute.
const BUNDLE_TIMEOUT: Duration = Duration::from_mins(11);
/// Directory changes buffered per client link before it lags and gets the whole directory again.
const EVENT_BUFFER: usize = 1024;
/// Events the log holds for [`Verb::Events`]; a cursor older than the oldest reads on from it
/// and is told how many it missed.
pub const EVENT_LOG: usize = 4096;
/// Most events one [`Verb::Events`] returns; the caller asks again from `next`.
pub const EVENTS_PER_ANSWER: usize = 500;
/// Keyed [`Verb::ForgetWorker`]s remembered, the oldest dropped first. People forget a worker
/// now and then; this holds far more than [`KEY_LIFETIME`] brings.
const FORGETS_KEPT: usize = 256;
/// The most bytes of events one [`Verb::Events`] answer carries, by their estimate: a
/// project's change can be tens of kilobytes, and an answer is one frame.
const EVENTS_BYTES: usize = 8 << 20;

/// The registry and the router, shared by every link and the MCP endpoint.
#[derive(Clone, Debug)]
pub struct Hub {
    inner: Arc<Inner>,
}

/// A [`Hub`] that does not keep it alive: what a task that outlives no server holds.
#[derive(Clone, Debug)]
pub struct WeakHub(std::sync::Weak<Inner>);

impl WeakHub {
    /// The hub, while anything else keeps it.
    #[must_use]
    pub fn upgrade(&self) -> Option<Hub> {
        self.0.upgrade().map(|inner| Hub { inner })
    }
}

/// The server machine's own LAN: its ports, and the magic packet sent from one of them.
pub trait Lan: Send + Sync + std::fmt::Debug {
    /// This machine's ports now.
    fn ports(&self) -> Vec<LanPort>;
    /// Send the magic packet for `macs` from `from`, one of [`Self::ports`].
    fn wake(&self, from: LanPort, macs: Vec<MacAddr>) -> WakeFuture;
}

/// What [`Lan::wake`] returns.
pub type WakeFuture = Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;

/// The machine's real interfaces ([`slopty_tailnet::lan`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLan;

impl Lan for SystemLan {
    fn ports(&self) -> Vec<LanPort> {
        slopty_tailnet::lan::ports()
    }

    fn wake(&self, from: LanPort, macs: Vec<MacAddr>) -> WakeFuture {
        Box::pin(async move { slopty_tailnet::lan::wake(&from, &macs).await })
    }
}

#[derive(Debug)]
struct Inner {
    name: String,
    lan: Arc<dyn Lan>,
    state: Mutex<State>,
    events: broadcast::Sender<FromServer>,
    persist: watch::Sender<Vec<WorkerInfo>>,
    /// Taken only inside `state`'s lock, or alone.
    log: Mutex<Log>,
    /// The log's next sequence number, for [`Verb::Events`] to wait on.
    head: watch::Sender<u64>,
    /// Wakes the loop that delivers reports ([`Hub::deliver_reports`]).
    deliver: Arc<Notify>,
    /// The `settings.toml` the server follows, whose `[server]` another device reads and edits
    /// ([`Verb::Settings`]), once the daemon gave it.
    settings_file: std::sync::OnceLock<std::path::PathBuf>,
}

/// The newest [`HubEvent`]s, oldest first.
#[derive(Debug)]
struct Log {
    ring: VecDeque<HubEvent>,
    /// This run's first sequence number ([`run_seed`]).
    first: u64,
    next: u64,
}

/// One read of the log.
#[derive(Debug)]
struct Page {
    events: Vec<HubEvent>,
    next: u64,
    missed: u64,
}

#[derive(Debug, Default)]
struct State {
    workers: HashMap<WorkerId, Entry>,
    next_request: RequestId,
    next_generation: u64,
    /// Workers forgotten under a key, oldest first.
    forgets: VecDeque<Forget>,
    /// Every project.
    projects: Projects,
    /// Project changes made under a key, oldest first.
    project_keys: VecDeque<Keyed>,
    /// Starts made under a key, oldest first, with what the hub forwarded for them.
    start_keys: VecDeque<KeyedStart>,
    /// Starts of agents and tasks' terminals placed and not live yet.
    starting: Vec<Starting>,
    next_start: u64,
    /// Reports on their way to the agents they are for.
    deliveries: Deliveries,
    /// Terminals the server watches for as long as they live ([`Watched`]), with when each
    /// was first watched: agents it started in `default` mode, and terminals an agent opened or
    /// typed into, whose CLI speaks for an agent. The store keeps them, so a restart does too.
    watched: HashMap<SessionId, (Watched, tokio::time::Instant)>,
    /// Where every change to the projects goes to be kept ([`crate::store::ProjectStore`]).
    keeper: Option<mpsc::UnboundedSender<Keep>>,
    /// What the server does for tasks around their agents: clones, branches brought home.
    steps: steps::Steps,
    /// The projects' lanes running: their verifiers and merge queues.
    lanes: queue::Lanes,
    /// Every worker's thread rows, the ladder made of them, and where the person is.
    board: ladder::Board,
}

/// A project change made under a key, so a repeat of the verb answers as the first did.
///
/// The verb is kept as a digest of its encoding, and the answer as what it names (a task, a
/// project), read again for a repeat: a whole project's status or a task's brief held for
/// each of a thousand keys would be gigabytes.
#[derive(Debug)]
struct Keyed {
    key: IdempotencyKey,
    digest: blake3::Hash,
    answer: Remembered,
    at: tokio::time::Instant,
    wall: WallMs,
}

/// A start made under a key: what its caller sent, as a digest, and the start the hub
/// forwarded for it with the terminal id it chose, until its worker answered. A repeat before
/// the answer forwards that start again, so the worker's own ledger answers it as it answers
/// the first; a repeat after it is given that answer. Either way no second start is placed, and
/// a terminal closed since is never opened again uncounted.
#[derive(Debug)]
struct KeyedStart {
    key: IdempotencyKey,
    digest: blake3::Hash,
    first: StartKept,
    at: tokio::time::Instant,
    wall: WallMs,
}

/// What a repeat of a keyed start does.
enum Again {
    /// Forward the first start again.
    Forward(Verb),
    /// Answer as the first was answered.
    Answer(Outcome),
}

/// Who sends a verb, as the link it came on says: the server tells an agent from the person
/// by it, since only the person may answer a permission or merge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speaker {
    /// A client app, or the CLI outside any Slopty terminal.
    Person,
    /// An agent: an MCP surface.
    Agent,
    /// The CLI inside the Slopty terminal `session`, by its word alone: an agent's when an
    /// agent runs there, it works on a project or an agent drove it, the person's otherwise.
    Shell(SessionId),
    /// The CLI inside the terminal `session`, proven by the token its worker gave it
    /// ([`slopty_agent::vouch`]): the person's or an agent's as [`Self::Shell`] is.
    ProvenShell(SessionId),
    /// An agent's tools proven to speak from the terminal `session` by its token.
    Proven(SessionId),
}

impl Speaker {
    /// The terminal it proved it speaks from.
    #[must_use]
    pub const fn proven(self) -> Option<SessionId> {
        match self {
            Self::Proven(session) | Self::ProvenShell(session) => Some(session),
            Self::Person | Self::Agent | Self::Shell(_) => None,
        }
    }
}

/// What a client or an agent link is told first: the fleet and its projects, read together.
#[derive(Debug)]
pub struct Snapshot {
    /// The last event logged before it was read: an event at or below it is in it already.
    pub seq: u64,
    /// Every worker, by name.
    pub directory: Vec<WorkerInfo>,
    /// Every project whole, with its timeline's latest entries.
    pub projects: Vec<ProjectStatus>,
}

/// A worker forgotten under a key, so a repeat of the verb answers as the first did.
#[derive(Debug)]
struct Forget {
    key: IdempotencyKey,
    worker: WorkerId,
    at: tokio::time::Instant,
}

#[derive(Debug)]
struct Entry {
    info: WorkerInfo,
    /// Each with its agent kept current from the worker's `Agent` events.
    sessions: Vec<SessionSummary>,
    /// Which registration the entry reflects; a lease of another generation is stale.
    generation: u64,
    link: Option<Link>,
    /// What each session's status line said of its worktree and pull request, for a task its
    /// agent is put on later.
    branches: HashMap<SessionId, AgentBranch>,
    /// What the worker reported it is and has.
    facts: Facts,
    /// The key its terminals' tokens are made under, once it registered.
    session_key: Option<SessionKey>,
}

#[derive(Debug)]
struct Link {
    tx: mpsc::Sender<FromServer>,
    /// Forwarded requests awaiting the worker's reply; dropped with the link, which answers
    /// every waiter [`ErrorCode::WorkerUnreachable`].
    pending: HashMap<RequestId, oneshot::Sender<Outcome>>,
}

/// A worker's hold on its registry entry, for as long as its connection lives. Dropping it ends
/// the lease: the worker turns unreachable, and every request pending on it fails.
#[derive(Debug)]
pub struct Lease {
    hub: Hub,
    worker: WorkerId,
    generation: u64,
}

impl Hub {
    /// A registry named `name` (the `Welcome` every link gets) that starts from `known`, the
    /// workers a previous run persisted, each listed as [`Liveness::Gone`] until it registers.
    #[must_use]
    pub fn new(name: String, known: Vec<WorkerInfo>) -> Self {
        Self::with_lan(name, known, Arc::new(SystemLan))
    }

    /// [`Self::new`], waking sleeping workers through `lan` rather than this machine's own
    /// interfaces.
    #[must_use]
    pub fn with_lan(name: String, known: Vec<WorkerInfo>, lan: Arc<dyn Lan>) -> Self {
        let mut state = State::default();
        for mut info in known {
            info.liveness = Liveness::Gone;
            let entry = Entry {
                info,
                sessions: Vec::new(),
                generation: 0,
                link: None,
                branches: HashMap::new(),
                facts: Facts::new(),
                session_key: None,
            };
            state.workers.insert(entry.info.worker, entry);
        }
        let (events, _none) = broadcast::channel(EVENT_BUFFER);
        let (persist, _none) = watch::channel(Vec::new());
        let first = run_seed();
        let log = Mutex::new(Log { ring: VecDeque::with_capacity(EVENT_LOG), first, next: first });
        let (head, _none) = watch::channel(first);
        let state = Mutex::new(state);
        let deliver = Arc::new(Notify::new());
        let settings_file = std::sync::OnceLock::new();
        let inner = Inner { name, lan, state, events, persist, log, head, deliver, settings_file };
        Self { inner: Arc::new(inner) }
    }

    /// Read and edit `[server]` of the settings file at `path` for another device from now on
    /// ([`Verb::Settings`] with no worker). The first one given stays.
    pub fn set_settings_file(&self, path: std::path::PathBuf) {
        if self.inner.settings_file.set(path).is_err() {
            tracing::warn!("the settings file was given twice; the first stays");
        }
    }

    /// `[server]` of the server's own settings file after `edits`; an edit that does not hold
    /// is [`ErrorCode::Invalid`] and writes nothing.
    async fn own_settings(&self, edits: Vec<slopty_proto::settings::SettingEdit>) -> Outcome {
        use slopty_settings::daemon::{Edit, File, Refused, read_and_edit};
        let Some(path) = self.inner.settings_file.get().cloned() else {
            return error(ErrorCode::Unsupported, "this server follows no settings file");
        };
        let read = tokio::task::spawn_blocking(move || {
            let edits: Vec<Edit<'_>> = edits
                .iter()
                .map(|e| Edit {
                    table: &e.table,
                    key: &e.key,
                    entry: e.entry.as_deref(),
                    literal: e.literal.as_deref(),
                })
                .collect();
            read_and_edit(&path, "server", &edits)
        })
        .await;
        match read {
            Ok(Ok(File { path, text, problems })) => {
                Outcome::Settings(Box::new(slopty_proto::settings::DaemonSettings {
                    path: path.to_string_lossy().into_owned(),
                    text,
                    tables: vec!["server".to_owned()],
                    problems,
                }))
            }
            Ok(Err(Refused::Edit(why))) => error(ErrorCode::Invalid, &why),
            Ok(Err(Refused::Io(why))) => error(ErrorCode::Failed, &why),
            Err(gone) => error(ErrorCode::Failed, &gone.to_string()),
        }
    }

    /// Whether `token` proves a link speaks from the terminal `session`: the token its worker
    /// gave it, under the key the worker registered with.
    #[must_use]
    pub fn vouches(&self, session: SessionId, token: &str) -> bool {
        let state = self.inner.state.lock();
        // Only the worker holding a key mints tokens under it, so the terminal is that worker's
        // even before the worker announces it.
        state.workers.values().any(|e| e.session_key.is_some_and(|key| key.vouches(session, token)))
    }

    /// This hub, not kept alive by the handle.
    #[must_use]
    pub fn downgrade(&self) -> WeakHub {
        WeakHub(Arc::downgrade(&self.inner))
    }

    /// Take up the projects and the watched terminals a store kept, before any link is served.
    pub fn adopt_projects(&self, mut file: ProjectsFile) {
        let mut state = self.inner.state.lock();
        let now = tokio::time::Instant::now();
        state.watched = file.watched.drain(..).map(|w| (w.term.session, (w, now))).collect();
        let wall = WallMs::now().as_millis();
        for kept in file.keys.drain(..) {
            // How long ago it was used, by the wall clock, is how long before now it was.
            let age = Duration::from_millis(wall.saturating_sub(kept.at.as_millis()));
            let Some(at) = now.checked_sub(age).filter(|_| age < KEY_LIFETIME) else { continue };
            let KeptKey { key, digest, first, at: wall } = kept;
            let digest = blake3::Hash::from_bytes(digest);
            match first {
                First::Change(answer) => {
                    state.project_keys.push_back(Keyed { key, digest, answer, at, wall });
                }
                First::Start(first) => {
                    state.start_keys.push_back(KeyedStart { key, digest, first, at, wall });
                }
            }
        }
        state.steps.adopt_merges(file.merges.drain(..));
        state.projects = Projects::restore(file);
    }

    /// Every project and watched terminal as it is now, as the store writes it after `through`
    /// changes.
    #[must_use]
    pub fn projects_file(&self, through: u64) -> ProjectsFile {
        let state = self.inner.state.lock();
        let watched = state.watched.values().map(|(w, _)| *w).collect();
        let mut file = state.projects.file(watched, through);
        let changes = state.project_keys.iter().map(|k| KeptKey {
            key: k.key.clone(),
            digest: *k.digest.as_bytes(),
            first: First::Change(k.answer.clone()),
            at: k.wall,
        });
        let starts =
            state.start_keys.iter().map(|k| start_kept(&k.key, k.digest, &k.first, k.wall));
        file.keys = changes.chain(starts).collect();
        file.keys.sort_by_key(|k| k.at);
        file.merges = state.steps.merges();
        file
    }

    /// Every change to the projects from now on, in order, for the store to keep: what a
    /// change carries is sent under the hub's lock, never the whole state. A second call
    /// takes the changes from the first.
    #[must_use]
    pub fn keep_projects(&self) -> mpsc::UnboundedReceiver<Keep> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.state.lock().keeper = Some(tx);
        rx
    }

    /// Take up the reports the store kept on their way to the agents, before any link is
    /// served: what was outstanding goes again as each worker registers.
    pub(crate) fn adopt_deliveries(&self, kept: crate::deliver::Kept) {
        let mut state = self.inner.state.lock();
        state.deliveries.adopt(kept, tokio::time::Instant::now(), WallMs::now());
        drop(state);
        self.inner.deliver.notify_one();
    }

    /// The reports on their way to the agents, as the store keeps them.
    #[must_use]
    pub(crate) fn deliveries_file(&self) -> crate::deliver::Kept {
        self.inner.state.lock().deliveries.kept(tokio::time::Instant::now(), WallMs::now())
    }

    /// Keep the reports on their way to the agents in `store`: as they are now, then after
    /// each burst of changes settles, until the hub is gone. The changes are watched from the
    /// call, so none made before the keeper first runs is missed.
    pub(crate) fn keep_deliveries(
        &self,
        store: crate::store::DeliveryStore,
    ) -> impl Future<Output = ()> + use<> {
        let mut changes = self.inner.state.lock().deliveries.changes();
        changes.mark_changed();
        let hub = self.downgrade();
        async move {
            while changes.changed().await.is_ok() {
                tokio::time::sleep(crate::store::PROJECTS_SETTLE).await;
                changes.mark_unchanged();
                let Some(kept) = hub.upgrade().map(|h| h.deliveries_file()) else { return };
                if let Err(e) = store.save(&kept).await {
                    tracing::warn!(path = %store.path().display(), error = %e, "reports not kept");
                }
            }
        }
    }

    /// Stop sending the store changes: it writes what it has and finishes.
    pub fn stop_keeping(&self) {
        self.inner.state.lock().keeper = None;
    }

    /// Deliver reports to the agents they are for, when each falls due, until the hub is gone.
    pub async fn deliver_reports(hub: WeakHub) {
        let Some(wake) = hub.upgrade().map(|h| Arc::clone(&h.inner.deliver)) else { return };
        loop {
            let notified = wake.notified();
            let Some(next) = hub.upgrade().map(|h| h.deliver_due()) else { return };
            match next {
                Some(at) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(at) => {}
                        () = notified => {}
                    }
                }
                None => notified.await,
            }
        }
    }

    /// The server's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Every worker, by name.
    #[must_use]
    pub fn directory(&self) -> Vec<WorkerInfo> {
        listing(&self.inner.state.lock())
    }

    /// The projects of `snapshot` in parts that each fit a link frame.
    #[must_use]
    pub fn project_parts(snapshot: &mut Snapshot) -> Vec<ProjectsPart> {
        projects::parts(snapshot.seq, std::mem::take(&mut snapshot.projects))
    }

    /// The directory and every project, read together.
    #[must_use]
    pub fn state(&self) -> Snapshot {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let directory = listing(state);
        let projects = Self::projects_snapshot(state);
        // Events are logged under the state lock, so none falls between this and the read.
        let seq = self.inner.log.lock().next.saturating_sub(1);
        drop(guard);
        Snapshot { seq, directory, projects }
    }

    /// How many [`Verb::Events`] are waiting.
    #[cfg(test)]
    pub(crate) fn events_waiting(&self) -> usize {
        self.inner.head.receiver_count()
    }

    /// Directory changes and worker events, as they happen. Subscribe before reading
    /// [`Self::directory`] so nothing falls between the two; a receiver that lags should read the
    /// directory again.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<FromServer> {
        self.inner.events.subscribe()
    }

    /// The directory as it should be persisted, updated only when the registry changes shape
    /// (a worker appears, is renamed, moves, changes capabilities, or loses its link), never on
    /// a load tick.
    #[must_use]
    pub fn persisted(&self) -> watch::Receiver<Vec<WorkerInfo>> {
        self.inner.persist.subscribe()
    }

    /// Register a worker that connected from `ip`. `tx` carries what the server sends it; the
    /// caller queues `Welcome` on it before registering, so no forwarded request overtakes it.
    ///
    /// A worker whose id is on a live link already is refused: a second machine claiming the
    /// id, or the same worker back before its old connection timed out (it retries).
    pub fn register(
        &self,
        registration: Registration,
        ip: IpAddr,
        tx: mpsc::Sender<FromServer>,
    ) -> Result<Lease, Refusal> {
        let mut state = self.inner.state.lock();
        let Registration { worker, name, listen, caps, sessions, session_key } = registration;
        let key = SessionKey::from_bytes(session_key);
        if state.workers.get(&worker).is_some_and(|e| e.link.is_some()) {
            return Err(Refusal::DuplicateWorker);
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        let info = WorkerInfo {
            worker,
            name,
            address: SocketAddr::new(ip, listen.port()).to_string(),
            liveness: Liveness::Online,
            caps,
            // The worker reports it right after its hello.
            load: 0.0,
            last_seen_ms: WallMs::now(),
        };
        let link = Some(Link { tx, pending: HashMap::new() });
        let replaced: Vec<WorkerId> = state
            .workers
            .values()
            .filter(|e| e.info.worker != worker && e.link.is_none())
            .filter(|e| e.info.name == info.name && e.info.address == info.address)
            .map(|e| e.info.worker)
            .collect();
        for old in &replaced {
            tracing::info!(worker = %old, name = %info.name, by = %worker, "worker replaced");
            if let Some(gone) = state.workers.remove(old) {
                self.close_all(&mut state, *old, &gone.sessions);
                self.happen(Happening::WorkerRemoved { worker: *old, name: gone.info.name });
            }
        }
        let (reshaped, gone, opened) = if let Some(entry) = state.workers.get_mut(&worker) {
            let reshaped = !same_shape(&entry.info, &info);
            let gone: Vec<SessionId> = entry
                .sessions
                .iter()
                .map(|s| s.id)
                .filter(|id| !sessions.iter().any(|s| s.id == *id))
                .collect();
            let opened: Vec<SessionSummary> = sessions
                .iter()
                .filter(|s| !entry.sessions.iter().any(|old| old.id == s.id))
                .cloned()
                .collect();
            entry.info = info.clone();
            entry.sessions = sessions;
            entry.generation = generation;
            entry.link = link;
            entry.session_key = Some(key);
            entry.branches.retain(|session, _| !gone.contains(session));
            (reshaped, gone, opened)
        } else {
            let opened = sessions.clone();
            let (branches, facts) = (HashMap::new(), Facts::new());
            let entry = Entry {
                info: info.clone(),
                sessions,
                generation,
                link,
                branches,
                facts,
                session_key: Some(key),
            };
            state.workers.insert(worker, entry);
            (true, Vec::new(), opened)
        };
        let (name, liveness) = (info.name.clone(), info.liveness);
        self.happen(Happening::Worker { worker, name: name.clone(), liveness });
        self.announce(FromServer::Worker(info));
        self.machine_seen(&mut state, worker, &name, Seen::Back);
        if !replaced.is_empty() {
            self.announce(FromServer::Directory(listing(&state)));
        }
        for session in gone {
            self.session_closed(&mut state, TermRef { worker, session });
        }
        if let Some(entry) = state.workers.get(&worker) {
            let waiting: Vec<SessionId> = entry
                .sessions
                .iter()
                .filter(|s| ladder::program_blocked(s).is_some())
                .map(|s| s.id)
                .collect();
            ladder::programs_back(&mut state.board, worker, &waiting);
        }
        // After a restart the server knew none of the worker's sessions: what its tasks worked
        // in and it no longer has ended while the server was away.
        let open: Vec<SessionId> = state
            .workers
            .get(&worker)
            .map(|e| e.sessions.iter().map(|s| s.id).collect())
            .unwrap_or_default();
        let ended = state.projects.reconcile(worker, &open, WallMs::now());
        self.projects_moved(&mut state, ended);
        self.free_closed(&mut state, worker);
        for summary in opened {
            let term = TermRef { worker, session: summary.id };
            self.happen(Happening::SessionOpened { worker, summary: Box::new(summary) });
            self.adopt(&mut state, term);
            self.repo_seen(&mut state, term);
        }
        // What was sent to a terminal that closed while the server was away waits for its
        // node's next; what was sent to its agents and never handed over goes again.
        for term in state.deliveries.gone_on(worker, &open) {
            state.deliveries.closed(term);
        }
        let now = tokio::time::Instant::now();
        let mut unsent = false;
        for batch in state.deliveries.outstanding_on(worker) {
            unsent |= !Self::push_batch(&mut state, &batch, now);
        }
        if unsent {
            self.inner.deliver.notify_one();
        }
        self.unpark_deliveries(&mut state);
        // What a restart of the server left under way on it is taken up.
        self.resume_steps(&mut state, worker);
        // A lane that stopped for want of a worker, or one a restart left, goes on.
        self.kick_all(&mut state);
        if reshaped || !replaced.is_empty() {
            self.persist(&state);
        }
        drop(state);
        Ok(Lease { hub: self.clone(), worker, generation })
    }

    /// Answer a verb that comes with no key, from the person.
    pub async fn dispatch(&self, verb: Verb) -> Outcome {
        self.dispatch_keyed(None, verb).await
    }

    /// Answer a verb from the person ([`Self::dispatch_as`]).
    pub async fn dispatch_keyed(&self, key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
        self.dispatch_as(Speaker::Person, key, verb).await
    }

    /// Whether an agent's `speaker` may report on `task` of `project`: only from that task's
    /// own terminal, proven by its token, so no agent speaks for another's task.
    fn reports_own(
        &self,
        speaker: Speaker,
        project: &ProjectId,
        task: TaskId,
    ) -> Result<(), Outcome> {
        let Some(session) = speaker.proven() else {
            return Err(error(
                ErrorCode::Forbidden,
                &format!(
                    "an agent reports only on its own task, from the terminal the server started \
                     for it; this caller shows no {} for its terminal",
                    slopty_proto::ctl::SESSION_TOKEN_ENV
                ),
            ));
        };
        let on = {
            let state = self.inner.state.lock();
            term_of(&state, session).and_then(|term| state.projects.working_on(term))
        };
        match on {
            Some((p, Some(t))) if p == *project && t == task => Ok(()),
            Some((p, Some(t))) => Err(error(
                ErrorCode::Forbidden,
                &format!("this terminal works on task {t} of {p}, not task {task} of {project}"),
            )),
            Some((p, None)) => Err(error(
                ErrorCode::Forbidden,
                &format!("this terminal is the orchestrator of {p}, which reports to no one"),
            )),
            None => Err(error(
                ErrorCode::Forbidden,
                "this terminal works on no task now, so it reports on none",
            )),
        }
    }

    /// Whether an agent may type into `term`: a shell or a program, not another agent's TUI.
    ///
    /// Keys typed into Claude Code reach more than its prompt: `!` runs a command unasked,
    /// `/permissions` and `/sandbox` add what it may do unasked, and shift-tab cycles its mode.
    /// The TUI is the person's, so an agent speaks to another through reports and Claude
    /// Code's own messages, which it reads as a peer's, never as the person's keys. The person
    /// allows it per project (`[server.projects] permission_flags`).
    fn types_into_shell(&self, from: Option<SessionId>, term: TermRef) -> Result<(), Outcome> {
        let state = self.inner.state.lock();
        let agent = state.board.agent_at(term).is_some();
        let project = projects::project_of(&state, term);
        let allowed = projects::allowance(&state, Caller::Agent, from, project.as_ref());
        drop(state);
        if !agent || allowed {
            return Ok(());
        }
        Err(error(
            ErrorCode::Forbidden,
            "an agent's TUI takes keys from the person only: an agent reaches another with \
             task_report or Claude Code's own messages to its session",
        ))
    }

    /// Whether an agent may start what `verb` starts with the environment it names. A variable
    /// that moves what runs or what it may do (where programs and settings are found, what a
    /// runtime loads, which worker socket answers its hooks, which server or task it speaks
    /// for) would give the started program more than its starter has, so an agent names none
    /// unless the person allows looser starts for the project (`[server.projects]
    /// permission_flags`).
    fn env_of_agent(&self, from: Option<SessionId>, verb: &Verb) -> Result<(), Outcome> {
        let (envs, project) = match verb {
            Verb::SpawnAgent { env, .. } | Verb::OpenTerminal { env, .. } => (vec![env], None),
            Verb::TaskSpawn { project, launch, .. } => (vec![&launch.env], Some(project)),
            _ => return Ok(()),
        };
        let mut names = envs.into_iter().flatten().map(|(name, _)| name.as_str());
        let Some(name) = names.find(|n| steers(n)) else {
            return Ok(());
        };
        let allowed = projects::allowance(&self.inner.state.lock(), Caller::Agent, from, project);
        if allowed {
            return Ok(());
        }
        Err(error(
            ErrorCode::Limit,
            &format!(
                "{name} in an agent's start may give what it starts more than the agent has; the \
                 person allows that only in the server's settings.toml (`[server.projects] \
                 permission_flags`)"
            ),
        ))
    }

    /// Who `speaker` is now: the person or an agent.
    fn caller(&self, speaker: Speaker) -> Caller {
        match speaker {
            Speaker::Person => Caller::Person,
            Speaker::Agent | Speaker::Proven(_) => Caller::Agent,
            Speaker::Shell(session) | Speaker::ProvenShell(session) => {
                let state = self.inner.state.lock();
                let agent_here = term_of(&state, session)
                    .is_some_and(|term| state.board.agent_at(term).is_some());
                let working = term_of(&state, session)
                    .is_none_or(|term| state.projects.working_on(term).is_some());
                let driven = projects::driven(&state, session);
                drop(state);
                // A terminal the server does not know is nobody's to vouch for.
                if agent_here || working || driven { Caller::Agent } else { Caller::Person }
            }
        }
    }

    /// Answer a verb from `speaker`: the directory verbs from the registry, the projects from
    /// the store, the rest forwarded to the owning worker with `key` and its reply returned.
    /// Never fails; a failure is an [`Outcome::Error`].
    pub async fn dispatch_as(
        &self,
        speaker: Speaker,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> Outcome {
        let caller = self.caller(speaker);
        let from = speaker.proven();
        let verb = match caller {
            Caller::Agent => {
                let scoped = projects::agent_scope(&self.inner.state.lock(), from, verb);
                match scoped {
                    Ok(verb) => verb,
                    Err(refused) => return refused,
                }
            }
            Caller::Person => verb,
        };
        if caller == Caller::Agent
            && let Verb::SendInput { term, .. } = &verb
        {
            if let Err(refused) = self.types_into_shell(from, *term) {
                return refused;
            }
            projects::watch(&mut self.inner.state.lock(), *term, |w| {
                w.drove.get_or_insert(Drove::Typed);
            });
        }
        if caller == Caller::Agent
            && let Verb::TaskReport { project, task, .. } = &verb
            && let Err(refused) = self.reports_own(speaker, project, *task)
        {
            return refused;
        }
        if caller == Caller::Agent
            && let Err(refused) = self.env_of_agent(from, &verb)
        {
            return refused;
        }
        match verb {
            Verb::ListWorkers => Outcome::Workers(self.directory()),
            Verb::ListTerminals { worker } => self.terminals(worker),
            Verb::Events { since, timeout_ms, filter } => {
                self.events(since, timeout_ms, filter).await
            }
            Verb::ForgetWorker { worker } => self.forget_worker(key, worker),
            Verb::Wake { worker } => self.wake(worker).await,
            Verb::ProjectList => Outcome::Projects(self.inner.state.lock().projects.list()),
            Verb::ProjectStatus { project, since, timeout_ms } => {
                self.project_status(&project, since, timeout_ms).await
            }
            Verb::TaskSpawn { project, task, launch } => {
                self.task_spawn(caller, key, project, task, launch).await
            }
            Verb::TaskRestart { project, task, agent } => {
                self.task_restart(caller, key, project, task, agent).await
            }
            Verb::WorkerFacts { worker } => self.worker_facts(worker),
            Verb::TaskGet { project, task } => self.task_get(&project, task),
            Verb::WorkingOn { session } => self.working_on(session),
            verb @ Verb::SpawnAgent { .. } => self.spawn_agent(caller, from, key, verb).await,
            verb @ Verb::OpenTerminal { .. } => self.open_terminal(caller, from, key, verb).await,
            // Only the person answers a permission, so only the person's read holds prompts.
            Verb::ReadThread { of, view, after, .. } => match self.thread_on(of) {
                Ok(of) => {
                    let hold = caller == Caller::Person;
                    self.forward(key, Verb::ReadThread { of, view, after, hold }).await
                }
                Err(refused) => refused,
            },
            Verb::AnswerRequest { .. } if caller == Caller::Agent => Outcome::Error {
                code: ErrorCode::Forbidden,
                message: "a request is the person's to answer, never an agent's; it waits in \
                          the agent's thread and on the person's devices"
                    .to_owned(),
            },
            Verb::AnswerRequest { of, ask, choice, message } => match self.thread_on(of) {
                Ok(of) => self.forward(key, Verb::AnswerRequest { of, ask, choice, message }).await,
                Err(refused) => refused,
            },
            Verb::CloneRepo { .. } | Verb::BundleBranch { .. } | Verb::FetchBundle { .. } => error(
                ErrorCode::Forbidden,
                "the server clones and carries branches for tasks itself; task_start and \
                     task_report do it",
            ),
            Verb::ProjectDelete { .. } if caller == Caller::Agent => {
                error(ErrorCode::Forbidden, "a project is the person's to let go, never an agent's")
            }
            verb @ (Verb::TaskPush { .. } | Verb::ProjectDelete { .. }) => {
                self.once(caller, key, verb).await
            }
            Verb::Verify { .. }
            | Verb::Rebase { .. }
            | Verb::FastForward { .. }
            | Verb::CatchUp { .. }
            | Verb::LandPull { .. }
            | Verb::TestDiff { .. } => error(
                ErrorCode::Forbidden,
                "the server checks and merges tasks itself, one at a time; a task's done report \
                 starts its checks, and the person's task merge lands it",
            ),
            Verb::RemoveWorktree { .. } => error(
                ErrorCode::Forbidden,
                "the server frees a finished task's worktree itself, once its agent is closed",
            ),
            Verb::DropBranches { .. } => error(
                ErrorCode::Forbidden,
                "the server drops the branches it named itself, once their work has landed",
            ),
            Verb::Settings { .. } if caller == Caller::Agent => error(
                ErrorCode::Forbidden,
                "a machine's settings are the person's to read and change, never an agent's",
            ),
            Verb::Settings { of: None, edits } => self.own_settings(edits).await,
            Verb::StartThread { .. } => error(
                ErrorCode::Forbidden,
                "the server starts a task's thread itself; task_start with an agent asks for it",
            ),
            verb @ (Verb::ProjectCreate { .. }
            | Verb::ProjectSet { .. }
            | Verb::TaskCreate { .. }
            | Verb::TaskUpdate { .. }
            | Verb::TaskReport { .. }
            | Verb::TaskTell { .. }
            | Verb::TaskMerge { .. }) => self.project_change(caller, key, &verb),
            other => self.forward(key, other).await,
        }
    }

    /// A push or a project let go, made once under `key`: a repeat answers as the first did.
    async fn once(&self, caller: Caller, key: Option<IdempotencyKey>, verb: Verb) -> Outcome {
        if let Some(key) = &key {
            let mut state = self.inner.state.lock();
            if let Some(answer) = keyed(&mut state, caller, key, &verb) {
                return answer;
            }
        }
        let outcome = match &verb {
            Verb::TaskPush { project, task } => self.task_push(caller, (project, *task)).await,
            Verb::ProjectDelete { project } => self.project_delete(project),
            _ => error(ErrorCode::Invalid, "only a push or a project let go is made once here"),
        };
        if let Some(key) = key {
            remember(&mut self.inner.state.lock(), caller, key, &verb, &outcome);
        }
        outcome
    }

    /// The events from `since` (from now when absent) that pass `filter`, waiting up to
    /// `timeout_ms` (capped at [`WAIT_CAP_MS`]) for a first one.
    async fn events(&self, since: Option<u64>, timeout_ms: u32, filter: EventFilter) -> Outcome {
        let wait = Duration::from_millis(u64::from(timeout_ms.min(WAIT_CAP_MS)));
        let deadline = tokio::time::Instant::now().checked_add(wait);
        let mut head = self.inner.head.subscribe();
        let mut from = since.unwrap_or_else(|| *head.borrow_and_update());
        let mut missed = 0_u64;
        loop {
            // Seen before the read: an event logged after it wakes the wait below.
            head.borrow_and_update();
            let page = self.read_log(from, filter);
            // The log can wrap past the cursor between two reads of one long wait.
            missed = missed.saturating_add(page.missed);
            if !page.events.is_empty() {
                return Outcome::Events { events: page.events, next: page.next, missed };
            }
            from = page.next;
            let woke = match deadline {
                Some(at) => tokio::time::timeout_at(at, head.changed()).await,
                None => Ok(head.changed().await),
            };
            if !matches!(woke, Ok(Ok(()))) {
                return Outcome::Events { events: Vec::new(), next: from, missed };
            }
        }
    }

    fn read_log(&self, from: u64, filter: EventFilter) -> Page {
        let log = self.inner.log.lock();
        let oldest = log.ring.front().map_or(log.next, |e| e.seq);
        let (from, missed) = if (log.first..=log.next).contains(&from) {
            (from.max(oldest), oldest.saturating_sub(from))
        } else {
            // From another run (or a clock set back): everything held is new to it, and what
            // that run logged after the cursor is unknown, so it missed at least one.
            (oldest, oldest.saturating_sub(log.first).saturating_add(1))
        };
        let skip = usize::try_from(from.saturating_sub(oldest)).unwrap_or(usize::MAX);
        let mut page = Page { events: Vec::new(), next: log.next, missed };
        let mut bytes = 0_usize;
        for event in log.ring.iter().skip(skip) {
            if page.events.len() == EVENTS_PER_ANSWER || bytes > EVENTS_BYTES {
                page.next = event.seq;
                break;
            }
            if filter.admits(&event.what) {
                bytes = bytes.saturating_add(match &event.what {
                    Happening::Project(update) => update.approx_bytes(),
                    _ => 1024,
                });
                page.events.push(event.clone());
            }
        }
        drop(log);
        page
    }

    /// Remove a worker that holds no lease. A live one would register again at once, so it is
    /// refused; every link hears the directory without the worker, and the state file loses it.
    /// A repeat under the key of a forget that happened answers as it did.
    fn forget_worker(&self, key: Option<IdempotencyKey>, worker: WorkerId) -> Outcome {
        let mut state = self.inner.state.lock();
        let now = tokio::time::Instant::now();
        state.forgets.retain(|f| now.duration_since(f.at) < KEY_LIFETIME);
        if let Some(key) = &key
            && let Some(first) = state.forgets.iter().find(|f| f.key == *key)
        {
            return if first.worker == worker { Outcome::Done } else { key.reused() };
        }
        let Some(entry) = state.workers.get(&worker) else { return unknown_worker(worker) };
        if entry.link.is_some() {
            return Outcome::Error {
                code: ErrorCode::Invalid,
                message: format!(
                    "worker {} ({worker}) is online and would register again at once; stop it \
                     first (`slopty worker uninstall` on that machine)",
                    entry.info.name
                ),
            };
        }
        let Some(gone) = state.workers.remove(&worker) else { return unknown_worker(worker) };
        tracing::info!(%worker, name = %gone.info.name, "worker forgotten");
        self.close_all(&mut state, worker, &gone.sessions);
        self.happen(Happening::WorkerRemoved { worker, name: gone.info.name });
        self.announce(FromServer::Directory(listing(&state)));
        self.persist(&state);
        if let Some(key) = key {
            if state.forgets.len() == FORGETS_KEPT {
                state.forgets.pop_front();
            }
            state.forgets.push_back(Forget { key, worker, at: now });
        }
        drop(state);
        Outcome::Done
    }

    /// Wake a worker that is not online: the server sends the magic packet itself when it
    /// shares a subnet with one of the worker's last reported ports, else it asks each online
    /// worker that does, in turn, until one has sent it.
    async fn wake(&self, worker: WorkerId) -> Outcome {
        let (name, ports, senders) = {
            let state = self.inner.state.lock();
            let Some(entry) = state.workers.get(&worker) else { return unknown_worker(worker) };
            let name = entry.info.name.clone();
            if entry.link.is_some() {
                return Outcome::Error {
                    code: ErrorCode::Invalid,
                    message: format!("worker {name} ({worker}) is online, so awake already"),
                };
            }
            if entry.info.caps.lan.is_empty() {
                return Outcome::Error {
                    code: ErrorCode::Unsupported,
                    message: format!(
                        "worker {name} ({worker}) reported no LAN interface to wake it by"
                    ),
                };
            }
            let ports = entry.info.caps.lan.clone();
            let mut senders: Vec<(f32, WorkerId, String, Vec<LanPort>)> = state
                .workers
                .values()
                .filter(|e| e.link.is_some() && e.info.worker != worker)
                .map(|e| (e.info.load, e.info.worker, e.info.name.clone(), e.info.caps.lan.clone()))
                .collect();
            drop(state);
            // The least loaded first: the one most likely to answer at once.
            senders.sort_by(|a, b| a.0.total_cmp(&b.0));
            (name, ports, senders)
        };
        let own = self.inner.lan.ports();
        let mut sent = Vec::new();
        for (from, targets) in slopty_tailnet::lan::plan(&own, &ports) {
            let macs = targets.iter().map(|t| t.mac).collect();
            match self.inner.lan.wake(from.clone(), macs).await {
                Ok(()) => sent.extend(targets.into_iter().map(|t| t.interface)),
                Err(e) => tracing::warn!(%worker, from = %from.interface, error = %e, "wake"),
            }
        }
        if !sent.is_empty() {
            tracing::info!(%worker, %name, to = ?sent, "woke from the server");
            return Outcome::WakeSent { by: self.inner.name.clone(), to: sent };
        }
        for (_, sender, by, lan) in senders {
            let peer: Vec<LanPort> =
                slopty_tailnet::lan::plan(&lan, &ports).into_iter().flat_map(|(_, t)| t).collect();
            if peer.is_empty() {
                continue;
            }
            let to = peer.iter().map(|t| t.interface.clone()).collect();
            match self.forward(None, Verb::WakePeer { worker: sender, peer }).await {
                Outcome::Done => {
                    tracing::info!(%worker, %name, by = %by, "woke through a worker");
                    return Outcome::WakeSent { by, to };
                }
                other => tracing::warn!(%worker, by = %by, ?other, "wake through a worker"),
            }
        }
        let subnets: Vec<String> =
            ports.iter().map(|p| format!("{}/{}", p.addr, p.prefix)).collect();
        Outcome::Error {
            code: ErrorCode::Failed,
            message: format!(
                "nothing online shares a LAN with worker {name} ({worker}) to wake it: the \
                 server and every online worker are off its subnet ({})",
                subnets.join(", ")
            ),
        }
    }

    fn terminals(&self, only: Option<WorkerId>) -> Outcome {
        let state = self.inner.state.lock();
        if let Some(worker) = only
            && !state.workers.contains_key(&worker)
        {
            return unknown_worker(worker);
        }
        let mut terminals: Vec<(WorkerId, SessionSummary)> = state
            .workers
            .values()
            .filter(|e| only.is_none_or(|w| w == e.info.worker))
            .flat_map(|e| e.sessions.iter().map(|s| (e.info.worker, s.clone())))
            .collect();
        terminals.sort_by_key(|(worker, s)| (*worker, s.id));
        let agents = terminals
            .iter()
            .filter_map(|(worker, s)| {
                let term = TermRef { worker: *worker, session: s.id };
                Some((term, state.board.agent_at(term)?))
            })
            .collect();
        drop(state);
        Outcome::Terminals { terminals, agents }
    }

    async fn forward(&self, key: Option<IdempotencyKey>, mut verb: Verb) -> Outcome {
        let deadline = match &mut verb {
            Verb::WaitFor { timeout_ms, .. } => {
                *timeout_ms = (*timeout_ms).min(WAIT_CAP_MS);
                Duration::from_millis(u64::from(*timeout_ms)).saturating_add(WAIT_GRACE)
            }
            Verb::CloneRepo { .. } => CLONE_TIMEOUT,
            Verb::BundleBranch { .. } | Verb::FetchBundle { .. } => BUNDLE_TIMEOUT,
            _ => FORWARD_TIMEOUT,
        };
        let Some(worker) = target(&verb) else {
            return error(ErrorCode::Invalid, "this verb names no worker");
        };
        let (id, generation, tx, reply) = match self.expect_reply(worker) {
            Ok(pending) => pending,
            Err(outcome) => return outcome,
        };
        // However this call ends, answered, timed out or dropped by its caller mid-wait, the
        // request stops waiting on the link.
        let _pending = Pending { hub: self, worker, generation, id };
        // The deadline holds from here: a link that stays up and does not drain holds the
        // request no longer than an answer that does not come.
        let end = tokio::time::Instant::now().checked_add(deadline);
        let end = end.unwrap_or_else(tokio::time::Instant::now);
        match tokio::time::timeout_at(end, tx.send(FromServer::Request { id, key, verb })).await {
            Ok(Ok(())) => {}
            Ok(Err(_closed)) => {
                return error(ErrorCode::WorkerUnreachable, "the worker's link closed");
            }
            Err(_elapsed) => {
                return error(
                    ErrorCode::WorkerUnreachable,
                    "the worker's link took nothing in time; the request was not sent",
                );
            }
        }
        match tokio::time::timeout_at(end, reply).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_dropped)) => error(
                ErrorCode::Interrupted,
                "the worker disconnected before it answered; it may have done the work, and \
                 sent again under the same idempotency key it will not do it twice",
            ),
            Err(_elapsed) => error(
                ErrorCode::Interrupted,
                "the worker did not answer in time; it may have done the work, and sent again \
                 under the same idempotency key it will not do it twice",
            ),
        }
    }

    /// Requests waiting on `worker`'s link.
    #[cfg(test)]
    fn pending(&self, worker: WorkerId) -> usize {
        let state = self.inner.state.lock();
        state.workers.get(&worker).and_then(|e| e.link.as_ref()).map_or(0, |l| l.pending.len())
    }

    /// A new request id pending on `worker`'s link, with the link's generation, its queue and
    /// where the reply will arrive.
    fn expect_reply(
        &self,
        worker: WorkerId,
    ) -> Result<(RequestId, u64, mpsc::Sender<FromServer>, oneshot::Receiver<Outcome>), Outcome>
    {
        let mut state = self.inner.state.lock();
        state.next_request = state.next_request.wrapping_add(1);
        let id = state.next_request;
        let Some(entry) = state.workers.get_mut(&worker) else {
            return Err(unknown_worker(worker));
        };
        let Some(link) = entry.link.as_mut() else {
            return Err(unreachable(&entry.info));
        };
        let (reply_tx, reply) = oneshot::channel();
        link.pending.insert(id, reply_tx);
        let pending = (id, entry.generation, link.tx.clone(), reply);
        drop(state);
        Ok(pending)
    }

    /// Drop a forwarded request nobody waits for any more.
    fn forget(&self, worker: WorkerId, generation: u64, id: RequestId) {
        if let Some(entry) = self.inner.state.lock().workers.get_mut(&worker)
            && entry.generation == generation
            && let Some(link) = entry.link.as_mut()
        {
            link.pending.remove(&id);
        }
    }

    /// Every session of a removed worker ended, for the log and every link: its agents stop
    /// counting as waiting anywhere.
    fn close_all(&self, state: &mut State, worker: WorkerId, sessions: &[SessionSummary]) {
        for session in sessions.iter().map(|s| s.id) {
            self.session_closed(state, TermRef { worker, session });
        }
    }

    /// A terminal ended: logged, and an agent working in it is gone from its task; what was
    /// sent to it and never handed over waits for its node's next terminal.
    fn session_closed(&self, state: &mut State, term: TermRef) {
        self.happen(Happening::SessionClosed { term });
        if let Some(entry) = state.workers.get_mut(&term.worker) {
            entry.branches.remove(&term.session);
        }
        state.deliveries.closed(term);
        projects::unwatch(state, term.session);
        let updates = state.projects.session_ended(term, WallMs::now());
        self.projects_moved(state, updates);
        self.free_closed(state, term.worker);
    }

    /// Log and push each project change, and send the store what it keeps. Called under the
    /// state lock, as [`Self::happen`] is, so the store and every link see the changes in the
    /// order they were made.
    fn projects_moved(&self, state: &mut State, mut changes: Vec<Change>) {
        changes.extend(self.hear(state));
        if changes.is_empty() {
            return;
        }
        for change in changes {
            ladder::tell_project(state, &change.kept);
            self.happen(Happening::Project(Box::new(state.projects.pushed(&change.kept))));
            if change.durable {
                projects::keep(state, Keep::Project(Box::new(change.kept)));
            }
        }
        self.unpark_deliveries(state);
    }

    /// A node may have a terminal now for reports that waited for one.
    fn unpark_deliveries(&self, state: &mut State) {
        if state.deliveries.unpark() {
            self.inner.deliver.notify_one();
        }
    }

    fn announce(&self, msg: FromServer) {
        let _no_listeners = self.inner.events.send(msg);
    }

    /// Log an event under the next sequence number, wake the waiting [`Verb::Events`] and
    /// push it to every link. Called under the state lock, so the log's order and every link's
    /// are the order changes were made.
    fn happen(&self, what: Happening) {
        let mut log = self.inner.log.lock();
        let seq = log.next;
        log.next = seq.saturating_add(1);
        let event = HubEvent { seq, at_ms: WallMs::now(), what };
        // Room is made before the push: a push onto a full ring would double its buffer to
        // hold one event over the bound, and a ring cycles through all of its buffer.
        if log.ring.len() == EVENT_LOG {
            log.ring.pop_front();
        }
        log.ring.push_back(event.clone());
        self.inner.head.send_replace(log.next);
        drop(log);
        self.announce(FromServer::Event(event));
    }

    fn persist(&self, state: &State) {
        let mut all: Vec<WorkerInfo> = state.workers.values().map(|e| e.info.clone()).collect();
        all.sort_by_key(|w| w.worker);
        self.inner.persist.send_replace(all);
    }

    /// The lease of `generation` ended.
    fn lost(&self, worker: WorkerId, generation: u64) {
        let mut state = self.inner.state.lock();
        let Some(entry) = state.workers.get_mut(&worker) else { return };
        if entry.generation != generation || entry.link.is_none() {
            return;
        }
        entry.link = None;
        entry.info.liveness = Liveness::Unreachable;
        entry.info.last_seen_ms = WallMs::now();
        let info = entry.info.clone();
        tracing::info!(%worker, name = %info.name, "worker unreachable");
        let (name, liveness) = (info.name.clone(), info.liveness);
        self.happen(Happening::Worker { worker, name: name.clone(), liveness });
        self.announce(FromServer::Worker(info));
        self.machine_seen(&mut state, worker, &name, Seen::Away);
        self.persist(&state);
        drop(state);
        // A lease dropped as the runtime shuts down has no timer to run; the next start lists
        // the worker as gone anyway.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let hub = Arc::downgrade(&self.inner);
            runtime.spawn(async move {
                tokio::time::sleep(GONE_AFTER).await;
                if let Some(inner) = hub.upgrade() {
                    Self { inner }.expire(worker, generation);
                }
            });
        }
    }

    /// The machine of `worker` went away or came back: its tasks' timelines say so, and each
    /// task's orchestrator hears of it, at once when it went away.
    fn machine_seen(&self, state: &mut State, worker: WorkerId, name: &str, seen: Seen) {
        let (changes, told) = state.projects.machine_seen(worker, name, seen, WallMs::now());
        self.projects_moved(state, changes);
        if told.is_empty() {
            return;
        }
        let at = tokio::time::Instant::now();
        for (project, task) in told {
            let (kind, words) = match seen {
                Seen::Away => (
                    crate::deliver::Kind::Stuck,
                    format!(
                        "task {task}'s machine {name} stopped answering, with its agent at work \
                         there. Its work waits for the machine to come back; to go on without \
                         it, start the task again on another machine (task_start)."
                    ),
                ),
                Seen::Back => (
                    crate::deliver::Kind::NeedsInput,
                    format!("task {task}'s machine {name} answers again; its agent goes on."),
                ),
            };
            state.deliveries.notice((project, None), task, kind, &words, at);
        }
        self.inner.deliver.notify_one();
    }

    fn expire(&self, worker: WorkerId, generation: u64) {
        let mut state = self.inner.state.lock();
        let Some(entry) = state.workers.get_mut(&worker) else { return };
        if entry.generation != generation || entry.info.liveness != Liveness::Unreachable {
            return;
        }
        entry.info.liveness = Liveness::Gone;
        tracing::info!(%worker, name = %entry.info.name, "worker gone");
        let info = entry.info.clone();
        let name = info.name.clone();
        self.happen(Happening::Worker { worker, name, liveness: Liveness::Gone });
        drop(state);
        self.announce(FromServer::Worker(info));
    }
}

/// The orchestration tools run on the hub in-process, the same dispatch the QUIC links use,
/// for the person.
impl slopty_tools::Dispatch for Hub {
    fn send(
        &self,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> impl Future<Output = Outcome> + Send {
        self.dispatch_keyed(key, verb)
    }
}

/// The hub as one speaker sees it: what the MCP endpoint serves agents through.
#[derive(Clone, Debug)]
pub struct Acting {
    hub: Hub,
    speaker: Speaker,
}

impl Acting {
    /// `hub`, answering `speaker`.
    #[must_use]
    pub const fn new(hub: Hub, speaker: Speaker) -> Self {
        Self { hub, speaker }
    }
}

impl slopty_tools::Dispatch for Acting {
    fn send(
        &self,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> impl Future<Output = Outcome> + Send {
        self.hub.dispatch_as(self.speaker, key, verb)
    }
}

impl Lease {
    /// The worker holding it.
    #[must_use]
    pub const fn worker(&self) -> WorkerId {
        self.worker
    }

    /// Whether a pocketed phone can answer, as it moves, for the worker's link to send
    /// ([`FromServer::Pushes`]).
    #[must_use]
    pub fn pushes(&self) -> watch::Receiver<bool> {
        self.hub.inner.state.lock().board.pushes()
    }

    /// Answer forwarded request `id` here, in the worker's place: it could not be sent.
    pub fn answer(&self, id: RequestId, outcome: Outcome) {
        let mut state = self.hub.inner.state.lock();
        let waiter = state
            .workers
            .get_mut(&self.worker)
            .filter(|e| e.generation == self.generation)
            .and_then(|e| e.link.as_mut())
            .and_then(|l| l.pending.remove(&id));
        drop(state);
        if let Some(waiter) = waiter {
            let _gave_up = waiter.send(outcome);
        }
    }

    /// Take in one message from the worker.
    pub fn handle(&self, msg: ToServer) {
        let hub = &self.hub;
        let mut state = hub.inner.state.lock();
        let Some(entry) = state.workers.get_mut(&self.worker) else { return };
        if entry.generation != self.generation {
            return;
        }
        entry.info.last_seen_ms = WallMs::now();
        let worker = self.worker;
        match msg {
            ToServer::Reply { id, outcome } => {
                let waiter = entry.link.as_mut().and_then(|l| l.pending.remove(&id));
                if let Some(waiter) = waiter {
                    let _gave_up = waiter.send(outcome);
                } else {
                    tracing::debug!(%worker, id, "reply to no pending request");
                }
            }
            ToServer::Caps(caps) => {
                if entry.info.caps != caps {
                    entry.info.caps = caps;
                    hub.announce(FromServer::Worker(entry.info.clone()));
                    hub.persist(&state);
                }
            }
            ToServer::Load(load) => {
                entry.info.load = load;
                hub.announce(FromServer::Load { worker, load });
            }
            ToServer::SessionChanged(summary) => {
                let term = TermRef { worker, session: summary.id };
                // A known session reports a change (a resize, a new directory): no event of its
                // own, but for its program exiting.
                let waited = entry.sessions.iter().find(|s| s.id == summary.id);
                let waited = waited.is_some_and(|known| ladder::program_blocked(known).is_some());
                ladder::program_moved(&mut state.board, term, waited, Some(&summary));
                let Some(entry) = state.workers.get_mut(&worker) else { return };
                if let Some(known) = entry.sessions.iter_mut().find(|s| s.id == summary.id) {
                    let was = known.state;
                    known.clone_from(&summary);
                    if let (SessionState::Running, SessionState::Exited { status }) =
                        (was, summary.state)
                    {
                        hub.happen(Happening::SessionExited { term, status });
                        // The program in a task's terminal ended: the node above hears.
                        state.projects.exited(term);
                        hub.projects_moved(&mut state, Vec::new());
                    }
                } else {
                    entry.sessions.push(summary.clone());
                    let term = TermRef { worker, session: summary.id };
                    hub.happen(Happening::SessionOpened { worker, summary: Box::new(summary) });
                    hub.adopt(&mut state, term);
                    hub.unpark_deliveries(&mut state);
                }
                hub.repo_seen(&mut state, term);
            }
            ToServer::SessionClosed { session, .. } => {
                entry.sessions.retain(|s| s.id != session);
                ladder::program_moved(&mut state.board, TermRef { worker, session }, false, None);
                hub.session_closed(&mut state, TermRef { worker, session });
            }
            ToServer::Facts(facts) => entry.facts = crate::placement::bounded(facts),
            ToServer::Cloning { clone, phase, percent } => {
                hub.clone_moved(&mut state, clone, phase, percent);
            }
            ToServer::Report(report) => {
                let term = TermRef { worker, session: report.session() };
                match &report {
                    AgentReport::Branch(branch) => {
                        if branch.worktree.is_none() {
                            entry.branches.remove(&branch.session);
                        } else {
                            entry.branches.insert(branch.session, branch.clone());
                        }
                    }
                    AgentReport::Delivered { batch, .. } => {
                        hub.delivered(&mut state, term, *batch);
                        return;
                    }
                    AgentReport::PermissionMode { mode, .. } => {
                        hub.permission_mode(&mut state, term, mode);
                    }
                    AgentReport::Loosened { found, .. } => {
                        hub.loosened(&mut state, term, found);
                        return;
                    }
                    AgentReport::SubagentStarted { .. }
                    | AgentReport::SubagentStopped { .. }
                    | AgentReport::NativeTask { .. } => {}
                }
                let moved = state.projects.report(worker, &report, WallMs::now());
                hub.projects_moved(&mut state, moved);
            }
            ToServer::Threads(frame) => {
                let now = WallMs::now();
                let mut moved = Vec::new();
                state.board.take(worker, frame);
                for (term, status) in state.board.seat_moves(worker) {
                    hub.adopt(&mut state, term);
                    moved.extend(state.projects.agent_status(term, &status, now));
                }
                for (terminal, agent) in state.board.rung_moves(worker) {
                    hub.happen(Happening::Rung { worker, terminal, agent });
                }
                // A subagent of an agent with no hooks is a native as one a hook reports.
                for report in state.board.native_moves(worker) {
                    moved.extend(state.projects.report(worker, &report, now));
                }
                let pulls = state.board.pulls(worker);
                let (seen, merged) = state.projects.pulls_seen(worker, &pulls, now);
                moved.extend(seen);
                hub.projects_moved(&mut state, moved);
                for (project, task) in merged {
                    hub.catch_up(project, task);
                }
                hub.threads_ended(&mut state, worker, now);
            }
            ToServer::Hello { .. }
            | ToServer::Request { .. }
            | ToServer::Presence(_)
            | ToServer::PushDevice { .. } => {
                tracing::debug!(%worker, "ignored a message a worker does not send");
            }
        }
        // Announced under the lock, so every link hears changes in the order they were made.
        drop(state);
    }
}

/// Whether the environment variable `name` moves what a started program runs or may do:
/// where programs, shells' startup files and settings are found, what a runtime or the
/// dynamic loader loads, and Slopty's, Claude Code's and Anthropic's own variables.
fn steers(name: &str) -> bool {
    const NAMES: [&str; 7] =
        ["PATH", "HOME", "ZDOTDIR", "BASH_ENV", "ENV", "SHELL", "XDG_CONFIG_HOME"];
    const PREFIXES: [&str; 8] =
        ["SLOPTY_", "CLAUDE", "ANTHROPIC_", "NODE_", "BUN_", "DYLD_", "LD_", "GIT_CONFIG"];
    let upper = name.to_ascii_uppercase();
    NAMES.contains(&upper.as_str()) || PREFIXES.iter().any(|p| upper.starts_with(p))
}

/// A verb's digest as `caller` sent it: equal verbs from the same side, equal digests. A key is
/// its caller's: an agent repeating the person's key is refused as a key used with other
/// arguments, never given the person's answer or start.
fn digest(caller: Caller, verb: &Verb) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[u8::from(caller == Caller::Agent)]);
    hasher.update(&slopty_proto::codec::encode_body(verb).unwrap_or_default());
    hasher.finalize()
}

/// The answer to a repeat of a keyed project change: the first one's (what it named, read
/// again), or a refusal of the key used with other arguments; `None` for a key not used yet.
fn keyed(state: &mut State, caller: Caller, key: &IdempotencyKey, verb: &Verb) -> Option<Outcome> {
    let now = tokio::time::Instant::now();
    state.project_keys.retain(|k| now.duration_since(k.at) < KEY_LIFETIME);
    let first = state.project_keys.iter().find(|k| k.key == *key)?;
    if first.digest != digest(caller, verb) {
        return Some(key.reused());
    }
    Some(match &first.answer {
        Remembered::Outcome(outcome) => outcome.clone(),
        Remembered::Task(project, task) => match state.projects.task(project, *task) {
            Ok(task) => Outcome::Task(Box::new(task.clone())),
            Err(refused) => refused,
        },
        Remembered::Status(project) => {
            let project = project.clone();
            match Hub::status_of(state, &project, None) {
                Ok(status) => Outcome::Project(Box::new(status)),
                Err(refused) => refused,
            }
        }
    })
}

/// Keep what a keyed project change answered, for a repeat of it.
fn remember(
    state: &mut State,
    caller: Caller,
    key: IdempotencyKey,
    verb: &Verb,
    outcome: &Outcome,
) {
    if state.project_keys.len() >= KEYS_REMEMBERED {
        state.project_keys.pop_front();
    }
    let project = match verb {
        Verb::TaskCreate { project, .. }
        | Verb::TaskUpdate { project, .. }
        | Verb::TaskSpawn { project, .. }
        | Verb::TaskRestart { project, .. }
        | Verb::TaskTell { project, .. }
        | Verb::TaskReport { project, .. }
        | Verb::TaskMerge { project, .. }
        | Verb::TaskPush { project, .. } => Some(project),
        _ => None,
    };
    let answer = match (outcome, project) {
        (Outcome::Task(task), Some(project)) => Remembered::Task(project.clone(), task.id),
        (Outcome::Project(status), _) => Remembered::Status(status.project.id.clone()),
        (other, _) => Remembered::Outcome(other.clone()),
    };
    let (at, wall, digest) = (tokio::time::Instant::now(), WallMs::now(), digest(caller, verb));
    let kept = KeptKey {
        key: key.clone(),
        digest: *digest.as_bytes(),
        first: First::Change(answer.clone()),
        at: wall,
    };
    state.project_keys.push_back(Keyed { key, digest, answer, at, wall });
    projects::keep(state, Keep::Key(Box::new(kept)));
}

/// A repeat of a keyed start: what the first start was answered, or the start it forwarded
/// while its answer is not sure, or a refusal of the key used with other arguments; `None` for
/// a key not used yet. `sent` is the digest of what the caller sent, before the hub chose
/// anything.
fn start_again(
    state: &mut State,
    key: &IdempotencyKey,
    sent: blake3::Hash,
) -> Option<Result<Again, Outcome>> {
    let now = tokio::time::Instant::now();
    state.start_keys.retain(|k| now.duration_since(k.at) < KEY_LIFETIME);
    let first = state.start_keys.iter().find(|k| k.key == *key)?;
    if first.digest != sent {
        return Some(Err(key.reused()));
    }
    Some(Ok(match &first.first {
        StartKept::Forwarded(verb) => Again::Forward((**verb).clone()),
        StartKept::Answered(outcome) => Again::Answer(outcome.clone()),
    }))
}

/// Keep what a keyed start forwarded for `sent`, for a repeat of it.
fn keep_start(state: &mut State, key: IdempotencyKey, sent: blake3::Hash, forwarded: &Verb) {
    if state.start_keys.len() >= KEYS_REMEMBERED {
        state.start_keys.pop_front();
    }
    let (at, wall) = (tokio::time::Instant::now(), WallMs::now());
    let first = StartKept::Forwarded(Box::new(forwarded.clone()));
    let kept = start_kept(&key, sent, &first, wall);
    state.start_keys.push_back(KeyedStart { key, digest: sent, first, at, wall });
    projects::keep(state, Keep::Key(Box::new(kept)));
}

/// A keyed start as the store keeps it.
fn start_kept(key: &IdempotencyKey, sent: blake3::Hash, first: &StartKept, at: WallMs) -> KeptKey {
    KeptKey { key: key.clone(), digest: *sent.as_bytes(), first: First::Start(first.clone()), at }
}

/// Keep the answer a keyed start was given, in place of the start, when it is sure: an
/// answer that may have been lost on the way leaves the start to forward again.
fn start_answered(state: &mut State, key: &IdempotencyKey, outcome: &Outcome) {
    if maybe_lost(outcome) {
        return;
    }
    if let Some(held) = state.start_keys.iter_mut().find(|k| k.key == *key) {
        held.first = StartKept::Answered(outcome.clone());
        let kept = start_kept(key, held.digest, &held.first, held.wall);
        projects::keep(state, Keep::Key(Box::new(kept)));
    }
}

/// An answer that says only that the verb may or may not have been done.
const fn maybe_lost(outcome: &Outcome) -> bool {
    matches!(
        outcome,
        Outcome::Error { code: ErrorCode::Interrupted | ErrorCode::WorkerUnreachable, .. }
    )
}

/// The terminal `session` names, on whichever worker runs it.
fn term_of(state: &State, session: SessionId) -> Option<TermRef> {
    state
        .workers
        .values()
        .find(|e| e.sessions.iter().any(|s| s.id == session))
        .map(|e| TermRef { worker: e.info.worker, session })
        // A task's thread with no terminal of its own, by the seat it was started at.
        .or_else(|| state.board.seat(session))
        .or_else(|| state.projects.thread_seat(session))
}

/// `term`, if given, is a terminal the server knows.
fn known_term(state: &State, term: Option<TermRef>) -> Result<(), Outcome> {
    let Some(term) = term else { return Ok(()) };
    let Some(entry) = state.workers.get(&term.worker) else {
        return Err(unknown_worker(term.worker));
    };
    if entry.sessions.iter().any(|s| s.id == term.session) {
        Ok(())
    } else {
        Err(Outcome::Error {
            code: ErrorCode::UnknownTerminal,
            message: format!("no terminal {} on worker {}", term.session, entry.info.name),
        })
    }
}

/// What the status line of the agent in `term` said of its branch.
fn branch_of(state: &State, term: TermRef) -> Option<AgentBranch> {
    state.workers.get(&term.worker)?.branches.get(&term.session).cloned()
}

/// A forwarded request's place on its link, given up when the forward ends.
struct Pending<'h> {
    hub: &'h Hub,
    worker: WorkerId,
    generation: u64,
    id: RequestId,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.hub.forget(self.worker, self.generation, self.id);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.hub.lost(self.worker, self.generation);
    }
}

/// The worker a forwarded verb goes to.
const fn target(verb: &Verb) -> Option<WorkerId> {
    match verb {
        Verb::ListWorkers
        | Verb::ListTerminals { .. }
        | Verb::Events { .. }
        | Verb::ForgetWorker { .. }
        | Verb::Wake { .. }
        | Verb::ProjectCreate { .. }
        | Verb::ProjectSet { .. }
        | Verb::ProjectList
        | Verb::ProjectStatus { .. }
        | Verb::TaskCreate { .. }
        | Verb::TaskUpdate { .. }
        | Verb::TaskSpawn { .. }
        | Verb::TaskRestart { .. }
        | Verb::TaskTell { .. }
        | Verb::WorkerFacts { .. }
        | Verb::TaskGet { .. }
        | Verb::WorkingOn { .. }
        | Verb::TaskReport { .. }
        | Verb::TaskMerge { .. }
        | Verb::TaskPush { .. }
        | Verb::ProjectDelete { .. } => None,
        Verb::OpenTerminal { worker, .. }
        | Verb::SpawnAgent { worker, .. }
        | Verb::ReadFile { worker, .. }
        | Verb::ListDir { worker, .. }
        | Verb::Stat { worker, .. }
        | Verb::FsChange { worker, .. }
        | Verb::Search { worker, .. }
        | Verb::ListPorts { worker }
        | Verb::ListItems { worker }
        | Verb::OpenItem { worker, .. }
        | Verb::ListWindows { worker }
        | Verb::CaptureStill { worker, .. }
        | Verb::Upload { worker, .. }
        | Verb::WakePeer { worker, .. }
        | Verb::CloneRepo { worker, .. }
        | Verb::BundleBranch { worker, .. }
        | Verb::FetchBundle { worker, .. }
        | Verb::Verify { worker, .. }
        | Verb::Rebase { worker, .. }
        | Verb::TestDiff { worker, .. }
        | Verb::FastForward { worker, .. }
        | Verb::CatchUp { worker, .. }
        | Verb::LandPull { worker, .. }
        | Verb::RemoveWorktree { worker, .. }
        | Verb::DropBranches { worker, .. }
        | Verb::StartThread { worker, .. } => Some(*worker),
        Verb::Settings { of, .. } => *of,
        Verb::RenameItem { item, .. } | Verb::RemoveItem { item } => Some(item.worker),
        Verb::SendInput { term, .. }
        | Verb::ReadScreen { term }
        | Verb::ReadOutput { term, .. }
        | Verb::ListCommands { term, .. }
        | Verb::WaitFor { term, .. }
        | Verb::AgentStatus { term }
        | Verb::ResizeTerminal { term, .. }
        | Verb::Close { term } => Some(term.worker),
        Verb::ReadThread { of, .. } | Verb::AnswerRequest { of, .. } => match of {
            ThreadOf::On { worker, .. } => Some(*worker),
            // The server finds the worker first ([`Hub::thread_on`]).
            ThreadOf::Task { .. } | ThreadOf::Term(_) | ThreadOf::Thread(_) => None,
        },
    }
}

/// Every worker, by name.
fn listing(state: &State) -> Vec<WorkerInfo> {
    let mut out: Vec<WorkerInfo> = state.workers.values().map(|e| e.info.clone()).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.worker.cmp(&b.worker)));
    out
}

/// Whether two infos persist the same: name, address and capabilities.
fn same_shape(a: &WorkerInfo, b: &WorkerInfo) -> bool {
    a.worker == b.worker && a.name == b.name && a.address == b.address && a.caps == b.caps
}

fn error(code: ErrorCode, message: &str) -> Outcome {
    Outcome::Error { code, message: message.to_owned() }
}

fn unknown_worker(worker: WorkerId) -> Outcome {
    Outcome::Error {
        code: ErrorCode::UnknownWorker,
        message: format!("no worker {worker}; `slopty workers` names the known ones"),
    }
}

fn unreachable(info: &WorkerInfo) -> Outcome {
    Outcome::Error {
        code: ErrorCode::WorkerUnreachable,
        message: format!("worker {} ({}) is {:?}", info.name, info.worker, info.liveness),
    }
}

/// Where this run's event sequence starts: microseconds since the Unix epoch. A run logs far
/// fewer than one event a microsecond, so each run numbers above everything an earlier one did,
/// and a cursor kept across a restart is never mistaken for one of this run.
fn run_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1))
}

#[cfg(test)]
mod project_tests;

#[cfg(test)]
mod outcome_tests;

#[cfg(test)]
mod thread_tests;

#[cfg(test)]
pub(crate) mod tests {
    use slopty_proto::orchestration::{Screen, TermRef};
    use slopty_proto::server::{Os, WorkerCaps};
    use slopty_proto::terminal::CloseReason;
    use slopty_proto::thread::{Phase, ThreadId};

    use super::*;

    pub(crate) fn caps() -> WorkerCaps {
        WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            form: slopty_proto::server::Form::Desktop,
            cpus: 12,
            memory: 32 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            virtual_displays: false,
            curtain: false,
            version: "0.1.0".to_owned(),
            lan: Vec::new(),
            wake_on_lan: None,
            writes_failing: None,
            stops_at_logout: None,
        }
    }

    pub(crate) fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: "zsh".to_owned(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            progress: None,
            restored: None,
            program: Vec::new(),
            repo_id: None,
        }
    }

    pub(crate) fn registration(worker: WorkerId, sessions: Vec<SessionSummary>) -> Registration {
        let listen = SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45550));
        Registration {
            worker,
            name: "studio".to_owned(),
            listen,
            caps: caps(),
            sessions,
            session_key: [7; 32],
        }
    }

    fn ip() -> IpAddr {
        IpAddr::from([100, 64, 0, 7])
    }

    fn liveness(hub: &Hub, worker: WorkerId) -> Liveness {
        hub.directory().iter().find(|w| w.worker == worker).unwrap().liveness
    }

    fn screen() -> Screen {
        Screen {
            lines: Vec::new(),
            cursor: (0, 0),
            title: String::new(),
            cwd: None,
            alternate: false,
        }
    }

    /// The next message of the directory, past the events.
    async fn next_listing(rx: &mut broadcast::Receiver<FromServer>) -> FromServer {
        loop {
            match rx.recv().await.unwrap() {
                FromServer::Event(_) => {}
                other => return other,
            }
        }
    }

    /// The next terminal opened or closed, past everything else.
    async fn next_session(rx: &mut broadcast::Receiver<FromServer>) -> Happening {
        loop {
            if let FromServer::Event(HubEvent {
                what: what @ (Happening::SessionOpened { .. } | Happening::SessionClosed { .. }),
                ..
            }) = rx.recv().await.unwrap()
            {
                return what;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_lease_goes_unreachable_then_gone_and_comes_back_online() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let mut events = hub.subscribe();
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let info = &hub.directory()[0];
        assert_eq!((info.liveness, info.address.as_str()), (Liveness::Online, "100.64.0.7:45550"));
        assert!(
            matches!(next_listing(&mut events).await, FromServer::Worker(w) if w.liveness == Liveness::Online)
        );

        drop(lease);
        assert_eq!(liveness(&hub, worker), Liveness::Unreachable, "at once when the link ends");
        assert!(
            matches!(next_listing(&mut events).await, FromServer::Worker(w) if w.liveness == Liveness::Unreachable)
        );
        tokio::time::sleep(GONE_AFTER.checked_sub(Duration::from_millis(100)).unwrap()).await;
        assert_eq!(liveness(&hub, worker), Liveness::Unreachable, "not gone before 20 s");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(liveness(&hub, worker), Liveness::Gone);
        assert!(
            matches!(next_listing(&mut events).await, FromServer::Worker(w) if w.liveness == Liveness::Gone)
        );

        let (tx, _rx) = mpsc::channel(8);
        let _back = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        assert_eq!(liveness(&hub, worker), Liveness::Online, "the same id reconnects");
        assert_eq!(hub.directory().len(), 1, "one entry per id");
    }

    #[tokio::test(start_paused = true)]
    async fn a_reconnect_inside_the_grace_keeps_the_worker_from_going_gone() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        drop(hub.register(registration(worker, Vec::new()), ip(), tx).unwrap());
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (tx, _rx) = mpsc::channel(8);
        let _lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        tokio::time::sleep(GONE_AFTER).await;
        assert_eq!(liveness(&hub, worker), Liveness::Online, "the old timer is stale");
    }

    #[tokio::test]
    async fn a_second_live_link_with_the_same_id_is_refused() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let first = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let (tx, _rx2) = mpsc::channel(8);
        let err = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap_err();
        assert_eq!(err, Refusal::DuplicateWorker);
        assert_eq!(liveness(&hub, worker), Liveness::Online, "the first keeps its lease");
        drop(first);
        let (tx, _rx3) = mpsc::channel(8);
        hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
    }

    #[tokio::test]
    async fn verbs_route_to_the_worker_and_fail_readably_when_they_cannot() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let term = TermRef { worker, session: SessionId::new() };

        let nobody = hub.dispatch(Verb::ReadScreen { term }).await;
        assert!(
            matches!(nobody, Outcome::Error { code: ErrorCode::UnknownWorker, .. }),
            "{nobody:?}"
        );

        let (tx, mut rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let asked = tokio::spawn({
            let hub = hub.clone();
            async move { hub.dispatch(Verb::ReadScreen { term }).await }
        });
        let Some(FromServer::Request { id, verb, .. }) = rx.recv().await else {
            panic!("no request")
        };
        assert_eq!(verb, Verb::ReadScreen { term });
        lease.handle(ToServer::Reply { id, outcome: Outcome::Screen(screen()) });
        assert_eq!(asked.await.unwrap(), Outcome::Screen(screen()));

        // Dropped mid-request: the waiter hears at once that it may have been done, and the key
        // went down with the verb.
        let key = IdempotencyKey::new("k1").unwrap();
        let asked = tokio::spawn({
            let (hub, key) = (hub.clone(), key.clone());
            async move { hub.dispatch_keyed(Some(key), Verb::ReadScreen { term }).await }
        });
        let Some(FromServer::Request { key: sent, .. }) = rx.recv().await else {
            panic!("no request")
        };
        assert_eq!(sent, Some(key));
        drop(lease);
        let dropped = asked.await.unwrap();
        assert!(
            matches!(dropped, Outcome::Error { code: ErrorCode::Interrupted, .. }),
            "{dropped:?}"
        );

        let offline = hub.dispatch(Verb::Close { term }).await;
        assert!(
            matches!(offline, Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }),
            "{offline:?}"
        );
    }

    /// The verbs that reach another agent, the screen and a file in parts go to the worker
    /// they name, the key with the ones that change something.
    #[tokio::test]
    async fn the_agent_screen_and_upload_verbs_go_to_their_worker() {
        use slopty_proto::orchestration::{ThreadView, UploadPart};
        use slopty_proto::screen::CaptureTarget;

        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, mut rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let upload = slopty_core::XferId::new();
        let key = IdempotencyKey::new("once").unwrap();
        let verbs = [
            Verb::ReadThread {
                of: ThreadOf::On { worker, thread: ThreadId::new() },
                view: ThreadView::Activity,
                after: None,
                hold: true,
            },
            Verb::AnswerRequest {
                of: ThreadOf::On { worker, thread: ThreadId::new() },
                ask: slopty_proto::thread::AskId("3".to_owned()),
                choice: "allow".to_owned(),
                message: None,
            },
            Verb::CaptureStill {
                worker,
                target: CaptureTarget::Display(slopty_core::DisplayId(1)),
            },
            Verb::Upload {
                worker,
                path: "/w/a".to_owned(),
                upload,
                part: UploadPart::Bytes { offset: 0, bytes: vec![1] },
            },
        ];
        for verb in verbs {
            let asked = tokio::spawn({
                let (hub, verb, key) = (hub.clone(), verb.clone(), key.clone());
                async move { hub.dispatch_keyed(Some(key), verb).await }
            });
            let Some(FromServer::Request { id, verb: sent, key: sent_key }) = rx.recv().await
            else {
                panic!("no request")
            };
            assert_eq!(sent, verb);
            assert_eq!(sent_key, Some(key.clone()), "the worker decides what the key guards");
            lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
            assert_eq!(asked.await.unwrap(), Outcome::Done);
        }
    }

    /// A caller that gives up on a forwarded verb (an MCP client that went away) takes its
    /// request off the link, so a lease that lives for weeks does not collect them.
    #[tokio::test]
    async fn a_forward_its_caller_dropped_leaves_nothing_pending() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, mut rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let term = TermRef { worker, session: SessionId::new() };
        let asked = tokio::spawn({
            let hub = hub.clone();
            async move { hub.dispatch(Verb::ReadScreen { term }).await }
        });
        let Some(FromServer::Request { id, .. }) = rx.recv().await else { panic!("no request") };
        assert_eq!(hub.pending(worker), 1);
        asked.abort();
        assert!(asked.await.unwrap_err().is_cancelled());
        assert_eq!(hub.pending(worker), 0, "the dropped call took its request with it");
        // The worker's late answer is to nothing, and harmless.
        lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
        assert_eq!(hub.pending(worker), 0);
    }

    /// A worker whose link stays up and takes nothing holds a forwarded verb no longer than
    /// its deadline: the send waits inside it, and the request is said not to have gone.
    #[tokio::test(start_paused = true)]
    async fn a_link_that_does_not_drain_holds_a_forward_no_longer_than_its_deadline() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(FromServer::Directory(Vec::new())).unwrap();
        let _lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let term = TermRef { worker, session: SessionId::new() };
        let started = tokio::time::Instant::now();
        let asked = hub.dispatch(Verb::ReadScreen { term }).await;
        assert_eq!(started.elapsed(), FORWARD_TIMEOUT);
        let Outcome::Error { code, message } = asked else { panic!("{asked:?}") };
        assert_eq!(code, ErrorCode::WorkerUnreachable);
        assert!(message.contains("not sent"), "{message}");
        assert_eq!(hub.pending(worker), 0, "nothing left waiting on the link");
        assert!(matches!(rx.try_recv(), Ok(FromServer::Directory(_))));
        assert!(rx.try_recv().is_err(), "the request never went");
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_is_capped_below_the_mcp_idle_abort() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, mut rx) = mpsc::channel(8);
        let _lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let term = TermRef { worker, session: SessionId::new() };
        let until = slopty_proto::orchestration::WaitUntil::Exit;
        let asked = tokio::spawn({
            let hub = hub.clone();
            async move { hub.dispatch(Verb::WaitFor { term, until, timeout_ms: 900_000 }).await }
        });
        let Some(FromServer::Request { verb, .. }) = rx.recv().await else { panic!("no request") };
        assert!(matches!(verb, Verb::WaitFor { timeout_ms: WAIT_CAP_MS, .. }), "{verb:?}");
        tokio::time::sleep(Duration::from_secs(250)).await;
        assert!(!asked.is_finished(), "the forward outlives the wait it carries");
        tokio::time::sleep(Duration::from_secs(10)).await;
        let late = asked.await.unwrap();
        assert!(matches!(late, Outcome::Error { code: ErrorCode::Interrupted, .. }), "{late:?}");
    }

    #[tokio::test]
    async fn sessions_and_agents_are_tracked_and_fanned_out() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (first, second) = (SessionId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(first)]), ip(), tx).unwrap();
        let mut events = hub.subscribe();
        lease.handle(ToServer::SessionChanged(summary(second)));
        lease.handle(ToServer::SessionClosed { session: first, reason: CloseReason::Exited });
        let Outcome::Terminals { terminals: list, .. } =
            hub.dispatch(Verb::ListTerminals { worker: None }).await
        else {
            panic!("terminals")
        };
        assert_eq!(list, vec![(worker, summary(second))]);
        assert!(
            matches!(next_session(&mut events).await, Happening::SessionOpened { summary, .. } if summary.id == second)
        );
        assert!(
            matches!(next_session(&mut events).await, Happening::SessionClosed { term } if term.session == first)
        );
        let elsewhere = hub.dispatch(Verb::ListTerminals { worker: Some(WorkerId::new()) }).await;
        assert!(matches!(elsewhere, Outcome::Error { code: ErrorCode::UnknownWorker, .. }));

        // A re-registration reports the difference.
        drop(lease);
        let (tx, _rx) = mpsc::channel(8);
        let _lease = hub.register(registration(worker, vec![summary(first)]), ip(), tx).unwrap();
        assert!(
            matches!(next_session(&mut events).await, Happening::SessionClosed { term } if term.session == second)
        );
        assert!(
            matches!(next_session(&mut events).await, Happening::SessionOpened { summary, .. } if summary.id == first)
        );
    }

    /// `ListTerminals` answers with each terminal's agent as its thread's row says, kept
    /// current; an agent that ended leaves its terminal without one, and a thread in a terminal
    /// the worker does not list adds nothing.
    #[tokio::test]
    async fn listed_terminals_carry_the_agent_as_its_row_says() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(session)]), ip(), tx).unwrap();
        let listed = async || {
            let Outcome::Terminals { terminals, agents } =
                hub.dispatch(Verb::ListTerminals { worker: None }).await
            else {
                panic!("terminals")
            };
            assert_eq!(terminals.len(), 1, "the one terminal");
            agents.into_iter().map(|(term, a)| (term.session, a.phase, a.asks)).collect::<Vec<_>>()
        };
        assert_eq!(listed().await, [], "no agent yet");
        let thread = ThreadId::new();
        lease.handle(agent_report(thread, session, Phase::Working));
        assert_eq!(listed().await, [(session, Phase::Working, None)]);
        lease.handle(agent_report(thread, session, Phase::NeedsYou));
        let asks = Some("Which branch?".to_owned());
        assert_eq!(listed().await, [(session, Phase::NeedsYou, asks)], "kept current");
        let mut ended = ladder::tests::row(Phase::Idle, 2, Some(session));
        ended.id = thread;
        ended.status.liveness = slopty_proto::thread::Liveness::Exited { resumable: true };
        lease.handle(ladder::tests::snapshot(vec![ended]));
        assert_eq!(listed().await, [], "the agent ended");
        lease.handle(agent_report(ThreadId::new(), SessionId::new(), Phase::Idle));
        assert_eq!(listed().await, [], "a thread in a terminal not listed adds nothing");
    }

    #[tokio::test]
    async fn only_a_change_of_shape_is_persisted() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let mut persisted = hub.persisted();
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        assert!(persisted.has_changed().unwrap(), "a new worker");
        assert_eq!(persisted.borrow_and_update().len(), 1);

        lease.handle(ToServer::Load(3.0));
        assert!(!persisted.has_changed().unwrap(), "a load tick is not a change of shape");
        assert!((hub.directory()[0].load - 3.0).abs() < f32::EPSILON, "but it is live");
        lease.handle(ToServer::Caps(caps()));
        assert!(!persisted.has_changed().unwrap(), "nor are the same capabilities again");

        lease.handle(ToServer::Caps(WorkerCaps { can_capture: false, ..caps() }));
        assert!(persisted.has_changed().unwrap(), "a permission change is");
        drop(persisted.borrow_and_update());
        drop(lease);
        assert!(persisted.has_changed().unwrap(), "so is the end of a lease (its last-seen time)");
    }

    #[test]
    fn known_workers_start_gone() {
        let info = WorkerInfo {
            worker: WorkerId::new(),
            name: "old".to_owned(),
            address: "100.64.0.9:45550".to_owned(),
            liveness: Liveness::Online,
            caps: caps(),
            load: 0.0,
            last_seen_ms: WallMs::from_millis(1),
        };
        let hub = Hub::new("server".to_owned(), vec![info.clone()]);
        assert_eq!(hub.directory(), vec![WorkerInfo { liveness: Liveness::Gone, ..info }]);
    }

    /// The worker's table with its one thread, `thread`, in `session` at `phase`.
    fn agent_report(thread: ThreadId, session: SessionId, phase: Phase) -> ToServer {
        table(&[(thread, session, phase)])
    }

    /// The worker's whole table: each thread in its session at its phase; one that needs the
    /// person asks which branch.
    pub(crate) fn table(threads: &[(ThreadId, SessionId, Phase)]) -> ToServer {
        let rows = threads.iter().map(|&(thread, session, phase)| {
            let mut row = ladder::tests::row(phase, 1, Some(session));
            row.id = thread;
            if phase == Phase::NeedsYou {
                row = ladder::tests::asking(row, "Which branch?");
            }
            row
        });
        ladder::tests::snapshot(rows.collect())
    }

    /// Whether `what` is the rung of the agent in `term`, at `phase`.
    fn rung_at(what: &Happening, term: TermRef, phase: Phase) -> bool {
        matches!(what, Happening::Rung { worker, terminal, agent }
            if *worker == term.worker && *terminal == Some(term.session) && agent.phase == phase)
    }

    async fn events(hub: &Hub, since: Option<u64>, timeout_ms: u32) -> (Vec<HubEvent>, u64, u64) {
        let filter = EventFilter::All;
        match hub.dispatch(Verb::Events { since, timeout_ms, filter }).await {
            Outcome::Events { events, next, missed } => (events, next, missed),
            other => panic!("{other:?}"),
        }
    }

    /// Events read from a cursor: what happened after it, the cursor to go on from, a wait for
    /// the next one that wakes when it comes and gives up at its timeout, and a filter that
    /// skips what it does not want while the cursor still moves past it.
    #[tokio::test(start_paused = true)]
    async fn events_are_read_from_a_cursor_and_waited_for() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let (none, now, missed) = events(&hub, None, 0).await;
        assert_eq!((none.len(), missed), (0, 0), "nothing yet");

        let (worker, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        lease.handle(ToServer::SessionChanged(summary(session)));
        let (seen, next, _) = events(&hub, Some(now), 0).await;
        let whats: Vec<&Happening> = seen.iter().map(|e| &e.what).collect();
        assert!(
            matches!(whats[..], [
                Happening::Worker { liveness: Liveness::Online, .. },
                Happening::SessionOpened { summary, .. },
            ] if summary.id == session),
            "{whats:?}"
        );
        assert_eq!((seen[0].seq, seen[1].seq, next), (now, now + 1, now + 2));
        lease.handle(ToServer::SessionChanged(summary(session)));
        assert!(events(&hub, Some(next), 0).await.0.is_empty(), "a known session's update");
        let exited =
            SessionSummary { state: SessionState::Exited { status: 2 }, ..summary(session) };
        lease.handle(ToServer::SessionChanged(exited.clone()));
        let (seen, next, _) = events(&hub, Some(next), 0).await;
        assert!(
            matches!(seen.as_slice(), [HubEvent { what: Happening::SessionExited { term, status: 2 }, .. }]
                if term.session == session),
            "the program's exit is an event: {seen:?}"
        );
        lease.handle(ToServer::SessionChanged(exited));
        assert!(events(&hub, Some(next), 0).await.0.is_empty(), "and only once");

        // A wait wakes for the next event, however long before its timeout it comes.
        let waiting = tokio::spawn({
            let hub = hub.clone();
            async move { events(&hub, None, 60_000).await }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiting.is_finished(), "nothing new yet");
        let thread = ThreadId::new();
        lease.handle(agent_report(thread, session, Phase::NeedsYou));
        let (woke, after, _) = waiting.await.unwrap();
        let term = TermRef { worker, session };
        assert!(
            matches!(woke.as_slice(), [HubEvent { what, .. }] if rung_at(what, term, Phase::NeedsYou)),
            "{woke:?}"
        );
        lease.handle(agent_report(thread, session, Phase::NeedsYou));
        let started = tokio::time::Instant::now();
        let (none, same, _) = events(&hub, Some(after), 1_000).await;
        assert!(none.is_empty(), "the same status again is no event");
        assert_eq!(same, after, "the cursor stays");
        assert_eq!(started.elapsed(), Duration::from_secs(1), "gave up at the timeout");

        // Filtered: only an agent that needs a human or went idle, the cursor past the rest.
        lease.handle(agent_report(thread, session, Phase::Working));
        lease.handle(ToServer::SessionClosed {
            session: SessionId::new(),
            reason: CloseReason::Exited,
        });
        lease.handle(agent_report(thread, session, Phase::Idle));
        let filter = EventFilter::AgentNeedsInput;
        let asked = Verb::Events { since: Some(after), timeout_ms: 0, filter };
        let Outcome::Events { events: needs, next, .. } = hub.dispatch(asked).await else {
            panic!("events")
        };
        assert!(
            matches!(needs.as_slice(), [HubEvent { what, .. }] if rung_at(what, term, Phase::Idle)),
            "{needs:?}"
        );
        assert_eq!(next, after + 3);

        drop(lease);
        let (gone, ..) = events(&hub, Some(next), 0).await;
        assert!(
            matches!(
                gone.as_slice(),
                [HubEvent { what: Happening::Worker { liveness: Liveness::Unreachable, .. }, .. }]
            ),
            "{gone:?}"
        );
    }

    /// The log keeps the newest [`EVENT_LOG`]: an older cursor reads on from the oldest held and
    /// is told how many it missed, one answer holds [`EVENTS_PER_ANSWER`], and a cursor from
    /// before a restart reads from the oldest.
    #[tokio::test]
    async fn the_event_log_is_bounded_and_says_what_was_missed() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let (_, first, _) = events(&hub, None, 0).await;
        let (worker, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(session)]), ip(), tx).unwrap();
        let extra = 10;
        let thread = ThreadId::new();
        for i in 0..EVENT_LOG + extra {
            let phase = if i % 2 == 0 { Phase::Working } else { Phase::Idle };
            lease.handle(agent_report(thread, session, phase));
        }
        // The worker coming online and its terminal opening, then the agent's flips.
        let logged = u64::try_from(EVENT_LOG + extra + 2).unwrap();
        let (page, next, missed) = events(&hub, Some(first), 0).await;
        let oldest = first + logged - u64::try_from(EVENT_LOG).unwrap();
        assert_eq!(missed, oldest - first);
        assert_eq!(page.len(), EVENTS_PER_ANSWER);
        assert_eq!(page[0].seq, oldest);
        assert_eq!(next, oldest + u64::try_from(EVENTS_PER_ANSWER).unwrap());
        let (_page, _next, missed) = events(&hub, Some(next), 0).await;
        assert_eq!(missed, 0, "a cursor inside the log missed nothing");
        let (ahead, _next, missed) = events(&hub, Some(first + logged + 1_000), 0).await;
        assert_eq!(ahead[0].seq, oldest, "a cursor from another run reads from the oldest");
        assert_eq!(missed, oldest - first + 1);
    }

    /// A cursor kept across a restart, below where the new run's sequence starts, reads the
    /// new run's events from its first and is told it missed some; it never lands inside the
    /// new run's range and skips its first events unannounced.
    #[tokio::test]
    async fn a_cursor_from_before_a_restart_misses_nothing_unannounced() {
        let flips = |hub: &Hub, n: usize| {
            let (worker, session) = (WorkerId::new(), SessionId::new());
            let (tx, _rx) = mpsc::channel(8);
            let lease =
                hub.register(registration(worker, vec![summary(session)]), ip(), tx).unwrap();
            let thread = ThreadId::new();
            for i in 0..n {
                let phase = if i % 2 == 0 { Phase::Working } else { Phase::Idle };
                lease.handle(agent_report(thread, session, phase));
            }
            lease
        };
        let before = Hub::new("server".to_owned(), Vec::new());
        let _lease = flips(&before, 28);
        let (_, kept, _) = events(&before, None, 0).await;
        drop(before);
        // A restart takes far longer than this; the sequence starts from the wall clock.
        tokio::time::sleep(Duration::from_millis(2)).await;

        let after = Hub::new("server".to_owned(), Vec::new());
        let (_, first, _) = events(&after, None, 0).await;
        let _lease = flips(&after, 38);
        let (seen, _next, missed) = events(&after, Some(kept), 0).await;
        assert_eq!(seen.first().map(|e| e.seq), Some(first), "from the new run's first");
        assert_eq!(seen.len(), 40, "every event of the new run");
        assert!(missed > 0, "told it missed what the old run logged after the cursor");
    }

    /// A long wait that sees the log wrap past its cursor while it waits reports the events
    /// dropped, not only what was missing at its first look.
    #[tokio::test(start_paused = true)]
    async fn a_long_wait_counts_what_the_log_dropped_while_it_waited() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let (worker, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(session)]), ip(), tx).unwrap();
        let (_, from, _) = events(&hub, None, 0).await;
        let waiting = tokio::spawn({
            let hub = hub.clone();
            let filter = EventFilter::AgentNeedsInput;
            async move {
                hub.dispatch(Verb::Events { since: Some(from), timeout_ms: 60_000, filter }).await
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!waiting.is_finished(), "nothing it wants yet");
        // Past the log's size in one go, none of it what the wait wants, then one it does.
        let extra = 10;
        let thread = ThreadId::new();
        for i in 0..EVENT_LOG + extra {
            let phase = if i % 2 == 0 { Phase::Working } else { Phase::Waiting };
            lease.handle(agent_report(thread, session, phase));
        }
        lease.handle(agent_report(thread, session, Phase::Idle));
        let Outcome::Events { events, missed, .. } = waiting.await.unwrap() else {
            panic!("events")
        };
        let term = TermRef { worker, session };
        assert!(
            matches!(events.as_slice(), [HubEvent { what, .. }] if rung_at(what, term, Phase::Idle)),
            "{events:?}"
        );
        assert_eq!(missed, u64::try_from(extra + 1).unwrap(), "the events dropped mid-wait");
    }

    /// A worker removed, forgotten or replaced by its reinstall, takes its terminals with it:
    /// every link hears each one closed, and so does the event log, so no agent badge outlives
    /// the entry.
    #[tokio::test]
    async fn a_removed_worker_closes_its_sessions_everywhere() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let closed = |links: &mut broadcast::Receiver<FromServer>| {
            let mut closed = Vec::new();
            while let Ok(msg) = links.try_recv() {
                if let FromServer::Event(HubEvent {
                    what: Happening::SessionClosed { term }, ..
                }) = msg
                {
                    closed.push(term);
                }
            }
            closed
        };
        let logged = async |from| {
            let (heard, ..) = events(&hub, Some(from), 0).await;
            heard
                .into_iter()
                .filter_map(|e| match e.what {
                    Happening::SessionClosed { term } => Some(term),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let (forgotten, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease =
            hub.register(registration(forgotten, vec![summary(session)]), ip(), tx).unwrap();
        drop(lease);
        let mut links = hub.subscribe();
        let (_, from, _) = events(&hub, None, 0).await;
        assert_eq!(hub.dispatch(Verb::ForgetWorker { worker: forgotten }).await, Outcome::Done);
        let term = TermRef { worker: forgotten, session };
        assert_eq!(closed(&mut links), [term], "forgotten");
        assert_eq!(logged(from).await, [term]);

        let (old, session) = (WorkerId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        drop(hub.register(registration(old, vec![summary(session)]), ip(), tx).unwrap());
        drop(closed(&mut links));
        let (_, from, _) = events(&hub, None, 0).await;
        let (tx, _rx) = mpsc::channel(8);
        let _reinstalled =
            hub.register(registration(WorkerId::new(), Vec::new()), ip(), tx).unwrap();
        let term = TermRef { worker: old, session };
        assert_eq!(closed(&mut links), [term], "replaced by its reinstall");
        assert_eq!(logged(from).await, [term]);
    }

    /// A worker without a lease is forgotten: gone from the directory, the state file and every
    /// link's listing, with an event. A live one is refused, an unknown one is unknown.
    #[tokio::test]
    async fn a_worker_is_forgotten_only_when_it_is_not_online() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let mut persisted = hub.persisted();
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, Vec::new()), ip(), tx).unwrap();
        let online = hub.dispatch(Verb::ForgetWorker { worker }).await;
        let Outcome::Error { code: ErrorCode::Invalid, message } = online else {
            panic!("{online:?}")
        };
        assert!(message.contains("online"), "{message}");
        assert_eq!(hub.directory().len(), 1, "still listed");

        drop(lease);
        drop(persisted.borrow_and_update());
        let mut links = hub.subscribe();
        let (_, from, _) = events(&hub, None, 0).await;
        assert_eq!(hub.dispatch(Verb::ForgetWorker { worker }).await, Outcome::Done);
        assert_eq!(hub.directory(), Vec::<WorkerInfo>::new());
        assert!(persisted.borrow_and_update().is_empty(), "the state file loses it");
        assert!(
            matches!(next_listing(&mut links).await, FromServer::Directory(list) if list.is_empty())
        );
        let (heard, ..) = events(&hub, Some(from), 0).await;
        assert!(
            matches!(heard.as_slice(), [HubEvent { what: Happening::WorkerRemoved { worker: w, .. }, .. }] if *w == worker),
            "{heard:?}"
        );
        let again = hub.dispatch(Verb::ForgetWorker { worker }).await;
        assert!(
            matches!(again, Outcome::Error { code: ErrorCode::UnknownWorker, .. }),
            "{again:?}"
        );
    }

    /// A forget repeated under its key answers as the first did, though the worker is no longer
    /// listed; the key on another worker is refused, and it lapses with its lifetime.
    #[tokio::test(start_paused = true)]
    async fn a_keyed_forget_repeats_its_answer() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        drop(hub.register(registration(worker, Vec::new()), ip(), tx).unwrap());
        let key = Some(IdempotencyKey::new("forget-1").unwrap());
        let forget = Verb::ForgetWorker { worker };
        assert_eq!(hub.dispatch_keyed(key.clone(), forget.clone()).await, Outcome::Done);
        assert_eq!(hub.dispatch_keyed(key.clone(), forget.clone()).await, Outcome::Done);
        let other = Verb::ForgetWorker { worker: WorkerId::new() };
        let reused = hub.dispatch_keyed(key.clone(), other).await;
        assert!(matches!(reused, Outcome::Error { code: ErrorCode::Invalid, .. }), "{reused:?}");
        tokio::time::advance(KEY_LIFETIME).await;
        let lapsed = hub.dispatch_keyed(key, forget).await;
        assert!(
            matches!(lapsed, Outcome::Error { code: ErrorCode::UnknownWorker, .. }),
            "{lapsed:?}"
        );
    }

    /// A worker set up again registers a new id under its old name from its old address: the
    /// old entry, known from the last run, is dropped and every link hears the directory
    /// without it. A namesake elsewhere, and an entry still on a live link, stay.
    #[tokio::test]
    async fn a_worker_set_up_again_replaces_its_old_entry() {
        let known = |name: &str, address: &str| WorkerInfo {
            worker: WorkerId::new(),
            name: name.to_owned(),
            address: address.to_owned(),
            liveness: Liveness::Online,
            caps: caps(),
            load: 0.0,
            last_seen_ms: WallMs::from_millis(1),
        };
        let old = known("studio", "100.64.0.7:45550");
        let namesake = known("studio", "100.64.0.8:45550");
        let hub = Hub::new("server".to_owned(), vec![old.clone(), namesake.clone()]);
        let mut persisted = hub.persisted();
        let mut events = hub.subscribe();

        let first = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let _first = hub.register(registration(first, Vec::new()), ip(), tx).unwrap();
        let listed: Vec<WorkerId> = hub.directory().iter().map(|w| w.worker).collect();
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed.contains(&first) && listed.contains(&namesake.worker), "{listed:?}");
        assert!(
            matches!(next_listing(&mut events).await, FromServer::Worker(w) if w.worker == first)
        );
        let FromServer::Directory(heard) = next_listing(&mut events).await else {
            panic!("the directory again, without the replaced entry")
        };
        assert_eq!(heard, hub.directory());
        assert!(!persisted.borrow_and_update().iter().any(|w| w.worker == old.worker));

        // Two live links on one address are two workers, whatever their names.
        let second = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let _second = hub.register(registration(second, Vec::new()), ip(), tx).unwrap();
        assert_eq!(hub.directory().len(), 3);
    }

    /// A LAN that records what the server would send, and sends nothing.
    #[derive(Debug, Default)]
    struct FakeLan {
        ports: Mutex<Vec<LanPort>>,
        sent: Mutex<Vec<(LanPort, Vec<MacAddr>)>>,
    }

    impl Lan for FakeLan {
        fn ports(&self) -> Vec<LanPort> {
            self.ports.lock().clone()
        }

        fn wake(&self, from: LanPort, macs: Vec<MacAddr>) -> WakeFuture {
            self.sent.lock().push((from, macs));
            Box::pin(std::future::ready(Ok(())))
        }
    }

    fn port(interface: &str, addr: [u8; 4], mac: u8) -> LanPort {
        LanPort {
            interface: interface.to_owned(),
            mac: MacAddr([0x3c, 0x22, 0xfb, 0, 0, mac]),
            addr: std::net::Ipv4Addr::from(addr),
            prefix: 24,
        }
    }

    /// Register a worker named `name` on `lan`, from its own tailnet address.
    fn on_lan(
        hub: &Hub,
        name: &str,
        lan: Vec<LanPort>,
        last: u8,
    ) -> (WorkerId, Lease, mpsc::Receiver<FromServer>) {
        let worker = WorkerId::new();
        let registration = Registration {
            name: name.to_owned(),
            caps: WorkerCaps { lan, ..caps() },
            ..registration(worker, Vec::new())
        };
        let (tx, rx) = mpsc::channel(8);
        let lease = hub.register(registration, IpAddr::from([100, 64, 0, last]), tx).unwrap();
        (worker, lease, rx)
    }

    /// With the server off the sleeping worker's subnet, the wake goes to the online worker on
    /// it, for the port it shares, and not to a worker elsewhere; its answer is who sent it.
    #[tokio::test]
    async fn a_wake_is_relayed_to_a_worker_on_the_sleepers_subnet() {
        let lan = Arc::new(FakeLan::default());
        *lan.ports.lock() = vec![port("en0", [10, 9, 9, 2], 1)];
        let hub = Hub::with_lan("server".to_owned(), Vec::new(), Arc::<FakeLan>::clone(&lan));
        let en0 = port("en0", [192, 168, 1, 20], 20);
        let far = port("en1", [172, 16, 0, 20], 21);
        let (sleeper, lease, _rx) = on_lan(&hub, "studio", vec![en0.clone(), far], 20);
        drop(lease);
        let (beside, beside_lease, mut beside_rx) =
            on_lan(&hub, "mini", vec![port("en0", [192, 168, 1, 30], 30)], 30);
        let (_, _elsewhere, mut elsewhere_rx) =
            on_lan(&hub, "laptop", vec![port("en0", [10, 0, 0, 5], 5)], 5);

        let asked = tokio::spawn({
            let hub = hub.clone();
            async move { hub.dispatch(Verb::Wake { worker: sleeper }).await }
        });
        let Some(FromServer::Request { id, verb, .. }) = beside_rx.recv().await else {
            panic!("no request")
        };
        assert_eq!(verb, Verb::WakePeer { worker: beside, peer: vec![en0] });
        beside_lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
        assert_eq!(
            asked.await.unwrap(),
            Outcome::WakeSent { by: "mini".to_owned(), to: vec!["en0".to_owned()] }
        );
        assert!(elsewhere_rx.try_recv().is_err(), "the worker off the subnet is not asked");
        assert!(lan.sent.lock().is_empty(), "the server is off the subnet");
    }

    /// A server on the sleeping worker's subnet sends the packet itself, from its port there,
    /// and asks no worker.
    #[tokio::test]
    async fn a_server_on_the_subnet_wakes_the_worker_itself() {
        let lan = Arc::new(FakeLan::default());
        let own = port("en0", [192, 168, 1, 2], 2);
        *lan.ports.lock() = vec![port("en9", [10, 9, 9, 2], 9), own.clone()];
        let hub = Hub::with_lan("server".to_owned(), Vec::new(), Arc::<FakeLan>::clone(&lan));
        let en0 = port("en0", [192, 168, 1, 20], 20);
        let (sleeper, lease, _rx) = on_lan(&hub, "studio", vec![en0.clone()], 20);
        drop(lease);
        let (_, _beside, mut beside_rx) =
            on_lan(&hub, "mini", vec![port("en0", [192, 168, 1, 30], 30)], 30);

        let outcome = hub.dispatch(Verb::Wake { worker: sleeper }).await;
        assert_eq!(
            outcome,
            Outcome::WakeSent { by: "server".to_owned(), to: vec!["en0".to_owned()] }
        );
        assert_eq!(*lan.sent.lock(), [(own, vec![en0.mac])]);
        assert!(beside_rx.try_recv().is_err(), "no worker is asked");
    }

    /// An online worker, one with no LAN port, one nothing shares a subnet with, and one the
    /// server does not know each fail with a reason, and nothing is sent.
    #[tokio::test]
    async fn a_wake_that_cannot_happen_says_why() {
        let lan = Arc::new(FakeLan::default());
        *lan.ports.lock() = vec![port("en0", [10, 9, 9, 2], 1)];
        let hub = Hub::with_lan("server".to_owned(), Vec::new(), Arc::<FakeLan>::clone(&lan));
        let (awake, _awake_lease, _rx) =
            on_lan(&hub, "mini", vec![port("en0", [192, 168, 1, 30], 30)], 30);
        let (bare, lease, _rx) = on_lan(&hub, "pi", Vec::new(), 40);
        drop(lease);
        let (alone, lease, _rx) =
            on_lan(&hub, "studio", vec![port("en0", [172, 16, 0, 20], 20)], 20);
        drop(lease);
        let code = |outcome: Outcome| match outcome {
            Outcome::Error { code, message } => {
                assert_ne!(message, "");
                code
            }
            other => panic!("{other:?}"),
        };
        assert_eq!(code(hub.dispatch(Verb::Wake { worker: awake }).await), ErrorCode::Invalid);
        assert_eq!(code(hub.dispatch(Verb::Wake { worker: bare }).await), ErrorCode::Unsupported);
        let alone = hub.dispatch(Verb::Wake { worker: alone }).await;
        assert!(
            matches!(&alone, Outcome::Error { code: ErrorCode::Failed, message } if message.contains("172.16.0.20/24")),
            "{alone:?}"
        );
        let nobody = hub.dispatch(Verb::Wake { worker: WorkerId::new() }).await;
        assert_eq!(code(nobody), ErrorCode::UnknownWorker);
        assert!(lan.sent.lock().is_empty());
    }
}
