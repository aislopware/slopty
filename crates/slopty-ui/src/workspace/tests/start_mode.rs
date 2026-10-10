//! A start names the mode it will begin in: the one the machine says a start that asks for none
//! begins in (`Offers::mode`), which the start then leaves to the agent.

use gpui::Modifiers;
use slopty_core::WorkerId;
use slopty_proto::thread::wire::{Start, ThreadRequest};
use slopty_proto::thread::{AgentId, Mode, Offers};

use super::super::actions::NewAgent;
use super::super::projects::worker_key;
use super::*;

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    tree(cx).into_iter().filter_map(|n| n.label).collect()
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    settle(cx);
}

fn starts(fake: &mut Fake) -> Vec<Start> {
    fake.drain()
        .into_iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => Some(*start),
            _ => None,
        })
        .collect()
}

/// Claude Code's offers as 2.1.295 makes them on a machine whose settings choose no mode.
fn claude_offers() -> Offers {
    let mode = |id: &str, label: &str| Mode {
        id: id.to_owned(),
        label: label.to_owned(),
        description: None,
    };
    Offers {
        modes: vec![mode("auto", "Auto"), mode("manual", "Manual"), mode("plan", "Plan")],
        mode: Some("auto".to_owned()),
        ..Offers::default()
    }
}

/// A Claude Code start with no mode chosen names Auto, the mode the machine says it begins
/// in, and asks for none, so the folder's own settings still decide. A mode switched to goes
/// with the start.
#[gpui::test]
fn a_start_names_the_mode_it_begins_in(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let mut studio = connect(&view, cx, worker_key(WorkerId::new()).value(), "studio");
    let shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/src/app"));
    let caps = WorkerCaps {
        agents: vec![slopty_proto::server::InstalledAgent {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            version: "2.1.295 (Claude Code)".to_owned(),
            offers: claude_offers(),
            managed_hooks_off: false,
        }],
        ..healthy()
    };
    view.update_in(cx, |v, _w, cx| {
        v.set_worker_caps(studio.key, caps, cx);
        v.focus_tile(shell, cx);
    });
    settle(cx);
    let begin = |cx: &mut VisualTestContext| {
        cx.dispatch_action(NewAgent);
        settle(cx);
        // The one agent and the one machine pass by; the folder step takes the shell's.
        cx.simulate_keystrokes("enter");
        settle(cx);
    };

    begin(cx);
    assert!(
        labels(&view, cx).iter().any(|l| l == "Mode, Auto"),
        "the chip names the mode it begins in: {:?}",
        labels(&view, cx)
    );
    cx.simulate_input("Read the parser.");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [start] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(start.mode, None, "the start asks for no mode");

    begin(cx);
    click("thread-attach", cx);
    click("thread-add-menu-modes", cx);
    click("thread-menu-2", cx);
    assert!(labels(&view, cx).iter().any(|l| l == "Mode, Plan"), "switched");
    cx.simulate_input("Plan the parser.");
    cx.simulate_keystrokes("enter");
    settle(cx);
    let sent = starts(&mut studio);
    let [start] = sent.as_slice() else { panic!("one start: {sent:?}") };
    assert_eq!(start.mode.as_deref(), Some("plan"), "a chosen mode goes with it");
}
