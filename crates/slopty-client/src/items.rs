//! One host's item registry as a client sees it.
//!
//! The host is authoritative: the client applies every [`ItemSync`] it receives and proposes
//! changes as [`ItemOp`]s. So that a rename or a new note shows at once, the client applies its
//! own proposals immediately (optimistic) and recognises the host's echo of them.

use std::collections::BTreeMap;

use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync};

/// The registry mirror.
#[derive(Clone, Debug, Default)]
pub struct ItemDoc {
    version: u64,
    items: BTreeMap<ItemId, Item>,
}

/// What changed after applying a sync or an op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemChange {
    /// Everything (snapshot).
    Reset,
    /// An item this registry did not have. `by_me` when this client caused it (its own
    /// proposal, or the terminal the host made for its `OpenSession`), which is what decides
    /// where the layout puts it.
    Added {
        /// The item.
        id: ItemId,
        /// This client caused it.
        by_me: bool,
    },
    /// An item it had changed (a rename, a note's text, sleep).
    Changed(ItemId),
    /// One item disappeared.
    Removed(ItemId),
    /// The host echoed our own op, or an op on an item already gone: nothing to do.
    Echo,
    /// Another client pointed at this item. Ephemeral: nothing in the registry changed, and
    /// the item may be one this registry does not have.
    Pointed(ItemId),
}

impl ItemDoc {
    /// Registry version (0 before the first snapshot).
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Items in id order.
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.values()
    }

    /// One item.
    #[must_use]
    pub fn get(&self, id: ItemId) -> Option<&Item> {
        self.items.get(&id)
    }

    /// Number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True with no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The item showing `session`, if any.
    #[must_use]
    pub fn item_for_session(&self, session: SessionId) -> Option<&Item> {
        self.items
            .values()
            .find(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session))
    }

    /// Apply a host sync. `me` is this client's id, used to recognise echoes.
    pub fn apply_sync(&mut self, sync: ItemSync, me: ClientId) -> ItemChange {
        match sync {
            ItemSync::Snapshot { version, items } => {
                self.version = version;
                self.items = items.into_iter().map(|i| (i.id, i)).collect();
                ItemChange::Reset
            }
            ItemSync::Delta { version, by, op } => {
                self.version = version;
                let known = match &op {
                    ItemOp::Upsert(item) => self.items.contains_key(&item.id),
                    ItemOp::Remove(id) | ItemOp::Sleep { id, .. } => self.items.contains_key(id),
                };
                if by == me && known {
                    // Already applied optimistically. Re-apply anyway so a host-side
                    // sanitising (a trimmed name) wins.
                    self.apply_op(&op, true);
                    return ItemChange::Echo;
                }
                self.apply_op(&op, by == me)
            }
            ItemSync::Pointed { client, .. } if client == me => ItemChange::Echo,
            ItemSync::Pointed { item, .. } => ItemChange::Pointed(item),
        }
    }

    /// Apply an op locally (the optimistic path with `by_me`, and the host's deltas).
    pub fn apply_op(&mut self, op: &ItemOp, by_me: bool) -> ItemChange {
        match op {
            ItemOp::Upsert(item) => match self.items.insert(item.id, item.clone()) {
                Some(_) => ItemChange::Changed(item.id),
                None => ItemChange::Added { id: item.id, by_me },
            },
            ItemOp::Remove(id) => match self.items.remove(id) {
                Some(_) => ItemChange::Removed(*id),
                None => ItemChange::Echo,
            },
            ItemOp::Sleep { id, sleeping } => match self.items.get_mut(id) {
                Some(item) => {
                    item.sleeping = *sleeping;
                    ItemChange::Changed(*id)
                }
                None => ItemChange::Echo,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn term() -> Item {
        Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session: SessionId::new() },
            sleeping: false,
            name: None,
        }
    }

    #[test]
    fn a_pointing_names_its_item_and_my_own_is_an_echo() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = ItemDoc::default();
        let item = ItemId::new();
        let mine = ItemSync::Pointed { client: me, name: "me".to_owned(), item };
        assert_eq!(doc.apply_sync(mine, me), ItemChange::Echo);
        let theirs = ItemSync::Pointed { client: other, name: "phone".to_owned(), item };
        assert_eq!(doc.apply_sync(theirs, me), ItemChange::Pointed(item), "even unknown");
        assert_eq!(doc.version(), 0, "a pointing never touches the registry version");
        assert!(doc.is_empty());
    }

    /// A snapshot replaces everything; another client's upsert is an addition not by me; my
    /// own optimistic upsert is an addition by me whose echo is nothing, and the host's
    /// sanitised copy wins; the terminal the host made for my `OpenSession` (never applied
    /// here first) is an addition by me.
    #[test]
    fn snapshot_then_deltas_and_echoes() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = ItemDoc::default();
        let (a, b) = (term(), term());
        let change = doc
            .apply_sync(ItemSync::Snapshot { version: 3, items: vec![b.clone(), a.clone()] }, me);
        assert_eq!(change, ItemChange::Reset);
        assert_eq!((doc.version(), doc.len()), (3, 2));

        let c = term();
        let delta = ItemSync::Delta { version: 4, by: other, op: ItemOp::Upsert(c.clone()) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Added { id: c.id, by_me: false });

        let mut note = Item { kind: ItemKind::Note { text: String::new() }, ..term() };
        assert_eq!(
            doc.apply_op(&ItemOp::Upsert(note.clone()), true),
            ItemChange::Added { id: note.id, by_me: true }
        );
        note.name = Some("plan".to_owned());
        let echo = ItemSync::Delta { version: 5, by: me, op: ItemOp::Upsert(note.clone()) };
        assert_eq!(doc.apply_sync(echo, me), ItemChange::Echo);
        assert_eq!(doc.get(note.id).and_then(|i| i.name.as_deref()), Some("plan"));

        let opened = term();
        let delta = ItemSync::Delta { version: 6, by: me, op: ItemOp::Upsert(opened.clone()) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Added { id: opened.id, by_me: true });
        let session = match opened.kind {
            ItemKind::Terminal { session } => session,
            _ => SessionId::nil(),
        };
        assert_eq!(doc.item_for_session(session).map(|i| i.id), Some(opened.id));

        let renamed = Item { name: Some("logs".to_owned()), ..a.clone() };
        let delta = ItemSync::Delta { version: 7, by: other, op: ItemOp::Upsert(renamed) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(a.id));
        let slept = ItemOp::Sleep { id: b.id, sleeping: true };
        let delta = ItemSync::Delta { version: 8, by: other, op: slept };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(b.id));
        assert_eq!(doc.get(b.id).map(|i| i.sleeping), Some(true));

        let delta = ItemSync::Delta { version: 9, by: other, op: ItemOp::Remove(a.id) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Removed(a.id));
        let again = ItemSync::Delta { version: 10, by: other, op: ItemOp::Remove(a.id) };
        assert_eq!(doc.apply_sync(again, me), ItemChange::Echo, "gone already");
        assert_eq!(doc.version(), 10);
    }
}
