//! Coding-agent state as observed by the host.

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

/// Unified agent status, shape-coded on the canvas.
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
}
