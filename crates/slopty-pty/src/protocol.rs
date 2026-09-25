//! The worker ↔ ptyd messages. Framed with [`slopty_proto::codec`] over a Unix stream socket; the
//! PTY master rides as `SCM_RIGHTS` ancillary data on the first byte of an `Attached` frame.

use std::path::PathBuf;

use serde::ser::SerializeStructVariant as _;
use serde::{Deserialize, Serialize, Serializer};
use slopty_core::SessionId;
use slopty_proto::terminal::TermSize;

use crate::PtyError;
use crate::pty::SpawnSpec;

/// Bytes ptyd retains per session: output read while detached, or tapped by the worker since
/// its last checkpoint.
pub const DEFAULT_BACKLOG_BYTES: usize = 4 << 20;

/// Where the daemon listens: `$TMPDIR/slopty/ptyd.sock` (per-user, mode 0700 on macOS).
#[must_use]
pub fn socket_path() -> PathBuf {
    std::env::var_os("SLOPTY_PTYD_SOCKET")
        .map_or_else(|| std::env::temp_dir().join("slopty").join("ptyd.sock"), PathBuf::from)
}

/// The worker → ptyd.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PtydRequest {
    /// First message. Nothing is versioned: ptyd and the worker are built and installed
    /// together.
    Hello,
    /// Create a PTY and spawn on it. Answer: `Spawned` or `Error`.
    Spawn {
        /// Chosen by the worker.
        id: SessionId,
        /// What to run.
        spec: SpawnSpec,
    },
    /// Hand over the master and the detached backlog. Answer: `Attached` (with fd) or `Error`.
    /// ptyd stops reading the master until the connection drops.
    Attach {
        /// The session it is about.
        id: SessionId,
    },
    /// A copy of output the attached worker just read from the master. No reply. ptyd appends it
    /// to the session's ring so that a worker which dies without detaching can be replaced by one
    /// that replays everything since the last `Checkpoint`.
    Output {
        /// The session it is about.
        id: SessionId,
        /// The bytes, in read order.
        bytes: Vec<u8>,
    },
    /// The session's whole terminal state as a VT byte stream (what the attached worker's engine
    /// would emit to rebuild itself: modes, palette, scrollback, screen, cursor). No reply. ptyd
    /// keeps the newest one and empties the ring, since the ring's bytes are now inside it.
    Checkpoint {
        /// The session it is about.
        id: SessionId,
        /// The state.
        state: Vec<u8>,
    },
    /// Resize (ptyd owns the size of record so a reattaching worker sees the truth).
    Resize {
        /// The session it is about.
        id: SessionId,
        /// New size.
        size: TermSize,
    },
    /// Kill the child and forget the session.
    Close {
        /// The session it is about.
        id: SessionId,
    },
    /// Enumerate sessions.
    List,
    /// Exit the daemon after closing every session.
    Shutdown,
}

/// ptyd → the worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PtydEvent {
    /// Reply to `Hello`.
    Hello {
        /// Daemon pid.
        pid: u32,
    },
    /// Spawned.
    Spawned {
        /// The session it is about.
        id: SessionId,
        /// Child pid.
        pid: u32,
    },
    /// The master fd is attached to this frame. `checkpoint` is the newest state a previous
    /// worker left (empty if none); `backlog` is every byte since it — tapped by that worker, then
    /// read by ptyd while detached; `dropped` is how many bytes fell off the ring before that.
    Attached {
        /// The session it is about.
        id: SessionId,
        /// The last worker's terminal state, replayed before `backlog`.
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
        /// The session it is about.
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
    /// The id the worker chose when it spawned the session.
    pub id: SessionId,
    /// Child pid.
    pub pid: u32,
    /// Slave device path.
    pub tty: PathBuf,
    /// Size of record.
    pub size: TermSize,
    /// A worker holds the master.
    pub attached: bool,
    /// Exit status if the child is gone.
    pub exited: Option<i32>,
    /// Backlog bytes held.
    pub backlog: usize,
    /// Checkpoint bytes held.
    pub checkpoint: usize,
}

/// [`PtydRequest::Output`] as its codec frame, encoded from a borrow: the tap copies the bytes
/// once, into the frame, instead of into a request first and the frame after.
///
/// # Errors
///
/// A frame over the codec's limit.
pub fn output_frame(id: SessionId, bytes: &[u8]) -> Result<Vec<u8>, PtyError> {
    use slopty_proto::codec::{CodecError, MAX_FRAME_BYTES, PREFIX_BYTES};

    /// Serializes as `PtydRequest::Output` does: the variant's index, then its fields.
    struct Output<'a> {
        id: SessionId,
        bytes: &'a [u8],
    }
    impl Serialize for Output<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut v = serializer.serialize_struct_variant("PtydRequest", 3, "Output", 2)?;
            v.serialize_field("id", &self.id)?;
            v.serialize_field("bytes", self.bytes)?;
            v.end()
        }
    }

    let mut frame = Vec::with_capacity(bytes.len().saturating_add(32));
    frame.extend_from_slice(&[0; PREFIX_BYTES]);
    let mut frame = postcard::to_extend(&Output { id, bytes }, frame)
        .map_err(|e| PtyError::Codec(CodecError::Encode(e)))?;
    let body = frame.len().saturating_sub(PREFIX_BYTES);
    if body > MAX_FRAME_BYTES {
        return Err(PtyError::Codec(CodecError::TooLarge { len: body, max: MAX_FRAME_BYTES }));
    }
    let prefix = u32::try_from(body).unwrap_or(u32::MAX).to_le_bytes();
    if let Some(head) = frame.get_mut(..PREFIX_BYTES) {
        head.copy_from_slice(&prefix);
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_borrowed_output_frame_is_the_requests_frame() {
        let id = SessionId::new();
        let bytes = b"\x1b[31mred\x1b[0m".repeat(40);
        let owned =
            slopty_proto::codec::encode(&PtydRequest::Output { id, bytes: bytes.clone() }).unwrap();
        assert_eq!(output_frame(id, &bytes).unwrap(), owned.to_vec());
    }
}
