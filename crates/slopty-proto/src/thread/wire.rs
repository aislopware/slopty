//! What a client asks of a worker's threads and what it is sent.
//!
//! - **The thread table.** One small replicated table per worker, a [`ThreadRow`] per thread, with
//!   a cursor of its own ([`ThreadRequest::Table`], [`TableFrame`]), so every list (the navigator,
//!   the inbox, the board) is right the moment a link comes back without following anything.
//! - **Following a thread.** [`ThreadRequest::Follow`] with the cursor the client holds is answered
//!   on a stream of the thread's own by the actions after it, when the log still holds them in the
//!   same epoch, else by a [`ThreadFrame::Snapshot`] of the last turns, then live
//!   [`ThreadFrame::Actions`]. Older turns come by [`ThreadRequest::Page`].
//! - **Intents.** Everything a client asks a thread to do is an [`Intent`] under a client-made
//!   [`IntentId`]. The worker acts on an id once and answers each with an [`IntentDone`]; a repeat
//!   gets the first answer again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs};

use super::{
    Action, AgentId, AskId, Cap, Changed, Choice, ContentRef, Cursor, Delivery, Drive, IntentId,
    Item, ItemId, Link, Status, ThreadId, ThreadState, Turn, TurnId,
};

/// What a client asks of a worker's threads.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ThreadRequest {
    /// Send the thread table: the rows changed since `have`, or all of them.
    Table {
        /// The table cursor the client holds.
        have: Option<Cursor>,
    },
    /// Stream a thread to this client.
    Follow {
        /// The thread.
        thread: ThreadId,
        /// The cursor the client holds of it, from a cache or an earlier follow.
        have: Option<Cursor>,
        /// How many of the last turns a snapshot carries.
        turns: u32,
        /// How long the worker may hold appends to send them together; 0 sends each at once.
        max_latency_ms: u32,
    },
    /// Stop streaming a thread.
    Unfollow {
        /// The thread.
        thread: ThreadId,
    },
    /// Send the turns before `before`.
    Page {
        /// The thread.
        thread: ThreadId,
        /// The first turn the client holds.
        before: TurnId,
        /// How many turns.
        turns: u32,
    },
    /// Send the whole of a clipped text or a picture.
    Expand {
        /// The thread.
        thread: ThreadId,
        /// What.
        content: ContentRef,
    },
    /// Start a thread.
    Start {
        /// The intent's id.
        id: IntentId,
        /// What to start.
        start: Box<Start>,
    },
    /// Act on a thread.
    Intent {
        /// The intent's id.
        id: IntentId,
        /// The thread.
        thread: ThreadId,
        /// What to do.
        intent: Intent,
    },
    /// Whether this client answers requests: while one does, the worker holds a request for
    /// the person instead of giving it straight back to the agent's own prompt.
    Approvals {
        /// On or off.
        on: bool,
    },
}

/// A thread to start.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Start {
    /// Its agent.
    pub agent: AgentId,
    /// Where it works.
    pub cwd: String,
    /// How to reach it; the agent's default when `None`.
    pub drive: Option<Drive>,
    /// What to tell it first.
    pub prompt: Option<String>,
    /// The model, by the agent's own id.
    pub model: Option<String>,
    /// More arguments for the agent, checked by its adapter.
    pub args: Vec<String>,
}

/// Something a client asks a thread to do.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Intent {
    /// Send a message.
    Send {
        /// What it says.
        text: String,
        /// When it goes.
        delivery: Delivery,
    },
    /// Take back a message that has not gone yet.
    Withdraw {
        /// The intent that sent it.
        pending: IntentId,
    },
    /// Change a message that has not gone yet.
    Edit {
        /// The intent that sent it.
        pending: IntentId,
        /// What it says now.
        text: String,
    },
    /// Stop the turn under way.
    Interrupt,
    /// Answer a request.
    Answer {
        /// The request.
        ask: AskId,
        /// The choice taken ([`Choice::id`]), or what was written for a question.
        choice: String,
        /// Words to go with it, where the agent takes them.
        message: Option<String>,
    },
    /// Give a request back to the agent's own prompt.
    Release {
        /// The request.
        ask: AskId,
    },
    /// Switch the model, by the agent's own id.
    SetModel {
        /// The model.
        model: String,
    },
    /// Switch the permission mode, by the agent's own name.
    SetMode {
        /// The mode.
        mode: String,
    },
    /// Compact the context.
    Compact,
    /// Stop one background task.
    StopTask {
        /// The task.
        task: String,
    },
}

impl Intent {
    /// The capability a thread needs for this intent.
    #[must_use]
    pub const fn needs(&self) -> &'static str {
        match self {
            Self::Send { delivery: Delivery::Steer, .. } => Cap::STEER,
            Self::Send { delivery: Delivery::Queue, .. }
            | Self::Withdraw { .. }
            | Self::Edit { .. } => Cap::QUEUE,
            Self::Interrupt => Cap::INTERRUPT,
            Self::Answer { .. } | Self::Release { .. } => Cap::APPROVALS,
            Self::SetModel { .. } => Cap::SET_MODEL,
            Self::SetMode { .. } => Cap::SET_MODE,
            Self::Compact => Cap::COMPACT,
            Self::StopTask { .. } => Cap::STOP_TASK,
        }
    }
}

/// The worker's answer to an intent. A repeat of an id is answered with the first answer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct IntentDone {
    /// The intent.
    pub id: IntentId,
    /// How it went.
    pub outcome: Outcome,
}

/// How an intent went.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Outcome {
    /// Done.
    Done,
    /// Taken, and held on the worker until its moment: a queued message, a send behind a draft.
    Accepted,
    /// A thread was started.
    Started {
        /// It.
        thread: ThreadId,
    },
    /// Not done, and why, in words.
    Refused {
        /// Why.
        reason: String,
    },
    /// The thread's agent cannot do it through Slopty.
    Unsupported {
        /// What it lacks.
        cap: Cap,
    },
}

/// What a followed thread's stream carries.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ThreadFrame {
    /// The thread as it is, from its last turns: a client replaces what it holds.
    Snapshot {
        /// Where in the log it stands.
        cursor: Cursor,
        /// The state.
        state: Box<ThreadState>,
    },
    /// Actions, in order, that take a client holding `first` of `epoch` to `next`.
    ///
    /// There may be fewer than `next - first`: the worker merges appends to the same part that
    /// come together, which leaves the state as their run one by one would.
    Actions {
        /// The log's epoch.
        epoch: u64,
        /// The cursor's `seq` they apply to.
        first: u64,
        /// Its `seq` after them.
        next: u64,
        /// The actions.
        actions: Vec<Action>,
    },
    /// Older turns, asked for by [`ThreadRequest::Page`].
    Page(Page),
    /// The whole of something clipped.
    Expanded {
        /// What was asked for.
        content: ContentRef,
        /// It.
        body: Expanded,
    },
}

/// Older turns of a thread.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Page {
    /// The turns, oldest first.
    pub turns: Vec<Turn>,
    /// Their items, in order.
    pub items: Vec<Item>,
    /// There are turns before these.
    pub older: bool,
}

/// The whole of something clipped.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Expanded {
    /// A text.
    Text(String),
    /// A picture's bytes.
    Bytes(#[serde(with = "serde_bytes")] Vec<u8>),
    /// It is no longer there.
    Gone,
}

/// The thread table, whole or changed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TableFrame {
    /// Every row: a client replaces what it holds.
    Snapshot {
        /// Where the table stands.
        cursor: Cursor,
        /// The rows.
        rows: Vec<ThreadRow>,
    },
    /// The rows changed and gone since the client's cursor.
    Delta {
        /// Where the table stands now.
        cursor: Cursor,
        /// Rows new or changed.
        rows: Vec<ThreadRow>,
        /// Threads gone.
        removed: Vec<ThreadId>,
    },
}

/// What every list needs of a thread without following it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadRow {
    /// The thread.
    pub id: ThreadId,
    /// Its agent.
    pub agent: AgentId,
    /// What it is about.
    pub title: String,
    /// Where it is.
    pub status: Status,
    /// Its open requests, as cards a list can answer.
    pub requests: Vec<RequestCard>,
    /// The last line the agent wrote.
    pub last_line: Option<String>,
    /// Lines added and removed over the turns the worker holds.
    pub changed: Changed,
    /// The terminal its TUI runs in.
    pub terminal: Option<SessionId>,
    /// For a subagent: where it hangs.
    pub parent: Option<Link>,
    /// How Slopty reaches it.
    pub drive: Drive,
    /// What it can do through Slopty.
    pub caps: Vec<Cap>,
    /// Its open facts.
    pub facts: BTreeMap<String, String>,
    /// When it last changed.
    pub updated_ms: WallMs,
}

/// An open request, small enough for a list, a notification or the inbox to answer.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RequestCard {
    /// The request.
    pub id: AskId,
    /// The call it is about.
    pub item: Option<ItemId>,
    /// What it asks ([`super::Request::kind`]).
    pub kind: String,
    /// In a line.
    pub title: String,
    /// The answers the agent offers.
    pub options: Vec<Choice>,
    /// When it opened.
    pub opened_ms: WallMs,
}
