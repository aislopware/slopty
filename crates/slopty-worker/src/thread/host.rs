//! The thread host: every thread this worker holds, their logs, the table and the intents.
//!
//! One lock guards it all. What happens under it is small: applying actions, appending them
//! to a file, and a snapshot written at a turn's end once in a while. A follower subscribes
//! and takes its catch-up under the same lock, so it misses nothing and is sent nothing twice
//! ([`Host::follow`]).
//!
//! A thread's log is opened only when the thread is first used. At the start the host reads each
//! thread's head ([`Log::head`]): its row for the table, and its state without items for a look
//! over every thread ([`Host::visit`]). A thread whose head is missing or behind its log is
//! opened then, and its head written, so the next start reads the head alone.
//!
//! A thread opened goes back to its head once it has rested ([`Host::rest`]): nobody follows it
//! and nothing used it for a while. Its next use opens it again, the same, so a worker that runs
//! for weeks holds the threads in use and not every thread ever opened.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::{Outcome, Page, Start, TableFrame};
use slopty_proto::thread::{
    Action, AgentId, Cursor, IntentId, ItemBody, Link, Phase, Status, ThreadId, ThreadMeta,
    ThreadState, ToolCall, ToolState, TurnId,
};
use tokio::sync::{broadcast, watch};

use super::Seated;
use super::intents::Intents;
use super::log::{Catchup, Head, Limits, Log};
use super::table::Table;

/// Batches a slow follower may fall behind by before it catches up from the log instead.
const FEED_BATCHES: usize = 1024;

/// Tool calls a slow listener may fall behind by; one that lags misses those calls.
const TOOLS: usize = 256;

/// A tool call of a thread's agent, running or run ([`Host::tools`]).
#[derive(Clone, Debug)]
pub struct ToolSeen {
    /// The thread.
    pub thread: ThreadId,
    /// The call as it stands.
    pub call: Arc<ToolCall>,
}

/// What the host tells beside the actions themselves.
#[derive(Debug)]
struct Heard {
    /// Tool calls ([`Host::tools`]).
    tools: broadcast::Sender<ToolSeen>,
}

/// Actions applied together, as every follower of the thread hears them.
#[derive(Debug)]
pub struct Batch {
    /// The log's epoch after them. A new one means the log started over ([`Host::reset`]),
    /// and `actions` is empty.
    pub epoch: u64,
    /// The `seq` they apply to.
    pub seq: u64,
    /// The actions.
    pub actions: Vec<Action>,
}

/// A follower's start: what it missed, and the thread's actions from there on.
#[derive(Debug)]
pub struct Follow {
    /// What it missed.
    pub catchup: Catchup,
    /// Each batch after it.
    pub feed: broadcast::Receiver<Arc<Batch>>,
}

/// The threads this worker holds. Cheap to clone; every clone shares them.
#[derive(Clone, Debug)]
pub struct Host {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    limits: Limits,
    /// The threads whose logs are open.
    threads: HashMap<ThreadId, Hosted>,
    /// The threads held whose logs are not opened yet, by their heads.
    cold: HashMap<ThreadId, Head>,
    table: Table,
    /// Intents that start a thread, which have no thread of their own to be kept with yet.
    starts: Intents,
    heard: Heard,
    /// Slopty's CLI, whose `mcp` serves Slopty's tools, once the daemon found it.
    tools_cli: Option<String>,
    /// The starts under way, as kept ([`Host::start_under_way`]).
    under_way: Vec<UnderWay>,
}

/// The file in a thread's directory that keeps the seat it was started at ([`Seated`]).
const SEAT_FILE: &str = "seat.json";

/// The file in the threads' directory that keeps the starts under way ([`UnderWay`]).
const UNDER_WAY_FILE: &str = "starts-under-way.json";

/// A start under way, kept until its outcome is recorded ([`Host::start_under_way`]).
///
/// A worker that stops meanwhile (an update, a crash) leaves the next one what it needs to
/// finish it or say it failed: no client waits on a start nobody holds.
///
/// It holds the start as the client sent it, which names nothing secret.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct UnderWay {
    /// The start's intent.
    pub id: IntentId,
    /// As it was asked.
    pub start: Start,
    /// Past its worktree's setup, its agent opening: one taken up again could open it twice.
    pub begun: bool,
}

#[derive(Debug)]
struct Hosted {
    log: Log,
    intents: Intents,
    feed: broadcast::Sender<Arc<Batch>>,
    own: Own,
    /// When it was last used ([`Host::rest`]).
    used: tokio::time::Instant,
}

impl Hosted {
    /// Whether it can go back to its head: nobody follows it and nothing used it since
    /// `before`. Its log and files hold all of it.
    fn resting(&self, before: tokio::time::Instant) -> bool {
        self.used <= before && self.feed.receiver_count() == 0
    }

    fn open(dir: &Path, log: Log) -> io::Result<Self> {
        let mut own = Own::of(log.state());
        own.seated = match std::fs::read(dir.join(SEAT_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .inspect_err(|e| tracing::warn!(dir = %dir.display(), "a seat is unreadable: {e}"))
                .ok(),
            Err(_) => None,
        };
        Ok(Self {
            log,
            intents: Intents::open(&dir.join("intents"))?,
            feed: broadcast::Sender::new(FEED_BATCHES),
            own,
            used: tokio::time::Instant::now(),
        })
    }
}

/// What the worker adds to a thread that its agent's records never tell. A thread read again
/// from its agent's session ([`Host::reset`]) keeps it.
#[derive(Debug, Default)]
struct Own {
    /// The seat the server started it at ([`slopty_proto::agent::SEAT_FACT`]): an agent's
    /// records that tell the thread's metadata again know nothing of it.
    seat: Option<String>,
    /// The seat it was started at, all of it, kept in its directory ([`SEAT_FILE`]).
    seated: Option<Seated>,
    /// The call it hangs off, once its records said: a subagent read again from nothing may
    /// name its parent only later, and a thread never moves to another.
    parent: Option<Link>,
}

impl Own {
    fn of(state: &ThreadState) -> Self {
        let seat = state.meta.facts.get(slopty_proto::agent::SEAT_FACT).cloned();
        Self { seat, seated: None, parent: state.meta.parent.clone() }
    }

    /// `meta` with what the worker knows of it: the seat it was started at, the call it hangs
    /// off.
    fn meta(&mut self, meta: &mut ThreadMeta) {
        match &meta.parent {
            Some(parent) => self.parent = Some(parent.clone()),
            None => meta.parent.clone_from(&self.parent),
        }
        if let Some(seat) = &self.seat {
            meta.facts.insert(slopty_proto::agent::SEAT_FACT.to_owned(), seat.clone());
        }
        for (name, value) in self.seated.iter().flat_map(|seated| &seated.facts) {
            meta.facts.insert(name.clone(), value.clone());
        }
    }

    /// `actions` with what the worker knows put back in.
    fn mark(&mut self, actions: &mut [Action]) {
        for action in actions {
            if let Action::Meta(meta) = action {
                self.meta(meta);
            }
        }
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "each step is the lock's whole: an apply and its send, a catch-up and its subscription, \
              an intent's check, act and record"
)]
impl Host {
    /// The threads kept under `dir`, which it makes. A thread whose log cannot be read is
    /// left out, with a warning: its agent's own session still has everything.
    ///
    /// # Errors
    ///
    /// When the OS refuses `dir` (it cannot be made or listed) or the record of starts in it:
    /// what is in them never fails an open.
    pub fn open(dir: &Path, limits: Limits) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut cold = HashMap::new();
        let mut table = Table::new(ThreadId::new().as_uuid().as_u64_pair().1);
        for entry in std::fs::read_dir(dir)? {
            // One entry the OS will not list is one thread lost, not every thread.
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(e) => {
                    tracing::warn!(dir = %dir.display(), "a thread's entry is unreadable: {e}");
                    continue;
                }
            };
            if !path.is_dir() {
                continue;
            }
            if let Some(head) = Log::head(&path) {
                table.put(head.row.clone());
                cold.insert(head.row.id, head);
                continue;
            }
            // No head, or one the log has moved past: the log is read once, and its head
            // written for the next start.
            let hosted = Log::open(&path, limits).and_then(|log| Hosted::open(&path, log));
            match hosted {
                Ok(mut hosted) => {
                    let row = hosted.log.state().row(WallMs::now());
                    table.put(row.clone());
                    match hosted.log.write_head(&row) {
                        Ok(head) => {
                            cold.insert(row.id, head);
                        }
                        Err(e) => {
                            tracing::warn!(path = %path.display(), "a thread's head was not kept: {e}");
                            let head = Head { row, state: hosted.log.state().clone() };
                            cold.insert(head.row.id, head);
                        }
                    }
                }
                Err(e) => unreadable(&path, &e),
            }
        }
        let starts = Intents::open(&dir.join("starts"))?;
        let under_way = match std::fs::read(dir.join(UNDER_WAY_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("the starts under way are unreadable: {e}");
                Vec::new()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::warn!("the starts under way are unreadable: {e}");
                Vec::new()
            }
        };
        let host = Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_owned(),
                limits,
                threads: HashMap::new(),
                cold,
                table,
                starts,
                heard: Heard { tools: broadcast::Sender::new(TOOLS) },
                tools_cli: None,
                under_way,
            })),
        };
        Ok(host)
    }

    /// Host a new thread.
    ///
    /// # Errors
    ///
    /// When its log cannot be made, or the thread is held already
    /// ([`io::ErrorKind::AlreadyExists`]): one is never made over another.
    pub fn create(&self, meta: ThreadMeta) -> io::Result<Cursor> {
        create(&mut self.inner.lock(), meta)
    }

    /// Host `meta`'s thread, or apply what `change` makes of it when it is held already, in one
    /// step: nothing makes or changes it between the look and the write. `None` when it was
    /// made here, else what `change` returned beside its actions.
    ///
    /// # Errors
    ///
    /// When its log cannot be made.
    pub fn create_or_update<F, T>(&self, meta: ThreadMeta, change: F) -> io::Result<Option<T>>
    where
        F: FnOnce(&ThreadState) -> (Vec<Action>, T),
    {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(meta.id);
        let Some(hosted) = inner.threads.get_mut(&meta.id) else {
            return create(inner, meta).map(|_| None);
        };
        let (actions, out) = change(hosted.log.state());
        if !actions.is_empty() {
            apply(hosted, &mut inner.table, &inner.heard, actions);
        }
        Ok(Some(out))
    }

    /// Begin `state`'s thread anew, as its agent's records tell it from nothing, in one step:
    /// hosted as `state` says when it is not; else its log started over from `state` as
    /// [`Self::reset`] does, its meta what `meta` makes of the one held, so what others wrote of
    /// it since the records were read is not lost.
    ///
    /// # Errors
    ///
    /// When its log cannot be made or written.
    pub fn begin<F>(&self, mut state: ThreadState, meta: F) -> io::Result<Cursor>
    where
        F: FnOnce(&ThreadMeta) -> ThreadMeta,
    {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(state.meta.id);
        let Some(hosted) = inner.threads.get_mut(&state.meta.id) else {
            return create(inner, state.meta);
        };
        state.meta = meta(&hosted.log.state().meta);
        restart(hosted, &mut inner.table, state)
    }

    /// Whether `thread` is held.
    #[must_use]
    pub fn holds(&self, thread: ThreadId) -> bool {
        let inner = self.inner.lock();
        inner.threads.contains_key(&thread) || inner.cold.contains_key(&thread)
    }

    /// How many threads have their logs open: those used since the worker started that have
    /// not rested since ([`Self::rest`]).
    #[must_use]
    pub fn opened(&self) -> usize {
        self.inner.lock().threads.len()
    }

    /// Put every opened thread that has rested `idle` back to its head, as at the start: its
    /// head written over its log, its state and tail let go, its files closed. How many went.
    /// One whose head cannot be written keeps its head in memory alone, as at the start.
    pub fn rest(&self, idle: std::time::Duration) -> usize {
        let Some(before) = tokio::time::Instant::now().checked_sub(idle) else { return 0 };
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let resting: Vec<ThreadId> = inner
            .threads
            .iter()
            .filter(|(_, hosted)| hosted.resting(before))
            .map(|(thread, _)| *thread)
            .collect();
        for thread in &resting {
            let Some(mut hosted) = inner.threads.remove(thread) else { continue };
            let row = inner
                .table
                .row(*thread)
                .cloned()
                .unwrap_or_else(|| hosted.log.state().row(WallMs::now()));
            let head = hosted.log.write_head(&row).unwrap_or_else(|e| {
                tracing::warn!(%thread, "a resting thread's head was not kept: {e}");
                let mut state = hosted.log.state().clone();
                state.items.clear();
                Head { row, state }
            });
            inner.cold.insert(*thread, head);
        }
        resting.len()
    }

    /// The threads held.
    #[must_use]
    pub fn threads(&self) -> Vec<ThreadId> {
        let inner = self.inner.lock();
        let mut ids: Vec<ThreadId> =
            inner.threads.keys().chain(inner.cold.keys()).copied().collect();
        drop(inner);
        ids.sort_unstable();
        ids
    }

    /// What `see` makes of each thread held, those it makes nothing of left out. The lock is
    /// taken for one thread at a time, so an adapter waits on no more than one thread's look.
    ///
    /// A thread not opened yet is seen by its head: everything but its items, which stay on
    /// disk.
    pub fn visit<T>(&self, mut see: impl FnMut(&ThreadState) -> Option<T>) -> Vec<T> {
        self.threads()
            .into_iter()
            .filter_map(|thread| {
                let inner = self.inner.lock();
                match inner.threads.get(&thread) {
                    Some(hosted) => see(hosted.log.state()),
                    None => inner.cold.get(&thread).and_then(|head| see(&head.state)),
                }
            })
            .collect()
    }

    /// What `thread` is (its meta) and how it stands, without its items, if it is held.
    #[must_use]
    pub fn meta(&self, thread: ThreadId) -> Option<(ThreadMeta, Status)> {
        let inner = self.inner.lock();
        let state = match inner.threads.get(&thread) {
            Some(hosted) => hosted.log.state(),
            None => &inner.cold.get(&thread)?.state,
        };
        Some((state.meta.clone(), state.status.clone()))
    }

    /// Apply `actions` to `thread`, log them and send them to its followers; the cursor after
    /// them, or `None` for a thread not held. A log that cannot be written is warned of, and
    /// the actions count anyway.
    pub fn apply(&self, thread: ThreadId, actions: Vec<Action>) -> Option<Cursor> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let hosted = inner.threads.get_mut(&thread)?;
        Some(apply(hosted, &mut inner.table, &inner.heard, actions))
    }

    /// The thread held of agent `agent`'s session `native`, and its title, when one is.
    #[must_use]
    pub fn session(&self, agent: &AgentId, native: &str) -> Option<(ThreadId, String)> {
        let inner = self.inner.lock();
        let open = inner.threads.iter().map(|(id, hosted)| (id, &hosted.log.state().meta));
        let cold = inner.cold.iter().map(|(id, head)| (id, &head.state.meta));
        open.chain(cold).find_map(|(id, meta)| {
            (meta.agent == *agent && meta.native == native).then(|| (*id, meta.title.clone()))
        })
    }

    /// Apply what `change` makes of `thread` as it stands, in one step: nothing else changes
    /// it between the look and the actions. `None` for a thread not held, else what `change`
    /// returned beside its actions.
    pub fn update<F, T>(&self, thread: ThreadId, change: F) -> Option<T>
    where
        F: FnOnce(&ThreadState) -> (Vec<Action>, T),
    {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let hosted = inner.threads.get_mut(&thread)?;
        let (actions, out) = change(hosted.log.state());
        if !actions.is_empty() {
            apply(hosted, &mut inner.table, &inner.heard, actions);
        }
        Some(out)
    }

    /// Start `thread`'s log over from `state`, under a new epoch: every follower gets a
    /// snapshot.
    ///
    /// # Errors
    ///
    /// When the log cannot be written.
    pub fn reset(&self, thread: ThreadId, state: ThreadState) -> io::Result<Option<Cursor>> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let Some(hosted) = inner.threads.get_mut(&thread) else { return Ok(None) };
        restart(hosted, &mut inner.table, state).map(Some)
    }

    /// Stop hosting `thread` and delete its log. Its followers' feeds close.
    ///
    /// # Errors
    ///
    /// When its files cannot be removed.
    pub fn remove(&self, thread: ThreadId) -> io::Result<()> {
        let mut inner = self.inner.lock();
        inner.table.remove(thread);
        if inner.cold.remove(&thread).is_some() {
            return std::fs::remove_dir_all(inner.dir.join(thread.to_string()));
        }
        match inner.threads.remove(&thread) {
            Some(hosted) => hosted.log.delete(),
            None => Ok(()),
        }
    }

    /// `thread` was started at `seated`'s seat: its row says so
    /// ([`slopty_proto::agent::SEAT_FACT`], and its facts) from now on, whatever its adapter
    /// tells of it,
    /// and the seat is kept in its directory, so the thread taken up again after a restart is
    /// given the same. A seat that cannot be written is warned of.
    ///
    /// In one step: the row says the seat on the meta as it stands, so nothing written of the
    /// thread meanwhile is lost.
    pub fn seated(&self, thread: ThreadId, seated: &Seated) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let Some(hosted) = inner.threads.get_mut(&thread) else { return };
        hosted.own.seat = Some(seated.seat.to_string());
        hosted.own.seated = Some(seated.clone());
        let dir = inner.dir.join(thread.to_string());
        if let Err(e) = write_seat(&dir, seated) {
            tracing::warn!(%thread, "its seat could not be kept: {e}");
        }
        let held = &hosted.log.state().meta;
        let mut meta = held.clone();
        hosted.own.meta(&mut meta);
        if meta != *held {
            apply(hosted, &mut inner.table, &inner.heard, vec![Action::Meta(Box::new(meta))]);
        }
    }

    /// Serve Slopty's tools through `cli mcp` to every agent started from now on, the person's
    /// own and seated ones alike.
    pub fn set_tools_cli(&self, cli: Option<String>) {
        self.inner.lock().tools_cli = cli;
    }

    /// Slopty's CLI whose `mcp` serves every agent Slopty's tools ([`Self::set_tools_cli`]).
    #[must_use]
    pub fn tools_cli(&self) -> Option<String> {
        self.inner.lock().tools_cli.clone()
    }

    /// The seat `thread` was started at, all of it, when it was ([`Self::seated`]).
    #[must_use]
    pub fn seat_of(&self, thread: ThreadId) -> Option<Seated> {
        self.inner.lock().hosted(thread)?.own.seated.clone()
    }

    /// The thread started at `seat`, if one is.
    #[must_use]
    pub fn seated_at(&self, seat: SessionId) -> Option<ThreadId> {
        let seat = seat.to_string();
        let inner = self.inner.lock();
        let open = inner.threads.iter().find(|(_, h)| h.own.seat.as_deref() == Some(seat.as_str()));
        let cold = || {
            inner.cold.iter().find(|(_, head)| {
                head.state.meta.facts.get(slopty_proto::agent::SEAT_FACT) == Some(&seat)
            })
        };
        open.map(|(thread, _)| *thread).or_else(|| cold().map(|(thread, _)| *thread))
    }

    /// Every tool call any thread's agent makes from now on, as it runs and once it ran.
    #[must_use]
    pub fn tools(&self) -> broadcast::Receiver<ToolSeen> {
        self.inner.lock().heard.tools.subscribe()
    }

    /// The outcome intent `id` for `thread` had, if it was acted on.
    #[must_use]
    pub fn outcome(&self, thread: ThreadId, id: IntentId) -> Option<Outcome> {
        self.inner.lock().hosted(thread)?.intents.outcome(&id).cloned()
    }

    /// How many follow `thread` now: clients' streams and anything else that watches it.
    #[must_use]
    pub fn followers(&self, thread: ThreadId) -> usize {
        self.inner.lock().threads.get(&thread).map_or(0, |h| h.feed.receiver_count())
    }

    /// Every batch `thread` applies from now on; `None` for a thread not held.
    #[must_use]
    pub fn watch(&self, thread: ThreadId) -> Option<broadcast::Receiver<Arc<Batch>>> {
        self.inner.lock().hosted(thread).map(|h| h.feed.subscribe())
    }

    /// Follow `thread` from `have`: what was missed, and every batch after it. `None` for a
    /// thread not held.
    #[must_use]
    pub fn follow(&self, thread: ThreadId, have: Option<Cursor>, turns: u32) -> Option<Follow> {
        let mut inner = self.inner.lock();
        let hosted = inner.hosted(thread)?;
        Some(Follow { catchup: hosted.log.since(have, turns), feed: hosted.feed.subscribe() })
    }

    /// Up to `turns` turns of `thread` before `before`.
    #[must_use]
    pub fn page(&self, thread: ThreadId, before: TurnId, turns: u32) -> Option<Page> {
        self.inner.lock().hosted(thread).map(|h| h.log.state().page(before, turns))
    }

    /// `thread` as it stands, and its cursor.
    #[must_use]
    pub fn state(&self, thread: ThreadId) -> Option<(ThreadState, Cursor)> {
        self.inner.lock().hosted(thread).map(|h| (h.log.state().clone(), h.log.cursor()))
    }

    /// The table rows a client holding `have` lacks.
    #[must_use]
    pub fn table(&self, have: Option<Cursor>) -> TableFrame {
        self.inner.lock().table.since(have)
    }

    /// Told whenever the table changes.
    #[must_use]
    pub fn table_watch(&self) -> watch::Receiver<Cursor> {
        self.inner.lock().table.watch()
    }

    /// Act on intent `id` for `thread` once. The first time, `act` decides from the thread's
    /// state what comes of it and which actions to apply; a repeat of the id gets that
    /// outcome back and acts on nothing. `None` for a thread not held.
    pub fn intent<F>(&self, thread: ThreadId, id: IntentId, act: F) -> Option<Outcome>
    where
        F: FnOnce(&ThreadState) -> (Outcome, Vec<Action>),
    {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let hosted = inner.threads.get_mut(&thread)?;
        if let Some(outcome) = hosted.intents.outcome(&id) {
            return Some(outcome.clone());
        }
        let (outcome, actions) = act(hosted.log.state());
        if !actions.is_empty() {
            apply(hosted, &mut inner.table, &inner.heard, actions);
        }
        if let Err(e) = hosted.intents.record(id, outcome.clone()) {
            tracing::warn!(%thread, "an intent could not be recorded: {e}");
        }
        Some(outcome)
    }

    /// The outcome intent `id` had as a start, if it was acted on.
    #[must_use]
    pub fn started(&self, id: IntentId) -> Option<Outcome> {
        self.inner.lock().starts.outcome(&id).cloned()
    }

    /// Note that intent `id` came to `outcome`, for a start whose thread began on its own (an
    /// shared agent's, which the adapter begins when the agent first speaks). A
    /// repeat of the id gets the first outcome back.
    pub fn record_start(&self, id: IntentId, outcome: Outcome) -> Outcome {
        let mut inner = self.inner.lock();
        inner.settle_under_way(id);
        if let Some(first) = inner.starts.outcome(&id) {
            return first.clone();
        }
        if let Err(e) = inner.starts.record(id, outcome.clone()) {
            tracing::warn!("a start could not be recorded: {e}");
        }
        outcome
    }

    /// Start `id`, as `start` asks, is under way: kept until its outcome is recorded
    /// ([`Self::record_start`]), across a restart too ([`Self::starts_under_way`]). One that
    /// cannot be written is warned of, and is held for this run alone.
    pub fn start_under_way(&self, id: IntentId, start: &Start) {
        let mut inner = self.inner.lock();
        if inner.starts.outcome(&id).is_some() || inner.under_way.iter().any(|u| u.id == id) {
            return;
        }
        inner.under_way.push(UnderWay { id, start: start.clone(), begun: false });
        inner.keep_under_way();
    }

    /// Start `id` is past its worktree's setup and opens its agent.
    pub fn start_begins(&self, id: IntentId) {
        let mut inner = self.inner.lock();
        let Some(under_way) = inner.under_way.iter_mut().find(|u| u.id == id) else { return };
        if under_way.begun {
            return;
        }
        under_way.begun = true;
        inner.keep_under_way();
    }

    /// The starts under way, those a worker before this one left included.
    #[must_use]
    pub fn starts_under_way(&self) -> Vec<UnderWay> {
        self.inner.lock().under_way.clone()
    }

    /// Start a thread for intent `id` once: `start` makes its metadata, or says why not. A
    /// repeat of the id gets the first outcome back and starts nothing.
    pub fn start<F>(&self, id: IntentId, start: F) -> Outcome
    where
        F: FnOnce() -> Result<ThreadMeta, String>,
    {
        let mut inner = self.inner.lock();
        if let Some(outcome) = inner.starts.outcome(&id) {
            return outcome.clone();
        }
        let outcome = match start() {
            Ok(meta) => {
                let thread = meta.id;
                match create(&mut inner, meta) {
                    Ok(_) => Outcome::Started { thread },
                    Err(e) => Outcome::Refused { reason: format!("Couldn't make its log: {e}") },
                }
            }
            Err(reason) => Outcome::Refused { reason },
        };
        if let Err(e) = inner.starts.record(id, outcome.clone()) {
            tracing::warn!("a start could not be recorded: {e}");
        }
        inner.settle_under_way(id);
        outcome
    }
}

/// The log in thread directory `path` did not read, for `e`. One that no longer decodes (an
/// older build wrote it) is deleted, with a warning: it would never read, its agent's own
/// session still has everything, and its adapter begins the thread anew. One the disk refused
/// is left for the next start.
fn unreadable(path: &Path, e: &io::Error) {
    if e.kind() != io::ErrorKind::InvalidData {
        tracing::warn!(path = %path.display(), "a thread's log is unreadable: {e}");
        return;
    }
    tracing::warn!(path = %path.display(), "a thread's log no longer reads, so it goes: {e}");
    if let Err(e) = std::fs::remove_dir_all(path) {
        tracing::warn!(path = %path.display(), "an unreadable thread's log could not go: {e}");
    }
}

/// Keep `seated` in thread directory `dir`, whole or not at all: it goes to a sibling that is
/// renamed into place.
fn write_seat(dir: &Path, seated: &Seated) -> io::Result<()> {
    write_atomic(dir, SEAT_FILE, seated)
}

/// Write `value` as `name` in `dir` whole or not at all.
fn write_atomic(dir: &Path, name: &str, value: &impl serde::Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let staging = dir.join(format!(".{name}"));
    std::fs::write(&staging, bytes)?;
    std::fs::rename(&staging, dir.join(name))
}

impl Inner {
    /// Start `id` has its outcome: it is under way no more.
    fn settle_under_way(&mut self, id: IntentId) {
        let before = self.under_way.len();
        self.under_way.retain(|u| u.id != id);
        if self.under_way.len() != before {
            self.keep_under_way();
        }
    }

    /// Write the starts under way, warning of a write that failed: they are held for this run
    /// alone then.
    fn keep_under_way(&self) {
        let kept = if self.under_way.is_empty() {
            match std::fs::remove_file(self.dir.join(UNDER_WAY_FILE)) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                gone => gone,
            }
        } else {
            write_atomic(&self.dir, UNDER_WAY_FILE, &self.under_way)
        };
        if let Err(e) = kept {
            tracing::warn!("the starts under way could not be kept: {e}");
        }
    }

    /// Open `thread`'s log if it is held and not opened yet, on its first use. One that cannot
    /// be read is let go, with a warning: its agent's own session still has everything.
    fn load(&mut self, thread: ThreadId) {
        if let Some(hosted) = self.threads.get_mut(&thread) {
            hosted.used = tokio::time::Instant::now();
            return;
        }
        let Some(head) = self.cold.remove(&thread) else { return };
        let dir = self.dir.join(thread.to_string());
        match Log::open(&dir, self.limits).and_then(|log| Hosted::open(&dir, log)) {
            Ok(hosted) => {
                self.table.put(hosted.log.state().row(head.row.updated_ms));
                self.threads.insert(thread, hosted);
            }
            Err(e) => {
                unreadable(&dir, &e);
                self.table.remove(thread);
            }
        }
    }

    /// `thread`, its log opened on its first use; `None` for a thread not held.
    fn hosted(&mut self, thread: ThreadId) -> Option<&mut Hosted> {
        self.load(thread);
        self.threads.get_mut(&thread)
    }
}

/// Host the new thread `meta` says, unless one is held as that thread already.
fn create(inner: &mut Inner, meta: ThreadMeta) -> io::Result<Cursor> {
    if inner.threads.contains_key(&meta.id) || inner.cold.contains_key(&meta.id) {
        let held = format!("thread {} is held already", meta.id);
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, held));
    }
    let dir = inner.dir.join(meta.id.to_string());
    let log = Log::create(&dir, ThreadState::new(meta), inner.limits)?;
    let cursor = log.cursor();
    inner.table.put(log.state().row(WallMs::now()));
    let hosted = Hosted::open(&dir, log)?;
    inner.threads.insert(hosted.log.id(), hosted);
    Ok(cursor)
}

/// Start `hosted`'s log over from `state`, under a new epoch: every follower gets a snapshot.
fn restart(hosted: &mut Hosted, table: &mut Table, mut state: ThreadState) -> io::Result<Cursor> {
    // The person's own marks (how far they have seen it, their name for it) are the worker's,
    // which no agent's session has: they stay. What the agent's TUI holds (its prompt box, its
    // queue) is told again by whoever reads the session.
    hosted.own.meta(&mut state.meta);
    state.seen = hosted.log.state().seen;
    state.named.clone_from(&hosted.log.state().named);
    hosted.log.reset(state)?;
    let cursor = hosted.log.cursor();
    let row = hosted.log.state().row(WallMs::now());
    if hosted.log.state().status.phase != Phase::Working
        && let Err(e) = hosted.log.write_head(&row)
    {
        tracing::debug!(thread = %hosted.log.id(), "a thread's head was not kept: {e}");
    }
    table.put(row);
    let _no_follower =
        hosted.feed.send(Arc::new(Batch { epoch: cursor.epoch, seq: 0, actions: Vec::new() }));
    Ok(cursor)
}

/// What the threads' logs are called when they cannot be written.
const LOGS: &str = "Thread logs";

/// The tool call `action` tells of, when it runs or has run in the thread's last turn: a log
/// read again from the start tells every old call too.
fn live_call(action: &Action, last: Option<TurnId>) -> Option<&ToolCall> {
    let (Action::ItemStarted(item) | Action::ItemUpdated(item) | Action::ItemCompleted(item)) =
        action
    else {
        return None;
    };
    let ItemBody::Tool(call) = &item.body else { return None };
    let ran = matches!(call.state, ToolState::Running | ToolState::Completed);
    (ran && Some(item.turn) == last).then_some(&**call)
}

fn apply(
    hosted: &mut Hosted,
    table: &mut Table,
    heard: &Heard,
    mut actions: Vec<Action>,
) -> Cursor {
    hosted.own.mark(&mut actions);
    let first = hosted.log.cursor();
    match hosted.log.append(&actions) {
        Ok(()) => crate::caps::wrote(LOGS),
        Err(e) => {
            tracing::warn!(thread = %hosted.log.id(), "a thread's log could not be written: {e}");
            crate::caps::not_written(LOGS, &e);
        }
    }
    let row = hosted.log.state().row(WallMs::now());
    // A thread at rest keeps its head current, so the next start reads it alone; one at work
    // writes it once it rests.
    if hosted.log.state().status.phase != Phase::Working
        && let Err(e) = hosted.log.write_head(&row)
    {
        tracing::debug!(thread = %hosted.log.id(), "a thread's head was not kept: {e}");
    }
    table.put(row);
    let thread = hosted.log.id();
    let state = hosted.log.state();
    let last = state.last_turn().map(|t| t.id);
    if heard.tools.receiver_count() > 0 {
        for call in actions.iter().filter_map(|action| live_call(action, last)) {
            let _nobody = heard.tools.send(ToolSeen { thread, call: Arc::new(call.clone()) });
        }
    }
    let _no_follower =
        hosted.feed.send(Arc::new(Batch { epoch: first.epoch, seq: first.seq, actions }));
    hosted.log.cursor()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use slopty_proto::thread::Drive;

    use super::*;

    fn meta(title: &str) -> ThreadMeta {
        ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: "2.1.314".to_owned(),
            native: "s1".to_owned(),
            cwd: "/work".to_owned(),
            title: title.to_owned(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::OBSERVED),
            facts: BTreeMap::new(),
            created_ms: WallMs::from_millis(1),
        }
    }

    fn named(meta: &ThreadMeta, fact: &str, value: &str) -> ThreadMeta {
        let mut meta = meta.clone();
        meta.facts.insert(fact.to_owned(), value.to_owned());
        meta
    }

    fn held(host: &Host, thread: ThreadId) -> ThreadState {
        host.state(thread).expect("held").0
    }

    /// A thread is never made over one held: making it again is refused and leaves the one held
    /// as it was, opened or not opened yet; one held already is changed instead in one step
    /// with the look, which makes it only when none is held.
    #[test]
    fn a_thread_is_never_made_over_one_held() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let first = meta("First");
        let thread = first.id;
        host.create(first.clone()).unwrap();
        host.apply(thread, vec![Action::Meta(Box::new(named(&first, "kept", "yes")))]);
        let again = ThreadMeta { title: "Second".to_owned(), ..first };
        let refused = host.create(again.clone()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        let state = held(&host, thread);
        assert_eq!(state.meta.title, "First", "the one held stays");
        assert_eq!(state.meta.facts.get("kept").map(String::as_str), Some("yes"));
        drop(host);

        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let refused = host.create(again.clone()).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "not opened yet");
        assert_eq!(held(&host, thread).meta.title, "First");

        let looked = host.create_or_update(again, |state| {
            let renamed = ThreadMeta { title: "Changed".to_owned(), ..state.meta.clone() };
            (vec![Action::Meta(Box::new(renamed))], state.meta.title.clone())
        });
        assert_eq!(looked.unwrap(), Some("First".to_owned()), "the one held, changed");
        assert_eq!(held(&host, thread).meta.title, "Changed");
        let fresh = meta("Fresh");
        let made = host.create_or_update(fresh.clone(), |_| -> (Vec<Action>, ()) {
            panic!("none is held to change")
        });
        assert_eq!(made.unwrap(), None, "made");
        assert_eq!(held(&host, fresh.id).meta, fresh);
    }

    /// A thread begun anew from its agent's records is made as they say when none is held;
    /// else its log starts over, under a new epoch, from what they say, its meta what the caller
    /// makes of the one held, the person's marks kept.
    #[test]
    fn a_thread_begun_anew_keeps_what_its_caller_keeps_of_the_one_held() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let told = meta("Told");
        let thread = told.id;
        host.begin(ThreadState::new(told.clone()), |_| panic!("none is held")).unwrap();
        assert_eq!(held(&host, thread).meta, told, "made as they say");

        let worker = ThreadMeta { cwd: "/moved".to_owned(), ..named(&told, "terminal", "t1") };
        host.apply(thread, vec![Action::Meta(Box::new(worker.clone()))]);
        let before = host.state(thread).unwrap().1;
        let retold = ThreadMeta { title: "Retold".to_owned(), ..told };
        let kept = host
            .begin(ThreadState::new(retold), |held| ThreadMeta {
                title: "Retold".to_owned(),
                ..held.clone()
            })
            .unwrap();
        assert_ne!(kept.epoch, before.epoch, "started over");
        let state = held(&host, thread);
        assert_eq!(state.meta, ThreadMeta { title: "Retold".to_owned(), ..worker });
    }

    /// A thread seated says its seat on its meta as it stands, everything written of it kept,
    /// in one batch; seated again the same, nothing is written.
    #[test]
    fn a_seat_is_said_on_the_meta_as_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(dir.path(), Limits::default()).unwrap();
        let started = meta("Started");
        let thread = started.id;
        host.create(started.clone()).unwrap();
        let worker = named(&started, ThreadMeta::TERMINAL_FACT, "t1");
        host.apply(thread, vec![Action::Meta(Box::new(worker))]);
        let mut feed = host.watch(thread).unwrap();
        let seated = Seated {
            seat: SessionId::new(),
            facts: vec![("parent".to_owned(), "p1".to_owned())],
            system: None,
        };
        host.seated(thread, &seated);
        let batch = feed.try_recv().expect("one batch");
        let [Action::Meta(said)] = batch.actions.as_slice() else { panic!("{batch:?}") };
        let facts = |meta: &ThreadMeta, name: &str| meta.facts.get(name).cloned();
        assert_eq!(facts(said, ThreadMeta::TERMINAL_FACT).as_deref(), Some("t1"));
        assert_eq!(facts(said, "parent").as_deref(), Some("p1"));
        let seat = Some(seated.seat.to_string());
        assert_eq!(facts(said, slopty_proto::agent::SEAT_FACT), seat);
        assert_eq!(held(&host, thread).meta, **said);
        host.seated(thread, &seated);
        assert!(feed.try_recv().is_err(), "nothing more");
    }
}
