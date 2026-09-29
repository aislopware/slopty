//! The navigator's list in the headless workspace: it lays out only the rows in view, brings
//! the focused tile's row into view, keeps a row with nothing to add to one line, and laid
//! over the frame runs through the home indicator's band.

use gpui::{AppContext as _, Bounds, Context, IntoElement, Render, Window};

use super::*;

/// `n` notes from elsewhere on `fake`, in one snapshot: rows enough to overflow the list.
fn notes(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake, n: usize) {
    let note = || Item {
        id: ItemId::new(),
        kind: ItemKind::Note { text: "a note\nits second line\n".into() },
        sleeping: false,
        name: None,
    };
    let items = std::iter::repeat_with(note).take(n).collect();
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx));
    cx.run_until_parked();
}

fn rows(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Vec<TileRef> {
    view.read_with(cx, |v, _| v.navigator_tiles())
}

fn drawn(cx: &mut VisualTestContext, tile: TileRef) -> Option<Bounds<Pixels>> {
    cx.debug_bounds(selector("nav-tile", tile.item))
}

/// Whether `tile`'s row is drawn wholly inside the list.
fn in_view(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) -> bool {
    let list = view.read_with(cx, |v, _| v.navigator_list_bounds());
    drawn(cx, tile).is_some_and(|row| row.top() >= list.top() && row.bottom() <= list.bottom())
}

/// Scroll the list by `dy` points of wheel.
fn scroll_list(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, dy: f32) {
    let at = view.read_with(cx, |v, _| v.navigator_list_bounds()).center();
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        momentum_phase: None,
    });
    cx.run_until_parked();
}

/// Of 120 tiles only the rows in the list's view, and a little past it, are laid out and
/// drawn; scrolled to the end, the last ones are and the first are not.
#[gpui::test]
fn the_navigator_draws_only_the_rows_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 120);
    let tiles = rows(&view, cx);
    assert_eq!(tiles.len(), 120);
    let count =
        |cx: &mut VisualTestContext| tiles.iter().filter(|t| drawn(cx, **t).is_some()).count();
    let shown = count(cx);
    // 800 points of window hold about 18 rows of 40.
    assert!((10..40).contains(&shown), "{shown} of 120 rows drawn");
    assert!(drawn(cx, tiles[0]).is_some(), "the first row is in view");
    assert!(drawn(cx, tiles[119]).is_none(), "the last is not laid out");

    scroll_list(&view, cx, -100_000.0);
    assert!(in_view(&view, cx, tiles[119]), "scrolled to the end, the last row shows");
    assert!(drawn(cx, tiles[0]).is_none(), "and the first is gone");
    assert!(count(cx) < 40, "still only what is in view: {}", count(cx));
}

/// Focus moving to a tile whose row is out of view scrolls the row into view, once: the human
/// can scroll away from it again. A shell opened at the end of a long list, its row new in
/// that very frame, is in view in that frame.
#[gpui::test]
fn the_focused_tiles_row_scrolls_into_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 60);
    let tiles = rows(&view, cx);
    let last = tiles[59];
    assert!(!in_view(&view, cx, last), "out of view at first");

    view.update_in(cx, |v, _w, cx| v.go_to_tile(last, cx));
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(last));
    assert!(in_view(&view, cx, last), "the focused row came into view");

    scroll_list(&view, cx, 100_000.0);
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(!in_view(&view, cx, last), "scrolled away, it stays away");
    assert!(in_view(&view, cx, tiles[0]));

    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert_eq!(focused(&view, cx), Some(shell));
    assert!(rows(&view, cx).last() == Some(&shell), "idle tiles in reading order: {shell:?}");
    assert!(in_view(&view, cx, shell), "a new row at the end came into view with it");
}

/// A waiting tile scrolled out of the list's view is listed under *Needs you*, which shows at
/// the top of a list at its top; scrolled back into view, its own row says it and the section
/// goes.
#[gpui::test]
fn needs_you_lists_a_waiting_tile_scrolled_out_of_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    notes(&view, cx, &studio, 60);
    let session = SessionId::new();
    let waiting = opens(&view, cx, &laptop, session, laptop.me, 1);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    let redraw = |cx: &mut VisualTestContext| {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    };
    redraw(cx);
    assert!(in_view(&view, cx, waiting), "focused, its row came into view");
    assert!(cx.debug_bounds("nav-needs-you").is_none(), "its own row says it");

    scroll_list(&view, cx, 100_000.0);
    redraw(cx);
    assert!(!in_view(&view, cx, waiting), "scrolled to the top, the row is far below");
    let list = view.read_with(cx, |v, _| v.navigator_list_bounds());
    let heading = cx.debug_bounds("nav-needs-you").expect("out of view, the section lists it");
    assert!(heading.top() >= list.top() && heading.bottom() <= list.bottom(), "{heading:?}");
    let row = Box::leak(format!("nav-waiting-{session}").into_boxed_str());
    assert!(cx.debug_bounds(row).is_some(), "the waiting agent's row");

    scroll_list(&view, cx, -100_000.0);
    redraw(cx);
    assert!(in_view(&view, cx, waiting), "scrolled to the end");
    assert!(cx.debug_bounds("nav-needs-you").is_none(), "back in view, the section goes");
}

/// A row under its worker's header never repeats the worker's name: a shell with no directory
/// yet, or only its home, has nothing to add, and its row is one line. A directory, a command
/// or an agent's words give it its second.
#[gpui::test]
fn a_row_with_nothing_to_add_is_one_line(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let bare = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let home = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/Users/me"));
    let work = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 3, Some("/w/oss/app"));
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    let metas: Vec<&str> = lines.iter().map(|(_, meta, _)| meta.as_str()).collect();
    assert!(!metas.contains(&"studio"), "no worker's name under its header: {metas:?}");
    assert!(!metas.contains(&"~"), "a home alone says nothing: {metas:?}");
    assert!(metas.contains(&"oss/app"), "{metas:?}");
    let height = |cx: &mut VisualTestContext, t: TileRef| {
        f32::from(cx.debug_bounds(selector("nav-tile", t.item)).expect("drawn").size.height)
    };
    let theme = Theme::default();
    assert!((height(cx, bare) - crate::kit::Row::One.height(&theme)).abs() < 0.5);
    assert!((height(cx, home) - crate::kit::Row::One.height(&theme)).abs() < 0.5);
    assert!((height(cx, work) - crate::kit::Row::Two.height(&theme)).abs() < 0.5);
    let meta = |id: ItemId| format!("nav-meta-{}", id.as_uuid());
    assert!(cx.debug_bounds(Box::leak(meta(bare.item).into_boxed_str())).is_none());
}

/// A tile's kind glyph sits under its worker's name, the list reading as a tree.
#[gpui::test]
fn a_tiles_glyph_sits_under_its_workers_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let name = format!("nav-worker-name-{}", studio.key);
    let kind = format!("nav-kind-{}", shell.item.as_uuid());
    let name = cx.debug_bounds(Box::leak(name.into_boxed_str())).expect("the name");
    let kind = cx.debug_bounds(Box::leak(kind.into_boxed_str())).expect("the glyph's slot");
    let glyph = f32::from(kind.left()) + navigator::glyph_margin(&Theme::default());
    assert!((glyph - f32::from(name.left())).abs() < 0.5, "{kind:?} under {name:?}");
}

/// The home indicator's band under the workspace, as the app lays it out on a phone.
const BAND: f32 = 34.0;

/// The app on a phone: the workspace, then the band the app draws below it.
struct Phone(Entity<WorkspaceView>);

impl Render for Phone {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .child(gpui::div().flex_1().min_h_0().w_full().child(self.0.clone()))
            .child(gpui::div().w_full().h(px(BAND)))
    }
}

/// On a phone the drawer and its scrim run to the window's bottom edge, over the band below
/// the workspace, not only to the workspace's.
#[gpui::test]
fn the_phone_drawer_runs_through_the_home_indicator_band(cx: &mut TestAppContext) {
    runs_through_the_band(cx, (390.0, 844.0), navigator::Mode::Drawer);
}

/// On an iPad, and anywhere else the navigator is laid over the frame, it and its scrim run to
/// the window's bottom edge as the phone's drawer does.
#[gpui::test]
fn the_overlaid_navigator_runs_through_the_home_indicator_band(cx: &mut TestAppContext) {
    runs_through_the_band(cx, (700.0, 900.0), navigator::Mode::Overlay);
}

/// Open the navigator in a window of `(w, h)` over the band, where it sits in `mode`: it and
/// its scrim end at the window's bottom edge.
fn runs_through_the_band(cx: &mut TestAppContext, (w, h): (f32, f32), mode: navigator::Mode) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(key_bindings());
        cx.bind_keys(crate::terminal::key_bindings());
    });
    let (host, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = WorkspaceView::new(Theme::default(), None, cx);
            view.set_animation(false);
            view
        });
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        Phone(view)
    });
    cx.simulate_resize(size(px(w), px(h)));
    cx.run_until_parked();
    let view = host.read_with(cx, |host, _| host.0.clone());
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.nav.drawn), Some(mode));

    let workspace = cx.debug_bounds("workspace").expect("the workspace is drawn");
    let drawer = cx.debug_bounds("navigator").expect("the drawer is open");
    let scrim = cx.debug_bounds("navigator-away").expect("over its scrim");
    assert!((f32::from(workspace.bottom()) - (h - BAND)).abs() < 0.5, "{workspace:?}");
    assert!((f32::from(drawer.bottom()) - h).abs() < 0.5, "to the bottom: {drawer:?}");
    assert!((f32::from(scrim.bottom()) - h).abs() < 0.5, "the scrim too: {scrim:?}");
    assert!(drawer.top() == workspace.top(), "from the top: {drawer:?}");
}
