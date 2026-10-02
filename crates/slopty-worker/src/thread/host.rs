//! The thread host: every thread this worker holds, their logs, the table and the intents.
//!
//! One lock guards it all. What happens under it is small: applying actions, appending them
//! to a file, and a snapshot written at a turn's end once in a while. A follower subscribes
//! and takes its catch-up under the same lock, so it misses nothing and is sent nothing twice
//! ([`Host::follow`]).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Outcome, Page, TableFrame};
use slopty_proto::thread::{
    Action, Cursor, Edge, IntentId, ItemBody, ItemId, PendingState, Phase, ThreadId, ThreadMeta,
    ThreadState, TreeRef, TurnId,
};
use tokio::sync::{broadcast, watch};

use super::intents::Intents;
use super::log::{Catchup, Limits, Log};
use super::table::Table;

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
    threads: HashMap<ThreadId, Hosted>,
    table: Table,
    /// Intents that start a thread, which have no thread of their own to be kept with yet.
    starts: Intents,
    edges: broadcast::Sender<TurnEdge>,
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
        let own = Own::of(log.state());
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
        Self { typed: VecDeque::new(), sent, trees }
    }

    /// `actions` with what the worker knows put back in: an item's intent, a turn's trees.
    fn mark(&mut self, actions: &mut [Action]) {
        for action in actions {
            match action {
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
    /// When `dir` cannot be made or read.
    pub fn open(dir: &Path, limits: Limits) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut threads = HashMap::new();
        let mut table = Table::new(ThreadId::new().as_uuid().as_u64_pair().1);
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let hosted = Log::open(&path, limits).and_then(|log| Hosted::open(&path, log));
            match hosted {
                Ok(hosted) => {
                    table.put(hosted.log.state().row(WallMs::now()));
                    threads.insert(hosted.log.id(), hosted);
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), "a thread's log is unreadable: {e}");
                }
            }
        }
        let starts = Intents::open(&dir.join("starts"))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_owned(),
                limits,
                threads,
                table,
                starts,
                edges: broadcast::Sender::new(EDGES),
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
        self.inner.lock().threads.contains_key(&thread)
    }

    /// The threads held.
    #[must_use]
    pub fn threads(&self) -> Vec<ThreadId> {
        let mut ids: Vec<ThreadId> = self.inner.lock().threads.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Apply `actions` to `thread`, log them and send them to its followers; the cursor after
    /// them, or `None` for a thread not held. A log that cannot be written is warned of, and
    /// the actions count anyway.
    pub fn apply(&self, thread: ThreadId, actions: Vec<Action>) -> Option<Cursor> {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        let hosted = inner.threads.get_mut(&thread)?;
        Some(apply(hosted, &mut inner.table, &inner.edges, actions))
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
        let hosted = inner.threads.get_mut(&thread)?;
        let (actions, out) = change(hosted.log.state());
        if !actions.is_empty() {
            apply(hosted, &mut inner.table, &inner.edges, actions);
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
        let Some(hosted) = inner.threads.get_mut(&thread) else { return Ok(None) };
        // What waits to be sent and whether the tree is to review are the worker's own, which
        // no agent's session has: they stay.
        // One that was being typed may be in the terminal already, so it is not typed again.
        let mut state = state;
        state.pending.clone_from(&hosted.log.state().pending);
        state.to_review = hosted.log.state().to_review;
        for pending in &mut state.pending {
            if pending.state == PendingState::Sending {
                pending.state =
                    PendingState::Held { reason: super::compose::TYPED_NOT_SENT.to_owned() };
            }
        }
        hosted.log.reset(state)?;
        let cursor = hosted.log.cursor();
        inner.table.put(hosted.log.state().row(WallMs::now()));
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
        match inner.threads.remove(&thread) {
            Some(hosted) => hosted.log.delete(),
            None => Ok(()),
        }
    }

    /// Message `text` was typed for intent `id` into `thread`'s agent: the item the agent
    /// makes of it is marked with the intent ([`slopty_proto::thread::UserMessage::intent`]).
    pub fn typed(&self, thread: ThreadId, id: IntentId, text: &str) {
        let mut inner = self.inner.lock();
        let Some(hosted) = inner.threads.get_mut(&thread) else { return };
        let typed = &mut hosted.own.typed;
        if typed.len() >= TYPED {
            typed.pop_front();
        }
        typed.push_back((id, text.to_owned()));
    }

    /// Every turn edge any thread reaches from now on.
    #[must_use]
    pub fn edges(&self) -> broadcast::Receiver<TurnEdge> {
        self.inner.lock().edges.subscribe()
    }

    /// The outcome intent `id` for `thread` had, if it was acted on.
    #[must_use]
    pub fn outcome(&self, thread: ThreadId, id: IntentId) -> Option<Outcome> {
        self.inner.lock().threads.get(&thread)?.intents.outcome(&id).cloned()
    }

    /// Every batch `thread` applies from now on; `None` for a thread not held.
    #[must_use]
    pub fn watch(&self, thread: ThreadId) -> Option<broadcast::Receiver<Arc<Batch>>> {
        self.inner.lock().threads.get(&thread).map(|h| h.feed.subscribe())
    }

    /// Follow `thread` from `have`: what was missed, and every batch after it. `None` for a
    /// thread not held.
    #[must_use]
    pub fn follow(&self, thread: ThreadId, have: Option<Cursor>, turns: u32) -> Option<Follow> {
        let inner = self.inner.lock();
        let hosted = inner.threads.get(&thread)?;
        Some(Follow { catchup: hosted.log.since(have, turns), feed: hosted.feed.subscribe() })
    }

    /// Up to `turns` turns of `thread` before `before`.
    #[must_use]
    pub fn page(&self, thread: ThreadId, before: TurnId, turns: u32) -> Option<Page> {
        self.inner.lock().threads.get(&thread).map(|h| h.log.state().page(before, turns))
    }

    /// `thread` as it stands, and its cursor.
    #[must_use]
    pub fn state(&self, thread: ThreadId) -> Option<(ThreadState, Cursor)> {
        self.inner.lock().threads.get(&thread).map(|h| (h.log.state().clone(), h.log.cursor()))
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
        let hosted = inner.threads.get_mut(&thread)?;
        if let Some(outcome) = hosted.intents.outcome(&id) {
            return Some(outcome.clone());
        }
        let (outcome, actions) = act(hosted.log.state());
        if !actions.is_empty() {
            apply(hosted, &mut inner.table, &inner.edges, actions);
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
    /// observed or shared agent's, which the adapter begins when the agent first speaks). A
    /// repeat of the id gets the first outcome back.
    pub fn record_start(&self, id: IntentId, outcome: Outcome) -> Outcome {
        let mut inner = self.inner.lock();
        if let Some(first) = inner.starts.outcome(&id) {
            return first.clone();
        }
        if let Err(e) = inner.starts.record(id, outcome.clone()) {
            tracing::warn!("a start could not be recorded: {e}");
        }
        outcome
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
        outcome
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

fn apply(
    hosted: &mut Hosted,
    table: &mut Table,
    edges: &broadcast::Sender<TurnEdge>,
    mut actions: Vec<Action>,
) -> Cursor {
    hosted.own.mark(&mut actions);
    let first = hosted.log.cursor();
    let was_working = hosted.log.state().status.phase == Phase::Working;
    if let Err(e) = hosted.log.append(&actions) {
        tracing::warn!(thread = %hosted.log.id(), "a thread's log could not be written: {e}");
    }
    table.put(hosted.log.state().row(WallMs::now()));
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
    let _no_follower =
        hosted.feed.send(Arc::new(Batch { epoch: first.epoch, seq: first.seq, actions }));
    hosted.log.cursor()
}
