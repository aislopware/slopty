//! One PTY-backed child, its output ring and the last host's checkpoint.

use std::os::fd::BorrowedFd;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use slopty_core::SessionId;
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
        /// Session.
        id: SessionId,
        /// Status.
        status: i32,
    },
}

/// Shared session state.
pub struct Session {
    /// Id.
    pub id: SessionId,
    /// Child pid.
    pub pid: u32,
    /// Slave device.
    pub tty: PathBuf,
    /// Master, shared with the reader task.
    pub master: Arc<PtyMaster>,
    /// Size of record.
    pub size: Mutex<TermSize>,
    /// Output since the last checkpoint: tapped by the attached host, read by us while detached.
    pub ring: Mutex<Ring>,
    /// The last host's terminal state (empty until a host sends one).
    pub checkpoint: Mutex<Vec<u8>>,
    /// Connection id holding the master, if any.
    pub attached_by: Mutex<Option<u64>>,
    /// Exit status once known.
    pub exited: Mutex<Option<i32>>,
    /// `true` = reader must stay out of the fd.
    pub pause: watch::Sender<bool>,
    /// Reader acknowledges it is out of the fd (or finished) by setting this to `true`.
    pub parked: watch::Sender<bool>,
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
        let master = Arc::new(PtyMaster::new(pty.into_master())?);
        let (pause, _) = watch::channel(false);
        let (parked, _) = watch::channel(false);
        let session = Arc::new(Self {
            id,
            pid,
            tty,
            master,
            size: Mutex::new(spec.size),
            ring: Mutex::new(Ring::new(backlog_bytes)),
            checkpoint: Mutex::new(Vec::new()),
            attached_by: Mutex::new(None),
            exited: Mutex::new(None),
            pause,
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
            *waiter.exited.lock() = Some(status);
            tracing::info!(session = %waiter.id, pid = waiter.pid, status, "child exited");
            let _ignored = events.send(Broadcast::Exited { id: waiter.id, status });
        });
        Ok(session)
    }

    /// Drain the master into the ring whenever not paused.
    async fn read_loop(&self) {
        let mut pause_rx = self.pause.subscribe();
        let mut buf = vec![0_u8; 64 << 10];
        loop {
            if *pause_rx.borrow_and_update() {
                self.parked.send_replace(true);
                if pause_rx.changed().await.is_err() {
                    return;
                }
                continue;
            }
            self.parked.send_replace(false);
            tokio::select! {
                changed = pause_rx.changed() => {
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
                    Ok(n) => self.ring.lock().push(buf.get(..n).unwrap_or_default()),
                    Err(e) => {
                        tracing::warn!(session = %self.id, error = %e, "master read failed");
                        self.parked.send_replace(true);
                        return;
                    }
                },
            }
        }
    }

    /// Stop the reader and wait until it is out of the fd, then take the backlog.
    pub async fn pause_reader(&self) -> (Vec<u8>, u64) {
        self.pause.send_replace(true);
        let mut parked = self.parked.subscribe();
        while !*parked.borrow_and_update() {
            if parked.changed().await.is_err() {
                break;
            }
        }
        let mut ring = self.ring.lock();
        let dropped = ring.dropped();
        (ring.drain(), dropped)
    }

    /// Output the attached host read, in order: goes after whatever the ring already holds.
    pub fn tap(&self, bytes: &[u8]) {
        self.ring.lock().push(bytes);
    }

    /// Replace the checkpoint; the ring's bytes are inside it now, so they go.
    pub fn set_checkpoint(&self, state: Vec<u8>) {
        // Both locks, ring first, so an `Attach` racing this sees either the old pair or the new
        // pair: it takes `checkpoint` under its own lock only after `pause_reader` released ours.
        let mut ring = self.ring.lock();
        *self.checkpoint.lock() = state;
        ring.clear();
    }

    /// Let the reader drain again.
    pub fn resume_reader(&self) {
        self.pause.send_replace(false);
    }

    /// The master fd.
    #[must_use]
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Snapshot for `List`.
    #[must_use]
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            pid: self.pid,
            tty: self.tty.clone(),
            size: *self.size.lock(),
            attached: self.attached_by.lock().is_some(),
            exited: *self.exited.lock(),
            backlog: self.ring.lock().len(),
            checkpoint: self.checkpoint.lock().len(),
        }
    }

    /// Send a signal to the child's process group (the child is its own session leader).
    pub fn signal(&self, signal: i32) -> Result<(), std::io::Error> {
        let pid =
            i32::try_from(self.pid).map_err(|_overflow| std::io::Error::other("pid overflow"))?;
        let sig = nix::sys::signal::Signal::try_from(signal)
            .map_err(|_bad| std::io::Error::other("bad signal"))?;
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), sig).map_err(std::io::Error::from)
    }
}

/// Exit code, or the terminating signal negated.
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().or_else(|| status.signal().map(i32::saturating_neg)).unwrap_or(-1)
}
