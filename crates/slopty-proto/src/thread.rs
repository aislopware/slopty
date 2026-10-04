//! The agent-neutral thread model: what any coding agent's session looks like to a client, and
//! the one way it changes.
//!
//! Every agent (Claude Code observed through its TUI, Codex over its app-server, pi over its RPC
//! mode, any ACP agent) is mapped on the worker by its adapter into the same shape: a thread of
//! turns, each turn a run of items (what the person sent, what the agent wrote, the tools it
//! called), beside the requests it holds open for the person, the messages waiting to go to it,
//! and its plan, background work and meters. The shape follows Codex's thread, turn and item
//! primitives; how it is kept in step on many clients follows Microsoft's Agent Host Protocol
//! (`docs/decisions/agents.md`).
//!
//! **One mutation.** A thread changes only by an [`Action`]. [`ThreadState::apply`] is pure and
//! is the same code on the worker, which keeps the thread's log, and on every client, which
//! mirrors it, so the two can never disagree about what a run of actions means.
//!
//! **Open where agents differ.** What an agent is ([`AgentId`]), what it can do ([`Cap`]), how
//! Slopty reaches it ([`Drive`]), what kind of tool it called ([`ToolCall::kind`]), what a
//! request asks ([`Request::kind`]) and which tokens it counts ([`Usage`]) are open strings with
//! known values named here, never closed enums: a new agent brings its own without a wire
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
use slopty_core::{ClientId, SessionId, WallMs};
use uuid::Uuid;

pub mod attention;
pub mod detail;
mod reduce;
pub mod wire;

pub use detail::{Clipped, ContentRef, Image, Patch, ToolDetail};
pub use reduce::{RESOLVED_KEPT, TableState, ThreadState};

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

/// How Slopty reaches an agent. Open; the known ones are [`Drive::OBSERVED`],
/// [`Drive::SHARED`] and [`Drive::DRIVEN`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Drive(pub String);

impl Drive {
    /// Slopty runs the agent over its protocol, and its TUI takes over by a handoff at idle.
    pub const DRIVEN: &'static str = "driven";
    /// Its own TUI runs in a terminal and Slopty reads what it publishes (hooks, transcript,
    /// mod), typing into the TUI only on the person's word.
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

/// Something a thread's agent can do through Slopty. Open: an adapter declares the set its
/// agent has, and a client shows a control only where its capability is present.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Cap(pub String);

impl Cap {
    /// Requests are answered through Slopty ([`wire::Intent::Answer`]).
    pub const APPROVALS: &'static str = "approvals";
    /// [`wire::Intent::Compact`].
    pub const COMPACT: &'static str = "compact";
    /// [`wire::Intent::Continue`]: a new thread, on any agent here, that goes on from this one
    /// with a portable account of it as its first message, held for the person to send.
    pub const CONTINUE: &'static str = "continue";
    /// [`wire::Intent::Fork`]: a new thread branched off this one.
    pub const FORK: &'static str = "fork";
    /// The agent's own TUI takes the session over by a handoff.
    pub const HANDOFF: &'static str = "handoff";
    /// [`wire::Intent::Interrupt`].
    pub const INTERRUPT: &'static str = "interrupt";
    /// Text streams as the model writes it ([`Action::Append`]).
    pub const LIVE_TEXT: &'static str = "live-text";
    /// The agent's own TUI is live beside the face.
    pub const LIVE_TUI: &'static str = "live-tui";
    /// [`wire::Intent::Send`] with [`Delivery::Queue`], held until the turn ends, and the
    /// editing of what is held.
    pub const QUEUE: &'static str = "queue";
    /// [`wire::Intent::Rewind`]: the agent branches its session before an earlier turn through
    /// its own door.
    pub const REWIND: &'static str = "rewind";
    /// [`wire::Intent::Send`] with a delivery the worker keeps ([`Delivery::is_kept`]): it holds
    /// the message until its moment, or the person's word for a draft.
    pub const SCHEDULE: &'static str = "schedule";
    /// [`wire::Intent::SetMode`].
    pub const SET_MODE: &'static str = "set-mode";
    /// [`wire::Intent::SetModel`].
    pub const SET_MODEL: &'static str = "set-model";
    /// Its agent can be put to sleep at rest and woken on its own session: its process ends and
    /// the thread is kept ([`Liveness::Asleep`]).
    pub const SLEEP: &'static str = "sleep";
    /// The worker snapshots the working tree at each turn edge ([`Action::Snapshot`]).
    pub const SNAPSHOTS: &'static str = "snapshots";
    /// [`wire::Intent::Send`] with [`Delivery::Steer`]: a message taken mid-turn.
    pub const STEER: &'static str = "steer";
    /// [`wire::Intent::StopTask`].
    pub const STOP_TASK: &'static str = "stop-task";

    /// The capability named `name`.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self(name.to_owned())
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
    /// The terminal its TUI runs in, when it has one.
    pub terminal: Option<SessionId>,
    /// For a subagent: the thread and the call that started it.
    pub parent: Option<Link>,
    /// How it came to be. Open: [`ThreadMeta::PERSON`], [`ThreadMeta::SUBAGENT`],
    /// [`ThreadMeta::FORK`], [`ThreadMeta::ORCHESTRATED`], [`ThreadMeta::AUTOMATION`].
    pub origin: String,
    /// For a fork: where it branched.
    pub forked_from: Option<Fork>,
    /// How Slopty reaches it.
    pub drive: Drive,
    /// What it can do through Slopty, sorted.
    pub caps: Vec<Cap>,
    /// The models it can be switched to ([`wire::Intent::SetModel`]), from its adapter's
    /// catalogue; empty where it cannot be.
    pub models: Vec<Model>,
    /// The modes it can be switched to ([`wire::Intent::SetMode`]), as its agent publishes
    /// them (an ACP agent's session modes); empty where it publishes none.
    pub modes: Vec<Mode>,
    /// Open facts about it: its project, task, branch, pull request, model.
    pub facts: BTreeMap<String, String>,
    /// When it began.
    pub created_ms: WallMs,
}

impl ThreadMeta {
    /// Started by an automation.
    pub const AUTOMATION: &'static str = "automation";
    /// Branched from another thread.
    pub const FORK: &'static str = "fork";
    /// Started by a project's orchestrator.
    pub const ORCHESTRATED: &'static str = "orchestrated";
    /// Started by the person.
    pub const PERSON: &'static str = "person";
    /// Started by another thread's call.
    pub const SUBAGENT: &'static str = "subagent";

    /// Whether the thread's agent can do `cap`.
    #[must_use]
    pub fn can(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.0 == cap)
    }
}

/// A model a thread's agent can be switched to.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Model {
    /// The agent's own name for it, as its switch takes it (`opus`, a full model id).
    pub id: String,
    /// Its name for people.
    pub label: String,
}

/// A mode a thread's agent can be switched to: how it asks before acting (`plan`, `ask`,
/// `code`), by its own name.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Mode {
    /// The agent's own name for it, as its switch takes it.
    pub id: String,
    /// Its name for people.
    pub label: String,
    /// What it does, in the agent's words, when it says.
    pub description: Option<String>,
}

/// Where a subagent's thread hangs: the thread and the call that started it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Link {
    /// The parent thread.
    pub thread: ThreadId,
    /// The call in it.
    pub item: ItemId,
}

/// Where a fork branched.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Fork {
    /// The thread it came from.
    pub thread: ThreadId,
    /// The last turn it shares with it, when that is known: a fork the agent made on its own
    /// names only the thread it came from.
    pub turn: Option<TurnId>,
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
    /// Waiting on the person: a request is open, or it asked something.
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
    /// Open: `permission`, `question`, `plan`, `input`, `task`, `wakeup`.
    pub kind: String,
    /// What it waits for, worded by the adapter ("Wants to run cargo test").
    pub text: String,
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
    /// It sleeps until a wakeup.
    Sleeping {
        /// When it wakes.
        until_ms: WallMs,
    },
    /// It runs but has said nothing for a while.
    Silent {
        /// Since when.
        since_ms: WallMs,
    },
    /// Put to sleep on the person's word: its agent was ended at rest, and the thread is kept
    /// with its session, which a wake or the next message takes up again.
    Asleep {
        /// Since when.
        since_ms: WallMs,
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
    /// The working tree when it began, when the worker snapshots it.
    pub before: Option<TreeRef>,
    /// And when it ended.
    pub after: Option<TreeRef>,
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
        /// When what stopped it lifts, where the agent says: the reset of a usage limit it hit.
        until_ms: Option<WallMs>,
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

/// What a turn used, by kind: the tokens, and what they cost where the agent says it.
///
/// Open: each agent counts its own (Claude Code reads and writes a cache, Codex counts
/// reasoning), so the kinds are keys, with the common ones named here.
/// [`Usage::COST_MICRO_USD`] is not a token count, so a sum of tokens leaves it out
/// ([`Usage::tokens`]).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Usage(pub BTreeMap<String, u64>);

impl Usage {
    /// Input tokens read from the prompt cache.
    pub const CACHE_READ: &'static str = "cache-read";
    /// Input tokens written to the prompt cache.
    pub const CACHE_WRITE: &'static str = "cache-write";
    /// What the turn cost, in millionths of a US dollar.
    pub const COST_MICRO_USD: &'static str = "cost-micro-usd";
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

    /// The tokens counted, of every kind: everything but the cost.
    #[must_use]
    pub fn tokens(&self) -> u64 {
        self.0
            .iter()
            .filter(|(kind, _)| kind.as_str() != Self::COST_MICRO_USD)
            .fold(0, |sum, (_, n)| sum.saturating_add(*n))
    }

    /// Add `other`'s counts to these.
    pub fn add(&mut self, other: &Self) {
        for (kind, n) in &other.0 {
            let have = self.0.entry(kind.clone()).or_insert(0);
            *have = have.saturating_add(*n);
        }
    }
}

/// A snapshot of the working tree: a git tree id in the worker's private refs.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct TreeRef(pub String);

/// Which edge of a turn a snapshot was taken at.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Edge {
    /// As the turn began.
    Before,
    /// As it ended.
    After,
}

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
    /// The intent that sent it, when Slopty did: a client's pending message turns into this.
    pub intent: Option<IntentId>,
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
    /// Open: [`Notice::API_ERROR`], [`Notice::COMMAND`], [`Notice::INFO`], [`Notice::HOOK`],
    /// [`Notice::INTERRUPTED`], [`Notice::LIMIT`], [`Notice::REWOUND`].
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
    /// The API failed, or a request is being retried.
    pub const API_ERROR: &'static str = "api-error";
    /// What a command printed.
    pub const COMMAND: &'static str = "command";
    /// A hook spoke or stopped the turn.
    pub const HOOK: &'static str = "hook";
    /// Something the agent wanted to say.
    pub const INFO: &'static str = "info";
    /// The person stopped the agent.
    pub const INTERRUPTED: &'static str = "interrupted";
    /// The agent stopped on a usage limit: its words, and the turn says when it resets.
    pub const LIMIT: &'static str = "limit";
    /// The person went back to an earlier message; what came after it is gone.
    pub const REWOUND: &'static str = "rewound";

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

/// Something the agent holds open for the person: an approval, a question, a plan, a form.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Request {
    /// Its id.
    pub id: AskId,
    /// The call it is about, when it is about one.
    pub item: Option<ItemId>,
    /// What it asks. Open: [`Request::APPROVAL`], [`Request::QUESTION`], [`Request::PLAN`],
    /// [`Request::ELICITATION`].
    pub kind: String,
    /// What it asks, in a line ("Run cargo test?").
    pub title: String,
    /// More of it, when there is more.
    pub text: Option<Clipped>,
    /// The answers the agent offers, in its order. Slopty never invents one.
    pub options: Vec<Choice>,
    /// The questions, for a question.
    pub questions: Vec<detail::Question>,
    /// The change it would make, for an approval of an edit.
    pub proposed: Option<Patch>,
    /// The parts of the call's input the person may change before allowing it, where the
    /// agent takes an allow with the input changed ([`Editable::choice`]).
    pub editable: Vec<Editable>,
    /// For a form: its JSON schema.
    pub schema_json: Option<String>,
    /// For a form the person fills in elsewhere: where.
    pub url: Option<String>,
    /// How it stands.
    pub state: RequestState,
    /// When it opened.
    pub opened_ms: WallMs,
    /// When the worker stops holding it and gives it back to the agent's own prompt.
    pub until_ms: Option<WallMs>,
}

impl Request {
    /// May the agent do something.
    pub const APPROVAL: &'static str = "approval";
    /// A form to fill in.
    pub const ELICITATION: &'static str = "elicitation";
    /// A plan to approve.
    pub const PLAN: &'static str = "plan";
    /// Questions for the person.
    pub const QUESTION: &'static str = "question";

    /// Whether it still waits on an answer.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        matches!(self.state, RequestState::Open)
    }
}

/// An answer an agent offers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Choice {
    /// Its id, as the agent knows it.
    pub id: String,
    /// What it says.
    pub label: String,
    /// What it means.
    pub effect: Effect,
    /// How far it reaches beyond this once, in the agent's words ("this session",
    /// "Bash(cargo test:*)").
    pub scope: Option<String>,
    /// It also stops the turn.
    pub stops: bool,
}

/// A part of a call's input the person may change before allowing it: an edit's new text, a
/// written file's content.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Editable {
    /// The input's field, as the agent names it (`new_string`, `content`).
    pub field: String,
    /// What the agent proposed for it.
    pub text: String,
}

impl Editable {
    /// The key of the JSON object an edited allow is ([`Editable::choice`]).
    const EDITED: &'static str = "allow-edited";
    /// The longest text offered for editing, in bytes: a longer proposal is allowed or denied
    /// whole.
    pub const TEXT_MAX: usize = 256 * 1024;

    /// The choice of an [`Intent::Answer`](wire::Intent::Answer) that allows the call with
    /// `fields` changed, each a field and its text as the person left it: a JSON object, which
    /// no other choice is.
    #[must_use]
    pub fn choice(fields: &BTreeMap<String, String>) -> String {
        let edited: BTreeMap<&str, &BTreeMap<String, String>> =
            BTreeMap::from([(Self::EDITED, fields)]);
        serde_json::to_string(&edited).unwrap_or_default()
    }

    /// The fields an edited allow's [`choice`](Self::choice) changes; `None` for any other
    /// choice.
    #[must_use]
    pub fn read(choice: &str) -> Option<BTreeMap<String, String>> {
        if !choice.starts_with('{') {
            return None;
        }
        let mut edited: BTreeMap<String, BTreeMap<String, String>> =
            serde_json::from_str(choice).ok()?;
        edited.remove(Self::EDITED)
    }
}

/// What an answer means, so a client can tell yes from no without knowing the agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Effect {
    /// Go ahead.
    Allow,
    /// Do not.
    Deny,
    /// Neither: an answer to a question, a choice in a form.
    Answer,
}

/// How a request stands.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RequestState {
    /// It waits.
    Open,
    /// Someone answered it.
    Answered {
        /// Who.
        by: Answerer,
        /// The choice taken ([`Choice::id`]), or what was written.
        choice: String,
    },
    /// Slopty gave it back to the agent's own prompt (the TUI's dialog).
    Released,
    /// The agent stopped asking.
    Withdrawn,
}

/// Who answered a request.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Answerer {
    /// The client, when one did.
    pub client: Option<ClientId>,
    /// Its name, or where the answer came from ("terminal").
    pub name: String,
}

/// A message on its way to the agent, held by the worker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Pending {
    /// The intent that sent it.
    pub intent: IntentId,
    /// What it says.
    pub text: String,
    /// The files sent with it, by their paths on the worker ([`wire::Intent::Send`]).
    pub attachments: Vec<String>,
    /// When it goes.
    pub delivery: Delivery,
    /// Where it is.
    pub state: PendingState,
}

impl Pending {
    /// Move the message of intent `which` in `list` to just before the one of `before`, or to
    /// the end when `before` is `None` ([`wire::Intent::Reorder`]); `id` names each entry's
    /// intent. Whether both were there: when not, `list` is as it was.
    pub fn reorder<T>(
        list: &mut [T],
        id: impl Fn(&T) -> IntentId,
        which: IntentId,
        before: Option<IntentId>,
    ) -> bool {
        let Some(from) = list.iter().position(|t| id(t) == which) else { return false };
        let to = match before {
            None => list.len(),
            Some(before) => match list.iter().position(|t| id(t) == before) {
                Some(to) => to,
                None => return false,
            },
        };
        // Moved right, it lands before what was at `to`, which shifts left as it leaves.
        if from < to {
            if let Some(run) = list.get_mut(from..to) {
                run.rotate_left(1);
            }
        } else if let Some(run) = list.get_mut(to..=from) {
            run.rotate_right(1);
        }
        true
    }
}

/// When a message goes to the agent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Delivery {
    /// Now, into the turn under way, which the agent takes at its next step.
    Steer,
    /// When the turn under way ends.
    Queue,
    /// At a time: held on the worker until then, through a restart, and then queued
    /// ([`Cap::SCHEDULE`]).
    At {
        /// When.
        at_ms: WallMs,
    },
    /// Once another thread has rested for a while: held on the worker until thread `thread`
    /// has been at rest for `settle_ms`, its turn ended and nothing asked, and then queued
    /// ([`Cap::SCHEDULE`]).
    After {
        /// The thread waited on.
        thread: ThreadId,
        /// How long it rests first.
        settle_ms: u32,
    },
    /// On the person's word alone: a draft held on the worker, through a restart, never sent
    /// on its own. Sending it ([`wire::Intent::Promote`]) queues it; it can be changed or
    /// withdrawn until then ([`Cap::SCHEDULE`]). A continued thread's first message waits so
    /// ([`wire::Intent::Continue`]).
    Draft,
    /// Now, for an agent with no steer of its own ([`Cap::STEER`]): the worker puts the
    /// message first in the agent's queue and stops the turn under way, so it goes as that
    /// turn ends. It needs [`Cap::INTERRUPT`] and [`Cap::QUEUE`]; with no turn under way it
    /// goes as a queued one does.
    Interrupt,
}

impl Delivery {
    /// Whether the worker keeps the message ([`Self::At`], [`Self::After`], [`Self::Draft`]),
    /// rather than the agent's queue.
    #[must_use]
    pub const fn is_kept(self) -> bool {
        matches!(self, Self::At { .. } | Self::After { .. } | Self::Draft)
    }

    /// Whether the message goes on its own at a moment the worker watches for ([`Self::At`],
    /// [`Self::After`]).
    #[must_use]
    pub const fn is_scheduled(self) -> bool {
        matches!(self, Self::At { .. } | Self::After { .. })
    }
}

/// Where a pending message is.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PendingState {
    /// It waits for its moment.
    Waiting,
    /// Something is in its way, in words ("Your draft in the terminal is in the way").
    Held {
        /// What.
        reason: String,
    },
    /// The worker is giving it to the agent.
    Sending,
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
    /// What kind of work it is. Open: [`BackgroundTask::SHELL`], [`BackgroundTask::AGENT`].
    pub kind: String,
    /// What it is.
    pub title: String,
    /// Open, as the agent says: `running`, `completed`, `failed`, `killed`.
    pub state: String,
    /// The call that started it.
    pub item: Option<ItemId>,
    /// The end of what it printed.
    pub output: Option<Clipped>,
    /// When it began.
    pub started_ms: WallMs,
    /// When it ended, once it has and the agent says when.
    pub ended_ms: Option<WallMs>,
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
    /// It runs.
    pub const RUNNING: &'static str = "running";
    /// A command left running.
    pub const SHELL: &'static str = "shell";

    /// Whether it still runs.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state == Self::RUNNING
    }
}

/// A thread's meters: its model, context and spend.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Meters {
    /// The model's name for people.
    pub model: Option<String>,
    /// The model's id.
    pub model_id: Option<String>,
    /// The permission mode, as the agent names it, shown and never set by typing keys.
    pub mode: Option<String>,
    /// How hard the model thinks, as the agent names it (Codex's reasoning effort, pi's thinking
    /// level, an ACP agent's thought level).
    pub effort: Option<String>,
    /// Tokens in the context.
    pub context_tokens: Option<u64>,
    /// The context window.
    pub context_window: Option<u64>,
    /// What the session cost, in millionths of a US dollar.
    pub cost_micro_usd: Option<u64>,
    /// The plan's rate windows.
    pub limits: Vec<Limit>,
}

/// One of a plan's rate windows.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Limit {
    /// Open, as the agent names it: `five-hour`, `seven-day`.
    pub name: String,
    /// How much is used, in hundredths of a percent.
    pub used_bp: u32,
    /// When it resets.
    pub resets_ms: Option<WallMs>,
}

/// A command the agent takes in its composer (`/compact`).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Command {
    /// Its name, without the slash.
    pub name: String,
    /// What it does.
    pub description: String,
    /// What its arguments look like.
    pub argument_hint: Option<String>,
    /// Open, where it comes from: `built-in`, `personal`, `project`, `plugin`.
    pub source: String,
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
    /// A request opened, or changed.
    RequestOpened(Box<Request>),
    /// A request was settled.
    RequestResolved {
        /// The request.
        id: AskId,
        /// How.
        state: RequestState,
    },
    /// The messages waiting to go, in their order.
    PendingSet(Vec<Pending>),
    /// The plan.
    PlanSet(Option<Plan>),
    /// The background work.
    TasksSet(Vec<BackgroundTask>),
    /// The meters.
    MetersSet(Meters),
    /// The commands the composer offers.
    CommandsSet(Vec<Command>),
    /// The turns after `after` are gone (a rewind, a fork); `None` empties the thread.
    Truncated {
        /// The last turn kept.
        after: Option<TurnId>,
    },
    /// The working tree was snapshotted at a turn's edge.
    Snapshot {
        /// The turn.
        turn: TurnId,
        /// Which edge.
        edge: Edge,
        /// The tree.
        tree: TreeRef,
    },
    /// Whether the working tree, as last snapshotted, holds changes the person has not kept:
    /// the worker compares it with what they kept, or the thread's first snapshot when they
    /// have kept nothing.
    ToReview(bool),
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
