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

/// Where an agent's work lands, as its status line names it: the worktree it runs in and the
/// pull request (or merge request) open for its branch.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentBranch {
    /// The terminal session the agent runs in.
    pub session: SessionId,
    /// The open pull request, when there is one.
    pub pr: Option<PullRequest>,
    /// The worktree, when the agent runs in one Claude Code made (`--worktree`).
    pub worktree: Option<Worktree>,
}

/// A pull request, or a GitLab merge request.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullRequest {
    /// Its number.
    pub number: u32,
    /// Its page.
    pub url: String,
    /// Where its review stands, when the forge said.
    pub review: Option<Review>,
    /// A GitLab merge request rather than a GitHub pull request.
    pub merge_request: bool,
}

/// Where a pull request's review stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Review {
    /// Approved.
    Approved,
    /// Waiting on reviewers.
    Pending,
    /// Changes asked for.
    ChangesRequested,
    /// Still a draft.
    Draft,
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
