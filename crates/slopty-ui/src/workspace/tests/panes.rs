//! A tab's panes drawn from the tiling model, headless: each pane at its rectangle, the sash
//! dragged within each pane's least room, a double-click evening the split, the drop's wash, and
//! the title bar's tabs.

use std::str::FromStr as _;

use gpui::{
    Bounds, InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _,
    Pixels, Render, ScrollHandle, Styled as _, div, point,
};
use slopty_client::layout::tiling::{Drop, TabId, Tiling, TilingConfig};
use slopty_client::layout::tree::{PANE_MIN_W, Sash, Side};
use slopty_client::layout::{GroupKey, Rect};

use super::super::panes::{self, PaneHost, Panes};
use super::super::title_tabs::{self, TitleTab, TitleTabsHost};
use super::*;

/// The panes and the title tabs over a tiling, alone in a window.
struct Harness {
    theme: Theme,
    tiling: Tiling,
    panes: Panes,
    drop: Option<Drop>,
    released: usize,
    tabs_scroll: ScrollHandle,
    closed: Vec<TabId>,
}

impl PaneHost for Harness {
    fn panes(&self) -> &Panes {
        &self.panes
    }

    fn panes_mut(&mut self) -> &mut Panes {
        &mut self.panes
    }

    fn sash_pressed(&mut self, _cx: &mut Context<Self>) {}

    fn drag_sash(&mut self, sash: &Sash, delta: f32, cx: &mut Context<Self>) -> f32 {
        let went = self.tiling.drag_sash(sash, delta);
        cx.notify();
        went
    }

    fn sash_released(&mut self, _cx: &mut Context<Self>) {
        self.released = self.released.saturating_add(1);
    }

    fn equalize(&mut self, path: &[usize], _cx: &mut Context<Self>) {
        self.tiling.equalize(path);
    }
}

impl TitleTabsHost for Harness {
    fn show_title_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.tiling.show_tab(id);
        cx.notify();
    }

    fn close_title_tab(&mut self, id: TabId, _window: &mut Window, cx: &mut Context<Self>) {
        self.closed.push(id);
        self.tiling.drop_tab(id);
        cx.notify();
    }
}

/// The title tabs of the project on show.
fn title_tabs(tiling: &Tiling) -> Vec<TitleTab> {
    let Some(project) = tiling.shown_project() else { return Vec::new() };
    project
        .tabs()
        .iter()
        .enumerate()
        .map(|(i, tab)| TitleTab {
            id: tab.id(),
            title: format!("Tab {i}").into(),
            marks: vec![crate::icons::Status::Working; tab.panes().count().min(2)],
            shown: i == project.shown_index(),
        })
        .collect()
}

/// The window over a [`Harness`], drawn from it read and never written, as the workspace's own
/// views are ([`crate::draw`]).
struct Shown {
    harness: Entity<Harness>,
    _heard: Subscription,
}

impl Render for Shown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::draw::build(&self.harness.downgrade(), window, cx, |this, _window, cx| {
            let frame = this.tiling.frame();
            let tabs = title_tabs(&this.tiling);
            let body = |laid: &slopty_client::layout::tree::Laid| {
                let id = laid.pane.get();
                div().size_full().debug_selector(move || format!("body-{id}")).into_any_element()
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(div().h(px(TITLE_H)).w_full().flex().child(title_tabs::render(
                    &this.theme,
                    &tabs,
                    &this.tabs_scroll,
                    cx,
                )))
                .child(div().flex_1().w_full().child(panes::render(
                    this,
                    &this.theme,
                    &frame,
                    this.drop,
                    body,
                    cx,
                )))
                .into_any_element()
        })
    }
}

/// A selector made at run time, for `debug_bounds`, which takes one that lives forever.
fn sel(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// The title row's height in the harness: the panes lie below it.
const TITLE_H: f32 = 40.0;

/// The area the panes are laid out in.
const AREA: (f32, f32) = (1264.0, 800.0);

fn tile_n(n: u128) -> TileRef {
    let item = ItemId::from_str(&format!("00000000-0000-0000-0000-{n:012x}")).unwrap();
    TileRef { worker: WorkerKey::new(1), item }
}

fn atlas() -> GroupKey {
    GroupKey::new("project", "atlas")
}

/// A harness on `tiling`, laid out on [`AREA`] under the title row.
fn harness(cx: &mut TestAppContext, tiling: Tiling) -> (Entity<Harness>, &mut VisualTestContext) {
    cx.update(gpui_kit::init);
    let (shown, cx) = cx.add_window_view(|_window, cx| {
        let harness = gpui::AppContext::new(cx, |_| Harness {
            theme: Theme::default(),
            tiling,
            panes: Panes::default(),
            drop: None,
            released: 0,
            tabs_scroll: ScrollHandle::new(),
            closed: Vec::new(),
        });
        let heard = cx.observe(&harness, |_, _, cx| cx.notify());
        Shown { harness, _heard: heard }
    });
    cx.simulate_resize(size(px(AREA.0), px(AREA.1 + TITLE_H)));
    cx.run_until_parked();
    let view = shown.read_with(cx, |s, _| s.harness.clone());
    (view, cx)
}

/// Two tiles side by side on the Mac's area.
fn two_side_by_side() -> Tiling {
    let mut tiling = Tiling::new(TilingConfig::default());
    tiling.set_area(AREA.0, AREA.1);
    tiling.new_tab(tile_n(1), &atlas());
    tiling.split_focused(tile_n(2), Side::Right, &atlas());
    tiling
}

/// `r`, laid out under the title row, as the window's bounds.
fn on_screen(r: Rect) -> Bounds<Pixels> {
    Bounds::new(point(px(r.x), px(r.y + TITLE_H)), size(px(r.w), px(r.h)))
}

#[track_caller]
fn same(a: Bounds<Pixels>, b: Bounds<Pixels>) {
    let close = |x: Pixels, y: Pixels| (f32::from(x) - f32::from(y)).abs() < 0.5;
    assert!(
        close(a.origin.x, b.origin.x)
            && close(a.origin.y, b.origin.y)
            && close(a.size.width, b.size.width)
            && close(a.size.height, b.size.height),
        "{a:?} vs {b:?}"
    );
}

/// Each pane draws at the rectangle the model gives it, square and edge to edge, and each
/// sash lies on the edge two panes share.
#[gpui::test]
fn a_pane_draws_at_its_rect_and_the_sash_on_its_edge(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, two_side_by_side());
    let frame = view.read_with(cx, |h, _| h.tiling.frame());
    assert_eq!(frame.panes.len(), 2);
    for laid in &frame.panes {
        let drawn = cx.debug_bounds(sel(format!("pane-{}", laid.pane.get())));
        same(drawn.expect("the pane is drawn"), on_screen(laid.rect));
    }
    let sash = cx.debug_bounds("sash--0").expect("the sash is drawn");
    let edge = f32::from(sash.origin.x + sash.size.width / 2.0);
    assert!((edge - 632.0).abs() < 0.5, "on the shared edge: {sash:?}");
}

/// A press on the sash and a move drag it: the two panes beside it share the room as the
/// pointer says, down to a pane's least room and no further, and the release is said once. A
/// double-click evens them again.
#[gpui::test]
fn a_sash_drag_moves_the_two_panes_within_their_least_room(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, two_side_by_side());
    let sash = cx.debug_bounds("sash--0").unwrap();
    let at = sash.center();
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at - point(px(60.0), px(0.0)), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let left =
        |cx: &mut VisualTestContext| view.read_with(cx, |h, _| h.tiling.frame().panes[0].rect.w);
    assert!((left(cx) - 572.0).abs() < 1.0, "{}", left(cx));
    cx.simulate_mouse_move(at - point(px(400.0), px(0.0)), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!((left(cx) - PANE_MIN_W).abs() < 1.0, "never under the least: {}", left(cx));
    // Back towards the start: the line follows the pointer from where it stopped.
    cx.simulate_mouse_move(at - point(px(60.0), px(0.0)), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!((left(cx) - 572.0).abs() < 1.0, "{}", left(cx));
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |h, _| h.released), 1);
    let drawn = cx.debug_bounds("pane-1").unwrap();
    assert!((f32::from(drawn.size.width) - 572.0).abs() < 1.0, "drawn as laid out: {drawn:?}");

    let sash = cx.debug_bounds("sash--0").unwrap();
    cx.simulate_click(sash.center(), Modifiers::default());
    cx.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: sash.center(),
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_mouse_up(sash.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!((left(cx) - 632.0).abs() < 1.0, "a double-click evens them: {}", left(cx));
}

/// A drop's wash is the panel the tile would become: the half on the edge it splits, the
/// whole pane where it joins the tabs.
#[gpui::test]
fn a_drop_washes_the_panel_the_tile_would_become(cx: &mut TestAppContext) {
    let (view, cx) = harness(cx, two_side_by_side());
    let right = view.read_with(cx, |h, _| h.tiling.frame().panes[1]);
    view.update(cx, |h, cx| {
        h.drop = Some(Drop { pane: right.pane, edge: Some(Side::Bottom) });
        cx.notify();
    });
    cx.run_until_parked();
    let half = Rect { y: right.rect.y + right.rect.h / 2.0, h: right.rect.h / 2.0, ..right.rect };
    same(cx.debug_bounds("drop-wash").unwrap(), on_screen(half));
    view.update(cx, |h, cx| {
        h.drop = Some(Drop { pane: right.pane, edge: None });
        cx.notify();
    });
    cx.run_until_parked();
    same(cx.debug_bounds("drop-wash").unwrap(), on_screen(right.rect));
}

/// The title bar shows the tabs of the project on show, a mark per agent in each; a press
/// shows a tab and its panes, and its close takes it away.
#[gpui::test]
fn the_title_bar_shows_the_projects_tabs_and_their_agents(cx: &mut TestAppContext) {
    let mut tiling = two_side_by_side();
    tiling.new_tab(tile_n(3), &atlas());
    tiling.new_tab(tile_n(9), &GroupKey::new("project", "web"));
    tiling.show_project(&atlas());
    let (view, cx) = harness(cx, tiling);
    let ids: Vec<TabId> = view.read_with(cx, |h, _| {
        h.tiling
            .shown_project()
            .unwrap()
            .tabs()
            .iter()
            .map(slopty_client::layout::tree::Tab::id)
            .collect()
    });
    assert_eq!(ids.len(), 2, "only the project on show");
    let tab = |id: TabId| sel(format!("title-tab-{}", id.get()));
    assert!(cx.debug_bounds(tab(ids[0])).is_some() && cx.debug_bounds(tab(ids[1])).is_some());
    let mark = |id: TabId, i: usize| sel(format!("title-tab-mark-{}-{i}", id.get()));
    assert!(cx.debug_bounds(mark(ids[0], 1)).is_some(), "two agents, two marks");
    assert!(cx.debug_bounds(mark(ids[1], 1)).is_none(), "one agent, one mark");
    assert_eq!(
        view.read_with(cx, |h, _| h.tiling.shown_tab().map(slopty_client::layout::tree::Tab::id)),
        Some(ids[1])
    );

    let first = cx.debug_bounds(tab(ids[0])).unwrap();
    cx.simulate_click(first.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |h, _| h.tiling.shown_tab().map(slopty_client::layout::tree::Tab::id)),
        Some(ids[0])
    );
    let panes = view.read_with(cx, |h, _| h.tiling.frame().panes);
    assert_eq!(panes.len(), 2);
    for laid in panes {
        assert!(
            cx.debug_bounds(sel(format!("pane-{}", laid.pane.get()))).is_some(),
            "its panes drawn"
        );
    }

    let close = sel(format!("title-tab-close-{}", ids[0].get()));
    let close = cx.debug_bounds(close).expect("the shown tab's close");
    cx.simulate_click(close.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |h, _| h.closed.clone()), [ids[0]]);
    assert!(cx.debug_bounds(tab(ids[0])).is_none());
}
