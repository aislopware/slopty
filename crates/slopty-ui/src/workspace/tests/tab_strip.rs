//! The frame's top in the headless workspace: the navigator the window's height with the
//! title bar from its edge, and the workspaces as tabs.

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

/// Each workspace with something in it is a tab, the active one too when empty. A tab is one
/// width whatever it holds, switching tabs moves none, and "+" opens a new workspace. A tab's
/// slot rolls up what its tiles want; its name carries no chord.
#[gpui::test]
fn the_workspaces_are_tabs_in_the_title_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(first, _), _, _] = three_shells(&view, cx, &studio);
    assert!(cx.debug_bounds("ws-tab-0").is_some());
    assert!(cx.debug_bounds("ws-tab-1").is_none(), "the empty workspace below has no tab");

    click(cx, "new-workspace");
    assert_eq!(active(&view, cx), 1, "+ goes to a new, empty workspace");
    let (zero, one) = (
        cx.debug_bounds("ws-tab-0").expect("drawn"),
        cx.debug_bounds("ws-tab-1").expect("the active one has a tab while empty"),
    );
    assert_eq!(zero.size, one.size, "one width");
    click(cx, "ws-tab-0");
    assert_eq!(active(&view, cx), 0);
    assert!(cx.debug_bounds("ws-tab-1").is_none(), "left empty, it has no tab");

    // A column moved down makes a second workspace worth a tab; switching moves no tab.
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_column_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    let before = (cx.debug_bounds("ws-tab-0").unwrap(), cx.debug_bounds("ws-tab-1").unwrap());
    click(cx, "ws-tab-0");
    let after = (cx.debug_bounds("ws-tab-0").unwrap(), cx.debug_bounds("ws-tab-1").unwrap());
    assert_eq!(before, after, "switching moves no tab");

    assert!(cx.debug_bounds("ws-rollup-0").is_none(), "at rest, an empty slot");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(first), cx));
    cx.run_until_parked();
    let first_ws =
        view.read_with(cx, |v, _| v.layout().position(v.tile_of_session(first).unwrap()).unwrap());
    let rollup = if first_ws.workspace == 0 { "ws-rollup-0" } else { "ws-rollup-1" };
    assert!(cx.debug_bounds(rollup).is_some(), "the warn mark on its workspace's tab");
    let after = (cx.debug_bounds("ws-tab-0").unwrap(), cx.debug_bounds("ws-tab-1").unwrap());
    assert_eq!(before, after, "the mark takes the tab's own slot");

    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let tabs: Vec<String> =
        tree.into_iter().filter_map(|n| n.label).filter(|l| l.starts_with("Workspace ")).collect();
    assert!(tabs.iter().any(|l| l.ends_with(", 1 needs you")), "{tabs:#?}");
    assert!(tabs.iter().all(|l| !l.contains(['⌘', '⌥', '⌃'])), "no chords on tabs: {tabs:#?}");
}
