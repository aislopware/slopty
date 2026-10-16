//! What the worker sees of a coding agent a person runs by hand in a terminal
//! ([`crate::AgentTable`]): whether one is there, by the terminal's foreground process.
//!
//! It stays on the worker, so orchestration never types into such a terminal. An agent Slopty
//! drives is a thread, whose row (`slopty_proto::thread::wire::ThreadRow`) says far more.

use slopty_core::{SessionId, WallMs};

/// A terminal's agent, as its foreground process shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    /// No agent runs in the terminal.
    None,
    /// An agent runs there: the worker knows nothing of its turns.
    Running,
}

/// The agent a session runs, as the worker's table holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionAgent {
    /// Whether it runs.
    pub status: AgentStatus,
    /// When it started to, by the worker's clock. Zero with no agent.
    pub since_ms: WallMs,
}

/// A change of a terminal's agent, as the worker's table heard it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgentEvent {
    /// Session.
    pub session: SessionId,
    /// Status.
    pub status: AgentStatus,
    /// When it started to run, by the worker's clock. Zero with no agent.
    pub since_ms: WallMs,
}

impl From<&AgentEvent> for SessionAgent {
    fn from(event: &AgentEvent) -> Self {
        Self { status: event.status, since_ms: event.since_ms }
    }
}
