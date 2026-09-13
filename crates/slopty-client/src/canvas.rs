//! The canvas document as a client sees it, plus the camera that maps it to the screen.
//!
//! The host is authoritative: the client applies every [`CanvasSync`] it receives and proposes
//! changes as [`CanvasOp`]s. To keep dragging responsive the client applies its own proposals
//! immediately (optimistic) and skips the host's echo of them.

use std::collections::BTreeMap;

use slopty_core::{ClientId, ItemId};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, Rect};
use slopty_proto::handshake::ClientKind;

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
/// Size the host gives a new terminal item (`slopty_host::canvas::TERMINAL_SIZE`).
pub const TERMINAL_SIZE: (f32, f32) = (720.0, 440.0);

/// The document.
#[derive(Clone, Debug, Default)]
pub struct CanvasDoc {
    version: u64,
    items: BTreeMap<ItemId, CanvasItem>,
    /// Other clients and where they look; never this client.
    lookers: BTreeMap<ClientId, Looker>,
}

/// Another client's viewport on the canvas, from `CanvasSync::Presence`.
#[derive(Clone, PartialEq, Debug)]
pub struct Looker {
    /// Who.
    pub client: ClientId,
    /// What kind of device.
    pub kind: ClientKind,
    /// Its name.
    pub name: String,
    /// Where it looks, in canvas units.
    pub view: Rect,
}

/// What changed after applying a sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CanvasChange {
    /// Everything (snapshot).
    Reset,
    /// One item appeared or changed.
    Item(ItemId),
    /// One item disappeared.
    Removed(ItemId),
    /// The host echoed our own op: nothing to do.
    Echo,
    /// Another client moved its viewport, or left.
    Presence(ClientId),
    /// Another client pointed at this item. Ephemeral: nothing in the document changed, and
    /// the item may be one this document does not have.
    Pointed(ItemId),
}

impl CanvasDoc {
    /// Document version (0 before the first snapshot).
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Other clients and where they look, in a stable order.
    pub fn lookers(&self) -> impl Iterator<Item = &Looker> {
        self.lookers.values()
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
            CanvasSync::Presence { client, .. } if client == me => CanvasChange::Echo,
            CanvasSync::Presence { client, kind, name, view } => {
                match view {
                    Some(view) => {
                        self.lookers.insert(client, Looker { client, kind, name, view });
                    }
                    None => {
                        self.lookers.remove(&client);
                    }
                }
                CanvasChange::Presence(client)
            }
            CanvasSync::Pointed { client, .. } if client == me => CanvasChange::Echo,
            CanvasSync::Pointed { item, .. } => CanvasChange::Pointed(item),
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

    /// Bring `r` into view at a usable size with the least movement: pan when it fits at the
    /// current zoom and the zoom is not card level; otherwise zoom to fit it, clamped to
    /// `CARD_ZOOM..=1` (a terminal stays a terminal on a phone that cannot fit a
    /// desktop-sized one; it may then hang off the right/bottom edge), anchored top-left.
    pub fn reveal(&mut self, r: Rect, viewport: (f32, f32)) {
        let (vw, vh) = viewport;
        let pad = GAP * 2.0;
        let fit_zoom = ((vw - pad) / r.w.max(1.0)).min((vh - pad) / r.h.max(1.0));
        if fit_zoom < self.zoom || self.cards() {
            self.zoom = fit_zoom.clamp(CARD_ZOOM, 1.0);
            self.x = r.x - GAP / self.zoom;
            self.y = r.y - GAP / self.zoom;
            return;
        }
        let s = self.to_screen(r);
        let dx = if s.x < GAP {
            GAP - s.x
        } else if s.x + s.w > vw - GAP {
            (vw - GAP) - (s.x + s.w)
        } else {
            0.0
        };
        let dy = if s.y < GAP {
            GAP - s.y
        } else if s.y + s.h > vh - GAP {
            (vh - GAP) - (s.y + s.h)
        } else {
            0.0
        };
        self.pan(dx, dy);
    }

    /// The camera [`Self::fit`] would leave, without moving this one.
    #[must_use]
    pub fn fitted(rects: impl IntoIterator<Item = Rect>, viewport: (f32, f32)) -> Self {
        let mut camera = Self::default();
        camera.fit(rects, viewport);
        camera
    }

    /// The camera that puts `r`'s centre in the middle of the viewport at `zoom`.
    ///
    /// What ⌘0 wants: 100 % without losing the thing being looked at.
    #[must_use]
    pub fn centred_on(r: Rect, viewport: (f32, f32), zoom: f32) -> Self {
        let zoom = if zoom.is_finite() { zoom.clamp(MIN_ZOOM, MAX_ZOOM) } else { 1.0 };
        let (vw, vh) = viewport;
        Self {
            x: (r.x + r.w / 2.0) - vw / zoom / 2.0,
            y: (r.y + r.h / 2.0) - vh / zoom / 2.0,
            zoom,
        }
    }

    /// Interpolate towards `to`. `t` is clamped to `0..=1`.
    ///
    /// Zoom moves geometrically (a camera going 0.25 → 1 spends half the flight under 0.5, as
    /// the eye reads it) while the top-left corner is derived from the interpolated *centre*,
    /// so the thing in the middle of the viewport stays in the middle instead of swinging.
    #[must_use]
    pub fn lerp(self, to: Self, t: f32, viewport: (f32, f32)) -> Self {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
        let (vw, vh) = viewport;
        let centre = |c: Self| (c.x + vw / c.zoom / 2.0, c.y + vh / c.zoom / 2.0);
        let (fx, fy) = centre(self);
        let (tx, ty) = centre(to);
        let zoom = if self.zoom > 0.0 && to.zoom > 0.0 {
            self.zoom * (to.zoom / self.zoom).powf(t)
        } else {
            to.zoom
        };
        let (cx, cy) = ((tx - fx).mul_add(t, fx), (ty - fy).mul_add(t, fy));
        Self { x: cx - vw / zoom / 2.0, y: cy - vh / zoom / 2.0, zoom }
    }

    /// Fit `rects` into a `(w, h)` viewport with padding, centred.
    pub fn fit(&mut self, rects: impl IntoIterator<Item = Rect>, viewport: (f32, f32)) {
        let mut bounds: Option<(f32, f32, f32, f32)> = None;
        for r in rects {
            let b = bounds.get_or_insert((r.x, r.y, r.x, r.y));
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

/// How long a camera move takes, in seconds. Long enough to read as movement, short enough
/// that a second ⌘2 never feels queued.
pub const FLIGHT: f32 = 0.18;

/// Cubic ease-out: fast off the mark, settling at the end. `t` is clamped to `0..=1`.
#[must_use]
pub fn ease_out(t: f32) -> f32 {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
    let inv = 1.0 - t;
    (-inv).mul_add(inv * inv, 1.0)
}

/// A camera move in progress: pure, driven by whoever has a clock.
///
/// The UI advances it once per frame with the time since the last one, which is why there is
/// no timer anywhere: the render loop is the clock, and a test can be its own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Flight {
    from: Camera,
    to: Camera,
    viewport: (f32, f32),
    elapsed: f32,
    duration: f32,
}

impl Flight {
    /// A flight from `from` to `to` over `duration` seconds.
    #[must_use]
    pub fn new(from: Camera, to: Camera, viewport: (f32, f32), duration: f32) -> Self {
        let duration = if duration.is_finite() && duration > 0.0 { duration } else { 0.0 };
        Self { from, to, viewport, elapsed: 0.0, duration }
    }

    /// Where it ends.
    #[must_use]
    pub const fn target(&self) -> Camera {
        self.to
    }

    /// Whether the flight has landed.
    #[must_use]
    pub fn done(&self) -> bool {
        self.elapsed >= self.duration
    }

    /// Advance by `dt` seconds and return where the camera is now. A landed flight returns its
    /// target exactly, so the last frame is never a rounding error away from it.
    pub fn advance(&mut self, dt: f32) -> Camera {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        self.elapsed += dt;
        if self.done() {
            return self.to;
        }
        self.from.lerp(self.to, ease_out(self.elapsed / self.duration), self.viewport)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_core::SessionId;

    use super::*;

    #[test]
    fn presence_keeps_the_others_and_never_me() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = CanvasDoc::default();
        let view = Rect { x: 1.0, y: 2.0, w: 300.0, h: 200.0 };
        let mine = CanvasSync::Presence {
            client: me,
            kind: ClientKind::Mac,
            name: "me".to_owned(),
            view: Some(view),
        };
        assert_eq!(doc.apply_sync(mine, me), CanvasChange::Echo);
        assert_eq!(doc.lookers().count(), 0, "my own echo is not a looker");
        let theirs = CanvasSync::Presence {
            client: other,
            kind: ClientKind::IPhone,
            name: "phone".to_owned(),
            view: Some(view),
        };
        assert_eq!(doc.apply_sync(theirs, me), CanvasChange::Presence(other));
        let looker = doc.lookers().next().expect("one looker");
        assert_eq!(
            (looker.client, looker.kind, looker.name.as_str()),
            (other, ClientKind::IPhone, "phone")
        );
        assert_eq!(looker.view, view);
        let moved = Rect { x: 50.0, ..view };
        let again = CanvasSync::Presence {
            client: other,
            kind: ClientKind::IPhone,
            name: "phone".to_owned(),
            view: Some(moved),
        };
        doc.apply_sync(again, me);
        assert_eq!(doc.lookers().next().map(|l| l.view), Some(moved), "replaced, not added");
        let gone = CanvasSync::Presence {
            client: other,
            kind: ClientKind::IPhone,
            name: "phone".to_owned(),
            view: None,
        };
        assert_eq!(doc.apply_sync(gone, me), CanvasChange::Presence(other));
        assert_eq!(doc.lookers().count(), 0);
        assert_eq!(doc.version(), 0, "presence never touches the document version");
    }

    #[test]
    fn a_pointing_names_its_item_and_my_own_is_an_echo() {
        let me = ClientId::new();
        let other = ClientId::new();
        let mut doc = CanvasDoc::default();
        let item = ItemId::new();
        let mine = CanvasSync::Pointed { client: me, name: "me".to_owned(), item };
        assert_eq!(doc.apply_sync(mine, me), CanvasChange::Echo);
        let theirs = CanvasSync::Pointed { client: other, name: "phone".to_owned(), item };
        assert_eq!(doc.apply_sync(theirs, me), CanvasChange::Pointed(item), "even unknown");
        assert_eq!(doc.version(), 0, "a pointing never touches the document version");
        assert_eq!(doc.items().count(), 0);
    }

    fn term(x: f32, z: u32) -> CanvasItem {
        CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session: SessionId::new() },
            rect: Rect { x, y: 0.0, w: 100.0, h: 50.0 },
            z,
            group: None,
            sleeping: false,
            name: None,
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
    fn reveal_pans_when_the_item_fits_and_zooms_out_when_it_does_not() {
        let mut cam = Camera::default();
        let r = Rect { x: 1000.0, y: 0.0, w: 300.0, h: 200.0 };
        cam.reveal(r, (800.0, 600.0));
        let s = cam.to_screen(r);
        assert!((cam.zoom - 1.0).abs() < f32::EPSILON);
        assert!((s.x + s.w - (800.0 - GAP)).abs() < 0.01, "{s:?}");
        assert!(s.y >= GAP - 0.01, "{s:?}");

        let mut cam = Camera::default();
        let wide = Rect { x: 0.0, y: 0.0, w: 1200.0, h: 200.0 };
        cam.reveal(wide, (400.0, 800.0));
        assert!((cam.zoom - CARD_ZOOM).abs() < f32::EPSILON, "{cam:?}");
        let s = cam.to_screen(wide);
        assert!((s.x - GAP).abs() < 0.01 && (s.y - GAP).abs() < 0.01, "{s:?}");

        // Card level: zoom in to the item even though it "fits".
        let mut cam = Camera { x: 0.0, y: 0.0, zoom: 0.2 };
        let small = Rect { x: 2000.0, y: 0.0, w: 300.0, h: 200.0 };
        cam.reveal(small, (400.0, 800.0));
        assert!((cam.zoom - 1.0).abs() < f32::EPSILON, "{cam:?}");
        let s = cam.to_screen(small);
        assert!((s.x - GAP).abs() < 0.01 && (s.y - GAP).abs() < 0.01, "{s:?}");

        let mut cam = Camera::default();
        let tall = Rect { x: 0.0, y: 0.0, w: 300.0, h: 400.0 };
        cam.reveal(tall, (800.0, 300.0));
        assert!(cam.zoom < 1.0 && cam.zoom > CARD_ZOOM, "{cam:?}");
        let s = cam.to_screen(tall);
        assert!(s.y + s.h <= 300.0 - GAP + 0.01, "{s:?}");
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
    fn free_slot_follows_the_item_that_reaches_furthest_right() {
        let mut doc = CanvasDoc::default();
        // `wide` starts at the left but ends furthest right; `near` starts further right.
        let session = SessionId::new();
        let wide = CanvasItem {
            kind: ItemKind::Terminal { session },
            rect: Rect { x: 0.0, y: 64.0, w: 500.0, h: 50.0 },
            ..term(0.0, 0)
        };
        let near = term(100.0, 1);
        doc.apply_op(&CanvasOp::Upsert(wide.clone()));
        doc.apply_op(&CanvasOp::Upsert(near));
        assert_eq!(doc.items().count(), 2);
        assert_eq!(doc.item_for_session(session).map(|i| i.id), Some(wide.id));
        // The slot goes past `wide`'s right edge, on `wide`'s row.
        let slot = doc.free_slot((640.0, 400.0));
        assert_eq!((slot.x, slot.y), (snap(500.0 + GAP), 64.0));
        // A narrow item starting past `wide`'s edge reaches further still: the slot follows
        // its right edge, which is where it starts plus its width, not minus.
        doc.apply_op(&CanvasOp::Upsert(term(560.0, 2)));
        let slot = doc.free_slot((640.0, 400.0));
        assert_eq!((slot.x, slot.y), (snap(660.0 + GAP), 0.0), "{slot:?}");
        // Snapping rounds to the nearest grid line, in both directions.
        assert!((snap(100.0) - 96.0).abs() < f32::EPSILON);
        assert!((snap(-40.0) - -48.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_camera_pans_in_canvas_units_and_zooms_about_a_point_off_the_origin() {
        // A screen-space drag moves the camera against the drag, scaled by the zoom.
        let mut cam = Camera { x: 10.0, y: 20.0, zoom: 2.0 };
        cam.pan(4.0, -8.0);
        assert_eq!(cam, Camera { x: 8.0, y: 24.0, zoom: 2.0 });
        // Zooming in about screen (40, 40) keeps the canvas point under it where it is: the
        // camera's origin ends up 40 screen points, at the new zoom, before that point.
        let under = cam.to_canvas(40.0, 40.0);
        cam.zoom_at(2.0, 40.0, 40.0);
        assert_eq!(cam, Camera { x: under.0 - 10.0, y: under.1 - 10.0, zoom: 4.0 });
        assert_eq!(cam.to_canvas(40.0, 40.0), under);
        // Cards start below the card zoom, not at it.
        assert!(!Camera { x: 0.0, y: 0.0, zoom: CARD_ZOOM }.cards());
        assert!(Camera { x: 0.0, y: 0.0, zoom: CARD_ZOOM - 0.01 }.cards());
    }

    #[test]
    fn reveal_nudges_the_least_on_each_side_and_fits_by_the_tighter_axis() {
        let vp = (600.0, 400.0);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
        let origin = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
        let inset = GAP * 2.0;
        // Off the left and above: the item lands one gap inside both edges.
        let mut cam = origin;
        let corner = Rect { x: -100.0, y: -50.0, w: 200.0, h: 100.0 };
        cam.reveal(corner, vp);
        let s = cam.to_screen(corner);
        assert!(close(s.x, GAP) && close(s.y, GAP), "{s:?}");
        // Off the right and below: its far edges land one gap inside.
        let mut cam = origin;
        let far = Rect { x: 700.0, y: 500.0, w: 200.0, h: 100.0 };
        cam.reveal(far, vp);
        let s = cam.to_screen(far);
        assert!(close(s.x + s.w, vp.0 - GAP) && close(s.y + s.h, vp.1 - GAP), "{s:?}");
        // Exactly one gap inside every edge is in view: nothing moves.
        let mut cam = origin;
        let edge = Rect { x: GAP, y: GAP, w: vp.0 - inset, h: vp.1 - inset };
        cam.reveal(edge, vp);
        assert_eq!(cam, origin);
        // Well inside: nothing moves either (the far edges are compared, not the near ones).
        let mut cam = origin;
        cam.reveal(Rect { x: 100.0, y: 100.0, w: 200.0, h: 100.0 }, vp);
        assert_eq!(cam, origin);
        // A little past the right and bottom gaps: nudged by exactly that little.
        let mut cam = origin;
        let over = Rect { x: vp.0 - GAP - 186.0, y: vp.1 - GAP - 86.0, w: 200.0, h: 100.0 };
        cam.reveal(over, vp);
        assert!(close(cam.x, 14.0) && close(cam.y, 14.0), "{cam:?}");
        // An item that fits exactly at the current zoom is panned to, not re-zoomed: the
        // vertical axis, already in view, stays where it was.
        let mut cam = Camera { x: 500.0, y: 0.0, zoom: 1.0 };
        let exact = Rect { x: 0.0, y: 100.0, w: vp.0 - inset, h: 200.0 };
        cam.reveal(exact, vp);
        assert!(close(cam.zoom, 1.0) && close(cam.x, -GAP) && close(cam.y, 0.0), "{cam:?}");
        // Too wide for the viewport: the width decides the zoom, less a gap on each side.
        let mut cam = origin;
        cam.reveal(Rect { x: 0.0, y: 0.0, w: 800.0, h: 100.0 }, vp);
        assert!(close(cam.zoom, (vp.0 - inset) / 800.0), "{cam:?}");
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

    /// The camera flies with a fake clock: an ease-out that starts fast, lands exactly on the
    /// target, and keeps whatever is in the middle of the viewport in the middle.
    #[test]
    fn a_flight_eases_out_and_lands_on_its_target() {
        let vp = (800.0, 600.0);
        let from = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
        let to = Camera::centred_on(Rect { x: 2000.0, y: 1000.0, w: 400.0, h: 300.0 }, vp, 0.5);
        let mut flight = Flight::new(from, to, vp, FLIGHT);

        let quarter = flight.advance(FLIGHT / 4.0);
        assert!(!flight.done());
        // Ease-out: a quarter of the time is more than a quarter of the way there.
        let progress = (quarter.x - from.x) / (to.x - from.x);
        assert!(progress > 0.25 && progress < 1.0, "{progress}");

        let mid = flight.advance(FLIGHT / 4.0);
        assert!(mid.x > quarter.x, "still moving: {mid:?} after {quarter:?}");
        assert!(mid.zoom < from.zoom && mid.zoom > to.zoom, "zoom follows: {mid:?}");

        // Time only runs forward: a negative or unreal step leaves the flight where it is.
        assert_eq!(flight.advance(-1.0), mid);
        assert_eq!(flight.advance(f32::NAN), mid);

        let landed = flight.advance(FLIGHT);
        assert!(flight.done());
        assert_eq!(landed, to, "the last frame is the target exactly");
        assert_eq!(flight.advance(1.0), to, "and stays there");
    }

    /// Mid-flight the viewport centre travels straight from one centre to the other, so a
    /// zoom-out and a pan read as one movement instead of a swing.
    #[test]
    fn the_middle_of_the_viewport_goes_straight_there() {
        let vp = (800.0, 600.0);
        let from = Camera { x: 0.0, y: 0.0, zoom: 2.0 };
        let to = Camera { x: 900.0, y: 300.0, zoom: 0.5 };
        let centre = |c: Camera| (c.x + vp.0 / c.zoom / 2.0, c.y + vp.1 / c.zoom / 2.0);
        let (fx, fy) = centre(from);
        let (tx, ty) = centre(to);
        for t in [0.0, 0.25, 0.5, 0.75, 1.0_f32] {
            let (cx, cy) = centre(from.lerp(to, t, vp));
            assert!((cx - (tx - fx).mul_add(t, fx)).abs() < 0.01, "at {t}: {cx}");
            assert!((cy - (ty - fy).mul_add(t, fy)).abs() < 0.01, "at {t}: {cy}");
        }
        // Zoom moves geometrically: halfway through, the scale is the geometric mean.
        let half = from.lerp(to, 0.5, vp).zoom;
        assert!((half - (2.0_f32 * 0.5).sqrt()).abs() < 0.01, "{half}");
    }

    /// ⌘0: 100 % without losing what is being looked at.
    #[test]
    fn reset_keeps_the_item_in_the_middle() {
        let vp = (800.0, 600.0);
        let item = Rect { x: 100.0, y: 50.0, w: 400.0, h: 300.0 };
        let cam = Camera::centred_on(item, vp, 1.0);
        assert!((cam.zoom - 1.0).abs() < f32::EPSILON);
        let screen = cam.to_screen(item);
        assert!((screen.x + screen.w / 2.0 - vp.0 / 2.0).abs() < 0.01, "{screen:?}");
        assert!((screen.y + screen.h / 2.0 - vp.1 / 2.0).abs() < 0.01, "{screen:?}");

        // And the same at another zoom, since ⌘2 lands through the same constructor.
        let closer = Camera::centred_on(item, vp, 0.5);
        let screen = closer.to_screen(item);
        assert!((screen.x + screen.w / 2.0 - vp.0 / 2.0).abs() < 0.01, "{screen:?}");
    }

    /// `fitted` is `fit` without moving the camera it was called on.
    #[test]
    fn fit_puts_the_union_in_the_middle_and_fills_the_tighter_side() {
        let vp = (800.0, 600.0);
        let inset = GAP * 2.0;
        let close = |a: f32, b: f32| (a - b).abs() < 0.01;
        let mut cam = Camera::default();
        // The union runs from the first rect's corner to the second's far corner.
        cam.fit(
            [
                Rect { x: 100.0, y: 50.0, w: 1000.0, h: 500.0 },
                Rect { x: 2000.0, y: 800.0, w: 100.0, h: 100.0 },
            ],
            vp,
        );
        let union = cam.to_screen(Rect { x: 100.0, y: 50.0, w: 2000.0, h: 850.0 });
        // Wider than tall for this viewport: the width fills it less a gap on each side, and
        // the union is centred both ways.
        assert!(close(union.x, GAP) && close(union.w, vp.0 - inset), "{union:?}");
        assert!(close(union.y + union.h / 2.0, vp.1 / 2.0), "{union:?}");
        // Taller than wide: the height is the tighter side, less a gap top and bottom.
        let mut cam = Camera::default();
        cam.fit([Rect { x: 0.0, y: 0.0, w: 100.0, h: 1000.0 }], vp);
        let tall = cam.to_screen(Rect { x: 0.0, y: 0.0, w: 100.0, h: 1000.0 });
        assert!(close(cam.zoom, (vp.1 - inset) / 1000.0), "{cam:?}");
        assert!(close(tall.y, GAP) && close(tall.h, vp.1 - inset), "{tall:?}");
        assert!(close(tall.x + tall.w / 2.0, vp.0 / 2.0), "{tall:?}");
        // Zoom is a fit, never a blow-up: a small union sits at 1:1 in the middle.
        let mut cam = Camera::default();
        cam.fit([Rect { x: 10.0, y: 20.0, w: 200.0, h: 100.0 }], vp);
        let small = cam.to_screen(Rect { x: 10.0, y: 20.0, w: 200.0, h: 100.0 });
        assert!(close(cam.zoom, 1.0), "{cam:?}");
        assert!(close(small.x + small.w / 2.0, vp.0 / 2.0), "{small:?}");
        assert!(close(small.y + small.h / 2.0, vp.1 / 2.0), "{small:?}");
        // The centre lands at any zoom, on both axes.
        let item = Rect { x: 100.0, y: 50.0, w: 400.0, h: 300.0 };
        let screen = Camera::centred_on(item, vp, 0.5).to_screen(item);
        assert!(close(screen.y + screen.h / 2.0, vp.1 / 2.0), "{screen:?}");
        // Cubic ease-out: half the time is seven eighths of the way.
        assert!(
            close(ease_out(0.5), 0.875) && close(ease_out(0.0), 0.0) && close(ease_out(1.0), 1.0)
        );
        // A zoom of zero on either side cannot be interpolated through: the target's is taken.
        let flat = Camera { x: 0.0, y: 0.0, zoom: 0.0 };
        let one = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
        assert!(close(flat.lerp(one, 0.5, vp).zoom, 1.0));
        assert!(close(one.lerp(flat, 0.0, vp).zoom, 0.0));
    }

    #[test]
    fn fitted_is_fit_without_the_side_effect() {
        let vp = (800.0, 600.0);
        let rects = [
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 },
            Rect { x: 900.0, y: 400.0, w: 200.0, h: 200.0 },
        ];
        let mut moved = Camera::default();
        moved.fit(rects, vp);
        assert_eq!(Camera::fitted(rects, vp), moved);
    }
}
