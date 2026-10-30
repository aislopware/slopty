//! The agent-neutral thread model: what any coding agent's session looks like to a client, and
//! the one way it changes.
//!
//! Every agent's session is mapped on the worker by its adapter into the same shape, read from the
//! agent's own records and published doors (`docs/decisions/agents.md`, "TUI-first"): a thread of
//! turns, each turn a run of items (what the person sent, what the agent wrote, the tools it
//! called), beside its own prompt box and the prompts queued behind its turn ([`prompt`]), and
//! its plan and background work. The shape
//! follows Codex's thread, turn and item primitives; how it is kept in step on many clients
//! follows Microsoft's Agent Host Protocol.
//!
//! **One mutation.** A thread changes only by an [`Action`]. [`ThreadState::apply`] is pure and
//! is the same code on the worker, which keeps the thread's log, and on every client, which
//! mirrors it, so the two can never disagree about what a run of actions means.
//!
//! **Open where agents differ.** What an agent is ([`AgentId`]), how Slopty reaches it
//! ([`Drive`]), what kind of tool it called ([`ToolCall::kind`]) and which tokens it counts
//! ([`Usage`]) are open strings with known values
//! named here, never closed enums: a new agent brings its own without a wire
//! change, and a client shows what it does not know as a quiet, plain row. What is closed is
//! the structure every agent shares: the phases a thread moves through, a tool call's states,
//! and where text is appended.
//!
//! **Synchronisation.** Each followed thread streams a snapshot, then actions numbered by a
//! [`Cursor`] (`epoch`, `seq`). A client that comes back sends the cursor it holds and gets only
//! what it missed, or a fresh snapshot when the worker's log was rewritten or no longer holds
//! the gap ([`wire`]). Intents carry a client-made [`IntentId`], so one sent twice across a
//! dropped link is acted on once.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
use uuid::Uuid;

pub mod attention;
pub mod detail;
pub mod prompt;
mod reduce;
pub mod wire;
pub mod work;

pub use detail::{Clipped, ContentRef, Image, Patch, ToolDetail};
pub use prompt::{Draft, DraftImage};
pub use reduce::{TableState, ThreadState};
pub use work::{CallNote, TaskNote, TaskUsage};

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A fresh, time-ordered id.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing UUID.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

uuid_id!(
    /// A thread, minted by the worker that hosts it. A subagent's thread has its own.
    ThreadId
);
impl ThreadId {
    /// The thread named by `parts` alone, the same on every host that knows them: BLAKE3 over
    /// each part behind its length, its first 16 bytes as a UUID. An adapter names a thread by
    /// its agent's own session this way, so a worker that rebuilds its log from the session, or
    /// a client that knows the session, comes to the same thread.
    #[must_use]
    pub fn derived(parts: &[&str]) -> Self {
        let mut hasher = blake3::Hasher::new();
        for part in parts {
            hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(hasher.finalize().as_bytes().get(..16).unwrap_or(&[0; 16]));
        Self(uuid::Builder::from_custom_bytes(bytes).into_uuid())
    }

    /// The thread of Claude Code subagent `agent` in the session whose thread is `self`: the
    /// one the worker's adapter keeps for it, and the one a client opens it by.
    #[must_use]
    pub fn subagent(self, agent: &str) -> Self {
        Self::derived(&["claude-code subagent", &self.to_string(), agent])
    }
}

uuid_id!(
    /// One thing a client asked of a thread, minted by the client.
    ///
    /// The worker acts on an id once and answers a repeat with the first outcome, so a client
    /// resends what it never heard back on, under the same id, after a dropped link or a
    /// relaunch.
    IntentId
);

/// A turn of a thread: from what started it to the agent's answer. Counted from 1 in each
/// thread; [`TurnId::BEFORE`] holds what came before the first turn (a resumed session's
/// notices).
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default, Serialize, Deserialize,
)]
pub struct TurnId(pub u32);

impl TurnId {
    /// Before the first turn.
    pub const BEFORE: Self = Self(0);

    /// The turn after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// An item's id: the agent's own where it is stable (a tool call's `tool_use_id`, a Codex item
/// id), so the item keeps it across reads and a result that comes later finds its call.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct ItemId(pub String);

/// A request's id, the agent's own where it has one (a Codex request id, pi's UI request id),
/// else the worker's.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AskId(pub String);

/// An agent, by name. Open: [`AgentId::CLAUDE_CODE`], [`AgentId::CODEX`], [`AgentId::PI`], and
/// `acp:<name>` for an agent reached over ACP.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    /// What names an agent reached over ACP: `acp:<name>`, `<name>` its registry's.
    pub const ACP_PREFIX: &'static str = "acp:";
    /// Claude Code.
    pub const CLAUDE_CODE: &'static str = "claude-code";
    /// Codex.
    pub const CODEX: &'static str = "codex";
    /// pi.
    pub const PI: &'static str = "pi";

    /// The agent named `name`.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self(name.to_owned())
    }

    /// The ACP agent the registry names `name`.
    #[must_use]
    pub fn acp(name: &str) -> Self {
        Self(format!("{}{name}", Self::ACP_PREFIX))
    }

    /// The registry's name of the ACP agent this is, when it is one.
    #[must_use]
    pub fn acp_name(&self) -> Option<&str> {
        self.0.strip_prefix(Self::ACP_PREFIX).filter(|name| !name.is_empty())
    }

    /// Whether this is the agent named `name`.
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.0 == name
    }
}

/// How Slopty reaches an agent. Open; the known ones are [`Drive::SHARED`] and
/// [`Drive::DRIVEN`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Drive(pub String);

impl Drive {
    /// Slopty runs the agent over its protocol.
    pub const DRIVEN: &'static str = "driven";
    /// The agent runs as its own TUI in a Slopty terminal, which the thread names
    /// ([`ThreadMeta::TERMINAL_FACT`]); Slopty reads it through its own records, read-only.
    pub const OBSERVED: &'static str = "observed";
    /// Slopty is a second live client of the agent's own server, beside its TUI.
    pub const SHARED: &'static str = "shared";

    /// The drive named `name`.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self(name.to_owned())
    }

    /// Whether this is the drive named `name`.
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.0 == name
    }
}

/// What a thread is: its agent, where it runs and how it came to be. Changes rarely
/// ([`Action::Meta`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadMeta {
    /// The thread.
    pub id: ThreadId,
    /// Its agent.
    pub agent: AgentId,
    /// The agent's version, as it says it.
    pub agent_version: String,
    /// The agent's own id for the session, the record this thread mirrors: Claude Code's
    /// session id, a Codex thread id, a pi session id.
    pub native: String,
    /// Where it works.
    pub cwd: String,
    /// What it is about, from the task or the first prompt.
    pub title: String,
    /// For a subagent: the thread and the call that started it.
    pub parent: Option<Link>,
    /// How it came to be. Open: [`ThreadMeta::PERSON`], [`ThreadMeta::SUBAGENT`],
    /// [`ThreadMeta::ORCHESTRATED`].
    pub origin: String,
    /// How Slopty reaches it.
    pub drive: Drive,
    /// Open facts about it: its seat, its parent, branch, pull request, model.
    pub facts: BTreeMap<String, String>,
    /// When it began.
    pub created_ms: WallMs,
}

impl ThreadMeta {
    /// Started by another agent ([`crate::orchestration::Verb::AgentStart`]).
    pub const ORCHESTRATED: &'static str = "orchestrated";
    /// Started by the person.
    pub const PERSON: &'static str = "person";
    /// Started by another thread's call.
    pub const SUBAGENT: &'static str = "subagent";
    /// The fact naming the terminal (its `SessionId`) the thread's agent runs in now, as its
    /// own TUI: a client opens that terminal for the thread. Only a main thread carries it, and
    /// only while its session runs there; at most one thread names a terminal at a time.
    pub const TERMINAL_FACT: &'static str = "slopty.terminal";

    /// The terminal the thread's agent runs in now ([`Self::TERMINAL_FACT`]).
    #[must_use]
    pub fn terminal(&self) -> Option<slopty_core::SessionId> {
        self.facts.get(Self::TERMINAL_FACT).and_then(|id| id.parse().ok())
    }
}

/// Where a subagent's thread hangs: the thread and the call that started it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Link {
    /// The parent thread.
    pub thread: ThreadId,
    /// The call in it.
    pub item: ItemId,
}

/// Where a thread is, as every adapter maps its agent: the contract the attention ladder ranks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Phase {
    /// At rest, waiting for a message.
    #[default]
    Idle,
    /// Working on a turn.
    Working,
    /// Waiting on its own background work or a wakeup, not on the person.
    Waiting,
    /// Waiting on the person: it asked something, or its own dialog is open.
    NeedsYou,
    /// It finished what it was asked.
    Done,
    /// It stopped on an error.
    Failed,
    /// It was stopped.
    Stopped,
}

impl Phase {
    /// Its place on the attention ladder, highest first: needs you, failed, working, waiting,
    /// then the rest.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::NeedsYou => 5,
            Self::Failed => 4,
            Self::Working => 3,
            Self::Waiting => 2,
            Self::Done => 1,
            Self::Idle | Self::Stopped => 0,
        }
    }
}

/// What a thread waits on, in words.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Wait {
    /// Open: what the agent's records and hooks say it waits on the person for (`permission`,
    /// `question`, `plan`, `input`), [`Wait::TASK`], [`Wait::COMPACTING`], else the adapter's
    /// own word.
    pub kind: String,
    /// What it waits for, worded by the adapter ("Wants to run cargo test").
    pub text: String,
}

impl Wait {
    /// At work compacting its context: the turn waits on the summary, not on the model's next
    /// word.
    pub const COMPACTING: &'static str = "compacting";
    /// At rest with work of its own left running in the background: a subagent, a monitor, a
    /// scheduled prompt, or work the adapter cannot tell apart.
    pub const TASK: &'static str = "task";
}

/// Whether the thread's agent is there, beside where it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Liveness {
    /// Its process runs.
    #[default]
    Live,
    /// Its process ended.
    Exited {
        /// Its session can be resumed.
        resumable: bool,
    },
}

/// A thread's state at a glance ([`Action::Status`]).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    /// Where it is.
    pub phase: Phase,
    /// What it waits on, when it waits.
    pub wait: Option<Wait>,
    /// Whether its agent is there.
    pub liveness: Liveness,
    /// When the phase began.
    pub since_ms: WallMs,
}

/// A turn.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Turn {
    /// Its number.
    pub id: TurnId,
    /// The item that started it: the person's message, a task notification.
    pub input: Option<ItemId>,
    /// How it stands.
    pub state: TurnState,
    /// When it began.
    pub started_ms: WallMs,
    /// When it ended.
    pub ended_ms: Option<WallMs>,
    /// The tokens it took.
    pub usage: Usage,
    /// The models that answered in it.
    pub models: Vec<String>,
    /// Lines added and removed in the working tree over the turn.
    pub changed: Changed,
}

/// How a turn stands.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TurnState {
    /// Under way.
    Active,
    /// The agent answered.
    Complete,
    /// The person stopped it.
    Interrupted,
    /// It ended on an error.
    Failed {
        /// What went wrong.
        error: String,
    },
}

/// Lines added and removed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Changed {
    /// Added.
    pub added: u32,
    /// Removed.
    pub removed: u32,
}

/// What a turn used, by kind: the tokens.
///
/// Open: each agent counts its own (Claude Code reads and writes a cache, Codex counts
/// reasoning), so the kinds are keys, with the common ones named here. No dollar figure is
/// kept: what the person's plan bills is not knowable without their credentials.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Usage(pub BTreeMap<String, u64>);

impl Usage {
    /// Input tokens read from the prompt cache.
    pub const CACHE_READ: &'static str = "cache-read";
    /// Input tokens written to the prompt cache.
    pub const CACHE_WRITE: &'static str = "cache-write";
    /// Tokens read as input.
    pub const INPUT: &'static str = "input";
    /// Tokens written.
    pub const OUTPUT: &'static str = "output";
    /// Output tokens spent on reasoning.
    pub const REASONING: &'static str = "reasoning";

    /// The count of `kind`, zero when none.
    #[must_use]
    pub fn get(&self, kind: &str) -> u64 {
        self.0.get(kind).copied().unwrap_or(0)
    }

    /// The tokens counted, of every kind.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.0.values().fold(0, |sum, n| sum.saturating_add(*n))
    }

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        for (kind, n) in &other.0 {
            let have = self.0.entry(kind.clone()).or_insert(0);
            *have = have.saturating_add(*n);
        }
    }
}

/// A snapshot of the working tree, taken for a review: a git tree id in the worker's private
/// refs.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TreeRef(pub String);

/// One thing in a thread.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Item {
    /// Its id, stable across reads.
    pub id: ItemId,
    /// The turn it is in.
    pub turn: TurnId,
    /// When the agent wrote it; zero when it says nothing.
    pub at_ms: WallMs,
    /// What it is.
    pub body: ItemBody,
}

/// What an item is.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ItemBody {
    /// What the person (or an automation) sent.
    User(UserMessage),
    /// The agent's answer.
    Text(Clipped),
    /// The agent's reasoning, as much as it shows.
    Reasoning(Clipped),
    /// A tool call.
    Tool(Box<ToolCall>),
    /// The context was compacted here.
    Compaction(Compaction),
    /// Something the agent itself said, not the model: an API error, a hook's word, an
    /// interrupt, a rewind.
    Notice(Notice),
    /// The agent entered or left a review of its own.
    Review {
        /// Entered rather than left.
        entered: bool,
    },
    /// A background task's notice, a quiet row linked to its task: an agent finished or waits on
    /// its own work, a command exited, a monitor's event.
    Task(Box<TaskNote>),
    /// A record the adapter does not know yet, kept as it came so it is never dropped. A
    /// client shows it as a quiet row.
    Extra {
        /// The record's kind, as the agent names it.
        kind: String,
        /// The record, as JSON, clipped.
        json: Clipped,
    },
}

/// A message to the agent.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    /// The words.
    pub text: Clipped,
    /// Pictures with it.
    pub images: Vec<Image>,
    /// A command it is (`/compact`), or `!` for a shell command.
    pub command: Option<String>,
}

/// A compaction.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Compaction {
    /// What started it, as the agent says: `manual`, `auto`.
    pub trigger: Option<String>,
    /// Context tokens before.
    pub before_tokens: Option<u64>,
    /// And after.
    pub after_tokens: Option<u64>,
    /// The summary the thread goes on from.
    pub summary: Option<Clipped>,
}

/// A notice from the agent.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Notice {
    /// Open: [`Notice::API_ERROR`], [`Notice::COMMAND`], [`Notice::DECLINED`], [`Notice::INFO`],
    /// [`Notice::HOOK`], [`Notice::INTERRUPTED`], [`Notice::LIMIT`], [`Notice::REPORT`],
    /// [`Notice::REWOUND`], [`Notice::ALLOWED`], [`Notice::FALLBACK`], [`Notice::GOAL`],
    /// [`Notice::MEMORY`], [`Notice::MODE`], [`Notice::STOPPED`], [`Notice::TURN`],
    /// [`Notice::NOTE`], [`Notice::WARNING`], [`Notice::ERROR`].
    pub kind: String,
    /// What it says.
    pub text: Clipped,
    /// For a failure the agent tries again: which attempt comes next and when.
    pub retry: Option<Retry>,
}

/// An agent trying a failed request again.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Retry {
    /// The attempt that comes next, from 1.
    pub attempt: u32,
    /// How many attempts it makes before it gives up, when it says.
    pub max: Option<u32>,
    /// How long it waits before that attempt, in ms, when it says.
    pub in_ms: Option<u64>,
}

impl Notice {
    /// The person allowed calls, and the agent tries them again: "Allowed cargo test".
    pub const ALLOWED: &'static str = "allowed";
    /// The API failed, or a request is being retried.
    pub const API_ERROR: &'static str = "api-error";
    /// What a command printed.
    pub const COMMAND: &'static str = "command";
    /// What a command printed as its errors: a slash command's or a `!` command's standard
    /// error, drawn apart from its output as the TUI draws it, in red.
    pub const COMMAND_ERROR: &'static str = "command-error";
    /// The agent's own judge of risk (Claude Code's auto mode) turned a call down, with no
    /// person asked.
    pub const DECLINED: &'static str = "declined";
    /// The agent ended on its own, nobody having asked: what kind of end it was in plain words,
    /// with the one thing to do next (Claude Code's out of memory, killed, crashed, signed out…).
    pub const ENDED: &'static str = "ended";
    /// Something went wrong that the agent says in its own words, apart from a turn's failure
    /// (an ACP agent's error notice): its first line is the row, the rest opens under it.
    pub const ERROR: &'static str = "error";
    /// The turn went on with another model, or the model refused and another took over.
    pub const FALLBACK: &'static str = "fallback";
    /// The goal's check said it is met, not yet, or cannot be: "Goal achieved (5m · 3 turns)".
    pub const GOAL: &'static str = "goal";
    /// A hook failed, blocked a call or stopped the turn.
    pub const HOOK: &'static str = "hook";
    /// Something the agent wanted to say.
    pub const INFO: &'static str = "info";
    /// The person stopped the agent.
    pub const INTERRUPTED: &'static str = "interrupted";
    /// The agent stopped on a usage limit, in its words.
    pub const LIMIT: &'static str = "limit";
    /// The agent saved or recalled memories: "Saved 2 memories", "Recalled 3 memories".
    pub const MEMORY: &'static str = "memory";
    /// The agent's permission mode changed without the person switching it: a plan approved
    /// with a mode, plan mode entered or left ("Switched to accept edits").
    pub const MODE: &'static str = "mode";
    /// A quiet line of the agent's that is neither a failure nor the person's words: a
    /// prompt's origin ("Prompt from the deploy plugin"), an MCP server's update, a message
    /// from another session. Its first line is the row, the rest opens under it.
    pub const NOTE: &'static str = "note";
    /// Slopty's own reports handed to the agent with a turn (`slopty-server`'s deliveries, in
    /// their `<slopty-reports>` frame), kept as they were framed.
    pub const REPORT: &'static str = "report";
    /// The person went back to an earlier message; what came after it is gone.
    pub const REWOUND: &'static str = "rewound";
    /// Background agents were stopped at once, named ("2 background agents were stopped: …"),
    /// or "All background agents stopped" when none is known by name.
    pub const STOPPED: &'static str = "stopped";
    /// The turn's own footer, when the agent says more than its times: "Waiting for 2 background
    /// agents to finish", "3 background tasks still running".
    pub const TURN: &'static str = "turn";
    /// Something the agent warns of (an ACP agent's warning notice): its first line is the row,
    /// the rest opens under it.
    pub const WARNING: &'static str = "warning";

    /// A notice of `kind` saying `text`, with no retry.
    #[must_use]
    pub fn new(kind: &str, text: Clipped) -> Self {
        Self { kind: kind.to_owned(), text, retry: None }
    }
}

/// A tool call.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    /// The tool's own name, as the agent calls it (`Edit`, `commandExecution`, `bash`).
    pub name: String,
    /// What kind of call it is, which picks how a client draws it. Open: the [`kind`]
    /// constants; an unknown kind is drawn from the title, input and output.
    pub kind: String,
    /// What it does, worded by the adapter, for a list or a notification.
    pub title: String,
    /// Its input as the agent gave it, as JSON.
    pub input: Clipped,
    /// Where it is.
    pub state: ToolState,
    /// What it printed or returned, as text.
    pub output: Option<Clipped>,
    /// Pictures it returned.
    pub images: Vec<Image>,
    /// A typed body for its kind, where the adapter has one.
    pub detail: Option<ToolDetail>,
    /// A subagent's thread, for a call that started one.
    pub child: Option<ThreadId>,
    /// When it ended.
    pub ended_ms: Option<WallMs>,
}

/// The tool-call kinds clients draw on their own.
pub mod kind {
    /// Reads a file.
    pub const READ: &str = "read";
    /// Edits a file in place.
    pub const EDIT: &str = "edit";
    /// Writes a whole file.
    pub const WRITE: &str = "write";
    /// Runs a command.
    pub const EXEC: &str = "exec";
    /// Searches files or their contents.
    pub const SEARCH: &str = "search";
    /// Fetches a URL.
    pub const FETCH: &str = "fetch";
    /// Searches the web.
    pub const WEB_SEARCH: &str = "web-search";
    /// Calls an MCP server's tool.
    pub const MCP: &str = "mcp";
    /// Starts a subagent.
    pub const AGENT: &str = "agent";
    /// Asks the person.
    pub const QUESTION: &str = "question";
    /// Proposes a plan.
    pub const PLAN: &str = "plan";
    /// Keeps the task list.
    pub const TASKS: &str = "tasks";
    /// Anything else.
    pub const OTHER: &str = "other";
}

/// Where a tool call is. It moves forward only: streaming, then pending on a request or
/// running, then one of the final states.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ToolState {
    /// The model is still writing its input.
    Streaming,
    /// It waits on the person's answer to a request.
    Pending {
        /// The request.
        ask: AskId,
    },
    /// It runs.
    Running,
    /// It ran.
    Completed,
    /// It failed, or the agent's own rules denied it.
    Failed,
    /// The person refused it.
    Rejected,
    /// It was stopped before it finished.
    Cancelled,
}

impl ToolState {
    /// Whether it is over.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Rejected | Self::Cancelled)
    }

    /// Whether a call in this state may move to `next`: never back from a final state, and
    /// never back to streaming once the input is whole.
    #[must_use]
    pub const fn may_become(&self, next: &Self) -> bool {
        match (self, next) {
            (current, _) if current.is_final() => false,
            (Self::Streaming, _) => true,
            (_, Self::Streaming) => false,
            _ => true,
        }
    }
}

/// The agent's plan or task list.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Plan {
    /// Its words, when it is prose.
    pub text: Option<Clipped>,
    /// Its steps.
    pub steps: Vec<Step>,
}

/// A step of a plan.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Step {
    /// Its id, when the agent gives one.
    pub id: Option<String>,
    /// What it is.
    pub text: String,
    /// Open, as the agent says: `pending`, `in_progress`, `completed`.
    pub status: String,
}

/// Work the agent runs in the background, past the call that started it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BackgroundTask {
    /// Its id, as the agent knows it.
    pub id: String,
    /// What kind of work it is. Open: [`BackgroundTask::SHELL`], [`BackgroundTask::AGENT`],
    /// [`BackgroundTask::MONITOR`], [`BackgroundTask::WORKFLOW`], [`BackgroundTask::MCP`],
    /// [`BackgroundTask::REMOTE`], [`BackgroundTask::TEAMMATE`], else the agent's own word.
    pub kind: String,
    /// What it is.
    pub title: String,
    /// Open, as the agent says: [`BackgroundTask::RUNNING`], [`BackgroundTask::COMPLETED`],
    /// [`BackgroundTask::FAILED`], [`BackgroundTask::KILLED`], [`BackgroundTask::WAITING`],
    /// [`BackgroundTask::PENDING`], [`BackgroundTask::PAUSED`].
    pub state: String,
    /// The call that started it.
    pub item: Option<ItemId>,
    /// The end of what it printed.
    pub output: Option<Clipped>,
    /// When it began.
    pub started_ms: WallMs,
    /// When it ended, once it has and the agent says when.
    pub ended_ms: Option<WallMs>,
    /// A subagent's kind (`general-purpose`), the agents list's first word.
    pub agent_type: Option<String>,
    /// What it is doing now, in a phrase: an agent's own progress summary, else its last tool
    /// ("Running full check"); the agents list's middle.
    pub step: Option<String>,
    /// Its figures so far: tokens, tool uses, how long it has run.
    pub usage: Option<TaskUsage>,
    /// The task that started it, for one a subagent started.
    pub parent: Option<String>,
    /// The thread of a subagent's run, where Slopty keeps one.
    pub child: Option<ThreadId>,
    /// Housekeeping of the agent's own (a memory pass), which a tray folds away.
    pub ambient: bool,
    /// Whether the turn waits on it rather than goes on beside it.
    pub awaited: bool,
    /// Whether it runs in the background, past its call, rather than blocking it.
    pub backgrounded: bool,
    /// How it ended, in the agent's words ("finished", "is waiting on its own background work",
    /// "failed: out of memory", "was stopped by user", "exit code 2").
    pub outcome: Option<String>,
    /// A monitor's or a command's time limit, in ms, when it has one.
    pub timeout_ms: Option<u64>,
    /// A monitor that watches until it is stopped.
    pub persistent: bool,
}

impl BackgroundTask {
    /// A subagent left running.
    pub const AGENT: &'static str = "agent";
    /// It finished.
    pub const COMPLETED: &'static str = "completed";
    /// It failed.
    pub const FAILED: &'static str = "failed";
    /// It was stopped.
    pub const KILLED: &'static str = "killed";
    /// An MCP server's long task.
    pub const MCP: &'static str = "mcp";
    /// A watch that wakes the agent on each event (Claude Code's `Monitor`).
    pub const MONITOR: &'static str = "monitor";
    /// Paused.
    pub const PAUSED: &'static str = "paused";
    /// Not started yet.
    pub const PENDING: &'static str = "pending";
    /// An agent running elsewhere (a cloud session).
    pub const REMOTE: &'static str = "remote";
    /// It runs.
    pub const RUNNING: &'static str = "running";
    /// A command left running.
    pub const SHELL: &'static str = "shell";
    /// A teammate of an agent team.
    pub const TEAMMATE: &'static str = "teammate";
    /// An agent that stopped with work of its own still running: it may go on by itself.
    pub const WAITING: &'static str = "waiting";
    /// A dynamic workflow.
    pub const WORKFLOW: &'static str = "workflow";

    /// Whether it still runs.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state == Self::RUNNING
    }

    /// Whether it is still at work in any way: running, pending, paused, or waiting on work of
    /// its own.
    #[must_use]
    pub fn is_live(&self) -> bool {
        matches!(self.state.as_str(), Self::RUNNING | Self::PENDING | Self::PAUSED | Self::WAITING)
    }

    /// Task `id` of `kind` named `title`, running in the background since `started_ms`, with
    /// nothing else known yet.
    #[must_use]
    pub fn new(id: &str, kind: &str, title: &str, started_ms: WallMs) -> Self {
        Self {
            id: id.to_owned(),
            kind: kind.to_owned(),
            title: title.to_owned(),
            state: Self::RUNNING.to_owned(),
            item: None,
            output: None,
            started_ms,
            ended_ms: None,
            agent_type: None,
            step: None,
            usage: None,
            parent: None,
            child: None,
            ambient: false,
            awaited: false,
            backgrounded: true,
            outcome: None,
            timeout_ms: None,
            persistent: false,
        }
    }
}

/// Where in an item an [`Action::Append`] goes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PartKey {
    /// The text of a message, an answer, reasoning or a notice.
    Body,
    /// A tool call's input.
    Input,
    /// A tool call's output.
    Output,
}

/// The one way a thread changes. [`ThreadState::apply`] applies one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Action {
    /// What the thread is.
    Meta(Box<ThreadMeta>),
    /// Where it is.
    Status(Status),
    /// A turn began, or what is known of it changed.
    TurnStarted(Turn),
    /// A turn ended.
    TurnEnded {
        /// The turn.
        turn: TurnId,
        /// How.
        state: TurnState,
        /// The tokens it took.
        usage: Usage,
        /// When.
        ended_ms: WallMs,
    },
    /// An item began. One already there is replaced in place.
    ItemStarted(Item),
    /// Text grew at the end of an item.
    Append {
        /// The item.
        item: ItemId,
        /// Where in it.
        part: PartKey,
        /// What came.
        text: String,
    },
    /// An item changed. A tool call never moves back from a final state this way.
    ItemUpdated(Item),
    /// An item is whole: authoritative, it replaces whatever came before it.
    ItemCompleted(Item),
    /// An item went.
    ItemRemoved {
        /// The item.
        item: ItemId,
    },
    /// The prompts the person queued in the agent's own TUI behind the turn under way, in the
    /// order they go, as its transcript says them (its `queue-operation` records); empty once
    /// none waits.
    QueuedSet(Vec<String>),
    /// The plan.
    PlanSet(Option<Plan>),
    /// The background work.
    TasksSet(Vec<BackgroundTask>),
    /// The turns after `after` are gone (a rewind, a fork); `None` empties the thread.
    Truncated {
        /// The last turn kept.
        after: Option<TurnId>,
    },
    /// The windows and displays on the worker its agent drives, the latest first; empty once
    /// it drives none.
    ScreensSet(Vec<AgentScreen>),
    /// The person saw the thread's turns through this one ([`wire::Intent::Seen`]); a mark
    /// below the one held changes nothing.
    Seen(TurnId),
    /// What the agent's own prompt box holds as the person types in its TUI ([`Draft`]);
    /// `None` once it holds nothing, or its TUI ended.
    DraftSet(Option<Draft>),
    /// The person's own name for it ([`wire::Intent::Name`]); empty once taken back.
    NameSet(String),
    /// What the agent says of one call beside the call ([`CallNote`]): its running time, the
    /// task it became, the words it ended with. Replaces the note held for the item.
    CallNoted {
        /// The call.
        item: ItemId,
        /// The note.
        note: CallNote,
    },
    /// The worker last heard the thread's agent alive at this moment, by its own clock, while a
    /// turn was under way ([`wire::ThreadRow::lease_ms`]): the agent's own word (Claude Code's
    /// mod beats every few seconds), else the worker's check that its process lives. A client
    /// that sees it go stale shows the turn as no longer known to run.
    Lease(WallMs),
}

/// A window or display on the thread's worker that its agent drives.
///
/// A browser its tools opened, a simulator, the desktop it works through computer use. The
/// worker names it from the agent's own tool calls, so the person can watch it beside the
/// thread and take it over.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AgentScreen {
    /// What a stream of it opens.
    pub target: crate::screen::CaptureTarget,
    /// What it is. Open: [`AgentScreen::BROWSER`], [`AgentScreen::SIMULATOR`],
    /// [`AgentScreen::DESKTOP`], [`AgentScreen::APP`].
    pub kind: String,
    /// Its name for people: the window's app and title, or the display's.
    pub label: String,
    /// When the agent last drove it.
    pub used_ms: WallMs,
}

impl AgentScreen {
    /// Another application's window the agent drives.
    pub const APP: &'static str = "app";
    /// A web browser's window.
    pub const BROWSER: &'static str = "browser";
    /// A whole display, driven through computer use.
    pub const DESKTOP: &'static str = "desktop";
    /// A simulated device's window.
    pub const SIMULATOR: &'static str = "simulator";
}

/// A place in a thread's log, or in a worker's thread table.
///
/// `seq` only grows. `epoch` changes only when the log is rewritten (a rewind, a fork, a
/// resync from the agent's own session), and a cursor of another epoch is answered with a
/// fresh snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Cursor {
    /// The log's generation.
    pub epoch: u64,
    /// The actions applied in it.
    pub seq: u64,
}

#[cfg(test)]
mod tests;
