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
    /// The mode it starts in, by the agent's own name
    /// ([`Offers::modes`](super::Offers::modes)); the agent's own default when `None`. An
    /// agent that names no modes refuses one.
    pub mode: Option<String>,
    /// How hard it starts thinking, by the agent's own name
    /// ([`Offers::efforts`](super::Offers::efforts)); the agent's own default when `None`.
    pub effort: Option<String>,
    /// Files sent with the first message, as [`Intent::Send`]'s `attachments`.
    pub attachments: Vec<String>,
    /// More arguments for the agent, checked by its adapter.
    pub args: Vec<String>,
    /// A git worktree of its own to work in: the worker makes it from the clone `cwd` is in
    /// (`.claude/worktrees/<name>` on branch `worktree-<name>`), or reopens it when it is
    /// there, and the agent starts in it where `cwd` stands in the clone. A start whose `cwd`
    /// is in no clone is refused.
    pub worktree: Option<NewWorktree>,
}

/// A worktree a thread starts in ([`Start::worktree`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NewWorktree {
    /// Its name: a single plain name, the branch `worktree-<name>`.
    pub name: String,
    /// The branch it starts from; the one the clone has checked out when `None`, else the
    /// clone's `HEAD`. The worker fetches it from `origin` first, for a moment, and starts
    /// from `origin`'s copy when that holds every commit of the clone's own, so the worktree
    /// is current without losing work not yet pushed.
    pub base: Option<String>,
    /// The pull request of `origin` whose head it checks out, in place of `base`, which is then
    /// not read. Its branch tracks the pull request, so gh finds it from the worktree: a pull
    /// request from a branch of `origin` itself tracks that branch, one from a fork its
    /// `pull/<n>/head`.
    pub pull: Option<u32>,
    /// Whether the repository's own setup runs in it before anything starts there, once: the
    /// first setup file of another tool's that the worktree's checkout has (`conductor.json`,
    /// `.cursor/worktrees.json` and the like). `false` starts without it, as the person asked
    /// after a setup failed.
    pub setup: bool,
}

impl NewWorktree {
    /// The worktree `name`, from the branch the clone has checked out, set up.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), base: None, pull: None, setup: true }
    }
}

/// A repository's setup in a new worktree, running or failed ([`Outcome::SetupFailed`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Setup {
    /// The file it was read from, relative to the worktree's root: `conductor.json`.
    pub from: String,
    /// Its last lines of output, stdout and stderr as they came, the newest last; at most
    /// [`Setup::TAIL`].
    pub tail: Vec<String>,
}

impl Setup {
    /// How many of its last lines a setup carries.
    pub const TAIL: usize = 12;
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
    /// Send a queued message now: into the turn under way, as a steer would go, where the agent
    /// takes one ([`Cap::STEER`]), else first after stopping that turn, as a send by interrupt
    /// would ([`Delivery::Interrupt`]); with no turn under way it starts one. Any thread that
    /// holds messages takes it ([`Cap::QUEUE`]).
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
    /// Close a thread for good: an aside put away, or the runs of a message the person did not
    /// keep. Its agent ends and the worker forgets it; the agent's session stays wherever the
    /// agent keeps it. A worktree an agent works in under its clone's `.claude/worktrees/` goes
    /// too, changes not committed and all, unless another thread or a terminal works in it; its
    /// branch goes only once every commit on it has landed, so no commit is lost.
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
    /// The capability a thread needs for this intent; none for closing it ([`Self::Discard`]),
    /// which the worker does whatever its agent can.
    ///
    /// A send by interrupt ([`Delivery::Interrupt`]) needs [`Cap::QUEUE`] too.
    #[must_use]
    pub const fn needs(&self) -> Option<&'static str> {
        Some(match self {
            Self::Discard => return None,
            Self::Send { delivery: Delivery::Steer, .. } => Cap::STEER,
            Self::Send { delivery: Delivery::At { .. }, .. } => Cap::SCHEDULE,
            Self::Send { delivery: Delivery::Queue, .. }
            | Self::Withdraw { .. }
            | Self::Edit { .. }
            | Self::Promote { .. } => Cap::QUEUE,
            Self::Send { delivery: Delivery::Interrupt, .. } | Self::Interrupt => Cap::INTERRUPT,
            Self::Answer { .. } | Self::Release { .. } => Cap::APPROVALS,
            Self::SetModel { .. } => Cap::SET_MODEL,
            Self::SetMode { .. } => Cap::SET_MODE,
            Self::SetEffort { .. } => Cap::SET_EFFORT,
            Self::Compact => Cap::COMPACT,
            Self::Handoff | Self::TakeBack => Cap::HANDOFF,
            Self::Fork { .. } | Self::Aside | Self::KeepAside => Cap::FORK,
            Self::Keep(_) | Self::Revert(_) => Cap::SNAPSHOTS,
            Self::Review { .. } => Cap::REVIEW,
            Self::Continue { .. } => Cap::CONTINUE,
            Self::Rewind { .. } => Cap::REWIND,
        })
    }

    /// The answer to this intent from an agent that cannot do it: the capability it lacks.
    #[must_use]
    pub fn unsupported(&self) -> Outcome {
        self.needs().map_or_else(
            || Outcome::Refused { reason: "not something this agent does".to_owned() },
            |cap| Outcome::Unsupported { cap: Cap::named(cap) },
        )
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
    /// A start's worktree was made, but the repository's setup in it failed: the worktree is
    /// kept, and the start can be tried again, or made without the setup
    /// ([`NewWorktree::setup`]).
    SetupFailed {
        /// What ran, and what it said last.
        setup: Setup,
        /// Its exit status; `None` when a signal ended it.
        code: Option<i32>,
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

/// The span of work a review covers: a diff between two trees of the working tree.
///
/// "Now" is one taken for the review. A thread's spans are between its snapshots
/// ([`Action::Snapshot`]); [`Self::WorkingTree`] is any folder's, a thread's or none.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReviewScope {
    /// One turn: from its start to its end, or to now while it runs.
    Turn(TurnId),
    /// From a turn's start to now.
    Since(TurnId),
    /// From what the person has kept ([`Intent::Keep`]) to now; what is left to review.
    Kept,
    /// The working tree now, new files and all, against a commit of its repository
    /// (`crate::git::GitOp::Changes` asks it of a folder with no thread).
    WorkingTree(Against),
}

/// What a working tree's review compares it with ([`ReviewScope::WorkingTree`]).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Against {
    /// `HEAD`: what is not committed.
    Head,
    /// Where the branch left its base branch: the merge base of `HEAD` and `origin`'s default
    /// branch, else of the local `main` or `master`. All the branch's work, committed or not.
    Base,
    /// Where the branch left the branch named, a project's target: the merge base of `HEAD` and
    /// `origin`'s copy of it when that holds every commit of the local one, else the local one.
    /// What merging the branch into it would bring, committed or not.
    Branch(String),
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
    /// Its branch's pull request, as the worker last read it ([`super::Action::PullSeen`]).
    pub pull: Option<PullSeen>,
    /// Its meters: model, mode, context, cost, the plan's rate windows.
    pub meters: super::Meters,
    /// When it last changed.
    pub updated_ms: WallMs,
}

/// A thread's pull request in a line: what a list shows of it and what the attention ladder
/// reads, the worker's summary of the forge's own answer ([`crate::git::PullStatus`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PullSeen {
    /// Where it lives, which says what it is called and how its number is written.
    pub forge: crate::git::Forge,
    /// Its number.
    pub number: u32,
    /// Its page.
    pub url: String,
    /// Its title.
    pub title: String,
    /// The branch it merges into, as the forge names it: what its whole change is read
    /// against.
    pub base: String,
    /// Where it stands.
    pub stands: PullStands,
    /// How many of its checks failed.
    pub failed: u32,
    /// The first of them, by name.
    pub failed_first: Option<String>,
    /// How many still run.
    pub running: u32,
}

/// Where a pull request stands, as the person reads it: the first that holds, in this order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PullStands {
    /// Merged.
    Merged,
    /// Closed without a merge.
    Closed,
    /// Still a draft.
    Draft,
    /// It conflicts with its base.
    Conflicted,
    /// A check failed.
    ChecksFailed,
    /// Its reviewers asked for changes.
    ChangesRequested,
    /// Checks still run.
    Running,
    /// Waiting on a review, or anything else the forge holds it for.
    Waiting,
    /// Nothing stands between it and a merge.
    Ready,
}

impl PullStands {
    /// Whether it waits on the person: a check failed, changes were asked for, or it
    /// conflicts.
    #[must_use]
    pub const fn needs_you(self) -> bool {
        matches!(self, Self::Conflicted | Self::ChecksFailed | Self::ChangesRequested)
    }
}

impl PullSeen {
    /// Its number as its forge writes it: `#42`, a merge request's `!42`.
    #[must_use]
    pub fn named(&self) -> String {
        format!("{}{}", self.forge.mark(), self.number)
    }

    /// What it says of itself in a line: "#42: lint failed", "!42 is ready to merge".
    #[must_use]
    pub fn line(&self) -> String {
        let n = self.named();
        match self.stands {
            PullStands::Merged => format!("{n} merged"),
            PullStands::Closed => format!("{n} closed"),
            PullStands::Draft => format!("{n} is a draft"),
            PullStands::Conflicted => format!("{n} conflicts with its base"),
            PullStands::ChecksFailed => match (&self.failed_first, self.failed) {
                (Some(first), 1) => format!("{n}: {first} failed"),
                (Some(first), more) => {
                    format!("{n}: {first} and {} more failed", more.saturating_sub(1))
                }
                (None, more) => format!("{n}: {more} checks failed"),
            },
            PullStands::ChangesRequested => format!("{n}: changes requested"),
            PullStands::Running => format!("{n}: checks running"),
            PullStands::Waiting => format!("{n} waits on a review"),
            PullStands::Ready => format!("{n} is ready to merge"),
        }
    }
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

impl RequestCard {
    /// Whether it is a yes or no that "Allow" and "Deny" answer whole, where it is shown: an
    /// approval offering a plain allow and a deny ([`super::once`]). A note's buttons, on the
    /// phone or in the app, answer only such a request.
    #[must_use]
    pub fn answerable(&self) -> bool {
        self.kind == super::Request::APPROVAL
            && super::once(&self.options, true).is_some()
            && super::once(&self.options, false).is_some()
    }
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

/// The [`PastSession::facts`] key of a session a live process of its agent holds.
///
/// Its value is how that process runs on the worker's machine: `interactive`, or how it runs in
/// the background. A start that takes the session up would make a second writer, and is
/// refused.
pub const PAST_RUNNING: &str = "running";

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
