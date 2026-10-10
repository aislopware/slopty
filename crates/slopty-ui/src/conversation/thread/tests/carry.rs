//! Carrying a thread on: a fork from a message of the person's, the new thread's composer
//! opening on the message.

use gpui::{Modifiers, MouseButton, TestAppContext};
use slopty_core::WallMs;
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome};
use slopty_proto::thread::{
    Cap, Changed, Clipped, Item, ItemBody, ItemId, Phase, ThreadId, Turn, TurnId, TurnState, Usage,
    UserMessage,
};

use super::{hub, intents, snapshot, view};
use crate::conversation::thread::hub::ThreadHub;
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

/// Open the message `u`'s own menu with a right click.
fn open_menu(cx: &mut gpui::VisualTestContext) {
    let at = cx.debug_bounds("item-u").expect("the message").center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
}

/// The labels of the open menu's rows.
fn menu_rows(cx: &mut gpui::VisualTestContext) -> Vec<String> {
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    tree.iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label.clone()).collect()
}

/// A message of the person's goes on in two ways from its own menu, Fork from here and Ask
/// aside, beside Copy and Quote, and no other. Fork asks the agent for a fork through the turn
/// before the message, offered only while no turn runs, and the new thread's composer holds the
/// message to change and send.
#[gpui::test]
fn a_message_forks_from_its_menu(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.meta.caps = vec![Cap::named(Cap::FORK)];
    state.status.phase = Phase::Working;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Active)];
    state.items = vec![user("t", 1), user("u", 2)];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    open_menu(cx);
    assert_eq!(menu_rows(cx), ["Copy", "Quote in reply", "Ask aside"], "no fork while it runs");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    state.status.phase = Phase::Idle;
    state.turns = vec![turn(1, TurnState::Complete), turn(2, TurnState::Complete)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 2), cx));
    cx.run_until_parked();
    open_menu(cx);
    assert_eq!(menu_rows(cx), ["Copy", "Quote in reply", "Fork from here", "Ask aside"]);
    click(cx, "message-menu-fork");
    assert_eq!(intents(&sent), [Intent::Fork { after: Some(TurnId(1)) }]);
    assert!(cx.debug_bounds("message-menu").is_none(), "the choice is made");

    // The new thread's composer holds the message it started before, to change and send.
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
    let (fresh, cx) = view(cx, &hub, new);
    assert_eq!(fresh.read_with(cx, ThreadView::draft), "Count the lines");
}
