//! The navigator's list in the headless workspace: it lays out only the rows in view, brings
//! the focused tile's row into view, gives a shell with no directory a second line, and on a
//! phone runs its drawer through the home indicator's band.

use gpui::{AppContext as _, Bounds, Context, IntoElement, Render, Window};

use super::*;

/// `n` notes from elsewhere on `fake`, in one snapshot: rows enough to overflow the list.
fn notes(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, fake: &Fake, n: usize) {
    let note = || Item {
        id: ItemId::new(),
        kind: ItemKind::Note { text: "a note\n".into() },
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

/// A shell with no directory yet reads its worker's name where the directory goes, so its
/// second line is never a lone age.
#[gpui::test]
fn a_shell_with_no_directory_names_its_worker(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].1, "studio", "{lines:?}");
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
    const PHONE: (f32, f32) = (390.0, 844.0);
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
    cx.simulate_resize(size(px(PHONE.0), px(PHONE.1)));
    cx.run_until_parked();
    let view = host.read_with(cx, |host, _| host.0.clone());
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();

    let workspace = cx.debug_bounds("workspace").expect("the workspace is drawn");
    let drawer = cx.debug_bounds("navigator").expect("the drawer is open");
    let scrim = cx.debug_bounds("navigator-away").expect("over its scrim");
    assert!((f32::from(workspace.bottom()) - (PHONE.1 - BAND)).abs() < 0.5, "{workspace:?}");
    assert!((f32::from(drawer.bottom()) - PHONE.1).abs() < 0.5, "to the bottom: {drawer:?}");
    assert!((f32::from(scrim.bottom()) - PHONE.1).abs() < 0.5, "the scrim too: {scrim:?}");
    assert!(drawer.top() == workspace.top(), "from the top: {drawer:?}");
}
