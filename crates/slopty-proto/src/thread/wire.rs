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
    Item, ItemId, Link, Patch, Status, ThreadId, ThreadState, TreeRef, Turn, TurnId,
};
use crate::search::Span;

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
    /// What changed in the working tree of a followed thread over `scope`, sent on its
    /// stream as a [`ThreadFrame::Review`].
    Review {
        /// The thread.
        thread: ThreadId,
        /// Over what.
        scope: ReviewScope,
    },
    /// Past sessions of this machine's agents, from each agent's own record of them; answered
    /// with [`PastSessions`] on the control stream.
    ///
    /// With no `query` and both an agent and a folder, the agent lists its sessions there itself,
    /// newest first: every session it keeps, prompted or not. Otherwise the person's past prompts
    /// as each agent records them (Claude Code's and Codex's prompt histories, pi's session
    /// files) answer: the sessions whose prompts hold every word of `query`, best match first,
    /// each with the prompts that matched; with no `query`, the sessions prompted last first,
    /// each with its last prompt.
    Sessions {
        /// The agent; every agent whose prompts the worker reads when `None`.
        agent: Option<AgentId>,
        /// The folder, as a [`Start`] names it; every folder when `None`.
        cwd: Option<String>,
        /// Words to find in the person's past prompts; empty for none.
        query: String,
        /// The most sessions to list.
        limit: u32,
    },
    /// What was said in the threads this worker holds: every word of `query` in the person's
    /// messages, the agent's answers and reasoning, its calls' titles and its notices, as the
    /// worker's log of each thread keeps them (a text clipped there is searched as far as it
    /// is kept). Answered with [`ThreadHits`] on the control stream.
    Search {
        /// The words; nothing is found for none.
        query: String,
        /// The most threads to answer with, up to [`SEARCH_THREADS`].
        limit: u32,
    },
    /// Which threads wrote the lines of a file as it is on the worker now; answered with
    /// [`Authors`] on the control stream.
    Authors {
        /// The thread whose repository a relative `path` is in; `None` for an absolute one.
        thread: Option<ThreadId>,
        /// The file: absolute, or relative to `thread`'s repository.
        path: String,
    },
}

/// What wrote the lines of a file, as a [`ThreadRequest::Authors`] asked.
///
/// A line is a thread's when the worker's snapshots of one of its turns show that turn bring it
/// in, or, past what the snapshots keep, when the commit that brought it carries the thread in a
/// `Slopty-Thread` trailer. A client keeps the answer for the file as it was read
/// ([`Authors::modified_ms`], [`Authors::blob`]) and asks again only once it changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Authors {
    /// The thread, as asked.
    pub thread: Option<ThreadId>,
    /// The file, as asked.
    pub path: String,
    /// When the file read was last changed.
    pub modified_ms: Option<WallMs>,
    /// The id git gives the file read, as a review's [`FileDiff`] names its sides.
    pub blob: Option<String>,
    /// Runs of lines and who wrote them, in order; a line no thread is known to have written
    /// is in none.
    pub runs: Vec<AuthorRun>,
    /// Why nothing could be said of the file, in words: not in git, unreadable.
    pub absent: Option<String>,
}

/// Lines of a file that one thread wrote ([`Authors`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AuthorRun {
    /// The first line, from 1.
    pub start: u32,
    /// How many lines.
    pub lines: u32,
    /// The thread that wrote them. It may be another worker's, known from a commit.
    pub thread: ThreadId,
    /// Its turn that brought them in, while the worker keeps that turn's snapshots.
    pub turn: Option<TurnId>,
    /// The commit that brought them in, where only its trailer names the thread.
    pub commit: Option<String>,
    /// When they were written: the turn's end, or the commit's.
    pub at_ms: WallMs,
}

/// The most threads a [`ThreadRequest::Search`] answers with.
pub const SEARCH_THREADS: u32 = 50;

/// The most of a thread's items with a match a [`ThreadHit`] carries.
pub const HITS_PER_THREAD: usize = 3;

/// How much of an item an [`ItemHit`] carries, in bytes: the part round its first match.
pub const ITEM_HIT_BYTES: usize = 240;

/// What a [`ThreadRequest::Search`] found.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadHits {
    /// The words asked for, as they were asked.
    pub query: String,
    /// The threads with a match, the best first: by their best item's match, then by when it
    /// was said, the latest first.
    pub threads: Vec<ThreadHit>,
    /// The threads with a match left out past the limit.
    pub more: u32,
}

/// A thread with a match. Its title, agent and folder are its row in the table.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadHit {
    /// The thread.
    pub thread: ThreadId,
    /// Its items with a match, the best first, at most [`HITS_PER_THREAD`].
    pub hits: Vec<ItemHit>,
    /// Its items with a match left out.
    pub more: u32,
}

/// An item that matched, as much of it as shows where.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ItemHit {
    /// The item, to scroll the thread to.
    pub item: ItemId,
    /// Its turn, to page the thread back to.
    pub turn: TurnId,
    /// Open, what kind of words they are: [`ItemHit::PERSON`], [`ItemHit::AGENT`],
    /// [`ItemHit::REASONING`], [`ItemHit::TOOL`], [`ItemHit::NOTICE`].
    pub said: String,
    /// The words, cut to [`ITEM_HIT_BYTES`] round the first match.
    pub text: String,
    /// Where the words asked for are in [`Self::text`], in order, never overlapping.
    pub spans: Vec<Span>,
    /// Text of the item before [`Self::text`] was cut off.
    pub cut_before: bool,
    /// Text of the item after [`Self::text`] was cut off.
    pub cut_after: bool,
    /// When it was said.
    pub at_ms: WallMs,
}

impl ItemHit {
    /// The agent's answer.
    pub const AGENT: &'static str = "agent";
    /// A notice of the agent's own.
    pub const NOTICE: &'static str = "notice";
    /// The person's message.
    pub const PERSON: &'static str = "person";
    /// The agent's reasoning.
    pub const REASONING: &'static str = "reasoning";
    /// A call's title.
    pub const TOOL: &'static str = "tool";
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
        /// Files sent with it: absolute paths on the worker, where an upload landed them
        /// (`transfer::Dest::Attachment`, which needs no terminal) or where they already were.
        /// Each adapter gives them to its agent in the agent's own form: a picture as a picture
        /// where the agent takes one, any other file by its path.
        attachments: Vec<String>,
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
    /// Send a queued message now, into the turn under way, as a steer would go
    /// ([`Cap::STEER`]); with no turn under way it starts one.
    Promote {
        /// The intent that sent it.
        pending: IntentId,
    },
    /// Stop the turn under way.
    Interrupt,
    /// Answer a request.
    Answer {
        /// The request.
        ask: AskId,
        /// The choice taken ([`Choice::id`]), or the answers to a request's questions.
        ///
        /// One answer answers all of a request's questions
        /// ([`detail::Answer::choice`](super::detail::Answer::choice)): the words typed, for
        /// a lone question that offers nothing; otherwise a JSON list of
        /// [`detail::Answer`](super::detail::Answer), one per question keyed by its text,
        /// several picks of one question joined with `", "` and the words of one's own last
        /// ([`detail::Answer::JOIN`](super::detail::Answer::JOIN)). Claude Code takes the list
        /// as `AskUserQuestion`'s answers as they are; an adapter whose agent takes picks apart
        /// splits them again ([`detail::Answer::parts`](super::detail::Answer::parts)).
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
    /// Take a change into what the person has kept ([`ReviewScope::Kept`]): the whole file,
    /// or the hunks named. Refused when the file or what was kept no longer is what the
    /// review showed.
    Keep(Pick),
    /// Put a change back in the working tree as it was: the whole file, or the hunks named.
    /// Refused when the file no longer is what the review showed.
    Revert(Pick),
    /// Give the session to the agent's own TUI, in a terminal of the worker's, once the agent
    /// rests ([`Cap::HANDOFF`]).
    Handoff,
    /// Take the session back from the agent's own TUI once it rests, and drive it again.
    TakeBack,
    /// Branch a new thread off this one, sharing its turns through `after`, or all of them
    /// ([`Cap::FORK`]). Answered with [`Outcome::Started`] and the new thread, whose
    /// [`ThreadMeta::forked_from`](super::ThreadMeta::forked_from) says where it branched; this
    /// one goes on as it was.
    Fork {
        /// The last turn the new thread shares with this one; `None` for all of them.
        after: Option<TurnId>,
    },
    /// Go on from this thread in a new one on agent `agent` (this one's own, to start afresh)
    /// ([`Cap::CONTINUE`]). The new thread starts with nothing sent; the client that asked
    /// puts a pointer to this one in its composer (its id, folder and branch, and how to read
    /// it), for the person to send. Answered with [`Outcome::Started`] and the new thread,
    /// whose [`ThreadMeta::forked_from`](super::ThreadMeta::forked_from) names this one; this
    /// one goes on as it was.
    Continue {
        /// The agent the new thread runs.
        agent: AgentId,
    },
    /// Edit from turn `turn`: go back to just before it, in a new thread ([`Cap::REWIND`]).
    ///
    /// The agent branches its session before the turn through its own door, and the client
    /// that asked puts the turn's message in the new thread's composer for the person to
    /// change and send. With `files`, the folder goes back to the turn's before-snapshot too,
    /// what it held first kept under the thread's refs. Refused while a turn is under way.
    /// Answered with [`Outcome::Started`] and the new thread; this one goes on as it was, and
    /// no agent's session file is ever written.
    Rewind {
        /// The turn gone back to the start of.
        turn: TurnId,
        /// The folder goes back to the turn's before-snapshot too.
        files: bool,
    },
    /// Set how hard the model thinks, by the agent's own name for the level
    /// ([`ThreadMeta::efforts`](super::ThreadMeta::efforts)).
    SetEffort {
        /// The level.
        effort: String,
    },
    /// Ask aside: branch the whole thread into a new one marked as its aside
    /// ([`ThreadMeta::ASIDE_FACT`](super::ThreadMeta::ASIDE_FACT)), for a side question that
    /// stays out of this thread and shares its context ([`Cap::FORK`]). Answered with
    /// [`Outcome::Started`] and the aside, which takes the question as its first message.
    Aside,
    /// Close an aside for good: its agent ends and the worker forgets it. Its agent's session
    /// stays wherever the agent keeps it. Refused for a thread that is not an aside.
    Discard,
    /// Keep an aside as an ordinary thread of its own: it shows from then on.
    KeepAside,
    /// Ask the thread's agent for its own review of a change ([`Cap::REVIEW`]): what differs
    /// between two snapshots of its working tree, as a review showed them ([`Review::from`],
    /// [`Review::to`]). The agent reviews through its own door, as the person's turn, and its
    /// findings come back as its answer in the thread.
    Review {
        /// The old side.
        from: TreeRef,
        /// The new side.
        to: TreeRef,
    },
}

/// A file's change as a review showed it, or some of its hunks.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Pick {
    /// The file, from the repository's root.
    pub path: String,
    /// Its blob on the review's old side ([`FileDiff::from`]).
    pub from: Option<String>,
    /// Its blob on the new side, as the review showed it ([`FileDiff::to`]): the stamp the
    /// worker checks the file against.
    pub stamp: Option<String>,
    /// The hunks, by their place in [`FileDiff::patch`]; empty for the whole file.
    pub hunks: Vec<u32>,
}

impl Intent {
    /// The capability a thread needs for this intent.
    ///
    /// A send by interrupt ([`Delivery::Interrupt`]) needs [`Cap::QUEUE`] too.
    #[must_use]
    pub const fn needs(&self) -> &'static str {
        match self {
            Self::Send { delivery: Delivery::Steer, .. } | Self::Promote { .. } => Cap::STEER,
            Self::Send { delivery: Delivery::At { .. }, .. } => Cap::SCHEDULE,
            Self::Send { delivery: Delivery::Queue, .. }
            | Self::Withdraw { .. }
            | Self::Edit { .. } => Cap::QUEUE,
            Self::Send { delivery: Delivery::Interrupt, .. } | Self::Interrupt => Cap::INTERRUPT,
            Self::Answer { .. } | Self::Release { .. } => Cap::APPROVALS,
            Self::SetModel { .. } => Cap::SET_MODEL,
            Self::SetMode { .. } => Cap::SET_MODE,
            Self::SetEffort { .. } => Cap::SET_EFFORT,
            Self::Compact => Cap::COMPACT,
            Self::Handoff | Self::TakeBack => Cap::HANDOFF,
            Self::StopTask { .. } => Cap::STOP_TASK,
            Self::Fork { .. } | Self::Aside | Self::Discard | Self::KeepAside => Cap::FORK,
            Self::Keep(_) | Self::Revert(_) => Cap::SNAPSHOTS,
            Self::Review { .. } => Cap::REVIEW,
            Self::Continue { .. } => Cap::CONTINUE,
            Self::Rewind { .. } => Cap::REWIND,
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
    /// What changed over a scope, asked for by [`ThreadRequest::Review`].
    Review(Box<Review>),
}

/// The span of a thread's work a review covers, each a diff between two snapshots of the
/// working tree ([`Action::Snapshot`]); "now" is a snapshot taken for the review.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReviewScope {
    /// One turn: from its start to its end, or to now while it runs.
    Turn(TurnId),
    /// From a turn's start to now.
    Since(TurnId),
    /// From what the person has kept ([`Intent::Keep`]) to now; what is left to review.
    Kept,
}

/// A review: each file that differs between two trees.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Review {
    /// What was asked for.
    pub scope: ReviewScope,
    /// The old side.
    pub from: Option<TreeRef>,
    /// The new side.
    pub to: Option<TreeRef>,
    /// The files, by path.
    pub files: Vec<FileDiff>,
    /// Why there is nothing to compare, when there is not: no git repository, no snapshot.
    pub absent: Option<String>,
}

/// One file of a review.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileDiff {
    /// The file, from the repository's root.
    pub path: String,
    /// Its blob on the old side; `None` when it was added.
    pub from: Option<String>,
    /// Its blob on the new side; `None` when it was removed.
    pub to: Option<String>,
    /// Its bytes are not text, so it has no hunks.
    pub binary: bool,
    /// Its hunks, the ones [`Pick::hunks`] count.
    pub patch: Patch,
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

/// The most of a text one [`Expanded::Text`] carries, in chars: the rest is cut.
pub const EXPANDED_CHARS: usize = 1_000_000;

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
    /// What it is doing now: the title of its newest tool call that runs or waits on the
    /// person ("Edit src/main.rs").
    pub doing: Option<String>,
    /// Lines added and removed over the turns the worker holds.
    pub changed: Changed,
    /// The terminal its TUI runs in.
    pub terminal: Option<SessionId>,
    /// The directory it works in, as its agent said.
    pub cwd: Option<String>,
    /// The root of the repository that directory is in, once the worker has looked.
    pub repo: Option<String>,
    /// Which repository that is on every machine, once the worker has read it.
    pub repo_id: Option<crate::terminal::RepoId>,
    /// For a subagent: where it hangs.
    pub parent: Option<Link>,
    /// How Slopty reaches it.
    pub drive: Drive,
    /// What it can do through Slopty.
    pub caps: Vec<Cap>,
    /// Its open facts.
    pub facts: BTreeMap<String, String>,
    /// Whether its tree holds changes the person has not kept ([`super::Action::ToReview`]).
    pub to_review: bool,
    /// Its meters: model, mode, context, cost, the plan's rate windows.
    pub meters: super::Meters,
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

/// Past sessions: the answer to [`ThreadRequest::Sessions`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PastSessions {
    /// The agent asked about, as it was asked.
    pub agent: Option<AgentId>,
    /// The folder asked about, as it was asked.
    pub cwd: Option<String>,
    /// The words asked for, as they were asked.
    pub query: String,
    /// The sessions, in the order [`ThreadRequest::Sessions`] says.
    pub sessions: Vec<PastSession>,
    /// Why none could be listed, in words, when none could: the agent is not installed, or keeps
    /// no list Slopty can read.
    pub absent: Option<String>,
    /// Why the answer may lack sessions, in words, when it may: a record too large to read
    /// whole, or a read that ran out of time. Asking again reads on from where it stopped.
    pub cut: Option<String>,
}

/// One of an agent's sessions, as its own record lists it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PastSession {
    /// Its agent.
    pub agent: AgentId,
    /// The agent's own id for it ([`ThreadMeta::native`](super::ThreadMeta::native)).
    pub native: String,
    /// The folder it ran in, when its record says.
    pub cwd: Option<String>,
    /// What it is about, when the agent or the worker's thread of it says.
    pub title: Option<String>,
    /// When it last changed, when the agent says.
    pub updated_ms: Option<WallMs>,
    /// The thread this worker keeps of it, when it keeps one: a client opens that rather than
    /// start another.
    pub thread: Option<ThreadId>,
    /// The arguments of a [`Start`] in the same folder that take it up again, in the agent's
    /// own words as its adapter takes them.
    pub resume: Vec<String>,
    /// Open facts about it, as the agent records them: its branch, its model, its size.
    pub facts: BTreeMap<String, String>,
    /// The person's prompts in it that matched the words asked for, best first; with no words,
    /// its last prompt. Empty when the agent listed it.
    pub prompts: Vec<PromptHit>,
}

/// How much of a prompt a [`PromptHit`] carries, in bytes: the part round its first match.
pub const PROMPT_HIT_BYTES: usize = 600;

/// One of the person's past prompts, as a search found it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PromptHit {
    /// The prompt, cut to [`PROMPT_HIT_BYTES`] round its first match.
    pub text: String,
    /// Where the words asked for are in [`Self::text`], in order, never overlapping.
    pub spans: Vec<Span>,
    /// Text of the prompt before [`Self::text`] was cut off.
    pub cut_before: bool,
    /// Text of the prompt after [`Self::text`] was cut off.
    pub cut_after: bool,
    /// When the person sent it, when the agent recorded that.
    pub at_ms: Option<WallMs>,
}
