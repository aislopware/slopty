//! The worker's thread table: a row per thread, numbered by a cursor of its own, so a client
//! that comes back is sent only the rows that changed and the threads that went.
//!
//! The table lives in memory and is rebuilt from the logs when the worker starts, under a new
//! epoch, so a client's cursor from before gets every row again. It remembers the last
//! [`REMOVED_KEPT`] removals; a cursor older than the oldest of them gets every row too.
//!
//! A row says which repository its thread works in, as a shell's summary does, so a client
//! groups an agent with no terminal (a Codex, pi or ACP thread, or one with no tile on that
//! client) with the other clones of its project (`Places`).

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use slopty_core::SessionId;
use slopty_proto::terminal::RepoId;
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use slopty_proto::thread::{Cursor, ThreadId};
use tokio::sync::watch;

/// Removals the table remembers for deltas.
pub const REMOVED_KEPT: usize = 1024;

/// The thread table.
#[derive(Debug)]
pub struct Table {
    cursor: Cursor,
    /// Each row, and the `seq` it last changed at.
    rows: HashMap<ThreadId, (ThreadRow, u64)>,
    /// The threads gone, and when.
    removed: VecDeque<(u64, ThreadId)>,
    /// A cursor below this has missed a removal the table no longer remembers.
    oldest: u64,
    changed: watch::Sender<Cursor>,
    places: Places,
}

/// Which repository each directory a thread worked in is, found once per directory: the walk
/// up for `.git` ([`crate::repo::root_of`]) and its config's origin
/// ([`crate::repo::origin_of`]), a few `stat` calls and a file read and never a `git`. The first
/// commit, which takes a `git`, is left to the shells' summaries; the origin is what tells one
/// repository's clones on two machines apart from the rest.
#[derive(Debug, Default)]
struct Places {
    known: HashMap<String, Option<(String, Option<RepoId>)>>,
}

impl Places {
    /// `row` with its directory's repository said.
    fn fill(&mut self, mut row: ThreadRow) -> ThreadRow {
        let Some(cwd) = row.cwd.as_deref() else { return row };
        let place = self.known.entry(cwd.to_owned()).or_insert_with(|| {
            let root = crate::repo::root_of(Path::new(cwd))?;
            let origin = crate::repo::origin_of(&root);
            let id = origin.map(|origin| RepoId { origin: Some(origin), ..RepoId::default() });
            Some((root.to_string_lossy().into_owned(), id))
        });
        if let Some((root, id)) = place {
            row.repo = Some(root.clone());
            row.repo_id.clone_from(id);
        }
        row
    }
}

impl Table {
    /// An empty table under `epoch`.
    #[must_use]
    pub fn new(epoch: u64) -> Self {
        let cursor = Cursor { epoch, seq: 0 };
        Self {
            cursor,
            rows: HashMap::new(),
            removed: VecDeque::new(),
            oldest: 0,
            changed: watch::Sender::new(cursor),
            places: Places::default(),
        }
    }

    /// The row of the thread whose agent runs in terminal `session`, hanging from no other:
    /// the latest to change, when several did.
    #[must_use]
    pub fn at_terminal(&self, session: SessionId) -> Option<&ThreadRow> {
        self.rows
            .values()
            .map(|(row, _)| row)
            .filter(|row| row.parent.is_none() && row.terminal == Some(session))
            .max_by_key(|row| (row.updated_ms, row.id))
    }

    /// Where it stands.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Told the cursor whenever the table changes.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Cursor> {
        self.changed.subscribe()
    }

    /// Put a thread's row, its repository said (`Places`). A row that says what it said
    /// before, but for when, changes nothing.
    pub fn put(&mut self, row: ThreadRow) {
        let row = self.places.fill(row);
        if let Some((have, _)) = self.rows.get(&row.id) {
            let mut same = row.clone();
            same.updated_ms = have.updated_ms;
            if same == *have {
                return;
            }
        }
        let seq = self.bump();
        self.rows.insert(row.id, (row, seq));
        self.changed.send_replace(self.cursor);
    }

    /// Take a thread out.
    pub fn remove(&mut self, id: ThreadId) {
        if self.rows.remove(&id).is_none() {
            return;
        }
        let seq = self.bump();
        self.removed.push_back((seq, id));
        while self.removed.len() > REMOVED_KEPT {
            if let Some((gone, _)) = self.removed.pop_front() {
                self.oldest = gone;
            }
        }
        self.changed.send_replace(self.cursor);
    }

    /// The rows a client holding `have` lacks: a delta in this epoch when the table still
    /// remembers everything since, else every row.
    #[must_use]
    pub fn since(&self, have: Option<Cursor>) -> TableFrame {
        match have {
            Some(have)
                if have.epoch == self.cursor.epoch
                    && have.seq >= self.oldest
                    && have.seq <= self.cursor.seq =>
            {
                let mut rows: Vec<ThreadRow> = self
                    .rows
                    .values()
                    .filter(|(_, at)| *at > have.seq)
                    .map(|(row, _)| row.clone())
                    .collect();
                rows.sort_by_key(|r| r.id);
                let removed = self
                    .removed
                    .iter()
                    .filter(|(at, _)| *at > have.seq)
                    .map(|(_, id)| *id)
                    .collect();
                TableFrame::Delta { cursor: self.cursor, rows, removed }
            }
            _ => {
                let mut rows: Vec<ThreadRow> =
                    self.rows.values().map(|(row, _)| row.clone()).collect();
                rows.sort_by_key(|r| r.id);
                TableFrame::Snapshot { cursor: self.cursor, rows }
            }
        }
    }

    const fn bump(&mut self) -> u64 {
        self.cursor.seq = self.cursor.seq.saturating_add(1);
        self.cursor.seq
    }
}
