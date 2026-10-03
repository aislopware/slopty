//! What every adapter's fork shares.

use slopty_proto::thread::{ThreadState, TurnId};

/// The last turn a fork of `state` through `after` shares with it.
///
/// For an agent that forks a whole session only (`who`, as a sentence names it), `after` must be
/// the thread's last turn, or `None` for all of it. `None` when the thread has no turn yet.
///
/// # Errors
///
/// When `after` is an earlier turn than the last, in words.
pub fn whole(
    state: &ThreadState,
    after: Option<TurnId>,
    who: &str,
) -> Result<Option<TurnId>, String> {
    let last = state.last_turn().map(|t| t.id);
    match after {
        None => Ok(last),
        Some(after) if Some(after) == last => Ok(last),
        Some(_) => Err(format!("{who} forks a whole session, not from an earlier turn")),
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{
        Action, AgentId, Drive, ThreadId, ThreadMeta, Turn, TurnState, Usage,
    };

    use super::*;

    fn state(turns: u32) -> ThreadState {
        let meta = ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::PI),
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
        for n in 1..=turns {
            state.apply(&Action::TurnStarted(Turn {
                id: TurnId(n),
                input: None,
                state: TurnState::Complete,
                started_ms: WallMs::ZERO,
                ended_ms: None,
                usage: Usage::default(),
                models: Vec::new(),
                changed: slopty_proto::thread::Changed::default(),
                before: None,
                after: None,
            }));
        }
        state
    }

    /// The whole thread, or through its last turn, is one fork; an earlier turn is refused.
    #[test]
    fn a_whole_session_forks_only_through_its_last_turn() {
        let two = state(2);
        assert_eq!(whole(&two, None, "pi"), Ok(Some(TurnId(2))));
        assert_eq!(whole(&two, Some(TurnId(2)), "pi"), Ok(Some(TurnId(2))));
        whole(&two, Some(TurnId(1)), "pi").unwrap_err();
        assert_eq!(whole(&state(0), None, "pi"), Ok(None));
    }
}
