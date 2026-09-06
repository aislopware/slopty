//! hostd ↔ ptyd messages. Framed with [`slopty_proto::codec`] over a Unix stream socket; the
//! PTY master rides as `SCM_RIGHTS` ancillary data on the first byte of an `Attached` frame.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;
use slopty_proto::terminal::TermSize;

use crate::pty::SpawnSpec;

/// Bumped on incompatible change. Both sides must match exactly.
pub const PTYD_PROTOCOL: u16 = 2;

/// Bytes ptyd retains per session: output read while detached, or tapped by the host since
/// its last checkpoint.
pub const DEFAULT_BACKLOG_BYTES: usize = 4 << 20;

/// Where the daemon listens: `$TMPDIR/slopty/ptyd.sock` (per-user, mode 0700 on macOS).
#[must_use]
pub fn socket_path() -> PathBuf {
    std::env::var_os("SLOPTY_PTYD_SOCKET")
        .map_or_else(|| std::env::temp_dir().join("slopty").join("ptyd.sock"), PathBuf::from)
}

/// hostd → ptyd.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PtydRequest {
    /// First message.
    Hello {
        /// Must equal [`PTYD_PROTOCOL`].
        protocol: u16,
    },
    /// Create a PTY and spawn on it. Answer: `Spawned` or `Error`.
    Spawn {
        /// Chosen by the host.
        id: SessionId,
        /// What to run.
        spec: SpawnSpec,
    },
    /// Hand over the master and the detached backlog. Answer: `Attached` (with fd) or `Error`.
    /// ptyd stops reading the master until the connection drops or `Detach` arrives.
    Attach {
        /// Session.
        id: SessionId,
    },
    /// Give the master back to ptyd's care; ptyd resumes draining.
    Detach {
        /// Session.
        id: SessionId,
    },
    /// A copy of output the attached host just read from the master. No reply. ptyd appends it
    /// to the session's ring so that a host which dies without detaching can be replaced by one
    /// that replays everything since the last `Checkpoint`.
    Output {
        /// Session.
        id: SessionId,
        /// The bytes, in read order.
        bytes: Vec<u8>,
    },
    /// The session's whole terminal state as a VT byte stream (what the attached host's engine
    /// would emit to rebuild itself: modes, palette, scrollback, screen, cursor). No reply. ptyd
    /// keeps the newest one and empties the ring, since the ring's bytes are now inside it.
    Checkpoint {
        /// Session.
        id: SessionId,
        /// The state.
        state: Vec<u8>,
    },
    /// Resize (ptyd owns the size of record so a reattaching host sees the truth).
    Resize {
        /// Session.
        id: SessionId,
        /// New size.
        size: TermSize,
    },
    /// Send a signal to the child's process group.
    Signal {
        /// Session.
        id: SessionId,
        /// Signal number.
        signal: i32,
    },
    /// Kill the child and forget the session.
    Close {
        /// Session.
        id: SessionId,
    },
    /// Enumerate sessions.
    List,
    /// Exit the daemon after closing every session.
    Shutdown,
}

/// ptyd → hostd.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PtydEvent {
    /// Reply to `Hello`.
    Hello {
        /// Daemon's protocol.
        protocol: u16,
        /// Daemon pid.
        pid: u32,
    },
    /// Spawned.
    Spawned {
        /// Session.
        id: SessionId,
        /// Child pid.
        pid: u32,
    },
    /// The master fd is attached to this frame. `checkpoint` is the newest state a previous
    /// host left (empty if none); `backlog` is every byte since it — tapped by that host, then
    /// read by ptyd while detached; `dropped` is how many bytes fell off the ring before that.
    Attached {
        /// Session.
        id: SessionId,
        /// The last host's terminal state, replayed before `backlog`.
        checkpoint: Vec<u8>,
        /// Buffered output.
        backlog: Vec<u8>,
        /// Bytes lost before `backlog`.
        dropped: u64,
        /// Size of record.
        size: TermSize,
    },
    /// Generic success.
    Ok,
    /// Session list.
    Sessions(Vec<SessionInfo>),
    /// A child exited. Unsolicited; may arrive at any time.
    Exited {
        /// Session.
        id: SessionId,
        /// Exit status, or the signal number negated.
        status: i32,
    },
    /// Request failed.
    Error {
        /// Related session.
        id: Option<SessionId>,
        /// Message.
        message: String,
    },
}

/// One session as ptyd sees it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Id.
    pub id: SessionId,
    /// Child pid.
    pub pid: u32,
    /// Slave device path.
    pub tty: PathBuf,
    /// Size of record.
    pub size: TermSize,
    /// A host holds the master.
    pub attached: bool,
    /// Exit status if the child is gone.
    pub exited: Option<i32>,
    /// Backlog bytes held.
    pub backlog: usize,
    /// Checkpoint bytes held.
    pub checkpoint: usize,
}
