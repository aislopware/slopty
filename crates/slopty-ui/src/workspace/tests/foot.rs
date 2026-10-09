//! The foot bar along the window's foot: the plan's usage, the focused agent, what runs out of
//! sight, and the toggle of the tab's terminal.

use gpui::Modifiers;
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use slopty_proto::thread::{AgentId, Cursor, Limit, ThreadId};

use super::*;
use crate::workspace::foot::{HIDE_TERMINAL, SHOW_TERMINAL};

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    tree(cx).into_iter().filter_map(|n| n.label).collect()
}

/// A Claude Code thread with no terminal whose status line said `limits`.
fn plan_row(limits: Vec<Limit>) -> ThreadRow {
    let mut row = crate::conversation::thread::fixtures::thread("edit").row(WallMs::now());
    row.id = ThreadId::new();
    row.terminal = None;
    row.agent = AgentId(AgentId::CLAUDE_CODE.into());
    row.meters.limits = limits;
    row
}

fn window(name: &str, used_bp: u32) -> Limit {
    Limit { name: name.to_owned(), used_bp, resets_ms: None }
}

/// The bar runs along the window's foot under the panes, the status step tall, on the chrome
/// with a hairline over it, beside the navigator rather than under it. A phone has none.
#[gpui::test]
fn the_foot_bar_runs_along_the_windows_foot(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    assert!(cx.debug_bounds("foot").is_none(), "no bar before a machine is known");
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let theme = Theme::default();
    let foot = cx.debug_bounds("foot").expect("the foot bar");
    let window = cx.update(|window, _| window.viewport_size());
    assert_eq!(foot.bottom(), window.height, "at the window's foot");
    assert_eq!(foot.size.height, px(theme.density.status), "the status step tall");
    assert_eq!(foot.right(), window.width, "to the window's trailing edge");
    if let Some(nav) = cx.debug_bounds("navigator") {
        assert!(foot.left() >= nav.right() - px(1.0), "beside the navigator: {foot:?} {nav:?}");
    }
    let area = cx.debug_bounds("pane-area").or_else(|| cx.debug_bounds("workspace-area"));
    if let Some(area) = area {
        assert!(area.bottom() <= foot.top() + px(0.5), "the panes end over it");
    }
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let chrome = crate::colors::hsla(theme.surfaces.chrome);
    let near = |a: f32, b: Pixels| f32::from(b).mul_add(-scale, a).abs() < 1.0;
    assert!(
        quads.iter().any(|q| near(q.bounds.origin.y.0, foot.top())
            && near(q.bounds.size.height.0, foot.size.height)
            && q.background.as_solid() == Some(chrome)),
        "on the chrome"
    );

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("foot").is_none(), "a phone has no foot bar");
}

/// The plan's usage on the focused tile's machine is always said at the bar's start, in `warn`
/// once a window is 80 % used, and a click lists every machine's readings above the bar. A
/// machine whose agents published none shows no meter. The title bar says none of it.
#[gpui::test]
fn the_plans_usage_leads_the_foot_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let here = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let there = opens(&view, cx, &laptop, SessionId::new(), laptop.me, 1);
    let key = studio.key;
    let publish = |cx: &mut VisualTestContext, seq, seven_day| {
        let row = plan_row(vec![window("five-hour", 2_300), window("seven-day", seven_day)]);
        let table = TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, rows: vec![row] };
        view.update_in(cx, |v, _w, cx| {
            v.threads_linked(key, cx);
            v.thread_table(key, &table, cx);
            v.focus_tile(here, cx);
        });
        cx.run_until_parked();
    };
    publish(cx, 1, 4_100);
    let foot = cx.debug_bounds("foot").expect("the foot bar");
    let plan = cx.debug_bounds("readout-plan").expect("always said");
    assert!(foot.contains(&plan.center()), "in the foot bar");
    assert!(plan.left() - foot.left() < px(Theme::default().spacing.md), "at its start");
    assert!(labels(&view, cx).iter().any(|l| l == "Plan usage 5h 23% · 7d 41%"));
    assert!(cx.debug_bounds("readouts").is_none(), "the title bar says none of it");
    publish(cx, 2, 8_200);
    assert!(labels(&view, cx).iter().any(|l| l == "Plan usage 5h 23% · 7d 82%"));

    click(cx, "readout-plan");
    let plans = cx.debug_bounds("plans").expect("every reading, listed");
    let row = "studio · Claude Code, 5h 23% · 7d 82% · now".to_owned();
    assert!(labels(&view, cx).contains(&row), "{:?}", labels(&view, cx));
    assert!(plans.bottom() <= foot.top(), "above the bar: {plans:?} {foot:?}");
    assert!(plans.left() >= foot.left(), "from its start");

    view.update_in(cx, |v, _w, cx| v.focus_tile(there, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-plan").is_none(), "the laptop's agents said nothing");
}

/// A shell whose command runs out of sight has a chip, which shows it; one on show says it in
/// its own header and has none. The toggle shows and hides the tab's terminal, and says which.
#[gpui::test]
fn what_runs_out_of_sight_and_the_tabs_terminal_are_at_the_bars_end(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (first, second) = (SessionId::new(), SessionId::new());
    let away = opens(&view, cx, &fake, first, fake.me, 1);
    let here = opens(&view, cx, &fake, second, fake.me, 2);
    on_new_tab(&view, cx, here);
    view.update_in(cx, |v, _w, _cx| v.running_after = Duration::ZERO);
    for session in [first, second] {
        view.update_in(cx, |v, _w, cx| {
            let runs = marked_frame(
                2,
                &[
                    ("~ % make", SemanticMark::Prompt { exit: None, input: Some(4) }),
                    ("building", SemanticMark::Output),
                ],
                1,
            );
            v.term_event(session, runs, cx);
        });
    }
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let chip = leak(format!("foot-shell-{first}"));
    assert!(cx.debug_bounds(chip).is_some(), "the shell in the other tab has a chip");
    assert!(
        cx.debug_bounds(leak(format!("foot-shell-{second}"))).is_none(),
        "the one on show says it in its header"
    );
    click(cx, chip);
    assert_eq!(focused(&view, cx), Some(away), "the chip goes to it");
    assert!(cx.debug_bounds(chip).is_none(), "and, on show, has no chip");

    assert!(labels(&view, cx).iter().any(|l| l == SHOW_TERMINAL));
    click(cx, "foot-terminal");
    let asked = view.read_with(cx, |v, _| {
        v.workers.values().any(|w| w.openings.iter().any(|(_, o)| *o == tabs::Opening::Terminal))
    });
    assert!(asked, "the tab's terminal is asked for");
    let terminal = opens(&view, cx, &fake, SessionId::new(), fake.me, 3);
    assert!(view.read_with(cx, |v, _| v.layout.on_show(terminal)), "below the tab");
    assert!(labels(&view, cx).iter().any(|l| l == HIDE_TERMINAL), "the toggle says it shows");
    click(cx, "foot-terminal");
    assert!(!view.read_with(cx, |v, _| v.layout.on_show(terminal)), "put away");
    assert!(labels(&view, cx).iter().any(|l| l == SHOW_TERMINAL));
}

/// The focused tile's agent is named at the bar's start, after the plan, with how it is doing.
#[gpui::test]
fn the_focused_agent_is_named_in_the_foot_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let tile = opens(&view, cx, &fake, session, fake.me, 1);
    assert!(cx.debug_bounds("foot-agent").is_none(), "a plain shell names no agent");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(session), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let foot = cx.debug_bounds("foot").expect("the foot bar");
    let agent = cx.debug_bounds("foot-agent").expect("the agent");
    assert!(foot.contains(&agent.center()));
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "Claude Code, Needs you"), "{names:#?}");
}
