//! One thread's log: its state, the actions that brought it there, and both on disk.
//!
//! A thread's directory holds `snapshot` (the state and its cursor) and `tail` (a header naming
//! the cursor it continues from, then each action since, framed as the wire frames them). An
//! action is applied, appended to the tail file and kept in memory, the last
//! [`Limits::tail_actions`] of them (and at most [`Limits::tail_bytes`]), which is what a
//! follower that comes back is sent instead of a snapshot. At a turn's end, once the tail file
//! has grown past [`Limits::compact_after`] actions, the snapshot is written anew and the tail
//! file starts over; the in-memory tail is kept, so a compaction costs no follower a snapshot.
//!
//! Beside them, `head` keeps the thread's row and its state without its items, written whenever
//! the thread rests ([`Log::write_head`]): a worker's start reads the heads alone and opens a log
//! only when its thread is first used ([`Log::head`]). A head names the tail it was written
//! over, by the cursor the tail continues from and its length, so one the log has moved past
//! since is not taken.
//!
//! The log is a cache of the agent's own session, so nothing is synced to the disk: a crash
//! loses at most what the operating system had not written, the torn end of the tail file is
//! dropped on reading, and the adapter rebuilds from the native session ([`Log::reset`]), which
//! starts a new epoch.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use slopty_proto::codec;
use slopty_proto::thread::wire::{ThreadFrame, ThreadRow};
use slopty_proto::thread::{Action, Cursor, ThreadId, ThreadState};

const SNAPSHOT: &str = "snapshot";
const TAIL: &str = "tail";
const HEAD: &str = "head";

/// How much a log keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Actions kept in memory for followers that come back.
    pub tail_actions: usize,
    /// Encoded bytes of them, at most.
    pub tail_bytes: usize,
    /// Actions in the tail file past which a turn's end writes a new snapshot.
    pub compact_after: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { tail_actions: 4096, tail_bytes: 4 << 20, compact_after: 1024 }
    }
}

/// What a follower holding a cursor is sent to catch up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Catchup {
    /// The actions it missed, maybe none.
    Actions {
        /// From where it is.
        from: Cursor,
        /// The actions.
        actions: Vec<Action>,
    },
    /// Everything again: its cursor is of another epoch, or older than the tail.
    Snapshot {
        /// Where the log stands.
        cursor: Cursor,
        /// The state, cut to its last turns.
        state: Box<ThreadState>,
    },
}

impl Catchup {
    /// As a frame of the thread's stream, and the cursor it leaves a follower at.
    #[must_use]
    pub fn frame(self) -> (ThreadFrame, Cursor) {
        match self {
            Self::Actions { from, actions } => {
                let next =
                    from.seq.saturating_add(u64::try_from(actions.len()).unwrap_or(u64::MAX));
                let frame =
                    ThreadFrame::Actions { epoch: from.epoch, first: from.seq, next, actions };
                (frame, Cursor { epoch: from.epoch, seq: next })
            }
            Self::Snapshot { cursor, state } => (ThreadFrame::Snapshot { cursor, state }, cursor),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    cursor: Cursor,
    state: ThreadState,
}

/// The cursor a tail file continues from.
#[derive(Serialize, Deserialize)]
struct TailHead {
    from: Cursor,
}

/// What a worker's start reads of a thread in place of its log ([`Log::head`]).
#[derive(Clone, Debug)]
pub struct Head {
    /// Its row in the table.
    pub row: ThreadRow,
    /// Its state, with no items: what a look over every thread needs, without the bulk.
    pub state: ThreadState,
}

/// A head as kept: the tail it was written over, by the cursor that tail continues from and
/// its length then, and the head itself.
#[derive(Deserialize)]
struct StoredHead {
    from: Cursor,
    len: u64,
    row: ThreadRow,
    state: ThreadState,
}

#[derive(Serialize)]
struct StoredHeadRef<'a> {
    from: Cursor,
    len: u64,
    row: &'a ThreadRow,
    state: &'a ThreadState,
}

/// One thread's log.
#[derive(Debug)]
pub struct Log {
    dir: PathBuf,
    limits: Limits,
    state: ThreadState,
    cursor: Cursor,
    /// The last actions, the newest last: number `cursor.seq - tail.len()` first.
    tail: VecDeque<(Action, usize)>,
    tail_bytes: usize,
    file: File,
    /// Actions in the tail file.
    on_disk: u64,
    /// The cursor the tail file continues from.
    tail_from: Cursor,
    /// The tail file's length; [`u64::MAX`] once a write to it failed, so no head names it.
    tail_len: u64,
}

impl Log {
    /// A new log for `state` in `dir`, which it makes.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    pub fn create(dir: &Path, state: ThreadState, limits: Limits) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let cursor = Cursor { epoch: fresh_epoch(), seq: 0 };
        write_snapshot(dir, cursor, &state)?;
        let (file, tail_len) = start_tail(dir, cursor)?;
        Ok(Self {
            dir: dir.to_owned(),
            limits,
            state,
            cursor,
            tail: VecDeque::new(),
            tail_bytes: 0,
            file,
            on_disk: 0,
            tail_from: cursor,
            tail_len,
        })
    }

    /// The log in `dir`, as its files left it: the snapshot, then every whole action of the
    /// tail that continues it. A torn last action is cut off the file.
    ///
    /// # Errors
    ///
    /// When there is no readable snapshot.
    pub fn open(dir: &Path, limits: Limits) -> io::Result<Self> {
        let stored: Stored = decode(&std::fs::read(dir.join(SNAPSHOT))?)?;
        let (actions, whole) = read_tail(&dir.join(TAIL), stored.cursor);
        let (file, tail_len) = match whole {
            Some(len) => {
                let file = OpenOptions::new().append(true).open(dir.join(TAIL))?;
                file.set_len(len)?;
                (file, len)
            }
            None => start_tail(dir, stored.cursor)?,
        };
        let mut log = Self {
            dir: dir.to_owned(),
            limits,
            state: stored.state,
            cursor: stored.cursor,
            tail: VecDeque::new(),
            tail_bytes: 0,
            file,
            on_disk: 0,
            tail_from: stored.cursor,
            tail_len,
        };
        for (action, bytes) in actions {
            log.state.apply(&action);
            log.cursor.seq = log.cursor.seq.saturating_add(1);
            log.on_disk = log.on_disk.saturating_add(1);
            log.keep(action, bytes);
        }
        Ok(log)
    }

    /// The state the log in `dir` holds, as [`Self::open`] would read it, with nothing written:
    /// a look at a thread not opened yet.
    ///
    /// # Errors
    ///
    /// When there is no readable snapshot.
    pub fn read(dir: &Path) -> io::Result<ThreadState> {
        let stored: Stored = decode(&std::fs::read(dir.join(SNAPSHOT))?)?;
        let (actions, _) = read_tail(&dir.join(TAIL), stored.cursor);
        let mut state = stored.state;
        for (action, _) in &actions {
            state.apply(action);
        }
        Ok(state)
    }

    /// The head of the log in `dir` ([`Self::write_head`]), when it was written over the tail
    /// as it stands: `None` when there is none, it cannot be read, or the tail has moved since.
    #[must_use]
    pub fn head(dir: &Path) -> Option<Head> {
        let stored: StoredHead = decode(&std::fs::read(dir.join(HEAD)).ok()?).ok()?;
        let (from, len) = tail_mark(&dir.join(TAIL))?;
        (from == stored.from && len == stored.len)
            .then_some(Head { row: stored.row, state: stored.state })
    }

    /// Keep `row`, the thread's row now, and the state without its items as the log's head,
    /// over the tail as it stands; the head as kept.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    pub fn write_head(&mut self, row: &ThreadRow) -> io::Result<Head> {
        let items = std::mem::take(&mut self.state.items);
        let head = Head { row: row.clone(), state: self.state.clone() };
        self.state.items = items;
        let stored =
            StoredHeadRef { from: self.tail_from, len: self.tail_len, row, state: &head.state };
        let body = codec::encode_body(&stored).map_err(io::Error::other)?;
        let staging = self.dir.join(".head");
        std::fs::write(&staging, body)?;
        std::fs::rename(staging, self.dir.join(HEAD))?;
        Ok(head)
    }

    /// The thread.
    #[must_use]
    pub const fn id(&self) -> ThreadId {
        self.state.meta.id
    }

    /// The state.
    #[must_use]
    pub const fn state(&self) -> &ThreadState {
        &self.state
    }

    /// Where it stands.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Apply `actions` and append them, in order. A turn's end among them may write a new
    /// snapshot.
    ///
    /// # Errors
    ///
    /// When the tail cannot be written; the state has the actions anyway, since the log is a
    /// cache.
    pub fn append(&mut self, actions: &[Action]) -> io::Result<()> {
        let mut frames = Vec::new();
        let mut turn_ended = false;
        for action in actions {
            self.state.apply(action);
            self.cursor.seq = self.cursor.seq.saturating_add(1);
            turn_ended |= matches!(action, Action::TurnEnded { .. });
            let frame = codec::encode(action).map_err(io::Error::other)?;
            frames.extend_from_slice(&frame);
            self.keep(action.clone(), frame.len());
        }
        self.on_disk =
            self.on_disk.saturating_add(u64::try_from(actions.len()).unwrap_or(u64::MAX));
        match self.file.write_all(&frames) {
            Ok(()) => {
                let wrote = u64::try_from(frames.len()).unwrap_or(u64::MAX);
                self.tail_len = self.tail_len.saturating_add(wrote);
            }
            Err(e) => {
                self.tail_len = u64::MAX;
                return Err(e);
            }
        }
        if turn_ended && self.on_disk >= self.limits.compact_after {
            self.compact()?;
        }
        Ok(())
    }

    /// Start a new epoch from `state`: the agent's own session was read again, or the thread
    /// rewound or forked. Every follower gets a snapshot.
    ///
    /// # Errors
    ///
    /// When it cannot be written.
    pub fn reset(&mut self, state: ThreadState) -> io::Result<()> {
        self.state = state;
        self.cursor = Cursor { epoch: self.cursor.epoch.wrapping_add(1), seq: 0 };
        self.tail.clear();
        self.tail_bytes = 0;
        self.compact()
    }

    /// What a follower holding `have` needs: the actions after it when the tail still has them
    /// in this epoch, else a snapshot of the last `turns` turns.
    #[must_use]
    pub fn since(&self, have: Option<Cursor>, turns: u32) -> Catchup {
        let first =
            self.cursor.seq.saturating_sub(u64::try_from(self.tail.len()).unwrap_or(u64::MAX));
        match have {
            Some(have)
                if have.epoch == self.cursor.epoch
                    && have.seq >= first
                    && have.seq <= self.cursor.seq =>
            {
                let skip = usize::try_from(have.seq.saturating_sub(first)).unwrap_or(usize::MAX);
                Catchup::Actions {
                    from: have,
                    actions: self.tail.iter().skip(skip).map(|(a, _)| a.clone()).collect(),
                }
            }
            _ => {
                Catchup::Snapshot { cursor: self.cursor, state: Box::new(self.state.window(turns)) }
            }
        }
    }

    /// Delete the log's files.
    ///
    /// # Errors
    ///
    /// When they cannot be removed.
    pub fn delete(self) -> io::Result<()> {
        std::fs::remove_dir_all(&self.dir)
    }

    fn keep(&mut self, action: Action, bytes: usize) {
        self.tail.push_back((action, bytes));
        self.tail_bytes = self.tail_bytes.saturating_add(bytes);
        while self.tail.len() > self.limits.tail_actions || self.tail_bytes > self.limits.tail_bytes
        {
            let Some((_, gone)) = self.tail.pop_front() else { break };
            self.tail_bytes = self.tail_bytes.saturating_sub(gone);
        }
    }

    fn compact(&mut self) -> io::Result<()> {
        write_snapshot(&self.dir, self.cursor, &self.state)?;
        let (file, len) = start_tail(&self.dir, self.cursor)?;
        self.file = file;
        self.tail_from = self.cursor;
        self.tail_len = len;
        self.on_disk = 0;
        Ok(())
    }
}

/// An epoch no earlier log of the thread had: random, so a log lost and made again never
/// meets a follower's cursor of the old one by chance.
fn fresh_epoch() -> u64 {
    ThreadId::new().as_uuid().as_u64_pair().1
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    codec::decode_body(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_snapshot(dir: &Path, cursor: Cursor, state: &ThreadState) -> io::Result<()> {
    let body = codec::encode_body(&StoredRef { cursor, state }).map_err(io::Error::other)?;
    let staging = dir.join(".snapshot");
    std::fs::write(&staging, body)?;
    std::fs::rename(staging, dir.join(SNAPSHOT))
}

#[derive(Serialize)]
struct StoredRef<'a> {
    cursor: Cursor,
    state: &'a ThreadState,
}

/// A new tail file continuing from `from`, and its length.
fn start_tail(dir: &Path, from: Cursor) -> io::Result<(File, u64)> {
    let head = codec::encode(&TailHead { from }).map_err(io::Error::other)?;
    let staging = dir.join(".tail");
    std::fs::write(&staging, &head)?;
    std::fs::rename(&staging, dir.join(TAIL))?;
    let file = OpenOptions::new().append(true).open(dir.join(TAIL))?;
    Ok((file, u64::try_from(head.len()).unwrap_or(u64::MAX)))
}

/// The cursor the tail file at `path` continues from and its length, read from its first
/// frame alone; `None` when it is missing or its head is torn.
fn tail_mark(path: &Path) -> Option<(Cursor, u64)> {
    use std::io::Read as _;
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut first = [0_u8; TAIL_HEAD_READ];
    let read = file.read(&mut first).ok()?;
    let mut rest = BytesMut::from(first.get(..read)?);
    let body = codec::try_take(&mut rest).ok().flatten()?;
    let head: TailHead = decode(&body).ok()?;
    Some((head.from, len))
}

/// Bytes read for a tail file's first frame: its length prefix and a cursor, with room.
const TAIL_HEAD_READ: usize = 256;

/// The whole actions of the tail file that continues from `from`, each with its framed size,
/// and the length of the file's whole part. `None` when the file is missing, torn in its head,
/// or continues another cursor: a tail of another epoch, or the one a compaction had just
/// replaced when the worker stopped. It is then started again.
fn read_tail(path: &Path, from: Cursor) -> (Vec<(Action, usize)>, Option<u64>) {
    let Ok(bytes) = std::fs::read(path) else { return (Vec::new(), None) };
    let total = bytes.len();
    let mut rest = BytesMut::from(bytes.as_slice());
    let head =
        codec::try_take(&mut rest).ok().flatten().and_then(|body| decode::<TailHead>(&body).ok());
    if head.is_none_or(|head| head.from != from) {
        return (Vec::new(), None);
    }
    let mut actions = Vec::new();
    let mut whole = total.saturating_sub(rest.len());
    while let Ok(Some(body)) = codec::try_take(&mut rest) {
        let Ok(action) = decode::<Action>(&body) else { break };
        let size = codec::PREFIX_BYTES.saturating_add(body.len());
        whole = whole.saturating_add(size);
        actions.push((action, size));
    }
    (actions, u64::try_from(whole).ok())
}
