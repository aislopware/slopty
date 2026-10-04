//! A message sent now to an agent that has no steer of its own ([`Delivery::Interrupt`]).
//!
//! On the person's word the worker takes it through the agent's own doors, in order: the
//! message goes into the agent's queue, first, and the turn under way is stopped, so the
//! message goes as that turn ends. That is how T3 Code steers by restarting a run, and how an
//! ACP agent is steered. Nothing is typed into a screen, and nothing is answered for the person.

use slopty_proto::thread::wire::{Intent, Outcome};
use slopty_proto::thread::{Cap, Delivery, IntentId, Phase, ThreadId, ThreadState, TurnState};

use super::Host;

/// What comes of `intent` on `thread` when it is a message sent by interrupt; `None` for any
/// other intent.
///
/// `act` takes each step as a client's intent goes, once per id: the message is queued under
/// `id`, so the thread's pending list names it as the person sent it, then moved before what
/// already waits, then the turn is stopped under ids derived from `id` ([`step_of`]). A repeat
/// of `id` repeats nothing. The answer is the queue's: the stop is the agent's to make, and a
/// turn that ended first leaves the message to go as a queued one does.
pub fn act(
    host: &Host,
    thread: ThreadId,
    id: IntentId,
    intent: &Intent,
    act: impl Fn(IntentId, &Intent) -> Outcome,
) -> Option<Outcome> {
    let Intent::Send { text, attachments, delivery: Delivery::Interrupt } = intent else {
        return None;
    };
    let Some((state, _)) = host.state(thread) else {
        return Some(Outcome::Refused { reason: "There is no such thread here".to_owned() });
    };
    if let Some(cap) = [Cap::INTERRUPT, Cap::QUEUE].into_iter().find(|cap| !state.meta.can(cap)) {
        return Some(Outcome::Unsupported { cap: Cap::named(cap) });
    }
    let ahead = ahead(&state, id);
    let queued = Intent::Send {
        text: text.clone(),
        attachments: attachments.clone(),
        delivery: Delivery::Queue,
    };
    let outcome = act(id, &queued);
    if !matches!(outcome, Outcome::Done | Outcome::Accepted) {
        return Some(outcome);
    }
    if let Some(before) = ahead {
        let first = Intent::Reorder { pending: id, before: Some(before) };
        let moved = act(step_of(id, "first"), &first);
        tracing::debug!(%thread, %id, ?moved, "a message sent by interrupt goes first");
    }
    if working(&state) {
        let stopped = act(step_of(id, "interrupt"), &Intent::Interrupt);
        tracing::info!(%thread, %id, ?stopped, "a turn stopped for a message sent by interrupt");
    }
    Some(outcome)
}

/// The intent of step `step` of the send by interrupt `intent`.
#[must_use]
pub fn step_of(intent: IntentId, step: &str) -> IntentId {
    let derived = ThreadId::derived(&["sent by interrupt", step, &intent.to_string()]);
    IntentId::from_uuid(*derived.as_uuid())
}

/// The first message in the agent's queue other than `id`'s, when one is.
fn ahead(state: &ThreadState, id: IntentId) -> Option<IntentId> {
    state.pending.iter().find(|p| p.intent != id && p.delivery == Delivery::Queue).map(|p| p.intent)
}

/// Whether `state`'s agent has a turn under way to stop: working, or asking the person.
fn working(state: &ThreadState) -> bool {
    matches!(state.status.phase, Phase::Working | Phase::NeedsYou)
        || state.last_turn().is_some_and(|turn| turn.state == TurnState::Active)
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        AgentId, Drive, Liveness, Pending, PendingState, Status, ThreadMeta,
    };

    use super::*;

    fn pending(delivery: Delivery, state: PendingState) -> Pending {
        let (intent, text, attachments) = (IntentId::new(), "next".to_owned(), Vec::new());
        Pending { intent, text, attachments, delivery, state }
    }

    /// The message goes before the first one in the agent's queue, held or not, never before a
    /// draft or a scheduled one; the turn is stopped only while one is under way.
    #[test]
    fn it_goes_before_what_waits_and_stops_only_a_turn_under_way() {
        let meta = ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::acp("opencode"),
            agent_version: String::new(),
            native: "s".to_owned(),
            cwd: "/w".to_owned(),
            title: String::new(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: Vec::new(),
            models: Vec::new(),
            modes: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        };
        let mut state = ThreadState::new(meta);
        let sent = IntentId::new();
        assert_eq!(ahead(&state, sent), None);
        let held = PendingState::Held { reason: "in the way".to_owned() };
        state.pending = vec![
            pending(Delivery::Draft, PendingState::Waiting),
            pending(Delivery::At { at_ms: WallMs::ZERO }, PendingState::Waiting),
            pending(Delivery::Queue, held),
            pending(Delivery::Queue, PendingState::Waiting),
            pending(Delivery::Queue, PendingState::Waiting),
        ];
        assert_eq!(ahead(&state, sent), Some(state.pending[2].intent));
        state.pending[2].intent = sent;
        assert_eq!(ahead(&state, sent), Some(state.pending[3].intent), "never before itself");

        assert!(!working(&state));
        for (phase, under_way) in
            [(Phase::Working, true), (Phase::NeedsYou, true), (Phase::Done, false)]
        {
            state.status =
                Status { phase, wait: None, liveness: Liveness::Live, since_ms: WallMs::ZERO };
            assert_eq!(working(&state), under_way, "{phase:?}");
        }
    }
}
