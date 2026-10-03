//! One worker's item registry as a client sees it.
//!
//! The worker is authoritative: the client applies every [`ItemSync`] it receives and proposes
//! changes as [`ItemOp`]s, each carrying only the field it changes. So that a rename or a new
//! note shows at once, the client applies its own proposals immediately (optimistic) and
//! recognises the worker's echo of them.
//!
//! The list as last seen is kept on this device ([`ItemCache`]), so a cold launch draws each
//! tile of a worker not yet linked as what it was, under the pill saying where the worker is,
//! rather than a hole, until the worker's first snapshot replaces it.

use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::{fs, io};

use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::codec;
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
    /// proposal, or the terminal the worker made for its `OpenSession`), which is what decides
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
    /// The worker echoed our own op, or an op on an item already gone: nothing to do.
    Echo,
    /// Another client pointed at this item. Ephemeral: nothing in the registry changed, and
    /// the item may be one this registry does not have.
    Pointed(ItemId),
}

impl ItemDoc {
    /// The registry as this device last saw it ([`ItemCache`]), at version 0: the worker's
    /// first snapshot replaces it whole.
    #[must_use]
    pub fn cached(items: Vec<Item>) -> Self {
        Self { version: 0, items: items.into_iter().map(|i| (i.id, i)).collect() }
    }

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
            ItemSync::Pointed { client, .. } if client == me => ItemChange::Echo,
            ItemSync::Pointed { item, .. } => ItemChange::Pointed(item),
        }
    }

    /// Apply an op locally (the optimistic path with `by_me`, and the worker's deltas). An op
    /// on an item this registry does not have, one that sets what is already there, or one
    /// the item's kind does not take (a note's text on a terminal) changes nothing.
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
            ItemOp::Rename { .. }
            | ItemOp::SetNote { .. }
            | ItemOp::SetUrl { .. }
            | ItemOp::SetFolder { .. }
            | ItemOp::SetFact { .. } => match self.items.get_mut(&id).map(|item| item.apply(op)) {
                Some(Ok(true)) => ItemChange::Changed(id),
                Some(Ok(false) | Err(_)) | None => ItemChange::Echo,
            },
        }
    }
}

/// Why the item cache could not be written.
#[derive(Debug, thiserror::Error)]
pub enum ItemCacheError {
    /// The file system refused.
    #[error("item cache: {0}")]
    Io(#[from] io::Error),
    /// The list did not encode.
    #[error("item cache: {0}")]
    Codec(#[from] codec::CodecError),
}

/// Each worker's items as this device last saw them, a file per worker under one directory.
///
/// The directory and files are the user's alone (0700 and 0600, as a note's text is in them),
/// in postcard, replaced whole so a crash mid-write leaves the one before. A file from another
/// build, or cut short, reads as nothing and goes.
#[derive(Clone, Debug)]
pub struct ItemCache {
    dir: PathBuf,
}

impl ItemCache {
    /// The cache under `dir`, made when first written.
    #[must_use]
    pub const fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, worker: &str) -> PathBuf {
        self.dir.join(format!("{worker}.items"))
    }

    /// `worker`'s items as last kept; none when they were not, or the file is unreadable.
    #[must_use]
    pub fn items(&self, worker: &str) -> Vec<Item> {
        let path = self.file(worker);
        match fs::read(&path) {
            Ok(bytes) => codec::decode_body(&bytes).unwrap_or_else(|error| {
                tracing::debug!(%error, path = %path.display(), "item cache dropped");
                let _gone = fs::remove_file(&path);
                Vec::new()
            }),
            Err(_) => Vec::new(),
        }
    }

    /// What [`Self::write`] keeps of `items`: encoded where the registry lives, written off it.
    ///
    /// # Errors
    /// The encoder's.
    pub fn encode<'a>(items: impl Iterator<Item = &'a Item>) -> Result<Vec<u8>, ItemCacheError> {
        let items: Vec<&Item> = items.collect();
        Ok(codec::encode_body(&items)?)
    }

    /// Keep `bytes` ([`Self::encode`]) as `worker`'s items.
    ///
    /// # Errors
    /// The file system's.
    pub fn write(&self, worker: &str, bytes: &[u8]) -> Result<(), ItemCacheError> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        let path = self.file(worker);
        slopty_platform::fs::replace(&path, bytes)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    /// Where it is.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
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

        let note = Item { kind: ItemKind::Note { text: String::new() }, ..term() };
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
    }

    /// Another client's note edit changes only the text: the name this client just gave the
    /// note stays. An edit or a rename of an item this registry lacks, and a note's text for
    /// a shell, change nothing.
    #[test]
    fn a_note_edit_keeps_the_name_and_a_stray_op_changes_nothing() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = ItemDoc::default();
        let shell = term();
        let note = Item { kind: ItemKind::Note { text: "draft".to_owned() }, ..term() };
        let items = vec![shell.clone(), note.clone()];
        assert_eq!(doc.apply_sync(ItemSync::Snapshot { version: 1, items }, me), ItemChange::Reset);

        let named = ItemOp::Rename { id: note.id, name: Some("plan".to_owned()) };
        assert_eq!(doc.apply_op(&named, true), ItemChange::Changed(note.id));
        let text = "draft, then more".to_owned();
        let edit = ItemOp::SetNote { id: note.id, text: text.clone() };
        let delta = ItemSync::Delta { version: 2, by: other, op: edit };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(note.id));
        let now = doc.get(note.id).cloned();
        assert_eq!(now.as_ref().and_then(|i| i.name.as_deref()), Some("plan"));
        assert_eq!(now.map(|i| i.kind), Some(ItemKind::Note { text }));

        let stray = ItemOp::SetNote { id: shell.id, text: "no".to_owned() };
        assert_eq!(doc.apply_op(&stray, false), ItemChange::Echo);
        assert_eq!(doc.get(shell.id), Some(&shell));
        let gone = ItemId::new();
        let edit = ItemOp::SetNote { id: gone, text: String::new() };
        assert_eq!(doc.apply_op(&edit, false), ItemChange::Echo);
        let rename = ItemOp::Rename { id: gone, name: None };
        assert_eq!(doc.apply_op(&rename, false), ItemChange::Echo);
        assert_eq!(doc.len(), 2);
    }

    /// A browser's address moves with another client's `SetUrl`; the same address again, or
    /// an address for a shell, changes nothing.
    #[test]
    fn a_browser_takes_a_new_address_and_nothing_else_does() {
        let me = ClientId::new();
        let mut doc = ItemDoc::default();
        let shell = term();
        let page =
            Item { kind: ItemKind::Browser { url: "http://localhost:5173/".into() }, ..term() };
        let items = vec![shell.clone(), page.clone()];
        assert_eq!(doc.apply_sync(ItemSync::Snapshot { version: 1, items }, me), ItemChange::Reset);
        let url = "http://localhost:3000/docs".to_owned();
        let moved = ItemOp::SetUrl { id: page.id, url: url.clone() };
        let delta = ItemSync::Delta { version: 2, by: ClientId::new(), op: moved.clone() };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(page.id));
        assert_eq!(doc.get(page.id).map(|i| i.kind.clone()), Some(ItemKind::Browser { url }));
        assert_eq!(doc.apply_op(&moved, false), ItemChange::Echo, "already there");
        let stray = ItemOp::SetUrl { id: shell.id, url: "http://a.test/".into() };
        assert_eq!(doc.apply_op(&stray, false), ItemChange::Echo);
        assert_eq!(doc.get(shell.id), Some(&shell));
    }

    /// A folder moves with another client's `SetFolder`; the same folder again, or a folder
    /// for a shell, changes nothing.
    #[test]
    fn a_folder_moves_and_nothing_else_does() {
        let me = ClientId::new();
        let mut doc = ItemDoc::default();
        let shell = term();
        let folder = Item { kind: ItemKind::Folder { path: "/w".into() }, ..term() };
        let items = vec![shell.clone(), folder.clone()];
        assert_eq!(doc.apply_sync(ItemSync::Snapshot { version: 1, items }, me), ItemChange::Reset);
        let path = "/w/src".to_owned();
        let moved = ItemOp::SetFolder { id: folder.id, path: path.clone() };
        let delta = ItemSync::Delta { version: 2, by: ClientId::new(), op: moved.clone() };
        assert_eq!(doc.apply_sync(delta, me), ItemChange::Changed(folder.id));
        assert_eq!(doc.get(folder.id).map(|i| i.kind.clone()), Some(ItemKind::Folder { path }));
        assert_eq!(doc.apply_op(&moved, false), ItemChange::Echo, "already there");
        let stray = ItemOp::SetFolder { id: shell.id, path: "/tmp".into() };
        assert_eq!(doc.apply_op(&stray, false), ItemChange::Echo);
        assert_eq!(doc.get(shell.id), Some(&shell));
    }

    /// A review item comes in with the registry's snapshot and stays through the next one,
    /// so its tile is restored where the layout keeps it.
    #[test]
    fn a_review_is_restored_from_the_snapshot() {
        let me = ClientId::new();
        let mut doc = ItemDoc::default();
        let thread = slopty_proto::thread::ThreadId::new();
        let review = Item { kind: ItemKind::Review { thread }, ..term() };
        let items = vec![review.clone()];
        let first = ItemSync::Snapshot { version: 1, items: items.clone() };
        assert_eq!(doc.apply_sync(first, me), ItemChange::Reset);
        assert_eq!(doc.get(review.id), Some(&review));
        let again = ItemSync::Snapshot { version: 2, items };
        doc.apply_sync(again, me);
        assert_eq!(doc.get(review.id).map(|i| i.kind.clone()), Some(ItemKind::Review { thread }));
    }

    /// What is kept reads back as it was, the user's alone; a file cut short reads as nothing
    /// and goes, and a worker never kept has nothing.
    #[test]
    fn the_items_kept_read_back_and_a_broken_file_goes() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let cache = ItemCache::new(dir.path().join("items"));
        let note = Item { kind: ItemKind::Note { text: "plan".to_owned() }, ..term() };
        let items = [term(), note];
        let bytes = ItemCache::encode(items.iter()).unwrap();
        cache.write("w1", &bytes).unwrap();
        assert_eq!(cache.items("w1"), items);
        let file = cache.dir().join("w1.items");
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let doc = ItemDoc::cached(cache.items("w1"));
        assert_eq!((doc.version(), doc.len()), (0, 2), "before any snapshot");

        fs::write(&file, &bytes[..bytes.len() / 2]).unwrap();
        assert_eq!(cache.items("w1"), []);
        assert!(!file.exists(), "a broken file goes");
        assert_eq!(cache.items("never"), []);
    }
}
