//! One PTY-backed child, its output ring and the last worker's checkpoint.

use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rustix::process::{Pid, Signal};
use slopty_core::{SessionId, WallMs};
use slopty_proto::terminal::TermSize;
use slopty_pty::protocol::{Exit, Heir, SessionInfo};
use slopty_pty::shell_integration::ShellIntegration;
use slopty_pty::{Pty, PtyMaster, Ring, SpawnSpec};
use tokio::sync::{broadcast, watch};

/// Events every connection hears.
#[derive(Clone, Debug)]
pub enum Broadcast {
    /// A child exited.
    Exited {
        /// The session whose child it was.
        id: SessionId,
        /// How it ended.
        exit: Exit,
    },
}

/// Shared session state.
pub struct Session {
    /// The id the worker chose when it spawned the session.
    id: SessionId,
    /// Child pid.
    pid: u32,
    /// When the child was spawned.
    started_ms: WallMs,
    /// The terminfo name the child was given as `TERM`.
    term: String,
    /// Slave device.
    tty: PathBuf,
    /// Master, shared with the reader task.
    master: Arc<PtyMaster>,
    /// What the connections and the reader change, under one lock.
    state: Mutex<State>,
    /// Exit status once known.
    exited: watch::Sender<Option<Exit>>,
    /// What the reader is to do. It lives in the session it reads for, so it never closes: a
    /// stop is the only way out of a pause.
    reader: watch::Sender<Reader>,
    /// Reader acknowledges it is out of the fd (or finished) by setting this to `true`.
    parked: watch::Sender<bool>,
    /// The child is no child of this process: a worker handed the session back to a ptyd that
    /// started afresh ([`Self::adopt`]).
    orphan: bool,
    /// Such a child's start as the kernel recorded it when this ptyd took it
    /// ([`slopty_pty::process::start_mark`]): its pid is that child's only while it carries it.
    mark: Option<u64>,
}

/// How long a session a worker held when the ptyd before this build handed it over is kept out
/// of the reader, for that worker to take back (`PtydRequest::Reclaim`) before ptyd drains it
/// again.
const RECLAIM_GRACE: Duration = Duration::from_secs(30);

/// How often the pid of an adopted child is looked at: it is no child of ptyd, so no `SIGCHLD`
/// says it ended.
const ORPHAN_POLL: Duration = Duration::from_secs(1);

/// How a session's end is seen.
enum Watch {
    /// A child of this process, reaped.
    Child(slopty_pty::Child),
    /// A process that is no child of this one: gone once its pid is.
    Orphan,
}

/// What a session starts with, however it came to this ptyd.
struct Start {
    id: SessionId,
    pid: u32,
    started_ms: WallMs,
    term: String,
    tty: PathBuf,
    master: OwnedFd,
    state: State,
    reader: Reader,
    exited: Option<Exit>,
    orphan: bool,
    mark: Option<u64>,
}

/// What a session's reader is to do with the master.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reader {
    /// Drain it into the ring.
    Drain,
    /// Stay out of it: a worker holds it.
    Pause,
    /// Let go of the session: it is closed.
    Stop,
}

/// A session's state that the connections share: one lock, so an attach sees a checkpoint and
/// the ring after it as one pair.
struct State {
    /// Size of record.
    size: TermSize,
    /// Output since the last checkpoint: tapped by the attached worker, read by us while
    /// detached.
    ring: Ring,
    /// The last worker's terminal state (empty until a worker sends one).
    checkpoint: Vec<u8>,
    /// Connection id holding the master, if any.
    attached_by: Option<u64>,
}

/// What a worker taking the master is handed besides it.
#[derive(Debug)]
pub struct Handover {
    /// The last worker's terminal state.
    pub checkpoint: Vec<u8>,
    /// Output since it.
    pub backlog: Vec<u8>,
    /// Bytes lost before `backlog`.
    pub dropped: u64,
    /// Size of record.
    pub size: TermSize,
    /// When the child was spawned.
    pub started_ms: WallMs,
    /// The terminfo name the child was given as `TERM`.
    pub term: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Open a PTY, spawn, start the reader and the exit waiter.
    pub fn spawn(
        id: SessionId,
        spec: &SpawnSpec,
        backlog_bytes: usize,
        integration: Option<&ShellIntegration>,
        events: broadcast::Sender<Broadcast>,
    ) -> Result<Arc<Self>, slopty_pty::PtyError> {
        let pty = Pty::open(spec.size)?;
        let tty = pty.slave_path().to_path_buf();
        let slopty_pty::Spawned { child, term } = pty.spawn_with(spec, integration)?;
        let session = Self::start(Start {
            id,
            pid: child.id().unwrap_or(0),
            started_ms: WallMs::now(),
            term,
            tty,
            master: pty.into_master(),
            state: State {
                size: spec.size,
                ring: Ring::new(backlog_bytes),
                checkpoint: Vec::new(),
                attached_by: None,
            },
            reader: Reader::Drain,
            exited: None,
            orphan: false,
            mark: None,
        })?;
        session.watch(Watch::Child(child), events);
        Ok(session)
    }

    /// Keep a session a worker hands back with its master: this ptyd started afresh after the
    /// one that spawned it ended. Connection `conn` holds the master from now on, as after an
    /// attach, so the reader starts out of it. Its child is no child of this process.
    pub fn adopt(
        id: SessionId,
        master: OwnedFd,
        child: slopty_pty::Adoptee,
        backlog_bytes: usize,
        conn: u64,
        events: broadcast::Sender<Broadcast>,
    ) -> Result<Arc<Self>, slopty_pty::PtyError> {
        let slopty_pty::Adoptee { pid, size, started_ms, term } = child;
        let session = Self::start(Start {
            id,
            pid,
            started_ms,
            term,
            tty: slopty_pty::pty::slave_of(&master)?,
            master,
            state: State {
                size,
                ring: Ring::new(backlog_bytes),
                checkpoint: Vec::new(),
                attached_by: Some(conn),
            },
            reader: Reader::Pause,
            exited: None,
            orphan: true,
            mark: mark_of(pid),
        })?;
        session.watch(Watch::Orphan, events);
        Ok(session)
    }

    /// Take a session the ptyd before this build handed over ([`Heir`]), with its master. Its
    /// child is still this process's (the pid did not change), unless it was an orphan there.
    /// One a worker held is kept out of the reader for [`RECLAIM_GRACE`], for that worker.
    pub fn inherit(
        heir: Heir,
        master: OwnedFd,
        backlog_bytes: usize,
        events: broadcast::Sender<Broadcast>,
    ) -> Result<Arc<Self>, slopty_pty::PtyError> {
        let Heir {
            id,
            pid,
            tty,
            started_ms,
            term,
            size,
            checkpoint,
            backlog,
            dropped,
            exited,
            attached,
            orphan,
            orphan_mark,
            // Taken already: it is `master`.
            ..
        } = heir;
        // A child of this process still, unless it was adopted there or cannot be waited for
        // here: such a one is watched, and signalled, by its pid's mark.
        let child =
            if exited.is_none() && !orphan { slopty_pty::Child::inherited(pid) } else { None };
        let watched = exited.is_none() && child.is_none();
        let mark = if orphan {
            orphan_mark
        } else if watched {
            mark_of(pid)
        } else {
            None
        };
        let session = Self::start(Start {
            id,
            pid,
            started_ms,
            term,
            tty,
            master,
            state: State {
                size,
                ring: Ring::holding(backlog_bytes, &backlog, dropped),
                checkpoint,
                attached_by: None,
            },
            reader: if attached { Reader::Pause } else { Reader::Drain },
            exited,
            orphan: orphan || watched,
            mark,
        })?;
        match child {
            Some(child) => session.watch(Watch::Child(child), events),
            None if watched => session.watch(Watch::Orphan, events),
            None => {}
        }
        if attached {
            let kept = Arc::clone(&session);
            tokio::spawn(async move {
                tokio::time::sleep(RECLAIM_GRACE).await;
                // Taken back meanwhile, the reader stays out for that connection.
                kept.resume_unless_held();
            });
        }
        Ok(session)
    }

    fn start(start: Start) -> Result<Arc<Self>, slopty_pty::PtyError> {
        let Start { id, pid, started_ms, term, tty, master, state, reader, exited, orphan, mark } =
            start;
        let master = Arc::new(PtyMaster::new(master)?);
        let (reader, _) = watch::channel(reader);
        let (parked, _) = watch::channel(false);
        let session = Arc::new(Self {
            id,
            pid,
            started_ms,
            term,
            tty,
            master,
            state: Mutex::new(state),
            exited: watch::Sender::new(exited),
            reader,
            parked,
            orphan,
            mark,
        });
        let reading = Arc::clone(&session);
        tokio::spawn(async move { reading.read_loop().await });
        Ok(session)
    }

    /// Wait for the session's end as `how` says, then record it and tell every connection.
    fn watch(self: &Arc<Self>, how: Watch, events: broadcast::Sender<Broadcast>) {
        let waiter = Arc::clone(self);
        tokio::spawn(async move {
            let exit = match how {
                Watch::Child(mut child) => match child.wait().await {
                    Ok(status) => exit_of(status),
                    Err(e) => {
                        tracing::warn!(session = %waiter.id, error = %e, "wait failed");
                        Exit::UNKNOWN
                    }
                },
                // No child of this process: its end shows only as its pid gone.
                Watch::Orphan => {
                    while waiter.child_lives() {
                        tokio::time::sleep(ORPHAN_POLL).await;
                    }
                    Exit::UNKNOWN
                }
            };
            waiter.exited.send_replace(Some(exit));
            tracing::info!(session = %waiter.id, pid = waiter.pid, status = ?exit.status, "child exited");
            let _ignored = events.send(Broadcast::Exited { id: waiter.id, exit });
        });
    }

    /// Drain the master into the ring whenever not paused, until stopped or the child's side
    /// is gone. The task holds the session, master included, until this returns.
    async fn read_loop(&self) {
        let mut mode = self.reader.subscribe();
        let mut buf = vec![0_u8; 64 << 10];
        loop {
            let now = *mode.borrow_and_update();
            match now {
                Reader::Stop => {
                    self.parked.send_replace(true);
                    return;
                }
                Reader::Pause => {
                    self.parked.send_replace(true);
                    if mode.changed().await.is_err() {
                        return;
                    }
                    continue;
                }
                Reader::Drain => {}
            }
            self.parked.send_replace(false);
            tokio::select! {
                changed = mode.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                read = self.master.read(&mut buf) => match read {
                    Ok(0) => {
                        tracing::debug!(session = %self.id, "master EOF");
                        self.parked.send_replace(true);
                        return;
                    }
                    Ok(n) => self.state.lock().ring.push(buf.get(..n).unwrap_or_default()),
                    Err(e) => {
                        tracing::warn!(session = %self.id, error = %e, "master read failed");
                        self.parked.send_replace(true);
                        return;
                    }
                },
            }
        }
    }

    /// Hand the master to connection `conn` unless another holds it; `false` when one does.
    pub fn claim(&self, conn: u64) -> bool {
        let mut state = self.state.lock();
        match state.attached_by {
            Some(other) if other != conn => false,
            _ => {
                state.attached_by = Some(conn);
                true
            }
        }
    }

    /// Connection `conn` let go of the master (it hung up).
    pub fn release(&self, conn: u64) {
        let mut state = self.state.lock();
        if state.attached_by == Some(conn) {
            state.attached_by = None;
        }
    }

    /// Stop the reader and wait until it is out of the fd; `true` when this stopped it, `false`
    /// when it was out already (a worker holds the master, or the session is closed).
    pub async fn park(&self) -> bool {
        let paused = self.reader.send_if_modified(|mode| {
            let pause = *mode == Reader::Drain;
            if pause {
                *mode = Reader::Pause;
            }
            pause
        });
        let mut parked = self.parked.subscribe();
        while !*parked.borrow_and_update() {
            if parked.changed().await.is_err() {
                break;
            }
        }
        paused
    }

    /// The backlog with the checkpoint it follows and the size, taken once the reader is out of
    /// the fd ([`Self::park`]).
    pub fn hand_over(&self) -> Handover {
        let mut state = self.state.lock();
        let dropped = state.ring.dropped();
        Handover {
            checkpoint: state.checkpoint.clone(),
            backlog: state.ring.drain(),
            dropped,
            size: state.size,
            started_ms: self.started_ms,
            term: self.term.clone(),
        }
    }

    /// Output connection `conn` read from the master, in order: goes after whatever the ring
    /// already holds. Only the connection holding the master may tap: its frames and its EOF
    /// arrive in one order, so everything a dying worker tapped is in the ring before our
    /// reader resumes, and nothing it sends can land after the next worker attaches.
    pub fn tap(&self, conn: u64, bytes: &[u8]) {
        let mut state = self.state.lock();
        if state.attached_by == Some(conn) {
            state.ring.push(bytes);
        }
    }

    /// Replace the checkpoint with one from connection `conn`, if it holds the master; the
    /// ring's bytes are inside it now, so they go.
    pub fn set_checkpoint(&self, conn: u64, checkpoint: Vec<u8>) {
        let mut state = self.state.lock();
        if state.attached_by == Some(conn) {
            state.checkpoint = checkpoint;
            state.ring.clear();
        }
    }

    /// The size of record, which the next worker to attach starts at.
    pub fn set_size(&self, size: TermSize) {
        self.state.lock().size = size;
    }

    /// Wait up to `grace` for the child to exit; `true` when it has.
    pub async fn exits_within(&self, grace: Duration) -> bool {
        let mut exited = self.exited.subscribe();
        tokio::time::timeout(grace, exited.wait_for(Option::is_some)).await.is_ok_and(|w| w.is_ok())
    }

    /// Let the reader drain again unless a connection holds the master (or the session is
    /// closed): a handover that did not happen gives back what it paused.
    pub fn resume_unless_held(&self) {
        // Under the lock a claim takes, so a claim comes either before (and is seen) or after
        // (and parks the reader again).
        let state = self.state.lock();
        if state.attached_by.is_none() {
            self.resume_reader();
        }
        drop(state);
    }

    /// Let the reader drain again, unless the session is closed.
    pub fn resume_reader(&self) {
        self.reader.send_if_modified(|mode| {
            let resume = *mode == Reader::Pause;
            if resume {
                *mode = Reader::Drain;
            }
            resume
        });
    }

    /// The session is closed: the reader lets go of it now, whoever holds the master, and the
    /// child is hung up, then killed if it has not exited within `grace`. Once the child is
    /// reaped nothing holds the session, so its master, ring and checkpoint go.
    pub fn close(self: Arc<Self>, grace: Duration) {
        self.reader.send_replace(Reader::Stop);
        if self.exited.borrow().is_some() {
            return;
        }
        let _hup = self.signal(Signal::HUP);
        tokio::spawn(async move {
            if !self.exits_within(grace).await {
                let _kill = self.signal(Signal::KILL);
            }
        });
    }

    /// The master fd.
    #[must_use]
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// The session as this ptyd hands it to the build it runs next. Taken once the reader is
    /// out of the master ([`Self::park`]), so the ring is whole.
    #[must_use]
    pub fn heir(&self) -> Heir {
        let state = self.state.lock();
        Heir {
            id: self.id,
            master: self.master.as_fd().as_raw_fd(),
            pid: self.pid,
            tty: self.tty.clone(),
            started_ms: self.started_ms,
            term: self.term.clone(),
            size: state.size,
            checkpoint: state.checkpoint.clone(),
            backlog: state.ring.contents(),
            dropped: state.ring.dropped(),
            exited: *self.exited.borrow(),
            attached: state.attached_by.is_some(),
            orphan: self.orphan,
            orphan_mark: self.mark.filter(|_| self.orphan),
        }
    }

    /// Snapshot for `List`.
    #[must_use]
    pub fn info(&self) -> SessionInfo {
        let state = self.state.lock();
        SessionInfo {
            id: self.id,
            pid: self.pid,
            tty: self.tty.clone(),
            size: state.size,
            attached: state.attached_by.is_some(),
            exited: *self.exited.borrow(),
            backlog: state.ring.len(),
            checkpoint: state.checkpoint.len(),
        }
    }

    /// Send a signal to the child's process group (the child is its own session leader). An
    /// adopted child no longer there is not signalled: its pid may be another process's now.
    pub fn signal(&self, signal: Signal) -> Result<(), std::io::Error> {
        let pid = i32::try_from(self.pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| std::io::Error::other("not a process id"))?;
        if self.orphan && !self.child_lives() {
            return Err(std::io::Error::from_raw_os_error(rustix::io::Errno::SRCH.raw_os_error()));
        }
        rustix::process::kill_process_group(pid, signal).map_err(std::io::Error::from)
    }

    /// Whether the child still runs under its pid. Only asked of one that is no child of this
    /// process: no zombie holds its pid once it ends, so the pid is that child's only while it
    /// carries the start mark it had when this ptyd took it.
    fn child_lives(&self) -> bool {
        self.mark.is_some_and(|mark| mark_of(self.pid) == Some(mark))
    }
}

/// The exit code, or the terminating signal negated; unknown when the status says neither.
fn exit_of(status: std::process::ExitStatus) -> Exit {
    use std::os::unix::process::ExitStatusExt as _;
    Exit { status: status.code().or_else(|| status.signal().map(i32::saturating_neg)) }
}

/// Process `pid`'s start mark ([`slopty_pty::process::start_mark`]).
fn mark_of(pid: u32) -> Option<u64> {
    i32::try_from(pid).ok().and_then(slopty_pty::process::start_mark)
}
