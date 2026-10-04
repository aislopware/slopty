//! What this client asked of its worker's threads and has not seen through: each intent under the
//! id it was first sent with, sent again under the same id after a reconnect or a relaunch until
//! the worker answers, and kept after the answer until the thread's own state shows it, so what
//! the person did shows from the frame they did it to the frame the agent's record takes over.

use serde::{Deserialize, Serialize};
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome, ThreadRow};
use slopty_proto::thread::{
    AskId, IntentId, ItemBody, RequestState, ThreadId, ThreadState, TurnState,
};

/// One intent on its way.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Sent {
    /// Its id, the same every time it is sent.
    pub id: IntentId,
    /// The thread it is for.
    pub thread: ThreadId,
    /// What it asks.
    pub intent: Intent,
    /// The worker's answer, once it came.
    pub outcome: Option<Outcome>,
}

impl Sent {
    /// Whether the worker turned it down: refused, or the agent cannot do it.
    #[must_use]
    pub const fn failed(&self) -> bool {
        matches!(self.outcome, Some(Outcome::Refused { .. } | Outcome::Unsupported { .. }))
    }

    /// Why the worker turned it down, in words.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        match &self.outcome {
            Some(Outcome::Refused { reason }) => Some(reason.clone()),
            Some(Outcome::Unsupported { cap }) => Some(format!("The agent cannot {}", cap.0)),
            _ => None,
        }
    }

    /// Whether `state` shows what this intent did, so it no longer needs drawing on its own.
    pub(crate) fn shown_in(&self, state: &ThreadState) -> bool {
        match &self.intent {
            // A failed send stays until the person dismisses it: it holds their words.
            Intent::Send { .. } if self.failed() => false,
            Intent::Send { .. } => {
                state.pending.iter().any(|p| p.intent == self.id)
                    || state
                        .items
                        .iter()
                        .any(|i| matches!(&i.body, ItemBody::User(m) if m.intent == Some(self.id)))
            }
            Intent::Answer { ask, .. } | Intent::Release { ask } => {
                self.failed() || !request_open(state, ask)
            }
            Intent::Interrupt => {
                self.outcome.is_some()
                    && state.last_turn().is_none_or(|t| !matches!(t.state, TurnState::Active))
            }
            // A refused edit holds the person's new words: it stays while the message it was
            // for still waits, until they dismiss it. A done one shows once the message reads
            // as it says, or has gone.
            Intent::Edit { pending, text } => {
                let waiting = state.pending.iter().find(|p| p.intent == *pending);
                if self.failed() {
                    waiting.is_none()
                } else {
                    self.outcome.is_some() && waiting.is_none_or(|p| p.text == *text)
                }
            }
            Intent::Withdraw { .. }
            | Intent::Promote { .. }
            | Intent::SetModel { .. }
            | Intent::SetMode { .. }
            | Intent::Compact
            | Intent::Handoff
            | Intent::TakeBack
            | Intent::StopTask { .. }
            | Intent::Keep(_)
            | Intent::Revert(_)
            | Intent::Fork { .. }
            | Intent::Continue { .. }
            | Intent::Rewind { .. }
            | Intent::SetEffort { .. }
            | Intent::Aside
            | Intent::Discard
            | Intent::KeepAside
            | Intent::Review { .. } => self.outcome.is_some(),
        }
    }

    /// Whether `row` shows what this answered intent did, for a thread not open here: a list
    /// answered it, and only the table follows the thread.
    fn shown_in_row(&self, row: Option<&ThreadRow>) -> bool {
        match &self.intent {
            Intent::Send { .. } if self.failed() => false,
            Intent::Answer { ask, .. } | Intent::Release { ask } => {
                self.failed() || row.is_none_or(|row| !row.requests.iter().any(|r| r.id == *ask))
            }
            _ => true,
        }
    }
}

fn request_open(state: &ThreadState, ask: &AskId) -> bool {
    state.requests.iter().any(|r| r.id == *ask && matches!(r.state, RequestState::Open))
}

/// The intents on their way, oldest first.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Outbox {
    sent: Vec<Sent>,
}

impl Outbox {
    /// Every intent on its way.
    #[must_use]
    pub fn all(&self) -> &[Sent] {
        &self.sent
    }

    /// Those for `thread`, oldest first.
    pub fn of(&self, thread: ThreadId) -> impl Iterator<Item = &Sent> {
        self.sent.iter().filter(move |s| s.thread == thread)
    }

    /// Those the worker has not answered: what goes again when the link comes back.
    pub fn unanswered(&self) -> impl Iterator<Item = &Sent> {
        self.sent.iter().filter(|s| s.outcome.is_none())
    }

    /// Add one.
    pub(crate) fn push(&mut self, sent: Sent) {
        self.sent.push(sent);
    }

    /// The worker's answer. Returns whether it was for one of these.
    pub(crate) fn answered(&mut self, done: &IntentDone) -> bool {
        match self.sent.iter_mut().find(|s| s.id == done.id) {
            Some(sent) => {
                sent.outcome = Some(done.outcome.clone());
                true
            }
            None => false,
        }
    }

    /// Drop the answered ones `state` now shows. Returns whether any went.
    pub(crate) fn settle(&mut self, thread: ThreadId, state: &ThreadState) -> bool {
        let before = self.sent.len();
        self.sent.retain(|s| s.thread != thread || s.outcome.is_none() || !s.shown_in(state));
        self.sent.len() != before
    }

    /// Drop one the person is done with (a failed send they read). Returns it.
    pub(crate) fn dismiss(&mut self, id: IntentId) -> Option<Sent> {
        let at = self.sent.iter().position(|s| s.id == id)?;
        Some(self.sent.remove(at))
    }

    /// Drop the answered ones for `thread`, not open here, that its table row shows. Returns
    /// whether any went.
    pub(crate) fn settle_by_row(&mut self, thread: ThreadId, row: Option<&ThreadRow>) -> bool {
        let before = self.sent.len();
        self.sent.retain(|s| s.thread != thread || s.outcome.is_none() || !s.shown_in_row(row));
        self.sent.len() != before
    }
}
