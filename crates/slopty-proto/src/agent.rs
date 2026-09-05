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

/// One entry of an agent's conversation, as the client shows it. Tool results, thinking and
/// the agent's bookkeeping lines are not entries: the conversation view reads like a chat.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TranscriptEntry {
    /// What the human typed.
    User {
        /// Prompt text.
        text: String,
    },
    /// What the agent said (Markdown).
    Assistant {
        /// The message.
        markdown: String,
    },
    /// A tool the agent called.
    ToolUse {
        /// Tool name (`Bash`, `Edit`, …).
        name: String,
        /// One line about the call: the command, the file, the pattern.
        summary: String,
    },
}

/// Client → host: start or stop following a session's conversation.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TranscriptFollow {
    /// Session.
    pub session: SessionId,
    /// `true` to receive the transcript (a snapshot, then increments), `false` to stop.
    pub follow: bool,
}

/// Host → client: a slice of a session's conversation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TranscriptUpdate {
    /// Session.
    pub session: SessionId,
    /// `true`: replace everything shown with `entries` (the snapshot after a follow, or a new
    /// transcript file); `false`: append.
    pub reset: bool,
    /// Entries, oldest first.
    pub entries: Vec<TranscriptEntry>,
}
