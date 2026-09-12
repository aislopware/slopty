//! The authoritative canvas document.
//!
//! One document per host, persisted as JSON in the data directory. Clients propose
//! [`CanvasOp`]s; the store validates, bumps the version, persists, and hands back the
//! [`CanvasSync::Delta`] to broadcast. New sessions get a terminal item automatically so every
//! client sees them in the same place; closed sessions take their item with them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, NAME_MAX, Rect};

use crate::HostError;

/// Smallest item size accepted, in canvas units.
pub const MIN_SIZE: f32 = 160.0;
/// Largest item size accepted.
pub const MAX_SIZE: f32 = 16_384.0;
/// Farthest an item may sit from the origin.
pub const MAX_OFFSET: f32 = 1.0e6;
/// Default terminal item size.
pub const TERMINAL_SIZE: (f32, f32) = (720.0, 440.0);
/// Gap between auto-placed items.
const GAP: f32 = 24.0;
const SNAP: f32 = 16.0;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Doc {
    version: u64,
    items: BTreeMap<ItemId, CanvasItem>,
}

/// The store.
#[derive(Clone, Debug)]
pub struct CanvasStore {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    doc: Mutex<Doc>,
    /// Serialises writers: each takes the latest document under this lock, so concurrent
    /// persists never race on the temp file and the last write always holds the newest state.
    io: Mutex<()>,
}

impl CanvasStore {
    /// Load from `path`, or start empty when it does not exist.
    pub fn open(path: &Path) -> Result<Self, HostError> {
        let doc = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Doc>(&bytes)
                .map_err(|e| HostError::Canvas(format!("parse {}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Doc::default(),
            Err(e) => return Err(HostError::Canvas(format!("read {}: {e}", path.display()))),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.to_path_buf(),
                doc: Mutex::new(doc),
                io: Mutex::new(()),
            }),
        })
    }

    /// The whole document.
    #[must_use]
    pub fn snapshot(&self) -> CanvasSync {
        let doc = self.inner.doc.lock();
        CanvasSync::Snapshot { version: doc.version, items: doc.items.values().cloned().collect() }
    }

    /// Current version.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.inner.doc.lock().version
    }

    /// Validate and apply a client's proposal. Returns the delta to broadcast.
    pub fn apply(&self, op: CanvasOp, by: ClientId) -> Result<CanvasSync, HostError> {
        let op = sanitize(op)?;
        let delta = {
            let mut doc = self.inner.doc.lock();
            apply_in(&mut doc, &op)?;
            doc.version = doc.version.saturating_add(1);
            CanvasSync::Delta { version: doc.version, by, op }
        };
        self.persist();
        Ok(delta)
    }

    /// Give `session` a terminal item if it has none. Returns the delta to broadcast.
    pub fn ensure_terminal(&self, session: SessionId, by: ClientId) -> Option<CanvasSync> {
        let delta = {
            let mut doc = self.inner.doc.lock();
            let exists = doc
                .items
                .values()
                .any(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session));
            if exists {
                return None;
            }
            let rect = free_slot(&doc, TERMINAL_SIZE);
            let z = top_z(&doc);
            let item = CanvasItem {
                id: ItemId::new(),
                kind: ItemKind::Terminal { session },
                rect,
                z,
                group: None,
                sleeping: false,
                name: None,
            };
            doc.items.insert(item.id, item.clone());
            doc.version = doc.version.saturating_add(1);
            CanvasSync::Delta { version: doc.version, by, op: CanvasOp::Upsert(item) }
        };
        self.persist();
        Some(delta)
    }

    /// Remove every item showing `session`. Returns the deltas to broadcast.
    pub fn remove_session(&self, session: SessionId, by: ClientId) -> Vec<CanvasSync> {
        let deltas = {
            let mut doc = self.inner.doc.lock();
            let ids: Vec<ItemId> = doc
                .items
                .values()
                .filter(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session))
                .map(|i| i.id)
                .collect();
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                doc.items.remove(&id);
                doc.version = doc.version.saturating_add(1);
                out.push(CanvasSync::Delta { version: doc.version, by, op: CanvasOp::Remove(id) });
            }
            out
        };
        if !deltas.is_empty() {
            self.persist();
        }
        deltas
    }

    /// Write the document to disk (atomic rename). On a tokio runtime the write goes to a
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
        let snapshot = self.doc.lock().clone();
        if let Err(e) = write_atomic(&self.path, &snapshot) {
            tracing::warn!(path = %self.path.display(), error = %e, "persist canvas");
        }
    }
}

fn write_atomic(path: &Path, doc: &Doc) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(doc).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

fn snap(v: f32) -> f32 {
    (v / SNAP).round() * SNAP
}

fn top_z(doc: &Doc) -> u32 {
    doc.items.values().map(|i| i.z).max().map_or(0, |z| z.saturating_add(1))
}

/// Right of the rightmost item, on its row; the origin when empty.
fn free_slot(doc: &Doc, size: (f32, f32)) -> Rect {
    let (w, h) = size;
    let Some(rightmost) =
        doc.items.values().map(|i| i.rect).max_by(|a, b| (a.x + a.w).total_cmp(&(b.x + b.w)))
    else {
        return Rect { x: 0.0, y: 0.0, w, h };
    };
    Rect { x: snap(rightmost.x + rightmost.w + GAP), y: snap(rightmost.y), w, h }
}

fn check_rect(r: Rect) -> Result<Rect, HostError> {
    let finite = [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite());
    if !finite {
        return Err(HostError::Canvas("rect is not finite".to_owned()));
    }
    Ok(Rect {
        x: r.x.clamp(-MAX_OFFSET, MAX_OFFSET),
        y: r.y.clamp(-MAX_OFFSET, MAX_OFFSET),
        w: r.w.clamp(MIN_SIZE, MAX_SIZE),
        h: r.h.clamp(MIN_SIZE, MAX_SIZE),
    })
}

/// Clamp geometry and reject nonsense before it reaches the document.
fn sanitize(op: CanvasOp) -> Result<CanvasOp, HostError> {
    Ok(match op {
        CanvasOp::Upsert(mut item) => {
            item.rect = check_rect(item.rect)?;
            if let ItemKind::Note { text } = &item.kind
                && text.len() > 64 * 1024
            {
                return Err(HostError::Canvas("note too long".to_owned()));
            }
            if let ItemKind::File { path } = &item.kind
                && (path.is_empty() || path.len() > 4096)
            {
                return Err(HostError::Canvas("bad file path".to_owned()));
            }
            // A name is what the human typed, trimmed; blank is no name at all.
            item.name = item.name.take().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
            if item.name.as_ref().is_some_and(|n| n.chars().count() > NAME_MAX) {
                return Err(HostError::Canvas("name too long".to_owned()));
            }
            CanvasOp::Upsert(item)
        }
        CanvasOp::Place { id, rect } => CanvasOp::Place { id, rect: check_rect(rect)? },
        other @ (CanvasOp::Remove(_) | CanvasOp::Raise(_) | CanvasOp::Sleep { .. }) => other,
    })
}

fn apply_in(doc: &mut Doc, op: &CanvasOp) -> Result<(), HostError> {
    match op {
        CanvasOp::Upsert(item) => {
            doc.items.insert(item.id, item.clone());
        }
        CanvasOp::Remove(id) => {
            doc.items.remove(id).ok_or(HostError::NoSuchItem)?;
        }
        CanvasOp::Place { id, rect } => {
            doc.items.get_mut(id).ok_or(HostError::NoSuchItem)?.rect = *rect;
        }
        CanvasOp::Raise(id) => {
            let z = top_z(doc);
            doc.items.get_mut(id).ok_or(HostError::NoSuchItem)?.z = z;
        }
        CanvasOp::Sleep { id, sleeping } => {
            doc.items.get_mut(id).ok_or(HostError::NoSuchItem)?.sleeping = *sleeping;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, CanvasStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CanvasStore::open(&dir.path().join("canvas.json")).unwrap();
        (dir, store)
    }

    #[test]
    fn auto_place_then_remove_and_persist() {
        let (dir, store) = store();
        let by = ClientId::new();
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        let d1 = store.ensure_terminal(s1, by).unwrap();
        assert!(store.ensure_terminal(s1, by).is_none(), "idempotent");
        let d2 = store.ensure_terminal(s2, by).unwrap();
        let (
            CanvasSync::Delta { op: CanvasOp::Upsert(a), .. },
            CanvasSync::Delta { op: CanvasOp::Upsert(b), .. },
        ) = (d1, d2)
        else {
            panic!("expected upserts");
        };
        assert!(a.rect.x.abs() < f32::EPSILON, "{a:?}");
        assert!(b.rect.x >= a.rect.x + a.rect.w + GAP - SNAP, "{b:?}");
        assert!(b.z > a.z);

        let removed = store.remove_session(s1, by);
        assert_eq!(removed.len(), 1);
        assert_eq!(store.version(), 3);

        // Persisted and reloadable.
        store.flush();
        let path = dir.path().join("canvas.json");
        let again = CanvasStore::open(&path).unwrap();
        let CanvasSync::Snapshot { version, items } = again.snapshot() else { panic!("snapshot") };
        assert_eq!(version, 3);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, b.id);
    }

    /// A card's name is kept as typed but trimmed, a blank one is no name, and one past
    /// `NAME_MAX` characters is refused rather than cut (the client shows what was typed).
    #[test]
    fn a_name_is_trimmed_blank_is_none_and_too_long_is_refused() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let CanvasSync::Delta { op: CanvasOp::Upsert(item), .. } =
            store.ensure_terminal(SessionId::new(), by).unwrap()
        else {
            panic!("an upsert");
        };
        let named = |name: &str| {
            CanvasOp::Upsert(CanvasItem { name: Some(name.to_owned()), ..item.clone() })
        };
        let name_of = |delta: CanvasSync| match delta {
            CanvasSync::Delta { op: CanvasOp::Upsert(i), .. } => i.name,
            _ => panic!("an upsert"),
        };
        assert_eq!(
            name_of(store.apply(named("  build box "), by).unwrap()).as_deref(),
            Some("build box")
        );
        assert_eq!(name_of(store.apply(named("   "), by).unwrap()), None);
        let long = "n".repeat(NAME_MAX);
        assert_eq!(name_of(store.apply(named(&long), by).unwrap()).as_deref(), Some(long.as_str()));
        let err = store.apply(named(&"n".repeat(NAME_MAX + 1)), by).unwrap_err();
        assert!(matches!(err, HostError::Canvas(_)), "{err:?}");
    }

    #[test]
    fn rejects_unknown_and_clamps_geometry() {
        let (_dir, store) = store();
        let by = ClientId::new();
        let err = store.apply(CanvasOp::Raise(ItemId::new()), by).unwrap_err();
        assert!(matches!(err, HostError::NoSuchItem));
        let delta = store
            .ensure_terminal(SessionId::new(), by)
            .and_then(|d| match d {
                CanvasSync::Delta { op: CanvasOp::Upsert(i), .. } => Some(i.id),
                _ => None,
            })
            .unwrap();
        let placed = store.apply(
            CanvasOp::Place { id: delta, rect: Rect { x: 1.0e9, y: 0.0, w: 1.0, h: f32::NAN } },
            by,
        );
        assert!(placed.is_err(), "NaN rejected");
        let placed = store
            .apply(
                CanvasOp::Place { id: delta, rect: Rect { x: 1.0e9, y: 0.0, w: 1.0, h: 1.0 } },
                by,
            )
            .unwrap();
        let CanvasSync::Delta { op: CanvasOp::Place { rect, .. }, .. } = placed else { panic!() };
        assert!(
            (rect.x - MAX_OFFSET).abs() < 1.0 && (rect.w - MIN_SIZE).abs() < f32::EPSILON,
            "{rect:?}"
        );
    }
}
