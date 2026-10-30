//! What a client asks of a worker's threads and what it is sent.
//!
//! - **The thread table.** One small replicated table per worker, a [`ThreadRow`] per thread, with
//!   a cursor of its own ([`ThreadRequest::Table`], [`TableFrame`]), so every list of threads is
//!   right the moment a link comes back without following anything.
//! - **Following a thread.** [`ThreadRequest::Follow`] with the cursor the client holds is answered
//!   on a stream of the thread's own by the actions after it, when the log still holds them in the
//!   same epoch, else by a [`ThreadFrame::Snapshot`] of the last turns, then live
//!   [`ThreadFrame::Actions`]. Older turns come by [`ThreadRequest::Page`].
//! - **Intents.** A start, and the person's own marks on a thread ([`Intent`]), go under a
//!   client-made [`IntentId`]. The worker acts on an id once and answers each with an
//!   [`IntentDone`]; a repeat gets the first answer again. Everything else is done in the agent's
//!   own terminal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::{
    Action, AgentId, Changed, ContentRef, Cursor, Drive, IntentId, Item, ItemId, Link, Patch,
    Status, ThreadId, ThreadState, TreeRef, Turn, TurnId,
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
    /// What a page shown in `thread` asks of its MCP App ([`crate::visual::AppAsk`]): the
    /// worker passes it through the thread's agent (Claude Code's `mcp_read_resource` and
    /// `mcp_call` with `result_not_for_model`, Codex's `mcpServer/resource/read` and
    /// `mcpServer/tool/call`) and answers on the thread's stream with [`ThreadFrame::App`]
    /// under the same `id`. It never dials a server itself, and asks only of a followed thread.
    App {
        /// The thread.
        thread: ThreadId,
        /// The client's number for the ask, which the answer carries.
        id: u64,
        /// What the page asks.
        ask: crate::visual::AppAsk,
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
    /// The mode it starts in, by the agent's own name for it (Claude Code's permission mode,
    /// Codex's approval policy); the agent's own default when `None`.
    pub mode: Option<String>,
    /// How hard it starts thinking, by the agent's own name for it; the agent's own default
    /// when `None`.
    pub effort: Option<String>,
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
    /// The branch its pull request merges back into, which gh reads for a `gh pr create` with
    /// no `--base`, when that differs from the branch it starts from. The branch it starts
    /// from when `None`.
    pub merge_base: Option<String>,
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
        Self { name: name.into(), base: None, merge_base: None, pull: None, setup: true }
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

/// The person's own mark on a thread: all a client asks of a thread once it runs.
///
/// Everything else is done in the agent's own terminal, which Slopty reads but never types into
/// (`docs/decisions/agents.md`, "TUI-first"). Any thread takes either.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Intent {
    /// The person saw the thread's turns through `turn`, on whichever device: its row's
    /// [`ThreadRow::seen`] moves up to it and never back, so every device reads one account of
    /// what is unread.
    Seen {
        /// The last turn seen.
        turn: TurnId,
    },
    /// The person's own name for the thread: what every list shows over the agent's own
    /// ([`ThreadRow::title`]), on every device. Empty words take it back. At most
    /// [`Self::NAME_MAX`] characters.
    Name {
        /// The name.
        title: String,
    },
}

impl Intent {
    /// The most characters a thread's name ([`Self::Name`]) holds.
    pub const NAME_MAX: usize = 200;
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
    /// The agent's answer to a page's ask ([`ThreadRequest::App`]), by the ask's `id`.
    App {
        /// The ask's number.
        id: u64,
        /// What came of it.
        answer: crate::visual::AppAnswer,
    },
}

/// The span of work a review covers: a diff between two trees of a repository.
///
/// "Now" is one taken for the review, of any folder's working tree, a thread's or none.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ReviewScope {
    /// The working tree now, new files and all, against a commit of its repository
    /// (`crate::git::GitOp::Changes` asks it of a folder with no thread).
    WorkingTree(Against),
    /// The commits of `head` since it left `base`, as committed: no working tree
    /// (`crate::git::GitOp::Range`).
    Range {
        /// The branch it left.
        base: String,
        /// The branch reviewed.
        head: String,
    },
}

/// What a working tree's review compares it with ([`ReviewScope::WorkingTree`]).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Against {
    /// `HEAD`: what is not committed.
    Head,
    /// Where the branch left its base branch: the merge base of `HEAD` and `origin`'s default
    /// branch, else of the local `main` or `master`. All the branch's work, committed or not.
    Base,
    /// Where the branch left the branch named: the merge base of `HEAD` and
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
    /// The file, from the repository's root; its new path when it was renamed.
    pub path: String,
    /// Where it was on the old side when git found it renamed there (`-M`); `None` otherwise.
    pub old_path: Option<String>,
    /// Its blob on the old side; `None` when it was added.
    pub from: Option<String>,
    /// Its blob on the new side; `None` when it was removed.
    pub to: Option<String>,
    /// What its bytes are, and so whether it has hunks.
    pub kind: FileKind,
    /// Its mode on each side, when the two differ (an executable bit set, a file become a
    /// link); `None` when they are the same or a side is absent.
    pub modes: Option<Modes>,
    /// Its hunks, in order; none unless it is [`FileKind::Text`].
    pub patch: Patch,
}

/// What a reviewed file's bytes are: whether the worker cut it into hunks, and why not.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FileKind {
    /// Text, cut into hunks.
    Text,
    /// Bytes that are not text and not a picture the client draws.
    Binary,
    /// Text larger than the worker cuts into hunks, review or [`crate::git::GitOp::FileDiff`]
    /// alike: shown by its size, the file opened whole to read it.
    TooLarge {
        /// The larger side's size, in bytes.
        bytes: u64,
    },
    /// A picture (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, SVG), each side's bytes asked for by
    /// [`crate::git::GitOp::Blob`].
    Image {
        /// The larger side's size, in bytes.
        bytes: u64,
    },
}

/// A file's mode on each side of a review, as git writes it (`0o100644`, `0o100755`,
/// `0o120000` for a symbolic link).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Modes {
    /// On the old side.
    pub from: u32,
    /// On the new side.
    pub to: u32,
}

impl FileDiff {
    /// Whether it was cut into hunks: its bytes are text the worker reads whole.
    #[must_use]
    pub const fn is_text(&self) -> bool {
        matches!(self.kind, FileKind::Text)
    }

    /// Lines added and removed: how much there is to read in it, hunks shown or not.
    #[must_use]
    pub const fn weight(&self) -> u32 {
        self.patch.added.saturating_add(self.patch.removed)
    }
}

/// Lock files, by name.
const LOCKS: [&str; 10] = [
    "cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lock",
    "bun.lockb",
    "go.sum",
    "gemfile.lock",
    "poetry.lock",
    "uv.lock",
];

/// Whether `path` is the kind of file a reviewer reads last: tests, fixtures, snapshots, locks
/// and generated code. They are listed under the rest, quieter.
#[must_use]
pub fn quiet(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let in_dir =
        |dir: &str| lower.starts_with(&format!("{dir}/")) || lower.contains(&format!("/{dir}/"));
    LOCKS.contains(&name)
        || ["tests", "test", "__tests__", "fixtures", "testdata", "snapshots", "generated", "dist"]
            .iter()
            .any(|dir| in_dir(dir))
        || [".snap", ".min.js", ".pb.go", ".g.dart", "_pb2.py"]
            .iter()
            .any(|end| name.ends_with(end))
        || ["_test.", ".test.", ".spec.", "_spec."].iter().any(|mid| name.contains(mid))
        || name.starts_with("test_")
}

/// The order a review's files are read in, as places in `files`.
///
/// The weightiest come first ([`FileDiff::weight`]), the [`quiet`] ones after the rest, the
/// same weight by path. The worker spends a review's lines in this order, so the files read
/// first are the ones shown.
#[must_use]
pub fn reading_order(files: &[FileDiff]) -> Vec<usize> {
    let mut order: Vec<(bool, u32, usize)> =
        files.iter().enumerate().map(|(at, f)| (quiet(&f.path), f.weight(), at)).collect();
    order.sort_by(|a, b| {
        let path = |at: usize| files.get(at).map(|f| f.path.as_str());
        a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| path(a.2).cmp(&path(b.2)))
    });
    order.into_iter().map(|(_, _, at)| at).collect()
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
    /// What the agent said of their calls beside them
    /// ([`ThreadState::calls`](super::ThreadState::calls)).
    pub calls: BTreeMap<ItemId, super::CallNote>,
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
    /// The agent's own id for its session ([`ThreadMeta::native`](super::ThreadMeta::native)):
    /// what the agent's own terminal resumes it by (`claude --resume`).
    pub native: String,
    /// What it is about.
    pub title: String,
    /// Where it is.
    pub status: Status,
    /// The last line the agent wrote.
    pub last_line: Option<String>,
    /// What it is doing now: the title of its newest tool call that runs or waits on the
    /// person ("Edit src/main.rs").
    pub doing: Option<String>,
    /// Lines added and removed over the turns the worker holds.
    pub changed: Changed,
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
    /// Its open facts.
    pub facts: BTreeMap<String, String>,
    /// Its latest turn that ended, so a device that was not linked when it ended still knows.
    pub ended: Option<TurnEnded>,
    /// When its turn under way began, while one is: its time at work runs from here, whatever
    /// phases it passed through since.
    pub began_ms: Option<WallMs>,
    /// The last turn the person has seen, on any device ([`Intent::Seen`]): a turn in
    /// [`Self::ended`] past it is unread. [`TurnId::BEFORE`] until one is seen.
    pub seen: TurnId,
    /// When the worker last heard its agent alive during a turn, by the worker's clock
    /// ([`super::Action::Lease`]): a client shows a turn as running only while this is fresh,
    /// and none before any turn was leased.
    pub lease_ms: Option<WallMs>,
    /// When it began ([`ThreadMeta::created_ms`](super::ThreadMeta::created_ms)).
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
}

impl ThreadRow {
    /// The terminal the thread's agent runs in now
    /// ([`ThreadMeta::TERMINAL_FACT`](super::ThreadMeta::TERMINAL_FACT)): the one a client opens
    /// for it.
    #[must_use]
    pub fn terminal(&self) -> Option<slopty_core::SessionId> {
        self.facts.get(super::ThreadMeta::TERMINAL_FACT).and_then(|id| id.parse().ok())
    }
}

/// A thread's latest turn that ended ([`ThreadRow::ended`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TurnEnded {
    /// Which.
    pub turn: TurnId,
    /// When it ended.
    pub at_ms: WallMs,
    /// How long it ran, in milliseconds: a short one is not worth a word.
    pub ran_ms: u64,
    /// The agent answered it: the person did not stop it, and it did not fail.
    pub answered: bool,
}

/// Where a pull request stands, as the person reads it: the first that holds, in this order
/// ([`crate::git::PullStatus::standing`]).
///
/// Ended first; then what waits on the person, a
/// conflict before a failed check (a conflicted head's checks are stale) before changes asked
/// for, even on a draft, since an agent's draft that fails still needs fixing; then a draft,
/// checks running, and waiting or ready.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PullStands {
    /// Merged.
    Merged,
    /// Closed without a merge.
    Closed,
    /// It conflicts with its base.
    Conflicted,
    /// A check failed.
    ChecksFailed,
    /// Its reviewers asked for changes.
    ChangesRequested,
    /// Still a draft.
    Draft,
    /// Checks still run.
    Running,
    /// Waiting on a review, or anything else the forge holds it for.
    Waiting,
    /// Nothing stands between it and a merge.
    Ready,
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
