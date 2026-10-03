//! A Claude Code session as the daemon follows it for orchestration.
//!
//! Where its transcripts are, which the thread model's Claude Code adapter reads, and the
//! following that holds its permission prompts for an answer through the thread
//! ([`Verb::AnswerRequest`](slopty_proto::orchestration::Verb)) rather than in its TUI alone.
//! The prompts are the daemon's held ones (`crate::conversation::Holds`), which a verb
//! reaches as [`crate::conversation::ORCHESTRATION`] through [`Conversations`].

use std::path::PathBuf;

use slopty_core::SessionId;
use slopty_proto::conversation::Meters;

/// Where a session's conversation is written, and what the daemon heard of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sources {
    /// The transcript the agent writes now; `None` before it wrote one.
    pub main: Option<PathBuf>,
    /// The subagent transcripts the hooks named.
    pub subagents: Vec<PathBuf>,
    /// The status line's latest meters.
    pub meters: Option<Meters>,
}

/// The daemon's followed Claude Code sessions, as orchestration reaches them.
pub trait Conversations: Send + Sync {
    /// Orchestration follows `session` from now on, so its permission prompts are held for an
    /// answer through its thread. Following twice is following once.
    fn follow(&self, session: SessionId);
    /// Where the session's conversation is.
    fn sources(&self, session: SessionId) -> Sources;
    /// The session ended: orchestration stops following it.
    fn forget(&self, session: SessionId);
}
