//! Coding agents: which one a start asks for, and where its work lands.
//!
//! What an agent is doing travels as its thread's row ([`crate::thread::wire::ThreadRow`]);
//! the worker's hook tracker that feeds the row keeps its own vocabulary off the wire.

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;

/// Which agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AgentKind {
    /// Claude Code.
    ClaudeCode,
}

/// Where an agent's work lands, as its status line names it: the worktree it runs in.
///
/// Its branch's pull request is its thread's, which the worker reads from the forge
/// ([`crate::thread::wire::ThreadRow::pull`]), never what the status line says of one.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentBranch {
    /// The terminal session the agent runs in.
    pub session: SessionId,
    /// The worktree, when the agent runs in one Claude Code made (`--worktree`).
    pub worktree: Option<Worktree>,
}

/// A worktree Claude Code made for a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Worktree {
    /// Its name.
    pub name: String,
    /// Where it is.
    pub path: String,
    /// Its branch, when git made it.
    pub branch: Option<String>,
    /// The directory the session started in.
    pub original_cwd: String,
    /// The branch that directory was on, when git made the worktree.
    pub original_branch: Option<String>,
}
