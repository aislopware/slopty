//! Carrying a thread on: a branch from a message on any agent, the new thread's composer opening
//! on the message or on a pointer back, and a message that stops the turn to go.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Modifiers, TestAppContext};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome};
use slopty_proto::thread::{
    AgentId, Cap, Changed, Clipped, Delivery, Item, ItemBody, ItemId, Phase, ThreadId, Turn,
    TurnId, TurnState, Usage, UserMessage,
};

use super::{hub, intents, snapshot, view};
use crate::conversation::thread::hub::{HubEvent, ThreadHub};
use crate::conversation::thread::{ThreadView, fixtures};

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

fn user(id: &str, turn: u32) -> Item {
    Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(turn),
        at_ms: WallMs::ZERO,
        body: ItemBody::User(UserMessage {
            text: Clipped::whole("Count the lines"),
            images: Vec::new(),
            command: None,
            intent: None,
        }),
    }
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// Open "Branch from here" under the message `u`: the pointer on it shows its mark.
fn open_branch(cx: &mut gpui::VisualTestContext) {
    let message = cx.debug_bounds("item-u").expect("the message").center();
    cx.simulate_mouse_move(message, None, Modifiers::none());
    cx.run_until_parked();
    click(cx, "branch-u");
}

/// "Branch from here" offers the thread's own agent first, then the others the worker can
/// start; another agent starts afresh, the thread it started is handed to the workspace to
/// open, and its composer opens on a pointer back: the old thread's id, folder and branch,
/// and the command that reads it.
#[gpui::test]
fn branching_to_another_agent_carries_the_thread_over(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::CONTINUE), Cap::named(Cap::REWIND)];
    state.meta.facts.insert("branch".to_owned(), "main".to_owned());
    state.turns = vec![turn(1, TurnState::Complete)];
    state.items = vec![user("u", 1)];
    let codex = AgentId::named(AgentId::CODEX);
    hub.update(cx, |hub, cx| {
        hub.connected(cx);
        hub.set_agents(vec![AgentId::named(AgentId::CLAUDE_CODE), codex.clone()], cx);
    });
    let started: Rc<RefCell<Vec<(ThreadId, ThreadId)>>> = Rc::default();
    let into = Rc::clone(&started);
    cx.update(|cx| {
        cx.subscribe(&hub, move |_hub, event: &HubEvent, _cx| {
            if let HubEvent::Started { from, thread, .. } = event {
                into.borrow_mut().push((*from, *thread));
            }
        })
        .detach();
    });
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    open_branch(cx);
    assert!(cx.debug_bounds("branch-agent-2").is_none(), "its own agent once");
    assert!(cx.debug_bounds("branch-from-message").is_some(), "its own agent edits from here");
    click(cx, "branch-agent-1");
    assert!(cx.debug_bounds("branch-from-message").is_none(), "another agent takes it all");
    click(cx, "branch-go");
    assert_eq!(intents(&sent), [Intent::Continue { agent: codex }]);
    assert!(cx.debug_bounds("branch-panel").is_none(), "the panel shuts");
    let id = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            slopty_proto::ClientMsg::Thread(
                slopty_proto::thread::wire::ThreadRequest::Intent { id, .. },
            ) => Some(*id),
            _ => None,
        })
        .expect("sent");
    let new = ThreadId::new();
    let done = IntentDone { id, outcome: Outcome::Started { thread: new } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    assert_eq!(*started.borrow(), [(thread, new)], "the workspace is asked to open it");
    let (fresh, cx) = view(cx, &hub, new);
    let pointer = fresh.read_with(cx, ThreadView::draft);
    for part in [thread.to_string(), "in /w".to_owned(), "on branch main".to_owned()] {
        assert!(pointer.contains(&part), "{part:?} in {pointer:?}");
    }
    assert!(pointer.contains(&format!("slopty agent read --thread {thread}")), "{pointer}");
    let (again, cx) = view(cx, &hub, new);
    assert_eq!(again.read_with(cx, ThreadView::draft), "", "the pointer is given once");
}

/// On an agent that takes no message mid-turn but can be stopped, "Interrupt and send" stands
/// beside the queue's send while a turn runs; the message waits in the tray, never as a
/// bubble, until it goes.
#[gpui::test]
fn interrupt_and_send_waits_in_the_tray(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::QUEUE), Cap::named(Cap::INTERRUPT)];
    state.status.phase = Phase::Working;
    state.turns = vec![turn(1, TurnState::Active)];
    state.items = vec![user("u", 1)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-interrupt-send").is_none(), "nothing typed, nothing offered");
    cx.simulate_input("Stop and use the other parser");
    let at = cx.debug_bounds("thread-interrupt-send").expect("offered").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    let sent_now = intents(&sent);
    assert!(
        matches!(sent_now.as_slice(), [Intent::Send { delivery: Delivery::Interrupt, .. }]),
        "{sent_now:?}"
    );
    let id = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            slopty_proto::ClientMsg::Thread(
                slopty_proto::thread::wire::ThreadRequest::Intent { id, .. },
            ) => Some(*id),
            _ => None,
        })
        .expect("sent");
    let queued = format!("queued-{id}");
    assert!(cx.debug_bounds(Box::leak(queued.into_boxed_str())).is_some(), "in the tray");
    let bubble = format!("sending-{id}");
    assert!(cx.debug_bounds(Box::leak(bubble.into_boxed_str())).is_none(), "not a bubble");
}

/// Branching from a message of the person's on its own agent edits from just before it:
/// the files kept or put back, as chosen. While a turn runs, Branch asks nothing.
#[gpui::test]
fn branching_from_a_message_keeps_or_puts_back_the_files(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::REWIND), Cap::named(Cap::FORK)];
    state.status.phase = Phase::Working;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Active)];
    state.items = vec![user("u", 1), user("v", 2)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    open_branch(cx);
    click(cx, "branch-go");
    assert!(intents(&sent).is_empty(), "nothing while a turn runs");

    state.status.phase = Phase::Idle;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    click(cx, "branch-revert");
    click(cx, "branch-go");
    assert_eq!(intents(&sent), [Intent::Rewind { turn: TurnId(1), files: true }]);
    assert!(cx.debug_bounds("branch-panel").is_none(), "the choice is made");

    // Another thread edits the same folder: the worker turns the files down, and going back
    // without them is one press away.
    let id = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            slopty_proto::ClientMsg::Thread(
                slopty_proto::thread::wire::ThreadRequest::Intent { id, .. },
            ) => Some(*id),
            _ => None,
        })
        .expect("sent");
    let reason = "\u{201c}Docs\u{201d} is working in the same folder".to_owned();
    let done = IntentDone { id, outcome: Outcome::Refused { reason } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    click(cx, Box::leak(format!("refused-no-files-{id}").into_boxed_str()));
    assert_eq!(
        intents(&sent).last(),
        Some(&Intent::Rewind { turn: TurnId(1), files: false }),
        "the same turn, the files left as they are"
    );
    let refused = format!("refused-{id}");
    assert!(cx.debug_bounds(Box::leak(refused.into_boxed_str())).is_none(), "the refusal goes");

    // The new thread's composer holds the message it started before, to change and send.
    let again = sent
        .borrow()
        .iter()
        .rev()
        .find_map(|m| match m {
            slopty_proto::ClientMsg::Thread(
                slopty_proto::thread::wire::ThreadRequest::Intent { id, .. },
            ) => Some(*id),
            _ => None,
        })
        .expect("sent again");
    let new = ThreadId::new();
    let done = IntentDone { id: again, outcome: Outcome::Started { thread: new } };
    hub.update(cx, |hub, cx| hub.done(&done, cx));
    cx.run_until_parked();
    let (fresh, cx) = view(cx, &hub, new);
    assert_eq!(fresh.read_with(cx, ThreadView::draft), "Count the lines");
}
