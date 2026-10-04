//! A drag from this client over a remote window or display, the client's half
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! As a drag enters a remote tile, what it carries is read off its pasteboard ([`read`]): each
//! file it names, each file it promises, and every other item's representations, small ones
//! inline and the rest sent up at once as bulk streams. The worker begins a real drag session of
//! those items under the point, and the files go up as the drag's own transfer from that moment.
//! [`Hover`] keeps one such drag: the moves worth sending, what the worker last said a drop there
//! would do (what the client's own drag shows, and whether its drop is refused here), and how
//! the drop ended.

use std::path::PathBuf;

use slopty_core::WallMs;
use slopty_platform::pasteboard::{FILE_URL_UTI, Pasteboard, carried, clip_type};
use slopty_proto::drag::{
    DragEvent, DragId, DragInput, DragItem, DragOp, DragOps, FileMeta, Promised,
};
use slopty_proto::transfer::{ClipType, INLINE_CLIP_BYTES, MAX_CLIP_ITEMS, MODE_BITS, Rep};

use crate::clip::digest;

/// The largest representation a drag carries; past it, it is left off and the target sees the
/// item's other ones.
pub const MAX_DRAG_REP: u64 = 64 << 20;

/// The type a file promise names its file's content type under.
const PROMISED_CONTENT_TYPE: &str = "com.apple.pasteboard.promised-file-content-type";

/// A representation too big to ride inline, sent up as the drag enters.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Push {
    /// Which item.
    pub item: u16,
    /// Which representation.
    pub kind: ClipType,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// What a drag carries, as read off its pasteboard when it entered.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Read {
    /// The items, as the worker's drag carries them.
    pub items: Vec<DragItem>,
    /// The files the items name, here, in item order: the drag's upload.
    pub files: Vec<PathBuf>,
    /// The representations that go up as bulk streams.
    pub pushes: Vec<Push>,
}

impl Read {
    /// Whether any item promises a file, which comes only once the drop calls it in.
    #[must_use]
    pub fn promises(&self) -> bool {
        self.items.iter().any(|i| i.promised.is_some())
    }

    /// Whether it carries anything a remote app could take.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Whether a type is a file promise's.
fn promise_type(uti: &str) -> bool {
    uti.starts_with("com.apple.pasteboard.promised-")
        || uti == "com.apple.NSFilePromiseItemMetaData"
}

/// The `file://` URL a pasteboard names `path` by; `None` for a relative path.
#[must_use]
pub fn file_url(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// The file a `file://` URL names, by path.
fn file_path(url: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(url).ok()?.trim();
    let path = url::Url::parse(text).ok().filter(|u| u.scheme() == "file")?.to_file_path().ok()?;
    Some(path)
}

/// `path` as a drag's file: its name, size (none for a folder: the upload counts its files),
/// mode and time.
#[must_use]
pub fn file_meta(path: &std::path::Path) -> Option<FileMeta> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path).ok()?;
    let name = path.file_name()?.to_str()?.to_owned();
    Some(FileMeta {
        name,
        size: if meta.is_dir() { 0 } else { meta.len() },
        folder: meta.is_dir(),
        mode: meta.permissions().mode() & MODE_BITS,
        mtime_ms: meta.modified().map_or(WallMs::ZERO, WallMs::of),
        path: None,
    })
}

/// What the drag on `board` carries: see the module docs.
///
/// Inline bytes stop at
/// [`INLINE_CLIP_BYTES`] for the whole drag, and a representation past [`MAX_DRAG_REP`] is left
/// off. A file URL that names nothing here is left off, as is any item past
/// [`MAX_CLIP_ITEMS`].
pub fn read(board: &(impl Pasteboard + ?Sized)) -> Read {
    let mut read = Read::default();
    let mut inline_left = INLINE_CLIP_BYTES;
    for (n, types) in board.items().into_iter().take(MAX_CLIP_ITEMS).enumerate() {
        let item = u16::try_from(read.items.len()).unwrap_or(u16::MAX);
        let has = |uti: &str| types.iter().any(|t| t == uti);
        if has(FILE_URL_UTI) {
            let path = board.item_data(n, FILE_URL_UTI).and_then(|url| file_path(&url));
            if let Some(path) = path
                && let Some(meta) = file_meta(&path)
            {
                read.files.push(path);
                read.items.push(DragItem { file: Some(meta), promised: None, reps: Vec::new() });
            }
            continue;
        }
        if types.iter().any(|t| promise_type(t)) {
            let kind = has(PROMISED_CONTENT_TYPE)
                .then(|| board.item_data(n, PROMISED_CONTENT_TYPE))
                .flatten()
                .and_then(|b| String::from_utf8(b).ok())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "public.data".to_owned());
            read.items.push(DragItem { file: None, promised: Some(kind), reps: Vec::new() });
            continue;
        }
        let mut reps = Vec::new();
        for uti in types.iter().filter(|t| carried(t)) {
            let Some(slopty_platform::pasteboard::Capped::Data(bytes)) =
                board.item_data_within(n, uti, MAX_DRAG_REP)
            else {
                continue;
            };
            let kind = clip_type(uti);
            let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let hash = Some(digest(&bytes));
            let inline = if bytes.len() <= inline_left {
                inline_left = inline_left.saturating_sub(bytes.len());
                Some(bytes)
            } else {
                read.pushes.push(Push { item, kind: kind.clone(), bytes });
                None
            };
            reps.push(Rep { kind, size: Some(size), hash, inline });
        }
        if !reps.is_empty() {
            read.items.push(DragItem { file: None, promised: None, reps });
        }
    }
    read
}

/// Where a drop is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Over the tile.
    Hovering,
    /// Dropped: waiting for the worker to say how it landed.
    Dropped,
    /// Over: landed, refused, or left.
    Ended,
}

/// How a drop ended, as the person is told.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// It landed: what the target did.
    Landed(DragOp),
    /// The target took nothing.
    Refused,
    /// It could not land: why.
    Failed(String),
}

/// One drag from this client over a remote tile. See the module docs.
#[derive(Clone, Copy, Debug)]
pub struct Hover {
    drag: DragId,
    allowed: DragOps,
    /// The worker's last word on what a drop here does; a copy until it speaks, so the drop
    /// decides when it never does.
    op: DragOp,
    /// The point last sent.
    sent: (f32, f32),
    phase: Phase,
    /// Where it was dropped.
    dropped: Option<(f32, f32)>,
}

impl Hover {
    /// A drag entering at `at` (stream pixels) carrying `read`, which the client's source lets
    /// a target copy, link or move as `allowed` says: the hover, and the entry to send.
    #[must_use]
    pub fn enter(at: (f32, f32), read: &Read, allowed: DragOps) -> (Self, DragInput) {
        let drag = DragId::new();
        let this = Self {
            drag,
            allowed,
            op: DragOp::Copy.within(allowed),
            sent: at,
            phase: Phase::Hovering,
            dropped: None,
        };
        let enter = DragInput::Enter { drag, x: at.0, y: at.1, allowed, items: read.items.clone() };
        (this, enter)
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.drag
    }

    /// Where it is.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Where it was dropped, while the worker lands it.
    #[must_use]
    pub const fn dropped(&self) -> Option<(f32, f32)> {
        match self.phase {
            Phase::Dropped => self.dropped,
            Phase::Hovering | Phase::Ended => None,
        }
    }

    /// What a drop here does, as the client's drag shows it: the worker's last word.
    #[must_use]
    pub const fn op(&self) -> DragOp {
        self.op
    }

    /// The drag moved to `at`: the move to send, unless it is where the last one went.
    pub fn moved(&mut self, at: (f32, f32)) -> Option<DragInput> {
        #[expect(clippy::float_cmp, reason = "the very point sent, not a near one")]
        let same = self.sent.0 == at.0 && self.sent.1 == at.1;
        if self.phase != Phase::Hovering || same {
            return None;
        }
        self.sent = at;
        Some(DragInput::Move { drag: self.drag, x: at.0, y: at.1 })
    }

    /// Dropped at `at`, with the files its promises wrote: the drop to send, or `None` when the
    /// worker said a drop here does nothing, and the client refuses it as a local target would.
    pub fn drop(&mut self, at: (f32, f32), promised: Vec<Promised>) -> Option<DragInput> {
        if self.phase != Phase::Hovering || !self.op.takes() {
            return None;
        }
        self.phase = Phase::Dropped;
        self.dropped = Some(at);
        Some(DragInput::Drop { drag: self.drag, x: at.0, y: at.1, promised })
    }

    /// The drag left the tile, or the drop could not go on: the leave to send, once.
    pub fn leave(&mut self) -> Option<DragInput> {
        let was = std::mem::replace(&mut self.phase, Phase::Ended);
        (was != Phase::Ended).then_some(DragInput::Leave { drag: self.drag })
    }

    /// The worker said `event` of this drag: the outcome, when the drop is over.
    pub fn heard(&mut self, event: &DragEvent) -> Option<Outcome> {
        match event {
            DragEvent::Operation { drag, op } if *drag == self.drag => {
                self.op = op.within(self.allowed);
                None
            }
            DragEvent::Ended { drag, op, error } if *drag == self.drag => {
                if self.phase == Phase::Ended {
                    return None;
                }
                self.phase = Phase::Ended;
                Some(match error {
                    Some(error) => Outcome::Failed(error.clone()),
                    None if op.takes() => Outcome::Landed(*op),
                    None => Outcome::Refused,
                })
            }
            DragEvent::Operation { .. }
            | DragEvent::Ended { .. }
            | DragEvent::OutBegan { .. }
            | DragEvent::OutCaught { .. }
            | DragEvent::OutFailed { .. } => None,
        }
    }
}

pub mod out;
pub mod term;

#[cfg(test)]
mod tests;
