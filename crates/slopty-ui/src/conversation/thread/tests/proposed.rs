//! What a request's card puts before the person to judge: an edit's change under its file,
//! cut to its head until asked for all of it, and a plan whole in a well that scrolls.

use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, point, px};
use slopty_core::WallMs;
use slopty_proto::thread::detail::{EditDetail, Hunk};
use slopty_proto::thread::{
    Clipped, Item, ItemBody, ItemId, Patch, Request, ToolCall, ToolDetail, ToolState, TurnId, kind,
};

use super::{approval, hub, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;

/// A patch adding `added` lines to a file of one line.
fn patch(added: u32) -> Patch {
    let mut lines = vec![" fn main() {".to_owned()];
    lines.extend((0..added).map(|n| format!("+    step({n});")));
    let hunk = Hunk {
        old_start: 1,
        old_lines: 1,
        new_start: 1,
        new_lines: added.saturating_add(1),
        heading: None,
        lines,
    };
    Patch { hunks: vec![hunk], added, removed: 0, clipped_lines: 0, full: None }
}

/// The call the transcript holds while its hook asks: an edit of `src/main.rs`, under way.
fn edit_call(patch: &Patch) -> Item {
    let detail = ToolDetail::Edit(EditDetail {
        path: "/w/src/main.rs".to_owned(),
        edits: 1,
        replace_all: false,
        patch: patch.clone(),
    });
    Item {
        id: ItemId("e".to_owned()),
        turn: TurnId(0),
        at_ms: WallMs::ZERO,
        body: ItemBody::Tool(Box::new(ToolCall {
            name: "Edit".to_owned(),
            kind: kind::EDIT.to_owned(),
            title: "Edit src/main.rs".to_owned(),
            input: Clipped::default(),
            state: ToolState::Running,
            output: None,
            images: Vec::new(),
            detail: Some(detail),
            child: None,
            ended_ms: None,
        })),
    }
}

/// An edit put to the person by a hook that names no call shows the change it would make on
/// its card, under the file it names (found by the transcript's call carrying the same
/// change), not a bare "Allow Edit?".
#[gpui::test]
fn an_edit_s_approval_shows_its_file_and_its_change(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let change = patch(3);
    state.items = vec![edit_call(&change)];
    let mut asks = approval("a");
    asks.title = "Allow Edit?".to_owned();
    asks.proposed = Some(change);
    state.requests = vec![asks];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let card = cx.debug_bounds("request-a").expect("the request's card");
    let diff = cx.debug_bounds("patch-ask-a").expect("the change, drawn");
    assert!(card.contains(&diff.center()), "on the card: {card:?} {diff:?}");
    assert!(cx.debug_bounds("proposed-file-a").is_some(), "under its file's name");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Group", Some("Change to src/main.rs"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Link", Some("Open /w/src/main.rs"))), "{tree:#?}");
    assert!(cx.debug_bounds("whole-ask-a").is_none(), "a short change shows whole");
    assert!(cx.debug_bounds("answer-a-allow").is_some(), "with its answers under it");
}

/// A long change on a request's card shows whole in a well at most 0.4 of the window that
/// scrolls, the answers under it on screen. With no call to name its file, the change shows.
#[gpui::test]
fn a_long_change_scrolls_in_its_well(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut asks = approval("a");
    asks.proposed = Some(patch(30));
    state.requests = vec![asks];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("proposed-file-a").is_none(), "no call names its file");
    assert!(cx.debug_bounds("whole-ask-a").is_none(), "nothing held back on a request");
    let well = cx.debug_bounds("proposed-a").expect("the change's well");
    assert!(well.size.height <= px(240.0) + px(1.0), "at most 0.4 of the window: {well:?}");
    let all = cx.debug_bounds("patch-ask-a").expect("the change");
    assert!(all.size.height > well.size.height, "it scrolls: {well:?} {all:?}");
    let allow = cx.debug_bounds("answer-a-allow").expect("the answers");
    assert!(allow.bottom() <= px(600.0), "on screen: {allow:?}");
}

/// A long change scrolls under its file's line, which stays where it is: what is being
/// approved is named however far down the change is read.
#[gpui::test]
fn a_long_change_scrolls_under_its_file(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let change = patch(30);
    state.items = vec![edit_call(&change)];
    let mut asks = approval("a");
    asks.proposed = Some(change);
    state.requests = vec![asks];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let file = cx.debug_bounds("proposed-file-a").expect("its file's line");
    let diff = cx.debug_bounds("patch-ask-a").expect("the change");
    let scroller = cx.debug_bounds("proposed-scroll-a").expect("the change's scroller");
    assert!(file.bottom() <= scroller.top(), "the line over what scrolls: {file:?} {scroller:?}");
    cx.simulate_event(ScrollWheelEvent {
        position: scroller.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        momentum_phase: None,
    });
    cx.run_until_parked();
    let moved = cx.debug_bounds("patch-ask-a").expect("the change");
    assert!(moved.top() < diff.top(), "the change scrolled: {diff:?} {moved:?}");
    assert_eq!(cx.debug_bounds("proposed-file-a"), Some(file), "the file's line stayed");
}

/// An opened call's long diff shows its head and "Show all N lines"; a press shows the rest.
#[gpui::test]
fn a_call_s_long_diff_shows_its_head_until_asked(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut call = edit_call(&patch(30));
    if let ItemBody::Tool(c) = &mut call.body {
        c.state = ToolState::Completed;
    }
    state.items = vec![call];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let line = cx.debug_bounds("tool-e").expect("the call's line").center();
    cx.simulate_click(line, Modifiers::none());
    cx.run_until_parked();
    let head = cx.debug_bounds("patch-e").expect("the diff's head");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Button", Some("Show all 31 lines"))), "{tree:#?}");
    let more = cx.debug_bounds("whole-e").expect("the way to all of it").center();
    cx.simulate_click(more, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("whole-e").is_none(), "all of it shows");
    let all = cx.debug_bounds("patch-e").expect("the diff");
    assert!(all.size.height > head.size.height * 2.0, "longer than its head: {head:?} {all:?}");
}

/// An MCP tool's opened call shows what it was called with, laid out to read.
#[gpui::test]
fn an_mcp_call_shows_what_it_was_called_with(cx: &mut TestAppContext) {
    use slopty_proto::thread::detail::McpDetail;
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let mut call = edit_call(&patch(1));
    call.id = ItemId("m".to_owned());
    if let ItemBody::Tool(c) = &mut call.body {
        c.state = ToolState::Completed;
        c.kind = kind::MCP.to_owned();
        c.input = Clipped::whole(r#"{"query":"gpui"}"#);
        c.output = Some(Clipped::whole("3 results"));
        c.detail = Some(ToolDetail::Mcp(McpDetail {
            server: "docs".to_owned(),
            tool: "search_docs".to_owned(),
        }));
    }
    state.items = vec![call];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("call-input-m").is_none(), "folded under its line");
    let line = cx.debug_bounds("tool-m").expect("the call's line").center();
    cx.simulate_click(line, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("call-input-m").is_some(), "what it was called with");
}

/// A plan put to the person with no card of its own reads whole in a well, from its first
/// line, never its last twelve; a long one scrolls inside at most 0.4 of the window.
#[gpui::test]
fn a_plan_with_no_card_reads_whole_in_a_well(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let words = (1..=40).map(|n| format!("{n}. Do step {n}")).collect::<Vec<_>>().join("\n");
    let mut asks = approval("p");
    asks.kind = Request::PLAN.to_owned();
    asks.title = "Approve the plan?".to_owned();
    asks.text = Some(Clipped::whole(&words));
    state.requests = vec![asks];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let well = cx.debug_bounds("request-plan-p").expect("the plan, in its well");
    assert!(well.size.height <= px(240.0) + px(1.0), "at most 0.4 of the window: {well:?}");
    assert!(well.size.height > px(120.0), "and room to read it: {well:?}");
    assert!(cx.debug_bounds("answer-p-allow").is_some(), "the answers stay on screen");
}
