//! Agents' turns that ended while nobody looked: how long each ran, and which are left for the
//! person to review. They are what the navigator's *To review* lists and what the bell counts
//! beside the agents that need the person; a tile looked at reads its own.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui::Context;
use slopty_core::{SessionId, WallMs};
use slopty_proto::thread::Phase;

use super::{Finished, WorkspaceView};

/// What an agent's finished turn says when its worker gave no words of the turn's own.
pub(super) const TURN_FINISHED: &str = "Turn finished";

/// The turns under way and the ones that ended unread.
#[derive(Debug, Default)]
pub(super) struct Turns {
    /// When each agent's turn under way began, on its worker's clock.
    began: HashMap<SessionId, WallMs>,
    /// The sessions whose unread finish is an agent's turn rather than a command.
    ended: HashSet<SessionId>,
}

impl Turns {
    /// A command's finish took `session`'s place: it is no turn to review.
    pub(super) fn command_ended(&mut self, session: SessionId) {
        self.ended.remove(&session);
    }
}

/// Now, in milliseconds since the Unix epoch: the clock a worker stamps its times with.
pub(super) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl WorkspaceView {
    /// What the bell counts: the agents and threads that need the person, and the turns left to
    /// review. A shell's finish is its tile's dot alone.
    #[must_use]
    pub fn bell_count(&self) -> usize {
        self.needs_you_count().saturating_add(self.to_review().len())
    }

    /// An agent's state moved: a turn begins when it starts to work, and the turn it ends
    /// with "Turn finished" ran this long. A turn that waits on the person goes on; one that
    /// went idle without finishing, or an agent that is gone, has nothing to say.
    pub(super) fn agent_turn(
        &mut self,
        session: SessionId,
        phase: Phase,
        since: WallMs,
    ) -> Option<Duration> {
        let began = &mut self.turns.began;
        match phase {
            Phase::Working | Phase::Waiting => {
                began.entry(session).or_insert(since);
                None
            }
            Phase::NeedsYou => None,
            Phase::Done => {
                let began = began.remove(&session)?;
                let ran = since.as_millis().saturating_sub(began.as_millis());
                Some(Duration::from_millis(ran))
            }
            Phase::Idle | Phase::Failed | Phase::Stopped => {
                began.remove(&session);
                None
            }
        }
    }

    /// An agent's turn of `elapsed` ended in `session`. Long enough, and not watched, it earns
    /// a header badge, a row under *To review*, a count on the bell and a note while the app is
    /// away ([`super::attention::Look::turns`]), cleared when the tile is focused. The corner
    /// says nothing: it speaks only for what needs the person.
    pub(super) fn agent_finished(
        &mut self,
        session: SessionId,
        elapsed: Duration,
        cx: &mut Context<Self>,
    ) {
        let watched = self.app_active
            && self.tile_of_session(session).is_some_and(|t| self.focused() == Some(t));
        if watched || elapsed < self.slow_command {
            return;
        }
        // What the agent last said of the turn, as its thread's row has it.
        let said = self.face_summary(session).filter(|d| !d.trim().is_empty());
        let command = said.unwrap_or_else(|| TURN_FINISHED.to_owned());
        self.finished.insert(session, Finished { command, exit: None, elapsed });
        let finished = &self.finished;
        self.turns.ended.retain(|s| finished.contains_key(s));
        self.turns.ended.insert(session);
        cx.notify();
    }

    /// The sessions whose unread finish is an agent's turn.
    pub(super) fn agent_turns(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.turns.ended.iter().copied().filter(|s| self.finished.contains_key(s))
    }
}
