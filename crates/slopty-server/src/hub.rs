//! The registry: every worker the server knows, the lease each live one holds, and the verbs
//! routed to them.
//!
//! A worker's connection is its lease ([`Lease`]): registering takes it, dropping it (the link
//! ended, cleanly or by the idle timeout) marks the worker [`Liveness::Unreachable`], and
//! [`GONE_AFTER`] later, with no reconnect, [`Liveness::Gone`]. Every change goes out to every
//! client and agent link as [`FromServer::Worker`] or [`FromServer::Event`].
//!
//! [`Hub::dispatch`] is the one verb dispatch both front ends call (QUIC links and MCP): the
//! directory verbs are answered here, the rest go down the owning worker's link.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_core::{SessionId, WorkerId};
use slopty_proto::agent::AgentEvent;
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::server::{
    Event, FromServer, Liveness, Refusal, Registration, RequestId, ToServer, WorkerCaps, WorkerInfo,
};
use slopty_proto::terminal::SessionSummary;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

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
/// Directory changes buffered per client link before it lags and gets the whole directory again.
const EVENT_BUFFER: usize = 1024;

/// The registry and the router, shared by every link and the MCP endpoint.
#[derive(Clone, Debug)]
pub struct Hub {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    name: String,
    state: Mutex<State>,
    events: broadcast::Sender<FromServer>,
    persist: watch::Sender<Vec<WorkerInfo>>,
}

#[derive(Debug, Default)]
struct State {
    workers: HashMap<WorkerId, Entry>,
    next_request: RequestId,
    next_generation: u64,
}

#[derive(Debug)]
struct Entry {
    info: WorkerInfo,
    sessions: Vec<SessionSummary>,
    agents: HashMap<SessionId, AgentEvent>,
    /// Which registration the entry reflects; a lease of another generation is stale.
    generation: u64,
    link: Option<Link>,
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
        let mut state = State::default();
        for mut info in known {
            info.liveness = Liveness::Gone;
            let entry = Entry {
                info,
                sessions: Vec::new(),
                agents: HashMap::new(),
                generation: 0,
                link: None,
            };
            state.workers.insert(entry.info.worker, entry);
        }
        let (events, _none) = broadcast::channel(EVENT_BUFFER);
        let (persist, _none) = watch::channel(Vec::new());
        Self { inner: Arc::new(Inner { name, state: Mutex::new(state), events, persist }) }
    }

    /// The server's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// Every worker, by name.
    #[must_use]
    pub fn directory(&self) -> Vec<WorkerInfo> {
        let mut out: Vec<WorkerInfo> =
            self.inner.state.lock().workers.values().map(|e| e.info.clone()).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.worker.cmp(&b.worker)));
        out
    }

    /// Directory changes and worker events, as they happen. Subscribe before reading
    /// [`Self::directory`] so nothing falls between the two; a receiver that lags should read the
    /// directory again.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<FromServer> {
        self.inner.events.subscribe()
    }

    /// The directory as it should be persisted, updated only when the registry changes shape
    /// (a worker appears, is renamed, moves, changes capabilities other than its load, or loses
    /// its link), never on a load tick.
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
        let Registration { worker, name, port, caps, sessions } = registration;
        if state.workers.get(&worker).is_some_and(|e| e.link.is_some()) {
            return Err(Refusal::DuplicateWorker);
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        let info = WorkerInfo {
            worker,
            name,
            address: SocketAddr::new(ip, port).to_string(),
            liveness: Liveness::Online,
            caps,
            last_seen_ms: now_ms(),
        };
        let link = Some(Link { tx, pending: HashMap::new() });
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
            entry.agents.clear();
            entry.generation = generation;
            entry.link = link;
            (reshaped, gone, opened)
        } else {
            let opened = sessions.clone();
            let entry =
                Entry { info: info.clone(), sessions, agents: HashMap::new(), generation, link };
            state.workers.insert(worker, entry);
            (true, Vec::new(), opened)
        };
        self.announce(FromServer::Worker(info));
        for session in gone {
            self.announce(FromServer::Event(Event::SessionClosed { worker, session }));
        }
        for summary in opened {
            self.announce(FromServer::Event(Event::SessionOpened { worker, summary }));
        }
        if reshaped {
            self.persist(&state);
        }
        drop(state);
        Ok(Lease { hub: self.clone(), worker, generation })
    }

    /// Answer a verb: the directory verbs from the registry, the rest forwarded to the owning
    /// worker and its reply returned. Never fails; a failure is an [`Outcome::Error`].
    pub async fn dispatch(&self, verb: Verb) -> Outcome {
        match verb {
            Verb::ListWorkers => Outcome::Workers(self.directory()),
            Verb::ListTerminals { worker } => self.terminals(worker),
            other => self.forward(other).await,
        }
    }

    fn terminals(&self, only: Option<WorkerId>) -> Outcome {
        let state = self.inner.state.lock();
        if let Some(worker) = only
            && !state.workers.contains_key(&worker)
        {
            return unknown_worker(worker);
        }
        let mut out: Vec<(WorkerId, SessionSummary)> = state
            .workers
            .values()
            .filter(|e| only.is_none_or(|w| w == e.info.worker))
            .flat_map(|e| e.sessions.iter().map(|s| (e.info.worker, s.clone())))
            .collect();
        drop(state);
        out.sort_by_key(|(worker, s)| (*worker, s.id));
        Outcome::Terminals(out)
    }

    async fn forward(&self, mut verb: Verb) -> Outcome {
        let deadline = match &mut verb {
            Verb::WaitFor { timeout_ms, .. } => {
                *timeout_ms = (*timeout_ms).min(WAIT_CAP_MS);
                Duration::from_millis(u64::from(*timeout_ms)).saturating_add(WAIT_GRACE)
            }
            _ => FORWARD_TIMEOUT,
        };
        let Some(worker) = target(&verb) else {
            return error(ErrorCode::Invalid, "this verb names no worker");
        };
        let (id, generation, tx, reply) = match self.expect_reply(worker) {
            Ok(pending) => pending,
            Err(outcome) => return outcome,
        };
        if tx.send(FromServer::Request { id, verb }).await.is_err() {
            return error(ErrorCode::WorkerUnreachable, "the worker's link closed");
        }
        match tokio::time::timeout(deadline, reply).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_dropped)) => error(
                ErrorCode::WorkerUnreachable,
                "the worker disconnected before it answered; it may have done the work",
            ),
            Err(_elapsed) => {
                self.forget(worker, generation, id);
                error(ErrorCode::Failed, "the worker did not answer in time")
            }
        }
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

    fn announce(&self, msg: FromServer) {
        let _no_listeners = self.inner.events.send(msg);
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
        entry.info.last_seen_ms = now_ms();
        let info = entry.info.clone();
        tracing::info!(%worker, name = %info.name, "worker unreachable");
        self.announce(FromServer::Worker(info));
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

    fn expire(&self, worker: WorkerId, generation: u64) {
        let mut state = self.inner.state.lock();
        let Some(entry) = state.workers.get_mut(&worker) else { return };
        if entry.generation != generation || entry.info.liveness != Liveness::Unreachable {
            return;
        }
        entry.info.liveness = Liveness::Gone;
        tracing::info!(%worker, name = %entry.info.name, "worker gone");
        let info = entry.info.clone();
        drop(state);
        self.announce(FromServer::Worker(info));
    }
}

impl Lease {
    /// The worker holding it.
    #[must_use]
    pub const fn worker(&self) -> WorkerId {
        self.worker
    }

    /// Take in one message from the worker.
    pub fn handle(&self, msg: ToServer) {
        let hub = &self.hub;
        let mut state = hub.inner.state.lock();
        let Some(entry) = state.workers.get_mut(&self.worker) else { return };
        if entry.generation != self.generation {
            return;
        }
        entry.info.last_seen_ms = now_ms();
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
                let reshaped = !same_caps(&entry.info.caps, &caps);
                entry.info.caps = caps;
                hub.announce(FromServer::Worker(entry.info.clone()));
                if reshaped {
                    hub.persist(&state);
                }
            }
            ToServer::SessionOpened(summary) => {
                match entry.sessions.iter_mut().find(|s| s.id == summary.id) {
                    Some(known) => known.clone_from(&summary),
                    None => entry.sessions.push(summary.clone()),
                }
                hub.announce(FromServer::Event(Event::SessionOpened { worker, summary }));
            }
            ToServer::SessionClosed { session, .. } => {
                entry.sessions.retain(|s| s.id != session);
                entry.agents.remove(&session);
                hub.announce(FromServer::Event(Event::SessionClosed { worker, session }));
            }
            ToServer::Agent(event) => {
                entry.agents.insert(event.session, event.clone());
                hub.announce(FromServer::Event(Event::Agent { worker, event }));
            }
            ToServer::Hello { .. } | ToServer::Request { .. } => {
                tracing::debug!(%worker, "ignored a message a worker does not send");
            }
        }
        // Announced under the lock, so every link hears changes in the order they were made.
        drop(state);
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
        Verb::ListWorkers | Verb::ListTerminals { .. } => None,
        Verb::OpenTerminal { worker, .. }
        | Verb::SpawnAgent { worker, .. }
        | Verb::ReadFile { worker, .. }
        | Verb::WriteFile { worker, .. }
        | Verb::ListPorts { worker } => Some(*worker),
        Verb::SendInput { term, .. }
        | Verb::ReadScreen { term }
        | Verb::ReadOutput { term, .. }
        | Verb::ListCommands { term, .. }
        | Verb::WaitFor { term, .. }
        | Verb::AgentStatus { term }
        | Verb::Close { term } => Some(term.worker),
    }
}

/// Whether two infos persist the same: name, address and capabilities but for the load.
fn same_shape(a: &WorkerInfo, b: &WorkerInfo) -> bool {
    a.worker == b.worker
        && a.name == b.name
        && a.address == b.address
        && same_caps(&a.caps, &b.caps)
}

fn same_caps(a: &WorkerCaps, b: &WorkerCaps) -> bool {
    WorkerCaps { load: a.load, ..b.clone() } == *a
}

fn error(code: ErrorCode, message: &str) -> Outcome {
    Outcome::Error { code, message: message.to_owned() }
}

fn unknown_worker(worker: WorkerId) -> Outcome {
    Outcome::Error {
        code: ErrorCode::UnknownWorker,
        message: format!("no worker {worker}; list_workers names the known ones"),
    }
}

fn unreachable(info: &WorkerInfo) -> Outcome {
    Outcome::Error {
        code: ErrorCode::WorkerUnreachable,
        message: format!("worker {} ({}) is {:?}", info.name, info.worker, info.liveness),
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
pub(crate) mod tests {
    use slopty_proto::orchestration::{Screen, TermRef};
    use slopty_proto::server::Os;
    use slopty_proto::terminal::{SessionKind, SessionState};

    use super::*;

    pub fn caps() -> WorkerCaps {
        WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 12,
            memory: 32 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: true,
            can_inject: true,
            load: 0.5,
            version: "0.1.0".to_owned(),
        }
    }

    pub fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            kind: SessionKind::Terminal,
            title: "zsh".to_owned(),
            cwd: None,
            repo: None,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
        }
    }

    pub fn registration(worker: WorkerId, sessions: Vec<SessionSummary>) -> Registration {
        Registration { worker, name: "studio".to_owned(), port: 45550, caps: caps(), sessions }
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
            matches!(events.recv().await.unwrap(), FromServer::Worker(w) if w.liveness == Liveness::Online)
        );

        drop(lease);
        assert_eq!(liveness(&hub, worker), Liveness::Unreachable, "at once when the link ends");
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Worker(w) if w.liveness == Liveness::Unreachable)
        );
        tokio::time::sleep(GONE_AFTER.checked_sub(Duration::from_millis(100)).unwrap()).await;
        assert_eq!(liveness(&hub, worker), Liveness::Unreachable, "not gone before 20 s");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(liveness(&hub, worker), Liveness::Gone);
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Worker(w) if w.liveness == Liveness::Gone)
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
        let Some(FromServer::Request { id, verb }) = rx.recv().await else { panic!("no request") };
        assert_eq!(verb, Verb::ReadScreen { term });
        lease.handle(ToServer::Reply { id, outcome: Outcome::Screen(screen()) });
        assert_eq!(asked.await.unwrap(), Outcome::Screen(screen()));

        // Dropped mid-request: the waiter hears at once.
        let asked = tokio::spawn({
            let hub = hub.clone();
            async move { hub.dispatch(Verb::ReadScreen { term }).await }
        });
        let Some(FromServer::Request { .. }) = rx.recv().await else { panic!("no request") };
        drop(lease);
        let dropped = asked.await.unwrap();
        assert!(
            matches!(dropped, Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }),
            "{dropped:?}"
        );

        let offline = hub.dispatch(Verb::Close { term }).await;
        assert!(
            matches!(offline, Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }),
            "{offline:?}"
        );
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
        assert!(matches!(late, Outcome::Error { code: ErrorCode::Failed, .. }), "{late:?}");
    }

    #[tokio::test]
    async fn sessions_and_agents_are_tracked_and_fanned_out() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (first, second) = (SessionId::new(), SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(first)]), ip(), tx).unwrap();
        let mut events = hub.subscribe();
        lease.handle(ToServer::SessionOpened(summary(second)));
        lease.handle(ToServer::SessionClosed {
            session: first,
            reason: slopty_proto::terminal::CloseReason::Exited,
        });
        let Outcome::Terminals(list) = hub.dispatch(Verb::ListTerminals { worker: None }).await
        else {
            panic!("terminals")
        };
        assert_eq!(list, vec![(worker, summary(second))]);
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Event(Event::SessionOpened { summary, .. }) if summary.id == second)
        );
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Event(Event::SessionClosed { session, .. }) if session == first)
        );
        let elsewhere = hub.dispatch(Verb::ListTerminals { worker: Some(WorkerId::new()) }).await;
        assert!(matches!(elsewhere, Outcome::Error { code: ErrorCode::UnknownWorker, .. }));

        // A re-registration reports the difference.
        drop(lease);
        let _drain = events.recv().await;
        let (tx, _rx) = mpsc::channel(8);
        let _lease = hub.register(registration(worker, vec![summary(first)]), ip(), tx).unwrap();
        let _online = events.recv().await;
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Event(Event::SessionClosed { session, .. }) if session == second)
        );
        assert!(
            matches!(events.recv().await.unwrap(), FromServer::Event(Event::SessionOpened { summary, .. }) if summary.id == first)
        );
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

        lease.handle(ToServer::Caps(WorkerCaps { load: 3.0, ..caps() }));
        assert!(!persisted.has_changed().unwrap(), "a load tick is not a change of shape");
        assert!((hub.directory()[0].caps.load - 3.0).abs() < f32::EPSILON, "but it is live");

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
            last_seen_ms: 1,
        };
        let hub = Hub::new("server".to_owned(), vec![info.clone()]);
        assert_eq!(hub.directory(), vec![WorkerInfo { liveness: Liveness::Gone, ..info }]);
    }
}
