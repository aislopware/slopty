//! The strip: the tiles drawn where the layout's frame puts them, and the pointer and the
//! gestures that move it.
//!
//! A horizontal two-finger swipe (or a finger's pan on a phone) drags the strip, snapping to
//! a column when it ends, with the fling the swipe tracker measured; a vertical one scrolls
//! what is under it, or, over the bare strip, switches workspace. The axis is decided after
//! 16 points and kept for the gesture. The momentum macOS sends after the fingers lift is
//! swallowed after a strip or workspace gesture, which has already snapped. A focused remote
//! window takes every swipe over its picture (on a phone, every remote picture does). ⌘⌥ and
//! the wheel steps columns and workspaces. A pinch in opens the overview; out closes it.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, Context, DispatchPhase, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PinchEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, Styled as _, TouchPhase, Window, canvas, div, px,
};
use slopty_client::layout::{Axis, AxisLock, DropTarget, Frame, Rect, TileRef, WHEEL_TICK};
use slopty_core::ItemId;
use slopty_proto::items::ItemKind;
use slopty_theme::alpha;

use super::WorkspaceView;
use super::tile::{Chrome, HEADER_H};
use crate::colors::{hsla, hsla_alpha};

/// How far a header press travels before it is a move rather than a click.
const DRAG_SLOP: f32 = 4.0;

/// How much pinch it takes to open or close the overview.
const PINCH_STEP: f32 = 0.15;

/// The pointer in progress over the strip.
pub(super) enum Drag {
    /// A header pressed: a move once it travels [`DRAG_SLOP`].
    Move { tile: TileRef, grab: Point<Pixels>, moving: bool, target: Option<DropTarget> },
    /// The gap right of a column pressed.
    Resize {
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

impl WorkspaceView {
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
        self.tick();
        let before = self.column_sizes();
        if self.layout.resize_begin(column) {
            self.drag = Some(Drag::Resize { grab: ev.position, before });
            cx.notify();
        }
        cx.stop_propagation();
    }

    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        let d = p - self.viewport.origin;
        (f32::from(d.x), f32::from(d.y))
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
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
    fn under(&self, p: Point<Pixels>) -> Option<(TileRef, bool)> {
        self.placed.iter().rev().find(|(_, b)| b.contains(&p)).map(|(tile, b)| {
            let body = p.y > b.origin.y + px(HEADER_H * self.drawn_zoom);
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

    /// Every scroll over the strip, before the tiles see it (see the module docs).
    fn scroll_captured(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let (dx, dy) = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * WHEEL_TICK, l.y * WHEEL_TICK),
        };
        self.tick();
        let now = self.epoch.elapsed();
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
            Phase::Idle => {
                if self.gesture.swallow_coast {
                    cx.stop_propagation();
                }
            }
            Phase::Content => {}
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
        }
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

    /// The strip's area was measured: the layout lays out for it.
    fn measured(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.viewport.size != bounds.size {
            self.layout.set_viewport(f32::from(bounds.size.width), f32::from(bounds.size.height));
            // This frame was laid out for the old size; one more draws the new one. A notify
            // while drawing only marks the view, so it goes through a deferred effect.
            let this = cx.weak_entity();
            cx.defer(move |cx| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |_, cx| cx.notify());
                }
            });
        }
        self.viewport = bounds;
    }

    /// Remote tiles off screen are counted; after the grace their streams go. A timer
    /// comes back for them once the grace is up, since an idle strip draws no frames.
    fn track_visibility(&mut self, frame: &Frame, cx: &Context<Self>) {
        let (w, h) = self.layout.viewport();
        let screen = Rect { x: 0.0, y: 0.0, w, h };
        let visible: Vec<ItemId> = frame
            .tiles
            .iter()
            .filter(|p| !p.hidden && p.rect.intersects(&screen))
            .map(|p| p.tile.item)
            .collect();
        self.note_visible(&visible);
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

    /// Where a drop would land, drawn as a bar between columns or a wash over a column.
    fn drop_hint(&self, frame: &Frame) -> Option<gpui::AnyElement> {
        let Some(Drag::Move { moving: true, target: Some(target), .. }) = &self.drag else {
            return None;
        };
        let gaps = self.layout.config().gaps * frame.zoom;
        let column = |ws: usize, col: usize| -> Option<Rect> {
            let rects: Vec<Rect> = frame
                .tiles
                .iter()
                .filter(|p| p.pos.workspace == ws && p.pos.column == col && !p.hidden)
                .map(|p| p.rect)
                .collect();
            let first = rects.first()?;
            let (x, y) = (first.x, rects.iter().map(|r| r.y).fold(f32::MAX, f32::min));
            let bottom = rects.iter().map(Rect::bottom).fold(f32::MIN, f32::max);
            Some(Rect { x, y, w: first.w, h: bottom - y })
        };
        let rect = match *target {
            DropTarget::IntoColumn { workspace, column: col, .. } => column(workspace, col)?,
            DropTarget::NewColumn { workspace, index } => {
                // A bar centred in the gap the new column opens.
                let bar = |centre: f32, r: Rect| Rect { x: centre - 1.5, y: r.y, w: 3.0, h: r.h };
                let before = index.checked_sub(1).and_then(|i| column(workspace, i));
                match (column(workspace, index), before) {
                    (Some(r), _) => bar(r.x - gaps / 2.0, r),
                    (None, Some(r)) => bar(r.right() + gaps / 2.0, r),
                    (None, None) => frame.workspaces.iter().find(|(i, _)| *i == workspace)?.1,
                }
            }
            DropTarget::NewWorkspace { index } => {
                let r = frame.workspaces.iter().find(|(i, _)| *i == index).map(|(_, r)| *r)?;
                Rect { x: r.x, y: r.y - gaps, w: r.w, h: 3.0 }
            }
        };
        let theme = &self.theme;
        Some(
            div()
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w))
                .h(px(rect.h))
                .rounded(px(theme.radii.md))
                .bg(hsla_alpha(theme.surfaces.accent, alpha::FAINT))
                .border_1()
                .border_color(hsla(theme.surfaces.accent))
                .into_any_element(),
        )
    }

    /// The handles on the gaps right of each column of the active workspace: a drag resizes
    /// the column on its left.
    fn resize_handles(&self, frame: &Frame, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        if frame.overview > 0.0 {
            return Vec::new();
        }
        let active = self.layout.active_workspace();
        let gaps = self.layout.config().gaps;
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
                div()
                    .id(("gap", column))
                    .debug_selector(move || format!("gap-{column}"))
                    .absolute()
                    .left(px(r.right()))
                    .top(px(r.y))
                    .w(px(gaps))
                    .h(px(r.h))
                    .cursor_ew_resize()
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

    /// The strip, drawn from the layout's frame at the clock.
    pub(super) fn render_strip(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.tick();
        if let Some(Drag::Move { moving: true, .. }) = self.drag {
            // The pointer resting in an edge band keeps the strip scrolling.
            let (x, _) = self.local(window.mouse_position());
            if self.layout.dnd_edge_scroll(x) {
                window.request_animation_frame();
            }
        }
        let frame = self.layout.frame();
        if frame.animating {
            window.request_animation_frame();
        }
        let zooming = frame.overview > 0.0 && frame.overview < 1.0;
        let chrome = Chrome { k: frame.zoom, zooming };
        self.drawn_zoom = frame.zoom;
        self.track_visibility(&frame, cx);
        let origin = self.viewport.origin;
        let dragged = match &self.drag {
            Some(Drag::Move { tile, moving: true, .. }) => Some(*tile),
            _ => None,
        };
        let mut placed = Vec::new();
        let mut tiles = Vec::new();
        for p in &frame.tiles {
            if p.hidden || !(p.near || p.focused || dragged == Some(p.tile)) {
                continue;
            }
            if let Some(el) = self.render_tile(p, chrome, window, cx) {
                tiles.push(el);
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
        self.placed = placed;
        self.drawn_focus = self.layout.focused();
        let closing: Vec<gpui::AnyElement> = frame
            .closing
            .iter()
            .map(|c| self.render_closing(c.rect, c.alpha, c.scale, frame.zoom))
            .collect();
        let theme = &self.theme;
        // In the overview each workspace is a panel the tiles sit on.
        let backdrops: Vec<gpui::AnyElement> = if frame.overview > 0.0 {
            frame
                .workspaces
                .iter()
                .map(|(_, r)| {
                    div()
                        .absolute()
                        .left(px(r.x))
                        .top(px(r.y))
                        .w(px(r.w))
                        .h(px(r.h))
                        .rounded(px(theme.radii.md))
                        .bg(hsla_alpha(theme.surfaces.panel, frame.overview * alpha::VEIL))
                        .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        let hint = self.drop_hint(&frame);
        let handles = self.resize_handles(&frame, cx);
        let entity = cx.entity();
        let measure = canvas(
            {
                let entity = entity.clone();
                move |bounds, _window, cx| entity.update(cx, |this, cx| this.measured(bounds, cx))
            },
            move |bounds, (), window, _cx| {
                window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, _window, cx| {
                    if phase != DispatchPhase::Capture || !bounds.contains(&ev.position) {
                        return;
                    }
                    entity.update(cx, |this, cx| this.scroll_captured(ev, cx));
                });
            },
        )
        .absolute()
        .inset_0();
        let empty = self.layout.tiles().next().is_none();
        let hint_text = if self.workers.is_empty() {
            "No workers yet: add one from the … menu"
        } else {
            "⌘T opens a shell · ⌘O adds a window"
        };
        div()
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
            .children(handles)
            .children(hint)
            .when(empty, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(theme.typography.ui_size))
                        .text_color(hsla(theme.surfaces.text_muted))
                        .font_family(theme.typography.ui_family.clone())
                        .child(hint_text),
                )
            })
            .into_any_element()
    }
}
