//! The authoritative item registry.
//!
//! One registry per worker, persisted as JSON in the data directory. Clients propose
//! [`ItemOp`]s; the store validates, bumps the version, persists, and hands back the
//! [`ItemSync::Delta`] to broadcast. New sessions get a terminal item automatically so every
//! client shows them; closed sessions take their item with them. Where an item is shown is
//! each client's own business: the registry keeps no geometry.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// Serialises writers: each takes the latest registry under this lock, so concurrent
    /// persists never race on the temp file and the last write always holds the newest state.
    io: Mutex<()>,
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
            ItemSync::Delta { version: registry.version, by, op: ItemOp::Upsert(item) }
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

    /// Write the registry to disk (atomic rename). On a tokio runtime the write goes to a
    /// blocking thread; elsewhere (tests, tools) it runs inline.
    fn persist(&self) {
        let inner = Arc::clone(&self.inner);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let _task = handle.spawn_blocking(move || inner.write_latest());
            }
            Err(_no_runtime) => inner.write_latest(),
        }
    }

    /// Block until every pending write has landed (tests and shutdown).
    pub fn flush(&self) {
        self.inner.write_latest();
    }
}

impl Inner {
    fn write_latest(&self) {
        let _writer = self.io.lock();
        let snapshot = self.registry.lock().clone();
        if let Err(e) = write_atomic(&self.path, &snapshot) {
            tracing::warn!(path = %self.path.display(), error = %e, "persist items");
        }
    }
}

fn write_atomic(path: &Path, registry: &Registry) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(registry).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Reject nonsense before it reaches the registry.
fn sanitize(op: ItemOp) -> Result<ItemOp, WorkerError> {
    Ok(match op {
        ItemOp::Upsert(mut item) => {
            if let ItemKind::Note { text } = &item.kind
                && text.len() > NOTE_MAX
            {
                return Err(WorkerError::Items("note too long".to_owned()));
            }
            if let ItemKind::File { path } = &item.kind
                && (path.is_empty() || path.len() > PATH_MAX)
            {
                return Err(WorkerError::Items("bad file path".to_owned()));
            }
            if let ItemKind::Browser { url } = &item.kind
                && !web_address(url)
            {
                return Err(WorkerError::Items("bad url".to_owned()));
            }
            // A name is what the human typed, trimmed; blank is no name at all.
            item.name = item.name.take().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
            if item.name.as_ref().is_some_and(|n| n.chars().count() > NAME_MAX) {
                return Err(WorkerError::Items("name too long".to_owned()));
            }
            ItemOp::Upsert(item)
        }
        other @ (ItemOp::Remove(_) | ItemOp::Sleep { .. }) => other,
    })
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
        ItemOp::Upsert(item) => {
            registry.items.insert(item.id, item.clone());
        }
        ItemOp::Remove(id) => {
            registry.items.remove(id).ok_or(WorkerError::NoSuchItem)?;
        }
        ItemOp::Sleep { id, sleeping } => {
            registry.items.get_mut(id).ok_or(WorkerError::NoSuchItem)?.sleeping = *sleeping;
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

    fn upserted(delta: ItemSync) -> Item {
        match delta {
            ItemSync::Delta { op: ItemOp::Upsert(i), .. } => i,
            other => panic!("an upsert, not {other:?}"),
        }
    }

    #[test]
    fn a_session_gets_one_item_which_goes_with_it_and_persists() {
        let (dir, store) = store();
        let by = ClientId::new();
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let a = upserted(store.ensure_terminal(s1, by).unwrap());
        assert!(store.ensure_terminal(s1, by).is_none(), "idempotent");
        let b = upserted(store.ensure_terminal(s2, by).unwrap());
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
    /// `NAME_MAX` characters is refused rather than cut (the client shows what was typed).
    #[test]
    fn a_name_is_trimmed_blank_is_none_and_too_long_is_refused() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let item = upserted(store.ensure_terminal(SessionId::new(), by).unwrap());
        let named =
            |name: &str| ItemOp::Upsert(Item { name: Some(name.to_owned()), ..item.clone() });
        let name_of = |delta: ItemSync| upserted(delta).name;
        assert_eq!(
            name_of(store.apply(named("  build box "), by).unwrap()).as_deref(),
            Some("build box")
        );
        assert_eq!(name_of(store.apply(named("   "), by).unwrap()), None);
        let long = "n".repeat(NAME_MAX);
        assert_eq!(name_of(store.apply(named(&long), by).unwrap()).as_deref(), Some(long.as_str()));
        let err = store.apply(named(&"n".repeat(NAME_MAX + 1)), by).unwrap_err();
        assert!(matches!(err, WorkerError::Items(_)), "{err:?}");
    }

    #[test]
    fn an_unknown_item_is_refused() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let err = store.apply(ItemOp::Remove(ItemId::new()), by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem));
        let err = store.apply(ItemOp::Sleep { id: ItemId::new(), sleeping: true }, by).unwrap_err();
        assert!(matches!(err, WorkerError::NoSuchItem));
        let item = upserted(store.ensure_terminal(SessionId::new(), by).unwrap());
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

    /// A note and a file path have their byte limits.
    #[test]
    fn notes_and_paths_are_bounded() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let item = |kind: ItemKind| Item { id: ItemId::new(), kind, sleeping: false, name: None };
        let note = |text: String| item(ItemKind::Note { text });
        store.apply(ItemOp::Upsert(note("n".repeat(NOTE_MAX))), by).unwrap();
        let err = store.apply(ItemOp::Upsert(note("n".repeat(NOTE_MAX + 1))), by).unwrap_err();
        assert!(matches!(&err, WorkerError::Items(m) if m == "note too long"), "{err:?}");
        let file = |path: String| item(ItemKind::File { path });
        store.apply(ItemOp::Upsert(file("/".repeat(PATH_MAX))), by).unwrap();
        for path in [String::new(), "/".repeat(PATH_MAX + 1)] {
            let err = store.apply(ItemOp::Upsert(file(path)), by).unwrap_err();
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
            store.apply(ItemOp::Upsert(page(url)), by).unwrap();
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
            let err = store.apply(ItemOp::Upsert(page(url)), by).unwrap_err();
            assert!(matches!(&err, WorkerError::Items(m) if m == "bad url"), "{url}: {err:?}");
        }
    }
}
