//! Coding-agent status as the host observes it.
//!
//! Which agent a shell runs and whether it works, waits on the human or is done. The agent
//! itself is used through its own TUI in the terminal; nothing here drives it.

use serde::{Deserialize, Serialize};
use slopty_core::SessionId;

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
}

/// Which signal the host read the status from, weakest first.
///
/// The host watches four signals and keeps the strongest one that has spoken for a session:
/// the foreground process only says an agent is there, the terminal title tells working from
/// idle, the JSONL transcript names the turn and the tool, and the hooks say everything
/// including what the agent is blocked on. A client shows the same pill for all four; the
/// source is what tells it whether offering "install hooks" would buy the human anything.
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

/// Host → client.
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
}
