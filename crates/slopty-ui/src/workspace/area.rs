//! The area: the tab on show, its panes drawn where the tiling lays them out
//! ([`super::panes`]), each holding the tile it shows (its header, or its row of tabs, over its
//! body), and the pointer that carries a tile by its header or its tab. With no tab on show,
//! the start page.
//!
//! A pane's header or tab, a navigator's tile row, or a title tab pressed and moved past
//! [`DRAG_SLOP`] carries what it stands for ([`Carried`]). A tile over a pane, within a fifth of
//! the pane's shorter side from an edge, splits it there; elsewhere it joins the pane's tabs
//! ([`slopty_client::layout::Tiling::drop_target`]), and the wash shows the pane the tile would
//! become. Over the title strip, a tile becomes a tab of its own and a title tab moves, where a
//! mark between two tabs says. Over a project's row in the navigator, either goes to that
//! project, the row washed ([`Landing`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Bounds, Context, Entity, EntityId, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, canvas, div, px,
};
use slopty_client::layout::tiling::TabId;
use slopty_client::layout::{Drop, GroupKey, Laid, PaneId, Rect, Sash, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_proto::items::ItemKind;
use slopty_proto::thread::{AgentId, ThreadId};

use super::WorkspaceView;
use super::panes::{PaneHost, Panes};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::Symbol;
use crate::kit;

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
    /// A title tab, its layout whole.
    Tab(TabId),
    /// A thread with no tile here, from its navigator row: dropped on a pane, its tile opens
    /// there (`MonoCode`'s session card dragged onto a pane's edge).
    Thread { worker: WorkerKey, thread: ThreadId },
}

/// Where what a drag carries would land.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Landing {
    /// On a pane of the tab on show: a tile alone lands there.
    Pane(Drop),
    /// On the title strip, before the tab at this index (past the last, after it).
    Strip(usize),
    /// On a project's row in the navigator.
    Project(GroupKey),
}

/// Where the chrome outside the area takes a drop, as it was last drawn, in window
/// coordinates: the title strip and its tabs, and the navigator's project rows. Each surface
/// writes its own as it lays out ([`spot`]).
#[derive(Default)]
pub(super) struct DropSpots {
    /// The title strip.
    pub strip: Cell<Option<Bounds<Pixels>>>,
    /// Each title tab.
    pub tabs: RefCell<Vec<(TabId, Bounds<Pixels>)>>,
    /// Each project row the navigator drew.
    pub projects: RefCell<Vec<(GroupKey, Bounds<Pixels>)>>,
}

impl DropSpots {
    /// Title tab `id` lies at `bounds`: in place of where it lay, should it be laid out twice.
    pub fn put_tab(&self, id: TabId, bounds: Bounds<Pixels>) {
        let mut tabs = self.tabs.borrow_mut();
        tabs.retain(|(t, _)| *t != id);
        tabs.push((id, bounds));
    }

    /// The row of project `home` lies at `bounds`.
    pub fn put_project(&self, home: &GroupKey, bounds: Bounds<Pixels>) {
        let mut projects = self.projects.borrow_mut();
        projects.retain(|(k, _)| k != home);
        projects.push((home.clone(), bounds));
    }

    /// The title strip is not drawn.
    pub fn no_strip(&self) {
        self.strip.set(None);
        self.tabs.borrow_mut().clear();
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
    /// Its pane holds other tiles too, so its header is a row of tabs.
    pub tabs: bool,
    /// Its tab holds other panes too, so the focused one's shown tab wears the focus edge.
    pub shared: bool,
    /// It is the one tile on show in its tab ([`WorkspaceView::lone_tile`]): its header is
    /// the title bar's, and its pane has none.
    pub lone: bool,
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
    /// What the last build handed each body it drew, and what the one before handed
    /// ([`WorkspaceView::hand_over`]).
    pub handed: RefCell<HashMap<EntityId, Handed>>,
    pub handed_before: RefCell<HashMap<EntityId, Handed>>,
    /// Each pane's tab row: where it is scrolled, and the tab it last brought into view at the
    /// width it last had.
    pub tab_rows: RefCell<HashMap<PaneId, (gpui::ScrollHandle, ItemId, f32)>>,
}

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
    /// press: a navigator row, a title tab.
    pub(super) fn begin_carry(&mut self, carried: Carried, ev: &MouseDownEvent) {
        self.drag = Some(Drag::Move { carried, grab: ev.position, moving: false, target: None });
    }

    /// Where `carried` would land with the pointer at `p`: the title strip, then a project's
    /// row other than its own, then for a tile a pane of the tab on show.
    fn landing_at(&self, carried: Carried, p: Point<Pixels>) -> Option<Landing> {
        let spots = &self.drop_spots;
        let thread = matches!(carried, Carried::Thread { .. });
        if !thread && spots.strip.get().is_some_and(|b| b.contains(&p)) {
            let mut tabs = spots.tabs.borrow().clone();
            tabs.sort_by(|a, b| f32::from(a.1.origin.x).total_cmp(&f32::from(b.1.origin.x)));
            let index = tabs.iter().position(|(_, b)| p.x < b.center().x).unwrap_or(tabs.len());
            return Some(Landing::Strip(index));
        }
        let row =
            spots.projects.borrow().iter().find(|(_, b)| b.contains(&p)).map(|(k, _)| k.clone());
        if let Some(home) = row.filter(|_| !thread) {
            let own = match carried {
                Carried::Tile(tile) => self.layout.position(tile).map(|pos| pos.project),
                Carried::Tab(id) => self.layout.tab_place(id).map(|(p, _)| p),
                Carried::Thread { .. } => None,
            };
            let own = own
                .and_then(|p| self.layout.projects().get(p))
                .map(slopty_client::layout::Project::home);
            return (own != Some(&home)).then_some(Landing::Project(home));
        }
        if let Carried::Tab(_) = carried {
            return None;
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

    /// The views that draw a landing: the area's wash, the strip's mark, a row's wash.
    fn landing_moved(&self, cx: &mut Context<Self>) {
        App::notify(cx, self.area_host.entity_id());
        App::notify(cx, self.chrome.title_tabs.entity_id());
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
            let home = self.layout.shown_project().map(|p| p.home().clone());
            self.layout_action(cx, |l| match (carried, target) {
                (Carried::Tile(tile), Landing::Pane(drop)) => {
                    l.place(tile, drop);
                    l.focus(tile);
                }
                (Carried::Tile(tile), Landing::Strip(index)) => {
                    if let Some(home) = &home {
                        l.new_tab_at(tile, home, index);
                    }
                }
                (Carried::Tile(tile), Landing::Project(home)) => l.move_to_project(tile, &home),
                (Carried::Tab(id), Landing::Strip(index)) => {
                    l.move_tab(id, index);
                }
                (Carried::Tab(id), Landing::Project(home)) => {
                    l.move_tab_to_project(id, &home);
                }
                (Carried::Tab(_), Landing::Pane(_)) | (Carried::Thread { .. }, _) => {}
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

    /// The tile on show alone in the tab on show: one pane laid out, holding one tile, and no
    /// other pane zoomed away. Its tab in the title bar says its title, so its pane draws no
    /// header, and the bar holds the rest of one ([`super::tile_strip`]). A tab's terminal put
    /// away leaves it alone; a phone's bar is its focused tile's anyway.
    pub(super) fn lone_tile(&self) -> Option<TileRef> {
        let tab = self.layout.shown_tab()?;
        if self.phone || tab.zoomed().is_some_and(|_| tab.panes().nth(1).is_some()) {
            return None;
        }
        let frame = self.layout.frame();
        let [laid] = frame.panes.as_slice() else { return None };
        match tab.pane(laid.pane)?.tiles() {
            [tile] => Some(*tile),
            _ => None,
        }
    }

    /// News for the tiles' headers and bodies alone: the panes are drawn again, and so is the
    /// title bar's strip while it is the one tile's header, and its tab where what it says
    /// changed ("Edited").
    pub(super) fn panes_news(&self, cx: &mut App) {
        App::notify(cx, self.area_host.entity_id());
        if self.lone_tile().is_some() {
            App::notify(cx, self.chrome.tile_strip.entity_id());
            if *self.title_tabs_drawn.borrow() != self.title_tabs() {
                App::notify(cx, self.chrome.title_tabs.entity_id());
            }
        }
    }

    /// The tile each pane of the tab on show shows, where.
    pub(super) fn placed_tiles(&self) -> Vec<Placed> {
        let Some(tab) = self.layout.shown_tab() else { return Vec::new() };
        let focus = tab.focus();
        let frame = self.layout.frame();
        let shared = frame.panes.len() > 1;
        let lone = self.lone_tile();
        frame
            .panes
            .iter()
            .filter_map(|laid| {
                let pane = tab.pane(laid.pane)?;
                Some(Placed {
                    tile: pane.shown()?,
                    rect: laid.rect,
                    pane: laid.pane,
                    focused: laid.pane == focus,
                    tabs: pane.tiles().len() > 1,
                    shared,
                    lone: lone.is_some(),
                })
            })
            .collect()
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
    /// "What should we work on?", over a boxed composer in the reading column, vertically
    /// centred, whose ↵ hands the goal to an orchestrator as "New goal…" would
    /// ([`Self::goal_from_page`]), its foot saying where the goal goes; under it one quiet line
    /// keeps a terminal and a window one press away. With no worker, or the server out of reach,
    /// the page says where machines come from, a fifth of the way down, with its doors.
    fn render_empty(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let muted = hsla(s.text_muted);
        let title_of = |text: SharedString| {
            kit::typed(kit::inset_x(div(), theme), theme.roles().panel_title)
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
                .child(kit::inset_x(div(), theme).text_color(muted).child(SERVER_DOWN_NEXT))
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
                .child(kit::inset_x(div(), theme).text_color(muted).child(NO_WORKERS_NEXT))
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

    /// The composer page: the question, the composer, and the quiet line of a terminal and a
    /// window, centred in the reading column.
    fn composer_page(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let question = kit::typed(div(), theme.roles().empty_heading)
            .id("empty-question")
            .debug_selector(|| "empty-question".to_owned())
            .role(gpui::accesskit::Role::Heading)
            .aria_label(EMPTY_QUESTION)
            .w_full()
            .pb(px(spacing.md))
            .text_color(hsla(s.text))
            .child(EMPTY_QUESTION);
        // Where ↵ sends the goal, said inside the box: nothing starts somewhere unseen, and a
        // press opens the sheet that changes it.
        let foot = self.page_goal_place(cx).map(|place| {
            let el = kit::typed(div(), theme.roles().metadata)
                .id("empty-goal-foot")
                .debug_selector(|| "empty-goal-foot".to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(SharedString::from(place.clone()))
                .aria_description(CHANGE_PLACE)
                .flex_none()
                .flex()
                .items_center()
                .h(px(theme.density.chip))
                .px(px(spacing.xs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(hsla(s.text_secondary))
                .hover(|st| st.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .child(SharedString::from(place));
            crate::a11y::tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, window, cx| {
                this.goal_sheet_from_page(window, cx);
            }))
        });
        let composer = self.empty_goal.as_ref().map(|(goal, _)| {
            kit::field(div(), theme)
                .debug_selector(|| "empty-goal".to_owned())
                .w_full()
                .rounded(px(theme.radii.md))
                .px(px(kit::FIELD_INSET))
                .pt(px(spacing.xs))
                .pb(px(spacing.xxs))
                .flex()
                .flex_col()
                .child(
                    gpui_kit::component::input::Textarea::new(goal)
                        .appearance(false)
                        .aria_label(EMPTY_QUESTION),
                )
                // The foot's words start where the goal's text does: the field pads its text
                // by the inset, and the chip pads its words by its own.
                .child(div().w_full().flex().pl(px(kit::FIELD_INSET - spacing.xs)).children(foot))
        });
        let ghost = |id: &'static str, label: &'static str| {
            let el = kit::typed(div(), theme.roles().metadata)
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(label)
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .h(px(theme.density.chip))
                .px(px(spacing.xs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .hover(|st| st.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .child(label);
            crate::a11y::tab_stop(el, s.focus)
        };
        let line = div()
            .w_full()
            .pt(px(spacing.sm))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .child(ghost("empty-terminal", NEW_TERMINAL).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.new_terminal(&super::actions::NewTerminal, window, cx);
                },
            )))
            .child(ghost("empty-window", ADD_WINDOW).on_click(cx.listener(
                |this, _ev, window, cx| {
                    this.add_window(&super::actions::AddWindow, window, cx);
                },
            )));
        div()
            .id("empty-workspace")
            .debug_selector(|| "empty-workspace".to_owned())
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(spacing.xl))
            .py(px(spacing.xl))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .w_full()
                    .max_w(px(crate::conversation::thread::view::COLUMN))
                    .flex()
                    .flex_col()
                    .child(question)
                    .children(composer)
                    .child(line),
            )
            .into_any_element()
    }

    /// Whether no tab is on show: the start page shows.
    pub(super) fn bare(&self) -> bool {
        self.layout.shown_tab().is_none()
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
    /// first door raised as the one ↵ would take.
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
        let icon_ink = if primary { s.text } else { s.text_secondary };
        let row = kit::row(theme, kit::Row::One)
            .id(id)
            .debug_selector(move || selector)
            .role(gpui::accesskit::Role::Button)
            .aria_label(label.clone())
            .w_full()
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(primary, |el| el.bg(hsla(s.selected)))
            .when(!primary, |el| el.hover(|st| st.bg(hsla(s.hover))))
            .active(|st| st.bg(hsla(s.pressed)))
            .child(crate::palette::icon_slot(theme, icon, hsla(icon_ink)))
            .child(div().flex_none().text_color(hsla(s.text)).child(label));
        crate::a11y::tab_stop(row, s.focus)
    }
}

/// How wide the empty workspace's column stands while it says where machines come from.
pub(super) const EMPTY_W: f32 = 400.0;

/// The empty workspace's question over its composer.
pub(crate) const EMPTY_QUESTION: &str = "What should we work on?";

/// What the empty workspace's composer takes, before anything is written in it.
pub(crate) const EMPTY_GOAL_HINT: &str =
    "Describe the goal. An orchestrator plans it and starts the agents";

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
