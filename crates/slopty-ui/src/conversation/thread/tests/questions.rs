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
        editable: Vec::new(),
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

/// A thread that can move to the agent's own TUI offers to while Slopty drives it; once the
/// TUI holds it, the composer gives way to where it is and the way to take it back.
#[gpui::test]
fn a_session_goes_to_the_agents_tui_and_is_taken_back(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.agent = AgentId::named(AgentId::PI);
    state.meta.drive = Drive::named(Drive::DRIVEN);
    state.meta.caps = vec![Cap::named(Cap::HANDOFF), Cap::named(Cap::INTERRUPT)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-held").is_none());
    let handoff = cx.debug_bounds("thread-handoff").expect("offered while Slopty drives");
    cx.simulate_click(handoff.center(), Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Handoff]);

    state.meta.drive = Drive::named(Drive::OBSERVED);
    state.meta.terminal = Some(SessionId::new());
    state.meta.caps = vec![Cap::named(Cap::HANDOFF)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-composer").is_none(), "the TUI takes the messages");
    assert!(cx.debug_bounds("thread-handoff").is_none());
    let take = cx.debug_bounds("thread-take-back").expect("the way back");
    cx.simulate_click(take.center(), Modifiers::none());
    assert_eq!(intents(&sent), [Intent::Handoff, Intent::TakeBack]);
}

/// However much waits in the tray, it stands below the rows and never over them. A request
/// stands whole, what the person must answer; the plan under it keeps to a line or two and
/// scrolls; the composer stands whole under the tray. A click above the tray's edge is the
/// rows', not the tray's.
#[gpui::test]
fn a_request_stands_whole_and_the_rest_of_the_tray_gives_way(cx: &mut TestAppContext) {
    use slopty_proto::thread::{Clipped, Item, ItemBody, ItemId, Plan, Step, TurnId};

    let (hub, sent) = hub(cx, None);
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
    let plan = cx.debug_bounds("thread-plan").expect("the plan in the tray").center();
    cx.simulate_click(plan, Modifiers::none());
    cx.run_until_parked();

    let bounds = |cx: &mut VisualTestContext, s: &'static str| {
        cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} is drawn"))
    };
    let (whole, rows) = (bounds(cx, "thread"), bounds(cx, "thread-rows"));
    let (tray, composer) = (bounds(cx, "thread-tray"), bounds(cx, "thread-composer"));
    let (request, rest) = (bounds(cx, "request-q"), bounds(cx, "thread-activity-rest"));
    assert!(rows.size.height > px(0.0), "the rows keep what is left: {rows:?}");
    assert!(rows.bottom() <= tray.top() + px(0.5), "the tray is under the rows: {tray:?}");
    assert!(
        tray.top() <= request.top() + px(0.5) && request.bottom() <= rest.top() + px(0.5),
        "the request stands whole: {request:?} in {tray:?}"
    );
    assert!(
        rest.size.height <= whole.size.height * 0.12 + px(1.0),
        "the open plan under it keeps to a line or two: {rest:?}"
    );
    assert!(tray.bottom() <= composer.top() + px(0.5), "the tray is over the composer");
    assert!(composer.bottom() <= whole.bottom() + px(0.5), "which stands whole");

    let above = gpui::point(tray.center().x, tray.top() - px(2.0));
    cx.simulate_click(above, Modifiers::none());
    assert!(intents(&sent).is_empty(), "the rows' click: {:?}", intents(&sent));
    assert!(cx.debug_bounds("request-q").is_some(), "the request still waits");
}
