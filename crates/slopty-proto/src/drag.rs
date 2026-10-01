//! Drag and drop at a point in a streamed window or display, both ways
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! A drop from the client is a real drag session on the worker: the client's
//! [`DragInput`]s ride the stream's numbered input ([`crate::screen::ScreenInput::Drag`]),
//! the files go up as the drag's own transfer ([`crate::transfer::Dest::Drag`]) from the moment
//! the drag enters, and the worker answers with what the app under the pointer would do and how
//! the drop ended ([`DragEvent`], inside [`crate::screen::ScreenEvent::Drag`]). A drag that
//! begins in an app on the worker is told to the client as it begins, and caught on the worker
//! when the client's pointer takes it out of the tile.

use core::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
use uuid::Uuid;

use crate::transfer::Rep;

/// One drag, either way: sixteen random bytes, as a transfer's id is.
///
/// A drop's files go up as a transfer whose destination names it
/// ([`crate::transfer::Dest::Drag`]), and its data is fetched under it
/// ([`crate::transfer::Source::Drag`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DragId(Uuid);

impl DragId {
    /// A fresh, time-ordered identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID.
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for DragId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DragId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DragId({})", self.0)
    }
}

impl fmt::Display for DragId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

bitflags! {
    /// The operations a drag's source allows, as `NSDragOperation` and `UIDropOperation` name
    /// them.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct DragOps: u8 {
        /// The target may copy what is dragged.
        const COPY = 1 << 0;
        /// The target may link to it.
        const LINK = 1 << 1;
        /// The target may move it, and the source then deletes it.
        const MOVE = 1 << 2;
    }
}

/// What a drop does, or would do where the pointer is now.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum DragOp {
    /// Nothing takes it: the target refuses, or there is none.
    #[default]
    None,
    /// The target takes a copy.
    Copy,
    /// The target links to it.
    Link,
    /// The target moves it.
    Move,
}

impl DragOp {
    /// Whether the drop does anything.
    #[must_use]
    pub const fn takes(self) -> bool {
        !matches!(self, Self::None)
    }

    /// This operation when `allowed` lets the source give it, else `None`.
    #[must_use]
    pub const fn within(self, allowed: DragOps) -> Self {
        let bit = match self {
            Self::None => return Self::None,
            Self::Copy => DragOps::COPY,
            Self::Link => DragOps::LINK,
            Self::Move => DragOps::MOVE,
        };
        if allowed.contains(bit) { self } else { Self::None }
    }
}

/// A file a drag carries.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileMeta {
    /// Its name: in a drop, the top-level name in the drag's upload, and so the name it lands
    /// under on the worker.
    pub name: String,
    /// Bytes in it; zero for a folder, whose files the upload counts as they go.
    pub size: u64,
    /// A folder, which keeps its tree.
    pub folder: bool,
    /// Unix permission bits, within [`crate::transfer::MODE_BITS`].
    pub mode: u32,
    /// Last modification; zero when unknown.
    pub mtime_ms: WallMs,
    /// Where it is on the machine it is dragged from, for the other end to fetch it there by
    /// path (`XferMsg::Fetch`): a file a drag out of a worker's app names. `None` in a drop from
    /// the client, whose files come up as the drag's own upload.
    pub path: Option<String>,
}

/// One thing a drag carries, as the dragging app put it on its drag pasteboard: a file, a file
/// it promises, or data in representations.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DragItem {
    /// The file it names.
    pub file: Option<FileMeta>,
    /// The content type of a file it promises rather than names (Mail's attachments, Photos'
    /// pictures): written only once a drop names a folder, so it comes up after the drop, with
    /// its name in [`DragInput::Drop`].
    pub promised: Option<String>,
    /// Its data, richest first as the app ranked it, inline while the drag's inline budget
    /// ([`crate::transfer::INLINE_CLIP_BYTES`]) lasts; bigger ones follow as bulk streams of
    /// [`crate::transfer::Purpose::Rep`] under [`crate::transfer::Source::Drag`]. Empty for a
    /// file, whose own path means nothing on the other machine.
    pub reps: Vec<Rep>,
}

/// A file a drop's item promised, as it arrived on the client at the drop.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Promised {
    /// Which item of the drag, counted from 0.
    pub item: u16,
    /// The file as written, now going up with the drag's files; `None` when the promise was
    /// not kept, and the item is left out of the drop.
    pub file: Option<FileMeta>,
}

/// Client → worker: a drag over a streamed window or display, in the stream's pixels, inside
/// [`crate::screen::ScreenInput::Drag`] so it is numbered with the stream's other input.
///
/// One drag crosses a worker at a time, since it has one pointer: an `Enter` while another
/// client's drag is on is answered [`DragEvent::Ended`] with an error.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum DragInput {
    /// A drag from the client entered the tile: the worker begins a drag session of `items`
    /// under the point, which the drag's moves then carry.
    Enter {
        /// The drag.
        drag: DragId,
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
        /// What the client's source lets a target do.
        allowed: DragOps,
        /// What it carries.
        items: Vec<DragItem>,
    },
    /// The drag moved: newest wins, so it may overtake an older move.
    Move {
        /// The drag.
        drag: DragId,
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
    },
    /// The drag left the tile, or was cancelled: the worker ends its session with nothing
    /// dropped, and the upload stops.
    Leave {
        /// The drag.
        drag: DragId,
    },
    /// Dropped at the point: the worker lets go there once every file of the drag is whole,
    /// and answers [`DragEvent::Ended`].
    Drop {
        /// The drag.
        drag: DragId,
        /// Where, in stream pixels.
        x: f32,
        /// Where.
        y: f32,
        /// The files the items promised, called in at the drop and going up now.
        promised: Vec<Promised>,
    },
    /// The client's pointer left the tile while the worker's own drag `drag` was on
    /// ([`DragEvent::OutBegan`]): the worker catches it, and the client drags it on from here.
    Catch {
        /// The worker's drag.
        drag: DragId,
    },
}

impl DragInput {
    /// The drag it is about.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        match self {
            Self::Enter { drag, .. }
            | Self::Move { drag, .. }
            | Self::Leave { drag }
            | Self::Drop { drag, .. }
            | Self::Catch { drag } => *drag,
        }
    }
}

/// Worker → client, inside [`crate::screen::ScreenEvent::Drag`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DragEvent {
    /// What the app under the drag would do with a drop there now, sent as it changes: the
    /// client's own drag shows it, and a drop it would refuse is refused on the client.
    Operation {
        /// The drag.
        drag: DragId,
        /// What it would do.
        op: DragOp,
    },
    /// The drop is over on the worker: what the target did (`None` when it refused, or
    /// nothing was dropped), and for a person to read why not, when it failed.
    Ended {
        /// The drag.
        drag: DragId,
        /// What the target did.
        op: DragOp,
        /// Why it did not land.
        error: Option<String>,
    },
    /// An app on the worker began a drag under this client's press: what it carries.
    OutBegan {
        /// The worker's drag.
        drag: DragId,
        /// What it carries, as far as the worker reads before the drop.
        items: Vec<DragItem>,
    },
    /// The worker caught its drag ([`DragInput::Catch`]): what the client drops where its
    /// pointer lands, files by their path on the worker, data whole.
    OutCaught {
        /// The worker's drag.
        drag: DragId,
        /// What was caught.
        items: Vec<DragItem>,
    },
    /// The worker's drag could not be caught.
    OutFailed {
        /// The worker's drag.
        drag: DragId,
        /// For a person to read.
        error: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An operation the source does not allow is none; none is never allowed or refused.
    #[test]
    fn an_operation_holds_within_what_the_source_allows() {
        assert_eq!(DragOp::Copy.within(DragOps::COPY), DragOp::Copy);
        assert_eq!(DragOp::Move.within(DragOps::COPY | DragOps::LINK), DragOp::None);
        assert_eq!(DragOp::Link.within(DragOps::all()), DragOp::Link);
        assert_eq!(DragOp::None.within(DragOps::all()), DragOp::None);
        assert!(DragOp::Copy.takes() && !DragOp::None.takes());
    }
}
