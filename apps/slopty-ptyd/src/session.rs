//! One PTY-backed child, its output ring and the last worker's checkpoint.

use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use rustix::process::{Pid, Signal};
use slopty_core::{SessionId, WallMs};
use slopty_proto::terminal::TermSize;
use slopty_pty::protocol::SessionInfo;
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
        /// Exit code, or the terminating signal negated.
        status: i32,
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
    /// Slave device.
    tty: PathBuf,
    /// Master, shared with the reader task.
    master: Arc<PtyMaster>,
    /// What the connections and the reader change, under one lock.
    state: Mutex<State>,
    /// Exit status once known.
    exited: watch::Sender<Option<i32>>,
    /// What the reader is to do. It lives in the session it reads for, so it never closes: a
    /// stop is the only way out of a pause.
    reader: watch::Sender<Reader>,
    /// Reader acknowledges it is out of the fd (or finished) by setting this to `true`.
    parked: watch::Sender<bool>,
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
        let mut child = pty.spawn_with(spec, integration)?;
        let pid = child.id().unwrap_or(0);
        let started_ms = WallMs::now();
        let master = Arc::new(PtyMaster::new(pty.into_master())?);
        let (reader, _) = watch::channel(Reader::Drain);
        let (parked, _) = watch::channel(false);
        let state = State {
            size: spec.size,
            ring: Ring::new(backlog_bytes),
            checkpoint: Vec::new(),
            attached_by: None,
        };
        let session = Arc::new(Self {
            id,
            pid,
            started_ms,
            tty,
            master,
            state: Mutex::new(state),
            exited: watch::Sender::new(None),
            reader,
            parked,
        });

        let reader = Arc::clone(&session);
        tokio::spawn(async move { reader.read_loop().await });

        let waiter = Arc::clone(&session);
        tokio::spawn(async move {
            let status = match child.wait().await {
                Ok(status) => exit_code(status),
                Err(e) => {
                    tracing::warn!(session = %waiter.id, error = %e, "wait failed");
                    -1
                }
            };
            waiter.exited.send_replace(Some(status));
            tracing::info!(session = %waiter.id, pid = waiter.pid, status, "child exited");
            let _ignored = events.send(Broadcast::Exited { id: waiter.id, status });
        });
        Ok(session)
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

    /// Stop the reader and wait until it is out of the fd, then take the backlog with the
    /// checkpoint it follows and the size.
    pub async fn hand_over(&self) -> Handover {
        self.reader.send_if_modified(|mode| {
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
        let mut state = self.state.lock();
        let dropped = state.ring.dropped();
        Handover {
            checkpoint: state.checkpoint.clone(),
            backlog: state.ring.drain(),
            dropped,
            size: state.size,
            started_ms: self.started_ms,
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
    pub async fn exits_within(&self, grace: std::time::Duration) -> bool {
        let mut exited = self.exited.subscribe();
        tokio::time::timeout(grace, exited.wait_for(Option::is_some)).await.is_ok_and(|w| w.is_ok())
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
    pub fn close(self: Arc<Self>, grace: std::time::Duration) {
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

    /// Send a signal to the child's process group (the child is its own session leader).
    pub fn signal(&self, signal: Signal) -> Result<(), std::io::Error> {
        let pid = i32::try_from(self.pid)
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| std::io::Error::other("not a process id"))?;
        rustix::process::kill_process_group(pid, signal).map_err(std::io::Error::from)
    }
}

/// Exit code, or the terminating signal negated.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().or_else(|| status.signal().map(i32::saturating_neg)).unwrap_or(-1)
}
