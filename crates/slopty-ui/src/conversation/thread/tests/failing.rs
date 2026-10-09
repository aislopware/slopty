//! A turn that failed: "Try again" over the field sends one message, as ↵ does; a lapsed
//! sign-in sends nothing and points at the agent's own terminal.

use gpui::{Modifiers, TestAppContext};
use slopty_core::WallMs;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Changed, Turn, TurnId, TurnState, Usage};

use super::{hub, intents, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;

fn failed(error: &str) -> Turn {
    Turn {
        id: TurnId(1),
        input: None,
        state: TurnState::Failed { error: error.to_owned(), until_ms: None },
        started_ms: WallMs::from_millis(1_000),
        ended_ms: Some(WallMs::from_millis(2_000)),
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

/// An overloaded API fails the turn: the strip says so and "Try again" sends "Continue" once.
#[gpui::test]
fn try_again_sends_one_message(cx: &mut TestAppContext) {
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![failed("API Error: 529 Overloaded")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    cx.update(|window, _cx| window.set_a11y_active(true));
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let said = "Failed: API Error: 529 Overloaded";
    assert!(tree.iter().any(|n| n.is("Status", Some(said))), "{tree:#?}");
    let again = cx.debug_bounds("thread-try-again").expect("Try again").center();
    cx.simulate_click(again, Modifiers::none());
    let sends: Vec<Intent> =
        intents(&sent).into_iter().filter(|i| matches!(i, Intent::Send { .. })).collect();
    assert!(
        matches!(sends.as_slice(), [Intent::Send { text, .. }] if text == "Continue"),
        "one message: {sends:?}"
    );
}

/// A lapsed sign-in offers no Try again: it says where to sign in.
#[gpui::test]
fn a_lapsed_sign_in_points_at_the_terminal(cx: &mut TestAppContext) {
    let (hub, _sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    state.turns = vec![failed("Invalid API key \u{b7} Please run /login")];
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 0), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("thread-failed").is_some(), "the failure is said");
    assert!(cx.debug_bounds("thread-try-again").is_none(), "sending again cannot mend it");
}
