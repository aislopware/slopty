//! Clipboard, files and port tunnels: what makes a remote machine feel local.
//!
//! Control messages ([`ClipMsg`], [`XferMsg`]) ride the control stream. Bytes do not: anything
//! bigger than a clipboard's worth of text goes on a unidirectional stream of its own that opens
//! with [`UniHead::Bulk`] and is sent at a lower priority, so terminal rows, input and video
//! never queue behind a file. A forwarded TCP connection is a bidirectional stream that opens
//! with [`TunnelOpen`] and carries raw bytes both ways.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId, WorkerId, XferId};

/// Clipboard contents at most this big ride inline in an [`Offer`] or [`ClipMsg::Data`];
/// anything bigger is fetched over a bulk stream.
pub const INLINE_CLIP_BYTES: usize = 64 * 1024;

/// A BLAKE3 digest.
pub type Hash = [u8; 32];

/// The private pasteboard type every Slopty write carries, holding [`origin_bytes`]: a watcher
/// that finds it knows the change came from Slopty and does not announce it back.
pub const ORIGIN_TYPE: &str = "com.aislopware.slopty.origin";

/// What [`ORIGIN_TYPE`] holds: whose clipboard the contents came from and which of its changes
/// they were, `(Peer, u64)` in the wire encoding.
#[must_use]
pub fn origin_bytes(peer: Peer, generation: u64) -> Vec<u8> {
    crate::codec::encode_body(&(peer, generation)).unwrap_or_default()
}

/// The origin an [`ORIGIN_TYPE`] value names, `None` when it is not one.
#[must_use]
pub fn parse_origin(bytes: &[u8]) -> Option<(Peer, u64)> {
    crate::codec::decode_body(bytes).ok()
}

/// First message on every unidirectional stream, naming what follows.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum UniHead {
    /// Host → client: the [`crate::terminal::TermEvent`]s of one session follow.
    Session {
        /// The session.
        session: SessionId,
    },
    /// Either way: raw bytes follow, `header.size - header.offset` of them.
    Bulk(BulkHeader),
}

/// First message on a tunnel: a client-opened bidirectional stream other than the control one.
///
/// It is a TCP connection the client accepted, to be joined to `127.0.0.1:port` on the host.
/// Raw bytes follow both ways; a finished stream is a half-closed socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TunnelOpen {
    /// The host port.
    pub port: u16,
}

/// Who wrote something to a clipboard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Peer {
    /// A client app.
    Client(ClientId),
    /// A worker.
    Worker(WorkerId),
}

/// One representation of the clipboard's contents.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClipItem {
    /// Uniform type identifier (`public.utf8-plain-text`, `public.png`, `public.html`). One
    /// item per type: `public.file-url` holds every file's URL, one per line.
    pub uti: String,
    /// Size in bytes.
    pub size: u64,
    /// Digest of the bytes: equal digests are the same contents, so an echo is recognised.
    pub hash: Hash,
    /// The bytes, when they fit [`INLINE_CLIP_BYTES`] and the type is plain text.
    pub inline: Option<Vec<u8>>,
}

/// The clipboard changed: what it holds, announced and not pushed. The receiver puts promises
/// on its own clipboard and fetches a representation when something pastes it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Offer {
    /// Whose clipboard.
    pub origin: Peer,
    /// Increases with every change on `origin`; names the offer in fetches.
    pub generation: u64,
    /// The representations, richest first.
    pub items: Vec<ClipItem>,
}

/// Clipboard sync.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClipMsg {
    /// Client → host: whether this client wants the host's clipboard changes now (a remote
    /// tile has focus, or the app is frontmost). The host watches its pasteboard only while
    /// some client wants it.
    Watch(bool),
    /// Either way: the sender's clipboard changed.
    Offer(Offer),
    /// Either way: send representation `uti` of offer `generation`.
    Fetch {
        /// The offer.
        generation: u64,
        /// Which representation.
        uti: String,
    },
    /// Either way: the answer to a [`ClipMsg::Fetch`], inline when it fits
    /// [`INLINE_CLIP_BYTES`]; a bigger one arrives as a bulk stream with [`Purpose::Clip`].
    Data {
        /// The offer.
        generation: u64,
        /// Which representation.
        uti: String,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// Either way: that offer or representation is gone (the clipboard changed again).
    Unavailable {
        /// The offer.
        generation: u64,
    },
}

/// Where uploaded files land on the host.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Dest {
    /// The session's working directory (OSC 7), or a fresh `~/.slopty/drop/<xfer>/` when a
    /// name there is taken.
    SessionCwd(SessionId),
    /// A fresh `~/.slopty/drop/<xfer>/`, for a drop on a streamed window.
    Staging,
    /// This directory.
    Path(String),
}

/// What a bulk stream's bytes are for.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Purpose {
    /// Client → host: a file of an upload.
    Upload,
    /// Host → client: a file the client fetched.
    Download,
    /// Either way: a clipboard representation too big to inline.
    Clip {
        /// The offer.
        generation: u64,
        /// Which representation.
        uti: String,
    },
}

/// The header of a bulk stream.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BulkHeader {
    /// The transfer.
    pub xfer: XferId,
    /// What the bytes are for.
    pub purpose: Purpose,
    /// Path relative to the transfer's root, `/`-separated, without `..`; empty for
    /// [`Purpose::Clip`].
    pub name: String,
    /// Whole file size.
    pub size: u64,
    /// Last modification, milliseconds since the Unix epoch.
    pub mtime_ms: u64,
    /// Unix permission bits.
    pub mode: u32,
    /// The bytes that follow start here: non-zero when resuming.
    pub offset: u64,
}

/// File transfer control.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum XferMsg {
    /// The sender announces a transfer before its streams: how much, and for an upload,
    /// where to.
    Begin {
        /// The transfer.
        xfer: XferId,
        /// Where an upload lands; `None` for a download.
        dest: Option<Dest>,
        /// Files in it.
        files: u32,
        /// Total bytes.
        bytes: u64,
    },
    /// The sender asks how much of `name` the receiver already holds, to resume.
    Resume {
        /// The transfer.
        xfer: XferId,
        /// The file.
        name: String,
    },
    /// The answer to [`XferMsg::Resume`]: bytes the receiver kept, durable on its disk.
    Offset {
        /// The transfer.
        xfer: XferId,
        /// The file.
        name: String,
        /// Durable bytes.
        durable: u64,
    },
    /// The receiver's progress, at most every 100 ms.
    Progress {
        /// The transfer.
        xfer: XferId,
        /// Bytes received over the whole transfer.
        done: u64,
    },
    /// The receiver has one file whole and in place.
    Done {
        /// The transfer.
        xfer: XferId,
        /// The file, as named in its header.
        name: String,
        /// Where it landed.
        path: String,
        /// Digest of what landed, for the sender to compare.
        hash: Hash,
    },
    /// Every file of the transfer is in place: the paths to paste, top-level entries only.
    Finished {
        /// The transfer.
        xfer: XferId,
        /// Absolute paths.
        paths: Vec<String>,
    },
    /// A file, or the whole transfer when `name` is `None`, failed.
    Failed {
        /// The transfer.
        xfer: XferId,
        /// The file.
        name: Option<String>,
        /// For a person to read.
        error: String,
    },
    /// Either side stops the transfer; partial files stay for a resume.
    Cancel {
        /// The transfer.
        xfer: XferId,
    },
    /// Client → host: send this file or directory down as transfer `xfer`, from the start (a
    /// download does not resume).
    Fetch {
        /// The transfer the client names.
        xfer: XferId,
        /// Absolute path, or `~/…`.
        path: String,
    },
}
