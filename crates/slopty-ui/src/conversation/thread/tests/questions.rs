//! An agent's questions answered in the bar, the way back to the agent's own prompt, and the
//! session's move to and from the agent's own TUI.

use gpui::{Modifiers, TestAppContext, VisualTestContext, px};
use slopty_core::SessionId;
use slopty_proto::thread::detail::{Offered, Question};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AgentId, AskId, Cap, Choice, Drive, Effect, Request, RequestState};

use super::{hub, intents, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;

fn question(text: &str, header: Option<&str>, options: &[(&str, &str)], multi: bool) -> Question {
    Question {
        text: text.to_owned(),
        header: header.map(str::to_owned),
        options: options
            .iter()
            .map(|(label, means)| Offered {
                label: (*label).to_owned(),
                description: (!means.is_empty()).then(|| (*means).to_owned()),
            })
            .collect(),
        multi_select: multi,
    }
}

fn asking(id: &str, options: Vec<Choice>, questions: Vec<Question>) -> Request {
    Request {
        id: AskId(id.to_owned()),
        item: None,
        kind: Request::QUESTION.to_owned(),
        title: questions.first().map(|q| q.text.clone()).unwrap_or_default(),
        text: None,
        options,
        questions,
        proposed: None,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: slopty_core::WallMs::ZERO,
        until_ms: None,
    }
}

/// Claude Code's `AskUserQuestion` as its adapter maps it: a single-choice question with what
/// each answer means, then a multi-choice one, and Claude's own "Deny" beside them. The
/// questions take the keyboard from the empty composer: a digit takes an answer (and goes on,
/// for a single choice), ↓ walks to the field for an answer of one's own, ⌘↵ answers. One
/// answer goes, the answers keyed by each question's text, and the card flips to them. The
/// agent's prompt runs in a terminal, so the request can be taken back there.
#[gpui::test]
fn questions_are_answered_from_the_keyboard(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.terminal = Some(SessionId::new());
    state.meta.caps = vec![Cap::named(Cap::APPROVALS)];
    let deny = Choice {
        id: "deny".to_owned(),
        label: "Deny".to_owned(),
        effect: Effect::Deny,
        scope: None,
        stops: false,
    };
    state.requests = vec![asking(
        "q",
        vec![deny],
        vec![
            question(
                "Which layout?",
                Some("Layout"),
                &[("Split", "Side by side"), ("Stacked", "One over the other")],
                false,
            ),
            question("Which panes?", Some("Panes"), &[("Files", ""), ("Terminal", "")], true),
        ],
    )];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("request-q").is_some(), "the questions are on show");
    let form = view.read_with(cx, |v, _| v.questionnaire()).expect("a questionnaire");
    assert!(
        cx.update(|window, cx| form.read(cx).focus_handle().contains_focused(window, cx)),
        "it took the keyboard from the empty composer"
    );
    assert!(cx.debug_bounds("answer-q-deny").is_some(), "with the agent's own answer");
    assert!(cx.debug_bounds("release-q").is_some(), "and the way back to its terminal");

    // ⌘↵ goes on at once; a newer gpui-kit also goes on by itself a moment after the pick.
    cx.simulate_keystrokes("2 cmd-enter");
    cx.simulate_keystrokes("1 down down");
    cx.simulate_input("Logs");
    assert!(intents(&sent).is_empty(), "nothing goes before the last question is answered");
    cx.simulate_keystrokes("cmd-enter");

    let answers = r#"[{"question":"Which layout?","answer":"Stacked"},{"question":"Which panes?","answer":"Files, Logs"}]"#;
    assert_eq!(
        intents(&sent),
        [Intent::Answer { ask: AskId("q".to_owned()), choice: answers.to_owned(), message: None }]
    );
    assert!(cx.debug_bounds("request-q").is_none(), "the card flipped in that frame");
    assert!(cx.debug_bounds("answered-q").is_some(), "its answers show in its place");
}

/// As `MonoCode`'s question form: Home and End go to a question's first answer and its last;
/// going on needs an answer, so ⌘↵ on none stays; Skip goes on without one; a question that
/// takes several answers says so. The question skipped is answered with nothing, and the
/// answered line says it was skipped.
#[gpui::test]
fn a_question_is_walked_by_home_and_end_and_skipped(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::APPROVALS)];
    state.requests = vec![asking(
        "q",
        Vec::new(),
        vec![
            question("Which base?", None, &[("main", ""), ("next", ""), ("release", "")], false),
            question("Which checks?", None, &[("Lint", ""), ("Tests", "")], true),
        ],
    )];
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let form = view.read_with(cx, |v, _| v.questionnaire()).expect("a questionnaire");
    let focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            form.read(cx).focused_current_choice(window).map(ToString::to_string)
        })
    };
    assert_eq!(focused(cx).as_deref(), Some("0"), "the first answer has the keyboard");
    assert!(cx.debug_bounds("thread-question-several").is_none(), "one answer to pick");
    cx.simulate_keystrokes("end");
    assert_eq!(focused(cx).as_deref(), Some("2"), "End: the last answer");
    cx.simulate_keystrokes("home");
    assert_eq!(focused(cx).as_deref(), Some("0"), "Home: the first");

    cx.simulate_keystrokes("cmd-enter");
    let current = |cx: &mut VisualTestContext| {
        form.read_with(cx, |f, _| f.current_item().map(ToString::to_string))
    };
    assert_eq!(current(cx).as_deref(), Some("0"), "no answer, so nothing goes on");
    assert_eq!(intents(&sent), [], "and nothing went");

    let skip: &'static str =
        Box::leak(format!("questionnaire-{}-Skip", form.entity_id()).into_boxed_str());
    let at = cx.debug_bounds(skip).expect("Skip").center();
    cx.simulate_click(at, Modifiers::none());
    assert_eq!(current(cx).as_deref(), Some("1"), "Skip went on");
    assert!(cx.debug_bounds("thread-question-several").is_some(), "Select all that apply");
    cx.simulate_keystrokes("2 cmd-enter");

    let answers =
        r#"[{"question":"Which base?","answer":""},{"question":"Which checks?","answer":"Tests"}]"#;
    assert_eq!(
        intents(&sent),
        [Intent::Answer { ask: AskId("q".to_owned()), choice: answers.to_owned(), message: None }]
    );
    let said = crate::conversation::thread::questions::words(
        &[
            question("Which base?", None, &[("main", "")], false),
            question("Which checks?", None, &[("Tests", "")], true),
        ],
        answers,
    );
    assert_eq!(said, "Skipped; Tests");
}

/// pi's text dialog: one question that offers nothing is a field, focused, whose words are
/// the answer as they are. Slopty drives pi with no prompt of its own, so nothing offers to
/// answer in a terminal.
#[gpui::test]
fn a_question_that_offers_nothing_takes_words(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.agent = AgentId::named(AgentId::PI);
    state.meta.drive = Drive::named(Drive::DRIVEN);
    state.meta.caps = vec![Cap::named(Cap::APPROVALS)];
    state.requests =
        vec![asking("t", Vec::new(), vec![question("Name the branch", None, &[], false)])];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("release-t").is_none(), "no terminal to answer in");

    cx.simulate_input("fix-build");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&sent),
        [Intent::Answer {
            ask: AskId("t".to_owned()),
            choice: "fix-build".to_owned(),
            message: None
        }]
    );
}

/// A question is a card of its own after the turns, as Delta's are: in the thread's rows,
/// brought into view as it comes, the activity bar over the composer keeping only the plan;
/// the composer stands whole under them.
#[gpui::test]
fn a_question_is_a_card_in_the_thread(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Clipped, Item, ItemBody, ItemId, Plan, Step, TurnId};

    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.items = vec![Item {
        id: ItemId("t".to_owned()),
        turn: TurnId(1),
        at_ms: slopty_core::WallMs::ZERO,
        body: ItemBody::Text(Clipped::whole(&"An answer to read.\n\n".repeat(60))),
    }];
    let step =
        |n: usize| Step { id: None, text: format!("Step {n}"), status: "pending".to_owned() };
    state.plan = Some(Plan { text: None, steps: (1..=12).map(step).collect() });
    let options: Vec<(&str, &str)> = vec![
        ("Request extensions", "Kept on the request, never on the wire twice"),
        ("A header copy", "Copied header by header"),
    ];
    state.requests = vec![asking(
        "q",
        Vec::new(),
        vec![question("Where should the key live?", Some("Key"), &options, false)],
    )];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let bounds = |cx: &mut VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} is drawn"))
    };
    let (whole, rows) = (bounds(cx, "thread"), bounds(cx, "thread-rows"));
    let (tray, composer) = (bounds(cx, "thread-tray"), bounds(cx, "thread-composer"));
    let request = bounds(cx, "request-q");
    assert!(
        rows.top() <= request.top() + px(0.5) && request.bottom() <= rows.bottom() + px(0.5),
        "the question is in the rows, in view: {request:?} in {rows:?}"
    );
    assert!(request.bottom() <= tray.top() + px(0.5), "nothing of it over the composer");
    assert!(cx.debug_bounds("thread-activity-rest").is_some(), "the bar keeps the plan");
    assert!(tray.bottom() <= composer.top() + px(0.5), "the bar is over the composer");
    assert!(composer.bottom() <= whole.bottom() + px(0.5), "which stands whole");
}

/// One question with two answers that say what they mean, the agent's own "Deny", and a
/// terminal to answer in: as tall as Claude Code's `AskUserQuestion` gets with one question.
fn one_question() -> (slopty_proto::thread::ThreadState, slopty_proto::thread::ThreadId) {
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.terminal = Some(SessionId::new());
    state.meta.caps = vec![Cap::named(Cap::APPROVALS)];
    let deny = Choice {
        id: "deny".to_owned(),
        label: "Deny".to_owned(),
        effect: Effect::Deny,
        scope: None,
        stops: false,
    };
    let options =
        [("Split", "Old and new side by side"), ("Unified", "One column, changes inline")];
    state.requests = vec![asking(
        "q",
        vec![deny],
        vec![question("Which layout should the review use?", Some("Layout"), &options, false)],
    )];
    (state, thread)
}

/// Where the room is short (a phone with its keyboard up, a small window, a large text size),
/// the question's card scrolls with the thread; its "Submit" is the questionnaire's, not the
/// composer's under it, and sends what was typed as one's own answer.
#[gpui::test]
fn a_short_room_scrolls_the_question_with_the_thread(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let (state, thread) = one_question();
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    cx.simulate_resize(gpui::size(px(402.0), px(330.0)));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let form = view.read_with(cx, |v, _| v.questionnaire()).expect("a questionnaire");
    let submit: &'static str =
        Box::leak(format!("questionnaire-{}-Submit", form.entity_id()).into_boxed_str());
    cx.simulate_keystrokes("down down");
    cx.simulate_input("Unified, split for renames");
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();

    let rows = cx.debug_bounds("thread-rows").expect("the rows");
    let card = cx.debug_bounds("request-q").expect("the card");
    let composer = cx.debug_bounds("thread-composer").expect("the composer");
    // Taller than the room, the card shows from its foot up, the thread following its end.
    assert!(
        card.bottom() > rows.top() && card.top() < rows.bottom(),
        "the card is in the rows: {card:?} {rows:?}"
    );
    assert!(rows.bottom() <= composer.top() + px(0.5), "the rows stand over the composer");
    // The thread scrolls to the card's foot, where its answers are.
    for _ in 0..20 {
        let at = cx.debug_bounds(submit);
        if at.is_some_and(|at| at.bottom() <= rows.bottom() + px(0.5)) {
            break;
        }
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: rows.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-60.0))),
            ..Default::default()
        });
        cx.run_until_parked();
    }
    let at = cx.debug_bounds(submit).expect("Submit is drawn");
    assert!(at.bottom() <= rows.bottom() + px(0.5), "Submit scrolls into view: {at:?}");
    cx.simulate_click(at.center(), Modifiers::none());
    let sent = intents(&sent);
    assert!(
        matches!(sent.as_slice(), [Intent::Answer { choice, .. }] if choice.contains("Unified, split for renames")),
        "Submit answered: {sent:?}"
    );
}

/// The question's card is as tall in a window of 600 points as in one of 900, its row of
/// answers under the question: the room changes the thread's scroll, not the card.
#[gpui::test]
fn with_room_the_question_stands_whole(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let (state, thread) = one_question();
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let card = cx.debug_bounds("request-q").expect("the card");
    let question = cx.debug_bounds("thread-question").expect("the question");
    cx.simulate_resize(gpui::size(px(800.0), px(900.0)));
    cx.run_until_parked();
    let tall = cx.debug_bounds("request-q").expect("the card");
    let tall_question = cx.debug_bounds("thread-question").expect("the question");
    assert!(
        (card.size.height - tall.size.height).abs() < px(0.5)
            && (question.size.height - tall_question.size.height).abs() < px(0.5),
        "as tall at 600 as at 900: {card:?} against {tall:?}"
    );
}
