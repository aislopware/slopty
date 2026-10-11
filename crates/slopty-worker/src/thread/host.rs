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

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::wire::{Outcome, Page, Start, TableFrame, ThreadRow};
use slopty_proto::thread::{
    Action, AgentId, Cursor, Edge, Fork, IntentId, ItemBody, ItemId, Pending, PendingState, Phase,
    ThreadId, ThreadMeta, ThreadState, ToolCall, ToolState, TreeRef, TurnId,
};
use tokio::sync::{broadcast, watch};

use super::intents::Intents;
use super::log::{Catchup, Head, Limits, Log};
use super::table::Table;
use super::{SeatEnv, Seated};

/// Batches a slow follower may fall behind by before it catches up from the log instead.
const FEED_BATCHES: usize = 1024;

/// Turn edges a slow listener may fall behind by; one that lags misses those snapshots.
const EDGES: usize = 256;

/// A moment of a thread's work a snapshot is taken at ([`Host::edges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnEdge {
    /// The thread.
    pub thread: ThreadId,
    /// What happened.
    pub moment: Moment,
}

/// What a [`TurnEdge`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// The agent started working: the earliest word of a turn, which some adapters have
    /// before the turn itself shows.
    Busy,
    /// A turn began, as the thread's last.
    Began(TurnId),
    /// A turn ended, as the thread's last.
    Ended(TurnId),
}

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
    /// Turn edges ([`Host::edges`]).
    edges: broadcast::Sender<TurnEdge>,
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
    /// What the worker adds to a seat's variables, once the daemon says.
    seat_env: Option<EnvOf>,
    /// Told whenever a scheduled message is added or changed ([`Host::schedule_changed`]).
    scheduling: Arc<tokio::sync::Notify>,
    /// The threads whose start is held at Claude Code's trust dialog ([`Host::hold_start`]).
    held: Vec<ThreadId>,
    /// The starts under way, as kept ([`Host::start_under_way`]).
    under_way: Vec<UnderWay>,
}

/// A [`SeatEnv`], which shows as nothing more.
struct EnvOf(SeatEnv);

impl std::fmt::Debug for EnvOf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeatEnv")
    }
}

/// The file in a thread's directory that keeps the seat it was started at ([`Seated`]).
const SEAT_FILE: &str = "seat.json";

/// The file in a thread's directory that keeps its start while Claude Code is held at its
/// trust dialog ([`HeldStart`]).
const HELD_START_FILE: &str = "held-start.json";

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

/// A Claude Code start held at its trust dialog, kept in its thread's directory so the person's
/// "Trust this folder" still opens it again after the worker restarts ([`Host::hold_start`]).
///
/// It holds nothing secret: what the start gave `claude` (the session, the model, the mode, the
/// first message, a seat's role), never the relay, a token or a seat's variables, which are
/// given afresh where it opens again.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct HeldStart {
    /// What `claude` was opened with.
    pub args: Vec<String>,
    /// The session it was opened on.
    pub native: String,
    /// The folder it was opened in.
    pub cwd: String,
}

#[derive(Debug)]
struct Hosted {
    log: Log,
    intents: Intents,
    feed: broadcast::Sender<Arc<Batch>>,
    own: Own,
}

impl Hosted {
    fn open(dir: &Path, log: Log) -> io::Result<Self> {
        let mut log = log;
        let mut own = Own::of(log.state());
        let mut pending = log.state().pending.clone();
        own.pending(&mut pending);
        if pending != log.state().pending {
            log.append(&[Action::PendingSet(pending)])?;
        }
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
        })
    }
}

/// What the worker adds to a thread that its adapter never tells: which message an intent
/// sent, and each turn's snapshots. A thread read again from its agent's session
/// ([`Host::reset`]) gets them back as the adapter tells its items and turns again.
#[derive(Debug, Default)]
struct Own {
    /// Messages typed and not yet seen in the thread: the intent and the words.
    typed: VecDeque<(IntentId, String)>,
    /// The intent each of the person's items came from.
    sent: HashMap<ItemId, IntentId>,
    /// Each turn's snapshots.
    trees: HashMap<TurnId, (Option<TreeRef>, Option<TreeRef>)>,
    /// Where the thread branched, when the worker forked it: an agent that records a fork names
    /// only the thread it came from, or nothing.
    fork: Option<Fork>,
    /// The seat a server's task started it at ([`slopty_proto::project::SEAT_FACT`]): an adapter
    /// that tells the thread's metadata again knows nothing of it.
    seat: Option<String>,
    /// The seat it was started at, all of it, kept in its directory ([`SEAT_FILE`]).
    seated: Option<Seated>,
    /// The thread it is an aside of, while it is one ([`ThreadMeta::ASIDE_FACT`]).
    aside: Option<ThreadId>,
    /// The messages the worker holds until their moment ([`super::schedule`]), which no
    /// adapter knows: put back in every pending list an adapter tells.
    scheduled: Vec<Pending>,
    /// The start's first message, given on the agent's command line: on its way in the
    /// pending list until the agent's item for it shows ([`Host::first_message`]).
    first: Option<IntentId>,
}

/// Messages typed whose items are waited for; past it the oldest is given up.
const TYPED: usize = 64;

impl Own {
    fn of(state: &ThreadState) -> Self {
        let sent = state
            .items
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::User(message) => message.intent.map(|intent| (item.id.clone(), intent)),
                _ => None,
            })
            .collect();
        let trees = state
            .turns
            .iter()
            .filter(|t| t.before.is_some() || t.after.is_some())
            .map(|t| (t.id, (t.before.clone(), t.after.clone())))
            .collect();
        let fork = state.meta.forked_from;
        let seat = state.meta.facts.get(slopty_proto::project::SEAT_FACT).cloned();
        let aside = state.meta.aside_of();
        // One going as the worker stopped is never sent again.
        let scheduled = state
            .pending
            .iter()
            .filter(|p| p.delivery.is_kept())
            .map(|p| match p.state {
                PendingState::Sending => Pending {
                    state: PendingState::Held { reason: super::schedule::CUT_OFF.to_owned() },
                    ..p.clone()
                },
                _ => p.clone(),
            })
            .collect();
        Self {
            typed: VecDeque::new(),
            sent,
            trees,
            fork,
            seat,
            seated: None,
            aside,
            scheduled,
            first: None,
        }
    }

    /// The start's first message, once `actions` show the agent's item for it: it leaves the
    /// pending list.
    fn took_first(&mut self, actions: &[Action]) -> Option<IntentId> {
        let first = self.first?;
        let shown = actions.iter().any(|action| {
            matches!(
                action,
                Action::ItemStarted(item) | Action::ItemUpdated(item) | Action::ItemCompleted(item)
                    if matches!(&item.body, ItemBody::User(m) if m.intent == Some(first))
            )
        });
        shown.then(|| self.first.take()).flatten()
    }

    /// `pending`, an adapter's list, with the messages the worker holds put back after it.
    fn pending(&self, pending: &mut Vec<Pending>) {
        pending.retain(|p| !p.delivery.is_kept());
        pending.extend(self.scheduled.iter().cloned());
    }

    /// `meta` with what the worker knows of it: where it branched, the seat it was started at,
    /// whether it is an aside.
    fn meta(&self, meta: &mut ThreadMeta) {
        self.branched(meta);
        if let Some(seat) = &self.seat {
            meta.facts.insert(slopty_proto::project::SEAT_FACT.to_owned(), seat.clone());
        }
        match self.aside {
            Some(of) => meta.facts.insert(ThreadMeta::ASIDE_FACT.to_owned(), of.to_string()),
            None => meta.facts.remove(ThreadMeta::ASIDE_FACT),
        };
    }

    /// `meta` with where the worker branched it, unless its agent says more: an agent that
    /// records a fork knows the thread it came from, not the turn.
    fn branched(&self, meta: &mut ThreadMeta) {
        let Some(fork) = self.fork else { return };
        if meta.forked_from.is_none_or(|t| t.thread == fork.thread && t.turn.is_none()) {
            meta.forked_from = Some(fork);
            ThreadMeta::FORK.clone_into(&mut meta.origin);
        }
    }

    /// `actions` with what the worker knows put back in: an item's intent, a turn's trees,
    /// where the thread branched.
    fn mark(&mut self, actions: &mut [Action]) {
        for action in actions {
            match action {
                Action::Meta(meta) => self.meta(meta),
                Action::PendingSet(pending) => self.pending(pending),
                Action::ItemStarted(item)
                | Action::ItemUpdated(item)
                | Action::ItemCompleted(item) => {
                    let ItemBody::User(message) = &mut item.body else { continue };
                    if let Some(intent) = message.intent {
                        self.sent.insert(item.id.clone(), intent);
                        continue;
                    }
                    message.intent = self.sent.get(&item.id).copied().or_else(|| {
                        let words = message.text.text.trim();
                        let at = self.typed.iter().position(|(_, typed)| typed.trim() == words)?;
                        let (intent, _) = self.typed.remove(at)?;
                        self.sent.insert(item.id.clone(), intent);
                        Some(intent)
                    });
                }
                Action::TurnStarted(turn) => {
                    if let Some((before, after)) = self.trees.get(&turn.id) {
                        turn.before = turn.before.take().or_else(|| before.clone());
                        turn.after = turn.after.take().or_else(|| after.clone());
                    }
                }
                Action::Snapshot { turn, edge, tree } => {
                    let trees = self.trees.entry(*turn).or_default();
                    match edge {
                        Edge::Before => trees.0 = Some(tree.clone()),
                        Edge::After => trees.1 = Some(tree.clone()),
                    }
                }
                _ => {}
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
        let mut held = Vec::new();
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
            let held_start = path.join(HELD_START_FILE).is_file();
            if let Some(head) = Log::head(&path) {
                table.put(head.row.clone());
                if held_start {
                    held.push(head.row.id);
                }
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
                    if held_start {
                        held.push(row.id);
                    }
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
                Err(e) => {
                    tracing::warn!(path = %path.display(), "a thread's log is unreadable: {e}");
                }
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
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_owned(),
                limits,
                threads: HashMap::new(),
                cold,
                table,
                starts,
                heard: Heard {
                    edges: broadcast::Sender::new(EDGES),
                    tools: broadcast::Sender::new(TOOLS),
                },
                seat_env: None,
                scheduling: Arc::default(),
                held,
                under_way,
            })),
        })
    }

    /// Host a new thread.
    ///
    /// # Errors
    ///
    /// When its log cannot be made.
    pub fn create(&self, meta: ThreadMeta) -> io::Result<Cursor> {
        create(&mut self.inner.lock(), meta)
    }

    /// Whether `thread` is held.
    #[must_use]
    pub fn holds(&self, thread: ThreadId) -> bool {
        let inner = self.inner.lock();
        inner.threads.contains_key(&thread) || inner.cold.contains_key(&thread)
    }

    /// How many threads have their logs open: those used since the worker started.
    #[must_use]
    pub fn opened(&self) -> usize {
        self.inner.lock().threads.len()
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
    /// disk ([`Self::visit_whole`] reads them).
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

    /// [`Self::visit`] with every thread whole, its items too: a thread not opened yet is read
    /// from its log for the look alone and stays unopened. The reading is done off the lock.
    pub fn visit_whole<T>(&self, mut see: impl FnMut(&ThreadState) -> Option<T>) -> Vec<T> {
        self.threads()
            .into_iter()
            .filter_map(|thread| {
                let inner = self.inner.lock();
                if let Some(hosted) = inner.threads.get(&thread) {
                    return see(hosted.log.state());
                }
                let dir = inner.dir.join(thread.to_string());
                drop(inner);
                match Log::read(&dir) {
                    Ok(state) => see(&state),
                    Err(e) => {
                        tracing::debug!(%thread, "a thread's log did not read: {e}");
                        None
                    }
                }
            })
            .collect()
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
        // What waits to be sent, whether the tree is to review and the person's own marks
        // (how far they have seen it, what they were writing) are the worker's, which no
        // agent's session has: they stay.
        // One that was being typed may be in the terminal already, so it is not typed again.
        let mut state = state;
        hosted.own.meta(&mut state.meta);
        state.pending.clone_from(&hosted.log.state().pending);
        state.to_review = hosted.log.state().to_review;
        state.seen = hosted.log.state().seen;
        state.draft.clone_from(&hosted.log.state().draft);
        let first = hosted.own.first;
        for pending in state.pending.iter_mut().filter(|p| Some(p.intent) != first) {
            if pending.state == PendingState::Sending {
                pending.state =
                    PendingState::Held { reason: super::compose::TYPED_NOT_SENT.to_owned() };
            }
        }
        hosted.log.reset(state)?;
        let cursor = hosted.log.cursor();
        let row = hosted.log.state().row(WallMs::now());
        if hosted.log.state().status.phase != Phase::Working
            && let Err(e) = hosted.log.write_head(&row)
        {
            tracing::debug!(%thread, "a thread's head was not kept: {e}");
        }
        inner.table.put(row);
        let _no_follower =
            hosted.feed.send(Arc::new(Batch { epoch: cursor.epoch, seq: 0, actions: Vec::new() }));
        Ok(Some(cursor))
    }

    /// Stop hosting `thread` and delete its log. Its followers' feeds close.
    ///
    /// # Errors
    ///
    /// When its files cannot be removed.
    pub fn remove(&self, thread: ThreadId) -> io::Result<()> {
        let mut inner = self.inner.lock();
        inner.table.remove(thread);
        inner.held.retain(|t| *t != thread);
        if inner.cold.remove(&thread).is_some() {
            return std::fs::remove_dir_all(inner.dir.join(thread.to_string()));
        }
        match inner.threads.remove(&thread) {
            Some(hosted) => hosted.log.delete(),
            None => Ok(()),
        }
    }

    /// `thread` is an aside of thread `of` from now on, across a restart and a read again from
    /// its agent; with `None`, an ordinary thread again. Whether it is held.
    pub fn aside(&self, thread: ThreadId, of: Option<ThreadId>) -> bool {
        let meta = {
            let mut inner = self.inner.lock();
            inner.load(thread);
            let Some(hosted) = inner.threads.get_mut(&thread) else { return false };
            hosted.own.aside = of;
            hosted.log.state().meta.clone()
        };
        self.apply(thread, vec![Action::Meta(Box::new(meta))]);
        true
    }

    /// `thread` was branched off another as `fork` says, by the worker: kept with it from now on,
    /// whatever its adapter tells of it, across a restart and a read again from its agent.
    pub fn forked(&self, thread: ThreadId, fork: Fork) {
        let meta = {
            let mut inner = self.inner.lock();
            inner.load(thread);
            let Some(hosted) = inner.threads.get_mut(&thread) else { return };
            hosted.own.fork = Some(fork);
            hosted.log.state().meta.clone()
        };
        self.apply(thread, vec![Action::Meta(Box::new(meta))]);
    }

    /// `thread` was started at `seated`'s seat for a server's task: its row says so
    /// ([`slopty_proto::project::SEAT_FACT`]) from now on, whatever its adapter tells of it,
    /// and the seat is kept in its directory, so the thread taken up again after a restart is
    /// given the same ([`Self::seated_of`]). A seat that cannot be written is warned of.
    pub fn seated(&self, thread: ThreadId, seated: &Seated) {
        let meta = {
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
            hosted.log.state().meta.clone()
        };
        self.apply(thread, vec![Action::Meta(Box::new(meta))]);
    }

    /// `agent`'s threads in folder `cwd`, by their own sessions' ids, each with when it began:
    /// as their heads say, so no log is opened.
    #[must_use]
    pub fn begun_in(&self, agent: &AgentId, cwd: &str) -> Vec<(String, WallMs)> {
        let inner = self.inner.lock();
        let open = inner.threads.values().map(|hosted| &hosted.log.state().meta);
        let cold = inner.cold.values().map(|head| &head.state.meta);
        open.chain(cold)
            .filter(|meta| meta.agent == *agent && meta.cwd == cwd)
            .map(|meta| (meta.native.clone(), meta.created_ms))
            .collect()
    }

    /// `thread`'s start is held at Claude Code's trust dialog: kept in its directory until it is
    /// taken ([`Self::take_held_start`]) or the thread goes. One that cannot be written is
    /// warned of, and is kept for this run alone.
    pub fn hold_start(&self, thread: ThreadId, start: &HeldStart) {
        let mut inner = self.inner.lock();
        inner.load(thread);
        if !inner.threads.contains_key(&thread) {
            return;
        }
        let dir = inner.dir.join(thread.to_string());
        if let Err(e) = write_atomic(&dir, HELD_START_FILE, start) {
            tracing::warn!(%thread, "its held start could not be kept: {e}");
        }
        if !inner.held.contains(&thread) {
            inner.held.push(thread);
        }
    }

    /// The start `thread` holds at Claude Code's trust dialog, taken: it is kept no more.
    #[must_use]
    pub fn take_held_start(&self, thread: ThreadId) -> Option<HeldStart> {
        let mut inner = self.inner.lock();
        let at = inner.held.iter().position(|t| *t == thread)?;
        inner.held.remove(at);
        let path = inner.dir.join(thread.to_string()).join(HELD_START_FILE);
        drop(inner);
        let start = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%thread, "its held start could not be let go: {e}");
        }
        start
    }

    /// Every start held at Claude Code's trust dialog, as kept: across a restart too.
    #[must_use]
    pub fn held_starts(&self) -> Vec<(ThreadId, HeldStart)> {
        let (dir, held) = {
            let inner = self.inner.lock();
            (inner.dir.clone(), inner.held.clone())
        };
        held.into_iter()
            .filter_map(|thread| {
                let bytes =
                    std::fs::read(dir.join(thread.to_string()).join(HELD_START_FILE)).ok()?;
                Some((thread, serde_json::from_slice(&bytes).ok()?))
            })
            .collect()
    }

    /// The seat `thread` was started at, if it was ([`Self::seated`]).
    #[must_use]
    pub fn seated_of(&self, thread: ThreadId) -> Option<Seated> {
        self.inner.lock().hosted(thread)?.own.seated.clone()
    }

    /// Give the worker's own variables for a seat to every seated agent from now on.
    pub fn set_seat_env(&self, env: SeatEnv) {
        self.inner.lock().seat_env = Some(EnvOf(env));
    }

    /// Every variable `seated`'s agent and its Slopty tools run with: the server's, with the
    /// worker's own for the seat ([`Self::set_seat_env`]) where the daemon gave them.
    #[must_use]
    pub fn env_of(&self, seated: &Seated) -> Vec<(String, String)> {
        let env = self.inner.lock().seat_env.as_ref().map(|EnvOf(env)| Arc::clone(env));
        env.map_or_else(|| seated.env.clone(), |env| env(seated.seat, &seated.env))
    }

    /// The thread started at `seat`, if one is.
    #[must_use]
    pub fn seated_at(&self, seat: SessionId) -> Option<ThreadId> {
        let seat = seat.to_string();
        let inner = self.inner.lock();
        let open = inner.threads.iter().find(|(_, h)| h.own.seat.as_deref() == Some(seat.as_str()));
        let cold = || {
            inner.cold.iter().find(|(_, head)| {
                head.state.meta.facts.get(slopty_proto::project::SEAT_FACT) == Some(&seat)
            })
        };
        open.map(|(thread, _)| *thread).or_else(|| cold().map(|(thread, _)| *thread))
    }

    /// Message `text` was typed for intent `id` into `thread`'s agent: the item the agent
    /// makes of it is marked with the intent ([`slopty_proto::thread::UserMessage::intent`]).
    pub fn typed(&self, thread: ThreadId, id: IntentId, text: &str) {
        let mut inner = self.inner.lock();
        inner.load(thread);
        let Some(hosted) = inner.threads.get_mut(&thread) else { return };
        let typed = &mut hosted.own.typed;
        if typed.len() >= TYPED {
            typed.pop_front();
        }
        typed.push_back((id, text.to_owned()));
    }

    /// The first message of the start that opened `thread`'s agent, `pending`, given on the
    /// agent's command line rather than typed: it shows in the thread's pending list, on its way,
    /// from the start until the agent's item for it shows, so a client sees what was sent while
    /// the agent is still opening (or held at a dialog of its own). Typed text matches it to its
    /// item as [`Self::typed`] says.
    pub fn first_message(&self, thread: ThreadId, pending: Pending) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let Some(hosted) = inner.threads.get_mut(&thread) else { return };
        hosted.own.first = Some(pending.intent);
        let mut list = hosted.log.state().pending.clone();
        list.retain(|p| p.intent != pending.intent);
        list.insert(0, pending);
        apply(hosted, &mut inner.table, &inner.heard, vec![Action::PendingSet(list)]);
    }

    /// Every turn edge any thread reaches from now on.
    #[must_use]
    pub fn edges(&self) -> broadcast::Receiver<TurnEdge> {
        self.inner.lock().heard.edges.subscribe()
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

    /// The row of the thread whose agent runs in terminal `session`, hanging from no other:
    /// the latest to change, when several did.
    #[must_use]
    pub fn at_terminal(&self, session: SessionId) -> Option<ThreadRow> {
        self.inner.lock().table.at_terminal(session).cloned()
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

    /// Change `thread`'s scheduled messages for intent `id` once ([`super::schedule::act`]):
    /// `change` decides from the thread's state what comes of it and edits the list the worker
    /// holds, which goes into the thread's pending list and log. `None` for a thread not held.
    pub fn schedule<F>(&self, thread: ThreadId, id: IntentId, change: F) -> Option<Outcome>
    where
        F: FnOnce(&ThreadState, &mut Vec<Pending>) -> Outcome,
    {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.load(thread);
        let hosted = inner.threads.get_mut(&thread)?;
        if let Some(outcome) = hosted.intents.outcome(&id) {
            return Some(outcome.clone());
        }
        let mut scheduled = hosted.own.scheduled.clone();
        let outcome = change(hosted.log.state(), &mut scheduled);
        if scheduled != hosted.own.scheduled {
            hosted.own.scheduled = scheduled;
            let pending = hosted.log.state().pending.clone();
            apply(hosted, &mut inner.table, &inner.heard, vec![Action::PendingSet(pending)]);
            inner.scheduling.notify_one();
        }
        if let Err(e) = hosted.intents.record(id, outcome.clone()) {
            tracing::warn!(%thread, "an intent could not be recorded: {e}");
        }
        Some(outcome)
    }

    /// Whether message `intent` waits on the worker in `thread` ([`Self::schedule`]).
    #[must_use]
    pub fn is_scheduled(&self, thread: ThreadId, intent: IntentId) -> bool {
        self.inner
            .lock()
            .hosted(thread)
            .is_some_and(|h| h.own.scheduled.iter().any(|p| p.intent == intent))
    }

    /// Told whenever a scheduled message is added or changed.
    #[must_use]
    pub fn schedule_changed(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.inner.lock().scheduling)
    }

    /// The scheduled messages whose moment has come at `now`, each marked as going, and when
    /// the next one may come. Each goes queued where its thread's agent queues, else as a steer.
    pub fn due(&self, now: WallMs) -> (Vec<super::schedule::Due>, Option<WallMs>) {
        use super::schedule::{Due, When, when};
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        // A thread not opened yet that holds a message for later is opened to send it.
        let holding: Vec<ThreadId> = inner
            .cold
            .iter()
            .filter(|(_, head)| head.state.pending.iter().any(|p| p.delivery.is_kept()))
            .map(|(thread, _)| *thread)
            .collect();
        for thread in holding {
            inner.load(thread);
        }
        let mut next: Option<WallMs> = None;
        let mut moved: Vec<(ThreadId, IntentId, PendingState)> = Vec::new();
        let mut due = Vec::new();
        for (thread, hosted) in &inner.threads {
            for pending in hosted.own.scheduled.iter().filter(|p| p.state == PendingState::Waiting)
            {
                match when(pending, now) {
                    When::Now => {
                        let meta = &hosted.log.state().meta;
                        let delivery = meta.delivery_after_turn();
                        let send = slopty_proto::thread::wire::Intent::Send {
                            text: pending.text.clone(),
                            attachments: pending.attachments.clone(),
                            delivery,
                        };
                        due.push(Due { thread: *thread, intent: pending.intent, send });
                        moved.push((*thread, pending.intent, PendingState::Sending));
                    }
                    When::At(at) => next = Some(next.map_or(at, |n| n.min(at))),
                    When::Later => {}
                }
            }
        }
        for (thread, intent, state) in moved {
            set_scheduled(inner, thread, intent, |p| p.state = state);
        }
        (due, next)
    }

    /// Scheduled message `intent` of `thread` went with `outcome`: gone from the list once
    /// its agent took it, else held there with why not.
    pub fn fired(&self, thread: ThreadId, intent: IntentId, outcome: &Outcome) {
        let mut inner = self.inner.lock();
        match outcome {
            Outcome::Done | Outcome::Accepted | Outcome::Started { .. } => {
                if let Some(hosted) = inner.hosted(thread) {
                    hosted.own.scheduled.retain(|p| p.intent != intent);
                }
                set_scheduled(&mut inner, thread, intent, |_| {});
            }
            Outcome::Refused { reason } => {
                let held = PendingState::Held { reason: reason.clone() };
                set_scheduled(&mut inner, thread, intent, |p| p.state = held);
            }
            Outcome::Unsupported { cap } => {
                let held = PendingState::Held { reason: format!("Its agent cannot {}", cap.0) };
                set_scheduled(&mut inner, thread, intent, |p| p.state = held);
            }
            Outcome::SetupFailed { setup, .. } => {
                let reason = format!("Its setup from {} failed", setup.from);
                set_scheduled(&mut inner, thread, intent, |p| {
                    p.state = PendingState::Held { reason }
                });
            }
        }
    }

    /// The outcome intent `id` had as a start, if it was acted on.
    #[must_use]
    pub fn started(&self, id: IntentId) -> Option<Outcome> {
        self.inner.lock().starts.outcome(&id).cloned()
    }

    /// Note that intent `id` came to `outcome`, for a start whose thread began on its own (an
    /// observed or shared agent's, which the adapter begins when the agent first speaks). A
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
                    Err(e) => {
                        Outcome::Refused { reason: format!("its log could not be made: {e}") }
                    }
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

/// Change scheduled message `intent` of `thread` as `change` says, and tell the thread's
/// pending list.
fn set_scheduled(
    inner: &mut Inner,
    thread: ThreadId,
    intent: IntentId,
    change: impl FnOnce(&mut Pending),
) {
    inner.load(thread);
    let Some(hosted) = inner.threads.get_mut(&thread) else { return };
    if let Some(pending) = hosted.own.scheduled.iter_mut().find(|p| p.intent == intent) {
        change(pending);
    }
    let pending = hosted.log.state().pending.clone();
    apply(hosted, &mut inner.table, &inner.heard, vec![Action::PendingSet(pending)]);
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
        let Some(head) = self.cold.remove(&thread) else { return };
        let dir = self.dir.join(thread.to_string());
        match Log::open(&dir, self.limits).and_then(|log| Hosted::open(&dir, log)) {
            Ok(hosted) => {
                self.table.put(hosted.log.state().row(head.row.updated_ms));
                self.threads.insert(thread, hosted);
            }
            Err(e) => {
                tracing::warn!(%thread, "a thread's log is unreadable: {e}");
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

fn create(inner: &mut Inner, meta: ThreadMeta) -> io::Result<Cursor> {
    let dir = inner.dir.join(meta.id.to_string());
    let log = Log::create(&dir, ThreadState::new(meta), inner.limits)?;
    let cursor = log.cursor();
    inner.table.put(log.state().row(WallMs::now()));
    let hosted = Hosted::open(&dir, log)?;
    inner.threads.insert(hosted.log.id(), hosted);
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
    let edges = &heard.edges;
    hosted.own.mark(&mut actions);
    if let Some(first) = hosted.own.took_first(&actions) {
        let told = actions.iter().rev().find_map(|action| match action {
            Action::PendingSet(pending) => Some(pending.clone()),
            _ => None,
        });
        let mut pending = told.unwrap_or_else(|| hosted.log.state().pending.clone());
        pending.retain(|p| p.intent != first);
        actions.push(Action::PendingSet(pending));
    }
    let first = hosted.log.cursor();
    let was_working = hosted.log.state().status.phase == Phase::Working;
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
    if !was_working && state.status.phase == Phase::Working {
        let _nobody = edges.send(TurnEdge { thread, moment: Moment::Busy });
    }
    // Only the last turn's edges are the agent's now: a log read again from the start (a
    // restart) tells every old turn too, and the tree is long past those.
    let last = state.last_turn().map(|t| t.id);
    for action in &actions {
        let moment = match action {
            Action::TurnStarted(turn) => Some(Moment::Began(turn.id)),
            Action::TurnEnded { turn, .. } => Some(Moment::Ended(*turn)),
            _ => None,
        };
        let live = |turn: TurnId| Some(turn) == last;
        if let Some(moment) =
            moment.filter(|m| matches!(m, Moment::Began(t) | Moment::Ended(t) if live(*t)))
        {
            let _nobody = edges.send(TurnEdge { thread, moment });
        }
    }
    if heard.tools.receiver_count() > 0 {
        for call in actions.iter().filter_map(|action| live_call(action, last)) {
            let _nobody = heard.tools.send(ToolSeen { thread, call: Arc::new(call.clone()) });
        }
    }
    let _no_follower =
        hosted.feed.send(Arc::new(Batch { epoch: first.epoch, seq: first.seq, actions }));
    hosted.log.cursor()
}
