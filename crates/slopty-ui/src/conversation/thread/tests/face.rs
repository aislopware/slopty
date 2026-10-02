//! The thread's face: a request answered where its call is, the composer's chips and its one
//! solid, and the way down to the newest row.

use gpui::{
    Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point,
    px,
};
use slopty_core::WallMs;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    AskId, BackgroundTask, Cap, Changed, Clipped, Compaction, Item, ItemBody, ItemId, Model,
    Notice, Phase, Retry, ToolCall, ToolState, Turn, TurnId, TurnState, Usage, kind,
};

use super::{approval, hub, intents, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;

fn live_turn() -> Turn {
    Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Active,
        started_ms: WallMs::from_millis(1_000),
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

fn item(id: &str, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(1), at_ms: WallMs::ZERO, body }
}

fn scroll(cx: &mut VisualTestContext, dy: f32) {
    let at = cx.debug_bounds("thread").expect("drawn").center();
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
        momentum_phase: None,
    });
    cx.run_until_parked();
}

/// A request whose call is on screen is answered on the call's card, and the tray carries no
/// copy; while the call is scrolled away the tray carries it with the way back to the call,
/// which brings the answers back onto the card.
#[gpui::test]
fn a_request_is_answered_on_its_call_while_the_call_shows(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.status.phase = Phase::NeedsYou;
    state.turns = vec![live_turn()];
    let mut asked = approval("a");
    asked.item = Some(ItemId("x".to_owned()));
    state.requests = vec![asked];
    state.items = vec![
        item(
            "x",
            ItemBody::Tool(Box::new(ToolCall {
                name: "Bash".to_owned(),
                kind: kind::EXEC.to_owned(),
                title: "cargo test".to_owned(),
                input: Clipped::default(),
                state: ToolState::Pending { ask: AskId("a".to_owned()) },
                output: None,
                images: Vec::new(),
                detail: None,
                child: None,
                ended_ms: None,
            })),
        ),
        // Taller than the view, so the newest row's place puts the call above it.
        item("t", ItemBody::Text(Clipped::whole(&"A long answer.\n\n".repeat(120)))),
    ];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    assert!(cx.debug_bounds("request-a").is_some(), "the call is above: the tray carries it");
    let back = cx.debug_bounds("asked-scroll").expect("with the way back to the call").center();
    cx.simulate_click(back, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("request-a").is_none(), "the call shows: the tray has no copy");
    let card = cx.debug_bounds("call-card-x").expect("the call is a card");
    let allow = cx.debug_bounds("answer-a-allow").expect("answered on the card");
    assert!(card.contains(&allow.center()), "the answers sit on the call's card");

    cx.simulate_click(allow.center(), Modifiers::none());
    assert!(
        matches!(intents(&sent).as_slice(), [Intent::Answer { choice, .. }] if choice == "allow"),
        "{:?}",
        intents(&sent)
    );
}

/// The model chip opens the agent's models, and picking one asks the agent to switch.
#[gpui::test]
fn the_model_chip_switches_the_agent_s_model(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::SET_MODEL)];
    state.meta.models = vec![
        Model { id: "opus".to_owned(), label: "Opus".to_owned() },
        Model { id: "sonnet".to_owned(), label: "Sonnet".to_owned() },
    ];
    state.meters.model = Some("Opus".to_owned());
    state.meters.model_id = Some("opus".to_owned());
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    let chip = cx.debug_bounds("thread-model").expect("the model chip").center();
    cx.simulate_click(chip, Modifiers::none());
    assert!(cx.debug_bounds("thread-menu").is_some(), "the models are listed");
    let sonnet = cx.debug_bounds("thread-menu-1").expect("both of them").center();
    cx.simulate_click(sonnet, Modifiers::none());
    assert_eq!(intents(&sent), [Intent::SetModel { model: "sonnet".to_owned() }]);
    assert!(cx.debug_bounds("thread-menu").is_none(), "picking closes the menu");
}

/// The composer's one solid stops the turn while it runs and nothing is typed, and sends what
/// is typed.
#[gpui::test]
fn the_one_solid_stops_a_turn_or_sends_the_draft(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)];
    state.status.phase = Phase::Working;
    state.turns = vec![live_turn()];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    assert!(cx.debug_bounds("thread-stop").is_some(), "at work, nothing typed: stop");
    assert!(cx.debug_bounds("thread-send").is_none());
    cx.simulate_input("And the docs");
    let send = cx.debug_bounds("thread-send").expect("typed: send").center();
    cx.simulate_click(send, Modifiers::none());
    assert!(
        matches!(intents(&sent).as_slice(), [Intent::Send { text, .. }] if text == "And the docs"),
        "{:?}",
        intents(&sent)
    );
}

/// Scrolled up from the newest row, a round way down shows over the list's foot, and takes
/// the list back to following the newest row.
#[gpui::test]
fn a_thread_scrolled_up_offers_the_way_down(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::long(6, 4);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-down").is_none(), "at the newest row, no way down");

    scroll(cx, 400.0);
    assert!(!view.read_with(cx, |v, _| v.following()), "scrolled up");
    let down = cx.debug_bounds("thread-down").expect("the way down shows").center();
    cx.simulate_click(down, Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.following()), "following the newest row again");
}

/// Every step open on a long thread, the wheel thrown hard both ways, frame after frame: the
/// view reads the list's scroll only once a frame is drawn, never from inside the list.
#[gpui::test]
fn a_long_open_thread_survives_hard_scrolling(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::long(12, 6);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-o ctrl-o");
    cx.run_until_parked();
    for dy in [2_000.0, 6_000.0, -3_000.0, 9_000.0, -12_000.0, 600.0] {
        scroll(cx, dy);
    }
    let at_end = view.read_with(cx, |v, _| v.following());
    assert_eq!(cx.debug_bounds("thread-down").is_some(), !at_end, "the way down tells the truth");
}

/// At the overview's small zoom the composer, its field's words included, shrinks with the
/// rest of the thread rather than keeping its full size.
#[gpui::test]
fn the_composer_shrinks_with_the_zoom(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let tall = cx.debug_bounds("thread-composer").expect("the composer").size.height;
    view.update(cx, |v, cx| v.set_layout(0.25, 800.0, cx));
    cx.run_until_parked();
    let small = cx.debug_bounds("thread-composer").expect("the composer").size.height;
    assert!(small <= tall * 0.3, "{small:?} against {tall:?} at a quarter");
}

/// The agent's background work stays out of the way: a chip says how much runs, and opens
/// the panel of it, each piece with what it last printed.
#[gpui::test]
fn background_work_opens_from_a_chip(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.tasks = vec![BackgroundTask {
        id: "b1".to_owned(),
        kind: BackgroundTask::SHELL.to_owned(),
        title: "cargo build".to_owned(),
        state: BackgroundTask::RUNNING.to_owned(),
        item: None,
        output: Some(Clipped::whole("Compiling slopty-ui")),
        started_ms: WallMs::from_millis(1_000),
        ended_ms: None,
    }];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    assert!(cx.debug_bounds("task-b1").is_none(), "the panel waits to be asked");
    let chip = cx.debug_bounds("thread-tasks").expect("the chip says one runs").center();
    cx.simulate_click(chip, Modifiers::none());
    assert!(cx.debug_bounds("task-b1").is_some(), "the panel lists it");
}

/// The composer names how hard the model thinks and how far a Codex sandbox reaches, beside
/// the approval mode.
#[gpui::test]
fn the_composer_names_the_effort_and_the_sandbox(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meters.effort = Some("high".to_owned());
    state.meters.mode = Some("on-request".to_owned());
    state.meta.facts.insert("sandbox".to_owned(), "workspaceWrite".to_owned());
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-effort").is_some(), "the effort chip");
    assert!(cx.debug_bounds("thread-mode").is_some(), "the mode chip");
}

/// A retried failure says when the agent tries again; a compaction opens on its summary.
#[gpui::test]
fn a_retry_and_a_compaction_say_what_they_hold(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![live_turn()];
    state.items = vec![
        item(
            "n",
            ItemBody::Notice(Notice {
                kind: Notice::API_ERROR.to_owned(),
                text: Clipped::whole("Overloaded"),
                retry: Some(Retry { attempt: 2, max: Some(10), in_ms: Some(3_000) }),
            }),
        ),
        item(
            "c",
            ItemBody::Compaction(Compaction {
                trigger: None,
                before_tokens: Some(120_000),
                after_tokens: Some(18_000),
                summary: Some(Clipped::whole("The build passes; the docs wait.")),
            }),
        ),
    ];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("retry-n").is_some(), "the retry under its notice");
    assert!(cx.debug_bounds("summary-c").is_none(), "the summary waits to be asked");
    let note = cx.debug_bounds("note-c").expect("the compaction").center();
    cx.simulate_click(note, Modifiers::none());
    assert!(cx.debug_bounds("summary-c").is_some(), "the summary opens under it");
}

/// While the agent tries a failed request again, the working line says so.
#[gpui::test]
fn a_turn_that_retries_says_so(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.status.phase = Phase::Working;
    state.turns = vec![live_turn()];
    state.items = vec![item(
        "n",
        ItemBody::Notice(Notice {
            kind: Notice::API_ERROR.to_owned(),
            text: Clipped::whole("Overloaded"),
            retry: Some(Retry { attempt: 2, max: None, in_ms: Some(3_000) }),
        }),
    )];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-retrying").is_some(), "retrying, not just working");
}
