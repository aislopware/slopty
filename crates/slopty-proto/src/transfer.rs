//! Clipboard, files and port tunnels: what makes a remote machine feel local.
//!
//! Control messages ([`ClipMsg`], [`XferMsg`]) ride the control stream. Bytes do not: anything
//! bigger than a clipboard's worth of text goes on a unidirectional stream of its own that opens
//! with [`UniHead::Bulk`] and is sent at a lower priority, so terminal rows, input and video
//! never queue behind a file. A forwarded TCP connection is a bidirectional stream that opens
//! with [`TunnelOpen`] and carries raw bytes both ways.

use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId, WallMs, WorkerId, XferId};

use crate::drag::DragId;

/// Clipboard bytes at most this big ride inline: an [`Offer`]'s inline representations
/// together, or one [`ClipMsg::Data`]. Anything bigger is fetched over a bulk stream.
pub const INLINE_CLIP_BYTES: usize = 64 * 1024;

/// A BLAKE3 digest.
pub type Hash = [u8; 32];

/// The private pasteboard type every Slopty write carries, holding [`origin_bytes`]: a watcher
/// that finds it knows the change came from Slopty and does not announce it back.
pub const ORIGIN_TYPE: &str = "com.aislopware.slopty.origin";

/// nspasteboard.org's marker for a secret, such as a password manager's copy: offered with
/// [`Offer::concealed`] and never kept.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// nspasteboard.org's marker for contents that are about to go again: offered and kept as a
/// secret is.
pub const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// Whether a type on an Apple pasteboard travels in an offer.
///
/// Left behind: dynamic types (`dyn.…`), which name nothing on another machine; the
/// pre-UTI names AppKit still lists beside their UTIs (`NSStringPboardType`, `Apple PNG
/// pasteboard type`, `CorePasteboardFlavorType 0x…`), whose bytes the UTI already carries;
/// file promises, which only the copying process can keep; and the markers, which an offer
/// says in its own fields.
#[must_use]
pub fn carried(board_type: &str) -> bool {
    let legacy = !board_type.contains('.') || board_type.contains(' ');
    let marker = board_type == ORIGIN_TYPE || board_type.starts_with("org.nspasteboard.");
    let promise = board_type.starts_with("com.apple.pasteboard.promised-")
        || board_type == "com.apple.NSFilePromiseItemMetaData";
    !(legacy || marker || promise || board_type.starts_with("dyn."))
}

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
    /// Worker → client: the [`crate::thread::wire::ThreadFrame`]s of one followed agent thread
    /// follow, with the same framing.
    Thread {
        /// The thread.
        thread: crate::thread::ThreadId,
    },
}

/// First message on a tunnel: a client-opened bidirectional stream other than the control one.
///
/// It is a TCP connection the client accepted, to be joined to `host:port` as the worker
/// reaches it. Raw bytes follow both ways; a finished stream is a half-closed socket. A target
/// the worker cannot reach resets the stream with a [`TunnelRefusal`] code.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TunnelOpen {
    /// Where the worker dials.
    pub host: TunnelHost,
    /// The port there.
    pub port: u16,
}

/// The host a tunnel joins, as the worker reaches it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TunnelHost {
    /// The worker's own loopback: IPv4 first, then IPv6 (a dev server bound to `localhost`
    /// on macOS is often on `::1` only).
    Loopback,
    /// A name for the worker's resolver, `/etc/hosts` and its DNS included: a container, a
    /// LAN machine, anything only the worker can name.
    Name(String),
    /// An address the worker dials as it is.
    Ip(IpAddr),
}

/// Why the worker reset a tunnel before any byte moved: the stream's reset code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TunnelRefusal {
    /// The host answered and nothing listens on the port.
    Refused,
    /// The worker's resolver has no address for the name.
    Unresolved,
    /// No route to the host, or it did not answer in time.
    Unreachable,
}

impl TunnelRefusal {
    /// The reset code that carries it. Zero is no reason: a connection that broke once open.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Refused => 1,
            Self::Unresolved => 2,
            Self::Unreachable => 3,
        }
    }

    /// The reason a reset `code` carries, if it carries one.
    #[must_use]
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::Refused),
            2 => Some(Self::Unresolved),
            3 => Some(Self::Unreachable),
            _ => None,
        }
    }
}

/// Who wrote something to a clipboard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Peer {
    /// A client app.
    Client(ClientId),
    /// A worker.
    Worker(WorkerId),
}

/// A representation every end knows by name, whatever its platform calls it.
///
/// Each end maps it to its own pasteboard's types at its board (an Apple UTI on a Mac or an
/// iPhone, a MIME type elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClipFormat {
    /// A file, as its `file://` URL: one per item (`text/uri-list`).
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
    /// Every format, richest first.
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

/// A representation's type: a format every end knows, or an Apple uniform type identifier.
///
/// Between two Apple ends, the common case, every type a pasteboard holds travels as it is
/// (`com.apple.webarchive`, `com.adobe.pdf`, an app's private type), since macOS and iOS
/// pasteboards both take any UTI. An end that is not Apple's leaves an `Apple` type alone.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClipType {
    /// A format with a name on every platform.
    Format(ClipFormat),
    /// An Apple uniform type identifier that is none of the formats.
    Apple(String),
}

impl ClipType {
    /// The format, when it is one.
    #[must_use]
    pub const fn format(&self) -> Option<ClipFormat> {
        match self {
            Self::Format(format) => Some(*format),
            Self::Apple(_) => None,
        }
    }

    /// Whether it is `format`.
    #[must_use]
    pub fn is(&self, format: ClipFormat) -> bool {
        self.format() == Some(format)
    }

    /// A picture format.
    #[must_use]
    pub fn is_picture(&self) -> bool {
        self.format().is_some_and(ClipFormat::is_picture)
    }
}

/// One representation as announced.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Rep {
    /// What it is.
    pub kind: ClipType,
    /// Its size, when the sender read it (inline text, a file URL); `None` while it is lazy,
    /// read off the sender's pasteboard only when fetched.
    pub size: Option<u64>,
    /// Digest of the bytes, when read: the receiver checks what it fetches against it, and
    /// equal digests are the same contents, so an echo is recognised.
    pub hash: Option<Hash>,
    /// The bytes, when they fit the offer's inline budget ([`INLINE_CLIP_BYTES`]).
    #[serde(with = "serde_bytes")]
    pub inline: Option<Vec<u8>>,
}

/// One item on a clipboard: a file of a Finder copy, a picture of a Photos copy, the one item
/// of a text copy. Its representations, richest first as the copying app ranked them.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClipEntry {
    /// The representations.
    pub reps: Vec<Rep>,
}

/// Whose representations a fetch names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Source {
    /// A clipboard offer: `generation` of `origin`'s clipboard. A client relaying a worker's
    /// offer to another worker keeps its origin, so a fetch names where the bytes are.
    Offer {
        /// Whose clipboard.
        origin: Peer,
        /// Which of its changes.
        generation: u64,
    },
    /// A drag's items ([`crate::drag::DragItem::reps`]): a representation too big for the
    /// drag's inline budget, which the client sends up as the drag enters.
    Drag(DragId),
}

/// Representation `kind` of item `item` of `source`.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RepRef {
    /// Whose.
    pub source: Source,
    /// Which item, counted from 0.
    pub item: u16,
    /// Which representation.
    pub kind: ClipType,
}

/// Most items an offer lists: past this a copy is a file transfer's worth
/// ([`MAX_FILES`]), and the rest stay behind.
pub const MAX_CLIP_ITEMS: usize = MAX_FILES;

/// The clipboard changed: what it holds, announced and not pushed. The receiver puts promises
/// on its own clipboard and fetches a representation when something pastes it, or ahead of
/// that under a budget.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Offer {
    /// Whose clipboard.
    pub origin: Peer,
    /// Increases with every change on `origin`; names the offer in fetches.
    pub generation: u64,
    /// Milliseconds since the copy, on the sender's clock: the receiver places the copy on its
    /// own clock as arrival minus this, so the latest copy wins whichever machine made it.
    pub age_ms: u64,
    /// A secret (`org.nspasteboard.ConcealedType`) or contents about to go
    /// (`org.nspasteboard.TransientType`): no bytes inline, never fetched ahead, never logged,
    /// fetched only by a paste the person makes, and never kept on the receiver's clipboard
    /// past that.
    pub concealed: bool,
    /// The items, in pasteboard order.
    pub items: Vec<ClipEntry>,
}

impl Offer {
    /// What a fetch of this offer names it by.
    #[must_use]
    pub const fn source(&self) -> Source {
        Source::Offer { origin: self.origin, generation: self.generation }
    }

    /// Every representation with its item's index, in order.
    pub fn reps(&self) -> impl Iterator<Item = (u16, &Rep)> {
        self.items.iter().zip(0_u16..).flat_map(|(entry, n)| entry.reps.iter().map(move |r| (n, r)))
    }

    /// The representation `rep` names, when this offer lists it.
    #[must_use]
    pub fn rep(&self, rep: &RepRef) -> Option<&Rep> {
        if rep.source != self.source() {
            return None;
        }
        let entry = self.items.get(usize::from(rep.item))?;
        entry.reps.iter().find(|r| r.kind == rep.kind)
    }

    /// A reference to representation `kind` of item `item` of this offer.
    #[must_use]
    pub const fn rep_ref(&self, item: u16, kind: ClipType) -> RepRef {
        RepRef { source: self.source(), item, kind }
    }
}

/// Clipboard sync.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClipMsg {
    /// Client → worker: whether this client wants the worker's clipboard changes now (a remote
    /// tile has focus, and the app is frontmost). The worker watches its pasteboard only while
    /// some client wants it.
    Watch(bool),
    /// Either way: the sender's clipboard, as it is now. A client sends it when a tile of the
    /// worker takes the keyboard and ahead of a paste; the worker mirrors it onto its own
    /// pasteboard unless its own contents are newer.
    Offer(Offer),
    /// Either way: send representation `rep`.
    Fetch {
        /// Which.
        rep: RepRef,
        /// Answer [`ClipMsg::TooBig`] rather than send more than this: a fetch ahead of a paste,
        /// under the receiver's budget. `None` for a paste, which takes it whatever its size.
        max: Option<u64>,
        /// A paste or a drop waits on it: its bytes go ahead of background transfers.
        urgent: bool,
    },
    /// Either way: the answer to a [`ClipMsg::Fetch`], inline when it fits
    /// [`INLINE_CLIP_BYTES`]; a bigger one arrives as a bulk stream with [`Purpose::Rep`].
    Data {
        /// Which.
        rep: RepRef,
        /// The bytes.
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    },
    /// Either way: the answer to a [`ClipMsg::Fetch`] whose `max` the representation passes.
    TooBig {
        /// Which.
        rep: RepRef,
        /// Its size.
        size: u64,
    },
    /// Either way: that offer is gone (the clipboard changed again), or never was.
    Unavailable {
        /// Which.
        source: Source,
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
    /// `~/.slopty/drop/<drag>/`, with nothing put on the pasteboard: the files of a drag over a
    /// streamed window or display, sent from the moment it enters, so they are whole by the
    /// drop it lands them in.
    Drag(DragId),
}

/// What a bulk stream's bytes are for.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Purpose {
    /// Client → worker: a file of an upload.
    Upload,
    /// Worker → client: a file the client fetched.
    Download,
    /// Either way: a clipboard representation too big to inline.
    Rep {
        /// Which.
        rep: RepRef,
    },
    /// Worker → client: the bytes of a file read too big to inline (a text, or a picture or
    /// document), announced on the control stream by [`crate::file::FileRead::Streamed`] with
    /// the same transfer.
    FileBody,
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
    /// [`Purpose::Rep`].
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
    /// names what the client already holds, and each of those files resumes where it stopped
    /// while the worker's file is still the version held.
    Fetch {
        /// The transfer the client names.
        xfer: XferId,
        /// Absolute path, or `~/…`.
        path: String,
        /// Files already partly or wholly here.
        held: Vec<Held>,
    },
}

/// A file of a download the client holds part of, as a retried [`XferMsg::Fetch`] names it.
///
/// It carries the version its bytes are of, so the worker resumes it on any link and after a
/// restart of its own, and never continues one version's bytes with another's.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Held {
    /// Its name, as in its bulk header.
    pub name: String,
    /// The bytes durable on the client's disk.
    pub bytes: u64,
    /// The file's size when they were sent, as its bulk header said.
    pub size: u64,
    /// The file's modification time when they were sent, as its bulk header said.
    pub mtime_ms: WallMs,
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

    /// Every Apple type travels but dynamic ones, the pre-UTI names, file promises and the
    /// markers an offer says in its own fields.
    #[test]
    fn only_types_that_mean_something_elsewhere_travel() {
        for kept in ["public.png", "com.apple.webarchive", "com.adobe.pdf", "public.file-url"] {
            assert!(carried(kept), "{kept}");
        }
        for left in [
            "dyn.ah62d4rv4gu8y",
            "NSStringPboardType",
            "Apple PNG pasteboard type",
            "CorePasteboardFlavorType 0x75726C20",
            "com.apple.pasteboard.promised-file-url",
            ORIGIN_TYPE,
            CONCEALED_TYPE,
            TRANSIENT_TYPE,
            "org.nspasteboard.source",
        ] {
            assert!(!carried(left), "{left}");
        }
    }

    /// A reference names one representation of one item of one offer, and only that offer.
    #[test]
    fn a_rep_ref_finds_its_representation() {
        let text = ClipType::Format(ClipFormat::Text);
        let rep = |kind: ClipType| Rep { kind, size: None, hash: None, inline: None };
        let offer = Offer {
            origin: Peer::Client(ClientId::nil()),
            generation: 2,
            age_ms: 0,
            concealed: false,
            items: vec![
                ClipEntry { reps: vec![rep(text.clone())] },
                ClipEntry { reps: vec![rep(ClipType::Apple("com.adobe.pdf".to_owned()))] },
            ],
        };
        assert_eq!(offer.reps().map(|(n, _)| n).collect::<Vec<_>>(), [0, 1]);
        let pdf = offer.rep_ref(1, ClipType::Apple("com.adobe.pdf".to_owned()));
        assert!(offer.rep(&pdf).is_some());
        assert!(offer.rep(&offer.rep_ref(1, text)).is_none(), "not in that item");
        let older = RepRef { source: Source::Offer { origin: offer.origin, generation: 1 }, ..pdf };
        assert!(offer.rep(&older).is_none(), "another offer's");
    }

    #[test]
    fn a_partial_sits_beside_its_target() {
        assert_eq!(partial_of(Path::new("/tmp/a.txt")), Path::new("/tmp/a.txt.partial"));
    }
}
