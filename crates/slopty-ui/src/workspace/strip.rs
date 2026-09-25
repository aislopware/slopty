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

use gpui::{
    Bounds, Context, DispatchPhase, FontWeight, InteractiveElement as _, IntoElement as _,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PinchEvent,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _,
    Styled as _, TouchPhase, Window, canvas, div, px,
};
use slopty_client::layout::{Axis, AxisLock, DropTarget, Frame, Rect, TileRef, WHEEL_TICK};
use slopty_core::ItemId;
use slopty_proto::items::ItemKind;
use slopty_theme::{Typography, alpha};

use super::WorkspaceView;
use super::tile::{Chrome, HEADER_H};
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize};

/// How far a header press travels before it is a move rather than a click.
const DRAG_SLOP: f32 = 4.0;

/// How much pinch it takes to open or close the overview.
const PINCH_STEP: f32 = 0.15;

/// The resize handle's width, centred on the divider it drags so either side of the line
/// takes the press.
const HANDLE_W: f32 = 6.0;

/// One hairline: the divider a tile draws inside its right edge, and the accent line a
/// handle lays over it.
const HAIRLINE: f32 = 1.0;

/// The line that marks where a drop opens a new column or workspace.
const DROP_LINE: f32 = 2.0;

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
            self.drag = Some(Drag::Resize { column, grab: ev.position, before });
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

    /// Where a drop would land: an accent line on the divider a new column or workspace would
    /// open, or a faint accent wash over the column it would join. Neither has a corner or a
    /// frame, since the panes they mark have none.
    fn drop_hint(&self, frame: &Frame) -> Option<gpui::AnyElement> {
        let Some(Drag::Move { moving: true, target: Some(target), .. }) = &self.drag else {
            return None;
        };
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
        let row = |ix: usize| frame.workspaces.iter().find(|(i, _)| *i == ix).map(|(_, r)| *r);
        let accent = self.theme.surfaces.accent;
        let (rect, ink) = match *target {
            DropTarget::IntoColumn { workspace, column: col, .. } => {
                (column(workspace, col)?, hsla_alpha(accent, alpha::FAINT))
            }
            DropTarget::NewColumn { workspace, index } => {
                // The line straddles the divider the new column opens.
                let line =
                    |x: f32, r: Rect| Rect { x: x - DROP_LINE / 2.0, y: r.y, w: DROP_LINE, h: r.h };
                let before = index.checked_sub(1).and_then(|i| column(workspace, i));
                match (column(workspace, index), before) {
                    (Some(r), _) => (line(r.x, r), hsla(accent)),
                    (None, Some(r)) => (line(r.right(), r), hsla(accent)),
                    // An empty workspace: the whole of it is the new column.
                    (None, None) => (row(workspace)?, hsla_alpha(accent, alpha::FAINT)),
                }
            }
            DropTarget::NewWorkspace { index } => {
                // Midway down the free band between the workspace above and this one's name.
                let r = row(index)?;
                let above = index.checked_sub(1).and_then(row).map_or(0.0, |a| a.bottom());
                let centre = f32::midpoint(above, r.y - self.theme.spacing.xl);
                let rect = Rect { x: r.x, y: centre - DROP_LINE / 2.0, w: r.w, h: DROP_LINE };
                (rect, hsla(accent))
            }
        };
        Some(
            div()
                .debug_selector(|| "drop-hint".to_owned())
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w))
                .h(px(rect.h))
                .bg(ink)
                .into_any_element(),
        )
    }

    /// A handle on the divider right of each column of the active workspace, straddling the
    /// line: a drag resizes the column on its left. Its accent line lies over the divider while
    /// the pointer is on it and while it is dragged.
    fn resize_handles(&self, frame: &Frame, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
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

    /// The strip, drawn from the layout's frame at the clock.
    pub(super) fn render_strip(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.tick();
        // The overview's gaps hold the names drawn in them.
        self.layout.set_overview_label(self.theme.spacing.xl);
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
        self.placed = placed;
        self.drawn_focus = self.layout.focused();
        let closing: Vec<gpui::AnyElement> =
            frame.closing.iter().map(|c| self.render_closing(c.rect, c.alpha, c.scale)).collect();
        let theme = &self.theme;
        // In the overview each workspace is one flush block of its panes in a single hairline
        // frame, laid just outside the block so no pane covers it, with its name above; an
        // empty one (the one kept at the end for what comes next) is only its dashed outline,
        // a place and not a blank slab, with a plus and its name inside to say what dropping
        // there does. Nothing has a corner: the panes it holds have none.
        let backdrops: Vec<gpui::AnyElement> = if frame.overview > 0.0 {
            let workspaces = self.layout.workspaces();
            let active = self.layout.active_workspace();
            let s = &theme.surfaces;
            let fade = frame.overview;
            frame
                .workspaces
                .iter()
                .flat_map(|(ix, r)| {
                    let ix = *ix;
                    let tiles: usize = workspaces
                        .get(ix)
                        .map_or(0, |ws| ws.columns().iter().map(|c| c.tiles().len()).sum());
                    // The labels are chrome: drawn at the type scale whatever the zoom, so a
                    // name stays readable however many workspaces the overview fits.
                    if tiles == 0 {
                        let muted = hsla_alpha(s.text_muted, fade);
                        let zone = div()
                            .debug_selector(move || format!("overview-block-{ix}"))
                            .absolute()
                            .left(px(r.x))
                            .top(px(r.y))
                            .w(px(r.w))
                            .h(px(r.h))
                            .border_1()
                            .border_dashed()
                            .border_color(hsla_alpha(s.text_muted, fade * alpha::TINT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(theme.spacing.xs))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(theme.typography.small()))
                            .font_family(theme.typography.ui_family.clone())
                            .text_color(muted)
                            .child(crate::icons::icon(
                                theme,
                                IconName::Plus,
                                IconSize::Inline,
                                muted,
                            ))
                            .child(
                                div()
                                    .debug_selector(move || format!("overview-name-{ix}"))
                                    .child(NEW_WORKSPACE),
                            );
                        vec![zone.into_any_element()]
                    } else {
                        let ink = if ix == active { s.text } else { s.text_secondary };
                        let count =
                            if tiles == 1 { "1 tile".to_owned() } else { format!("{tiles} tiles") };
                        let block = div()
                            .debug_selector(move || format!("overview-block-{ix}"))
                            .absolute()
                            .left(px(r.x - HAIRLINE))
                            .top(px(r.y - HAIRLINE))
                            .w(px(2.0_f32.mul_add(HAIRLINE, r.w)))
                            .h(px(2.0_f32.mul_add(HAIRLINE, r.h)))
                            .border_1()
                            .border_color(hsla_alpha(s.border, fade))
                            .into_any_element();
                        let label = div()
                            .absolute()
                            .left(px(r.x))
                            .top(px(r.y - theme.spacing.xl))
                            .w(px(r.w))
                            .h(px(theme.spacing.xl))
                            .flex()
                            .items_center()
                            .gap(px(theme.spacing.sm))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_size(px(theme.typography.small()))
                            .font_family(theme.typography.ui_family.clone())
                            .child(
                                div()
                                    .debug_selector(move || format!("overview-name-{ix}"))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .font_weight(FontWeight(Typography::STRONG_WEIGHT))
                                    .text_color(hsla_alpha(ink, fade))
                                    .child(SharedString::from(self.workspace_name_at(ix))),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(hsla_alpha(s.text_muted, fade))
                                    .child(SharedString::from(count)),
                            );
                        vec![block, label.into_any_element()]
                    }
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
        let empty = self.layout.tiles().next().is_none().then(|| self.render_empty(cx));
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
            .children(dividers)
            .children(handles)
            .children(hint)
            .children(empty)
            .into_any_element()
    }

    /// An empty workspace: a large muted mark and what this is, then the three ways to begin,
    /// each with its key cap (a key cap teaches the chord where the chord is the way in), the
    /// first the accented one, then the workers, each marked only where its link is not up. With
    /// no worker there is nothing to open, and the page says where one comes from.
    fn render_empty(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let muted = hsla(s.text_muted);
        let title = |text: &'static str| {
            div()
                .pt(px(spacing.sm))
                .pb(px(spacing.xs))
                .text_size(px(theme.typography.title()))
                .font_weight(FontWeight(Typography::STRONG_WEIGHT))
                .text_color(hsla(s.text))
                .child(text)
        };
        let column = div().w(px(EMPTY_W)).flex().flex_col().items_center().gap(px(spacing.xs));
        let column = if self.workers.is_empty() {
            column
                .child(crate::icons::icon(theme, IconName::Server, IconSize::Large, muted))
                .child(title(NO_WORKERS))
                .child(div().text_color(muted).child(NO_WORKERS_NEXT))
        } else {
            let [terminal, agent, window] = &*BEGIN_KEYS;
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
                        true,
                    )
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.new_terminal(&super::actions::NewTerminal, window, cx);
                    })),
                )
                .child(
                    self.begin_row("empty-agent", IconName::Bot, NEW_AGENT, agent, false).on_click(
                        cx.listener(|this, _ev, window, cx| {
                            this.new_agent(&super::actions::NewAgent, window, cx);
                        }),
                    ),
                )
                .child(
                    self.begin_row("empty-window", IconName::AppWindow, ADD_WINDOW, window, false)
                        .on_click(cx.listener(|this, _ev, window, cx| {
                            this.add_window(&super::actions::AddWindow, window, cx);
                        })),
                );
            let workers = self.workers.iter().enumerate().map(|(ix, (key, w))| {
                let key = *key;
                let health = super::navigator::worker_health(&w.status);
                let label = match health {
                    Some((_, word)) => format!("{}, {word}", w.name),
                    None => w.name.clone(),
                };
                let row = div()
                    .id(("empty-worker", ix))
                    .debug_selector(move || format!("empty-worker-{ix}"))
                    .role(gpui::accesskit::Role::Button)
                    .aria_label(SharedString::from(label))
                    .w_full()
                    .px(px(spacing.md))
                    .py(px(spacing.xs))
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
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
                    .on_click(cx.listener(move |this, _ev, _window, cx| this.go_to_worker(key, cx)))
                    .into_any_element()
            });
            column
                .child(crate::icons::icon(theme, IconName::LayoutGrid, IconSize::Large, muted))
                .child(title(EMPTY_WORKSPACE))
                .child(begin)
                .child(
                    div()
                        .w_full()
                        .pt(px(spacing.md))
                        .flex()
                        .flex_col()
                        .child(
                            crate::palette::section_heading(
                                theme,
                                "empty-workers".into(),
                                "Workers",
                            )
                            .w_full(),
                        )
                        .children(workers),
                )
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .child(column)
            .into_any_element()
    }

    /// One way to begin: its icon, what it does and its key cap; `primary` is the accented one.
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
        let spacing = theme.spacing;
        let ink = if primary { s.accent } else { s.text };
        let icon_ink = if primary { s.accent } else { s.text_muted };
        let row = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(gpui::accesskit::Role::Button)
            .aria_label(label)
            .w_full()
            .px(px(spacing.md))
            .py(px(spacing.xs))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(|st| st.bg(hsla(s.raised)))
            .active(|st| st.bg(hsla(s.overlay)))
            .child(crate::palette::icon_slot(theme, icon, hsla(icon_ink)))
            .child(div().flex_1().text_color(hsla(ink)).child(label))
            .child(crate::kit::key_cap(theme, keys.to_owned()));
        crate::a11y::tab_stop(row, s.accent)
    }
}

/// How wide the empty workspace's column stands: room for the longest way to begin and its key
/// cap, narrow enough to read as one block in the middle of the strip.
const EMPTY_W: f32 = 320.0;

/// What the empty workspace says.
pub const EMPTY_WORKSPACE: &str = "Empty workspace";
pub const NO_WORKERS: &str = "No workers yet";
pub const NO_WORKERS_NEXT: &str = "Add a worker from the command palette.";
const NEW_TERMINAL: &str = "New terminal";
const NEW_AGENT: &str = "New agent";
const ADD_WINDOW: &str = "Add a window or display";
/// The overview's place for a new workspace.
pub const NEW_WORKSPACE: &str = "New workspace";

/// The keys of the three ways to begin, read once from the workspace's bindings.
pub(super) static BEGIN_KEYS: std::sync::LazyLock<[String; 3]> = std::sync::LazyLock::new(|| {
    let bindings = super::actions::key_bindings();
    [
        crate::palette::keys_for(&super::actions::NewTerminal, &bindings),
        crate::palette::keys_for(&super::actions::NewAgent, &bindings),
        crate::palette::keys_for(&super::actions::AddWindow, &bindings),
    ]
});
