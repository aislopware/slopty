//! The frame's top in the headless workspace: the navigator the window's height with the
//! title bar from its edge, and the breadcrumb's way between workspaces.

use gpui::Modifiers;

use super::*;

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn active(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout().active_workspace())
}

/// Docked, the navigator runs from the window's top and the title bar starts at its right
/// edge. The navigator's top row holds only the lights and the toggle, and the filter is the
/// first row under it. Hidden, the bar takes the whole width and starts past the traffic
/// lights, with the toggle where it stood.
#[gpui::test]
fn the_navigator_is_the_windows_height_and_the_bar_starts_at_its_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let (nav, bar) = (bounds(cx, "navigator"), bounds(cx, "titlebar"));
    assert!(f32::from(nav.top()).abs() < 0.5, "from the top: {nav:?}");
    assert!((f32::from(nav.size.height) - VIEWPORT.1).abs() < 0.5, "to the bottom: {nav:?}");
    assert_eq!(bar.left(), nav.right(), "the bar starts at its edge");
    let lights = bounds(cx, "nav-lights-row");
    assert_eq!(lights.bottom(), bar.bottom(), "the lights row is the bar's height");
    let docked = bounds(cx, "navigator-toggle");
    assert!(lights.contains(&docked.center()), "the toggle is in the lights row: {docked:?}");
    click(cx, "nav-search");
    let field = bounds(cx, "nav-filter-field");
    assert!(field.top() >= lights.bottom(), "the filter is under the lights row: {field:?}");
    assert!(field.left() > nav.left() && field.right() < nav.right(), "in from the sides");
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let bar = bounds(cx, "titlebar");
    assert!(f32::from(bar.left()).abs() < 0.5, "the whole width: {bar:?}");
    let toggle = bounds(cx, "navigator-toggle");
    assert!(f32::from(toggle.left()) >= titlebar::LEADING_INSET, "past the traffic lights");
    assert!(
        (f32::from(toggle.left()) - f32::from(docked.left())).abs() < 0.5,
        "the toggle never moves: {docked:?} then {toggle:?}"
    );
}

/// The breadcrumb's workspace segment is how the bar goes between workspaces: its menu lists
/// each one with something on it (the active one ticked, an empty active one too) and a new
/// one. What waits in another workspace shows as a mark on the segment, inside it, and its
/// label says so; no chord is spelled on it.
#[gpui::test]
fn the_breadcrumb_goes_between_workspaces(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(first, _), _, _] = three_shells(&view, cx, &studio);
    assert!(cx.debug_bounds("crumb-workspace").is_some());

    new_workspace_from_the_bar(cx);
    assert_eq!(active(&view, cx), 1, "+ goes to a new, empty workspace");
    click(cx, "crumb-workspace");
    assert!(cx.debug_bounds("menu-studio").is_some(), "the one with shells");
    assert!(cx.debug_bounds("menu-New workspace").is_some(), "and a new one");
    click(cx, "menu-studio");
    assert_eq!(active(&view, cx), 0, "a row goes there");
    assert!(cx.debug_bounds("menu").is_none(), "and closes the menu");

    // A column moved down makes a second workspace with something in it.
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_window_down_or_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    let first_ws =
        view.read_with(cx, |v, _| v.layout().position(v.tile_of_session(first).unwrap()).unwrap());
    let other = usize::from(first_ws.workspace == 0);
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.focus_workspace(other);
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("crumb-elsewhere").is_none(), "at rest, no mark");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(first), cx));
    cx.run_until_parked();
    let (segment, mark) = (
        cx.debug_bounds("crumb-workspace").expect("drawn"),
        cx.debug_bounds("crumb-elsewhere").expect("what waits elsewhere"),
    );
    assert!(segment.contains(&mark.center()), "inside its segment: {segment:?} {mark:?}");

    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let crumbs: Vec<String> =
        tree.into_iter().filter(|n| n.role == "Button").filter_map(|n| n.label).collect();
    assert!(crumbs.iter().any(|l| l.ends_with(", elsewhere 1 needs you")), "{crumbs:#?}");
    assert!(crumbs.iter().all(|l| !l.contains(['⌘', '⌥', '⌃'])), "no chords: {crumbs:#?}");
}

/// A shell in `cwd`, opened here.
fn shell_at(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    version: u64,
    cwd: Option<&str>,
) -> TileRef {
    opens_in(view, cx, fake, SessionId::new(), fake.me, version, cwd)
}

/// An unnamed workspace is named after the project most of its tiles share (a tie going to
/// the first in the strip, so the name holds as the focus moves), the worker's name only
/// where nothing else is shared, and the name the person gives wins.
#[gpui::test]
fn a_workspace_is_named_after_the_project_most_of_its_tiles_share(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let name = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.workspace_name());
    let home = shell_at(&view, cx, &studio, 1, None);
    assert_eq!(name(cx), "studio", "a home shell shares only its machine");
    let _site = shell_at(&view, cx, &studio, 2, Some("/w/site"));
    assert_eq!(name(cx), "site", "a project before a machine");
    let atlas_first = shell_at(&view, cx, &studio, 3, Some("/w/atlas"));
    assert_eq!(name(cx), "site", "a tie goes to the first in the strip");
    let atlas = shell_at(&view, cx, &studio, 4, Some("/w/atlas/docs"));
    let session = view.read_with(cx, |v, _| session_of(v, atlas));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        let moved = SessionSummary {
            repo: Some("/w/atlas".to_owned()),
            ..summary(session, Some("/w/atlas/docs"))
        };
        v.session_opened(key, moved, cx);
        let first = SessionSummary {
            repo: Some("/w/atlas".to_owned()),
            ..summary(session_of(v, atlas_first), Some("/w/atlas"))
        };
        v.session_opened(key, first, cx);
    });
    cx.run_until_parked();
    assert_eq!(name(cx), "atlas", "two of atlas outnumber one of site");
    view.update_in(cx, |v, _w, cx| v.focus_tile(home, cx));
    cx.run_until_parked();
    assert_eq!(name(cx), "atlas", "the focus does not rename it");
    view.update_in(cx, |v, _w, cx| {
        v.layout.set_workspace_name(0, Some("release".to_owned()));
        cx.notify();
    });
    assert_eq!(name(cx), "release", "a given name wins");
}
