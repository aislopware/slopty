//! The authoritative item registry.
//!
//! One registry per worker, persisted as JSON in the data directory. Clients propose
//! [`ItemOp`]s; the store validates, bumps the version, persists, and hands back the
//! [`ItemSync::Delta`] to broadcast. New sessions get a terminal item automatically so every
//! client shows them; closed sessions take their item with them. Where an item is shown is
//! each client's own business: the registry keeps no geometry.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::items::{Item, ItemKind, ItemOp, ItemSync, NAME_MAX};

use crate::WorkerError;

/// The longest note text accepted, in bytes.
pub const NOTE_MAX: usize = 64 * 1024;
/// The longest file path accepted, in bytes.
pub const PATH_MAX: usize = 4096;
/// The longest browser address accepted, in bytes.
pub const URL_MAX: usize = 2048;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Registry {
    version: u64,
    items: BTreeMap<ItemId, Item>,
}

/// The store.
#[derive(Clone, Debug)]
pub struct ItemStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    registry: Mutex<Registry>,
    /// Serialises writes: each takes the latest registry under this lock, so a write never
    /// races another on the temp file and the last one holds the newest state.
    io: Mutex<()>,
    /// The registry changed since it was last written.
    dirty: AtomicBool,
    /// A writer is at work, and will see `dirty`.
    writing: AtomicBool,
    /// Writes so far.
    #[cfg(test)]
    writes: std::sync::atomic::AtomicU64,
}

impl ItemStore {
    /// Load from `path`, or start empty when it does not exist.
    pub fn open(path: &Path) -> Result<Self, WorkerError> {
        let registry = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)
                .map_err(|e| WorkerError::Items(format!("parse {}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(e) => return Err(WorkerError::Items(format!("read {}: {e}", path.display()))),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                registry: Mutex::new(registry),
                io: Mutex::new(()),
                dirty: AtomicBool::new(false),
                writing: AtomicBool::new(false),
                #[cfg(test)]
                writes: std::sync::atomic::AtomicU64::new(0),
            }),
        })
    }

    /// Every item.
    #[must_use]
    pub fn snapshot(&self) -> ItemSync {
        let registry = self.inner.registry.lock();
        ItemSync::Snapshot {
            version: registry.version,
            items: registry.items.values().cloned().collect(),
        }
    }

    /// Every item, by id.
    #[must_use]
    pub fn items(&self) -> Vec<Item> {
        self.inner.registry.lock().items.values().cloned().collect()
    }

    /// The item `id`, if the registry holds it.
    #[must_use]
    pub fn get(&self, id: ItemId) -> Option<Item> {
        self.inner.registry.lock().items.get(&id).cloned()
    }

    /// Current version.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.registry.lock().version
    }

    /// Validate and apply a client's proposal. Returns the delta to broadcast.
    pub fn apply(&self, op: ItemOp, by: ClientId) -> Result<ItemSync, WorkerError> {
        let op = sanitize(op)?;
        let delta = {
            let mut registry = self.inner.registry.lock();
            apply_in(&mut registry, &op)?;
            registry.version = registry.version.saturating_add(1);
            ItemSync::Delta { version: registry.version, by, op }
        };
        self.persist();
        Ok(delta)
    }

    /// Give `session` a terminal item if it has none. Returns the delta to broadcast.
    pub fn ensure_terminal(&self, session: SessionId, by: ClientId) -> Option<ItemSync> {
        let delta = {
            let mut registry = self.inner.registry.lock();
            let exists = registry
                .items
                .values()
                .any(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session));
            if exists {
                return None;
            }
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::Terminal { session },
                sleeping: false,
                name: None,
            };
            registry.items.insert(item.id, item.clone());
            registry.version = registry.version.saturating_add(1);
            ItemSync::Delta { version: registry.version, by, op: ItemOp::Add(item) }
        };
        self.persist();
        Some(delta)
    }

    /// Remove every item showing `session`. Returns the deltas to broadcast.
    pub fn remove_session(&self, session: SessionId, by: ClientId) -> Vec<ItemSync> {
        let deltas = {
            let mut registry = self.inner.registry.lock();
            let ids: Vec<ItemId> = registry
                .items
                .values()
                .filter(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session))
                .map(|i| i.id)
                .collect();
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                registry.items.remove(&id);
                registry.version = registry.version.saturating_add(1);
                out.push(ItemSync::Delta { version: registry.version, by, op: ItemOp::Remove(id) });
            }
            out
        };
        if !deltas.is_empty() {
            self.persist();
        }
        deltas
    }

    /// Have the registry written to disk (atomic rename) by one writer, which writes the latest
    /// state and writes again only if something changed meanwhile: a burst of changes is a write
    /// or two, not one each. On a tokio runtime the writer runs on a blocking thread; elsewhere
    /// (tests, tools) it runs inline, so the change is on disk before this returns.
    fn persist(&self) {
        self.inner.dirty.store(true, Ordering::SeqCst);
        if self.inner.writing.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let _task = handle.spawn_blocking(move || inner.drain());
            }
            Err(_no_runtime) => inner.drain(),
        }
    }

    /// Block until every change so far is on disk (tests and shutdown).
    pub fn flush(&self) {
        self.inner.write_if_dirty();
    }
}

impl Inner {
    /// Write until nothing is left unwritten, then stand down. A change that marks the registry
    /// dirty just as the writer stands down found `writing` still set and left it to this
    /// writer, so it looks once more after letting go.
    fn drain(&self) {
        loop {
            while self.write_if_dirty() {}
            self.writing.store(false, Ordering::SeqCst);
            if !self.dirty.load(Ordering::SeqCst) || self.writing.swap(true, Ordering::SeqCst) {
                return;
            }
        }
    }

    /// Write the registry as it is now if it changed since the last write; `false` when it had
    /// not.
    fn write_if_dirty(&self) -> bool {
        let _writer = self.io.lock();
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return false;
        }
        let bytes = serde_json::to_vec(&*self.registry.lock());
        let written = bytes
            .map_err(std::io::Error::other)
            .and_then(|b| slopty_platform::fs::replace(&self.path, &b));
        if let Err(e) = written {
            tracing::warn!(path = %self.path.display(), error = %e, "persist items");
        }
        #[cfg(test)]
        self.writes.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// Reject nonsense before it reaches the registry.
fn sanitize(op: ItemOp) -> Result<ItemOp, WorkerError> {
    Ok(match op {
        ItemOp::Add(mut item) => {
            check_kind(&item.kind)?;
            item.name = clean_name(item.name)?;
            ItemOp::Add(item)
        }
        ItemOp::Rename { id, name } => ItemOp::Rename { id, name: clean_name(name)? },
        ItemOp::SetNote { id, text } => {
            check_note(&text)?;
            ItemOp::SetNote { id, text }
        }
        ItemOp::SetUrl { url, .. } if !web_address(&url) => {
            return Err(WorkerError::Items("bad url".to_owned()));
        }
        ItemOp::SetFolder { path, .. } if !good_path(&path) => {
            return Err(WorkerError::Items("bad folder path".to_owned()));
        }
        other @ (ItemOp::Remove(_)
        | ItemOp::Sleep { .. }
        | ItemOp::SetUrl { .. }
        | ItemOp::SetFolder { .. }) => other,
    })
}

fn check_kind(kind: &ItemKind) -> Result<(), WorkerError> {
    match kind {
        ItemKind::Note { text } => check_note(text),
        ItemKind::File { path } if !good_path(path) => {
            Err(WorkerError::Items("bad file path".to_owned()))
        }
        ItemKind::Folder { path } if !good_path(path) => {
            Err(WorkerError::Items("bad folder path".to_owned()))
        }
        ItemKind::Browser { url } if !web_address(url) => {
            Err(WorkerError::Items("bad url".to_owned()))
        }
        ItemKind::Terminal { .. }
        | ItemKind::Window { .. }
        | ItemKind::Display { .. }
        | ItemKind::File { .. }
        | ItemKind::Folder { .. }
        | ItemKind::Browser { .. }
        | ItemKind::Review { .. } => Ok(()),
    }
}

/// A path a file or folder item may name: something, [`PATH_MAX`] bytes at most.
const fn good_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= PATH_MAX
}

fn check_note(text: &str) -> Result<(), WorkerError> {
    if text.len() > NOTE_MAX {
        return Err(WorkerError::Items("note too long".to_owned()));
    }
    Ok(())
}

/// A name is what the human typed, trimmed; blank is no name at all.
fn clean_name(name: Option<String>) -> Result<Option<String>, WorkerError> {
    let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
    if name.as_ref().is_some_and(|n| n.chars().count() > NAME_MAX) {
        return Err(WorkerError::Items("name too long".to_owned()));
    }
    Ok(name)
}

/// An `http` or `https` address with a host, [`URL_MAX`] bytes at most, and nothing a URL
/// leaves out (whitespace, control characters): a tile never opens `file:` or `javascript:`.
fn web_address(url: &str) -> bool {
    if url.len() > URL_MAX || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !host.is_empty()
}

fn apply_in(registry: &mut Registry, op: &ItemOp) -> Result<(), WorkerError> {
    match op {
        ItemOp::Add(item) => match registry.items.entry(item.id) {
            Entry::Occupied(_) => return Err(WorkerError::Items("item exists".to_owned())),
            Entry::Vacant(slot) => {
                slot.insert(item.clone());
            }
        },
        ItemOp::Remove(id) => {
            registry.items.remove(id).ok_or(WorkerError::NoSuchItem)?;
        }
        // An edit that changes nothing is still taken and broadcast: the delta is how the
        // proposer hears its op went through.
        ItemOp::Sleep { id, .. }
        | ItemOp::Rename { id, .. }
        | ItemOp::SetNote { id, .. }
        | ItemOp::SetUrl { id, .. }
        | ItemOp::SetFolder { id, .. } => {
            let item = registry.items.get_mut(id).ok_or(WorkerError::NoSuchItem)?;
            item.apply(op).map_err(|e| WorkerError::Items(e.to_string()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ItemStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ItemStore::open(&dir.path().join("items.json")).unwrap();
        (dir, store)
    }

    fn added(delta: ItemSync) -> Item {
        match delta {
            ItemSync::Delta { op: ItemOp::Add(i), .. } => i,
            other => panic!("an addition, not {other:?}"),
        }
    }

    #[test]
    fn a_session_gets_one_item_which_goes_with_it_and_persists() {
        let (dir, store) = store();
        let by = ClientId::new();
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let a = added(store.ensure_terminal(s1, by).unwrap());
        assert!(store.ensure_terminal(s1, by).is_none(), "idempotent");
        let b = added(store.ensure_terminal(s2, by).unwrap());
        assert_eq!(a.kind, ItemKind::Terminal { session: s1 });
        assert_ne!(a.id, b.id);

        let removed = store.remove_session(s1, by);
        assert_eq!(removed.len(), 1);
        assert_eq!(store.version(), 3);

        // Persisted and reloadable.
        store.flush();
        let again = ItemStore::open(&dir.path().join("items.json")).unwrap();
        let ItemSync::Snapshot { version, items } = again.snapshot() else { panic!("snapshot") };
        assert_eq!(version, 3);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, b.id);
    }

    /// An item's name is kept as typed but trimmed, a blank one is no name, and one past
    /// `NAME_MAX` characters is refused rather than cut (the client shows what was typed). A
    /// rename and a new item's name are held to it alike.
    #[test]
    fn a_name_is_trimmed_blank_is_none_and_too_long_is_refused() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let id = added(store.ensure_terminal(SessionId::new(), by).unwrap()).id;
        let named = |name: &str| ItemOp::Rename { id, name: Some(name.to_owned()) };
        let name_of = |delta: ItemSync| match delta {
            ItemSync::Delta { op: ItemOp::Rename { name, .. }, .. } => name,
            other => panic!("a rename, not {other:?}"),
        };
        assert_eq!(
            name_of(store.apply(named("  build box "), by).unwrap()).as_deref(),
            Some("build box")
        );
        assert_eq!(store.get(id).unwrap().name.as_deref(), Some("build box"));
        assert_eq!(name_of(store.apply(named("   "), by).unwrap()), None);
        assert_eq!(store.get(id).unwrap().name, None);
        let long = "n".repeat(NAME_MAX);
        assert_eq!(name_of(store.apply(named(&long), by).unwrap()).as_deref(), Some(long.as_str()));
        let err = store.apply(named(&"n".repeat(NAME_MAX + 1)), by).unwrap_err();
        assert!(matches!(err, WorkerError::Items(_)), "{err:?}");
        assert_eq!(store.get(id).unwrap().name.as_deref(), Some(long.as_str()), "kept");

        let note = |name: &str| Item {
            id: ItemId::new(),
            kind: ItemKind::Note { text: String::new() },
            sleeping: false,
            name: Some(name.to_owned()),
        };
        let fresh = added(store.apply(ItemOp::Add(note(" plan ")), by).unwrap());
        assert_eq!(fresh.name.as_deref(), Some("plan"));
        let err = store.apply(ItemOp::Add(note(&"n".repeat(NAME_MAX + 1))), by).unwrap_err();
        assert!(matches!(err, WorkerError::Items(_)), "{err:?}");
    }

    /// Two clients each holding the same stale copy of a note, one renaming it and the other
    /// editing its text, both land: each op carries only its own field.
    #[test]
    fn a_rename_and_a_note_edit_from_two_clients_both_survive() {
        let (_dir, store) = store();
        let (mac, ipad) = (ClientId::new(), ClientId::new());
        let note = Item {
            id: ItemId::new(),
            kind: ItemKind::Note { text: "draft".to_owned() },
            sleeping: false,
            name: None,
        };
        let _added = store.apply(ItemOp::Add(note.clone()), mac).unwrap();
        let stale = note;
        let rename = ItemOp::Rename { id: stale.id, name: Some("plan".to_owned()) };
        let renamed = store.apply(rename, ipad).unwrap();
        assert!(matches!(renamed, ItemSync::Delta { by, .. } if by == ipad));
        let edit = ItemOp::SetNote { id: stale.id, text: "draft, then more".to_owned() };
        let edited = store.apply(edit, mac).unwrap();
        assert!(matches!(edited, ItemSync::Delta { version: 3, by, .. } if by == mac));
        let now = store.get(stale.id).unwrap();
        assert_eq!(now.name.as_deref(), Some("plan"));
        assert_eq!(now.kind, ItemKind::Note { text: "draft, then more".to_owned() });
    }

    /// An id the registry holds cannot be added again, so a stale copy put back never
    /// overwrites the live item; once removed, the same id comes back (a closed tile undone).
    #[test]
    fn adding_a_held_id_is_refused_and_a_removed_one_comes_back() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let item = added(store.ensure_terminal(SessionId::new(), by).unwrap());
        let _named =
            store.apply(ItemOp::Rename { id: item.id, name: Some("api".into()) }, by).unwrap();
        let err = store.apply(ItemOp::Add(item.clone()), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "item exists"), "{err:?}");
        assert_eq!(store.get(item.id).unwrap().name.as_deref(), Some("api"), "live item kept");
        assert_eq!(store.version(), 2, "a refusal bumps nothing");

        let _removed = store.apply(ItemOp::Remove(item.id), by).unwrap();
        let back = added(store.apply(ItemOp::Add(item.clone()), by).unwrap());
        assert_eq!(back, item);
        assert_eq!(store.get(item.id), Some(item));
    }

    /// An address is set only on a browser, and only an `http` or `https` one.
    #[test]
    fn an_address_takes_only_a_browser_and_a_web_address() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let shell = added(store.ensure_terminal(SessionId::new(), by).unwrap());
        let set = |id, url: &str| ItemOp::SetUrl { id, url: url.to_owned() };
        let err = store.apply(set(shell.id, "http://a.test/"), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "not a browser"), "{err:?}");
        let page = Item {
            id: ItemId::new(),
            kind: ItemKind::Browser { url: "http://localhost:5173/".to_owned() },
            sleeping: false,
            name: None,
        };
        let _added = store.apply(ItemOp::Add(page.clone()), by).unwrap();
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "http://", "http://a b/"] {
            let err = store.apply(set(page.id, bad), by).unwrap_err();
            assert!(matches!(&err, WorkerError::Items(m) if m == "bad url"), "{bad}: {err:?}");
        }
        store.apply(set(page.id, "http://localhost:3000/docs"), by).unwrap();
        let url = "http://localhost:3000/docs".to_owned();
        assert_eq!(store.get(page.id).map(|i| i.kind), Some(ItemKind::Browser { url }));
    }

    /// A folder moves only as a folder, to a path that is something and not too long, and
    /// where it went is what the registry reopens with.
    #[test]
    fn a_folder_moves_and_is_reopened_where_it_went() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("items.json");
        let store = ItemStore::open(&file).unwrap();
        let by = ClientId::new();
        let shell = added(store.ensure_terminal(SessionId::new(), by).unwrap());
        let folder = Item {
            id: ItemId::new(),
            kind: ItemKind::Folder { path: "/w".to_owned() },
            sleeping: false,
            name: None,
        };
        let _added = store.apply(ItemOp::Add(folder.clone()), by).unwrap();
        let set = |id, path: &str| ItemOp::SetFolder { id, path: path.to_owned() };
        let err = store.apply(set(shell.id, "/w/src"), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "not a folder"), "{err:?}");
        for bad in [String::new(), "a".repeat(PATH_MAX + 1)] {
            let err = store.apply(set(folder.id, &bad), by).unwrap_err();
            assert!(matches!(&err, WorkerError::Items(m) if m == "bad folder path"), "{err:?}");
        }
        store.apply(set(folder.id, "/w/src"), by).unwrap();
        drop(store);
        let reopened = ItemStore::open(&file).unwrap();
        let path = "/w/src".to_owned();
        assert_eq!(reopened.get(folder.id).map(|i| i.kind), Some(ItemKind::Folder { path }));
    }

    /// A thread's review is an item like any other: added, and reopened as it was.
    #[test]
    fn a_review_is_kept_and_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("items.json");
        let store = ItemStore::open(&file).unwrap();
        let thread = slopty_proto::thread::ThreadId::new();
        let review = Item {
            id: ItemId::new(),
            kind: ItemKind::Review { thread },
            sleeping: false,
            name: None,
        };
        let _added = store.apply(ItemOp::Add(review.clone()), ClientId::new()).unwrap();
        drop(store);
        let reopened = ItemStore::open(&file).unwrap();
        assert_eq!(reopened.get(review.id), Some(review));
    }

    /// A note's text is set only on a note, is bounded like a new note's, and an unknown item
    /// is refused.
    #[test]
    fn a_note_edit_takes_only_a_note_and_is_bounded() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let shell = added(store.ensure_terminal(SessionId::new(), by).unwrap());
        let set = |id, text: String| ItemOp::SetNote { id, text };
        let err = store.apply(set(shell.id, "hi".to_owned()), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "not a note"), "{err:?}");
        assert_eq!(store.get(shell.id), Some(shell), "untouched");
        let err = store.apply(set(ItemId::new(), String::new()), by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem), "{err:?}");

        let note = Item {
            id: ItemId::new(),
            kind: ItemKind::Note { text: String::new() },
            sleeping: false,
            name: None,
        };
        let _added = store.apply(ItemOp::Add(note.clone()), by).unwrap();
        store.apply(set(note.id, "n".repeat(NOTE_MAX)), by).unwrap();
        let err = store.apply(set(note.id, "n".repeat(NOTE_MAX + 1)), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "note too long"), "{err:?}");
        let kept = store.get(note.id).unwrap().kind;
        assert_eq!(kept, ItemKind::Note { text: "n".repeat(NOTE_MAX) });
        let err = store.apply(ItemOp::Rename { id: ItemId::new(), name: None }, by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem), "{err:?}");
    }

    #[test]
    fn an_unknown_item_is_refused() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let err = store.apply(ItemOp::Remove(ItemId::new()), by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem));
        let err = store.apply(ItemOp::Sleep { id: ItemId::new(), sleeping: true }, by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem));
        let item = added(store.ensure_terminal(SessionId::new(), by).unwrap());
        let slept = store.apply(ItemOp::Sleep { id: item.id, sleeping: true }, by).unwrap();
        assert!(matches!(slept, ItemSync::Delta { op: ItemOp::Sleep { sleeping: true, .. }, .. }));
        let ItemSync::Snapshot { items, .. } = store.snapshot() else { panic!("snapshot") };
        assert!(items[0].sleeping);
    }

    /// A store is unreadable when its path is a directory and refused when its registry
    /// does not parse; only a missing file starts empty.
    #[test]
    fn an_unreadable_path_and_a_bad_registry_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = ItemStore::open(dir.path()).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m.starts_with("read ")), "{err:?}");
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{ not json").unwrap();
        let err = ItemStore::open(&bad).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m.starts_with("parse ")), "{err:?}");
        let fresh = ItemStore::open(&dir.path().join("none.json")).unwrap();
        assert_eq!(fresh.version(), 0);
    }

    /// Without a runtime every change is on disk before the call returns, a removal too.
    #[test]
    fn every_change_is_on_disk_before_it_returns() {
        let (dir, store) = store();
        let path = dir.path().join("items.json");
        let by = ClientId::new();
        let session = SessionId::new();
        let _delta = store.ensure_terminal(session, by).unwrap();
        let ItemSync::Snapshot { version, items } = ItemStore::open(&path).unwrap().snapshot()
        else {
            panic!("snapshot")
        };
        assert_eq!((version, items.len()), (1, 1));
        assert_eq!(store.remove_session(session, by).len(), 1);
        assert!(store.remove_session(session, by).is_empty(), "nothing left to remove");
        let ItemSync::Snapshot { version, items } = ItemStore::open(&path).unwrap().snapshot()
        else {
            panic!("snapshot")
        };
        assert_eq!((version, items.len()), (2, 0));
    }

    /// On a runtime a burst of changes made while a write is under way is written once, as the
    /// burst left the registry, in compact JSON.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_changes_is_written_once_as_it_ended() {
        let (dir, store) = store();
        let path = dir.path().join("items.json");
        let by = ClientId::new();
        let under_way = store.inner.io.lock();
        for _ in 0..50 {
            let _delta = store.ensure_terminal(SessionId::new(), by).unwrap();
        }
        drop(under_way);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while store.inner.writing.load(Ordering::SeqCst) {
            assert!(std::time::Instant::now() < deadline, "the writer never stood down");
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        assert_eq!(store.inner.writes.load(Ordering::SeqCst), 1, "one write for the burst");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains('\n'), "compact, not pretty");
        let ItemSync::Snapshot { version, items } = ItemStore::open(&path).unwrap().snapshot()
        else {
            panic!("snapshot")
        };
        assert_eq!((version, items.len()), (50, 50));
        store.flush();
        assert_eq!(store.inner.writes.load(Ordering::SeqCst), 1, "nothing left to flush");
    }

    /// A note and a file path have their byte limits.
    #[test]
    fn notes_and_paths_are_bounded() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let item = |kind: ItemKind| Item { id: ItemId::new(), kind, sleeping: false, name: None };
        let note = |text: String| item(ItemKind::Note { text });
        store.apply(ItemOp::Add(note("n".repeat(NOTE_MAX))), by).unwrap();
        let err = store.apply(ItemOp::Add(note("n".repeat(NOTE_MAX + 1))), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "note too long"), "{err:?}");
        let file = |path: String| item(ItemKind::File { path });
        store.apply(ItemOp::Add(file("/".repeat(PATH_MAX))), by).unwrap();
        for path in [String::new(), "/".repeat(PATH_MAX + 1)] {
            let err = store.apply(ItemOp::Add(file(path)), by).unwrap_err();
            assert!(matches!(&err, WorkerError::Items(m) if m == "bad file path"), "{err:?}");
        }
    }

    /// A browser tile takes an http or https address with a host, 2 KiB at most.
    #[test]
    fn browser_addresses_are_web_ones() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let page = |url: &str| Item {
            id: ItemId::new(),
            kind: ItemKind::Browser { url: url.to_owned() },
            sleeping: false,
            name: None,
        };
        let long = format!("http://localhost/{}", "a".repeat(URL_MAX - 17));
        for url in ["http://localhost:5173/", "HTTPS://example.test/a?b#c", &long] {
            store.apply(ItemOp::Add(page(url)), by).unwrap();
        }
        let too_long = format!("{long}a");
        for url in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://",
            "http:///path",
            "localhost:5173",
            "http://local host/",
            "",
            &too_long,
        ] {
            let err = store.apply(ItemOp::Add(page(url)), by).unwrap_err();
            assert!(matches!(&err, WorkerError::Items(m) if m == "bad url"), "{url}: {err:?}");
        }
    }
}
