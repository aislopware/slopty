//! The thread's face: a request answered where its call is, the composer's chips and its one
//! solid, the way down to the newest row, and an answer's new words lifting in.

use gpui::{
    Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, point,
    px,
};
use gpui_kit::component::text::TextViewState;
use slopty_core::WallMs;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    AskId, BackgroundTask, Cap, Changed, Clipped, Compaction, Item, ItemBody, ItemId, Model,
    Notice, Phase, Request, Retry, ThreadState, ToolCall, ToolDetail, ToolState, Turn, TurnId,
    TurnState, Usage, kind,
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

/// A call that asks as it arrives at the foot of the thread is answered on its card in the
/// very frame that first shows it: no frame carries a copy in the tray that the list's layout
/// then takes back, so nothing jumps.
#[gpui::test]
fn a_call_that_asks_as_it_arrives_is_answered_on_its_card_at_once(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.status.phase = Phase::Working;
    state.turns = vec![live_turn()];
    state.items = vec![item("t", ItemBody::Text(Clipped::whole("Let me run it.")))];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();

    state.status.phase = Phase::NeedsYou;
    let mut asked = approval("a");
    asked.item = Some(ItemId("x".to_owned()));
    state.requests = vec![asked];
    state.items.push(item(
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
    ));
    let moved = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.marks_moved());
    let before = moved(cx);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert_eq!(moved(cx), before, "the first frame put the answers on the card");
    assert!(cx.debug_bounds("request-a").is_none(), "no copy in the tray");
    let card = cx.debug_bounds("call-card-x").expect("the call is a card");
    let allow = cx.debug_bounds("answer-a-allow").expect("answered on the card");
    assert!(card.contains(&allow.center()), "the answers sit on the call's card");
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

/// The mode chip names the mode by the agent's own name for it and opens the modes it
/// publishes; picking one asks the agent to switch. With no modes published it is a label.
#[gpui::test]
fn the_mode_chip_switches_the_agent_s_mode(cx: &mut TestAppContext) {
    use slopty_proto::thread::Mode;

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::SET_MODE)];
    let mode = |id: &str, label: &str| Mode {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(format!("{label} things")),
    };
    state.meta.modes = vec![mode("ask", "Ask"), mode("code", "Code")];
    state.meters.mode = Some("ask".to_owned());
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();

    let chip = cx.debug_bounds("thread-mode").expect("the mode chip").center();
    cx.simulate_click(chip, Modifiers::none());
    assert!(cx.debug_bounds("thread-menu").is_some(), "the modes are listed");
    let code = cx.debug_bounds("thread-menu-1").expect("both of them").center();
    cx.simulate_click(code, Modifiers::none());
    assert_eq!(intents(&sent), [Intent::SetMode { mode: "code".to_owned() }]);
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
/// the panel of it under its own head. Once nothing runs the chip says how it ended, never
/// that it is still in the background.
#[gpui::test]
fn background_work_opens_from_a_chip(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let task = |id: &str, state: &str| BackgroundTask {
        id: id.to_owned(),
        kind: BackgroundTask::SHELL.to_owned(),
        title: "cargo build".to_owned(),
        state: state.to_owned(),
        item: None,
        output: Some(Clipped::whole("Compiling slopty-ui")),
        started_ms: WallMs::from_millis(1_000),
        ended_ms: None,
    };
    state.tasks = vec![task("b1", BackgroundTask::RUNNING)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    let labelled = |cx: &mut VisualTestContext, label: &str| {
        cx.update(|window, _cx| crate::a11y::tree(window))
            .iter()
            .any(|n| n.label.as_deref() == Some(label))
    };

    assert!(cx.debug_bounds("task-b1").is_none(), "the panel waits to be asked");
    assert!(labelled(cx, "1 running"));
    let chip = cx.debug_bounds("thread-tasks").expect("the chip says one runs").center();
    cx.simulate_click(chip, Modifiers::none());
    assert!(cx.debug_bounds("task-b1").is_some(), "the panel lists it");
    assert!(labelled(cx, "In the background, 1 running"), "under its own head");
    assert!(labelled(cx, "cargo build: Running"), "what it printed stays off its line");

    state.tasks = vec![task("b1", BackgroundTask::COMPLETED), task("b2", BackgroundTask::FAILED)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(labelled(cx, "1 finished \u{b7} 1 failed"), "how it ended");
    let head = cx.debug_bounds("thread-tasks-head").expect("the head").center();
    cx.simulate_click(head, Modifiers::none());
    assert!(cx.debug_bounds("task-b1").is_none(), "the head folds it");
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

/// How often the thread's Markdown repaints over the next few fade ticks, nothing else
/// moving: only text lifting in repaints on a timer.
fn lift_ticks(cx: &mut VisualTestContext, repaints: &std::cell::Cell<usize>) -> usize {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    let before = repaints.get();
    for _ in 0..3 {
        cx.executor().advance_clock(std::time::Duration::from_millis(40));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    repaints.get().saturating_sub(before)
}

/// The words the latest turn's answer gains lift in as they arrive, paced to the stream, and
/// an earlier turn's do not; under Reduce Motion they land at once.
#[gpui::test]
fn the_latest_answer_s_new_words_lift_in(cx: &mut TestAppContext) {
    let repaints = std::rc::Rc::new(std::cell::Cell::new(0_usize));
    let counted = std::rc::Rc::clone(&repaints);
    cx.update(|cx| {
        cx.observe_new(move |_: &mut TextViewState, _, cx| {
            let counted = std::rc::Rc::clone(&counted);
            cx.observe_self(move |_, _| counted.set(counted.get().saturating_add(1))).detach();
        })
        .detach();
    });
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    let settled = Turn {
        state: TurnState::Complete,
        ended_ms: Some(WallMs::from_millis(2_000)),
        ..live_turn()
    };
    state.turns = vec![settled, Turn { id: TurnId(2), ..live_turn() }];
    let answer = |id: &str, turn: u32, text: &str| Item {
        turn: TurnId(turn),
        ..item(id, ItemBody::Text(Clipped::whole(text)))
    };
    state.items = vec![answer("t1", 1, "Earlier"), answer("t2", 2, "Reading")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    let mut seq = 1_u64;
    let mut show = |state: &ThreadState, cx: &mut VisualTestContext| {
        seq = seq.saturating_add(1);
        hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), seq), cx));
        cx.run_until_parked();
    };
    show(&state, cx);
    assert_eq!(lift_ticks(cx, &repaints), 0, "at rest nothing repaints");

    state.items[0] = answer("t1", 1, "Earlier, and a correction of it");
    show(&state, cx);
    assert_eq!(lift_ticks(cx, &repaints), 0, "an earlier turn's words land at once");

    state.items[1] = answer("t2", 2, "Reading the parser and its tests");
    show(&state, cx);
    assert!(lift_ticks(cx, &repaints) >= 3, "the latest answer's lift on the fade's ticks");

    cx.update(|_, cx| cx.set_reduce_motion(true));
    cx.update(|window, _| window.refresh());
    lift_ticks(cx, &repaints);
    state.items[1] = answer("t2", 2, "Reading the parser and its tests, then the lexer");
    show(&state, cx);
    assert_eq!(lift_ticks(cx, &repaints), 0, "under Reduce Motion they land at once");
}

/// A plan put to the person is a card in the thread, its title from its heading, its head
/// shown until opened, the answers on it; scrolled away, the tray says "Plan ready" with the
/// way back to it and no second copy of its words.
#[gpui::test]
fn a_plan_is_a_card_that_takes_its_answer(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.status.phase = Phase::NeedsYou;
    state.turns = vec![live_turn()];
    let steps = (1..=30).map(|n| format!("{n}. Step {n}")).collect::<Vec<_>>().join("\n");
    let words = format!("# Split the parser\n\n{steps}");
    let mut asked = approval("p");
    asked.kind = Request::PLAN.to_owned();
    asked.item = Some(ItemId("plan".to_owned()));
    asked.text = Some(Clipped::whole(&words));
    state.requests = vec![asked];
    state.items = vec![item(
        "plan",
        ItemBody::Tool(Box::new(ToolCall {
            name: "ExitPlanMode".to_owned(),
            kind: kind::PLAN.to_owned(),
            title: "Propose a plan".to_owned(),
            input: Clipped::default(),
            state: ToolState::Pending { ask: AskId("p".to_owned()) },
            output: None,
            images: Vec::new(),
            detail: Some(ToolDetail::Plan { text: Clipped::whole(&words) }),
            child: None,
            ended_ms: None,
        })),
    )];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.is("Article", Some("Plan: Split the parser, Awaiting approval"))),
        "named by its heading and how it stands"
    );
    let card = cx.debug_bounds("plan-plan").expect("a card");
    let short = card.size.height;
    let more = cx.debug_bounds("plan-more-plan").expect("a long plan shows its head");
    cx.simulate_click(more.center(), Modifiers::none());
    let opened = cx.debug_bounds("plan-plan").expect("still a card").size.height;
    assert!(opened > short, "opened, the whole plan: {short:?} then {opened:?}");
    assert!(cx.debug_bounds("plan-more-plan").is_none());
    assert!(cx.debug_bounds("request-p").is_none(), "answered on the card");
    let allow = cx.debug_bounds("answer-p-allow").expect("the answers on the card");
    assert!(cx.debug_bounds("plan-plan").expect("drawn").contains(&allow.center()));

    // Taller than the view, so the newest row's place puts the plan above it.
    state.items.push(Item {
        turn: TurnId(1),
        ..item("t", ItemBody::Text(Clipped::whole(&"More words.\n\n".repeat(120))))
    });
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    scroll(cx, -100_000.0);
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(cx.debug_bounds("request-p").is_some(), "the tray carries it while it is away");
    assert!(tree.iter().any(|n| n.label.as_deref() == Some("Plan ready")), "named, not copied");
    assert!(!tree.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("1. Step 1"))));
    let allow = cx.debug_bounds("answer-p-allow").expect("answered from the tray");
    cx.simulate_click(allow.center(), Modifiers::none());
    assert!(
        matches!(intents(&sent).as_slice(), [Intent::Answer { choice, .. }] if choice == "allow"),
        "{:?}",
        intents(&sent)
    );
}

/// A picture sent with a message opens large over the thread on a press, saying what it is;
/// Esc closes it, from the composer or outside it, and never also stops the turn under way.
/// A press anywhere on it closes it too.
#[gpui::test]
fn a_picture_opens_large_and_esc_closes_it(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.status.phase = Phase::Working;
    let picture = slopty_proto::thread::Image {
        digest: "d1".to_owned(),
        media_type: "image/png".to_owned(),
        bytes: 245_760,
        width: 1_600,
        height: 1_200,
        at: slopty_proto::thread::ContentRef("blob:d1".to_owned()),
    };
    state.turns = vec![live_turn()];
    state.items = vec![item(
        "u",
        ItemBody::User(slopty_proto::thread::UserMessage {
            text: Clipped::whole("Look at this"),
            images: vec![picture],
            command: None,
            intent: None,
        }),
    )];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let open = |cx: &mut VisualTestContext| {
        let at = cx.debug_bounds("picture-d1").expect("the picture").center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    };

    open(cx);
    assert!(cx.debug_bounds("picture-viewer").is_some(), "open large");
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter()
            .any(|n| n.label.as_deref()
                == Some("Picture, 1600 \u{d7} 1200 \u{b7} PNG \u{b7} 240 KB"))
    );
    cx.simulate_keystrokes("escape");
    assert!(cx.debug_bounds("picture-viewer").is_none(), "Esc closes it");
    open(cx);
    cx.dispatch_action(crate::conversation::Interrupt);
    cx.run_until_parked();
    assert!(cx.debug_bounds("picture-viewer").is_none(), "Esc outside the composer too");
    assert!(intents(&sent).is_empty(), "and the turn goes on: {:?}", intents(&sent));

    open(cx);
    let viewer = cx.debug_bounds("picture-viewer").expect("open again");
    cx.simulate_click(viewer.origin + point(px(4.0), px(4.0)), Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("picture-viewer").is_none(), "a press closes it");
}
