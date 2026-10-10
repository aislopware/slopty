//! "Fork from here": a new thread from just before a message of the person's, the message
//! waiting in its composer to be edited and sent. With "Ask aside" (`super::aside`) it is one of
//! the message menu's two ways to go on from a point of the thread (`super::message_menu`).
//!
//! It is the agent's own door: a fork through the turn before the message (`Intent::Fork`),
//! offered only where the agent forks ([`Cap::FORK`]), a turn comes before the message, and none
//! runs now. Moving work to another agent or another machine is a task's restart, not a
//! thread's, so the fork stays with this agent on this machine.

use gpui::{App, Context};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, ItemBody, ThreadState, TurnId};

use super::ThreadView;

/// The turn before `turn`, which a fork from just before `turn`'s message shares through.
fn turn_before(state: &ThreadState, turn: TurnId) -> Option<TurnId> {
    let at = state.turns.iter().position(|t| t.id == turn)?;
    at.checked_sub(1).and_then(|before| state.turns.get(before)).map(|t| t.id)
}

/// The fork from just before the message of `turn`, by the agent's own door; `None` where it
/// does not fork or no turn comes before.
pub(super) fn fork_intent(state: &ThreadState, turn: TurnId) -> Option<Intent> {
    if !state.meta.can(Cap::FORK) {
        return None;
    }
    turn_before(state, turn).map(|before| Intent::Fork { after: Some(before) })
}

/// The words the person sent to start `turn`: its input, else its first message of theirs.
pub(super) fn message_of(state: &ThreadState, turn: TurnId) -> Option<String> {
    let input = state.turns.iter().find(|t| t.id == turn)?.input.as_ref();
    let mine = |body: &ItemBody| match body {
        ItemBody::User(message) => Some(message.text.text.clone()),
        _ => None,
    };
    let items = || state.items.iter().filter(|i| i.turn == turn);
    input
        .and_then(|input| items().find(|i| i.id == *input).and_then(|i| mine(&i.body)))
        .or_else(|| items().find_map(|i| mine(&i.body)))
}

impl ThreadView {
    /// Whether the message of `turn` offers "Fork from here" now.
    pub(super) fn forks_from(&self, turn: TurnId, cx: &App) -> bool {
        !self.working(cx) && self.state(cx).and_then(|st| fork_intent(st, turn)).is_some()
    }

    /// Fork from just before the message of `turn`: the new thread opens with the message in
    /// its composer.
    pub(super) fn fork_from(&self, turn: TurnId, cx: &mut Context<Self>) {
        if self.working(cx) {
            return;
        }
        let Some(state) = self.state(cx) else { return };
        let Some(intent) = fork_intent(state, turn) else { return };
        let seed = message_of(state, turn);
        let thread = self.thread;
        let _id = self.hub.update(cx, |hub, cx| match seed {
            Some(seed) => hub.intent_seeded(thread, intent, seed, cx),
            None => hub.intent(thread, intent, cx),
        });
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{Cap, TurnId};

    use super::fork_intent;
    use crate::conversation::thread::fixtures;

    fn turn(id: u32) -> slopty_proto::thread::Turn {
        slopty_proto::thread::Turn {
            id: TurnId(id),
            input: None,
            state: slopty_proto::thread::TurnState::Complete,
            started_ms: slopty_core::WallMs::ZERO,
            ended_ms: None,
            usage: slopty_proto::thread::Usage::default(),
            models: Vec::new(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        }
    }

    /// A fork from a message goes through the turn before it, by the agent's own door: none
    /// where the agent does not fork, and none from the first message.
    #[test]
    fn a_fork_goes_through_the_turn_before_the_message() {
        let mut state = fixtures::empty();
        state.turns = vec![turn(1), turn(2)];
        assert_eq!(fork_intent(&state, TurnId(2)), None, "no fork door");
        state.meta.caps = vec![Cap::named(Cap::FORK)];
        assert_eq!(fork_intent(&state, TurnId(2)), Some(Intent::Fork { after: Some(TurnId(1)) }));
        assert_eq!(fork_intent(&state, TurnId(1)), None, "nothing before the first message");
    }
}
