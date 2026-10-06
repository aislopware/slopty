//! The overlays in the headless workspace: what a waiting row says, what the palette reaches
//! and clears out of its way, and how a sheet nests its rows.

use gpui::Modifiers;
use slopty_core::WallMs;

use super::*;

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = bounds(cx, selector).center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

/// The labels a screen reader reads in the last frame.
fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    tree.into_iter().filter_map(|n| n.label).collect()
}

/// A row under *Needs you* says what its agent asks, never "Needs approval" under the heading
/// that already says it waits, and an agent the hook named only its tool for says what that tool
/// does, not the tool. Its first line ends in how long ago it began to wait, counted from the
/// worker's stamp so a reconnect keeps it.
#[gpui::test]
fn a_waiting_row_says_what_it_asks_and_since_when(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (asking, bare) = (SessionId::new(), SessionId::new());
    let _asking = opens(&view, cx, &studio, asking, studio.me, 1);
    let _bare = opens(&view, cx, &studio, bare, studio.me, 2);
    let _here = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    let since_ms = turns::wall_ms().saturating_sub(5 * 60_000);
    view.update_in(cx, |v, _w, cx| {
        let status = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".into() });
        let detail = Some("$ touch refused.txt".into());
        v.agent_event(
            AgentEvent {
                since_ms: WallMs::from_millis(since_ms),
                status: status.clone(),
                detail,
                ..blocked(asking)
            },
            cx,
        );
        v.agent_event(
            AgentEvent {
                since_ms: WallMs::from_millis(since_ms),
                status,
                detail: None,
                ..blocked(bare)
            },
            cx,
        );
    });
    cx.run_until_parked();

    let row = bounds(cx, leak(format!("nav-waiting-{asking}")));
    let ago = bounds(cx, leak(format!("nav-waiting-time-{asking}")));
    assert!(ago.center().y < row.center().y, "on the first line: {ago:?} in {row:?}");
    let names = labels(&view, cx);
    assert!(
        names.iter().any(|l| l.contains("Run touch refused.txt")),
        "what it asks, not \"Needs approval\": {names:#?}"
    );
    assert!(
        names.iter().any(|l| l.contains("Wants to run a command")),
        "a bare tool says what it asks, not \"Bash\": {names:#?}"
    );
}

/// On a phone the drawer and the palette never stack: opening the palette puts the drawer
/// away, since whatever the palette goes to is under it.
#[gpui::test]
fn opening_the_palette_puts_the_phone_drawer_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_some(), "the drawer is out");
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_some());
    assert!(cx.debug_bounds("navigator").is_none(), "and put away for the palette");
}

/// A dismissed palette is closed at once, since the keys go back where they were, but it is
/// drawn for its way out and only then dropped; under Reduce Motion it goes in the same frame.
#[gpui::test]
fn a_dismissed_palette_draws_its_way_out(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.palette_open()), "closed to the keys");
    assert!(cx.debug_bounds("palette").is_some(), "still drawing its way out");
    cx.executor().advance_clock(crate::kit::Pace::Fade.duration());
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none(), "dropped once out");

    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("palette").is_none(), "gone in the same frame");
}

/// What floats as a sheet nests its rows: a menu's and the palette's rows sit the
/// sheet's hairline and pad in from its edge, so a row's 6 pt corner shares the 12 pt sheet's
/// centre.
#[gpui::test]
fn a_sheets_rows_nest_in_its_corners(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let theme = Theme::default();
    // From the sheet's outer edge: its hairline and its pad, which with a row's radius make
    // the sheet's.
    let pad = crate::kit::sheet_pad(&theme) + slopty_theme::stroke::HAIR;
    assert!((pad + theme.radii.sm - theme.radii.lg).abs() < f32::EPSILON, "concentric");
    let inset = |cx: &mut VisualTestContext, sheet: &'static str, row: &'static str| {
        let (sheet, row) = (
            cx.debug_bounds(sheet).unwrap_or_else(|| panic!("{sheet}")),
            cx.debug_bounds(row).unwrap_or_else(|| panic!("{row}")),
        );
        f32::from(row.left() - sheet.left())
    };
    let click = |cx: &mut VisualTestContext, selector: &'static str| {
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector}"));
        cx.simulate_click(at.center(), gpui::Modifiers::default());
        cx.run_until_parked();
    };

    click(cx, "more");
    let at = inset(cx, "menu", "menu-Command palette");
    assert!((at - pad).abs() < 0.5, "a menu's row: {at}");
    click(cx, "more");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    let at = inset(cx, "palette", "palette-item-0");
    assert!((at - pad).abs() < 0.5, "the palette's row: {at}");
}

/// A bar menu closes on Esc, which the shell under it never gets, and on a second press of
/// its button.
#[gpui::test]
fn a_bar_menu_closes_on_escape_and_a_second_press(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    click(cx, "more");
    assert!(cx.debug_bounds("menu").is_some(), "open");
    fake.drain();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu").is_none(), "Esc closes it");
    let typed = fake
        .drain()
        .into_iter()
        .any(|m| matches!(m, ClientMsg::Term { req: TermRequest::Key(_), .. }));
    assert!(!typed, "the shell does not get the Esc");
    click(cx, "more");
    click(cx, "more");
    assert!(cx.debug_bounds("menu").is_none(), "a second press closes it");
}
