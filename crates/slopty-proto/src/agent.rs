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
    /// A Claude Code hook said, the only observed signal that reports blocking.
    Hook,
    /// The host drives the agent over its structured protocol and hears everything.
    Driven,
}

/// Client → host: start an agent the host drives (a `SessionKind::Agent` session); answered
/// with `HostMsg::SessionOpened`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct OpenAgent {
    /// Working directory; the host's default when `None`.
    pub cwd: Option<String>,
    /// A Claude Code session id to resume, else a new conversation.
    pub resume: Option<String>,
    /// Model override (`--model`), else Claude Code's default.
    pub model: Option<String>,
    /// Display name.
    pub title: Option<String>,
}

/// Host → client: what a driven agent said about itself.
///
/// From `system/init`, the turn results and the answers to `ClientMsg::AgentSet`; sent whole
/// whenever any of it changes and with the snapshot a late client gets.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Claude Code's session id, the one `OpenAgent::resume` takes; known after `init`.
    pub agent_session: Option<String>,
    /// The model in use (`claude-fable-5-1`, …), as the agent last named it.
    pub model: Option<String>,
    /// The permission mode in force (`default`, `acceptEdits`, `plan`, …).
    pub permission_mode: Option<String>,
    /// Slash commands the agent takes as prompts (`/compact`, `/clear`, …).
    pub slash_commands: Vec<String>,
    /// Turns the conversation has had.
    pub turns: u32,
    /// Claude Code's own cost estimate for the conversation, in millionths of a dollar.
    pub cost_micro_usd: u64,
}

/// Client → host: retune a driven agent in place.
///
/// Claude Code's `set_model` and `set_permission_mode` control requests. A `None` leaves that
/// setting alone; the host answers with `HostMsg::AgentInfo` once the agent acknowledged.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentSet {
    /// The agent session.
    pub session: SessionId,
    /// The model to switch to: an alias (`fable`, `opus`, `sonnet`, `haiku`) or a full name.
    pub model: Option<String>,
    /// The permission mode to switch to.
    pub permission_mode: Option<String>,
}

/// One past Claude Code conversation the host found on disk, for resuming.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AgentSessionInfo {
    /// Claude Code's session id (`OpenAgent::resume`).
    pub id: String,
    /// The working directory it ran in.
    pub cwd: String,
    /// Its first prompt, clipped to a line; empty when the file holds none.
    pub title: String,
    /// When the transcript was last written, in milliseconds since the Unix epoch.
    pub modified_ms: u64,
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
        /// What the call would do, in the shape the client draws it.
        detail: ToolDetail,
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

/// A tool call waiting on the human, as a structured agent session reports it: Claude Code's
/// `can_use_tool` control request, with the input the tool would run with.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The request id the answer must carry.
    pub id: String,
    /// The tool call's own id (`tool_use_id`).
    pub tool_use: String,
    /// Tool name (`Bash`, `Write`, …).
    pub tool: String,
    /// One line about the call, the same line the transcript shows for it.
    pub summary: String,
    /// What the call would do, the same shape the transcript entry carries.
    pub detail: ToolDetail,
}

/// What a tool call would do, in the shape the client draws.
///
/// Read from the call's input on the host: an edit is a diff, a todo list is a checklist, a
/// command is a command line, and a tool nobody taught the host is its input as pretty JSON.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ToolDetail {
    /// A shell command (`Bash`).
    Command {
        /// The command line, clipped.
        command: Clipped,
        /// What the agent said it is for.
        description: Option<String>,
    },
    /// A file edit (`Edit`): the replaced text against its replacement, line by line.
    Diff {
        /// The file.
        path: String,
        /// The change, clipped to the same size as any other long text.
        lines: Vec<DiffLine>,
        /// Diff lines dropped after `lines`; zero when nothing was cut.
        more_lines: u32,
        /// Every occurrence is replaced, not only the first.
        replace_all: bool,
    },
    /// A file written whole (`Write`).
    Write {
        /// The file.
        path: String,
        /// The content, clipped.
        content: Clipped,
    },
    /// A file read (`Read`).
    Read {
        /// The file.
        path: String,
        /// The first line read, when the agent asked for a slice.
        offset: Option<u32>,
        /// Lines read, when the agent asked for a slice.
        limit: Option<u32>,
    },
    /// A search (`Grep`, `Glob`).
    Search {
        /// The pattern.
        pattern: String,
        /// Where, when the agent said.
        path: Option<String>,
        /// A file filter, when the agent gave one.
        glob: Option<String>,
    },
    /// The agent's task list (`TodoWrite`), whole.
    Todos {
        /// The items in the agent's order.
        items: Vec<Todo>,
    },
    /// A subagent (`Agent`, `Task`).
    Agent {
        /// The agent's one-line brief.
        description: String,
        /// The agent kind, when the agent chose one.
        kind: Option<String>,
        /// The whole brief, clipped.
        prompt: Clipped,
    },
    /// A question to the human (`AskUserQuestion`): the card answers it in place.
    Question {
        /// The questions, in the agent's order (Claude Code asks up to four at once).
        questions: Vec<Question>,
    },
    /// Any other tool: the input as pretty JSON, clipped.
    Json {
        /// The input.
        input: Clipped,
    },
}

/// One question of an `AskUserQuestion` call.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Question {
    /// The question as the agent wrote it; the key its answer is filed under.
    pub text: String,
    /// The agent's short label for it ("Auth method").
    pub header: String,
    /// Several options may be picked; the answer joins their labels with ", " (the SDK's
    /// convention, not yet seen on the wire).
    pub multi: bool,
    /// The offered options; the human may also type an answer of their own.
    pub options: Vec<Choice>,
}

/// One option of a [`Question`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Choice {
    /// What is picked, and what the agent receives.
    pub label: String,
    /// What it means, under the label.
    pub description: String,
}

/// The human's answer to one [`Question`], as `ClientMsg::AgentAnswer` carries it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QuestionAnswer {
    /// [`Question::text`].
    pub question: String,
    /// The picked label, the picked labels joined with ", ", or the typed text.
    pub answer: String,
}

/// One line of a [`ToolDetail::Diff`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DiffLine {
    /// Kept, removed or added.
    pub kind: DiffKind,
    /// The line without its newline.
    pub text: String,
}

/// Which side of a diff a line is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum DiffKind {
    /// The same on both sides.
    Context,
    /// Only in the replaced text.
    Removed,
    /// Only in the replacement.
    Added,
}

/// One item of a [`ToolDetail::Todos`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Todo {
    /// The item as the agent wrote it.
    pub text: String,
    /// Where it stands.
    pub status: TodoStatus,
}

/// Where a [`Todo`] stands, in Claude Code's three states.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// The one being worked on.
    InProgress,
    /// Done.
    Completed,
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
