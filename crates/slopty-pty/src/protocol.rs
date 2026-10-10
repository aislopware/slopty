//! The worker ↔ ptyd messages. Framed with [`slopty_proto::codec`] over a Unix stream socket; the
//! PTY master rides as `SCM_RIGHTS` ancillary data on the first byte of an `Attached` frame.

use std::path::PathBuf;

use serde::ser::SerializeStructVariant as _;
use serde::{Deserialize, Serialize, Serializer};
use slopty_core::{SessionId, WallMs};
use slopty_proto::ptyd::PtydError;
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
/// two lengths, `dropped`, the size, the start time and the `TERM` (at most
/// [`crate::pty::MAX_TERM_BYTES`]), well under this.
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

/// Where the daemon listens: `$SLOPTY_PTYD_SOCKET`, else `ptyd.sock` in this user's own directory.
///
/// ptyd makes that directory 0700. It is `$TMPDIR/slopty` on macOS, whose `$TMPDIR` is
/// per-user, and `$XDG_RUNTIME_DIR/slopty` on Linux, else `/tmp/slopty-<uid>`.
///
/// The same rule as `slopty_platform::dirs::runtime_dir`, spelled here because ptyd stays
/// clear of that crate and the AppKit it links on macOS.
#[must_use]
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SLOPTY_PTYD_SOCKET") {
        return PathBuf::from(path);
    }
    let tmp = std::env::temp_dir();
    #[cfg(target_os = "linux")]
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|run| run.is_absolute())
        .map_or_else(
            || tmp.join(format!("slopty-{}", rustix::process::getuid().as_raw())),
            |run| run.join("slopty"),
        );
    #[cfg(not(target_os = "linux"))]
    let dir = tmp.join("slopty");
    dir.join("ptyd.sock")
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
    /// Resize (ptyd owns the size of record so a reattaching worker sees the truth). No reply:
    /// the attached worker's taps ride the same connection behind it.
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
    /// Take back a session whose master this worker holds already, from a connection that is
    /// gone: ptyd ran a new build in place ([`Self::Succeed`]), which closed it. Answer: `Ok`,
    /// or `Error` (`NoSuchSession`: ptyd started afresh, so hand it over with [`Self::Adopt`];
    /// `AttachedElsewhere`). ptyd stays out of the master until the connection drops, as after
    /// an `Attach`. The taps sent while the connection was gone are lost, so the worker
    /// checkpoints next.
    Reclaim {
        /// The session it is about.
        id: SessionId,
    },
    /// Keep a session ptyd does not hold, whose master rides on this frame: a ptyd that started
    /// afresh after the one before it ended is handed back what the worker held. Answer: `Ok`
    /// or `Error`. The connection holds the master from then on, as after an `Attach`. The child
    /// is no child of this ptyd, so its end is seen when its pid is gone, and its exit status is
    /// not known: it is reported as -1, as when a wait fails.
    Adopt {
        /// The session it is about.
        id: SessionId,
        /// Its child, which ptyd signals on `Close`.
        pid: u32,
        /// Size of record.
        size: TermSize,
        /// When the child was spawned.
        started_ms: WallMs,
        /// The terminfo name the child was given as `TERM`.
        term: String,
    },
    /// Become the build at `program`, keeping every session: ptyd runs it in place, so its pid,
    /// its children and their masters stay, and hands it every session's state
    /// ([`Bequest`], [`Heir`]). No answer when it does: the connection closes as the new build
    /// starts, and that build says its custody beside the socket. Answer: `Error` when it
    /// cannot, `program` above all handing sessions on another way (its succession, which
    /// `slopty-ptyd --custody` prints, differs).
    ///
    /// The last request, so a request added later moves nothing an older ptyd reads of it.
    Succeed {
        /// The new build's `slopty-ptyd`.
        program: PathBuf,
    },
}

/// The flag a ptyd running a new build in place ([`PtydRequest::Succeed`]) runs it with,
/// followed by the descriptor of the handover's state file ([`inherit_args`]).
pub const INHERIT_FLAG: &str = "--inherit";

/// The command line, after the program, a ptyd runs the build it hands over to with.
///
/// It is [`INHERIT_FLAG`] and the descriptor `exec` keeps open for the state file. Everything else
/// the new build needs is in that file ([`Bequest`]), so nothing of it depends on the flags a
/// build takes.
#[must_use]
pub fn inherit_args(state: i32) -> Vec<String> {
    vec![INHERIT_FLAG.to_owned(), state.to_string()]
}

/// What a ptyd running a new build in place hands it, first in the state file.
///
/// The state file is already unlinked, kept open across `exec` and named by
/// [`inherit_args`]; it holds this frame and then one [`Heir`] per
/// session. Each session's master is kept open across `exec` too, its descriptor in its heir:
/// the process stays the same, so nothing has to cross a socket. Kept apart from the worker's
/// protocol: a change here changes the succession fingerprint, and an install then restarts
/// ptyd rather than hand it over.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bequest {
    /// Where the daemon listens.
    pub socket: PathBuf,
    /// Bytes of output kept per session.
    pub backlog_bytes: u64,
    /// Where the shell integration scripts go.
    pub shell_dir: PathBuf,
    /// How many [`Heir`] frames follow.
    pub sessions: u32,
    /// Every descriptor kept open across `exec` for the new build besides the state file, its
    /// masters: written first, so each is taken (and one whose heir does not read closed) even
    /// when a heir after this frame does not read.
    pub fds: Vec<i32>,
}

/// How a session's child ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Exit {
    /// Its exit status, or the signal number negated; `None` when it is not known: an adopted
    /// child ([`PtydRequest::Adopt`]) is no child of ptyd, and its end shows only as its process
    /// gone.
    pub status: Option<i32>,
}

impl Exit {
    /// An end whose status is not known.
    pub const UNKNOWN: Self = Self { status: None };

    /// An end with `status`.
    #[must_use]
    pub const fn with(status: i32) -> Self {
        Self { status: Some(status) }
    }
}

/// One session as a ptyd hands it to the build it runs next, a frame of the state file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Heir {
    /// The id the worker chose when it spawned the session.
    pub id: SessionId,
    /// The descriptor its master is open on, kept across `exec`.
    pub master: i32,
    /// Its child.
    pub pid: u32,
    /// Slave device path.
    pub tty: PathBuf,
    /// When the child was spawned.
    pub started_ms: WallMs,
    /// The terminfo name the child was given as `TERM`.
    pub term: String,
    /// Size of record.
    pub size: TermSize,
    /// The last worker's terminal state.
    #[serde(with = "byte_string")]
    pub checkpoint: Vec<u8>,
    /// Output since it.
    #[serde(with = "byte_string")]
    pub backlog: Vec<u8>,
    /// Bytes lost before `backlog`.
    pub dropped: u64,
    /// How the child ended, once it did.
    pub exited: Option<Exit>,
    /// A worker held the master: the new build keeps out of it a while for that worker to
    /// take it back ([`PtydRequest::Reclaim`]) rather than read it beside the worker.
    pub attached: bool,
    /// The child is no child of this process (it was adopted, [`PtydRequest::Adopt`]).
    pub orphan: bool,
    /// An adopted child's start as the kernel recorded it when it was adopted
    /// ([`crate::process::start_mark`]): it is that child only while its pid carries this mark.
    pub orphan_mark: Option<u64>,
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
        /// When ptyd spawned the child. ptyd outlives the worker, so this is the one place a
        /// session's start survives a worker restart.
        started_ms: WallMs,
        /// The terminfo name ptyd gave the child as `TERM`; a worker that adopts the shell
        /// answers for that terminal.
        term: String,
    },
    /// Generic success.
    Ok,
    /// Session list.
    Sessions(Vec<SessionInfo>),
    /// A child exited. Unsolicited; may arrive at any time.
    Exited {
        /// The session it is about.
        id: SessionId,
        /// How it ended.
        exit: Exit,
    },
    /// Request failed.
    Error {
        /// Related session.
        id: Option<SessionId>,
        /// Why.
        error: PtydError,
    },
}

/// `Vec<u8>` fields as byte strings. Postcard writes a byte string as it writes the sequence of
/// `u8` serde derives for a `Vec<u8>` (the length, then the bytes), so the wire is the same;
/// both ends copy the bytes at once instead of making a call per byte, which cost 1.5 ms per
/// MiB (MEASUREMENTS.md, "the ptyd tap, framed once").
mod byte_string {
    use serde::{Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
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
    /// How the child ended, if it is gone.
    pub exited: Option<Exit>,
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

/// [`PtydRequest::Checkpoint`] as its codec frame, in two parts: the head, then the state where
/// it already is. A state is megabytes, and encoding the request copied it into a buffer that
/// grew as it went.
#[derive(Debug)]
pub struct CheckpointFrame<'a> {
    head: Vec<u8>,
    state: &'a [u8],
}

impl<'a> CheckpointFrame<'a> {
    /// The frame for `id`'s `state`.
    ///
    /// # Errors
    ///
    /// A frame over the codec's limit.
    pub fn new(id: SessionId, state: &'a [u8]) -> Result<Self, PtyError> {
        use slopty_proto::codec::{CodecError, MAX_FRAME_BYTES, PREFIX_BYTES};

        /// Serializes as `PtydRequest::Checkpoint` does up to its state: the variant's index,
        /// then the id.
        struct Head {
            id: SessionId,
        }
        impl Serialize for Head {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut v =
                    serializer.serialize_struct_variant("PtydRequest", 3, "Checkpoint", 2)?;
                v.serialize_field("id", &self.id)?;
                v.end()
            }
        }

        let encode = |e| PtyError::Codec(CodecError::Encode(e));
        let head = postcard::to_extend(&Head { id }, vec![0; PREFIX_BYTES]).map_err(encode)?;
        // A byte string is its length, then its bytes.
        let mut head = postcard::to_extend(&state.len(), head).map_err(encode)?;
        let body = head.len().saturating_sub(PREFIX_BYTES).saturating_add(state.len());
        if body > MAX_FRAME_BYTES {
            return Err(PtyError::Codec(CodecError::TooLarge { len: body, max: MAX_FRAME_BYTES }));
        }
        let prefix = u32::try_from(body).unwrap_or(u32::MAX).to_le_bytes();
        if let Some(at) = head.get_mut(..PREFIX_BYTES) {
            at.copy_from_slice(&prefix);
        }
        Ok(Self { head, state })
    }

    /// The frame as the socket carries it, in order.
    #[must_use]
    pub fn parts(&self) -> [&[u8]; 2] {
        [&self.head, self.state]
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

    #[test]
    fn the_checkpoint_frame_in_parts_is_the_requests_frame() {
        let id = SessionId::new();
        for len in [0_usize, 5, 200, 70_000] {
            let state = b"\x1b[1mstate\x1b[0m".repeat(len / 14 + 1);
            let owned =
                slopty_proto::codec::encode(&PtydRequest::Checkpoint { id, state: state.clone() })
                    .unwrap();
            let frame = CheckpointFrame::new(id, &state).unwrap();
            assert_eq!(frame.parts().concat(), &*owned, "{len} bytes");
        }
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
            started_ms: WallMs::from_millis(u64::MAX),
            term: "x".repeat(crate::pty::MAX_TERM_BYTES),
        };
        slopty_proto::codec::encode(&attached).unwrap();
    }
}
