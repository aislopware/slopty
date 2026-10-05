//! The worker's item registry: what exists on a worker for its clients to show (terminals, streamed
//! windows and displays, files, folders, pages and reviews). Worker-authoritative, snapshot
//! + deltas.
//!
//! Where an item is shown is not here: each client arranges the items of every worker it reaches
//! in a layout of its own (`slopty_client::layout`), so a phone and a Mac share the set and not
//! the arrangement.

use std::collections::BTreeMap;

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
    /// An agent's thread, shown as the thread view: what is in it comes from the thread's own
    /// frames (`crate::thread::wire::ThreadRequest::Follow`), not the registry. A thread whose
    /// agent runs in a terminal names it, and the terminal's tile is one action away.
    Thread {
        /// The thread.
        thread: crate::thread::ThreadId,
    },
    /// A folder's changes in its repository, reviewed with no thread: what is in it comes from
    /// its repository (`crate::git::GitOp::Changes`), not the registry.
    Changes {
        /// A folder in the repository: absolute, or `~/…`.
        path: String,
    },
}

/// One item on a worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Item {
    /// Identity.
    pub id: ItemId,
    /// Content.
    pub kind: ItemKind,
    /// The name the human gave the item, shown as its title over whatever its content would
    /// say (a shell's title, a window's, a file's name); trimmed and at
    /// most [`NAME_MAX`] characters, or none.
    pub name: Option<String>,
    /// What the person said of the item, as open key and value: `project` pins it to a
    /// project, so every client groups it there. At most [`FACTS_MAX`], each within
    /// [`fact_fits`].
    pub facts: BTreeMap<String, String>,
}

/// The longest name an item takes, in characters.
pub const NAME_MAX: usize = 128;

/// The longest key of an item's fact, in characters.
pub const FACT_KEY_MAX: usize = 64;

/// The longest value of an item's fact, in bytes: a project's key holds a path.
pub const FACT_VALUE_MAX: usize = 1024;

/// The most facts one item keeps.
pub const FACTS_MAX: usize = 32;

/// Whether `key` and `value` may be an item's fact.
///
/// A key is one to [`FACT_KEY_MAX`] characters with no space or control character in it, and a
/// value up to [`FACT_VALUE_MAX`] bytes that is not blank. Either is trimmed already: the
/// worker trims what is typed.
#[must_use]
pub fn fact_fits(key: &str, value: &str) -> bool {
    let key_fits = (1..=FACT_KEY_MAX).contains(&key.chars().count())
        && !key.chars().any(|c| c.is_whitespace() || c.is_control());
    let value_fits = !value.trim().is_empty()
        && value.len() <= FACT_VALUE_MAX
        && !value.chars().any(char::is_control);
    key_fits && value_fits
}

/// A proposed change, carrying only what it changes, so two clients editing different fields
/// of one item both land. The worker validates and rebroadcasts as [`ItemSync::Delta`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemOp {
    /// A new item, or a just-closed one put back under its old id. Refused for an id the
    /// registry holds, so a stale copy never overwrites a live item.
    Add(Item),
    /// Remove.
    Remove(ItemId),
    /// Name the item, or clear its name with `None`.
    Rename {
        /// Item.
        id: ItemId,
        /// The name as typed; the worker trims it, and a blank one is none.
        name: Option<String>,
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
    /// Say a fact of the item (`project` = `atlas` pins it to a project), or take it back
    /// with `None`. The worker trims both; a blank value is none, and a fact past
    /// [`fact_fits`] or a new one past [`FACTS_MAX`] is refused.
    SetFact {
        /// Item.
        id: ItemId,
        /// The fact's key.
        key: String,
        /// Its value, or `None` to take it back.
        value: Option<String>,
    },
}

impl ItemOp {
    /// The item the op is about.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        match self {
            Self::Add(item) => item.id,
            Self::Remove(id)
            | Self::Rename { id, .. }
            | Self::SetUrl { id, .. }
            | Self::SetFolder { id, .. }
            | Self::SetFact { id, .. } => *id,
        }
    }
}

/// Why an [`ItemOp`] does not apply to an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Refused {
    /// The op edits a field this kind of item does not have (an address on a terminal).
    #[error("not a {0}")]
    WrongKind(&'static str),
    /// [`ItemOp::Add`] and [`ItemOp::Remove`] act on the registry, not on an item.
    #[error("adds and removes act on the registry, not on an item")]
    NotAnEdit,
    /// A fact past [`fact_fits`], or a new one on an item that holds [`FACTS_MAX`].
    #[error("a fact out of bounds")]
    FactBounds,
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
    /// [`Refused::WrongKind`] for an address or a folder on another kind of
    /// item, [`Refused::NotAnEdit`] for an add or a remove, and [`Refused::FactBounds`] for a
    /// fact out of bounds.
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
            (ItemOp::Rename { name, .. }, _) => set(&mut self.name, name),
            (ItemOp::SetUrl { url, .. }, ItemKind::Browser { url: at }) => set(at, url),
            (ItemOp::SetFolder { path, .. }, ItemKind::Folder { path: at }) => set(at, path),
            (ItemOp::SetFact { key, value: None, .. }, _) => self.facts.remove(key).is_some(),
            (ItemOp::SetFact { key, value: Some(value), .. }, _) => {
                let room = self.facts.contains_key(key) || self.facts.len() < FACTS_MAX;
                if !fact_fits(key, value) || !room {
                    return Err(Refused::FactBounds);
                }
                self.facts.insert(key.clone(), value.clone()).as_ref() != Some(value)
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: ItemKind) -> Item {
        Item { id: ItemId::nil(), kind, name: None, facts: BTreeMap::new() }
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

        let mut folder = item(ItemKind::Folder { path: "~/".to_owned() });
        let into = ItemOp::SetFolder { id, path: "~/src".to_owned() };
        assert_eq!((folder.apply(&into), folder.apply(&into)), (Ok(true), Ok(false)));

        let before = page.clone();
        assert_eq!(page.apply(&into), Err(Refused::WrongKind("folder")));
        assert_eq!(folder.apply(&moved), Err(Refused::WrongKind("browser")));
        assert_eq!(page.apply(&ItemOp::Remove(id)), Err(Refused::NotAnEdit));
        assert_eq!(page.apply(&ItemOp::Add(before.clone())), Err(Refused::NotAnEdit));
        assert_eq!(page, before, "a refused op leaves the item as it was");
        assert_eq!(Refused::WrongKind("folder").to_string(), "not a folder");
    }

    /// A fact is said, said again to no change, and taken back; one out of bounds, or a new
    /// one past the most an item keeps, is refused and leaves the item as it was.
    #[test]
    fn a_fact_is_said_and_taken_back_within_its_bounds() {
        let id = ItemId::nil();
        let mut window = item(ItemKind::Window { window: WindowId(7) });
        let pin = |value: Option<&str>| ItemOp::SetFact {
            id,
            key: "project".to_owned(),
            value: value.map(str::to_owned),
        };
        assert_eq!(window.apply(&pin(Some("atlas"))), Ok(true));
        assert_eq!(window.apply(&pin(Some("atlas"))), Ok(false), "once");
        assert_eq!(window.facts.get("project").map(String::as_str), Some("atlas"));
        assert_eq!(window.apply(&pin(None)), Ok(true));
        assert_eq!(window.apply(&pin(None)), Ok(false), "already gone");

        let fact = |key: String, value: String| ItemOp::SetFact { id, key, value: Some(value) };
        for bad in [
            fact("a".repeat(FACT_KEY_MAX + 1), "v".to_owned()),
            fact("two words".to_owned(), "v".to_owned()),
            fact(String::new(), "v".to_owned()),
            fact("k".to_owned(), "v".repeat(FACT_VALUE_MAX + 1)),
            fact("k".to_owned(), "  ".to_owned()),
        ] {
            assert_eq!(window.apply(&bad), Err(Refused::FactBounds), "{bad:?}");
        }
        assert!(window.facts.is_empty());
        for n in 0..FACTS_MAX {
            assert_eq!(window.apply(&fact(format!("k{n}"), "v".to_owned())), Ok(true));
        }
        let one_more = fact("more".to_owned(), "v".to_owned());
        assert_eq!(window.apply(&one_more), Err(Refused::FactBounds));
        assert_eq!(window.apply(&fact("k0".to_owned(), "w".to_owned())), Ok(true), "a change");
        assert_eq!(window.facts.len(), FACTS_MAX);
    }
}
