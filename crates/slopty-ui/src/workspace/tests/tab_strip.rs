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
/// edge; hidden, the bar takes the whole width and starts past the traffic lights.
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
    assert!(bounds(cx, "nav-filter").top() < bar.bottom(), "the filter is in the top row");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let bar = bounds(cx, "titlebar");
    assert!(f32::from(bar.left()).abs() < 0.5, "the whole width: {bar:?}");
    let toggle = bounds(cx, "navigator-toggle");
    assert!(f32::from(toggle.left()) >= titlebar::LEADING_INSET, "past the traffic lights");
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
        v.layout.move_column_to_workspace_down();
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
