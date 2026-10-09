//! An agent's status as the worker's hook tracker reads it, before the observed adapter maps it
//! into its thread's row.
//!
//! Whether the Claude Code a shell runs works, waits on the human or is done: the
//! vocabulary of Claude Code's hooks, its title and its transcript ([`crate::AgentTable`]). It
//! stays on the worker. What goes over the wire is the thread's row
//! (`slopty_proto::thread::wire::ThreadRow`), which every client and the server read alone.

use slopty_core::{SessionId, WallMs};

/// Why an agent is blocked on a human.
#[derive(Clone, PartialEq, Eq, Debug)]
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
#[derive(Clone, PartialEq, Eq, Debug)]
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

    /// Whether an agent with this status is at work: thinking, running a tool, or waiting on
    /// background work it started. Idle at its prompt, done, blocked on the person, or holding
    /// only scheduled prompts, it is not. What a project's spent time counts.
    #[must_use]
    pub const fn works(&self) -> bool {
        match self {
            Self::Working | Self::Tool { .. } => true,
            Self::Waiting { tasks, .. } => *tasks > 0,
            Self::None | Self::Idle | Self::Blocked(_) | Self::Done | Self::Failed { .. } => false,
        }
    }

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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
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
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionAgent {
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
#[derive(Clone, PartialEq, Eq, Debug)]
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

/// A change of a terminal's agent, as the worker's tracker heard it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AgentEvent {
    /// Session.
    pub session: SessionId,
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
            status: event.status.clone(),
            source: event.source,
            since_ms: event.since_ms,
            mode: event.mode.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentStatus, BlockReason};

    /// An agent works while it thinks, runs a tool or waits on background work it started; idle
    /// at its prompt, blocked on the person, or holding only a scheduled prompt, it does not.
    #[test]
    fn an_agent_works_while_its_turn_or_its_background_work_runs() {
        let tool = AgentStatus::Tool { tool: "Bash".to_owned() };
        let background = AgentStatus::Waiting { tasks: 1, crons: 0 };
        let scheduled = AgentStatus::Waiting { tasks: 0, crons: 1 };
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        for (status, works) in [
            (&AgentStatus::Working, true),
            (&tool, true),
            (&background, true),
            (&scheduled, false),
            (&blocked, false),
            (&AgentStatus::Idle, false),
            (&AgentStatus::Done, false),
            (&AgentStatus::None, false),
        ] {
            assert_eq!(status.works(), works, "{status:?}");
        }
    }
}
