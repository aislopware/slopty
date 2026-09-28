//! Clipboard, files and port tunnels: what makes a remote machine feel local.
//!
//! Control messages ([`ClipMsg`], [`XferMsg`]) ride the control stream. Bytes do not: anything
//! bigger than a clipboard's worth of text goes on a unidirectional stream of its own that opens
//! with [`UniHead::Bulk`] and is sent at a lower priority, so terminal rows, input and video
//! never queue behind a file. A forwarded TCP connection is a bidirectional stream that opens
//! with [`TunnelOpen`] and carries raw bytes both ways.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId, WallMs, WorkerId, XferId};

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
    /// Worker → client: the [`crate::terminal::TermEvent`]s of one session follow.
    Session {
        /// The session.
        session: SessionId,
    },
    /// Either way: raw bytes follow, `header.size - header.offset` of them.
    Bulk(BulkHeader),
    /// Worker → client: the [`crate::conversation::ConversationEvent`]s of one followed agent
    /// session follow, with the same framing.
    Conversation {
        /// The terminal session the agent runs in.
        session: SessionId,
    },
}

/// First message on a tunnel: a client-opened bidirectional stream other than the control one.
///
/// It is a TCP connection the client accepted, to be joined to `127.0.0.1:port` on the worker.
/// Raw bytes follow both ways; a finished stream is a half-closed socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TunnelOpen {
    /// The worker port.
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

/// A representation clipboard sync carries, whatever the platform calls it.
///
/// Each end maps it to its own pasteboard's types at its board (an Apple UTI on a Mac, a MIME
/// type elsewhere). Anything else on a clipboard stays where it is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClipFormat {
    /// Files, as `file://` URLs one per line (`text/uri-list`).
    FileUrls,
    /// `image/png`.
    Png,
    /// `image/tiff`, what a Mac screenshot is on its pasteboard.
    Tiff,
    /// `text/rtf`.
    Rtf,
    /// `text/html`.
    Html,
    /// `text/plain`, UTF-8.
    Text,
}

impl ClipFormat {
    /// Every format, richest first: the order an offer lists them in.
    pub const ALL: [Self; 6] =
        [Self::FileUrls, Self::Png, Self::Tiff, Self::Rtf, Self::Html, Self::Text];

    /// The MIME type.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::FileUrls => "text/uri-list",
            Self::Png => "image/png",
            Self::Tiff => "image/tiff",
            Self::Rtf => "text/rtf",
            Self::Html => "text/html",
            Self::Text => "text/plain;charset=utf-8",
        }
    }

    /// A picture: what a paste into a program that reads pictures off the pasteboard takes.
    #[must_use]
    pub const fn is_picture(self) -> bool {
        matches!(self, Self::Png | Self::Tiff)
    }
}

/// One representation of the clipboard's contents.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClipItem {
    /// What it is. One item per format: [`ClipFormat::FileUrls`] holds every file's URL, one
    /// per line.
    pub format: ClipFormat,
    /// Size in bytes.
    pub size: u64,
    /// Digest of the bytes: equal digests are the same contents, so an echo is recognised.
    pub hash: Hash,
    /// The bytes, when they fit [`INLINE_CLIP_BYTES`] and the type is plain text.
    #[serde(with = "serde_bytes")]
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
    /// Client → worker: whether this client wants the worker's clipboard changes now (a remote
    /// tile has focus, or the app is frontmost). The worker watches its pasteboard only while
    /// some client wants it.
    Watch(bool),
    /// Either way: the sender's clipboard changed.
    Offer(Offer),
    /// Either way: send representation `format` of offer `generation`.
    Fetch {
        /// The offer.
        generation: u64,
        /// Which representation.
        format: ClipFormat,
    },
    /// Either way: the answer to a [`ClipMsg::Fetch`], inline when it fits
    /// [`INLINE_CLIP_BYTES`]; a bigger one arrives as a bulk stream with [`Purpose::Clip`].
    Data {
        /// The offer.
        generation: u64,
        /// Which representation.
        format: ClipFormat,
        /// The bytes.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    /// Either way: that offer or representation is gone (the clipboard changed again).
    Unavailable {
        /// The offer.
        generation: u64,
    },
}

/// Where uploaded files land on the worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Dest {
    /// The session's working directory (OSC 7), or a fresh `~/.slopty/drop/<xfer>/` when a
    /// name there is taken.
    SessionCwd(SessionId),
    /// A fresh `~/.slopty/drop/<xfer>/`, for a drop on a streamed window.
    Staging,
    /// This directory.
    Path(String),
    /// A fresh `~/.slopty/drop/<xfer>/`, with nothing put on the pasteboard: a file attached
    /// to an agent's prompt, whose path the conversation's composer types.
    Attachment,
}

/// What a bulk stream's bytes are for.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Purpose {
    /// Client → worker: a file of an upload.
    Upload,
    /// Worker → client: a file the client fetched.
    Download,
    /// Either way: a clipboard representation too big to inline.
    Clip {
        /// The offer.
        generation: u64,
        /// Which representation.
        format: ClipFormat,
    },
    /// Worker → client: the text of a file read too big to inline, announced on the control
    /// stream by [`crate::file::FileRead::Streamed`] with the same transfer.
    FileText,
    /// Client → worker: a file tile's save too big to inline, the whole new text; answered as
    /// [`crate::ClientMsg::WriteFile`] is, with `WorkerMsg::Written`.
    Save {
        /// Absolute path on the worker.
        path: String,
        /// The modification time of the version the edit started from; `None` writes
        /// regardless.
        base_modified_ms: Option<WallMs>,
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
    /// Last modification; zero when unknown.
    pub mtime_ms: WallMs,
    /// Unix permission bits, within [`MODE_BITS`].
    pub mode: u32,
    /// The bytes that follow start here: non-zero when resuming.
    pub offset: u64,
}

/// The permission bits a transferred file carries: read, write and execute for its owner, its
/// group and others. Set-id and sticky bits never travel.
pub const MODE_BITS: u32 = 0o777;

/// Most files one transfer sends: a drop or a drag of a home directory must not walk the disk.
pub const MAX_FILES: usize = 10_000;

/// A transfer's file name ([`BulkHeader::name`]) as a path under the transfer's root; `None`
/// for one that could leave it or names nothing: empty, absolute, a `.` or `..` or empty
/// component, or a NUL.
#[must_use]
pub fn relative_path(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    let refused = name.is_empty()
        || name.contains('\0')
        || name.split('/').any(|c| c.is_empty() || c == "." || c == "..")
        || !path.components().all(|c| matches!(c, Component::Normal(_)));
    (!refused).then(|| path.to_owned())
}

/// Where the bytes of a file landing at `target` are written until it is whole and checked:
/// beside it, so landing is a rename, and under a name a resumed transfer finds again.
#[must_use]
pub fn partial_of(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
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
    /// Client → worker: send this file or directory down as transfer `xfer`. A retried fetch
    /// names what the client already holds, and each of those files resumes where it stopped.
    Fetch {
        /// The transfer the client names.
        xfer: XferId,
        /// Absolute path, or `~/…`.
        path: String,
        /// Files already partly here: name (as in its bulk header) and the bytes durable on
        /// the client's disk.
        held: Vec<(String, u64)>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_could_leave_its_root_is_refused() {
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "./a", "a/.", "a/", "a\0b"] {
            assert_eq!(relative_path(bad), None, "{bad:?}");
        }
        assert_eq!(relative_path("dir/a b.txt"), Some(PathBuf::from("dir/a b.txt")));
        assert_eq!(relative_path(".hidden/x"), Some(PathBuf::from(".hidden/x")));
    }

    #[test]
    fn a_partial_sits_beside_its_target() {
        assert_eq!(partial_of(Path::new("/tmp/a.txt")), Path::new("/tmp/a.txt.partial"));
    }
}
