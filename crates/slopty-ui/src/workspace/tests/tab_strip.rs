//! The frame's top in the headless workspace: the navigator the window's height with the
//! title bar from its edge, the breadcrumb's way between projects, and the project's tabs.

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

/// Docked, the navigator runs from the window's top and the title bar starts at its right
/// edge. The navigator's top row holds the lights and, at its end, back, forward and the toggle
/// (`MonoCode`'s rail header), and the filter is the first row under it. Hidden, the bar takes
/// the whole width and starts past the traffic lights, with the toggle first in it.
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
    assert!(nav.right() - docked.right() < px(16.0), "at the row's end: {docked:?}");
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
    let back = bounds(cx, "go-back");
    assert!(toggle.right() <= back.left(), "the toggle leads the bar: {toggle:?} {back:?}");
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
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
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

/// The title bar runs the tabs of the project on show, each named by its focused work with a
/// mark for each agent in it that needs the person; a press shows a tab, and its close closes
/// what it holds.
#[gpui::test]
fn the_title_bar_shows_the_projects_tabs_and_their_agents(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let session = SessionId::new();
    let other = opens(&view, cx, &studio, session, ClientId::new(), 2);
    let tabs = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            v.layout()
                .shown_project()
                .map(|p| p.tabs().iter().map(slopty_client::layout::Tab::id).collect::<Vec<_>>())
        })
        .unwrap_or_default()
    };
    let ids = tabs(cx);
    assert_eq!(ids.len(), 2, "its own tab, and the one from elsewhere");
    let n = |i: usize| ids[i].get();
    let sel = |what: &str, i: usize| -> &'static str {
        Box::leak(format!("{what}-{}", n(i)).into_boxed_str())
    };
    assert!(cx.debug_bounds(sel("title-tab", 0)).is_some(), "drawn in the bar");
    assert!(cx.debug_bounds(sel("title-tab", 1)).is_some());
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    let mark: &'static str = Box::leak(format!("title-tab-mark-{}-0", n(1)).into_boxed_str());
    assert!(cx.debug_bounds(mark).is_some(), "its agent needs the person");
    click(cx, sel("title-tab", 1));
    assert_eq!(focused(&view, cx), Some(other), "a press shows the tab");
    click(cx, sel("title-tab-close", 1));
    assert!(!view.read_with(cx, |v, _| v.layout().contains(other)), "closed with its tab");
    assert_eq!(tabs(cx).len(), 1);
    assert_eq!(focused(&view, cx), Some(first));
}

/// The title bar lies on the ground with nothing of its own but the line along its foot. The tab
/// on show is a selected pill the density's tab tall at radius 6, with air above and below it;
/// a tab not on show draws no fill at rest.
#[gpui::test]
fn the_shown_title_tab_is_a_pill_on_the_ground(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let _other = opens(&view, cx, &studio, SessionId::new(), ClientId::new(), 2);
    let ids: Vec<u64> = view.read_with(cx, |v, _| {
        v.layout()
            .shown_project()
            .map(|p| p.tabs().iter().map(|t| t.id().get()).collect())
            .unwrap_or_default()
    });
    let shown = view.read_with(cx, |v, _| v.layout().shown_tab().map(|t| t.id().get()));
    let (on, off) = match ids.as_slice() {
        [a, b] if Some(*a) == shown => (*a, *b),
        [a, b] => (*b, *a),
        _ => panic!("two tabs: {ids:?}"),
    };
    let bounds = |cx: &mut VisualTestContext, what: String| {
        let selector: &'static str = Box::leak(what.into_boxed_str());
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let bar = bounds(cx, "titlebar".to_owned());
    let tab = bounds(cx, format!("title-tab-{on}"));
    let theme_tab = px(Theme::default().density.tab);
    assert_eq!(tab.size.height, theme_tab, "a pill the density's tab tall: {tab:?}");
    assert!(tab.top() > bar.top() && tab.bottom() < bar.bottom(), "air round it: {tab:?} {bar:?}");
    let theme = Theme::default();
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let at = |b: Bounds<Pixels>, q: &gpui::Quad| {
        let near = |a: f32, p: Pixels| f32::from(p).mul_add(-scale, a).abs() < 1.0;
        near(q.bounds.origin.x.0, b.origin.x) && near(q.bounds.size.width.0, b.size.width)
    };
    let selected = crate::colors::hsla(theme.surfaces.selected);
    let fill = |q: &gpui::Quad| {
        (!q.background.is_transparent()).then(|| q.background.as_solid()).flatten()
    };
    assert!(
        quads.iter().any(|q| at(tab, q)
            && fill(q) == Some(selected)
            && q.corner_radii.top_left.0 / scale - theme.radii.sm < 0.5),
        "the shown tab a selected pill"
    );
    let rest = bounds(cx, format!("title-tab-{off}"));
    assert!(!quads.iter().any(|q| at(rest, q) && fill(q).is_some()), "a tab not shown is bare");
    // The bar paints nothing of its own over the ground but the one line along its foot.
    let band = |q: &gpui::Quad| {
        let top = q.bounds.origin.y.0 / scale;
        top < f32::from(bar.bottom()) - 0.5
            && q.bounds.size.height.0 / scale > f32::from(crate::kit::HAIR)
    };
    assert!(!quads.iter().any(|q| at(bar, q) && band(q) && fill(q).is_some()), "on the ground");
    // Each tab's words are set on the chrome's line, not the window's 16 pt one.
    let line = px(theme.roles().chrome.line);
    for n in [on, off] {
        let text = bounds(cx, format!("title-tab-text-{n}"));
        assert_eq!(text.size.height, line, "a title tab's words on the chrome's line");
    }
}

/// A title tab that closes folds its width away over `Pace::Sheet` in its place, bare and with
/// no control, and is gone after; under Reduce Motion it is gone at once.
#[gpui::test]
fn a_closed_title_tab_folds_away(cx: &mut TestAppContext) {
    use crate::kit::Pace;
    use crate::workspace::title_tabs::TitleTabsHost as _;

    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(Duration::ZERO));
    });
    let studio = connect(&view, cx, 1, "studio");
    let _first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    for n in 2..=4_u64 {
        let tile = opens(&view, cx, &studio, SessionId::new(), studio.me, n);
        on_new_tab(&view, cx, tile);
    }
    let tabs = |cx: &VisualTestContext| {
        view.read_with(cx, |v, _| v.title_tabs().into_iter().map(|t| t.id).collect::<Vec<_>>())
    };
    let close = |cx: &mut VisualTestContext, at: usize| {
        let id = tabs(cx)[at];
        let width = cx
            .debug_bounds(Box::leak(format!("title-tab-{}", id.get()).into_boxed_str()))
            .expect("the tab is drawn")
            .size
            .width;
        view.update_in(cx, |v, window, cx| v.close_title_tab(id, window, cx));
        cx.run_until_parked();
        width
    };
    let step = |cx: &mut VisualTestContext, at: Duration| {
        view.update(cx, |v, _| v.hold_clock(Some(at)));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    };
    let width = close(cx, 1);
    let ghost = cx.debug_bounds("title-tab-closing").expect("it folds away");
    assert_eq!(ghost.size.width, width, "from the width it had");
    let next = cx
        .debug_bounds(Box::leak(format!("title-tab-{}", tabs(cx)[1].get()).into_boxed_str()))
        .expect("the tab after it");
    assert!(ghost.right() <= next.left() + px(0.5), "in its place, before the next");
    step(cx, Pace::Sheet.duration().checked_div(2).unwrap_or_default());
    let half = cx.debug_bounds("title-tab-closing").expect("still folding").size.width;
    assert!(half < width && half > px(0.0), "narrowing: {half:?} of {width:?}");
    step(cx, Pace::Sheet.duration());
    assert!(cx.debug_bounds("title-tab-closing").is_none(), "gone once folded");

    cx.update(|_, cx| cx.set_reduce_motion(true));
    let _width = close(cx, 1);
    assert!(cx.debug_bounds("title-tab-closing").is_none(), "gone at once under Reduce Motion");
}
