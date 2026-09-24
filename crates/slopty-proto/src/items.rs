//! The host's item registry: what exists on a host for its clients to show (terminals, streamed
//! windows and displays, notes, file cards). Host-authoritative, snapshot + deltas.
//!
//! Where an item is shown is not here: each client arranges the items of every host it reaches
//! in a layout of its own (`slopty_client::layout`), so a phone and a Mac share the set and not
//! the arrangement.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, ItemId, SessionId, WindowId};

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
    /// A file on the host, read-only: the text comes over `HostMsg::File`, not the registry.
    File {
        /// Absolute path on the host.
        path: String,
    },
}

/// One item on a host.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Item {
    /// Identity.
    pub id: ItemId,
    /// Content.
    pub kind: ItemKind,
    /// Sleeping: kept but its session or stream is released.
    pub sleeping: bool,
    /// The name the human gave the item, shown as its title over whatever its content would
    /// say (a shell's title, a window's, a note's first line, a file's name); trimmed and at
    /// most [`NAME_MAX`] characters, or none.
    pub name: Option<String>,
}

/// The longest name an item takes, in characters.
pub const NAME_MAX: usize = 128;

/// A proposed change. The host validates and rebroadcasts as [`ItemSync::Delta`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemOp {
    /// Insert or replace (a rename is an upsert with the new name).
    Upsert(Item),
    /// Remove.
    Remove(ItemId),
    /// Sleep or wake.
    Sleep {
        /// Item.
        id: ItemId,
        /// True to sleep.
        sleeping: bool,
    },
}

/// Host → client registry state.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemSync {
    /// Every item (on connect and after a gap).
    Snapshot {
        /// Registry version.
        version: u64,
        /// Items.
        items: Vec<Item>,
    },
    /// One applied op.
    Delta {
        /// Version after applying.
        version: u64,
        /// Who caused it (so the originating client can skip its own echo).
        by: ClientId,
        /// The op.
        op: ItemOp,
    },
    /// One client pointed the others at an item (`ClientMsg::Point`): ephemeral, not part of
    /// the registry. Every connected client hears it, the pointer included.
    Pointed {
        /// Who.
        client: ClientId,
        /// Its name from `Hello`.
        name: String,
        /// The item.
        item: ItemId,
    },
}
