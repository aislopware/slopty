//! What the worker sees of a coding agent a person runs by hand in a terminal
//! ([`crate::AgentTable`]): whether one is there, and whether its title says a turn runs.
//!
//! It stays on the worker, for orchestration's waits and the sleep policy. An agent Slopty
//! drives is a thread, whose row (`slopty_proto::thread::wire::ThreadRow`) says far more.

use slopty_core::{SessionId, WallMs};

/// A terminal's agent, as its process and its title show it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    /// No agent runs in the terminal.
    None,
    /// An agent runs, at rest at its prompt or not saying otherwise.
    Idle,
    /// Its title says a turn runs.
    Working,
}

/// Which signal the worker read the status from, weakest first: the foreground process only
/// says an agent is there, the terminal title tells working from idle.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum AgentSource {
    /// The session's foreground process is the agent's; nothing else is known.
    Process,
    /// The terminal title (OSC 0/2) said working or idle.
    Title,
}

/// The agent a session runs, as the worker's table holds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SessionAgent {
    /// What it is doing.
    pub status: AgentStatus,
    /// Where the status came from.
    pub source: AgentSource,
    /// When the status last entered its phase, by the worker's clock. Zero with no agent.
    pub since_ms: WallMs,
}

/// A change of a terminal's agent, as the worker's table heard it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgentEvent {
    /// Session.
    pub session: SessionId,
    /// Status.
    pub status: AgentStatus,
    /// Where the status came from.
    pub source: AgentSource,
    /// When the status last entered its phase, by the worker's clock. Zero with no agent.
    pub since_ms: WallMs,
}

impl From<&AgentEvent> for SessionAgent {
    fn from(event: &AgentEvent) -> Self {
        Self { status: event.status, source: event.source, since_ms: event.since_ms }
    }
}
