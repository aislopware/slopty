//! The thread host: every thread this worker holds, their logs, the table and the intents.
//!
//! One lock guards it all. What happens under it is small: applying actions, appending them
//! to a file, and a snapshot written at a turn's end once in a while. A follower subscribes
//! and takes its catch-up under the same lock, so it misses nothing and is sent nothing twice
//! ([`Host::follow`]).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Outcome, Page, TableFrame};
use slopty_proto::thread::{Action, Cursor, IntentId, ThreadId, ThreadMeta, ThreadState, TurnId};
use tokio::sync::{broadcast, watch};

use super::intents::Intents;
use super::log::{Catchup, Limits, Log};
use super::table::Table;

/// Batches a slow follower may fall behind by before it catches up from the log instead.
const FEED_BATCHES: usize = 1024;

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
}

#[derive(Debug)]
struct Hosted {
    log: Log,
    intents: Intents,
    feed: broadcast::Sender<Arc<Batch>>,
}

impl Hosted {
    fn open(dir: &Path, log: Log) -> io::Result<Self> {
        Ok(Self {
            log,
            intents: Intents::open(&dir.join("intents"))?,
            feed: broadcast::Sender::new(FEED_BATCHES),
        })
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
        Some(apply(hosted, &mut inner.table, actions))
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
            apply(hosted, &mut inner.table, actions);
        }
        if let Err(e) = hosted.intents.record(id, outcome.clone()) {
            tracing::warn!(%thread, "an intent could not be recorded: {e}");
        }
        Some(outcome)
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

fn apply(hosted: &mut Hosted, table: &mut Table, actions: Vec<Action>) -> Cursor {
    let first = hosted.log.cursor();
    if let Err(e) = hosted.log.append(&actions) {
        tracing::warn!(thread = %hosted.log.id(), "a thread's log could not be written: {e}");
    }
    table.put(hosted.log.state().row(WallMs::now()));
    let _no_follower =
        hosted.feed.send(Arc::new(Batch { epoch: first.epoch, seq: first.seq, actions }));
    hosted.log.cursor()
}
