//! Coding-agent status as the worker observes it.
//!
//! Which agent a shell runs and whether it works, waits on the human or is done. The agent
//! itself is used through its own TUI in the terminal; nothing here drives it.

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};

/// Which agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum AgentKind {
    /// Claude Code.
    ClaudeCode,
}

/// Why an agent is blocked on a human.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BlockReason {
    /// A tool needs permission.
    Permission {
        /// Tool name.
        tool: String,
    },
    /// The agent asked a question.
    Question,
    /// An MCP elicitation.
    Elicitation,
    /// Waiting at the prompt after finishing a turn.
    IdlePrompt,
}

/// Unified agent status, shape-coded in the chrome.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AgentStatus {
    /// No agent detected in the session.
    None,
    /// Agent present and idle at its prompt.
    Idle,
    /// Thinking or streaming text.
    Working,
    /// Running a tool.
    Tool {
        /// Tool name.
        tool: String,
    },
    /// Blocked on the human.
    Blocked(BlockReason),
    /// A turn just finished; cleared on the next user action or after a delay.
    Done,
    /// A turn ended on an error the agent gave up on (Claude Code's `StopFailure`): at rest,
    /// like [`AgentStatus::Done`], but nothing was finished.
    Failed {
        /// The agent's kind of error ([`AgentStatus::RATE_LIMIT`], `overloaded`,
        /// `authentication_failed`, …); `unknown` when it names none.
        error: String,
        /// For a limit, when it resets, where that is known.
        until_ms: Option<WallMs>,
    },
    /// The turn ended with work still out: background commands, subagents or monitors that
    /// wake the agent when they finish, or scheduled prompts (`/loop`). Paused, not done: no
    /// "done" is announced until a turn ends with nothing out.
    Waiting {
        /// Background tasks in flight.
        tasks: u32,
        /// Prompts scheduled on the session.
        crons: u32,
    },
}

impl AgentStatus {
    /// The error of a turn a usage or rate limit stopped ([`AgentStatus::Failed`]).
    pub const RATE_LIMIT: &str = "rate_limit";

    /// Whether the agent is at rest: no turn under way and nothing asked of the person.
    #[must_use]
    pub const fn at_rest(&self) -> bool {
        matches!(self, Self::None | Self::Idle | Self::Done | Self::Failed { .. })
    }
}

/// Which signal the worker read the status from, weakest first.
///
/// The worker watches four signals and keeps the strongest one that has spoken for a session:
/// the foreground process only says an agent is there, the terminal title tells working from
/// idle, the JSONL transcript names the turn and the tool, and the hooks say everything
/// including what the agent is blocked on. A client shows the same pill for all four; the
/// source tells it how far the status can be trusted.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum AgentSource {
    /// The session's foreground process is the agent's; nothing else is known.
    Process,
    /// The terminal title (OSC 0/2) said working or idle.
    Title,
    /// The agent's JSONL transcript said what the turn is doing.
    Transcript,
    /// A Claude Code hook said, the only signal that reports blocking.
    Hook,
}

/// The agent a session runs, as a listing or a status query reports it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SessionAgent {
    /// Which agent.
    pub kind: AgentKind,
    /// What it is doing.
    pub status: AgentStatus,
    /// Where the status came from; anything short of [`AgentSource::Hook`] means the hooks are
    /// not installed (or have not spoken yet).
    pub source: AgentSource,
    /// When the status last entered its phase, by the worker's clock: busy (working or a
    /// tool), blocked, done or idle. A tool call inside a turn keeps the stamp, so a client
    /// reads how long the turn has run, and since the worker's table carries it, a client that
    /// reconnects reads the same. Zero with no agent.
    pub since_ms: WallMs,
    /// The permission mode it runs in, as it last said ([`HeardMode`]).
    pub mode: Option<HeardMode>,
}

/// The permission mode an agent runs in, as it last said.
///
/// Every hook Claude Code runs carries it (`permission_mode`), and its transcript notes it with
/// each prompt (`permissionMode`), so a change the person makes in the TUI (Shift-Tab) is heard
/// with the agent's next hook. Nothing is typed or asked to learn it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HeardMode {
    /// Claude Code's own name for it (`default`, `plan`, `acceptEdits`, `dontAsk`,
    /// `bypassPermissions`, …); one a newer Claude Code adds passes through as named.
    pub name: String,
    /// When it was heard, by the worker's clock.
    pub heard_ms: WallMs,
}

impl SessionAgent {
    /// This state of `session`'s agent as an event that raises no attention: what a snapshot
    /// of the terminals tells a client that missed the changes.
    #[must_use]
    pub fn quiet_event(&self, session: SessionId) -> AgentEvent {
        AgentEvent {
            session,
            kind: self.kind,
            status: self.status.clone(),
            agent_session: None,
            detail: None,
            attention: false,
            source: self.source,
            since_ms: self.since_ms,
            mode: self.mode.clone(),
        }
    }
}

/// Worker → client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Session.
    pub session: SessionId,
    /// Agent.
    pub kind: AgentKind,
    /// Status.
    pub status: AgentStatus,
    /// Agent session id (Claude Code `session_id`), when known.
    pub agent_session: Option<String>,
    /// Short human text ("Editing src/main.rs", "Waiting for permission: Bash").
    pub detail: Option<String>,
    /// Whether this transition should raise attention (sound, badge) or is a quiet correction.
    pub attention: bool,
    /// Where the status came from.
    pub source: AgentSource,
    /// When the status last entered its phase, by the worker's clock: busy (working or a
    /// tool), blocked, done or idle. A tool call inside a turn keeps the stamp, so a client
    /// reads how long the turn has run, and since the worker's table carries it, a client that
    /// reconnects reads the same. Zero with no agent.
    pub since_ms: WallMs,
    /// The permission mode it runs in, as it last said ([`HeardMode`]). An event goes out when
    /// only this changed.
    pub mode: Option<HeardMode>,
}

impl From<&AgentEvent> for SessionAgent {
    fn from(event: &AgentEvent) -> Self {
        Self {
            kind: event.kind,
            status: event.status.clone(),
            source: event.source,
            since_ms: event.since_ms,
            mode: event.mode.clone(),
        }
    }
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
