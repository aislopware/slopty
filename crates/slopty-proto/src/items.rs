//! The worker's item registry: what exists on a worker for its clients to show (terminals, streamed
//! windows and displays, notes, files, folders, pages and reviews). Worker-authoritative, snapshot
//! + deltas.
//!
//! Where an item is shown is not here: each client arranges the items of every worker it reaches
//! in a layout of its own (`slopty_client::layout`), so a phone and a Mac share the set and not
//! the arrangement.

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, DisplayId, ItemId, SessionId, WindowId};

/// What an item shows.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemKind {
    /// A terminal session.
    Terminal {
        /// Session.
        session: SessionId,
    },
    /// A streamed worker window.
    Window {
        /// Window.
        window: WindowId,
    },
    /// A streamed display.
    Display {
        /// Display.
        display: DisplayId,
    },
    /// Free text.
    Note {
        /// Markdown.
        text: String,
    },
    /// A text file on the worker, open to edit: the text comes over `WorkerMsg::File`, not the
    /// registry, and goes back as `ClientMsg::WriteFile`.
    File {
        /// Absolute path on the worker.
        path: String,
    },
    /// A web page, usually a server running on the worker reached through a forwarded port.
    Browser {
        /// The address as the worker sees it (`http://localhost:5173/`). A loopback address is
        /// the worker's own: each client opens it through its forward of that port, whatever
        /// local port the forward took.
        url: String,
    },
    /// A directory on the worker, browsed in place: what is in it comes over
    /// `WorkerMsg::Folder`, not the registry, and browsing moves the item
    /// ([`ItemOp::SetFolder`]).
    Folder {
        /// Absolute path on the worker, or `~/…` in its home.
        path: String,
    },
    /// The review of a thread's changes: what is in it comes from the thread's review frames
    /// (`crate::thread::wire::ThreadRequest::Review`), not the registry.
    Review {
        /// The thread.
        thread: crate::thread::ThreadId,
    },
}

/// One item on a worker.
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

/// A proposed change, carrying only what it changes, so two clients editing different fields
/// of one item both land. The worker validates and rebroadcasts as [`ItemSync::Delta`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemOp {
    /// A new item, or a just-closed one put back under its old id. Refused for an id the
    /// registry holds, so a stale copy never overwrites a live item.
    Add(Item),
    /// Remove.
    Remove(ItemId),
    /// Sleep or wake.
    Sleep {
        /// Item.
        id: ItemId,
        /// True to sleep.
        sleeping: bool,
    },
    /// Name the item, or clear its name with `None`.
    Rename {
        /// Item.
        id: ItemId,
        /// The name as typed; the worker trims it, and a blank one is none.
        name: Option<String>,
    },
    /// Replace a note's text. Refused for any other kind of item.
    SetNote {
        /// Item.
        id: ItemId,
        /// Markdown.
        text: String,
    },
    /// Point a browser item at another address, as typed in its header. Refused for any
    /// other kind of item and for anything but an `http` or `https` address.
    SetUrl {
        /// Item.
        id: ItemId,
        /// The address as the worker sees it (`http://localhost:5173/`).
        url: String,
    },
    /// Point a folder item at another directory, as browsing into one does. Refused for any
    /// other kind of item.
    SetFolder {
        /// Item.
        id: ItemId,
        /// Absolute path on the worker.
        path: String,
    },
}

impl ItemOp {
    /// The item the op is about.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        match self {
            Self::Add(item) => item.id,
            Self::Remove(id)
            | Self::Sleep { id, .. }
            | Self::Rename { id, .. }
            | Self::SetNote { id, .. }
            | Self::SetUrl { id, .. }
            | Self::SetFolder { id, .. } => *id,
        }
    }
}

/// Why an [`ItemOp`] does not apply to an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Refused {
    /// The op edits a field this kind of item does not have (a note's text on a terminal).
    #[error("not a {0}")]
    WrongKind(&'static str),
    /// [`ItemOp::Add`] and [`ItemOp::Remove`] act on the registry, not on an item.
    #[error("adds and removes act on the registry, not on an item")]
    NotAnEdit,
}

impl Item {
    /// Apply an edit to this item; whether anything changed.
    ///
    /// Setting a field to the value it holds changes nothing and says so: the worker and every
    /// client apply the same op, so a client redraws only for what moved, and one that applied
    /// its own op ahead of the worker sees the worker's echo as the no-op it is. The worker
    /// still broadcasts such an op, since the echo is how the proposer hears it was taken.
    ///
    /// # Errors
    /// [`Refused::WrongKind`] for a note's text, an address or a folder on another kind of
    /// item, and [`Refused::NotAnEdit`] for an add or a remove.
    pub fn apply(&mut self, op: &ItemOp) -> Result<bool, Refused> {
        fn set<T: PartialEq + Clone>(field: &mut T, value: &T) -> bool {
            let changed = field != value;
            if changed {
                field.clone_from(value);
            }
            changed
        }
        Ok(match (op, &mut self.kind) {
            (ItemOp::Add(_) | ItemOp::Remove(_), _) => return Err(Refused::NotAnEdit),
            (ItemOp::Sleep { sleeping, .. }, _) => set(&mut self.sleeping, sleeping),
            (ItemOp::Rename { name, .. }, _) => set(&mut self.name, name),
            (ItemOp::SetNote { text, .. }, ItemKind::Note { text: at }) => set(at, text),
            (ItemOp::SetUrl { url, .. }, ItemKind::Browser { url: at }) => set(at, url),
            (ItemOp::SetFolder { path, .. }, ItemKind::Folder { path: at }) => set(at, path),
            (ItemOp::SetNote { .. }, _) => return Err(Refused::WrongKind("note")),
            (ItemOp::SetUrl { .. }, _) => return Err(Refused::WrongKind("browser")),
            (ItemOp::SetFolder { .. }, _) => return Err(Refused::WrongKind("folder")),
        })
    }
}

/// Worker → client registry state.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: ItemKind) -> Item {
        Item { id: ItemId::nil(), kind, sleeping: false, name: None }
    }

    /// An edit reports a change only when the value moved, whatever the field; an edit of a
    /// field the kind lacks is refused and leaves the item alone, as are adds and removes.
    #[test]
    fn an_edit_changes_only_what_moves_and_only_where_it_fits() {
        let id = ItemId::nil();
        let mut page = item(ItemKind::Browser { url: "http://localhost:5173/".to_owned() });
        let same = ItemOp::SetUrl { id, url: "http://localhost:5173/".to_owned() };
        let moved = ItemOp::SetUrl { id, url: "http://localhost:8080/".to_owned() };
        assert_eq!(page.apply(&same), Ok(false), "already there");
        assert_eq!(page.apply(&moved), Ok(true));
        assert_eq!(page.apply(&moved), Ok(false), "once");
        assert_eq!(page.kind, ItemKind::Browser { url: "http://localhost:8080/".to_owned() });

        let rename = ItemOp::Rename { id, name: Some("dev".to_owned()) };
        assert_eq!((page.apply(&rename), page.apply(&rename)), (Ok(true), Ok(false)));
        let sleep = ItemOp::Sleep { id, sleeping: true };
        assert_eq!((page.apply(&sleep), page.apply(&sleep)), (Ok(true), Ok(false)));

        let mut note = item(ItemKind::Note { text: String::new() });
        let text = ItemOp::SetNote { id, text: "plan".to_owned() };
        assert_eq!((note.apply(&text), note.apply(&text)), (Ok(true), Ok(false)));
        let mut folder = item(ItemKind::Folder { path: "~/".to_owned() });
        let into = ItemOp::SetFolder { id, path: "~/src".to_owned() };
        assert_eq!((folder.apply(&into), folder.apply(&into)), (Ok(true), Ok(false)));

        let before = page.clone();
        assert_eq!(page.apply(&text), Err(Refused::WrongKind("note")));
        assert_eq!(page.apply(&into), Err(Refused::WrongKind("folder")));
        assert_eq!(note.apply(&moved), Err(Refused::WrongKind("browser")));
        assert_eq!(page.apply(&ItemOp::Remove(id)), Err(Refused::NotAnEdit));
        assert_eq!(page.apply(&ItemOp::Add(before.clone())), Err(Refused::NotAnEdit));
        assert_eq!(page, before, "a refused op leaves the item as it was");
        assert_eq!(Refused::WrongKind("note").to_string(), "not a note");
    }
}
