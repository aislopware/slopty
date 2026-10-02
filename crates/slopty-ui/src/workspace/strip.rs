//! The strip: the tiles drawn where the layout's frame puts them, and the pointer and the
//! gestures that move it.
//!
//! A horizontal two-finger swipe (or a finger's pan on a phone) drags the strip, snapping to
//! a column when it ends, with the fling the swipe tracker measured; a vertical one scrolls
//! what is under it, or, over the bare strip, switches workspace. The axis is decided after
//! 16 points and kept for the gesture. The momentum macOS sends after the fingers lift is
//! swallowed after a strip or workspace gesture, which has already snapped. A focused remote
//! window takes every swipe over its picture (on a phone, every remote picture does). ⌘⌥ and
//! the wheel steps columns and workspaces. A pinch in opens the overview; out closes it. A pinch
//! that begins over a remote picture which would take a sideways swipe zooms that picture
//! instead (`screen::zoom`), for the whole of the pinch.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, App, Bounds, Context, DispatchPhase, Entity, EntityId,
    FontWeight, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, PinchEvent, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, TouchPhase,
    WeakEntity, Window, canvas, div, px,
};
use slopty_client::layout::{
    Axis, AxisLock, DropTarget, Frame, Rect, Strip, TileRef, WHEEL_TICK, WorkerKey,
};
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::ItemKind;
use slopty_theme::{Typography, alpha};

use super::WorkspaceView;
use super::marks::Marks;
use super::tile::Chrome;
use crate::colors::{hsla, hsla_alpha};
use crate::draw::Draw;
use crate::icons::IconName;
use crate::kit;

/// How far a header press travels before it is a move rather than a click.
const DRAG_SLOP: f32 = 4.0;

/// How much pinch it takes to open or close the overview.
const PINCH_STEP: f32 = 0.15;

/// The resize handle's width, centred on the divider it drags so either side of the line
/// takes the press: 12 pt round the hairline, which a pointer finds without hunting.
const HANDLE_W: f32 = 12.0;

/// One hairline: the divider a tile draws inside its right edge, and the accent line a
/// handle lays over it.
const HAIRLINE: f32 = 1.0;

/// The line that marks where a drop opens a new column or workspace.
const DROP_LINE: f32 = 2.0;

/// One mark of a drop hint: `rect` filled with `ink`, square and frameless.
fn hint(selector: &'static str, rect: Rect, ink: gpui::Hsla) -> gpui::AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.w))
        .h(px(rect.h))
        .bg(ink)
        .into_any_element()
}

/// The group every resize handle belongs to, so its line lights while the pointer is on it.
const HANDLE_GROUP: &str = "column-divider";

/// The pointer in progress over the strip.
pub(super) enum Drag {
    /// A header pressed: a move once it travels [`DRAG_SLOP`].
    Move { tile: TileRef, grab: Point<Pixels>, moving: bool, target: Option<DropTarget> },
    /// The divider right of `column` pressed.
    Resize {
        column: usize,
        grab: Point<Pixels>,
        /// Every remote window's size before, to ask them to follow afterwards.
        before: Vec<(ItemId, (f32, f32))>,
    },
}

/// A trackpad swipe or touch pan in progress over the strip.
#[derive(Default)]
pub(super) struct Gesture {
    phase: Phase,
    lock: AxisLock,
    /// The momentum after a strip or workspace swipe is not for the content.
    swallow_coast: bool,
    /// Pinch travelled since it began.
    pinch: f32,
    /// The pinch in progress began over a remote picture, which takes it.
    pinch_to_content: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Phase {
    #[default]
    Idle,
    Undecided,
    Strip,
    Workspace,
    Content,
}

/// What the strip drew, kept by the strip's view ([`super::StripHost`]) and read by the
/// workspace's handlers: where each tile went, the zoom and the strip's bounds, the tiles on
/// screen, where the thumb sits and whether it shows. The strip's build writes it while it
/// reads the workspace and writes nothing there, so a frame of motion is no news for anything
/// else; hence cells, which the build holds shared.
pub(super) struct Drawn {
    /// The active workspace's strip as the last build laid it out: where the thumb sits.
    pub strip: RefCell<Strip>,
    /// The zoom the tiles were last drawn at (the overview's).
    pub zoom: Cell<f32>,
    /// The strip's bounds in the window, as of the last frame.
    pub viewport: Cell<Bounds<Pixels>>,
    /// Where each drawn tile was last frame, in window coordinates.
    pub placed: RefCell<Vec<(TileRef, Bounds<Pixels>)>>,
    /// The tile focused when the tiles were last drawn.
    pub focus: Cell<Option<TileRef>>,
    /// Tiles on screen in the frame last drawn: what the status bar need not count again.
    pub on_screen: RefCell<HashSet<ItemId>>,
    /// Bumped every time the strip builds.
    pub builds: Cell<u64>,
    /// What the last build handed each body it drew, and what the one before handed
    /// ([`WorkspaceView::hand_over`]).
    pub handed: RefCell<HashMap<EntityId, Handed>>,
    pub handed_before: RefCell<HashMap<EntityId, Handed>>,
    /// Whether the thumb shows ([`super::marks`]), the pointer is near the strip's bottom
    /// edge, the timer that takes the thumb down, and the generation of its fade.
    pub marks: Cell<Marks>,
    pub marks_near: Cell<bool>,
    pub marks_timer: RefCell<Option<gpui::Task<()>>>,
    pub marks_gen: Cell<u64>,
    /// A frame of motion is asked for and not yet run: one chain of them, however many
    /// builds and pointer moves ask.
    pub motion: Cell<bool>,
}

/// What a body takes from its tile: its zoom, and what else its kind is laid out by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Handed {
    Shell { zoom: f32, covered: bool, zooming: bool },
    Face { zoom: f32, width: f32 },
    Board { zoom: f32, width: f32 },
    Review { zoom: f32, width: f32 },
    Stream { painted: f32 },
    Text { zoom: f32, pad: f32, size: f32 },
    Folder { zoom: f32 },
    Page { live: bool },
}

impl Default for Drawn {
    fn default() -> Self {
        Self {
            strip: RefCell::default(),
            zoom: Cell::new(1.0),
            viewport: Cell::default(),
            placed: RefCell::default(),
            focus: Cell::default(),
            on_screen: RefCell::default(),
            builds: Cell::default(),
            handed: RefCell::default(),
            handed_before: RefCell::default(),
            marks: Cell::default(),
            marks_near: Cell::default(),
            marks_timer: RefCell::default(),
            marks_gen: Cell::default(),
            motion: Cell::default(),
        }
    }
}

impl WorkspaceView {
    /// Hand `view` what it takes from its tile, `handed`, once the strip has read the
    /// workspace, and only when it differs from what the last build handed it: the body is then
    /// built again in this frame. Compared with what was handed, never read off the body: a read
    /// would build the strip again with everything the body does.
    pub(super) fn hand_over<V: 'static>(
        &self,
        cx: &Draw<'_, Self>,
        view: &Entity<V>,
        handed: Handed,
        set: impl FnOnce(&mut V, &mut Context<V>) + 'static,
    ) {
        let id = view.entity_id();
        self.drawn.handed.borrow_mut().insert(id, handed);
        if self.drawn.handed_before.borrow().get(&id) != Some(&handed) {
            let view = view.clone();
            cx.later(move |_window, cx| view.update(cx, set));
        }
    }

    /// A tile was pressed: it takes the focus (and in the overview, the overview closes on it).
    pub(super) fn click_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let overview = self.layout.overview_open();
        if self.focused() != Some(tile) || overview {
            self.tick();
            self.layout.focus(tile);
            if overview {
                self.layout.set_overview(false);
            }
            self.after_focus_moved(cx);
            // A click lands in the terminal itself; the keyboard goes with it now.
            self.pending_focus = None;
            if let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| &i.kind) {
                self.pending_focus = Some(*session);
            }
            self.layout_touched(cx);
            cx.notify();
        }
    }

    pub(super) fn begin_move(
        &mut self,
        tile: TileRef,
        ev: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.click_tile(tile, cx);
        self.drag = Some(Drag::Move { tile, grab: ev.position, moving: false, target: None });
    }

    fn begin_resize(&mut self, column: usize, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if ev.click_count == 2 {
            self.reset_column_width(column, cx);
            cx.stop_propagation();
            return;
        }
        self.tick();
        let before = self.column_sizes();
        if self.layout.resize_begin(column) {
            self.drag = Some(Drag::Resize { column, grab: ev.position, before });
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// The divider right of `column` double-clicked: the column goes back to the width a new
    /// column opens at, and the remote windows in it follow.
    fn reset_column_width(&mut self, column: usize, cx: &mut Context<Self>) {
        self.tick();
        let before = self.column_sizes();
        if self.layout.reset_column_width(column) {
            self.resize_remote_windows(&before, cx);
            self.layout_touched(cx);
            cx.notify();
        }
    }

    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        let d = p - self.drawn.viewport.get().origin;
        (f32::from(d.x), f32::from(d.y))
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let viewport = self.drawn.viewport.get();
        let (top, height) = (viewport.top(), viewport.size.height);
        self.pointer_over_strip(f32::from(ev.position.y - top), f32::from(height), cx);
        if self.drag.is_none() {
            return;
        }
        if ev.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        self.tick();
        let (x, y) = self.local(ev.position);
        match &mut self.drag {
            Some(Drag::Move { grab, moving, target, .. }) => {
                let d = ev.position - *grab;
                if !*moving && f32::from(d.x).hypot(f32::from(d.y)) < DRAG_SLOP {
                    return;
                }
                *moving = true;
                *target = self.layout.drop_target(x, y);
                self.layout.dnd_edge_scroll(x);
            }
            Some(Drag::Resize { grab, .. }) => {
                let dx = f32::from(ev.position.x - grab.x);
                self.layout.resize_update(dx);
            }
            None => {}
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _ev: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        self.end_drag(cx);
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        self.tick();
        match drag {
            Drag::Move { tile, moving: true, target: Some(target), .. } => {
                self.layout.move_tile(tile, target);
                self.layout.dnd_scroll_end();
                self.after_focus_moved(cx);
            }
            Drag::Move { .. } => self.layout.dnd_scroll_end(),
            Drag::Resize { before, .. } => {
                self.layout.resize_end();
                self.resize_remote_windows(&before, cx);
            }
        }
        self.layout_touched(cx);
        cx.notify();
    }

    /// Where a scroll event lands: the tile under it and whether it is on the body (not the
    /// header).
    pub(super) fn under(&self, p: Point<Pixels>) -> Option<(TileRef, bool)> {
        let zoom = self.drawn.zoom.get();
        self.drawn.placed.borrow().iter().rev().find(|(_, b)| b.contains(&p)).map(|(tile, b)| {
            let body = p.y > b.origin.y + px(self.theme.density.header * zoom);
            (*tile, body)
        })
    }

    /// Whether the content under `p` keeps a swipe on `axis` for itself.
    fn content_takes(&self, p: Point<Pixels>, axis: Axis) -> bool {
        let Some((tile, body)) = self.under(p) else { return false };
        match axis {
            Axis::Vertical => true,
            Axis::Horizontal => {
                let remote = matches!(
                    self.item(tile).map(|i| &i.kind),
                    Some(ItemKind::Window { .. } | ItemKind::Display { .. })
                );
                body && remote && (cfg!(target_os = "ios") || self.focused() == Some(tile))
            }
            Axis::Undecided => false,
        }
    }

    /// Whether a pinch beginning at `p` zooms the remote picture under it rather than the
    /// strip: the picture that would keep a sideways swipe, with the overview closed (there
    /// the tiles are miniatures, and a pinch is the overview's).
    fn content_takes_pinch(&self, p: Point<Pixels>) -> bool {
        !self.layout.overview_open() && self.content_takes(p, Axis::Horizontal)
    }

    /// Every scroll over the strip, before the tiles see it (see the module docs).
    fn scroll_captured(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * WHEEL_TICK, l.y * WHEEL_TICK),
        };
        self.tick();
        let now = self.now();
        if ev.modifiers.platform && ev.modifiers.alt {
            if self.layout.wheel(-dx, -dy, now) {
                self.after_focus_moved(cx);
                self.layout_touched(cx);
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // A mouse wheel scrolls what it is over.
        if matches!(ev.delta, ScrollDelta::Lines(_)) {
            return;
        }
        if ev.momentum_phase.is_some() {
            if self.gesture.swallow_coast {
                cx.stop_propagation();
            }
            return;
        }
        match ev.touch_phase {
            TouchPhase::Started => {
                self.end_gesture(true, cx);
                self.gesture.phase = Phase::Undecided;
                self.gesture.lock = AxisLock::new();
                self.gesture.swallow_coast = false;
                self.gesture_moved(ev.position, dx, dy, now, cx);
            }
            TouchPhase::Moved => self.gesture_moved(ev.position, dx, dy, now, cx),
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let owned = matches!(self.gesture.phase, Phase::Strip | Phase::Workspace);
                self.end_gesture(ev.touch_phase == TouchPhase::Cancelled, cx);
                self.gesture.swallow_coast = owned;
                if owned {
                    cx.stop_propagation();
                }
            }
        }
    }

    fn gesture_moved(
        &mut self,
        at: Point<Pixels>,
        dx: f32,
        dy: f32,
        now: std::time::Duration,
        cx: &mut Context<Self>,
    ) {
        match self.gesture.phase {
            Phase::Idle | Phase::Content => {}
            Phase::Undecided => match self.gesture.lock.feed(-dx, -dy) {
                Axis::Undecided => {
                    // Until the axis is known, a sideways step is held back; an up-or-down
                    // one reaches the content, which keeps it if the swipe turns out to be
                    // vertical.
                    if dx.abs() > dy.abs() {
                        cx.stop_propagation();
                    }
                }
                axis if self.content_takes(at, axis) => self.gesture.phase = Phase::Content,
                Axis::Horizontal => {
                    let (px_, _) = self.gesture.lock.pending();
                    self.gesture.phase = Phase::Strip;
                    self.layout.view_gesture_begin();
                    self.layout.view_gesture_update(px_, now);
                    cx.stop_propagation();
                    cx.notify();
                }
                Axis::Vertical => {
                    let (_, py) = self.gesture.lock.pending();
                    self.gesture.phase = Phase::Workspace;
                    self.layout.ws_gesture_begin();
                    self.layout.ws_gesture_update(py, now);
                    cx.stop_propagation();
                    cx.notify();
                }
            },
            Phase::Strip => {
                self.layout.view_gesture_update(-dx, now);
                cx.stop_propagation();
                cx.notify();
            }
            Phase::Workspace => {
                self.layout.ws_gesture_update(-dy, now);
                cx.stop_propagation();
                cx.notify();
            }
        }
    }

    fn end_gesture(&mut self, cancelled: bool, cx: &mut Context<Self>) {
        let ended = match self.gesture.phase {
            Phase::Strip => self.layout.view_gesture_end(cancelled),
            Phase::Workspace => self.layout.ws_gesture_end(cancelled),
            Phase::Idle | Phase::Undecided | Phase::Content => false,
        };
        self.gesture.phase = Phase::Idle;
        if ended {
            self.after_focus_moved(cx);
            self.layout_touched(cx);
            cx.notify();
        }
    }

    fn pinch(&mut self, ev: &PinchEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if ev.phase == TouchPhase::Started {
            self.gesture.pinch = 0.0;
            self.gesture.pinch_to_content = self.content_takes_pinch(ev.position);
        }
        let to_content = self.gesture.pinch_to_content;
        if matches!(ev.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            self.gesture.pinch_to_content = false;
        }
        if to_content {
            return;
        }
        // The overview's pinch: the picture under it must not zoom as well.
        cx.stop_propagation();
        self.gesture.pinch += ev.delta;
        let open = self.layout.overview_open();
        let flip =
            if open { self.gesture.pinch > PINCH_STEP } else { self.gesture.pinch < -PINCH_STEP };
        if flip {
            self.gesture.pinch = 0.0;
            self.tick();
            self.layout.set_overview(!open);
            cx.notify();
        }
    }

    /// The strip's area was measured at a new size: the layout lays out for it, and one more
    /// frame draws the new size. Called once the frame that measured it is over.
    fn strip_resized(&mut self, size: gpui::Size<Pixels>, cx: &mut Context<Self>) {
        self.layout.set_viewport(f32::from(size.width), f32::from(size.height));
        cx.notify();
        // The tiles are placed from the layout just changed, and the strip's view is drawn
        // again only when told.
        App::notify(cx, self.strip_host.entity_id());
    }

    /// The tiles on screen in this build: `visible`, remote ones first seen off screen from
    /// now, and the streams of those off screen past the grace let go. What changed is the
    /// workspace's, so it is done once the frame is over, and only when there is something to
    /// do: a timer comes back for the streams once the grace is up, since an idle strip draws
    /// no frames.
    fn track_visibility(&self, frame: &Frame, cx: &Draw<'_, Self>) {
        let (w, h) = self.layout.viewport();
        let screen = Rect { x: 0.0, y: 0.0, w, h };
        let visible: Vec<ItemId> = frame
            .tiles
            .iter()
            .filter(|p| !p.hidden && p.rect.intersects(&screen))
            .map(|p| p.tile.item)
            .collect();
        let on_screen: HashSet<ItemId> = visible.iter().copied().collect();
        let moved = on_screen != *self.drawn.on_screen.borrow();
        if moved {
            *self.drawn.on_screen.borrow_mut() = on_screen;
        }
        if !moved && !self.visibility_due(&visible) {
            return;
        }
        let this = cx.weak_entity();
        cx.later(move |_window, cx| {
            cx.defer(move |cx| {
                let _gone = this.update(cx, |this, cx| this.visibility_changed(&visible, cx));
            });
        });
    }

    /// The remote tiles' seen and unseen marks after `visible`, and the park timer.
    fn visibility_changed(&mut self, visible: &[ItemId], cx: &Context<Self>) {
        self.note_visible(visible);
        let waiting = self.unseen.keys().any(|id| !self.parked.contains(id));
        if waiting && !self.park_pending {
            self.park_pending = true;
            let grace = self.stream_grace;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(grace).await;
                let _gone = this.update(cx, |this, cx| {
                    this.park_pending = false;
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Where a drop would land, and how much room it takes there. A new column is an accent
    /// line on the divider it would open, over a faint accent wash as wide as the column the
    /// tile brings; joining a column washes the share of it the tile would take (the lower
    /// half of a column of one); a new workspace is a line in the band between two. Nothing
    /// has a corner or a frame, since the panes they mark have none.
    fn drop_hint(&self, frame: &Frame) -> Vec<gpui::AnyElement> {
        let Some(Drag::Move { tile: dragged, moving: true, target: Some(target), .. }) = &self.drag
        else {
            return Vec::new();
        };
        let dragged = frame.tiles.iter().find(|p| p.tile == *dragged);
        let shown = |ws: usize, col: usize| {
            frame
                .tiles
                .iter()
                .filter(move |p| p.pos.workspace == ws && p.pos.column == col && !p.hidden)
        };
        let column = |ws: usize, col: usize| -> Option<Rect> {
            let rects: Vec<Rect> = shown(ws, col).map(|p| p.rect).collect();
            let first = rects.first()?;
            let (x, y) = (first.x, rects.iter().map(|r| r.y).fold(f32::MAX, f32::min));
            let bottom = rects.iter().map(Rect::bottom).fold(f32::MIN, f32::max);
            Some(Rect { x, y, w: first.w, h: bottom - y })
        };
        let row = |ix: usize| frame.workspaces.iter().find(|(i, _)| *i == ix).map(|(_, r)| *r);
        let accent = self.theme.surfaces.accent_fill;
        let (wash, line) = match *target {
            DropTarget::IntoColumn { workspace, column: col, index } => {
                let Some(r) = column(workspace, col) else { return Vec::new() };
                // A tabbed column takes it as one more tab: the whole of it. A stacked one
                // makes room for one more share, where it goes in.
                let tabbed = shown(workspace, col).any(|p| p.tabs.is_some());
                let own = dragged.filter(|d| d.pos.workspace == workspace && d.pos.column == col);
                let others =
                    shown(workspace, col).count().saturating_sub(usize::from(own.is_some()));
                let index = match own {
                    Some(d) if d.pos.tile < index => index.saturating_sub(1),
                    _ => index,
                };
                #[expect(clippy::cast_precision_loss, reason = "a column holds a few tiles")]
                let (share, at) = ((others.saturating_add(1)) as f32, index as f32);
                let slot = if tabbed {
                    r
                } else {
                    Rect { y: (r.h / share).mul_add(at, r.y), h: r.h / share, ..r }
                };
                (Some(slot), None)
            }
            DropTarget::NewColumn { workspace, index } => {
                // The line straddles the divider the new column opens; the wash runs right of
                // it as wide as the tile's column, which travels with it.
                let before = index.checked_sub(1).and_then(|i| column(workspace, i));
                let (x, r) = match (column(workspace, index), before) {
                    (Some(r), _) => (r.x, r),
                    (None, Some(r)) => (r.right(), r),
                    // An empty workspace: the whole of it is the new column.
                    (None, None) => {
                        let Some(r) = row(workspace) else { return Vec::new() };
                        return vec![hint("drop-wash", r, hsla_alpha(accent, alpha::FAINT))];
                    }
                };
                let line = Rect { x: x - DROP_LINE / 2.0, y: r.y, w: DROP_LINE, h: r.h };
                let width = dragged.map_or(r.w, |d| d.rect.w);
                let limit = row(workspace).map_or(f32::MAX, |w| w.right());
                let wash = Rect { x, y: r.y, w: width.min(limit - x).max(0.0), h: r.h };
                (Some(wash), Some(line))
            }
            DropTarget::NewWorkspace { index } => {
                // Midway down the free band between the workspace above and this one's name.
                let Some(r) = row(index) else { return Vec::new() };
                let pad = self.theme.spacing.xs;
                let above = index.checked_sub(1).and_then(row).map_or(0.0, |a| a.bottom() + pad);
                let centre = f32::midpoint(above, r.y - pad - self.theme.spacing.xl);
                (None, Some(Rect { x: r.x, y: centre - DROP_LINE / 2.0, w: r.w, h: DROP_LINE }))
            }
        };
        let wash = wash.map(|r| hint("drop-wash", r, hsla_alpha(accent, alpha::FAINT)));
        let line = line.map(|r| hint("drop-hint", r, hsla(accent)));
        wash.into_iter().chain(line).collect()
    }

    /// A handle on the divider right of each column of the active workspace, straddling the
    /// line: a drag resizes the column on its left, a double-click puts it back at the width a
    /// column opens at. Its accent line lies over the divider while the pointer is on it and
    /// while it is dragged.
    fn resize_handles(&self, frame: &Frame, cx: &Draw<'_, Self>) -> Vec<gpui::AnyElement> {
        if frame.overview > 0.0 {
            return Vec::new();
        }
        let active = self.layout.active_workspace();
        let dragged = match self.drag {
            Some(Drag::Resize { column, .. }) => Some(column),
            _ => None,
        };
        let accent = hsla(self.theme.surfaces.accent);
        let mut columns: Vec<(usize, Rect)> = Vec::new();
        for p in frame.tiles.iter().filter(|p| p.pos.workspace == active && !p.hidden) {
            match columns.iter_mut().find(|(c, _)| *c == p.pos.column) {
                Some((_, r)) => {
                    let bottom = r.bottom().max(p.rect.bottom());
                    r.y = r.y.min(p.rect.y);
                    r.h = bottom - r.y;
                }
                None => columns.push((p.pos.column, p.rect)),
            }
        }
        columns
            .into_iter()
            .filter(|(_, r)| r.right() > 0.0 && r.right() < self.layout.viewport().0)
            .map(|(column, r)| {
                // The divider is drawn inside the column's right edge; the line covers it.
                let line = div()
                    .debug_selector(move || format!("divider-line-{column}"))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(HANDLE_W / 2.0 - HAIRLINE))
                    .w(px(HAIRLINE));
                let line = if dragged == Some(column) {
                    line.bg(accent)
                } else {
                    line.group_hover(HANDLE_GROUP, move |st| st.bg(accent))
                };
                div()
                    .id(("divider", column))
                    .debug_selector(move || format!("divider-{column}"))
                    .group(HANDLE_GROUP)
                    .absolute()
                    .left(px(r.right() - HANDLE_W / 2.0))
                    .top(px(r.y))
                    .w(px(HANDLE_W))
                    .h(px(r.h))
                    .cursor_ew_resize()
                    .child(line)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                            this.begin_resize(column, ev, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The layout moved on to now for the frame about to be drawn: the clock, the overview's
    /// gaps, and a drag held in an edge band scrolled on. Whether that drag keeps scrolling.
    pub(super) fn advance(&mut self, window: &Window) -> bool {
        self.tick();
        // The overview's gaps hold the names drawn in them, and the blocks' margins either side.
        let spacing = self.theme.spacing;
        self.layout.set_overview_label(2.0_f32.mul_add(spacing.sm, spacing.xl));
        if let Some(Drag::Move { moving: true, .. }) = self.drag {
            // The pointer resting in an edge band keeps the strip scrolling.
            let (x, _) = self.local(window.mouse_position());
            return self.layout.dnd_edge_scroll(x);
        }
        false
    }

    /// Another frame of the strip's motion: before it is drawn, the layout moves on to its
    /// time and the strip's view, `host`, is built again. The workspace is updated but not
    /// notified, which is news only for what is inside the strip's view: a frame of motion
    /// draws nothing else.
    /// One is asked for at a time (`drawn.motion`): a build or a pointer move that asks again
    /// before it runs adds nothing.
    pub(super) fn motion_frame(
        this: WeakEntity<Self>,
        drawn: &Rc<Drawn>,
        host: EntityId,
        window: &Window,
    ) {
        if drawn.motion.replace(true) {
            return;
        }
        let drawn = Rc::clone(drawn);
        window.on_next_frame(move |window, cx| {
            drawn.motion.set(false);
            let edge = this.update(cx, |this, _cx| this.advance(window)).unwrap_or(false);
            cx.notify(host);
            if edge {
                Self::motion_frame(this, &drawn, host, window);
            }
        });
    }

    /// The strip, drawn from the layout's frame at the clock the workspace last moved it to,
    /// by the strip's own view `host`: read here, never written. What the strip drew goes to
    /// [`Drawn`]; what a tile's body takes from its tile (a zoom, a width) goes to it after the
    /// read (`Draw::later`), and only where it changed.
    pub(super) fn render_strip(
        &self,
        host: EntityId,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let frame = self.layout.frame();
        if frame.animating {
            Self::motion_frame(cx.weak_entity(), &self.drawn, host, window);
        }
        let drawn = &self.drawn;
        drawn.builds.set(drawn.builds.get().wrapping_add(1));
        drawn.handed_before.swap(&drawn.handed);
        drawn.handed.borrow_mut().clear();
        if *drawn.strip.borrow() != frame.strip {
            let scrolled = (frame.strip.view.0 - drawn.strip.borrow().view.0).abs() > f32::EPSILON;
            *drawn.strip.borrow_mut() = frame.strip.clone();
            // Not under the self-test, whose frames are still pictures of where things land.
            if scrolled && self.animate && super::marks::thumb(&frame.strip).is_some() {
                self.strip_scrolled(cx);
            }
        }
        let zooming = frame.overview > 0.0 && frame.overview < 1.0;
        // Which of several tiles has the keyboard is said by its title's tone, and under
        // Increase Contrast by a line as well; a lone tile's needs nothing, nor does the
        // overview, which rings its workspace instead.
        let (w, h) = self.layout.viewport();
        let screen = Rect { x: 0.0, y: 0.0, w, h };
        let focus_line = self.theme.contrast == slopty_theme::Contrast::Increased
            && frame.overview <= 0.0
            && frame
                .tiles
                .iter()
                .filter(|p| !p.hidden && p.rect.intersects(&screen))
                .nth(1)
                .is_some();
        let chrome = Chrome { k: frame.zoom, zooming, focus_line };
        drawn.zoom.set(frame.zoom);
        self.track_visibility(&frame, cx);
        let origin = drawn.viewport.get().origin;
        let dragged = match &self.drag {
            Some(Drag::Move { tile, moving: true, .. }) => Some(*tile),
            _ => None,
        };
        let mut placed = Vec::new();
        let mut tiles = Vec::new();
        let mut dividers = Vec::new();
        for p in &frame.tiles {
            if p.hidden || !(p.near || p.focused || dragged == Some(p.tile)) {
                continue;
            }
            if let Some(el) = self.render_tile(p, chrome, window, cx) {
                tiles.push(el);
                dividers.extend(self.render_dividers(p));
                let r = p.rect;
                placed.push((
                    p.tile,
                    Bounds::new(
                        origin + gpui::point(px(r.x), px(r.y)),
                        gpui::size(px(r.w), px(r.h)),
                    ),
                ));
            }
        }
        *drawn.placed.borrow_mut() = placed;
        drawn.focus.set(self.layout.focused());
        let closing: Vec<gpui::AnyElement> =
            frame.closing.iter().filter_map(|c| self.render_closing(c, chrome, cx)).collect();
        let backdrops =
            if frame.overview > 0.0 { self.overview_blocks(&frame, cx) } else { Vec::new() };
        let hint = self.drop_hint(&frame);
        let handles = self.resize_handles(&frame, cx);
        let this = cx.weak_entity();
        let measure = canvas(
            {
                let (this, drawn) = (this.clone(), Rc::clone(drawn));
                move |bounds, _window, cx| Self::strip_measured(&this, &drawn, bounds, cx)
            },
            move |bounds, (), window, _cx| {
                window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Capture || !bounds.contains(&ev.position) {
                        return;
                    }
                    let _gone = this.update(cx, |this, cx| this.scroll_captured(ev, cx));
                });
            },
        )
        .absolute()
        .inset_0();
        // Any workspace with nothing on it is a start page, once the strip has come to rest on
        // it: laid over a workspace still sliding in, it would hide the slide.
        let active = self.layout.active_workspace();
        let bare = self.layout.workspaces().get(active).is_none_or(|w| w.columns().is_empty());
        let empty =
            (bare && frame.overview <= 0.0 && !frame.animating).then(|| self.render_empty(cx));
        let strip = div()
            .id("strip")
            .debug_selector(|| "strip".to_owned())
            .relative()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .capture_pinch(cx.listener(Self::pinch))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(measure)
            .children(backdrops)
            .children(closing)
            .children(tiles)
            .children(dividers)
            .children(handles)
            .children(hint)
            .children(empty)
            .children(self.render_marks());
        strip.into_any_element()
    }

    /// The strip laid out at `bounds`: kept for the handlers, and a new size is the layout's,
    /// once this frame is over.
    fn strip_measured(
        this: &WeakEntity<Self>,
        drawn: &Drawn,
        bounds: Bounds<Pixels>,
        cx: &mut App,
    ) {
        let was = drawn.viewport.replace(bounds);
        if was.size != bounds.size {
            let this = this.clone();
            cx.defer(move |cx| {
                let _gone = this.update(cx, |this, cx| this.strip_resized(bounds.size, cx));
            });
        }
    }

    /// The overview's blocks, under the tiles: each workspace with tiles is one block on the
    /// content's surface holding its panes flush, a base unit wider all round so its corners
    /// (`radii.lg`, the radius of what floats) clear theirs, with a hairline. Only the active
    /// one floats: it takes the one elevation and a 1.5 pt accent edge flush with it, as a
    /// selected thumbnail has. A shadow under every block would say they all float. Each name
    /// sits above its block at the medium weight, its count in the meta size. The empty
    /// workspace kept at the end is where the next one goes: a ghost "New workspace" button
    /// under the last block, on its left edge, which opens it.
    fn overview_blocks(&self, frame: &Frame, cx: &Draw<'_, Self>) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        // A base unit, so the panes' square corners sit well inside the block's round ones.
        let pad = theme.spacing.sm;
        let workspaces = self.layout.workspaces();
        let active = self.layout.active_workspace();
        let fade = frame.overview;
        let (opening, moves) = (self.layout.overview_open(), self.chrome_moves(cx));
        // The words start on the panes' glyphs' edge: a miniature's label pads its glyph by `pad`.
        let glyph_slot = theme.typography.icon_large();
        let mut left = None;
        let mut out = Vec::new();
        for (ix, r) in frame.workspaces.iter().map(|(ix, r)| (*ix, *r)) {
            let tiles: usize = workspaces
                .get(ix)
                .map_or(0, |ws| ws.columns().iter().map(|c| c.tiles().len()).sum());
            // The labels are chrome: drawn at the type scale whatever the zoom, so a name
            // stays readable however many workspaces the overview fits.
            let label_top = r.y - pad - theme.spacing.xl;
            if tiles == 0 {
                let x = left.unwrap_or(r.x);
                let muted = hsla(s.text_muted);
                let new = div()
                    .id("overview-new-workspace")
                    .debug_selector(|| "overview-new-workspace".to_owned())
                    .role(gpui::accesskit::Role::Button)
                    .aria_label(NEW_WORKSPACE)
                    .absolute()
                    // Its glyph where the panes' glyphs and the names above the blocks start.
                    .left(px(x))
                    .top(px(r.y - pad))
                    .h(px(theme.density.row))
                    .px(px(pad))
                    .rounded(px(theme.radii.sm))
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .active(move |el| el.bg(hsla(s.overlay)))
                    .text_size(px(theme.typography.ui_size))
                    .font_family(theme.typography.ui_family.clone())
                    .text_color(hsla(s.text_secondary))
                    .child(
                        div().size(px(glyph_slot)).flex().items_center().justify_center().child(
                            crate::icons::icon(
                                theme,
                                IconName::Plus,
                                crate::icons::IconSize::Inline,
                                muted,
                            )
                            .size(px(theme.typography.icon())),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "overview-new-workspace-words".to_owned())
                            .child(NEW_WORKSPACE),
                    );
                let new = crate::a11y::tab_stop(new, s.accent).on_click(cx.listener(
                    move |this, _ev, _w, cx| {
                        this.layout.set_overview(false);
                        this.go_to_workspace(ix, cx);
                    },
                ));
                // Its own wrapper, so the button keeps its placement while the words fade.
                let new = div().absolute().inset_0().child(new);
                out.extend(overview_words(new, "overview-new-workspace-in", opening, moves));
                continue;
            }
            left = Some(r.x);
            let here = ix == active;
            let block = div()
                .debug_selector(move || format!("overview-block-{ix}"))
                .absolute()
                .left(px(r.x - pad))
                .top(px(r.y - pad))
                .w(px(2.0_f32.mul_add(pad, r.w)))
                .h(px(2.0_f32.mul_add(pad, r.h)))
                .rounded(px(theme.radii.lg))
                .opacity(fade)
                .map(|el| {
                    if here {
                        // Where you are: an edge of the full accent flush with the block, as a
                        // selected thumbnail has it. The keyboard's ring outside a gap read as
                        // focus round a block, not as the block chosen.
                        let ring = gpui::Outline {
                            color: hsla(s.accent),
                            width: px(OVERVIEW_EDGE),
                            offset: px(0.0),
                        };
                        kit::elevate(el, theme).outline_ring(ring)
                    } else {
                        el.border_1().border_color(hsla(s.border))
                    }
                })
                // The panes' own surface, lifted or not: the block is what they sit on.
                .bg(hsla(theme.content()));
            let ink = if here { s.text } else { s.text_secondary };
            let count = if tiles == 1 { "1 tile".to_owned() } else { format!("{tiles} tiles") };
            let label = div()
                .absolute()
                .left(px(r.x + pad))
                .top(px(label_top))
                .w(px(r.w - pad))
                .h(px(theme.spacing.xl))
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm))
                .overflow_hidden()
                .whitespace_nowrap()
                .font_family(theme.typography.ui_family.clone())
                .child(
                    div()
                        .debug_selector(move || format!("overview-name-{ix}"))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(theme.typography.ui_size))
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(ink))
                        .child(SharedString::from(self.workspace_name_at(ix))),
                )
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .text_size(px(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(count)),
                )
                .child(super::rollup::rollup_slot(
                    theme,
                    format!("overview-rollup-{ix}"),
                    self.workspace_rollup(ix).0,
                    true,
                ));
            out.push(block.into_any_element());
            let words = SharedString::from(format!("overview-words-{ix}"));
            out.extend(overview_words(label, words, opening, moves));
            out.extend(self.overview_more(ix, r, pad, cx));
        }
        out
    }

    /// Where a workspace's block runs past the window's edge in the overview, a button on
    /// that edge says there is more and steps its columns into view, as the strip's swipe
    /// does: a strip of nine tiles was four miniatures cut off at the edge, with nothing to
    /// say the rest was there.
    fn overview_more(
        &self,
        ix: usize,
        r: Rect,
        pad: f32,
        cx: &Draw<'_, Self>,
    ) -> Vec<gpui::AnyElement> {
        if !self.layout.overview_open() {
            return Vec::new();
        }
        let theme = &self.theme;
        let width = self.layout.viewport().0;
        let side = kit::icon_button_side(theme);
        let top = r.y + (r.h - side) / 2.0;
        let edge = theme.spacing.sm;
        let mut out = Vec::new();
        for (right, past) in [(false, r.x - pad < 0.0), (true, r.right() + pad > width)] {
            if !past {
                continue;
            }
            let (icon, words, id) = if right {
                (
                    IconName::ChevronRight,
                    "Show the next columns",
                    format!("overview-more-right-{ix}"),
                )
            } else {
                (
                    IconName::ChevronLeft,
                    "Show the columns before",
                    format!("overview-more-left-{ix}"),
                )
            };
            let button = kit::elevate(kit::icon_button_at(theme, id, icon, words, 1.0), theme)
                .rounded(px(theme.radii.full))
                .absolute()
                .top(px(top))
                .map(|el| if right { el.right(px(edge)) } else { el.left(px(edge)) })
                .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    if this.layout.active_workspace() != ix {
                        this.layout.focus_workspace(ix);
                    }
                    if right {
                        this.layout.focus_column_right();
                    } else {
                        this.layout.focus_column_left();
                    }
                    this.after_focus_moved(cx);
                    this.layout_touched(cx);
                    cx.notify();
                }));
            out.push(button.into_any_element());
        }
        out
    }

    /// An empty workspace: a start page composed as a list, never a centred picture. It hangs
    /// a fifth of the way down, where the palette opens, on one left edge: what this is, then
    /// the three ways to begin, the first shown as the palette shows the row Enter would run,
    /// each with its keys in the palette's plain muted glyphs where there is a keyboard to
    /// press them on; then where shells already stand on the workers ([`Self::recent_places`]),
    /// each opening another shell there; then the workers, each marked only where its link is
    /// not up, each opening a shell on itself here. With no worker there is nothing to open,
    /// and the page says where one comes from and offers the app's way to add one.
    fn render_empty(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let muted = hsla(s.text_muted);
        let title = |text: &'static str| {
            kit::inset_x(div(), theme)
                .pb(px(spacing.xs))
                .text_size(px(theme.typography.title()))
                .font_weight(FontWeight(Typography::STRONG_WEIGHT))
                .text_color(hsla(s.text))
                .child(text)
        };
        // A section: its quiet label, then its rows; a section apart from the one above by
        // space alone, no rule.
        let section = |id: &'static str, label: &'static str| {
            div().w_full().pt(px(spacing.lg)).flex().flex_col().child(
                kit::inset_x(kit::label(theme, label), theme)
                    .debug_selector(move || id.to_owned())
                    .pb(px(spacing.xs)),
            )
        };
        let column = div().w_full().max_w(px(EMPTY_W)).flex().flex_col();
        let column = if self.workers.is_empty() {
            let add = self.add_worker_run().map(|run| {
                div().w_full().pt(px(spacing.md)).child(
                    self.begin_row("empty-add-worker", IconName::Plus, ADD_WORKER, "", true)
                        .on_click(move |_ev, window, cx| run(window, cx)),
                )
            });
            column
                .child(title(NO_WORKERS))
                .child(kit::inset_x(div(), theme).text_color(muted).child(NO_WORKERS_NEXT))
                .children(add)
        } else {
            let [terminal, window] = &begin_keys();
            // The question leads; a shell and a window are the quieter ways in under it.
            let begin = div()
                .w_full()
                .pt(px(spacing.sm))
                .flex()
                .flex_col()
                .child(
                    self.begin_row(
                        "empty-terminal",
                        IconName::SquareTerminal,
                        NEW_TERMINAL,
                        terminal,
                        false,
                    )
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.new_terminal(&super::actions::NewTerminal, window, cx);
                    })),
                )
                .child(
                    self.begin_row("empty-window", IconName::AppWindow, ADD_WINDOW, window, false)
                        .on_click(cx.listener(|this, _ev, window, cx| {
                            this.add_window(&super::actions::AddWindow, window, cx);
                        })),
                );
            let several = self.workers.len() > 1;
            let places: Vec<gpui::AnyElement> = self
                .recent_places()
                .into_iter()
                .take(RECENT_PLACES)
                .enumerate()
                .map(|(ix, place)| {
                    let worker = self.worker_name(place.worker);
                    let meta = super::rollup::meta_line([
                        place.branch.as_deref(),
                        several.then_some(worker.as_str()),
                    ]);
                    let label = if several {
                        format!("New terminal in {} on {worker}", place.name)
                    } else {
                        format!("New terminal in {}", place.name)
                    };
                    let (key, cwd) = (place.worker, place.cwd);
                    self.place_row(("empty-place", ix), IconName::Folder, place.name, meta)
                        .debug_selector(move || format!("empty-place-{ix}"))
                        .aria_label(SharedString::from(label))
                        .on_click(cx.listener(move |this, _ev, _window, cx| {
                            this.open_session_on(key, Some(cwd.clone()), Vec::new(), None, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            let workers = self.workers.iter().enumerate().map(|(ix, (key, w))| {
                let key = *key;
                let health = super::navigator::worker_health(&w.status);
                let label = match health {
                    Some((_, word)) => format!("New terminal on {}, {word}", w.name),
                    None => format!("New terminal on {}", w.name),
                };
                let row = kit::row(theme, kit::Row::One)
                    .id(("empty-worker", ix))
                    .debug_selector(move || format!("empty-worker-{ix}"))
                    .role(gpui::accesskit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .w_full()
                    .rounded(px(theme.radii.sm))
                    .cursor_pointer()
                    .hover(|st| st.bg(hsla(s.raised)))
                    .active(|st| st.bg(hsla(s.overlay)))
                    .child(crate::palette::icon_slot(theme, IconName::Server, muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(w.name.clone())),
                    )
                    .children(health.map(|(_, word)| {
                        div()
                            .flex_none()
                            .text_size(px(theme.typography.small()))
                            .text_color(muted)
                            .child(word)
                    }))
                    .child(crate::icons::status_mark(theme, health.map(|(mark, _)| mark), 1.0));
                crate::a11y::tab_stop(row, s.accent)
                    .on_click(
                        cx.listener(move |this, _ev, _window, cx| this.new_terminal_on(key, cx)),
                    )
                    .into_any_element()
            });
            let recent =
                (!places.is_empty()).then(|| section("empty-recent", RECENT).children(places));
            column
                .children(self.render_ask(cx))
                .child(begin)
                .children(recent)
                .child(section("empty-workers", "Workers").children(workers))
        };
        // The strip is the content step with or without a tile on it: the empty workspace is
        // the page a tile would be, not a hole down to the bars' `canvas`. It starts a fifth of
        // the way down, where a dialog sits, by this frame's layout: the strip's size as the
        // last frame measured it is a frame behind chrome that comes or goes.
        div()
            .id("empty-workspace")
            .absolute()
            .inset_0()
            .overflow_y_scroll()
            .bg(hsla(theme.content()))
            .flex()
            .flex_col()
            .items_center()
            .px(px(spacing.md))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .child(div().flex_none().w_full().h(gpui::relative(kit::MODAL_ANCHOR)))
            // Slopty's mark, centred over the page's words: its cursor lit while a worker is
            // reachable ([`super::about::Mark`]).
            .child(div().flex_none().pb(px(spacing.xl)).child(self.empty_mark.clone()))
            .child(column.pb(px(spacing.xl)))
            .into_any_element()
    }

    /// Where shells stand across the workers that are up, one entry per directory on each: the
    /// most recently used tile's first (the tile recency), then the latest started. What the
    /// empty workspace offers as a way back to the work in progress.
    pub(super) fn recent_places(&self) -> Vec<RecentPlace> {
        let rank = |item: ItemId| self.recency.iter().rposition(|i| *i == item);
        let mut places: Vec<(Option<usize>, u64, RecentPlace)> = Vec::new();
        for (key, w) in self.workers.iter().filter(|(_, w)| w.link.is_some()) {
            let home = w.home.as_deref();
            let items: Vec<(SessionId, ItemId)> = w
                .doc
                .items()
                .filter_map(|i| match i.kind {
                    ItemKind::Terminal { session } => Some((session, i.id)),
                    _ => None,
                })
                .collect();
            for summary in w.sessions.values() {
                let Some(cwd) = summary.cwd.clone() else { continue };
                if places.iter().any(|(.., p)| p.worker == *key && p.cwd == cwd) {
                    continue;
                }
                let item = items.iter().find(|(s, _)| *s == summary.id).map(|(_, i)| *i);
                let place = RecentPlace {
                    worker: *key,
                    name: super::tile::repo_place(&cwd, summary.repo.as_deref(), home),
                    branch: summary.branch.clone(),
                    cwd,
                };
                places.push((item.and_then(rank), summary.started_ms.as_millis(), place));
            }
        }
        places.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        places.into_iter().map(|(.., place)| place).collect()
    }

    /// A row of the empty workspace that goes somewhere: its glyph, its name and, after it,
    /// its facts in the meta size.
    fn place_row(
        &self,
        id: impl Into<gpui::ElementId>,
        icon: IconName,
        name: String,
        meta: String,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let row = kit::row(theme, kit::Row::One)
            .id(id)
            .role(gpui::accesskit::Role::Button)
            .w_full()
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(|st| st.bg(hsla(s.raised)))
            .active(|st| st.bg(hsla(s.overlay)))
            .child(crate::palette::icon_slot(theme, icon, hsla(s.text_muted)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(name)),
            )
            .when(!meta.is_empty(), |el| {
                el.child(
                    kit::meta(div(), theme)
                        .flex_none()
                        .max_w(px(EMPTY_W / 2.0))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(meta)),
                )
            });
        crate::a11y::tab_stop(row, s.accent)
    }

    /// The worker a way to begin opens on: the one "+" chose, else the one in context.
    fn begin_target(&self) -> Option<String> {
        let chosen = self.new_on.filter(|k| self.workers.contains_key(k));
        let key = chosen.or_else(|| self.context_worker())?;
        self.workers.get(&key).map(|w| w.name.clone())
    }

    /// One way to begin: its icon, what it does, where it opens and, with a keyboard to press
    /// it on, its keys; `primary` wears the palette's selected fill, the row Enter would run
    /// there.
    fn begin_row(
        &self,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        keys: &str,
        primary: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let icon_ink = if primary { s.text } else { s.text_muted };
        let row = kit::row(theme, kit::Row::One)
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(gpui::accesskit::Role::Button)
            .aria_label(label)
            .w_full()
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(primary, |el| el.bg(hsla(s.overlay)))
            .when(!primary, |el| el.hover(|st| st.bg(hsla(s.raised))))
            .active(|st| st.bg(hsla(s.overlay)))
            .child(crate::palette::icon_slot(theme, icon, hsla(icon_ink)))
            .child(div().flex_none().text_color(hsla(s.text)).child(label))
            // Where it opens, as its meta: the worker "+" chose, else the one in context.
            .children(self.begin_target().map(|worker| {
                div()
                    .debug_selector(move || format!("{id}-target"))
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!("on {worker}")))
            }))
            .child(div().flex_1())
            // A chord is only worth printing where there are keys to press it on.
            .when(self.hardware_keyboard, |el| {
                el.child(
                    div()
                        .debug_selector(move || format!("{id}-keys"))
                        .flex_none()
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(crate::palette::drawn_keys(keys))),
                )
            });
        crate::a11y::tab_stop(row, s.accent)
    }
}

/// The active overview block's accent edge, in points.
const OVERVIEW_EDGE: f32 = 1.5;

/// How wide the empty workspace's column stands: room for a directory beside its branch and
/// worker, narrow enough to read as one block down the strip.
const EMPTY_W: f32 = 400.0;

/// How many directories the empty workspace offers.
const RECENT_PLACES: usize = 5;

/// The empty workspace's section of directories shells stand in.
pub(super) const RECENT: &str = "Recent";

/// A directory a shell stands in on a worker: where the empty workspace offers another shell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct RecentPlace {
    pub worker: WorkerKey,
    /// Its full path on the worker, where the new shell starts.
    pub cwd: String,
    /// What it is called: the repository and the path within it, else the path's tail.
    pub name: String,
    pub branch: Option<String>,
}

/// How long the keyed overview takes to land: the layout's critically damped spring, read to
/// where it stops moving to the eye.
const OVERVIEW_LANDS: std::time::Duration = std::time::Duration::from_millis(280);

/// The overview's words (names, counts, "New workspace") fading in over the last
/// [`kit::Pace::Fade`] of the zoom, drawn at the type scale once the blocks have all but landed,
/// so no text is ever seen scaling. `el` is drawn at once where chrome does not move, and not
/// at all while the overview closes: words over a zoom on its way in read as debris.
pub(super) fn overview_words(
    el: gpui::Div,
    id: impl Into<gpui::ElementId>,
    opening: bool,
    moves: bool,
) -> Option<gpui::AnyElement> {
    if !opening {
        return None;
    }
    if !moves {
        return Some(el.into_any_element());
    }
    let lands = OVERVIEW_LANDS.as_secs_f32();
    let wait = (lands - kit::Pace::Fade.duration().as_secs_f32()) / lands;
    let curve = kit::Pace::Fade.curve();
    Some(
        el.with_animation(id, Animation::new(OVERVIEW_LANDS), move |el, t| {
            el.opacity(curve.at(((t - wait) / (1.0 - wait)).clamp(0.0, 1.0)))
        })
        .into_any_element(),
    )
}

/// What the empty workspace says with no worker to begin on.
pub(crate) const NO_WORKERS: &str = "No workers yet";
pub(crate) const NO_WORKERS_NEXT: &str = "A worker runs your shells, agents and windows.";
/// The empty workspace's way to a first worker, as the "…" menu words it.
pub(crate) const ADD_WORKER: &str = "Add a worker";
const NEW_TERMINAL: &str = "New terminal";
const ADD_WINDOW: &str = "Add a window or display";
/// The overview's place for a new workspace.
pub(crate) const NEW_WORKSPACE: &str = "New workspace";

/// The keys of the two quieter ways to begin, read from the keymap in effect so a rebinding
/// shows at once. The keymap words its chords as it is installed, so a frame only looks them up.
pub(super) fn begin_keys() -> [String; 2] {
    let keymap = crate::keymap::current();
    [
        keymap.label_of(&super::actions::NewTerminal).to_owned(),
        keymap.label_of(&super::actions::AddWindow).to_owned(),
    ]
}
