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

/// The most a session's backlog may be set to hold: an `Attached` frame carries the whole
/// backlog beside the checkpoint, and the two must fit one frame.
pub const MAX_BACKLOG_BYTES: usize = DEFAULT_BACKLOG_BYTES;

/// Room an `Attached` frame needs besides its checkpoint and backlog: the variant, the id, the
/// two lengths, `dropped`, the size and the start time, well under this.
const ATTACHED_ENVELOPE: usize = 4 << 10;

/// The largest checkpoint ptyd keeps.
///
/// It is what is left of a frame once the largest backlog and the envelope are in it, so an
/// `Attached` reply always fits. A worker does not send a larger one, and ptyd ignores one that
/// comes anyway.
pub const MAX_CHECKPOINT_BYTES: usize = slopty_proto::codec::MAX_FRAME_BYTES
    .saturating_sub(MAX_BACKLOG_BYTES)
    .saturating_sub(ATTACHED_ENVELOPE);

const _: () = assert!(
    MAX_CHECKPOINT_BYTES >= 8 << 20,
    "a checkpoint holds a full scrollback's worth of state"
);

/// Where the daemon listens: `$TMPDIR/slopty/ptyd.sock` (per-user, mode 0700 on macOS).
#[must_use]
pub fn socket_path() -> PathBuf {
    std::env::var_os("SLOPTY_PTYD_SOCKET")
        .map_or_else(|| std::env::temp_dir().join("slopty").join("ptyd.sock"), PathBuf::from)
}

/// The worker → ptyd. Nothing is versioned: ptyd and the worker are built and installed
/// together.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PtydRequest {
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
        #[serde(with = "byte_string")]
        bytes: Vec<u8>,
    },
    /// The session's whole terminal state as a VT byte stream (what the attached worker's engine
    /// would emit to rebuild itself: modes, palette, scrollback, screen, cursor). No reply. ptyd
    /// keeps the newest one and empties the ring, since the ring's bytes are now inside it.
    Checkpoint {
        /// The session it is about.
        id: SessionId,
        /// The state.
        #[serde(with = "byte_string")]
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
        #[serde(with = "byte_string")]
        checkpoint: Vec<u8>,
        /// Buffered output.
        #[serde(with = "byte_string")]
        backlog: Vec<u8>,
        /// Bytes lost before `backlog`.
        dropped: u64,
        /// Size of record.
        size: TermSize,
        /// Milliseconds since the Unix epoch when ptyd spawned the child. ptyd outlives the
        /// worker, so this is the one place a session's start survives a worker restart.
        started_ms: u64,
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

/// `Vec<u8>` fields as byte strings. Postcard writes a byte string as it writes the sequence of
/// `u8` serde derives for a `Vec<u8>` (the length, then the bytes), so the wire is the same;
/// both ends copy the bytes at once instead of making a call per byte, which cost 1.5 ms per
/// MiB (MEASUREMENTS.md, "the ptyd tap, framed once").
mod byte_string {
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        struct Bytes;
        impl serde::de::Visitor<'_> for Bytes {
            type Value = Vec<u8>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a byte string")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                Ok(v.to_vec())
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                Ok(v)
            }
        }
        deserializer.deserialize_byte_buf(Bytes)
    }
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

/// [`PtydRequest::Output`] as its codec frame, ready for [`crate::PtydClient::output`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OutputFrame(Vec<u8>);

impl OutputFrame {
    /// The frame for `bytes` read from `id`'s master, encoded from the borrow: the tap copies
    /// the bytes once, into the frame, where building a request first and encoding it after
    /// copied them twice.
    ///
    /// # Errors
    ///
    /// A frame over the codec's limit.
    pub fn new(id: SessionId, bytes: &[u8]) -> Result<Self, PtyError> {
        output_frame(id, bytes).map(Self)
    }

    /// The frame as the socket carries it.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn output_frame(id: SessionId, bytes: &[u8]) -> Result<Vec<u8>, PtyError> {
    use slopty_proto::codec::{CodecError, MAX_FRAME_BYTES, PREFIX_BYTES};

    /// Serializes as `PtydRequest::Output` does: the variant's index, then its fields.
    struct Output<'a> {
        id: SessionId,
        bytes: &'a [u8],
    }
    impl Serialize for Output<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut v = serializer.serialize_struct_variant("PtydRequest", 2, "Output", 2)?;
            v.serialize_field("id", &self.id)?;
            v.serialize_field("bytes", &Bytes(self.bytes))?;
            v.end()
        }
    }
    /// The bytes as [`byte_string`] writes them.
    struct Bytes<'a>(&'a [u8]);
    impl Serialize for Bytes<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            byte_string::serialize(self.0, serializer)
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
        assert_eq!(OutputFrame::new(id, &bytes).unwrap().as_bytes(), &*owned);
    }

    /// What the tap costs per 64 KiB read. Before: the session actor copied the read into a
    /// `Vec`, and the frame took the bytes one serde call at a time, as the derived `Vec<u8>`
    /// does. After: the actor frames the read once, as a byte string. Run with `cargo nextest
    /// run -p slopty-pty --release --run-ignored only tap_copy_cost --no-capture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn tap_copy_cost() {
        #[derive(Serialize)]
        enum Derived {
            _Spawn,
            _Attach,
            Output { id: SessionId, bytes: Vec<u8> },
        }
        let id = SessionId::new();
        let read: Vec<u8> = (0..64_u32 << 10).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let rounds = 5_000_u32;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let bytes = std::hint::black_box(read.clone());
            std::hint::black_box(
                slopty_proto::codec::encode(&Derived::Output { id, bytes }).unwrap(),
            );
        }
        let before = started.elapsed() / rounds;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(OutputFrame::new(id, std::hint::black_box(&read)).unwrap());
        }
        let after = started.elapsed() / rounds;
        eprintln!(
            "tap_copy_cost: 64 KiB read, copied then encoded per byte {} ns, framed once {} ns",
            before.as_nanos(),
            after.as_nanos()
        );
    }

    /// The byte strings are on the wire as the derived `Vec<u8>` was: a request encoded before
    /// the change decodes, and encodes to the same bytes.
    #[test]
    fn byte_strings_keep_the_wire_of_a_sequence_of_bytes() {
        #[derive(Serialize)]
        enum Derived {
            _Spawn,
            _Attach,
            _Output,
            Checkpoint { id: SessionId, state: Vec<u8> },
        }
        let id = SessionId::new();
        let state = (0..300_u16).map(|i| u8::try_from(i % 256).unwrap()).collect::<Vec<u8>>();
        let derived =
            slopty_proto::codec::encode(&Derived::Checkpoint { id, state: state.clone() }).unwrap();
        let ours = slopty_proto::codec::encode(&PtydRequest::Checkpoint { id, state }).unwrap();
        assert_eq!(derived, ours);
        let mut buf = bytes::BytesMut::from(&*derived);
        let back: PtydRequest = slopty_proto::codec::try_decode(&mut buf).unwrap().unwrap();
        assert!(matches!(back, PtydRequest::Checkpoint { state, .. } if state.len() == 300));
    }

    /// The largest checkpoint and the largest backlog go in one `Attached` reply.
    #[test]
    fn the_largest_attach_fits_one_frame() {
        let size = TermSize {
            cols: u16::MAX,
            rows: u16::MAX,
            metrics: slopty_proto::input::CellMetrics {
                cell_width: u16::MAX,
                cell_height: u16::MAX,
            },
        };
        let attached = PtydEvent::Attached {
            id: SessionId::new(),
            checkpoint: vec![0x1b; MAX_CHECKPOINT_BYTES],
            backlog: vec![b'x'; MAX_BACKLOG_BYTES],
            dropped: u64::MAX,
            size,
            started_ms: u64::MAX,
        };
        slopty_proto::codec::encode(&attached).unwrap();
    }
}
