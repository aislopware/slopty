//! The area: the tab on show, its panes drawn where the tiling lays them out
//! ([`super::panes`]), each holding the tile it shows (its header, or its row of tabs, over its
//! body), and the pointer that carries a tile by its header or its tab. With no tab on show,
//! the start page.
//!
//! A pane's header or tab, or a navigator's tile row, pressed and moved past [`DRAG_SLOP`]
//! carries what it stands for ([`Carried`]). A tile over a pane, within a fifth of the pane's
//! shorter side from an edge, splits it there; elsewhere it joins the pane's tabs
//! ([`slopty_client::layout::Tiling::drop_target`]), and the wash shows the pane the tile would
//! become. Over a project's row in the navigator, it goes to that project, the row washed
//! ([`Landing`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Context, ElementId, Entity, EntityId, InteractiveElement as _, IntoElement as _,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, canvas, div,
    px,
};
use slopty_client::layout::{Drop, GroupKey, Laid, PaneId, Rect, Sash, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_kit::colors::hsla;
use slopty_kit::draw::Draw;
use slopty_kit::icons::Symbol;
use slopty_kit::kit;
use slopty_proto::items::ItemKind;
use slopty_proto::thread::{AgentId, ThreadId};

use super::WorkspaceView;
use super::panes::{PaneHost, Panes};

/// How far a header press travels before it is a move rather than a click.
const DRAG_SLOP: f32 = 4.0;

/// The pointer in progress: something pressed, a move once it travels [`DRAG_SLOP`], and
/// where it would land.
pub(super) enum Drag {
    Move { carried: Carried, grab: Point<Pixels>, moving: bool, target: Option<Landing> },
}

/// What a drag carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Carried {
    /// A tile: from its pane's header or tab, or its navigator row.
    Tile(TileRef),
    /// A thread with no tile here, from its navigator row: dropped on a pane, its tile opens
    /// there (`MonoCode`'s session card dragged onto a pane's edge).
    Thread { worker: WorkerKey, thread: ThreadId },
}

/// Where what a drag carries would land.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Landing {
    /// On a pane of the tab on show: a tile alone lands there.
    Pane(Drop),
    /// On a project's row in the navigator.
    Project(GroupKey),
}

/// Where the chrome outside the area takes a drop, as it was last drawn, in window
/// coordinates: the navigator's project rows. Each writes its own as it lays out ([`spot`]).
#[derive(Default)]
pub(super) struct DropSpots {
    /// Each project row the navigator drew.
    pub projects: RefCell<Vec<(GroupKey, Bounds<Pixels>)>>,
}

impl DropSpots {
    /// The row of project `home` lies at `bounds`.
    pub fn put_project(&self, home: &GroupKey, bounds: Bounds<Pixels>) {
        let mut projects = self.projects.borrow_mut();
        projects.retain(|(k, _)| k != home);
        projects.push((home.clone(), bounds));
    }
}

/// Whether the workspace behind `view` has something pressed that a move would carry.
fn dragging(view: &WeakEntity<WorkspaceView>, cx: &App) -> bool {
    view.read_with(cx, |this, _| this.drag.is_some()).unwrap_or(false)
}

/// A canvas over its parent that hands `put` the parent's bounds each time it is laid out:
/// how a drop spot is known where it was drawn.
pub(super) fn spot(put: impl Fn(Bounds<Pixels>) + 'static) -> impl gpui::IntoElement {
    canvas(move |bounds, _window, _cx| put(bounds), |_bounds, (), _window, _cx| {})
        .absolute()
        .inset_0()
}

/// One tile as its pane shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Placed {
    /// The tile its pane shows.
    pub tile: TileRef,
    /// Where its pane stands, in the area's coordinates.
    pub rect: Rect,
    /// Its pane.
    pub pane: PaneId,
    /// Its pane has the focus.
    pub focused: bool,
    /// Its tab holds other panes too, so the focused one's shown tab wears the focus dot.
    pub shared: bool,
    /// Its pane meets the window's top: its tab bar is part of the top line, whose empty run
    /// is the window's title bar.
    pub top: bool,
    /// Its pane meets the window's top line at its leading end: its tab bar starts with the
    /// window's lead ([`WorkspaceView::render_lead`]).
    pub top_left: bool,
    /// Its pane meets the window's top line at its trailing end: its tab bar ends with the
    /// window's cluster ([`super::cluster`]).
    pub top_right: bool,
}

/// What the area drew, kept by its view ([`super::AreaHost`]) and read by the workspace's
/// handlers: where each tile went, the area's bounds and the tiles on screen. The build writes
/// it while it reads the workspace and writes nothing there; hence cells, which the build holds
/// shared.
#[derive(Default)]
pub(super) struct Drawn {
    /// The area's bounds in the window, as of the last frame.
    pub viewport: Cell<Bounds<Pixels>>,
    /// Where each drawn tile was last frame, in window coordinates.
    pub placed: RefCell<Vec<(TileRef, Bounds<Pixels>)>>,
    /// The tile focused when the tiles were last drawn.
    pub focus: Cell<Option<TileRef>>,
    /// Tiles on screen in the frame last drawn: what a notice about one of them need not say
    /// again.
    pub on_screen: RefCell<HashSet<ItemId>>,
    /// Bumped every time the area builds.
    pub builds: Cell<u64>,
    /// Whether the window drew its own top line over the area in the last frame
    /// ([`WorkspaceView::follow_top_line`]).
    pub own_top_line: Cell<Option<bool>>,
    /// What the last build handed each body it drew, and what the one before handed
    /// ([`WorkspaceView::hand_over`]).
    pub handed: RefCell<HashMap<EntityId, Handed>>,
    pub handed_before: RefCell<HashMap<EntityId, Handed>>,
    /// Each pane's tab row: where it is scrolled, and the tab it last brought into view at the
    /// width it last had.
    pub tab_rows: RefCell<HashMap<PaneId, (gpui::ScrollHandle, ItemId, f32)>>,
    /// Each pane's tab row's motion ([`super::tab_look::Motion`]).
    pub tab_moves: RefCell<HashMap<PaneId, TabRowMoves>>,
}

/// A pane's tab row as it moves: its tabs growing and sliding, and where each tab lay last
/// frame, which a moved one slides from.
#[derive(Debug, Default)]
pub(super) struct TabRowMoves {
    pub motion: super::tab_look::Motion<ItemId>,
    pub spots: TabSpots,
}

/// Where each of a pane's tabs lay last frame, written as each is laid out.
pub(super) type TabSpots = Rc<RefCell<Vec<(ItemId, Bounds<Pixels>)>>>;

/// What a body takes from its tile: what its kind is laid out by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Handed {
    Shell { covered: bool },
    Face { width: f32 },
    Board { beside: bool },
    Review { width: f32, height: f32 },
    Stream { painted: f32 },
    Text { pad: f32, size: f32 },
}

impl WorkspaceView {
    /// Hand `view` what it takes from its tile, `handed`, once the area has read the
    /// workspace, and only when it differs from what the last build handed it: the body is then
    /// built again in this frame. Compared with what was handed, never read off the body: a read
    /// would build the area again with everything the body does.
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

    /// A tile was pressed: it takes the focus.
    pub(super) fn click_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        if self.focused() != Some(tile) {
            self.layout.focus(tile);
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
        self.begin_carry(Carried::Tile(tile), ev);
    }

    /// Something pressed that a move would carry, from a surface that does not focus it on the
    /// press: a navigator row.
    pub(super) fn begin_carry(&mut self, carried: Carried, ev: &MouseDownEvent) {
        self.drag = Some(Drag::Move { carried, grab: ev.position, moving: false, target: None });
    }

    /// Where `carried` would land with the pointer at `p`: a project's row other than its own,
    /// then a pane of the tab on show.
    fn landing_at(&self, carried: Carried, p: Point<Pixels>) -> Option<Landing> {
        let spots = &self.drop_spots;
        let row =
            spots.projects.borrow().iter().find(|(_, b)| b.contains(&p)).map(|(k, _)| k.clone());
        if let (Some(home), Carried::Tile(tile)) = (row, carried) {
            let own = self
                .layout
                .position(tile)
                .and_then(|pos| self.layout.projects().get(pos.project))
                .map(slopty_client::layout::Project::home);
            return (own != Some(&home)).then_some(Landing::Project(home));
        }
        if !self.drawn.viewport.get().contains(&p) {
            return None;
        }
        let (x, y) = self.local(p);
        self.layout.drop_target(x, y).map(Landing::Pane)
    }

    /// Where the drag in progress would land, while it moves.
    pub(super) const fn landing(&self) -> Option<&Landing> {
        match &self.drag {
            Some(Drag::Move { moving: true, target, .. }) => target.as_ref(),
            _ => None,
        }
    }

    /// The views that draw a landing: the area's wash, a row's wash.
    fn landing_moved(&self, cx: &mut Context<Self>) {
        App::notify(cx, self.area_host.entity_id());
        App::notify(cx, self.chrome.nav_rows.entity_id());
    }

    /// `p` in the area's own coordinates.
    fn local(&self, p: Point<Pixels>) -> (f32, f32) {
        let d = p - self.drawn.viewport.get().origin;
        (f32::from(d.x), f32::from(d.y))
    }

    /// What follows a drag wherever the pointer goes in the window, over the title bar and the
    /// navigator as over the area: listeners on the capture phase, so nothing under the pointer
    /// keeps a move or a release from it. They are there in every frame, so the first move after
    /// a press is followed without waiting for one.
    pub(super) fn render_follow(cx: &Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        canvas(
            |_bounds, _window, _cx| (),
            move |_bounds, (), window, _cx| {
                let moved = entity.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    if phase != gpui::DispatchPhase::Capture || !dragging(&moved, cx) {
                        return;
                    }
                    let _gone = moved.update(cx, |this, cx| this.drag_moved(ev, cx));
                });
                let released = entity;
                window.on_mouse_event(move |_ev: &MouseUpEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture && dragging(&released, cx) {
                        let _gone = released.update(cx, Self::end_drag);
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element()
    }

    /// The pointer moved while something is pressed.
    fn drag_moved(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(Drag::Move { carried, grab, moving, target }) = &self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let d = ev.position - *grab;
        if !*moving && f32::from(d.x).hypot(f32::from(d.y)) < DRAG_SLOP {
            return;
        }
        let now = self.landing_at(*carried, ev.position);
        let changed = !*moving || *target != now;
        if let Some(Drag::Move { moving, target, .. }) = &mut self.drag {
            *moving = true;
            *target = now;
        }
        if changed {
            self.landing_moved(cx);
        }
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let Drag::Move { carried, moving, target, .. } = drag;
        if let (true, Carried::Thread { worker, thread }, Some(Landing::Pane(drop))) =
            (moving, carried, &target)
        {
            self.open_thread_dropped(worker, thread, *drop, cx);
        } else if let (true, Some(target)) = (moving, target) {
            self.layout_action(cx, |l| match (carried, target) {
                (Carried::Tile(tile), Landing::Pane(drop)) => {
                    l.place(tile, drop);
                    l.focus(tile);
                }
                (Carried::Tile(tile), Landing::Project(home)) => l.move_to_project(tile, &home),
                (Carried::Thread { .. }, _) => {}
            });
        }
        // A press let go where it was is a click: nothing was drawn for it.
        if moving {
            self.landing_moved(cx);
        }
    }

    /// `thread` on `worker`, dropped on a pane: its tile there. One it already has here moves
    /// there; else its tile opens (its live terminal's, or its own) and lands where it was
    /// dropped once it comes.
    fn open_thread_dropped(
        &mut self,
        worker: WorkerKey,
        thread: ThreadId,
        drop: Drop,
        cx: &mut Context<Self>,
    ) {
        if let Some(tile) = self.tile_of_thread(thread) {
            self.layout_action(cx, |l| {
                l.place(tile, drop);
                l.focus(tile);
            });
            return;
        }
        let id = ItemId::new();
        let (key, session) = match self.live_terminal(thread) {
            Some((at, session)) => (at, Some(session)),
            None => (worker, None),
        };
        if let Some(w) = self.workers.get_mut(&key) {
            w.dropped.insert(id, drop);
        }
        match session {
            Some(session) => self.open_terminal_as(key, session, id, cx),
            None => self.open_thread_as(key, thread, id, cx),
        }
    }

    /// Where a press or a drop lands: the tile under it and whether it is on the body (not the
    /// header).
    pub(super) fn under(&self, p: Point<Pixels>) -> Option<(TileRef, bool)> {
        self.drawn.placed.borrow().iter().rev().find(|(_, b)| b.contains(&p)).map(|(tile, b)| {
            let body = p.y > b.origin.y + px(self.header_h());
            (*tile, body)
        })
    }

    /// The area laid out at `size`: the tiling's area, and the panes drawn again for it.
    fn area_resized(&mut self, size: gpui::Size<Pixels>, cx: &mut Context<Self>) {
        let before = self.pane_sizes();
        self.layout.set_area(f32::from(size.width), f32::from(size.height));
        self.resize_remote_windows(&before, cx);
        cx.notify();
        // The tiles are placed from the tiling just changed, and the area's view is drawn
        // again only when told.
        App::notify(cx, self.area_host.entity_id());
    }

    /// The area gives the window's own top line its height in the frame the line comes, and
    /// takes it back in the frame it goes (the first pane to meet the top holds the line in
    /// its tab bar), so what is placed in that frame, a display made for this device sized to
    /// its tile, is sized as it is drawn and not as the frame before had it.
    pub(super) fn follow_top_line(&mut self, window: &Window) {
        let own = self.own_top_line();
        let was = self.drawn.own_top_line.replace(Some(own));
        if was.is_none_or(|was| was == own) {
            return;
        }
        let line = super::titlebar_height(&self.theme) + f32::from(window.insets().effective().top);
        let area = self.layout.area();
        let h = if own { area.h - line } else { area.h + line };
        self.layout.set_area(area.w, h.max(0.0));
    }

    /// News for the tiles' headers and bodies alone: the panes are drawn again.
    pub(super) fn panes_news(&self, cx: &mut App) {
        App::notify(cx, self.area_host.entity_id());
    }

    /// The tile each pane of the tab on show shows, where.
    pub(super) fn placed_tiles(&self) -> Vec<Placed> {
        let Some(tab) = self.layout.shown_tab() else { return Vec::new() };
        let focus = tab.focus();
        let frame = self.layout.frame();
        let shared = frame.panes.len() > 1;
        let mut placed: Vec<Placed> = frame
            .panes
            .iter()
            .filter_map(|laid| {
                let pane = tab.pane(laid.pane)?;
                Some(Placed {
                    tile: pane.shown()?,
                    rect: laid.rect,
                    pane: laid.pane,
                    focused: laid.pane == focus,
                    shared,
                    top: false,
                    top_left: false,
                    top_right: false,
                })
            })
            .collect();
        // A pane meets the top line where its top is the area's, and the lead and the cluster
        // each go to one pane: the leftmost and the rightmost of those. A phone draws its own
        // bar, and the window its own line while no pane meets the top.
        if !self.own_top_line() {
            let area = self.layout.area();
            let top = |p: &Placed| (p.rect.y - area.y).abs() < 0.5;
            for p in &mut placed {
                p.top = top(p);
            }
            let left = placed
                .iter()
                .enumerate()
                .filter(|(_, p)| p.top)
                .min_by(|(_, a), (_, b)| a.rect.x.total_cmp(&b.rect.x))
                .map(|(ix, _)| ix);
            let right = placed
                .iter()
                .enumerate()
                .filter(|(_, p)| p.top)
                .max_by(|(_, a), (_, b)| (a.rect.x + a.rect.w).total_cmp(&(b.rect.x + b.rect.w)))
                .map(|(ix, _)| ix);
            if let Some(p) = left.and_then(|ix| placed.get_mut(ix)) {
                p.top_left = true;
            }
            if let Some(p) = right.and_then(|ix| placed.get_mut(ix)) {
                p.top_right = true;
            }
        }
        placed
    }

    /// The tiles on screen in this build: `visible`, remote ones first seen off screen from
    /// now, and the streams of those off screen past the grace let go. What changed is the
    /// workspace's, so it is done once the frame is over, and only when there is something to
    /// do: a timer comes back for the streams once the grace is up, since an idle area draws
    /// no frames.
    fn track_visibility(&self, placed: &[Placed], cx: &Draw<'_, Self>) {
        let visible: Vec<ItemId> = placed.iter().map(|p| p.tile.item).collect();
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

    /// Where a dragged tile would land, while it moves over a pane.
    const fn drop_shown(&self) -> Option<Drop> {
        match self.landing() {
            Some(Landing::Pane(drop)) => Some(*drop),
            _ => None,
        }
    }

    /// The tab on show, drawn from the tiling by the area's own view `host`: read here, never
    /// written. What the area drew goes to [`Drawn`]; what a tile's body takes from its tile (a
    /// width, a cover) goes to it after the read (`Draw::later`), and only where it changed.
    pub(super) fn render_area(
        &self,
        _host: EntityId,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let drawn = &self.drawn;
        drawn.builds.set(drawn.builds.get().wrapping_add(1));
        drawn.handed_before.swap(&drawn.handed);
        drawn.handed.borrow_mut().clear();
        let placed = self.placed_tiles();
        self.track_visibility(&placed, cx);
        let origin = drawn.viewport.get().origin;
        *drawn.placed.borrow_mut() = placed
            .iter()
            .map(|p| {
                let r = p.rect;
                let at = origin + gpui::point(px(r.x), px(r.y));
                (p.tile, Bounds::new(at, gpui::size(px(r.w), px(r.h))))
            })
            .collect();
        let frame = self.layout.frame();
        let body = |laid: &Laid| -> gpui::AnyElement {
            let Some(p) = placed.iter().find(|p| p.pane == laid.pane) else {
                return div().into_any_element();
            };
            let tile = self.render_tile(p, window, cx);
            let notices = self.tile_notices(p, cx);
            div().relative().size_full().children(tile).children(notices).into_any_element()
        };
        // A pane new to the tab on show arrives from its edge, on the layout's clock.
        let arrive = self.layout.shown_tab().map(|tab| super::panes::Arrive {
            tab: tab.id(),
            now: self.clock_instant(),
            moves: self.animate && kit::motion(cx),
        });
        let shown = super::panes::Shown { frame: &frame, drop: self.drop_shown(), arrive };
        let panes = super::panes::render(self, &self.theme, &shown, body, window, cx);
        // Only now: a body drawn above reads the focus it was last drawn with, so the two the
        // focus moved between are built again in this frame (`tile::body_view`).
        drawn.focus.set(self.layout.focused());
        let measure = canvas(
            {
                let (this, drawn) = (cx.weak_entity(), Rc::clone(drawn));
                move |bounds, _window, cx| Self::area_measured(&this, &drawn, bounds, cx)
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let empty = self.layout.shown_tab().is_none().then(|| self.render_empty(cx));
        div()
            .id("area")
            .debug_selector(|| "area".to_owned())
            .relative()
            .flex_1()
            .w_full()
            .overflow_hidden()
            .child(measure)
            .child(panes)
            .children(empty)
            .into_any_element()
    }

    /// The notices about `p`'s own work, under its header at its trailing edge.
    fn tile_notices(&self, p: &Placed, cx: &Draw<'_, Self>) -> Option<gpui::AnyElement> {
        let notices = self.render_tile_notices(p.tile, cx)?;
        let item = p.tile.item;
        let spacing = self.theme.spacing;
        Some(
            div()
                .debug_selector(move || format!("tile-notices-{}", item.as_uuid()))
                .absolute()
                .left_0()
                .right_0()
                .top(px(self.header_h()))
                .flex()
                .justify_end()
                .px(px(spacing.sm))
                .pt(px(spacing.xs))
                .child(notices)
                .into_any_element(),
        )
    }

    /// The area laid out at `bounds`: kept for the handlers, and a new size is the tiling's,
    /// once this frame is over.
    fn area_measured(this: &WeakEntity<Self>, drawn: &Drawn, bounds: Bounds<Pixels>, cx: &mut App) {
        let was = drawn.viewport.replace(bounds);
        if was.size != bounds.size {
            let this = this.clone();
            cx.defer(move |cx| {
                let _gone = this.update(cx, |this, cx| this.area_resized(bounds.size, cx));
            });
        }
    }

    /// An empty workspace. With a machine to begin on it is the composer page: one question,
    /// "What should we work on?", over the unboxed composer in a narrow column a third of the
    /// way down, whose ↵ hands the goal to an orchestrator as "New goal…" would
    /// ([`Self::goal_from_page`]), its foot saying where the goal goes; under it a short list
    /// keeps a terminal and a window one press away. With no worker, or the server out of reach,
    /// the page says where machines come from, a fifth of the way down, with its doors.
    fn render_empty(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let muted = hsla(s.text_muted);
        let title_of = |text: SharedString| {
            kit::typed(div(), theme.roles().panel_title)
                .pb(px(spacing.xs))
                .text_color(hsla(s.text))
                .child(text)
        };
        let column = div().w_full().max_w(px(EMPTY_W)).flex().flex_col();
        // With the server out of reach, no machine is listed because nobody lists them: the
        // page says so first, with its doors, rather than send the person to add a machine.
        let server_down = self.workers.is_empty().then(|| self.server_status.clone()).flatten();
        let column = if let Some(status) = server_down {
            let doors = self.server_entries.iter().enumerate().map(|(ix, entry)| {
                let run = Rc::clone(&entry.run);
                let icon = if ix == 0 { Symbol::ArrowClockwise } else { Symbol::Link };
                self.door_row(
                    if ix == 0 { "empty-server-retry" } else { "empty-server-other" },
                    icon,
                    entry.label.clone(),
                    ix == 0,
                )
                .on_click(move |_ev, window, cx| run(window, cx))
            });
            let add = self.add_worker_run().map(|run| {
                self.door_row("empty-add-worker", Symbol::Plus, ADD_WORKER.into(), false)
                    .on_click(move |_ev, window, cx| run(window, cx))
            });
            column
                .child(title_of(SharedString::from(super::readouts::sentence(&status))))
                .child(div().text_color(muted).child(SERVER_DOWN_NEXT))
                .child(
                    div()
                        .w_full()
                        .pt(px(spacing.md))
                        .flex()
                        .flex_col()
                        .children(doors)
                        .children(add),
                )
        } else if self.workers.is_empty() {
            let add = self.add_worker_run().map(|run| {
                div().w_full().pt(px(spacing.md)).child(
                    self.door_row("empty-add-worker", Symbol::Plus, ADD_WORKER.into(), true)
                        .on_click(move |_ev, window, cx| run(window, cx)),
                )
            });
            column
                .child(title_of(NO_WORKERS.into()))
                .child(div().text_color(muted).child(NO_WORKERS_NEXT))
                .children(add)
        } else {
            return kit::pane_surface(theme)
                .absolute()
                .inset_0()
                .child(self.composer_page(cx))
                .into_any_element();
        };
        // The empty workspace is the page a tile would be, a panel where the tile would stand,
        // not a hole down to the canvas. It starts a fifth of the way down, where a dialog
        // sits, by this frame's layout: the strip's size as the last frame measured it is a
        // frame behind chrome that comes or goes.
        let page = div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".to_owned())
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .px(px(spacing.md))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .child(div().flex_none().w_full().h(gpui::relative(kit::MODAL_ANCHOR)))
            .child(column.pb(px(spacing.xl)));
        kit::pane_surface(theme).absolute().inset_0().child(page).into_any_element()
    }

    /// The composer page, a thread with no turns (`.research/design-polish-2026-10-12.md`
    /// §4.9): the goal's composer at the pane's foot, where a thread keeps it, and above it a
    /// launcher in a 512 pt column whose top stands at 30 % of the pane, on one edge: the
    /// question, where a goal goes, a *Start* head over the ways that need no goal, then a
    /// *Recent* head over the tabs used last. The doors print no keys: keys are the palette's.
    fn composer_page(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let asked = self.empty_question();
        let question = kit::typed(div(), theme.roles().page_heading)
            .id("empty-question")
            .debug_selector(|| "empty-question".to_owned())
            .role(gpui::accesskit::Role::Heading)
            .aria_label(asked.clone())
            .w_full()
            .text_color(hsla(s.text))
            .child(asked.clone());
        // Where ↵ sends the goal, a muted line under the question: nothing starts somewhere
        // unseen, and a press opens the sheet that changes it.
        let place = self.page_goal_place(cx).map(|place| {
            let el = kit::typed(div(), theme.roles().metadata)
                .id("empty-goal-foot")
                .debug_selector(|| "empty-goal-foot".to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(SharedString::from(place.clone()))
                .aria_description(CHANGE_PLACE)
                .flex_none()
                .min_w_0()
                .pt(px(spacing.sm))
                .cursor_pointer()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(hsla(s.text_muted))
                .hover(|st| st.text_color(hsla(s.text)))
                .child(SharedString::from(place));
            slopty_kit::a11y::tab_stop(el, s.focus).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.goal_sheet_from_page(window, cx);
                },
            ))
        });
        let head = |id: &'static str, words: &'static str| {
            kit::typed(div(), theme.roles().section)
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(gpui::accesskit::Role::Heading)
                .aria_label(words)
                .pt(px(spacing.xl))
                .pb(px(spacing.xs))
                .text_color(hsla(s.text_muted))
                .child(words)
        };
        let doors = div()
            .w_full()
            .flex()
            .flex_col()
            .child(head("empty-start", START))
            .child(
                self.door_row("empty-terminal", Symbol::Terminal, NEW_TERMINAL.into(), false)
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.new_terminal(&super::actions::NewTerminal, window, cx);
                    })),
            )
            .child(
                self.door_row("empty-window", Symbol::Macwindow, ADD_WINDOW.into(), false)
                    .on_click(cx.listener(|this, _ev, window, cx| {
                        this.add_window(&super::actions::AddWindow, window, cx);
                    })),
            )
            .child(
                self.door_row(
                    "empty-new-goal",
                    Symbol::Flag,
                    super::titlebar::NEW_GOAL.into(),
                    false,
                )
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.new_goal(&super::actions::NewGoal, window, cx);
                })),
            );
        // The tabs used last, newest first, each a row that goes back to it.
        let recent: Vec<(ItemId, String, slopty_kit::icons::Mark)> = self
            .recency
            .iter()
            .rev()
            .filter(|id| self.tile_of(**id).is_some())
            .filter_map(|id| {
                let (_, item) = self.items().find(|(_, item)| item.id == *id)?;
                Some((*id, self.tile_title(item), self.kind_glyph(item)))
            })
            .take(RECENT_TABS)
            .collect();
        let recent = (!recent.is_empty()).then(|| {
            let rows = recent.into_iter().map(|(id, title, mark)| {
                let selector = format!("empty-recent-{}", id.as_uuid());
                let row = kit::row(theme, kit::Row::One)
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .role(gpui::accesskit::Role::Button)
                    .aria_label(SharedString::from(title.clone()))
                    .ml(px(-spacing.inset()))
                    .mr(px(-spacing.inset_trailing()))
                    .rounded(px(theme.radii.sm))
                    .cursor_pointer()
                    .hover(|st| st.bg(hsla(s.hover)))
                    .active(|st| st.bg(hsla(s.pressed)))
                    .child(crate::palette::lead_slot(theme, mark, hsla(s.icon_muted)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(title)),
                    )
                    .on_click(cx.listener(move |this, _ev, _window, cx| this.go_to(id, cx)));
                slopty_kit::a11y::tab_stop(row, s.focus)
            });
            div().w_full().flex().flex_col().child(head("empty-recent", RECENT)).children(rows)
        });
        // The goal's composer, unboxed, at the pane's foot in the reading column a thread
        // keeps it in, so the habit is one place.
        let composer = self.empty_goal.as_ref().map(|(goal, _)| {
            let (side, lane) = (spacing.lg, crate::conversation::thread::view::LANE);
            let column = crate::conversation::thread::view::PROSE;
            div().flex_none().w_full().flex().justify_center().pb(px(spacing.sm)).child(
                kit::message::shell(div(), theme)
                    .debug_selector(|| "empty-goal".to_owned())
                    .w_full()
                    .max_w(px(2.0_f32.mul_add(side, lane + column)))
                    .pl(px(side + lane))
                    .pr(px(side))
                    .pt(px(spacing.sm))
                    .child(
                        gpui_kit::component::input::Textarea::new(goal)
                            .appearance(false)
                            .px_0()
                            .aria_label(asked.clone()),
                    ),
            )
        });
        let launcher = div()
            .id("empty-launcher")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .px(px(spacing.page()))
            .child(div().flex_none().w_full().h(gpui::relative(EMPTY_ANCHOR)))
            .child(
                div()
                    .debug_selector(|| "empty-column".to_owned())
                    .w_full()
                    .max_w(px(EMPTY_W))
                    .pb(px(spacing.xl))
                    .flex()
                    .flex_col()
                    .child(question)
                    .children(place)
                    .child(doors)
                    .children(recent),
            );
        div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .child(launcher)
            .children(composer)
            .into_any_element()
    }

    /// Whether no tab is on show: the start page shows.
    pub(super) fn bare(&self) -> bool {
        self.layout.shown_tab().is_none()
    }

    /// The empty workspace's question: in the project on show, by its name, when one is (a
    /// folder's or a repository's), else [`EMPTY_QUESTION`]. A machine's own work is no
    /// project, and the composer's foot already says which machine a goal goes to.
    pub(super) fn empty_question(&self) -> SharedString {
        let project = self.layout.shown_project().filter(|p| p.home().worker().is_none());
        let name = project.map(|_| self.project_name()).unwrap_or_default();
        if name.is_empty() {
            return EMPTY_QUESTION.into();
        }
        format!("What should we work on in {name}?").into()
    }

    /// Where work stands or stood across the workers that are up, one entry per directory on
    /// each: where shells stand, where threads work, the last start's folder and where the
    /// agents' past sessions ran there; of the threads, the start and the past sessions only
    /// `agent`'s, when given. The most recently used tile's first (the tile recency), then the
    /// newest. What a start's folder step lists.
    pub(super) fn recent_places(&self, agent: Option<&AgentId>, cx: &App) -> Vec<RecentPlace> {
        // (tile recency, when, worker, folder, repository, branch)
        type Seen = (Option<usize>, u64, WorkerKey, String, Option<String>, Option<String>);
        let rank =
            |item: Option<ItemId>| item.and_then(|i| self.recency.iter().rposition(|r| *r == i));
        let ours = |a: &AgentId| agent.is_none_or(|want| want == a);
        let mut seen: Vec<Seen> = Vec::new();
        for (key, w) in self.workers.iter().filter(|(_, w)| w.link.is_some()) {
            let mut shells: HashMap<SessionId, ItemId> = HashMap::new();
            let mut threads: HashMap<ThreadId, ItemId> = HashMap::new();
            for item in w.doc.items() {
                match item.kind {
                    ItemKind::Terminal { session } => {
                        shells.insert(session, item.id);
                    }
                    ItemKind::Thread { thread } => {
                        threads.insert(thread, item.id);
                    }
                    _ => {}
                }
            }
            for summary in w.sessions.values() {
                let Some(cwd) = summary.cwd.clone() else { continue };
                let tile = rank(shells.get(&summary.id).copied());
                let (repo, branch) = (summary.repo.clone(), summary.branch.clone());
                seen.push((tile, summary.started_ms.as_millis(), *key, cwd, repo, branch));
            }
            for t in self.thread_folders(*key, cx).into_iter().filter(|t| ours(&t.agent)) {
                let tile = rank(threads.get(&t.thread).copied())
                    .max(rank(t.terminal.and_then(|s| shells.get(&s).copied())));
                seen.push((tile, t.updated.as_millis(), *key, t.cwd, t.repo, None));
            }
            let last = self.starts.last().filter(|l| l.worker == *key && ours(&l.agent));
            if let Some(last) = last {
                seen.push((None, last.at.as_millis(), *key, last.cwd.clone(), None, None));
            }
            let past = self.past_places.get(key).into_iter().flatten().filter(|p| ours(&p.agent));
            for p in past {
                let at = p.at.map_or(0, slopty_core::WallMs::as_millis);
                seen.push((None, at, *key, p.cwd.clone(), None, None));
            }
        }
        seen.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        // Each directory once, where it ranks best, with what any of its sightings knew of it.
        let mut places: Vec<(WorkerKey, String, Option<String>, Option<String>)> = Vec::new();
        for (.., worker, cwd, repo, branch) in seen {
            match places.iter_mut().find(|p| p.0 == worker && p.1 == cwd) {
                Some(place) => {
                    place.2 = place.2.take().or(repo);
                    place.3 = place.3.take().or(branch);
                }
                None => places.push((worker, cwd, repo, branch)),
            }
        }
        places
            .into_iter()
            .map(|(worker, cwd, repo, branch)| {
                let home = self.home_of(worker);
                let name = super::tile::repo_place(&cwd, repo.as_deref(), home);
                RecentPlace { worker, cwd, name, branch }
            })
            .collect()
    }

    /// A row of the empty page that opens a door, not a tile: its glyph and its words, the
    /// first door raised as the one ↵ would take. It hangs by its pad, so its glyph stands on
    /// the column's edge.
    fn door_row(
        &self,
        id: &'static str,
        icon: Symbol,
        label: SharedString,
        primary: bool,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let selector = id.to_owned();
        let icon_ink = if primary { s.icon } else { s.icon_muted };
        let row = kit::row(theme, kit::Row::One)
            .id(id)
            .debug_selector(move || selector)
            .role(gpui::accesskit::Role::Button)
            .aria_label(label.clone())
            .ml(px(-theme.spacing.inset()))
            .mr(px(-theme.spacing.inset_trailing()))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(primary, |el| el.bg(hsla(s.selected)))
            .when(!primary, |el| el.hover(|st| st.bg(hsla(s.hover))))
            .active(|st| st.bg(hsla(s.pressed)))
            .child(crate::palette::icon_slot(theme, icon, hsla(icon_ink)))
            .child(div().flex_1().min_w_0().truncate().text_color(hsla(s.text)).child(label));
        slopty_kit::a11y::tab_stop(row, s.focus)
    }
}

/// How wide the empty workspace's column stands: Zed's welcome's 512.
pub(super) const EMPTY_W: f32 = 512.0;

/// How far down the empty workspace's launcher starts: 30 % of the pane, higher than the
/// middle, as Zed's welcome stands.
const EMPTY_ANCHOR: f32 = 0.3;

/// How many tabs used last the empty workspace lists.
const RECENT_TABS: usize = 5;

/// The head over the ways to begin.
const START: &str = "Start";

/// The head over the tabs used last.
const RECENT: &str = "Recent";

/// The empty workspace's question over its composer.
pub(crate) const EMPTY_QUESTION: &str = "What should we work on?";

/// What the empty workspace's composer takes, before anything is written in it.
pub(crate) const EMPTY_GOAL_HINT: &str = "Describe the goal. An orchestrator plans it";

/// What the composer's foot says it does, to a screen reader, after where the goal goes.
pub(crate) const CHANGE_PLACE: &str = "Change where it goes";

/// A directory work stands or stood in on a worker: where a start's folder step offers to
/// start one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct RecentPlace {
    pub worker: WorkerKey,
    /// Its full path on the worker, where the start goes.
    pub cwd: String,
    /// What it is called: the repository and the path within it, else the path's tail.
    pub name: String,
    pub branch: Option<String>,
}

/// What the empty workspace says with no worker to begin on.
pub(crate) const NO_WORKERS: &str = "No machines yet";
pub(crate) const NO_WORKERS_NEXT: &str = "A machine runs your shells, agents and windows.";
/// What the empty workspace says under the server's state while it is out of reach.
pub(crate) const SERVER_DOWN_NEXT: &str =
    "The server lists your machines. Machines you reach directly still work.";
/// The empty workspace's way to a first worker, as the "…" menu words it.
pub(crate) const ADD_WORKER: &str = "Add a machine";
const NEW_TERMINAL: &str = "New terminal";
const ADD_WINDOW: &str = "Add a window or display";

impl PaneHost for WorkspaceView {
    fn panes(&self) -> &Panes {
        &self.panes
    }

    fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }

    fn sash_pressed(&mut self, _cx: &mut Context<Self>) {
        self.sash_before = self.pane_sizes();
    }

    fn drag_sash(&mut self, sash: &Sash, delta: f32, cx: &mut Context<Self>) -> f32 {
        let went = self.layout.drag_sash(sash, delta);
        if went != 0.0 {
            App::notify(cx, self.area_host.entity_id());
        }
        went
    }

    fn sash_released(&mut self, cx: &mut Context<Self>) {
        let before = std::mem::take(&mut self.sash_before);
        self.resize_remote_windows(&before, cx);
        self.layout_touched(cx);
        App::notify(cx, self.area_host.entity_id());
    }

    fn equalize(&mut self, path: &[usize], cx: &mut Context<Self>) {
        let path = path.to_vec();
        self.layout_action(cx, |l| {
            l.equalize(&path);
        });
    }
}
