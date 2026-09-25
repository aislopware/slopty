//! The registry: every worker the server knows, the lease each live one holds, and the verbs
//! routed to them.
//!
//! A worker's connection is its lease ([`Lease`]): registering takes it, dropping it (the link
//! ended, cleanly or by the idle timeout) marks the worker [`Liveness::Unreachable`], and
//! [`GONE_AFTER`] later, with no reconnect, [`Liveness::Gone`]. Every change goes out to every
//! client and agent link as [`FromServer::Worker`] or [`FromServer::Event`].
//!
//! A worker set up again (a new data directory) registers under a new id with its old name,
//! from its old address. Nothing else can be listening there, so the entries it replaces are
//! dropped and every link gets the directory again without them.
//!
//! [`Hub::dispatch`] is the one verb dispatch both front ends call (QUIC links and MCP): the
//! directory verbs, [`Verb::Events`] and [`Verb::ForgetWorker`] are answered here, the rest go
//! down the owning worker's link.
//!
//! Every change of liveness, terminal and agent status also goes into a bounded log of
//! [`HubEvent`]s under one sequence, which [`Verb::Events`] reads from a cursor and waits on:
//! one call watches the whole fleet, over MCP's stateless HTTP as well as a QUIC link.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use slopty_core::{SessionId, WorkerId};
use slopty_proto::agent::{AgentStatus, SessionAgent};
use slopty_proto::orchestration::{
    ErrorCode, EventFilter, Happening, HubEvent, Outcome, TermRef, Verb,
};
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
/// Events the log holds for [`Verb::Events`]; a cursor older than the oldest reads on from it
/// and is told how many it missed.
pub const EVENT_LOG: usize = 4096;
/// Most events one [`Verb::Events`] returns; the caller asks again from `next`.
pub const EVENTS_PER_ANSWER: usize = 500;

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
    /// Taken only inside `state`'s lock, or alone.
    log: Mutex<Log>,
    /// The log's next sequence number, for [`Verb::Events`] to wait on.
    head: watch::Sender<u64>,
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
}

#[derive(Debug)]
struct Entry {
    info: WorkerInfo,
    /// Each with its agent kept current from the worker's `Agent` events.
    sessions: Vec<SessionSummary>,
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
            let entry = Entry { info, sessions: Vec::new(), generation: 0, link: None };
            state.workers.insert(entry.info.worker, entry);
        }
        let (events, _none) = broadcast::channel(EVENT_BUFFER);
        let (persist, _none) = watch::channel(Vec::new());
        let first = run_seed();
        let log = Mutex::new(Log { ring: VecDeque::new(), first, next: first });
        let (head, _none) = watch::channel(first);
        let state = Mutex::new(state);
        Self { inner: Arc::new(Inner { name, state, events, persist, log, head }) }
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

    /// The directory and every terminal with its agent, read together.
    #[must_use]
    pub fn state(&self) -> (Vec<WorkerInfo>, Vec<(WorkerId, SessionSummary)>) {
        let state = self.inner.state.lock();
        let directory = listing(&state);
        let mut terminals: Vec<(WorkerId, SessionSummary)> = state
            .workers
            .values()
            .flat_map(|e| e.sessions.iter().map(|s| (e.info.worker, s.clone())))
            .collect();
        drop(state);
        terminals.sort_by_key(|(worker, s)| (*worker, s.id));
        (directory, terminals)
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
                self.close_all(*old, &gone.sessions);
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
            (reshaped, gone, opened)
        } else {
            let opened = sessions.clone();
            let entry = Entry { info: info.clone(), sessions, generation, link };
            state.workers.insert(worker, entry);
            (true, Vec::new(), opened)
        };
        let (name, liveness) = (info.name.clone(), info.liveness);
        self.happen(Happening::Worker { worker, name, liveness });
        self.announce(FromServer::Worker(info));
        if !replaced.is_empty() {
            self.announce(FromServer::Directory(listing(&state)));
        }
        for session in gone {
            self.happen(Happening::SessionClosed { term: TermRef { worker, session } });
            self.announce(FromServer::Event(Event::SessionClosed { worker, session }));
        }
        for summary in opened {
            self.happen(Happening::SessionOpened { worker, summary: summary.clone() });
            self.announce(FromServer::Event(Event::SessionOpened { worker, summary }));
        }
        if reshaped || !replaced.is_empty() {
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
            Verb::Events { since, timeout_ms, filter } => {
                self.events(since, timeout_ms, filter).await
            }
            Verb::ForgetWorker { worker } => self.forget_worker(worker),
            other => self.forward(other).await,
        }
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
        for event in log.ring.iter().skip(skip) {
            if page.events.len() == EVENTS_PER_ANSWER {
                page.next = event.seq;
                break;
            }
            if filter.admits(&event.what) {
                page.events.push(event.clone());
            }
        }
        drop(log);
        page
    }

    /// Remove a worker that holds no lease. A live one would register again at once, so it is
    /// refused; every link hears the directory without the worker, and the state file loses it.
    fn forget_worker(&self, worker: WorkerId) -> Outcome {
        let mut state = self.inner.state.lock();
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
        self.close_all(worker, &gone.sessions);
        self.happen(Happening::WorkerRemoved { worker, name: gone.info.name });
        self.announce(FromServer::Directory(listing(&state)));
        self.persist(&state);
        drop(state);
        Outcome::Done
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

    /// Every session of a removed worker ended, for the log and every link: its agents stop
    /// counting as waiting anywhere.
    fn close_all(&self, worker: WorkerId, sessions: &[SessionSummary]) {
        for session in sessions.iter().map(|s| s.id) {
            self.happen(Happening::SessionClosed { term: TermRef { worker, session } });
            self.announce(FromServer::Event(Event::SessionClosed { worker, session }));
        }
    }

    fn announce(&self, msg: FromServer) {
        let _no_listeners = self.inner.events.send(msg);
    }

    /// Log an event under the next sequence number and wake the waiting [`Verb::Events`].
    /// Called under the state lock, so the log's order is the order changes were made.
    fn happen(&self, what: Happening) {
        let mut log = self.inner.log.lock();
        let seq = log.next;
        log.next = seq.saturating_add(1);
        log.ring.push_back(HubEvent { seq, at_ms: now_ms(), what });
        if log.ring.len() > EVENT_LOG {
            log.ring.pop_front();
        }
        self.inner.head.send_replace(log.next);
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
        let (name, liveness) = (info.name.clone(), info.liveness);
        self.happen(Happening::Worker { worker, name, liveness });
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
        let name = info.name.clone();
        self.happen(Happening::Worker { worker, name, liveness: Liveness::Gone });
        drop(state);
        self.announce(FromServer::Worker(info));
    }
}

/// The orchestration tools run on the hub in-process, the same dispatch the QUIC links use.
impl slopty_tools::Dispatch for Hub {
    fn call(&self, verb: Verb) -> impl Future<Output = Outcome> + Send {
        self.dispatch(verb)
    }
}

impl Lease {
    /// The worker holding it.
    #[must_use]
    pub const fn worker(&self) -> WorkerId {
        self.worker
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
                // A known session reports a change (a resize): no event of its own.
                if let Some(known) = entry.sessions.iter_mut().find(|s| s.id == summary.id) {
                    known.clone_from(&summary);
                } else {
                    entry.sessions.push(summary.clone());
                    hub.happen(Happening::SessionOpened { worker, summary: summary.clone() });
                }
                hub.announce(FromServer::Event(Event::SessionOpened { worker, summary }));
            }
            ToServer::SessionClosed { session, .. } => {
                entry.sessions.retain(|s| s.id != session);
                hub.happen(Happening::SessionClosed { term: TermRef { worker, session } });
                hub.announce(FromServer::Event(Event::SessionClosed { worker, session }));
            }
            ToServer::Agent(event) => {
                let listed = entry.sessions.iter_mut().find(|s| s.id == event.session);
                let before = listed.as_ref().map(|s| s.agent.as_ref().map(|a| &a.status));
                // A report of the status already known (the same tool again) is no change.
                let same = before
                    .is_some_and(|before| before.unwrap_or(&AgentStatus::None) == &event.status);
                if let Some(summary) = listed {
                    summary.agent =
                        (event.status != AgentStatus::None).then(|| SessionAgent::from(&event));
                }
                if !same {
                    hub.happen(Happening::Agent {
                        term: TermRef { worker, session: event.session },
                        kind: event.kind,
                        status: event.status.clone(),
                        detail: event.detail.clone(),
                    });
                }
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
        Verb::ListWorkers
        | Verb::ListTerminals { .. }
        | Verb::Events { .. }
        | Verb::ForgetWorker { .. } => None,
        Verb::OpenTerminal { worker, .. }
        | Verb::SpawnAgent { worker, .. }
        | Verb::ReadFile { worker, .. }
        | Verb::WriteFile { worker, .. }
        | Verb::ListDir { worker, .. }
        | Verb::Stat { worker, .. }
        | Verb::ListPorts { worker } => Some(*worker),
        Verb::SendInput { term, .. }
        | Verb::ReadScreen { term }
        | Verb::ReadOutput { term, .. }
        | Verb::ListCommands { term, .. }
        | Verb::WaitFor { term, .. }
        | Verb::AgentStatus { term }
        | Verb::ResizeTerminal { term, .. }
        | Verb::Close { term } => Some(term.worker),
    }
}

/// Every worker, by name.
fn listing(state: &State) -> Vec<WorkerInfo> {
    let mut out: Vec<WorkerInfo> = state.workers.values().map(|e| e.info.clone()).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.worker.cmp(&b.worker)));
    out
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

/// Where this run's event sequence starts: microseconds since the Unix epoch. A run logs far
/// fewer than one event a microsecond, so each run numbers above everything an earlier one did,
/// and a cursor kept across a restart is never mistaken for one of this run.
fn run_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX).max(1))
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
pub(crate) mod tests {
    use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, BlockReason};
    use slopty_proto::orchestration::{Screen, TermRef};
    use slopty_proto::server::Os;
    use slopty_proto::terminal::{CloseReason, SessionState};

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
            title: "zsh".to_owned(),
            cwd: None,
            repo: None,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
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
        lease.handle(ToServer::SessionClosed { session: first, reason: CloseReason::Exited });
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

    /// `ListTerminals` answers with each agent as the worker last reported it, with the signal
    /// it was read from, and an agent that left leaves its terminal without one.
    #[tokio::test]
    async fn listed_terminals_carry_the_agent_as_last_reported() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(8);
        let lease = hub.register(registration(worker, vec![summary(session)]), ip(), tx).unwrap();
        let report = |session: SessionId, status: AgentStatus, source: AgentSource| {
            ToServer::Agent(AgentEvent {
                session,
                kind: AgentKind::ClaudeCode,
                status,
                agent_session: None,
                detail: None,
                attention: false,
                source,
            })
        };
        let agent =
            |status, source| Some(SessionAgent { kind: AgentKind::ClaudeCode, status, source });
        let listed = async || {
            let Outcome::Terminals(list) = hub.dispatch(Verb::ListTerminals { worker: None }).await
            else {
                panic!("terminals")
            };
            list.into_iter().map(|(_worker, s)| s.agent).collect::<Vec<_>>()
        };
        assert_eq!(listed().await, [None], "no agent yet");
        lease.handle(report(session, AgentStatus::Working, AgentSource::Title));
        assert_eq!(listed().await, [agent(AgentStatus::Working, AgentSource::Title)]);
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        lease.handle(report(session, blocked.clone(), AgentSource::Hook));
        assert_eq!(listed().await, [agent(blocked, AgentSource::Hook)], "kept current");
        lease.handle(report(session, AgentStatus::None, AgentSource::Hook));
        assert_eq!(listed().await, [None], "the agent left");
        lease.handle(report(SessionId::new(), AgentStatus::Idle, AgentSource::Hook));
        assert_eq!(listed().await, [None], "a report for a session not listed changes nothing");
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

    fn agent_report(session: SessionId, status: AgentStatus) -> ToServer {
        ToServer::Agent(AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: false,
            source: AgentSource::Hook,
        })
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
        lease.handle(ToServer::SessionOpened(summary(session)));
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
        lease.handle(ToServer::SessionOpened(summary(session)));
        assert!(events(&hub, Some(next), 0).await.0.is_empty(), "a known session's update");

        // A wait wakes for the next event, however long before its timeout it comes.
        let waiting = tokio::spawn({
            let hub = hub.clone();
            async move { events(&hub, None, 60_000).await }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiting.is_finished(), "nothing new yet");
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        lease.handle(agent_report(session, blocked.clone()));
        let (woke, after, _) = waiting.await.unwrap();
        let term = TermRef { worker, session };
        assert!(
            matches!(woke.as_slice(), [HubEvent { what: Happening::Agent { term: t, status, .. }, .. }]
                if *t == term && *status == blocked),
            "{woke:?}"
        );
        lease.handle(agent_report(session, blocked));
        let started = tokio::time::Instant::now();
        let (none, same, _) = events(&hub, Some(after), 1_000).await;
        assert!(none.is_empty(), "the same status again is no event");
        assert_eq!(same, after, "the cursor stays");
        assert_eq!(started.elapsed(), Duration::from_secs(1), "gave up at the timeout");

        // Filtered: only an agent that needs a human or went idle, the cursor past the rest.
        lease.handle(agent_report(session, AgentStatus::Working));
        lease.handle(ToServer::SessionClosed {
            session: SessionId::new(),
            reason: CloseReason::Exited,
        });
        lease.handle(agent_report(session, AgentStatus::Idle));
        let filter = EventFilter::AgentNeedsInput;
        let asked = Verb::Events { since: Some(after), timeout_ms: 0, filter };
        let Outcome::Events { events: needs, next, .. } = hub.dispatch(asked).await else {
            panic!("events")
        };
        assert!(
            matches!(
                needs.as_slice(),
                [HubEvent { what: Happening::Agent { status: AgentStatus::Idle, .. }, .. }]
            ),
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
        for i in 0..EVENT_LOG + extra {
            let status = if i % 2 == 0 { AgentStatus::Working } else { AgentStatus::Idle };
            lease.handle(agent_report(session, status));
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
            for i in 0..n {
                let status = if i % 2 == 0 { AgentStatus::Working } else { AgentStatus::Idle };
                lease.handle(agent_report(session, status));
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
        for i in 0..EVENT_LOG + extra {
            let status = if i % 2 == 0 {
                AgentStatus::Working
            } else {
                AgentStatus::Tool { tool: "Bash".to_owned() }
            };
            lease.handle(agent_report(session, status));
        }
        lease.handle(agent_report(session, AgentStatus::Idle));
        let Outcome::Events { events, missed, .. } = waiting.await.unwrap() else {
            panic!("events")
        };
        assert!(
            matches!(
                events.as_slice(),
                [HubEvent { what: Happening::Agent { status: AgentStatus::Idle, .. }, .. }]
            ),
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
                if let FromServer::Event(Event::SessionClosed { worker, session }) = msg {
                    closed.push(TermRef { worker, session });
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
        assert!(hub.directory().is_empty());
        assert!(persisted.borrow_and_update().is_empty(), "the state file loses it");
        assert!(
            matches!(links.recv().await.unwrap(), FromServer::Directory(list) if list.is_empty())
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
            last_seen_ms: 1,
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
        assert!(matches!(events.recv().await.unwrap(), FromServer::Worker(w) if w.worker == first));
        let FromServer::Directory(heard) = events.recv().await.unwrap() else {
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
}
