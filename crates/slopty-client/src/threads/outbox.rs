//! What this client asked of its worker's threads and has not heard answered: each intent under
//! the id it was first sent with, sent again under the same id after a reconnect or a relaunch
//! until the worker answers it.

use serde::{Deserialize, Serialize};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{IntentId, ThreadId};

/// One intent on its way.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Sent {
    /// Its id, the same every time it is sent.
    pub id: IntentId,
    /// The thread it is for.
    pub thread: ThreadId,
    /// What it asks.
    pub intent: Intent,
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

    /// Add one.
    pub(crate) fn push(&mut self, sent: Sent) {
        self.sent.push(sent);
    }

    /// The worker answered `id`: it is no longer on its way. Returns it, when it was one of
    /// these.
    pub(crate) fn answered(&mut self, id: IntentId) -> Option<Sent> {
        let at = self.sent.iter().position(|s| s.id == id)?;
        Some(self.sent.remove(at))
    }
}
