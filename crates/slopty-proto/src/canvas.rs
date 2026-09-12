//! The canvas document: what is on the plane and where. Host-authoritative, snapshot + deltas.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, ItemId, SessionId, WindowId};

/// Position and size in canvas units (1 unit = 1 logical point at zoom 1).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// What an item shows.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemKind {
    /// A terminal session.
    Terminal {
        /// Session.
        session: SessionId,
    },
    /// A streamed host window.
    Window {
        /// Window.
        window: WindowId,
    },
    /// A streamed display.
    Display {
        /// CoreGraphics display id.
        display: u32,
    },
    /// Free text.
    Note {
        /// Markdown.
        text: String,
    },
    /// A file on the host, read-only: the text comes over `HostMsg::File`, not the document.
    File {
        /// Absolute path on the host.
        path: String,
    },
}

/// One item on the canvas.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CanvasItem {
    /// Identity.
    pub id: ItemId,
    /// Content.
    pub kind: ItemKind,
    /// Placement.
    pub rect: Rect,
    /// Explicit z-order (higher is on top). Never inferred from array order.
    pub z: u32,
    /// Optional group (e.g. a repository) used for arrange-by and colouring.
    pub group: Option<String>,
    /// Sleeping: kept on the canvas but its session/stream is released.
    pub sleeping: bool,
    /// The name the human gave the card, shown as its title over whatever its content would
    /// say (a shell's title, a window's, a note's first line, a file's name); trimmed and at
    /// most [`NAME_MAX`] characters, or none.
    pub name: Option<String>,
}

/// The longest name a card takes, in characters.
pub const NAME_MAX: usize = 128;

/// A proposed change. The host validates and rebroadcasts as [`CanvasSync::Delta`].
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum CanvasOp {
    /// Insert or replace.
    Upsert(CanvasItem),
    /// Remove.
    Remove(ItemId),
    /// Move/resize only (cheap path while dragging).
    Place {
        /// Item.
        id: ItemId,
        /// New rect.
        rect: Rect,
    },
    /// Raise to top.
    Raise(ItemId),
    /// Sleep or wake.
    Sleep {
        /// Item.
        id: ItemId,
        /// True to sleep.
        sleeping: bool,
    },
}

/// Host → client canvas state.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum CanvasSync {
    /// The whole document (on connect and after a gap).
    Snapshot {
        /// Document version.
        version: u64,
        /// Items.
        items: Vec<CanvasItem>,
    },
    /// One applied op.
    Delta {
        /// Version after applying.
        version: u64,
        /// Who caused it (so the originating client can skip its own echo).
        by: ClientId,
        /// The op.
        op: CanvasOp,
    },
}
