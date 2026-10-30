//! The person's own marks on a thread, which no agent's session has: how far they have seen it
//! and their own name for it ([`Intent::Seen`], [`Intent::Name`]).
//!
//! Any device sets them and every device reads them on the thread's row, so what is unread
//! follows the person to the next device. They are the worker's, kept in the thread's log, and
//! stay when the log is read again from the agent ([`super::Host::reset`]).

use slopty_proto::thread::wire::{Intent, Outcome};
use slopty_proto::thread::{Action, ThreadState, TurnId};

/// What `intent` does to a thread at `state`: its outcome and the actions that carry it.
///
/// A seen mark moves only up, and never past the thread's last turn; a name past
/// [`Intent::NAME_MAX`] characters is refused, and empty words give the agent's own title back.
#[must_use]
pub fn act(state: &ThreadState, intent: &Intent) -> (Outcome, Vec<Action>) {
    match intent {
        Intent::Seen { turn } => {
            let last = state.last_turn().map_or(TurnId::BEFORE, |t| t.id);
            let turn = (*turn).min(last);
            let moved = (turn > state.seen).then_some(Action::Seen(turn));
            (Outcome::Done, moved.into_iter().collect())
        }
        Intent::Name { title } if title.trim().chars().count() > Intent::NAME_MAX => (
            Outcome::Refused {
                reason: format!("a thread's name is at most {} characters", Intent::NAME_MAX),
            },
            Vec::new(),
        ),
        Intent::Name { title } => {
            let name = title.trim();
            let renamed = (state.named != name).then(|| Action::NameSet(name.to_owned()));
            (Outcome::Done, renamed.into_iter().collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        AgentId, Changed, Drive, ThreadId, ThreadMeta, Turn, TurnState, Usage,
    };

    use super::*;

    fn state(turns: u32) -> ThreadState {
        let meta = ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::PI),
            agent_version: "1.1.0".to_owned(),
            native: "s".to_owned(),
            cwd: "/w".to_owned(),
            title: "t".to_owned(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::DRIVEN),
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
                mode: None,
                context: None,
            }));
        }
        state
    }

    fn after(mut state: ThreadState, intent: &Intent) -> (Outcome, ThreadState) {
        let (outcome, actions) = act(&state, intent);
        for action in &actions {
            state.apply(action);
        }
        (outcome, state)
    }

    /// The seen mark moves up to the turn named, never back and never past the last turn, and
    /// a mark where it stands already is no change to send.
    #[test]
    fn the_seen_mark_moves_only_up_to_the_last_turn() {
        let (outcome, seen) = after(state(3), &Intent::Seen { turn: TurnId(2) });
        assert_eq!((outcome, seen.seen), (Outcome::Done, TurnId(2)));
        let (_, back) = after(seen.clone(), &Intent::Seen { turn: TurnId(1) });
        assert_eq!(back.seen, TurnId(2), "never back");
        assert_eq!(act(&seen, &Intent::Seen { turn: TurnId(1) }).1, []);
        assert_eq!(act(&seen, &Intent::Seen { turn: TurnId(2) }).1, [], "where it stands");
        let (_, past) = after(seen, &Intent::Seen { turn: TurnId(9) });
        assert_eq!(past.seen, TurnId(3), "not past the last turn");
    }

    /// The person's name shows over the agent's on the row, empty words take it back, and an
    /// agent naming the thread afterwards changes nothing the person named.
    #[test]
    fn the_persons_name_shows_over_the_agents() {
        let mut agents = state(1);
        agents.meta.title = "Fix the build".to_owned();
        let (outcome, named) = after(agents, &Intent::Name { title: " Release ".to_owned() });
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(named.row(WallMs::ZERO).title, "Release");
        let mut renamed = named.clone();
        let mut meta = renamed.meta.clone();
        meta.title = "Agent's new title".to_owned();
        renamed.apply(&Action::Meta(Box::new(meta)));
        assert_eq!(renamed.row(WallMs::ZERO).title, "Release", "the agent's own does not win");
        let (_, back) = after(renamed, &Intent::Name { title: String::new() });
        assert_eq!(back.row(WallMs::ZERO).title, "Agent's new title");
        let long = Intent::Name { title: "x".repeat(Intent::NAME_MAX + 1) };
        assert!(matches!(after(named, &long).0, Outcome::Refused { .. }));
    }
}
