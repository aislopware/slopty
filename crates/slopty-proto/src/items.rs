//! The worker's item registry: what exists on a worker for its clients to show (terminals and
//! streamed windows and displays). Worker-authoritative, snapshot + deltas.
//!
//! Where an item is shown is not here: each client shows it where its interface puts it (a
//! terminal in its chat's terminal panel, a window in a right-pane surface), so a phone and a
//! Mac share the set and not the arrangement.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, DisplayId, ItemId, SessionId, WindowId};

/// What an item shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
}

/// One item on a worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Item {
    /// Identity.
    pub id: ItemId,
    /// Content.
    pub kind: ItemKind,
    /// The name given to the item, shown as its title over whatever its content would say (a
    /// shell's title, a window's); trimmed and at
    /// most [`NAME_MAX`] characters, or none.
    pub name: Option<String>,
    /// What is said of the item, as open key and value: `chat` names the chat a shell was opened
    /// for, so every client finds it there. At most [`FACTS_MAX`], each within [`fact_fits`].
    pub facts: BTreeMap<String, String>,
}

/// The longest name an item takes, in characters.
pub const NAME_MAX: usize = 128;

/// The longest key of an item's fact, in characters.
pub const FACT_KEY_MAX: usize = 64;

/// The longest value of an item's fact, in bytes: a value may hold a path.
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
    /// Say a fact of the item (`chat` = the chat's id), or take it back
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
            Self::Remove(id) | Self::Rename { id, .. } | Self::SetFact { id, .. } => *id,
        }
    }
}

/// Why an [`ItemOp`] does not apply to an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Refused {
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
    /// [`Refused::NotAnEdit`] for an add or a remove, and [`Refused::FactBounds`] for a fact out
    /// of bounds.
    pub fn apply(&mut self, op: &ItemOp) -> Result<bool, Refused> {
        fn set<T: PartialEq + Clone>(field: &mut T, value: &T) -> bool {
            let changed = field != value;
            if changed {
                field.clone_from(value);
            }
            changed
        }
        Ok(match op {
            ItemOp::Add(_) | ItemOp::Remove(_) => return Err(Refused::NotAnEdit),
            ItemOp::Rename { name, .. } => set(&mut self.name, name),
            ItemOp::SetFact { key, value: None, .. } => self.facts.remove(key).is_some(),
            ItemOp::SetFact { key, value: Some(value), .. } => {
                let room = self.facts.contains_key(key) || self.facts.len() < FACTS_MAX;
                if !fact_fits(key, value) || !room {
                    return Err(Refused::FactBounds);
                }
                self.facts.insert(key.clone(), value.clone()).as_ref() != Some(value)
            }
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

    /// A rename reports a change only when the name moved; adds and removes are refused and
    /// leave the item alone.
    #[test]
    fn an_edit_changes_only_what_moves() {
        let id = ItemId::nil();
        let mut shell = item(ItemKind::Terminal { session: SessionId::nil() });
        let rename = ItemOp::Rename { id, name: Some("dev".to_owned()) };
        assert_eq!((shell.apply(&rename), shell.apply(&rename)), (Ok(true), Ok(false)));

        let before = shell.clone();
        assert_eq!(shell.apply(&ItemOp::Remove(id)), Err(Refused::NotAnEdit));
        assert_eq!(shell.apply(&ItemOp::Add(before.clone())), Err(Refused::NotAnEdit));
        assert_eq!(shell, before, "a refused op leaves the item as it was");
    }

    /// A fact is said, said again to no change, and taken back; one out of bounds, or a new
    /// one past the most an item keeps, is refused and leaves the item as it was.
    #[test]
    fn a_fact_is_said_and_taken_back_within_its_bounds() {
        let id = ItemId::nil();
        let mut window = item(ItemKind::Window { window: WindowId(7) });
        let pin = |value: Option<&str>| ItemOp::SetFact {
            id,
            key: "chat".to_owned(),
            value: value.map(str::to_owned),
        };
        assert_eq!(window.apply(&pin(Some("atlas"))), Ok(true));
        assert_eq!(window.apply(&pin(Some("atlas"))), Ok(false), "once");
        assert_eq!(window.facts.get("chat").map(String::as_str), Some("atlas"));
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
