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

/// One entry of an agent's conversation, as the client shows it: what was said, when.
///
/// The agent's bookkeeping records and sidechains (subagents talking to themselves) are not
/// entries. Long texts (tool results, thinking, tool input) are cut on the host to a readable
/// size ([`Clipped`]), so the wire never carries a whole file.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// When the record was written, in milliseconds since the Unix epoch, when the record
    /// says.
    pub at: Option<u64>,
    /// What it is.
    pub body: TranscriptBody,
}

/// What one [`TranscriptEntry`] holds.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TranscriptBody {
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
    /// What the agent thought before answering; shown collapsed.
    Thinking {
        /// The thinking, clipped.
        text: Clipped,
    },
    /// A tool the agent called.
    ToolUse {
        /// Tool name (`Bash`, `Edit`, …).
        name: String,
        /// One line about the call: the command, the file, the pattern.
        summary: String,
        /// The whole input as pretty JSON, clipped; shown on expand.
        input: Clipped,
    },
    /// What a tool returned.
    ToolResult {
        /// The tool that produced it, when the call was seen.
        tool: Option<String>,
        /// The result text, clipped.
        output: Clipped,
        /// The tool failed.
        is_error: bool,
    },
}

/// Text cut on the host to a readable size, with a count of what was dropped.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Clipped {
    /// The kept text (whole lines, from the start).
    pub text: String,
    /// Lines dropped after `text`; zero when nothing was cut.
    pub more_lines: u32,
}

impl Clipped {
    /// Text that fits as it is.
    #[must_use]
    pub const fn whole(text: String) -> Self {
        Self { text, more_lines: 0 }
    }
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
