//! No status bar: the panes run to the window's foot, and what the bar held stands in the
//! window's cluster (the tab's terminal toggle, a plan far used) or nowhere.

use gpui::Modifiers;
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use slopty_proto::thread::{AgentId, Cursor, Limit, ThreadId};

use super::*;
use crate::workspace::cluster::{HIDE_TERMINAL, SHOW_TERMINAL};

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
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

/// The window has no status bar: nothing is drawn along its foot, the panes run down to it, and
/// the one control the bar held, the tab's terminal toggle, ends the top-right tab bar in the
/// window's cluster. A phone has neither.
#[gpui::test]
fn the_window_has_no_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert!(cx.debug_bounds("foot").is_none(), "no status bar");
    let window = cx.update(|window, _| window.viewport_size());
    let item = cx.debug_bounds(selector("item", shell.item)).expect("the shell's tile");
    assert!(
        (f32::from(item.bottom()) - f32::from(window.height)).abs() < 0.5,
        "the pane runs to the window's foot: {item:?}"
    );
    let cluster = cx.debug_bounds("cluster").expect("the window's cluster");
    let toggle = cx.debug_bounds("tab-terminal").expect("the tab's terminal toggle");
    assert!(cluster.contains(&toggle.center()), "in the cluster: {toggle:?} {cluster:?}");
    assert!(toggle.top() < px(Theme::default().density.header), "on the top line");

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("foot").is_none(), "a phone has none either");
    assert!(cx.debug_bounds("tab-terminal").is_none(), "nor the toggle");
}

/// The plan's usage says nothing while every window of it is under 80 % used; once one is past,
/// the cluster says it in `warn`, and a click lists every machine's readings under the top line.
/// A machine whose agents published none shows no meter.
#[gpui::test]
fn the_plan_speaks_in_the_cluster_once_far_used(cx: &mut TestAppContext) {
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
    assert!(cx.debug_bounds("readout-plan").is_none(), "nothing to warn of");
    publish(cx, 2, 8_200);
    let cluster = cx.debug_bounds("cluster").expect("the cluster");
    let plan = cx.debug_bounds("readout-plan").expect("far used, said");
    assert!(cluster.contains(&plan.center()), "in the cluster");
    assert!(labels(&view, cx).iter().any(|l| l == "Plan usage 5h 23% · 7d 82%"));

    click(cx, "readout-plan");
    let plans = cx.debug_bounds("plans").expect("every reading, listed");
    let row = "studio · Claude Code, 5h 23% · 7d 82% · now".to_owned();
    assert!(labels(&view, cx).contains(&row), "{:?}", labels(&view, cx));
    assert!(plans.top() >= cluster.bottom(), "under the top line: {plans:?} {cluster:?}");
    assert!(plans.right() <= cluster.right() + px(0.5), "its right edge on the cluster's");

    view.update_in(cx, |v, _w, cx| v.focus_tile(there, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("readout-plan").is_none(), "the laptop's agents said nothing");
}

/// The cluster's toggle shows and hides the tab's terminal, and says which.
#[gpui::test]
fn the_cluster_toggles_the_tabs_terminal(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _here = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    assert!(labels(&view, cx).iter().any(|l| l == SHOW_TERMINAL));
    click(cx, "tab-terminal");
    let asked = view.read_with(cx, |v, _| {
        v.workers.values().any(|w| w.openings.iter().any(|(_, o)| *o == tabs::Opening::Terminal))
    });
    assert!(asked, "the tab's terminal is asked for");
    let terminal = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    assert!(view.read_with(cx, |v, _| v.layout.on_show(terminal)), "below the tab");
    assert!(labels(&view, cx).iter().any(|l| l == HIDE_TERMINAL), "the toggle says it shows");
    click(cx, "tab-terminal");
    assert!(!view.read_with(cx, |v, _| v.layout.on_show(terminal)), "put away");
    assert!(labels(&view, cx).iter().any(|l| l == SHOW_TERMINAL));
}
