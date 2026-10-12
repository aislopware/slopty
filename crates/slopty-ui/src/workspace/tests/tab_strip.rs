//! The frame's top in the headless workspace: the navigator the window's height, its header on
//! the top line with the tab bars, the breadcrumb's way between projects, and no title tabs.

use gpui::Modifiers;

use super::*;

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn shown(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> Option<usize> {
    view.read_with(cx, |v, _| v.layout().shown_index())
}

/// Docked, the navigator runs from the window's top to its foot, and its header row meets the
/// first tab bar on one line: the lights' room, then the toggle, then the filter's way in. The
/// tab bar starts at the navigator's edge with back and forward. Hidden, the tab bar takes the
/// whole width and starts past the traffic lights, the toggle first in it.
#[gpui::test]
fn the_navigator_is_the_windows_height_and_the_bar_starts_at_its_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let (nav, bar) = (bounds(cx, "navigator"), bounds(cx, selector("title", shell.item)));
    assert!(f32::from(nav.top()).abs() < 0.5, "from the top: {nav:?}");
    assert!((f32::from(nav.size.height) - VIEWPORT.1).abs() < 0.5, "to the bottom: {nav:?}");
    assert_eq!(bar.left(), nav.right(), "the bar starts at its edge");
    let header = bounds(cx, "nav-header");
    assert_eq!(header.bottom(), bar.bottom(), "one line: {header:?} {bar:?}");
    let docked = bounds(cx, "navigator-toggle");
    assert!(header.contains(&docked.center()), "the toggle is in the header: {docked:?}");
    assert!(f32::from(docked.left()) >= titlebar::LEADING_INSET, "past the traffic lights");
    let search = bounds(cx, "nav-search");
    assert!(docked.right() <= search.left(), "the filter's way in after it: {search:?}");
    let back = bounds(cx, "go-back");
    assert!(bar.contains(&back.center()), "back leads the tab bar: {back:?} {bar:?}");
    click(cx, "nav-search");
    let field = bounds(cx, "nav-filter-field");
    assert!(header.contains(&field.center()), "the filter in the header row: {field:?}");
    cx.simulate_keystrokes("escape");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let bar = bounds(cx, selector("title", shell.item));
    assert!(f32::from(bar.left()).abs() < 0.5, "the whole width: {bar:?}");
    let toggle = bounds(cx, "navigator-toggle");
    assert!(f32::from(toggle.left()) >= titlebar::LEADING_INSET, "past the traffic lights");
    assert!(bar.contains(&toggle.center()), "in the tab bar: {toggle:?}");
    let back = bounds(cx, "go-back");
    assert!(toggle.right() <= back.left(), "the toggle leads the bar: {toggle:?} {back:?}");
}

/// One tab layer: the window draws no title tabs, docked or hidden, alone or with several
/// tabs. A project's tabs are its rows in the navigator, and a pane's tiles its tab bar's cells.
#[gpui::test]
fn no_title_tab_is_drawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    on_new_tab(&view, cx, second);
    let tabs = view.read_with(cx, |v, _| v.tab_lines());
    assert_eq!(tabs.len(), 2, "two tabs");
    for _ in 0..2 {
        for tab in &tabs {
            let id = tab.id.get();
            let title = Box::leak(format!("title-tab-{id}").into_boxed_str());
            assert!(cx.debug_bounds(title).is_none(), "no title tab {id}");
        }
        assert!(cx.debug_bounds("title-tabs").is_none(), "no strip of them");
        assert!(cx.debug_bounds("titlebar").is_none(), "no title bar over the panes");
        // The tab on show's one tile is one pane tab; the other tab's tile is not drawn.
        assert!(cx.debug_bounds(selector("tab", second.item)).is_some(), "the shown one's cell");
        assert!(cx.debug_bounds(selector("tab", first.item)).is_none(), "the other tab's not");
        cx.simulate_keystrokes("cmd-b");
        cx.run_until_parked();
    }
}

/// The breadcrumb's project segment is how the bar goes between projects: its menu lists each
/// one (the one on show ticked), and a row shows it. What waits in another project is the
/// bell's, so the segment says only the project's name; no chord is spelled on it.
#[gpui::test]
fn the_breadcrumb_goes_between_projects(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    // From elsewhere: a background tab in the laptop's own project.
    let session = SessionId::new();
    let theirs = opens(&view, cx, &laptop, session, ClientId::new(), 1);
    let here = shown(&view, cx);
    assert_eq!(focused(&view, cx), Some(mine));
    // The breadcrumb stands in the bar while the navigator is hidden.
    assert!(cx.debug_bounds("crumb-project").is_none(), "the navigator names the project");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("crumb-project").is_some());
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| slopty_kit::a11y::tree(window));
    let crumbs: Vec<String> =
        tree.into_iter().filter(|n| n.role == "Button").filter_map(|n| n.label).collect();
    assert!(crumbs.iter().any(|l| l == "studio"), "the project alone: {crumbs:#?}");
    assert!(crumbs.iter().all(|l| !l.contains(['⌘', '⌥', '⌃'])), "no chords: {crumbs:#?}");

    click(cx, "crumb-project");
    assert!(cx.debug_bounds("menu-studio").is_some(), "the one on show");
    click(cx, "menu-laptop");
    assert_ne!(shown(&view, cx), here, "a row shows its project");
    assert_eq!(focused(&view, cx), Some(theirs), "on the tab it was left on");
    assert!(cx.debug_bounds("menu").is_none(), "and closes the menu");
}

/// A project is named after its home, its machine's name where it has no project of its own,
/// and the name the person gives wins; the window is called by it.
#[gpui::test]
fn a_project_is_named_after_its_home(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let name = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.project_name());
    let _home = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert_eq!(name(cx), "studio", "a home shell's project is its machine's");
    let home = view.read_with(cx, |v, _| v.layout().shown_project().map(|p| p.home().clone()));
    view.update(cx, |v, cx| {
        v.layout.set_name(&home.expect("a project"), Some("release".to_owned()));
        cx.notify();
    });
    assert_eq!(name(cx), "release", "a given name wins");
    cx.run_until_parked();
    let titled = tree(cx).into_iter().any(|n| n.is("Window", Some("release")));
    assert!(titled, "the window is called by the project on show, not the app's name");
}
