//! The canvas document as a client sees it, plus the camera that maps it to the screen.
//!
//! The host is authoritative: the client applies every [`CanvasSync`] it receives and proposes
//! changes as [`CanvasOp`]s. To keep dragging responsive the client applies its own proposals
//! immediately (optimistic) and skips the host's echo of them.

use std::collections::BTreeMap;

use slopty_core::{ClientId, ItemId};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, Rect};

/// Smallest zoom: everything is a card.
pub const MIN_ZOOM: f32 = 0.1;
/// Largest zoom.
pub const MAX_ZOOM: f32 = 4.0;
/// Below this zoom terminals draw as summary cards instead of full grids.
pub const CARD_ZOOM: f32 = 0.6;
/// Grid new items snap to, in canvas units.
pub const SNAP: f32 = 16.0;
/// Gap between auto-placed items.
pub const GAP: f32 = 24.0;

/// The document.
#[derive(Clone, Debug, Default)]
pub struct CanvasDoc {
    version: u64,
    items: BTreeMap<ItemId, CanvasItem>,
}

/// What changed after applying a sync.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CanvasChange {
    /// Everything (snapshot).
    Reset,
    /// One item appeared or changed.
    Item(ItemId),
    /// One item disappeared.
    Removed(ItemId),
    /// The host echoed our own op: nothing to do.
    Echo,
}

impl CanvasDoc {
    /// Document version (0 before the first snapshot).
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Items, unordered.
    pub fn items(&self) -> impl Iterator<Item = &CanvasItem> {
        self.items.values()
    }

    /// One item.
    #[must_use]
    pub fn get(&self, id: ItemId) -> Option<&CanvasItem> {
        self.items.get(&id)
    }

    /// Items bottom to top (`z` ascending, id as tiebreak).
    #[must_use]
    pub fn by_z(&self) -> Vec<&CanvasItem> {
        let mut out: Vec<&CanvasItem> = self.items.values().collect();
        out.sort_by_key(|i| (i.z, i.id));
        out
    }

    /// The item showing `session`, if any.
    #[must_use]
    pub fn item_for_session(&self, session: slopty_core::SessionId) -> Option<&CanvasItem> {
        self.items
            .values()
            .find(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session))
    }

    /// A z above every item.
    #[must_use]
    pub fn top_z(&self) -> u32 {
        self.items.values().map(|i| i.z).max().map_or(0, |z| z.saturating_add(1))
    }

    /// Apply a host sync. `me` is this client's id, used to recognise echoes.
    pub fn apply_sync(&mut self, sync: CanvasSync, me: ClientId) -> CanvasChange {
        match sync {
            CanvasSync::Snapshot { version, items } => {
                self.version = version;
                self.items = items.into_iter().map(|i| (i.id, i)).collect();
                CanvasChange::Reset
            }
            CanvasSync::Delta { version, by, op } => {
                self.version = version;
                if by == me {
                    // Already applied optimistically. Re-apply anyway so a host-side clamp wins.
                    self.apply_op(&op);
                    return CanvasChange::Echo;
                }
                self.apply_op(&op)
            }
        }
    }

    /// Apply an op locally (optimistic path, and the host's deltas).
    pub fn apply_op(&mut self, op: &CanvasOp) -> CanvasChange {
        match op {
            CanvasOp::Upsert(item) => {
                self.items.insert(item.id, item.clone());
                CanvasChange::Item(item.id)
            }
            CanvasOp::Remove(id) => {
                self.items.remove(id);
                CanvasChange::Removed(*id)
            }
            CanvasOp::Place { id, rect } => match self.items.get_mut(id) {
                Some(item) => {
                    item.rect = *rect;
                    CanvasChange::Item(*id)
                }
                None => CanvasChange::Echo,
            },
            CanvasOp::Raise(id) => {
                let top = self.top_z();
                match self.items.get_mut(id) {
                    Some(item) => {
                        item.z = top;
                        CanvasChange::Item(*id)
                    }
                    None => CanvasChange::Echo,
                }
            }
            CanvasOp::Sleep { id, sleeping } => match self.items.get_mut(id) {
                Some(item) => {
                    item.sleeping = *sleeping;
                    CanvasChange::Item(*id)
                }
                None => CanvasChange::Echo,
            },
        }
    }

    /// Where a new `size`d item goes: to the right of the rightmost item on the top row, or at
    /// the origin when empty. Snapped to [`SNAP`].
    #[must_use]
    pub fn free_slot(&self, size: (f32, f32)) -> Rect {
        let (w, h) = size;
        let Some(rightmost) =
            self.items.values().map(|i| i.rect).max_by(|a, b| (a.x + a.w).total_cmp(&(b.x + b.w)))
        else {
            return Rect { x: 0.0, y: 0.0, w, h };
        };
        let x = snap(rightmost.x + rightmost.w + GAP);
        let y = snap(rightmost.y);
        Rect { x, y, w, h }
    }
}

/// Round to the snap grid.
#[must_use]
pub fn snap(v: f32) -> f32 {
    (v / SNAP).round() * SNAP
}

/// Where the viewport looks. `x`/`y` is the canvas point at the top-left of the viewport;
/// `zoom` is screen points per canvas unit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Camera {
    /// Canvas x at the viewport's left edge.
    pub x: f32,
    /// Canvas y at the viewport's top edge.
    pub y: f32,
    /// Scale.
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { x: -GAP, y: -GAP, zoom: 1.0 }
    }
}

impl Camera {
    /// Canvas → screen (viewport-relative points).
    #[must_use]
    pub fn to_screen(self, r: Rect) -> Rect {
        Rect {
            x: (r.x - self.x) * self.zoom,
            y: (r.y - self.y) * self.zoom,
            w: r.w * self.zoom,
            h: r.h * self.zoom,
        }
    }

    /// Screen point → canvas point.
    #[must_use]
    pub fn to_canvas(self, sx: f32, sy: f32) -> (f32, f32) {
        (sx / self.zoom + self.x, sy / self.zoom + self.y)
    }

    /// Move the viewport by a screen-space delta.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.x -= dx / self.zoom;
        self.y -= dy / self.zoom;
    }

    /// Multiply zoom by `factor`, keeping the canvas point under screen `(sx, sy)` fixed.
    pub fn zoom_at(&mut self, factor: f32, sx: f32, sy: f32) {
        let (cx, cy) = self.to_canvas(sx, sy);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.x = cx - sx / self.zoom;
        self.y = cy - sy / self.zoom;
    }

    /// Whether terminals should draw as cards at this zoom.
    #[must_use]
    pub fn cards(&self) -> bool {
        self.zoom < CARD_ZOOM
    }

    /// Fit `rects` into a `(w, h)` viewport with padding, centred.
    pub fn fit(&mut self, rects: impl IntoIterator<Item = Rect>, viewport: (f32, f32)) {
        let mut bounds: Option<(f32, f32, f32, f32)> = None;
        for r in rects {
            let b = bounds.get_or_insert((r.x, r.y, r.x + r.w, r.y + r.h));
            b.0 = b.0.min(r.x);
            b.1 = b.1.min(r.y);
            b.2 = b.2.max(r.x + r.w);
            b.3 = b.3.max(r.y + r.h);
        }
        let Some((x0, y0, x1, y1)) = bounds else {
            *self = Self::default();
            return;
        };
        let (vw, vh) = viewport;
        let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
        let pad = GAP * 2.0;
        let zoom = ((vw - pad) / w).min((vh - pad) / h).clamp(MIN_ZOOM, 1.0);
        self.zoom = zoom;
        self.x = x0 - (vw / zoom - w) / 2.0;
        self.y = y0 - (vh / zoom - h) / 2.0;
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_core::SessionId;

    use super::*;

    fn term(x: f32, z: u32) -> CanvasItem {
        CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session: SessionId::new() },
            rect: Rect { x, y: 0.0, w: 100.0, h: 50.0 },
            z,
            group: None,
            sleeping: false,
        }
    }

    #[test]
    fn snapshot_then_delta_and_echo() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = CanvasDoc::default();
        let a = term(0.0, 0);
        let b = term(200.0, 1);
        let change = doc
            .apply_sync(CanvasSync::Snapshot { version: 3, items: vec![b.clone(), a.clone()] }, me);
        assert_eq!(change, CanvasChange::Reset);
        assert_eq!(doc.version(), 3);
        assert_eq!(doc.by_z().iter().map(|i| i.id).collect::<Vec<_>>(), vec![a.id, b.id]);

        let moved = Rect { x: 5.0, y: 5.0, w: 100.0, h: 50.0 };
        let change = doc.apply_sync(
            CanvasSync::Delta {
                version: 4,
                by: other,
                op: CanvasOp::Place { id: a.id, rect: moved },
            },
            me,
        );
        assert_eq!(change, CanvasChange::Item(a.id));
        assert_eq!(doc.get(a.id).map(|i| i.rect), Some(moved));

        let change =
            doc.apply_sync(CanvasSync::Delta { version: 5, by: me, op: CanvasOp::Raise(a.id) }, me);
        assert_eq!(change, CanvasChange::Echo);
        assert_eq!(doc.get(a.id).map(|i| i.z), Some(2));
        assert_eq!(doc.top_z(), 3);
    }

    #[test]
    fn free_slot_goes_right_and_snaps() {
        let mut doc = CanvasDoc::default();
        assert_eq!(doc.free_slot((640.0, 400.0)), Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 });
        doc.apply_op(&CanvasOp::Upsert(term(0.0, 0)));
        let slot = doc.free_slot((640.0, 400.0));
        assert!((slot.x - snap(100.0 + GAP)).abs() < f32::EPSILON, "{slot:?}");
        assert!((slot.x % SNAP).abs() < f32::EPSILON, "{slot:?}");
    }

    #[test]
    fn camera_round_trips_and_zooms_about_point() {
        let mut cam = Camera { x: 10.0, y: 20.0, zoom: 2.0 };
        let r = Rect { x: 30.0, y: 40.0, w: 10.0, h: 5.0 };
        let s = cam.to_screen(r);
        assert_eq!(s, Rect { x: 40.0, y: 40.0, w: 20.0, h: 10.0 });
        assert_eq!(cam.to_canvas(s.x, s.y), (r.x, r.y));
        // Zoom about the item's screen origin: it must stay put.
        cam.zoom_at(0.5, s.x, s.y);
        let s2 = cam.to_screen(r);
        assert!((s2.x - s.x).abs() < 1e-3 && (s2.y - s.y).abs() < 1e-3, "anchor moved: {s2:?}");
        assert!((cam.zoom - 1.0).abs() < 1e-6, "zoom {}", cam.zoom);
        cam.zoom_at(0.001, 0.0, 0.0);
        assert!((cam.zoom - MIN_ZOOM).abs() < 1e-6, "clamped");
    }

    #[test]
    fn fit_contains_everything() {
        let mut cam = Camera::default();
        let rects = [
            Rect { x: 0.0, y: 0.0, w: 1000.0, h: 500.0 },
            Rect { x: 2000.0, y: 800.0, w: 100.0, h: 100.0 },
        ];
        cam.fit(rects, (800.0, 600.0));
        for r in rects {
            let s = cam.to_screen(r);
            assert!(s.x >= 0.0 && s.y >= 0.0 && s.x + s.w <= 800.0 && s.y + s.h <= 600.0, "{s:?}");
        }
    }
}
