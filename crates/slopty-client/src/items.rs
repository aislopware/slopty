//! One worker's item registry as a client sees it.
//!
//! The worker is authoritative: the client applies every [`ItemSync`] it receives and proposes
//! changes as [`ItemOp`]s, each carrying only the field it changes. So that a rename or a new
//! item shows at once, the client applies its own proposals immediately (optimistic) and
//! recognises the worker's echo of them.

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
    /// proposal, or the terminal the worker made for its `OpenSession`).
    Added {
        /// The item.
        id: ItemId,
        /// This client caused it.
        by_me: bool,
    },
    /// An item it had changed (a rename, a fact).
    Changed(ItemId),
    /// One item disappeared.
    Removed(ItemId),
    /// The worker echoed our own op, or an op on an item already gone: nothing to do.
    Echo,
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

    /// Apply a worker sync. `me` is this client's id, used to recognise echoes.
    pub fn apply_sync(&mut self, sync: ItemSync, me: ClientId) -> ItemChange {
        match sync {
            ItemSync::Snapshot { version, items } => {
                self.version = version;
                self.items = items.into_iter().map(|i| (i.id, i)).collect();
                ItemChange::Reset
            }
            ItemSync::Delta { version, by, op } => {
                self.version = version;
                let known = self.items.contains_key(&op.id());
                if by == me && known {
                    // Already applied optimistically, so this is an echo unless the worker
                    // sanitised the op (a trimmed name): then its version wins.
                    return self.apply_op(&op, true);
                }
                self.apply_op(&op, by == me)
            }
        }
    }

    /// Apply an op locally (the optimistic path with `by_me`, and the worker's deltas). An op
    /// on an item this registry does not have, one that sets what is already there, or one
    /// the item's kind does not take (an address on a terminal) changes nothing.
    pub fn apply_op(&mut self, op: &ItemOp, by_me: bool) -> ItemChange {
        let id = op.id();
        match op {
            ItemOp::Add(item) => match self.items.insert(id, item.clone()) {
                Some(old) if old == *item => ItemChange::Echo,
                Some(_) => ItemChange::Changed(id),
                None => ItemChange::Added { id, by_me },
            },
            ItemOp::Remove(_) => match self.items.remove(&id) {
                Some(_) => ItemChange::Removed(id),
                None => ItemChange::Echo,
            },
            ItemOp::Rename { .. } | ItemOp::SetFact { .. } => {
                match self.items.get_mut(&id).map(|item| item.apply(op)) {
                    Some(Ok(true)) => ItemChange::Changed(id),
                    Some(Ok(false) | Err(_)) | None => ItemChange::Echo,
                }
            }
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
            name: None,
            facts: BTreeMap::new(),
        }
    }

    /// A snapshot replaces everything; another client's addition is not by me; my own
    /// optimistic addition is by me, and its echo is nothing; the echo of my rename is nothing
    /// unless the worker trimmed the name, and then its name wins; the terminal the worker made
    /// for my `OpenSession` (never applied here first) is an addition by me.
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
        let delta = ItemSync::Delta { version: 4, by: other, op: ItemOp::Add(c.clone()) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Added { id: c.id, by_me: false });

        let note = Item { kind: ItemKind::Window { window: slopty_core::WindowId(4) }, ..term() };
        assert_eq!(
            doc.apply_op(&ItemOp::Add(note.clone()), true),
            ItemChange::Added { id: note.id, by_me: true }
        );
        let echo = ItemSync::Delta { version: 5, by: me, op: ItemOp::Add(note.clone()) };
        assert_eq!(doc.apply_sync(echo, me), ItemChange::Echo);
        let typed = ItemOp::Rename { id: note.id, name: Some(" plan ".to_owned()) };
        assert_eq!(doc.apply_op(&typed, true), ItemChange::Changed(note.id));
        let trimmed = ItemOp::Rename { id: note.id, name: Some("plan".to_owned()) };
        let echo = ItemSync::Delta { version: 5, by: me, op: trimmed.clone() };
        assert_eq!(doc.apply_sync(echo, me), ItemChange::Changed(note.id), "trimmed there");
        assert_eq!(doc.get(note.id).and_then(|i| i.name.as_deref()), Some("plan"));
        let echo = ItemSync::Delta { version: 5, by: me, op: trimmed };
        assert_eq!(doc.apply_sync(echo, me), ItemChange::Echo, "as typed");

        let opened = term();
        let delta = ItemSync::Delta { version: 6, by: me, op: ItemOp::Add(opened.clone()) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Added { id: opened.id, by_me: true });
        let session = match opened.kind {
            ItemKind::Terminal { session } => session,
            _ => SessionId::nil(),
        };
        assert_eq!(doc.item_for_session(session).map(|i| i.id), Some(opened.id));

        let renamed = ItemOp::Rename { id: a.id, name: Some("logs".to_owned()) };
        let delta = ItemSync::Delta { version: 7, by: other, op: renamed };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(a.id));
        let named = ItemOp::Rename { id: b.id, name: Some("build".to_owned()) };
        let delta = ItemSync::Delta { version: 8, by: other, op: named };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(b.id));
        assert_eq!(doc.get(b.id).and_then(|i| i.name.as_deref()), Some("build"));

        let delta = ItemSync::Delta { version: 9, by: other, op: ItemOp::Remove(a.id) };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Removed(a.id));
        let again = ItemSync::Delta { version: 10, by: other, op: ItemOp::Remove(a.id) };
        assert_eq!(doc.apply_sync(again, me), ItemChange::Echo, "gone already");
        assert_eq!(doc.version(), 10);

        let gone = ItemOp::Rename { id: ItemId::new(), name: None };
        assert_eq!(doc.apply_op(&gone, false), ItemChange::Echo, "an item it lacks");
    }
}
