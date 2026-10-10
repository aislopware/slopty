//! The navigator's list in the headless workspace: it lays out only the rows in view, brings
//! the focused tile's row into view, keeps a row with nothing to add to one line, and laid
//! over the frame runs through the home indicator's band.

use std::time::Instant;

use gpui::{AppContext as _, Bounds, Context, IntoElement, Render, Window};

use super::*;

/// `n` notes from elsewhere on `fake`, in one snapshot: rows enough to overflow the list.
fn notes(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake, n: usize) {
    let note = || Item {
        id: ItemId::new(),
        kind: ItemKind::Folder { path: "/w/notes".into() },
        name: None,
        facts: BTreeMap::new(),
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

/// How many viewports of rows a scroll layer paints on each side of a list's view, where GPUI
/// compiles layers in: gpui-fast's `fast::layers::paint::OVERSCAN_VIEWPORTS`.
const LAYER_OVERSCAN_VIEWPORTS: f32 = 2.0;

/// Of 200 tiles only the rows in the list's view, and a little past it, are laid out and
/// drawn; scrolled to the end, the last ones are and the first are not. Past the end nothing
/// is drawn, so what is drawn there is at most the view and, where the list is on a scroll
/// layer, the viewports of overscan above it, far fewer than every row.
#[gpui::test]
fn the_navigator_draws_only_the_rows_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 200);
    let tiles = rows(&view, cx);
    assert_eq!(tiles.len(), 200);
    let count =
        |cx: &mut VisualTestContext| tiles.iter().filter(|t| drawn(cx, **t).is_some()).count();
    let shown = count(cx);
    // 800 points of window hold about 18 rows of 40.
    assert!((10..40).contains(&shown), "{shown} of 200 rows drawn");
    assert!(drawn(cx, tiles[0]).is_some(), "the first row is in view");
    assert!(drawn(cx, tiles[199]).is_none(), "the last is not laid out");

    scroll_list(&view, cx, -100_000.0);
    assert!(in_view(&view, cx, tiles[199]), "scrolled to the end, the last row shows");
    assert!(drawn(cx, tiles[0]).is_none(), "and the first is gone");
    let list = view.read_with(cx, |v, _| v.navigator_list_bounds()).size.height;
    let row = drawn(cx, tiles[199]).expect("the last row").size.height;
    let in_view = (list / row).ceil() + 1.0;
    let most = in_view * (1.0 + LAYER_OVERSCAN_VIEWPORTS);
    let now = count(cx);
    let drawn_rows = f32::from(u16::try_from(now).expect("a count of rows"));
    assert!(drawn_rows <= most, "only the view and its overscan: {now} rows drawn, at most {most}");
}

/// A wheel over the navigator's list draws its rows again and leaves the panel around them, and
/// the filter field in it, as they were drawn: the field's input writes its own state each time
/// it is built, which a scroll need not pay for.
#[gpui::test]
fn a_scroll_of_the_navigator_draws_its_rows_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 120);
    let renders = |cx: &VisualTestContext| {
        view.read_with(cx, |v, cx| {
            (v.chrome.navigator.read(cx).renders, v.chrome.nav_rows.read(cx).renders)
        })
    };
    let (panel, rows) = renders(cx);
    for _ in 0..10 {
        scroll_list(&view, cx, -12.0);
    }
    let (panel_now, rows_now) = renders(cx);
    assert_eq!(panel_now, panel, "the panel was not drawn for a scroll");
    assert!(rows_now >= rows.saturating_add(10), "the rows were, each time: {rows} → {rows_now}");
}

/// What a frame of the navigator's list under the wheel costs: 120 notes scrolled 12 points a
/// frame, down for 100 frames and back up for 100, 600 frames after 20 of warm-up. It prints
/// the frame's time, the views it built, and how many frames composited a scroll layer, where
/// GPUI compiles them in. Run by hand (it prints, it does not judge); `docs/MEASUREMENTS.md`
/// has the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_the_navigator_scrolling(cx: &mut TestAppContext) {
    const FRAMES: usize = 600;
    const WARM: usize = 20;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 120);
    let mut took = Vec::with_capacity(FRAMES);
    for n in 0..WARM + FRAMES {
        if n == WARM {
            cx.update(|window, _| window.reset_layout_stats());
        }
        let dy = if (n / 100) % 2 == 0 { -12.0 } else { 12.0 };
        let start = Instant::now();
        scroll_list(&view, cx, dy);
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    let stats = cx.update(|window, _| window.layout_stats());
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    println!(
        "MEASURE navigator scrolling, 120 notes, {FRAMES} frames: p50 {:.3} ms p95 {:.3} ms; \
         {} views built; layer frames composited {} repainted {} demoted {}",
        pct(50),
        pct(95),
        stats.views_built,
        stats.layer_frames_composited,
        stats.layer_frames_repainted,
        stats.layers_demoted,
    );
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

/// A row never repeats what its header says: a shell with no directory yet, or only its home,
/// sits under its worker's name with nothing to add, and one at its project's root under the
/// project's; their rows are one line. A directory below the root, a branch, a command or an
/// agent's words give a row its second.
#[gpui::test]
fn a_row_with_nothing_to_add_is_one_line(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let bare = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let home = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/Users/me"));
    let root = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 3, Some("/w/notes"));
    let work = nav_rows::in_repo(&view, cx, &studio, 4, ("/w/oss/app", "/w/oss/app/src", "main"));
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    let metas: Vec<&str> = lines.iter().map(|(_, meta, _)| meta.as_str()).collect();
    assert!(!metas.contains(&"studio"), "no worker's name under its header: {metas:?}");
    assert!(!metas.contains(&"~"), "a home alone says nothing: {metas:?}");
    assert!(
        !metas.iter().any(|m| m.contains("notes")),
        "the project's root is its name: {metas:?}"
    );
    assert!(metas.contains(&"src · main"), "{metas:?}");
    let height = |cx: &mut VisualTestContext, t: TileRef| {
        f32::from(cx.debug_bounds(selector("nav-tile", t.item)).expect("drawn").size.height)
    };
    let theme = Theme::default();
    for one in [bare, home, root] {
        let want = navigator::row_height(&theme, crate::kit::Row::One);
        assert!((height(cx, one) - want).abs() < 0.5, "{one:?}");
    }
    let two = navigator::row_height(&theme, crate::kit::Row::Two);
    assert!((height(cx, work) - two).abs() < 0.5);
    let meta = |id: ItemId| format!("nav-meta-{}", id.as_uuid());
    assert!(cx.debug_bounds(Box::leak(meta(bare.item).into_boxed_str())).is_none());
}

/// A tile's kind glyph's slot starts under its worker's name, the list reading as a tree.
#[gpui::test]
fn a_tiles_glyph_sits_under_its_workers_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let name = format!("nav-worker-name-{}", studio.key);
    let kind = format!("nav-kind-{}", shell.item.as_uuid());
    let name = cx.debug_bounds(Box::leak(name.into_boxed_str())).expect("the name");
    let kind = cx.debug_bounds(Box::leak(kind.into_boxed_str())).expect("the glyph's slot");
    assert!(f32::from(kind.left() - name.left()).abs() < 0.5, "{kind:?} under {name:?}");
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

/// On a phone the home runs to the window's bottom edge, over the band below the workspace, not
/// only to the workspace's.
#[gpui::test]
fn the_phone_home_runs_through_the_home_indicator_band(cx: &mut TestAppContext) {
    runs_through_the_band(cx, (390.0, 844.0), navigator::Mode::Home);
}

/// On an iPad, and anywhere else the navigator is laid over the frame, it and its scrim run to
/// the window's bottom edge as the phone's home does.
#[gpui::test]
fn the_overlaid_navigator_runs_through_the_home_indicator_band(cx: &mut TestAppContext) {
    runs_through_the_band(cx, (700.0, 900.0), navigator::Mode::Overlay);
}

/// Open the navigator in a window of `(w, h)` over the band, where it sits in `mode`: its scrim
/// ends at the window's bottom edge, and so does the panel, past the workspace's bottom, from
/// the workspace's top.
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
    let drawer = cx.debug_bounds("navigator").expect("the navigator is open");
    let scrim = cx.debug_bounds("navigator-away").expect("over its scrim");
    assert!((f32::from(workspace.bottom()) - (h - BAND)).abs() < 0.5, "{workspace:?}");
    assert!((f32::from(drawer.bottom()) - h).abs() < 0.5, "to the bottom: {drawer:?}");
    assert!(drawer.bottom() > workspace.bottom(), "into the band: {drawer:?}");
    assert!((f32::from(scrim.bottom()) - h).abs() < 0.5, "the scrim to the edge: {scrim:?}");
    assert!((drawer.top() - workspace.top()).abs() < px(0.5), "from the top: {drawer:?}");
}

/// A wheel over the navigator's list composites its scroll layer where GPUI compiles layers in.
/// The list's view reads nothing a scroll changes, and the selected row's plate is painted
/// inside the row, so the background under the list bakes.
#[gpui::test]
fn a_scroll_of_the_navigator_composites_its_layer(cx: &mut TestAppContext) {
    const FRAMES: u64 = 30;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    notes(&view, cx, &studio, 120);
    for _ in 0..3 {
        scroll_list(&view, cx, -12.0);
    }
    cx.update(|window, _| window.reset_layout_stats());
    for _ in 0..FRAMES {
        scroll_list(&view, cx, -12.0);
    }
    let stats = cx.update(|window, _| window.layout_stats());
    assert_eq!((stats.layer_frames_composited, stats.layers_demoted), (FRAMES, 0));
}
