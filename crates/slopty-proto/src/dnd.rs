//! The worker and its drag helper (`slopty-worker dnd`), on a Unix socket in the worker's
//! runtime directory: local only, framed by [`crate::codec`].
//!
//! The helper is an accessory `NSApplication` apart from the process that serves the streams,
//! so AppKit's windows and drag sessions never stall a stream or a virtual display. It posts no
//! events itself: the worker's injector presses into the helper's source window and carries the
//! drag, and the helper says what the drag session did. Every message names its drag, and a
//! message for a drag that is not the current one is dropped.

use serde::{Deserialize, Serialize};

use crate::drag::{DragId, DragOp};

/// One thing the helper's drag carries, as its source declares it at the press.
///
/// Nothing is written for a target until it reads: a file's URL and each type are answered from
/// what the worker has handed over by then ([`ToHelper::Data`]), so a file still arriving is
/// declared now and named when it is whole.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SourceItem {
    /// Where the item's file lands on this Mac when it names or promises one: its URL is what
    /// the item gives as `public.file-url`, known at the press for a file whose name the drop
    /// said; `None` for data, and for a promised file until its name is known.
    pub file: Option<String>,
    /// Whether the item is a file at all (named or promised), so it offers `public.file-url`.
    pub is_file: bool,
    /// The uniform type identifiers of its data, richest first.
    pub types: Vec<String>,
    /// The data already here, by type.
    pub given: Vec<Given>,
}

/// One representation's bytes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Given {
    /// Its uniform type identifier.
    pub uti: String,
    /// The bytes.
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

/// Worker → helper.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ToHelper {
    /// Put the source window under the global point `(x, y)` with these items, for the worker's
    /// press into it: answered [`FromHelper::Ready`] once it is up.
    SourceAt {
        /// The drag.
        drag: DragId,
        /// Global points from the main display's top left.
        x: f64,
        /// Global points.
        y: f64,
        /// What the drag carries.
        items: Vec<SourceItem>,
    },
    /// What item `item` gives as `uti` from now on: its file's path for `public.file-url` once
    /// the file is whole, or a representation's bytes once they arrived; `None` when they will
    /// not come, and a target reading it gets nothing.
    Data {
        /// The drag.
        drag: DragId,
        /// Which item, counted from 0.
        item: u16,
        /// Which type.
        uti: String,
        /// The path or the bytes.
        #[serde(with = "serde_bytes")]
        bytes: Option<Vec<u8>>,
    },
    /// The drag is over for the worker: the source goes out of sight, and so does a catcher.
    Stop {
        /// The drag.
        drag: DragId,
    },
    /// Put the catcher under the global point `(x, y)` for the worker's own drag `drag`,
    /// calling its promised files into `dir`: answered [`FromHelper::Ready`] once it is up.
    CatcherAt {
        /// The drag.
        drag: DragId,
        /// Global points.
        x: f64,
        /// Global points.
        y: f64,
        /// Where promised files go.
        dir: String,
    },
}

/// Helper → worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FromHelper {
    /// The source window is up under the point, and the worker presses into it now; or the
    /// catcher is, and the worker carries its drag onto it.
    Ready {
        /// The drag.
        drag: DragId,
    },
    /// The press began a drag session.
    Began {
        /// The drag.
        drag: DragId,
    },
    /// What the target under the drag would do, read off the system cursor, on each change.
    Operation {
        /// The drag.
        drag: DragId,
        /// What it would do.
        op: DragOp,
    },
    /// The session ended: what the target did.
    Ended {
        /// The drag.
        drag: DragId,
        /// What it did.
        op: DragOp,
    },
    /// A target read `uti` of item `item` before the worker had it: the worker sends it as
    /// soon as it can, and the target waits for it.
    Asked {
        /// The drag.
        drag: DragId,
        /// Which item.
        item: u16,
        /// Which type.
        uti: String,
    },
    /// The catcher took the drop: files by path where they are, data whole.
    Caught {
        /// The drag.
        drag: DragId,
        /// The files the drag named, left in place.
        files: Vec<String>,
        /// Every other representation of the items without a file.
        data: Vec<CaughtData>,
        /// Promised files being called in, each followed by one [`FromHelper::Promised`].
        promises: u16,
    },
    /// A promised file called into the catcher's folder, or why it is not there.
    Promised {
        /// The drag.
        drag: DragId,
        /// Its path, when it arrived.
        path: Option<String>,
        /// Why not, when it did not.
        error: Option<String>,
    },
}

/// One representation the catcher kept.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CaughtData {
    /// Which item of the drag.
    pub item: u16,
    /// Its uniform type identifier.
    pub uti: String,
    /// The bytes; `None` past the catcher's cap, with [`Self::size`] saying how many.
    #[serde(with = "serde_bytes")]
    pub bytes: Option<Vec<u8>>,
    /// Bytes in it.
    pub size: u64,
}

impl FromHelper {
    /// The drag it is about.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        match self {
            Self::Ready { drag }
            | Self::Began { drag }
            | Self::Operation { drag, .. }
            | Self::Ended { drag, .. }
            | Self::Asked { drag, .. }
            | Self::Caught { drag, .. }
            | Self::Promised { drag, .. } => *drag,
        }
    }
}
