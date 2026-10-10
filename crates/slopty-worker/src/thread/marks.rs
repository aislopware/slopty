//! The person's own marks on a thread, which no agent's session has: how far they have seen
//! it and what they were writing to it ([`Intent::Seen`], [`Intent::Draft`]).
//!
//! Any device sets them and every device reads them on the thread's row, so what is unread
//! and a message begun on one device follow the person to the next. They are the worker's,
//! kept in the thread's log, and stay when the log is read again from the agent
//! ([`super::Host::reset`]).

use slopty_core::WallMs;
use slopty_proto::thread::wire::{Draft, Intent, Outcome};
use slopty_proto::thread::{Action, ThreadState, TurnId};

/// What `intent` does to a thread at `state`, at `now` on the worker's clock.
///
/// Its outcome and the actions that carry it: a seen mark moves only up, and never past the
/// thread's last turn; a draft past [`Draft::MAX_BYTES`] is refused, and empty words clear it
/// with an empty draft of their time, so a device that kept the old words sees them cleared
/// later than it wrote them.
/// Anything but a mark is refused, as no mark.
#[must_use]
pub fn act(state: &ThreadState, intent: &Intent, now: WallMs) -> (Outcome, Vec<Action>) {
    match intent {
        Intent::Seen { turn } => {
            let last = state.last_turn().map_or(TurnId::BEFORE, |t| t.id);
            let turn = (*turn).min(last);
            let moved = (turn > state.seen).then_some(Action::Seen(turn));
            (Outcome::Done, moved.into_iter().collect())
        }
        Intent::Draft { text } if text.len() > Draft::MAX_BYTES => (
            Outcome::Refused {
                reason: format!(
                    "a draft kept for other devices is at most {} KiB; this one stays here",
                    Draft::MAX_BYTES / 1024
                ),
            },
            Vec::new(),
        ),
        Intent::Draft { text } if text.trim().is_empty() => {
            let held = state.draft.as_ref().is_some_and(|d| !d.text.is_empty());
            let cleared = held.then(|| Action::DraftSet(Draft { text: String::new(), at_ms: now }));
            (Outcome::Done, cleared.into_iter().collect())
        }
        Intent::Draft { text } => {
            let draft = Draft { text: text.clone(), at_ms: now };
            (Outcome::Done, vec![Action::DraftSet(draft)])
        }
        _ => (Outcome::Refused { reason: "not a mark of the person's".to_owned() }, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{
        AgentId, Changed, Drive, ThreadId, ThreadMeta, Turn, TurnState, Usage,
    };

    use super::*;

    fn state(turns: u32) -> ThreadState {
        let meta = ThreadMeta {
            modes: Vec::new(),
            efforts: Vec::new(),
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::PI),
            agent_version: "1.1.0".to_owned(),
            native: "s".to_owned(),
            cwd: "/w".to_owned(),
            title: "t".to_owned(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::DRIVEN),
            caps: Vec::new(),
            models: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        };
        let mut state = ThreadState::new(meta);
        for n in 1..=turns {
            state.apply(&Action::TurnStarted(Turn {
                id: TurnId(n),
                input: None,
                state: TurnState::Complete,
                started_ms: WallMs::ZERO,
                ended_ms: Some(WallMs::from_millis(1)),
                usage: Usage::default(),
                models: Vec::new(),
                changed: Changed::default(),
                before: None,
                after: None,
            }));
        }
        state
    }

    fn after(mut state: ThreadState, intent: &Intent) -> (Outcome, ThreadState) {
        let (outcome, actions) = act(&state, intent, WallMs::from_millis(7));
        for action in &actions {
            state.apply(action);
        }
        (outcome, state)
    }

    /// The seen mark moves up to the turn named, never back and never past the last turn.
    #[test]
    fn the_seen_mark_moves_only_up_to_the_last_turn() {
        let (outcome, seen) = after(state(3), &Intent::Seen { turn: TurnId(2) });
        assert_eq!((outcome, seen.seen), (Outcome::Done, TurnId(2)));
        let (_, back) = after(seen.clone(), &Intent::Seen { turn: TurnId(1) });
        assert_eq!(back.seen, TurnId(2), "never back");
        assert_eq!(act(&seen, &Intent::Seen { turn: TurnId(1) }, WallMs::ZERO).1, []);
        let (_, past) = after(seen, &Intent::Seen { turn: TurnId(9) });
        assert_eq!(past.seen, TurnId(3), "not past the last turn");
    }

    /// A draft is kept with the worker's time, cleared by empty words, and one too long for a
    /// row is refused and keeps what was there.
    #[test]
    fn a_draft_is_kept_cleared_and_bounded() {
        let (outcome, kept) = after(state(1), &Intent::Draft { text: "and then".to_owned() });
        assert_eq!(outcome, Outcome::Done);
        let expected = Draft { text: "and then".to_owned(), at_ms: WallMs::from_millis(7) };
        assert_eq!(kept.draft, Some(expected.clone()));
        let long = "x".repeat(Draft::MAX_BYTES + 1);
        let (refused, still) = after(kept.clone(), &Intent::Draft { text: long });
        assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");
        assert_eq!(still.draft, Some(expected));
        let (_, cleared) = after(kept, &Intent::Draft { text: "  \n".to_owned() });
        let tombstone = Draft { text: String::new(), at_ms: WallMs::from_millis(7) };
        assert_eq!(cleared.draft, Some(tombstone), "cleared, with the time it was");
        assert_eq!(act(&cleared, &Intent::Draft { text: String::new() }, WallMs::ZERO).1, []);
    }
}
