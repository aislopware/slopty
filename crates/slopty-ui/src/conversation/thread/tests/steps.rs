//! The steps of a thread beyond its words: a subagent's thread opened from its call and left
//! again, every step opened at once, the commands run in the background, and a message's
//! copy.

use gpui::{Modifiers, TestAppContext};
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::detail::{ExecDetail, ExecStatus};
use slopty_proto::thread::wire::{TableFrame, ThreadRequest};
use slopty_proto::thread::{
    Changed, Clipped, Cursor, Item, ItemBody, ItemId, Link, ThreadId, ToolCall, ToolDetail,
    ToolState, Turn, TurnId, TurnState, Usage, UserMessage, kind,
};

use super::{hub, snapshot, view};
use crate::conversation::CycleDensity;
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::rows::Row;
use crate::conversation::thread::view::ThreadView;

fn turn(id: u32, state: TurnState) -> Turn {
    let ended_ms = (!matches!(state, TurnState::Active)).then(|| WallMs::from_millis(5_000));
    Turn {
        id: TurnId(id),
        input: None,
        state,
        started_ms: WallMs::from_millis(1_000),
        ended_ms,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

fn item(id: &str, turn: u32, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
}

fn user(id: &str, turn: u32) -> Item {
    item(
        id,
        turn,
        ItemBody::User(UserMessage {
            text: Clipped::whole("Count the lines"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    )
}

fn call(
    id: &str,
    turn: u32,
    kind: &str,
    detail: Option<ToolDetail>,
    child: Option<ThreadId>,
) -> Item {
    item(
        id,
        turn,
        ItemBody::Tool(Box::new(ToolCall {
            name: kind.to_owned(),
            kind: kind.to_owned(),
            title: "Count lines".to_owned(),
            input: Clipped::default(),
            state: ToolState::Completed,
            output: Some(Clipped::whole("Compiling a\nCompiling b\n")),
            images: Vec::new(),
            detail,
            child,
            ended_ms: None,
        })),
    )
}

fn background(status: ExecStatus) -> ToolDetail {
    ToolDetail::Exec(ExecDetail {
        command: Clipped::whole("cargo build"),
        description: Some("Build it".to_owned()),
        cwd: None,
        background: true,
        task: Some("b1".to_owned()),
        status,
        exit_code: None,
        stderr: None,
        duration_ms: None,
    })
}

fn follows(sent: &super::Sent) -> Vec<ThreadRequest> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(
                r @ (ThreadRequest::Follow { .. } | ThreadRequest::Unfollow { .. }),
            ) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// A subagent's call opens its thread in the view, followed for it, under a bar that leads
/// back; the tile's own thread stays the view's, and Esc goes back and lets the subagent's go.
#[gpui::test]
fn a_subagent_s_call_opens_its_thread_and_esc_leads_back(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut parent = fixtures::empty();
    let mut child = fixtures::empty();
    child.meta.title = "Count lines".to_owned();
    child.meta.parent = Some(Link { thread: parent.meta.id, item: ItemId("agent".to_owned()) });
    let (main, sub) = (parent.meta.id, child.meta.id);
    parent.turns = vec![turn(1, TurnState::Active)];
    parent.items = vec![user("u", 1), call("agent", 1, kind::AGENT, None, Some(sub))];
    let rows = vec![parent.row(WallMs::ZERO), child.row(WallMs::ZERO)];
    hub.update(cx, ThreadHub::connected);
    hub.update(cx, |hub, cx| {
        hub.table(&TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows }, cx);
    });
    let (view, cx) = view(cx, &hub, main);
    hub.update(cx, |hub, cx| hub.frame(main, snapshot(parent, 2), cx));
    cx.run_until_parked();
    sent.borrow_mut().clear();

    let at = cx.debug_bounds("tool-agent").expect("the subagent's call").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(view.read_with(cx, |v, _| v.shown()), sub, "its thread on show");
    assert_eq!(view.read_with(cx, |v, _| v.thread()), main, "still the tile's thread view");
    assert!(cx.debug_bounds("thread-trail").is_some(), "the bar that leads back");
    assert!(cx.debug_bounds("thread-composer").is_none(), "a subagent takes no messages");
    assert!(
        follows(&sent)
            .iter()
            .any(|r| matches!(r, ThreadRequest::Follow { thread, .. } if *thread == sub)),
        "followed for it: {:?}",
        follows(&sent)
    );
    hub.update(cx, |hub, cx| hub.frame(sub, snapshot(child, 1), cx));
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(cx, |v, _| v.shown()), main, "back");
    assert!(cx.debug_bounds("thread-trail").is_none());
    assert!(follows(&sent).contains(&ThreadRequest::Unfollow { thread: sub }), "let go");
    assert!(cx.debug_bounds("tool-agent").is_some(), "drawn at once");
}

/// ⌃O opens every settled turn and each of its steps; again, they fold.
#[gpui::test]
fn control_o_opens_every_step_and_folds_them_again(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    state.items = vec![
        user("u1", 1),
        call("c1", 1, kind::READ, None, None),
        user("u2", 2),
        call("c2", 2, kind::READ, None, None),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    let tools = |view: &gpui::Entity<ThreadView>, cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |v, _| v.rows().iter().filter(|r| matches!(r, Row::Tool { .. })).count())
    };
    assert_eq!(tools(&view, cx), 0, "folded");
    cx.dispatch_action(CycleDensity);
    assert_eq!(tools(&view, cx), 2, "every step");
    cx.dispatch_action(CycleDensity);
    assert_eq!(tools(&view, cx), 0, "folded again");
}

/// A command run in the background shows over the composer with its last line while it runs,
/// and once it ended only for the turn it ended in.
#[gpui::test]
fn a_background_command_shows_while_it_runs(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Active)];
    state.items = vec![
        user("u1", 1),
        call("old", 1, kind::EXEC, Some(background(ExecStatus::Done)), None),
        user("u2", 2),
        call("build", 2, kind::EXEC, Some(background(ExecStatus::Running)), None),
    ];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("background-build").is_some(), "the running one");
    assert!(cx.debug_bounds("background-old").is_none(), "not one an earlier turn ended");
}

/// A message's copy puts its words on the clipboard.
#[gpui::test]
fn a_message_s_copy_puts_its_words_on_the_clipboard(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete)];
    state.items = vec![user("u", 1)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("copy-u").is_none(), "quiet until the pointer is on it");
    let message = cx.debug_bounds("item-u").expect("the message").center();
    cx.simulate_mouse_move(message, None, Modifiers::none());
    cx.run_until_parked();
    let copy = cx.debug_bounds("copy-u").expect("its copy").center();
    cx.simulate_click(copy, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()).as_deref(), Some("Count the lines"));
}

/// Two turns, each a question and its answer, at `width`.
fn two_turns(
    cx: &mut TestAppContext,
    width: f32,
) -> (gpui::Entity<ThreadView>, &mut gpui::VisualTestContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    state.items = vec![
        user("u1", 1),
        item("a1", 1, ItemBody::Text(Clipped::whole("Twelve."))),
        user("u2", 2),
        item("a2", 2, ItemBody::Text(Clipped::whole("Done."))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(600.0)));
    view.update(cx, |v, cx| v.set_layout(1.0, width, cx));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    (view, cx)
}

/// Turns group: an answer follows its question at the small step and the next question opens
/// at the large one, as the person's actions wait beside the bubble's foot instead of keeping a
/// hidden line under it.
#[gpui::test]
fn a_question_and_its_answer_read_as_one_turn(cx: &mut TestAppContext) {
    let (_view, cx) = two_turns(cx, 800.0);
    let spacing = slopty_theme::Theme::default().spacing;
    let bounds = |cx: &mut gpui::VisualTestContext, id: &'static str| {
        cx.debug_bounds(id).unwrap_or_else(|| panic!("{id} drawn"))
    };
    let (u1, a1, u2) = (bounds(cx, "item-u1"), bounds(cx, "item-a1"), bounds(cx, "item-u2"));
    let within = f32::from(a1.top() - u1.bottom());
    let between = f32::from(u2.top() - a1.bottom());
    assert!((within - spacing.sm).abs() < 0.5, "the answer at the small step: {within}");
    assert!(between >= spacing.lg - 0.5, "the next question at the large one: {between}");

    cx.simulate_mouse_move(u1.center(), None, Modifiers::none());
    cx.run_until_parked();
    let copy = bounds(cx, "copy-u1");
    assert!(copy.top() >= u1.top() && copy.bottom() <= u1.bottom(), "beside the bubble {copy:?}");
}

/// The column stands off the tile by its room's gutter: wide, regular and narrow.
#[gpui::test]
fn the_column_s_gutter_follows_the_tile_s_room(cx: &mut TestAppContext) {
    let spacing = slopty_theme::Theme::default().spacing;
    let (view, cx) = two_turns(cx, 800.0);
    for (width, gutter) in [(800.0, spacing.xxxl), (600.0, spacing.xl), (360.0, spacing.lg)] {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(600.0)));
        view.update(cx, |v, cx| v.set_layout(1.0, width, cx));
        cx.run_until_parked();
        let answer = cx.debug_bounds("item-a1").expect("the answer");
        let left = f32::from(answer.left());
        assert!((left - gutter).abs() < 0.5, "{width}: the gutter {gutter}, at {left}");
    }
}

/// A message the person sent into a running turn stands between two folds, each saying what
/// its stretch of the work did; the turn's time stands on the last. Either opens the turn.
#[gpui::test]
fn a_steer_stands_between_the_folds_of_its_turn(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![turn(1, TurnState::Complete)];
    state.items = vec![
        user("u", 1),
        call("x", 1, kind::EXEC, None, None),
        user("s", 1),
        call("r", 1, kind::READ, None, None),
        item("a", 1, ItemBody::Text(Clipped::whole("Done."))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    for drawn in ["fold-1-0", "item-s", "fold-1"] {
        assert!(cx.debug_bounds(drawn).is_some(), "{drawn}");
    }
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Button", Some("Ran a command"))), "{tree:#?}");
    assert!(tree.iter().any(|n| n.is("Button", Some("Worked 4 s: Read a file"))), "{tree:#?}");
    let at = cx.debug_bounds("fold-1-0").expect("the first fold").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let tools = view
        .read_with(cx, |v, _| v.rows().iter().filter(|r| matches!(r, Row::Tool { .. })).count());
    assert_eq!(tools, 2, "the turn opens as one");
}
