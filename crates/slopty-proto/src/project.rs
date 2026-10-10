//! Project: one goal worked on by many agents across the fleet (`docs/decisions/projects.md`).
//!
//! A [`Project`] lives on the server. Its [`Task`]s are its orchestrator's, one level under it,
//! and form a graph by [`Task::depends_on`]. Each may be pinned to a
//! worker ([`Task::pin`]) and, once something runs for it, names that terminal, or the thread any
//! agent runs as ([`Assignment`]): Claude Code, Codex, pi or an ACP agent ([`TaskLaunch`]).
//! Claude Code's own subagents and task list inside a session show as
//! [`Natives`] of its node. Everything that happens is kept in the project's timeline
//! ([`TimelineEntry`]) and pushed to every client as a [`ProjectUpdate`], so the tree is followed
//! as it grows, never run where nobody can see it.
//!
//! The person sets the few limits there are: how much of a project's work may wait on their
//! review ([`Limits`]) and, for the whole fleet, how many agents run ([`Bounds`]). Agents read
//! both and raise neither.
//!
//! A worker reports what the server cannot see from agent status alone as [`AgentReport`]s:
//! where an agent's work lands, and the subagents and tasks Claude Code keeps inside a session.
//! It reports what it is and has as [`Facts`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};

use crate::agent::AgentBranch;
use crate::orchestration::TermRef;
use crate::terminal::RepoId;
use crate::thread::wire::PullSeen;

/// The variable naming the server, `host[:port]`, in every session a worker runs: `slopty mcp`
/// and the CLI inside it find the server with no flag.
pub const SERVER_ENV: &str = "SLOPTY_SERVER";
/// The variable naming the project, in the session of an agent spawned for one of its tasks.
pub const PROJECT_ENV: &str = "SLOPTY_PROJECT";
/// The variable naming the task, in the session of an agent spawned for it.
pub const TASK_ENV: &str = "SLOPTY_TASK";
/// Set to `1` in a terminal the server holds to asking.
///
/// That is one it opened for an agent whose project allows no looser permissions. A `claude`
/// typed there is held to asking as an agent the server starts is (`slopty hook wire`), so it
/// starts in `default` and cannot reach auto or bypass mode.
pub const ASKING_ENV: &str = "SLOPTY_ASKING";
/// The fact on a task's thread row ([`crate::thread::wire::ThreadRow::facts`]) naming the
/// seat it was started at ([`Assignment::thread`]), so the server knows the row as the task's
/// whatever terminal it has.
pub const SEAT_FACT: &str = "slopty.seat";
/// Claude Code's flags known to give an agent nothing the person would be asked for.
///
/// From the CLI reference, checked against 2.1.285. Any other flag loosens, or may, and is refused
/// in an agent's start: a new flag is judged before it is let through, never after.
/// [`PERMISSION_MODE_FLAG`], `--settings` and `--mcp-config` are judged by their values instead.
pub const SAFE_FLAGS: [&str; 56] = [
    "--advisor",
    "--append-subagent-system-prompt",
    "--append-subagent-system-prompt-file",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--autocompact",
    "--ax-screen-reader",
    "--betas",
    "--chrome",
    "--continue",
    "-c",
    "--debug",
    "--disable-slash-commands",
    "--disallowedTools",
    "--disallowed-tools",
    "--effort",
    "--exclude-dynamic-system-prompt-sections",
    "--fallback-model",
    "--fork-session",
    "--forward-subagent-text",
    "--from-pr",
    "--ide",
    "--include-hook-events",
    "--include-partial-messages",
    "--init",
    "--init-only",
    "--input-format",
    "--json-schema",
    "--maintenance",
    "--max-budget-usd",
    "--max-turns",
    "--model",
    "--name",
    "-n",
    "--no-chrome",
    "--no-session-persistence",
    "--output-format",
    "--print",
    "-p",
    "--prompt-suggestions",
    "--replay-user-messages",
    "--restricted",
    "--resume",
    "-r",
    "--session-id",
    "--strict-mcp-config",
    "--system-prompt-snapshot",
    "--teammate-mode",
    "--tools",
    "--verbose",
    "--version",
    "-v",
    "--worktree",
    "-w",
    "--help",
    "-h",
];
/// The flag that names Claude Code's permission mode.
pub const PERMISSION_MODE_FLAG: &str = "--permission-mode";
/// The permission modes that ask the person no less than `default` does (`manual` is
/// `default`'s other name).
pub const SAFE_MODES: [&str; 4] = ["default", "manual", "plan", "dontAsk"];
/// The most items one [`AgentReport::Loosened`] names.
pub const LOOSENED_MAX: usize = 16;
/// The longest item of an [`AgentReport::Loosened`], in bytes.
pub const LOOSENED_ITEM_MAX: usize = 256;

/// The longest metadata document, in bytes.
pub const METADATA_MAX: usize = 16 * 1024;
/// The longest status text, in bytes.
pub const STATUS_MAX: usize = 512;
/// The longest task kind, in bytes.
pub const KIND_MAX: usize = 64;
/// The longest verifier summary, timeline note or native's line, in bytes.
pub const SUMMARY_MAX: usize = 4096;
/// The longest branch, worktree, commit or path kept on a task or a native, in bytes.
pub const REF_MAX: usize = 1024;
/// The most tasks one task depends on.
pub const DEPENDS_MAX: usize = 64;
/// The most timeline entries one [`ProjectStatus`] carries; ask again from its `next` for
/// more.
pub const TIMELINE_PAGE: usize = 128;
/// The most bytes of timeline entries one [`ProjectStatus`] carries, by
/// [`TimelineEntry::approx_bytes`]: with the most tasks the person may allow, a status stays
/// within one link frame.
pub const TIMELINE_PAGE_BYTES: usize = 1 << 20;
/// The most bytes of timeline entries a project keeps, by [`TimelineEntry::approx_bytes`],
/// within its [`TIMELINE_KEPT`] entries: the oldest go first.
pub const TIMELINE_BYTES_KEPT: usize = 8 << 20;
/// The longest report note, in bytes.
pub const NOTE_MAX: usize = SUMMARY_MAX;
/// The most a card's pull request takes on the wire.
///
/// Its title, its page, its base and its first failed check are each cut by the server to
/// [`TITLE_MAX`] or [`REF_MAX`] as it takes the thread's row in, with room for the rest.
pub const PULL_MAX_BYTES: usize = TITLE_MAX + 3 * REF_MAX + 48;
/// The most artifacts one [`Report`] names.
pub const ARTIFACTS_MAX: usize = 32;

/// A project's name, which is its identity: 1 to [`ProjectId::MAX_LEN`] lowercase ASCII
/// letters, digits and dashes, starting and ending with a letter or a digit.
///
/// It is spelled into branch names (`slopty/<project>/<task>`), environment variables and
/// command lines, so it is kept to what all of them take as is.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

/// A string that is not a [`ProjectId`].
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error(
    "{0:?} is not a project name: 1 to 40 lowercase letters, digits and dashes, starting and \
     ending with a letter or a digit"
)]
pub struct BadProjectId(pub String);

impl ProjectId {
    /// The longest name, in bytes.
    pub const MAX_LEN: usize = 40;

    /// `name`, if it is a project name.
    ///
    /// # Errors
    /// [`BadProjectId`] otherwise.
    pub fn new(name: impl Into<String>) -> Result<Self, BadProjectId> {
        let name = name.into();
        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
        let edge = |b: Option<&u8>| b.is_some_and(|b| *b != b'-');
        let bytes = name.as_bytes();
        let fits = (1..=Self::MAX_LEN).contains(&bytes.len())
            && bytes.iter().copied().all(allowed)
            && edge(bytes.first())
            && edge(bytes.last());
        if fits { Ok(Self(name)) } else { Err(BadProjectId(name)) }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProjectId {
    type Error = BadProjectId;

    fn try_from(name: String) -> Result<Self, BadProjectId> {
        Self::new(name)
    }
}

impl From<ProjectId> for String {
    fn from(id: ProjectId) -> Self {
        id.0
    }
}

impl std::str::FromStr for ProjectId {
    type Err = BadProjectId;

    fn from_str(s: &str) -> Result<Self, BadProjectId> {
        Self::new(s)
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A task's number in its project, from 1 in the order the tasks were made.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub u32);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for TaskId {
    type Err = std::num::ParseIntError;

    /// `3` or `#3`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.trim().trim_start_matches('#').parse().map(Self)
    }
}

/// A value a worker reports of itself, or one part of it: JSON's shapes without null.
///
/// A fact is absent rather than null; a placement expression tests for it with `has()`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Fact {
    /// A flag (`ac_power`, a label `fast-disk = true`).
    Bool(bool),
    /// A count or a size (`cpus`, `memory_mb`).
    Int(i64),
    /// A measure (`load`).
    Float(f64),
    /// A word or a version (`os`, `agents.claude`).
    Text(String),
    /// Several (`gpus`, `rust_targets`).
    List(Vec<Self>),
    /// Named parts (`agents`, `acp`, `toolchains`).
    Map(BTreeMap<String, Self>),
}

/// What a worker is and has, by name: an open map the orchestrator reads to pick a worker.
///
/// The server fills in what it knows itself (`name`, `worker`, `os`, `cpus`, `load`,
/// `online`, `live_agents`, `repos`); the worker reports the rest (`docs/decisions/projects.md`
/// lists the built-in names).
pub type Facts = BTreeMap<String, Fact>;

/// One worker's facts.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WorkerFacts {
    /// Which worker.
    pub worker: WorkerId,
    /// Its facts, the server's and its own together.
    pub facts: Facts,
}

/// What the person allows a project, beside the fleet's [`Bounds`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Limits {
    /// Most of its tasks that may wait on the person ([`TaskCard::waits_on_person`]) before its
    /// orchestrator starts no more: what the person can review sets the pace. Only the person
    /// sets it.
    pub review: u16,
}

impl Default for Limits {
    fn default() -> Self {
        Self { review: 3 }
    }
}

/// A change to a project's [`Limits`]. What is absent stays.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LimitsChange {
    /// A new [`Limits::review`]; only the person sets it.
    pub review: Option<u16>,
}

/// Most projects the server keeps.
pub const PROJECTS_MAX: usize = 64;
/// Most tasks one project holds: with [`TaskCard::MAX_BYTES`], a project's cards fit one link
/// frame.
pub const TASKS_MAX: usize = 512;
/// Longest project or task title, in bytes.
pub const TITLE_MAX: usize = 256;
/// Longest task brief, in bytes.
pub const BRIEF_MAX: usize = 64 * 1024;
/// How many timeline entries a project keeps.
pub const TIMELINE_KEPT: usize = 4096;

/// What the person allows across the fleet, from the server's settings (`[server.projects]`).
///
/// Agents read them in [`ProjectStatus::bounds`] to plan, and no verb raises them: they bound
/// what agents can start, so they are the person's, not the agents'.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bounds {
    /// Most live agents across the fleet, in a project or not.
    pub live_agents: u16,
}

impl Default for Bounds {
    fn default() -> Self {
        Self { live_agents: 24 }
    }
}

impl Bounds {
    /// The most live agents any setting allows.
    pub const LIVE_AGENTS_MAX: u16 = 1024;

    /// Whether [`Self::live_agents`] is within [`Self::LIVE_AGENTS_MAX`].
    ///
    /// # Errors
    /// The bound over its ceiling, by its setting name, with both numbers.
    pub fn check(self) -> Result<(), String> {
        let most = Self::LIVE_AGENTS_MAX;
        if self.live_agents > most {
            return Err(format!(
                "live_agents = {} is over its ceiling of {most}",
                self.live_agents
            ));
        }
        Ok(())
    }
}

/// How many agents run now, against the [`Bounds`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Live {
    /// Across the fleet: every live agent the server knows, and those being started.
    pub fleet: u16,
    /// This project's: its tasks' terminals and its orchestrator.
    pub project: u16,
}

/// How far a project's agents go before they ask the person, set by the person per project.
///
/// Each agent carries it by its own permission modes, so the agent answers its own prompts at
/// the level chosen and Slopty answers nothing (`docs/decisions/projects.md`, "Autonomy per
/// project").
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Autonomy {
    /// Every edit and command the agent's own rules do not allow asks the person.
    #[default]
    Ask,
    /// Edits in the task's own worktree go without asking; anything else asks.
    Edits,
    /// The agent goes on its own within its sandbox, its own auto mode deciding.
    Own,
}

impl Autonomy {
    /// The Claude Code permission mode an agent at this level starts in: `default`,
    /// `acceptEdits` or `auto`.
    #[must_use]
    pub const fn claude_mode(self) -> &'static str {
        match self {
            Self::Ask => "default",
            Self::Edits => "acceptEdits",
            Self::Own => "auto",
        }
    }

    /// Whether Claude Code's permission `mode` goes no further than this level: one that asks
    /// ([`SAFE_MODES`]) at every level, `acceptEdits` from [`Self::Edits`], `auto` at
    /// [`Self::Own`], and bypass mode at none.
    #[must_use]
    pub fn allows_mode(self, mode: &str) -> bool {
        SAFE_MODES.contains(&mode)
            || (mode == "acceptEdits" && !matches!(self, Self::Ask))
            || (mode == "auto" && matches!(self, Self::Own))
    }

    /// Whether Claude Code may open auto mode at this level, so its settings leave it open.
    #[must_use]
    pub const fn opens_auto(self) -> bool {
        matches!(self, Self::Own)
    }

    /// Codex's approval policy at this level: `on-request` asks before anything outside its
    /// sandbox, and `never` goes on its own inside it.
    #[must_use]
    pub const fn codex_approval(self) -> &'static str {
        match self {
            Self::Ask | Self::Edits => "on-request",
            Self::Own => "never",
        }
    }
}

/// Where a project's goal stands, as its orchestrator last said
/// ([`crate::orchestration::Verb::ProjectProgress`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Progress {
    /// What is done and what runs, in a line or two.
    pub summary: String,
    /// What comes next, when the orchestrator knows.
    pub next: Option<String>,
    /// The goal is met: the person is told once.
    pub done: bool,
    /// When it said so, by the server's clock.
    pub at_ms: WallMs,
}

impl Progress {
    /// The longest summary or next step, in bytes.
    pub const TEXT_MAX: usize = 2048;
}

/// A project: its goal's home on the server, and the orchestrator that splits it into tasks.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Project {
    /// Its name.
    pub id: ProjectId,
    /// What it is for, in a line.
    pub title: String,
    /// The goal the person handed over, the orchestrator's first prompt; none for a project
    /// made around an orchestrator already at work.
    pub goal: Option<String>,
    /// How far its agents go before they ask the person.
    pub autonomy: Autonomy,
    /// Where the orchestrator last said the goal stands ([`Moment::Update`]).
    pub progress: Option<Progress>,
    /// The repository its tasks work in, as the orchestrator names it (a path or a URL).
    pub repo: String,
    /// Which repository that is on every machine ([`RepoId`]), learned from where its
    /// orchestrator works: a task started with no directory goes beside a clone of it.
    pub repo_id: Option<RepoId>,
    /// The branch finished work lands on.
    pub target: String,
    /// The command that says a task's work is right (`cargo gate`), when there is one.
    pub verifier: Option<String>,
    /// Whether the merge queue pushes the target branch to its clone's `origin` after each
    /// merge. Off unless the person turns it on: publishing is theirs to choose.
    pub push: bool,
    /// The terminal of the agent the person talks to, which splits the goal into tasks.
    pub orchestrator: Option<TermRef>,
    /// How long the orchestrator worked, idle waits left out: its share, apart from its
    /// tasks'.
    pub orchestrator_spent: Spent,
    /// Its limits.
    pub limits: Limits,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
}

/// Where a task stands.
///
/// An agent's own status moves a task among [`Self::Running`], [`Self::Waiting`] and
/// [`Self::Blocked`] while it works on it; every other move is the orchestrator's or the
/// person's ([`crate::orchestration::Verb::TaskUpdate`]), along [`Self::may_become`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum TaskState {
    /// Made, and nothing on it yet.
    Planned,
    /// Its agent is working.
    Running,
    /// Its agent is at its prompt, or waits on its own background work.
    Waiting,
    /// Its agent needs a person: a permission, a question.
    Blocked,
    /// Its verifier runs.
    Verifying,
    /// Its verifier passed; it waits to be merged.
    Done,
    /// Its work is on the target branch. Final.
    Merged,
    /// Given up; it may be planned again.
    Failed,
}

impl TaskState {
    /// Whether the task holds the paths it owns: until its work is merged or given up.
    #[must_use]
    pub const fn holds_paths(self) -> bool {
        !matches!(self, Self::Merged | Self::Failed)
    }

    /// Whether its agent's status decides it.
    #[must_use]
    pub const fn follows_the_agent(self) -> bool {
        matches!(self, Self::Running | Self::Waiting | Self::Blocked)
    }

    /// Whether a task may move from this state to `to`: anywhere but out of
    /// [`Self::Merged`], and into it only from [`Self::Done`] or [`Self::Verifying`].
    #[must_use]
    pub const fn may_become(self, to: Self) -> bool {
        match (self, to) {
            (Self::Merged, _) => false,
            (_, Self::Merged) => matches!(self, Self::Done | Self::Verifying),
            _ => true,
        }
    }
}

/// The terminal, or the thread, working on a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Assignment {
    /// Its terminal, or for a thread its seat: the id its Slopty tools speak as, and the
    /// terminal its agent runs in when it runs in one.
    pub term: TermRef,
    /// The thread its agent runs as, for a task whose agent runs as one. Whether
    /// it runs, and where its agent is, then come from the worker's thread table, the row
    /// marked with [`SEAT_FACT`].
    pub thread: Option<crate::thread::ThreadId>,
    /// Since when, by the server's clock.
    pub since_ms: WallMs,
    /// When its terminal closed; open while it runs.
    pub ended_ms: Option<WallMs>,
    /// The Claude Code conversation the server started it under (`--session-id`), known
    /// before its first hook; none for a command, or a terminal it was told of.
    pub conversation: Option<String>,
    /// The server started it for the task, rather than being told of a terminal that ran.
    pub spawned: bool,
}

impl Assignment {
    /// Whether its terminal still runs, as far as the server was told.
    #[must_use]
    pub const fn open(&self) -> bool {
        self.ended_ms.is_none()
    }
}

/// What a task's agent says of its finished work, for the project's orchestrator.
///
/// It is delivered once the task has settled, and a later report of the task replaces it. A
/// need or a block reaches the orchestrator as the turn end it already hears. Delivery is
/// through the receiving agent's own hooks, never typed into its terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    /// What it says, in a few lines.
    pub note: String,
    /// What it made: paths, commits, links.
    pub artifacts: Vec<String>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// The pull request it opened.
    pub pr: Option<u32>,
}

/// What a verifier said of a task's work, at the commits it ran on: a result counts for that
/// head only, so a later commit is verified again.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VerifierRun {
    /// Whether it passed.
    pub passed: bool,
    /// What it said: the last lines it printed when it failed, its last line when it passed.
    /// At most [`SUMMARY_MAX`] bytes.
    pub summary: String,
    /// The commit it verified, in hex.
    pub head: String,
    /// The commit the task's work was on top of then, in hex: where it left the target
    /// branch, or the target itself for a head the merge queue rebased.
    pub base: String,
    /// Its exit status, or the signal that ended it negated; none when it never ran to an
    /// end (it could not start, or its terminal was closed first).
    pub exit: Option<i32>,
    /// How long it ran, in milliseconds.
    pub took_ms: u64,
}

/// Where a task stands in its project's merge queue (`docs/decisions/projects.md`).
///
/// The queue takes its tasks one at a time, the longest queued first. It rebases each onto
/// the target branch in the orchestrator's clone, runs the verifier again on what the rebase
/// made unless that is the very commit already verified, and fast-forwards the target to it.
/// A task that conflicts or fails leaves the queue, and its agent is told why.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Merge {
    /// Waiting its turn: its verifier passed, or the person asked for the merge.
    Queued {
        /// Since when, by the server's clock: the queue's order.
        since_ms: WallMs,
    },
    /// Its work is on the target branch.
    Merged {
        /// The branch it landed on.
        target: String,
        /// The commit the target was moved to, in hex.
        head: String,
        /// The task's own commit that was rebased onto the target, in hex: what its worktree's
        /// branch holds on the machine it ran on, where the rebased `head` may never be.
        from: String,
        /// When, by the server's clock.
        at_ms: WallMs,
        /// Whether the target was pushed to its clone's `origin` too.
        pushed: bool,
        /// Why a push asked for did not happen, in git's words: the target moved all the same,
        /// and the person pushes again.
        push_failed: Option<String>,
    },
    /// Its target is protected on the forge, so its work, rebased and verified, waits in a pull
    /// request there; the task is merged once the pull request is.
    Pull {
        /// The branch it lands on.
        target: String,
        /// The commit that went up.
        head: String,
        /// The task's own commit that was rebased into `head`, in hex ([`Self::Merged::from`]).
        from: String,
        /// The pull request's number.
        number: u32,
        /// Its page.
        url: String,
        /// When it was opened or found, by the server's clock.
        since_ms: WallMs,
    },
}

impl Merge {
    /// When it joined the queue, while it waits there.
    #[must_use]
    pub const fn queued(&self) -> Option<WallMs> {
        match self {
            Self::Queued { since_ms } => Some(*since_ms),
            Self::Merged { .. } | Self::Pull { .. } => None,
        }
    }
}

/// Where a worker keeps the checkout a project's work is verified and rebased in.
///
/// It is `<VERIFY_PLACES>/<project>`, one per project, kept between runs so what a verifier
/// builds stays warm.
pub const VERIFY_PLACES: &str = "~/slopty/verify";

/// One of Claude Code's own subagents inside a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NativeAgent {
    /// Claude Code's id for it (`agent_id`), which names its thread in the conversation face.
    pub id: String,
    /// Its type: `general-purpose`, `Explore`, a custom agent's name.
    pub kind: String,
    /// When it started, by the server's clock.
    pub started_ms: WallMs,
    /// When it stopped; running while absent.
    pub stopped_ms: Option<WallMs>,
    /// Its own transcript on the worker, once it stopped.
    pub transcript: Option<String>,
    /// The first line of what it answered.
    pub last: Option<String>,
}

/// One item of Claude Code's own task list inside a session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NativeTask {
    /// Claude Code's id for it.
    pub id: String,
    /// Its title.
    pub subject: String,
    /// Whether it is completed.
    pub done: bool,
}

/// What Claude Code runs inside one session on its own: the tree's leaves below a node.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Natives {
    /// Its subagents, in the order they started.
    pub agents: Vec<NativeAgent>,
    /// Its task list, in the order the items were made.
    pub tasks: Vec<NativeTask>,
}

impl Natives {
    /// How many it holds, and how many of those are running or done.
    #[must_use]
    pub fn counts(&self) -> NativeCounts {
        let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        NativeCounts {
            agents: count(self.agents.len()),
            running: count(self.agents.iter().filter(|a| a.stopped_ms.is_none()).count()),
            todos: count(self.tasks.len()),
            done: count(self.tasks.iter().filter(|t| t.done).count()),
        }
    }
}

/// One node's [`Natives`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NodeNatives {
    /// The task whose terminal runs them; the orchestrator's when absent.
    pub task: Option<TaskId>,
    /// Them.
    pub natives: Natives,
}

/// One native leaf as it is now.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Native {
    /// A subagent.
    Agent(NativeAgent),
    /// A task-list item.
    Todo(NativeTask),
}

/// A native leaf changed under a node.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NativeChange {
    /// The node: a task, or the orchestrator when absent.
    pub task: Option<TaskId>,
    /// The leaf as it is now, which replaces the one of its id.
    pub native: Native,
}

/// What a new task is: everything about it before anything runs for it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskSpec {
    /// Tasks whose work it needs first. They and their own never lead back to it.
    pub depends_on: Vec<TaskId>,
    /// One of `depends_on` whose work it starts from once that is done and its verifier
    /// passed, before it merges: that work's branch, as the orchestrator's clone holds it, is
    /// the worktree's base, and it merges only after that task. The rest of `depends_on` are
    /// merged first.
    pub start_from: Option<TaskId>,
    /// What sort of work it is, in the orchestrator's own words (`build`, `review`, `bench`).
    pub kind: String,
    /// What it is, in a line.
    pub title: String,
    /// What its agent is told to do.
    pub brief: String,
    /// It only reads.
    pub read_only: bool,
    /// The worker it runs on and no other; the server places it when absent.
    pub pin: Option<WorkerId>,
    /// Its own verifier, over the project's.
    pub verifier: Option<String>,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
}

/// A task: one part of a project's goal its orchestrator started, and a node of its graph.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Task {
    /// Its number in the project.
    pub id: TaskId,
    /// Tasks whose work it needs first.
    pub depends_on: Vec<TaskId>,
    /// The one of `depends_on` whose done work it starts from before that merges
    /// ([`TaskSpec::start_from`]).
    pub start_from: Option<TaskId>,
    /// The commit of `start_from`'s verified work its worktree started on, in hex, once it
    /// started there: the merge queue picks only its commits after this one.
    pub started_on: Option<String>,
    /// What sort of work it is.
    pub kind: String,
    /// What it is, in a line.
    pub title: String,
    /// What its agent is told to do.
    pub brief: String,
    /// It only reads.
    pub read_only: bool,
    /// The worker it runs on and no other; the server places it when absent.
    pub pin: Option<WorkerId>,
    /// Its own verifier, over the project's.
    pub verifier: Option<String>,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
    /// Where it stands.
    pub state: TaskState,
    /// What its agent says it is doing, in its own words.
    pub status: Option<String>,
    /// The terminal on it, once one is.
    pub assignment: Option<Assignment>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// The worktree its agent works in, on its worker.
    pub worktree: Option<String>,
    /// The commit its work started from, in hex, so a diff, a verification again or a rebase
    /// outlives a restart.
    pub base: Option<String>,
    /// Its branch's pull request, as its thread's row last said it
    /// ([`crate::thread::wire::ThreadRow::pull`]): one watcher, the worker's, reads the forge.
    pub pull: Option<PullSeen>,
    /// What its verifier last said.
    pub verified: Option<VerifierRun>,
    /// Its place in the merge queue, or the merge that put its work on the target.
    pub merge: Option<Merge>,
    /// What the server last did for it around its agent: a clone made, its branch brought
    /// home, verified or merged.
    pub step: Option<TaskStep>,
    /// How long its agents worked on it, idle waits left out.
    pub spent: Spent,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
    /// The server's automatic give-backs of its work since the person last spoke on it.
    pub give_backs: GiveBacks,
    /// What its work did to the project's tests, as of its last done report.
    pub tests: Option<TestDiff>,
}

/// How many automatic give-backs a task takes before a failure goes to the person instead of
/// its agent: verifier failures and rebase conflicts together.
pub const GIVE_BACKS_MAX: u8 = 3;

/// The server's automatic give-backs of a task's work to its agent, since the person last
/// spoke on it (`docs/decisions/projects.md`, "At most three automatic give-backs").
///
/// Past [`GIVE_BACKS_MAX`] of them the next failure is held for the person: its agent is not told,
/// and the task waits on the person until they say what next. The person's next word on the task
/// starts the count again.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct GiveBacks {
    /// How many went back to its agent.
    pub count: u8,
    /// A failure past the cap waits for the person.
    pub held: bool,
}

impl GiveBacks {
    /// Whether one more give-back still goes to the agent.
    #[must_use]
    pub const fn room(self) -> bool {
        self.count < GIVE_BACKS_MAX
    }
}

/// The most test files a [`TestDiff`] names of each kind.
pub const TESTS_NAMED: usize = 8;
/// The key in a project's metadata naming paths that hold tests beside the usual ones
/// ([`is_test_path`]): an array of repository-relative paths.
pub const TEST_PATHS_KEY: &str = "test_paths";

/// What a task's work did to the project's tests: the test files it deleted, changed and
/// added, from `git diff --name-status` between where it left the target and its head.
///
/// A work that deletes or rewrites tests to pass is what a reviewer looks at first, so the
/// card, every give-back and the reviewer's brief say it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TestDiff {
    /// The commit it was read at, in hex.
    pub head: String,
    /// Test files it deleted, at most [`TESTS_NAMED`].
    pub deleted: Vec<String>,
    /// Test files it changed or renamed, at most [`TESTS_NAMED`].
    pub changed: Vec<String>,
    /// How many test files it deleted in all.
    pub deleted_count: u16,
    /// How many test files it changed in all.
    pub changed_count: u16,
    /// How many test files it added.
    pub added_count: u16,
}

impl TestDiff {
    /// The most it takes on the wire.
    pub const MAX_BYTES: usize = 2 * TESTS_NAMED * (REF_MAX + 2) + 64 + 24;

    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        self.deleted
            .iter()
            .chain(&self.changed)
            .map(|p| p.len().saturating_add(10))
            .fold(self.head.len().saturating_add(40), usize::saturating_add)
    }

    /// Whether it touched no test.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.deleted_count == 0 && self.changed_count == 0 && self.added_count == 0
    }

    /// What it did, in a line: `Tests: 1 deleted (tests/a.rs), 2 changed (…), 3 added`, or
    /// that it touched none.
    #[must_use]
    pub fn line(&self) -> String {
        if self.is_empty() {
            return "Tests: none deleted, changed or added".to_owned();
        }
        let named = |count: u16, paths: &[String], what: &str| {
            (count > 0).then(|| {
                let more = usize::from(count).saturating_sub(paths.len());
                let more = if more > 0 { format!(" and {more} more") } else { String::new() };
                format!("{count} {what} ({}{more})", paths.join(", "))
            })
        };
        let parts: Vec<String> = [
            named(self.deleted_count, &self.deleted, "deleted"),
            named(self.changed_count, &self.changed, "changed"),
            (self.added_count > 0).then(|| format!("{} added", self.added_count)),
        ]
        .into_iter()
        .flatten()
        .collect();
        format!("Tests: {}", parts.join(", "))
    }
}

/// Whether `path`, relative to the repository's root, holds tests.
///
/// It does when it is within one of `extra` (a project's [`TEST_PATHS_KEY`]), it has a
/// directory named `test`, `tests`, `spec` or `__tests__`, or its file is `test.*`, `tests.*`
/// or `test_*.*`, or ends in `_test.*`, `.test.*`, `_spec.*` or `.spec.*`.
#[must_use]
pub fn is_test_path(path: &str, extra: &[String]) -> bool {
    let path = path.trim_matches('/');
    let within = |outer: &str| {
        let outer = outer.trim().trim_matches('/');
        !outer.is_empty()
            && (path == outer || path.strip_prefix(outer).is_some_and(|r| r.starts_with('/')))
    };
    if extra.iter().any(|e| within(e)) {
        return true;
    }
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().unwrap_or_default();
    if parts.iter().any(|d| matches!(*d, "test" | "tests" | "spec" | "__tests__")) {
        return true;
    }
    let Some((stem, _ext)) = name.rsplit_once('.') else { return false };
    matches!(stem, "test" | "tests")
        || stem.starts_with("test_")
        || ["_test", "_spec", ".test", ".spec"].iter().any(|end| stem.ends_with(end))
}

/// How long an agent worked: the stretches it was at work, the waits between left out.
///
/// The server follows the agent's thread: a stretch begins when it starts working and ends
/// when it stops, so a wait at the prompt or on the person is not counted.
/// The stretch under way is counted by whoever reads it ([`Spent::at`]), so a running clock
/// needs no message per second.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Spent {
    /// The stretches that ended, in milliseconds.
    pub active_ms: u64,
    /// When the stretch under way began, while the agent works.
    pub since_ms: Option<WallMs>,
}

/// What following an agent's status did to its [`Spent`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stretch {
    /// It started working.
    Began,
    /// It stopped, and the stretch is counted.
    Ended,
}

impl Spent {
    /// Its agent is at work now or not: a stretch begins, or the one under way ends.
    pub const fn follow(&mut self, works: bool, now: WallMs) -> Option<Stretch> {
        match (self.since_ms, works) {
            (None, true) => {
                self.since_ms = Some(now);
                Some(Stretch::Began)
            }
            (Some(since), false) => {
                self.active_ms = self.at_from(since, now);
                self.since_ms = None;
                Some(Stretch::Ended)
            }
            _ => None,
        }
    }

    /// How long it worked as of `now`, the stretch under way included.
    #[must_use]
    pub fn at(&self, now: WallMs) -> u64 {
        self.since_ms.map_or(self.active_ms, |since| self.at_from(since, now))
    }

    const fn at_from(&self, since: WallMs, now: WallMs) -> u64 {
        self.active_ms.saturating_add(now.as_millis().saturating_sub(since.as_millis()))
    }
}

/// What the server does for a task around its agent, so no wait is silent.
///
/// A clone made before it can start, its branch brought to the orchestrator's machine once it
/// is done, its verifier run, and its merge.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskStep {
    /// Which.
    pub kind: StepKind,
    /// The worker it happens on: the one cloning, the one the branch comes to, or the one
    /// verifying and merging in the orchestrator's clone.
    pub worker: WorkerId,
    /// How it goes.
    pub state: StepState,
    /// When it began, by the server's clock.
    pub since_ms: WallMs,
    /// The terminal it runs in, for a person to open: a verifier's, kept after a failure so
    /// its whole output can still be read.
    pub term: Option<TermRef>,
    /// The commits it works on, once its worker said: what a verifier or a reviewer still
    /// running in its terminal across a restart of the server is taken up on.
    pub commits: Option<Commits>,
}

/// The commit a verifier or a reviewer works on, and the one that work is on top of.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Commits {
    /// The commit, in hex.
    pub head: String,
    /// Where the work left the target, in hex.
    pub base: String,
}

/// Which [`TaskStep`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StepKind {
    /// The repository cloned onto the worker the task is placed on, which had none.
    Clone,
    /// The task's branch fetched into the orchestrator's clone, from the worker it ran on.
    Home,
    /// The task's verifier run on its branch, in a checkout of the orchestrator's clone.
    Verify,
    /// The merge queue rebasing the task's work onto the target, verifying it again and
    /// fast-forwarding the target to it.
    Merge,
    /// The merge queue's rebase of the task's work onto the target. It is a step of its own
    /// only when it fails, and then it conflicts: the work goes back to its agent to resolve.
    Rebase,
    /// What a task's worktree starts from (the target, or the work it starts on), or the
    /// target it rebases onto, sent from the orchestrator's clone to the task's on another
    /// machine.
    Send,
}

/// How a [`TaskStep`] goes. Its texts are at most [`SUMMARY_MAX`] bytes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StepState {
    /// Under way.
    Running {
        /// What it is doing, in a few words.
        phase: String,
        /// How far, when known.
        percent: Option<u8>,
    },
    /// Finished.
    Done {
        /// What it made: the clone's path, the branch and its commit.
        detail: String,
    },
    /// It failed, and left nothing behind.
    Failed {
        /// Why.
        why: String,
    },
}

impl TaskStep {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub const fn approx_bytes(&self) -> usize {
        let text = match &self.state {
            StepState::Running { phase, .. } => phase,
            StepState::Done { detail } => detail,
            StepState::Failed { why } => why,
        };
        let commits = if self.commits.is_some() { 84 } else { 0 };
        text.len().saturating_add(104).saturating_add(commits)
    }

    /// Whether it is under way.
    #[must_use]
    pub const fn running(&self) -> bool {
        matches!(self.state, StepState::Running { .. })
    }
}

impl VerifierRun {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub const fn approx_bytes(&self) -> usize {
        self.summary
            .len()
            .saturating_add(self.head.len())
            .saturating_add(self.base.len())
            .saturating_add(48)
    }
}

impl Task {
    /// Whether it waits on the person, as its card says ([`TaskCard::waits_on_person`]).
    #[must_use]
    pub const fn waits_on_person(&self) -> bool {
        self.ready_to_merge() || matches!(self.state, TaskState::Blocked) || self.give_backs.held
    }

    /// Whether its work was checked and waits for the person's Merge.
    #[must_use]
    pub const fn ready_to_merge(&self) -> bool {
        matches!(self.state, TaskState::Done) && self.merge.is_none()
    }

    /// The branch its work lands as in the orchestrator's clone when it was done on another
    /// machine: `slopty/<project>/<task>`, a name only the server sets.
    #[must_use]
    pub fn home_branch(project: &ProjectId, task: TaskId) -> String {
        format!("slopty/{project}/{task}")
    }

    /// The branch a project made with no target lands its work on: `slopty/<project>/goal`,
    /// beside its tasks' home branches, so no ref is both a branch and a folder of them.
    #[must_use]
    pub fn goal_branch(project: &ProjectId) -> String {
        format!("slopty/{project}/goal")
    }

    /// The branch the project's target lands as in a task's clone on another machine, when
    /// the merge queue gives the task back to rebase onto it: `slopty/<project>/target`, a
    /// name only the server sets and no task's number can take.
    #[must_use]
    pub fn target_branch(project: &ProjectId) -> String {
        format!("slopty/{project}/target")
    }

    /// Its line in the tree, with its node's natives counted.
    #[must_use]
    pub fn card(&self, natives: &Natives) -> TaskCard {
        TaskCard {
            id: self.id,
            depends_on: self.depends_on.clone(),
            kind: self.kind.clone(),
            title: self.title.clone(),
            read_only: self.read_only,
            state: self.state,
            status: self.status.clone(),
            assignment: self.assignment.clone(),
            branch: self.branch.clone(),
            worktree: self.worktree.clone(),
            pull: self.pull.clone(),
            verified: self.verified.clone(),
            merge: self.merge.clone(),
            step: self.step.clone(),
            pin: self.pin,
            spent: self.spent,
            natives: natives.counts(),
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
            give_backs: self.give_backs,
            tests: self.tests.clone(),
        }
    }
}

/// A task as the tree shows it: everything but its brief, verifier and metadata, which
/// [`crate::orchestration::Verb::TaskGet`] fetches.
///
/// Every field is bounded ([`TITLE_MAX`], [`STATUS_MAX`], [`KIND_MAX`],
/// [`DEPENDS_MAX`], [`SUMMARY_MAX`], [`REF_MAX`]), so a card is at most
/// [`TaskCard::MAX_BYTES`] on the wire and a project's cards fit one link frame.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskCard {
    /// Its number.
    pub id: TaskId,
    /// The tasks it needs first.
    pub depends_on: Vec<TaskId>,
    /// What sort of work it is.
    pub kind: String,
    /// What it is, in a line.
    pub title: String,
    /// Whether it only reads.
    pub read_only: bool,
    /// Where it stands.
    pub state: TaskState,
    /// What its agent says it is doing.
    pub status: Option<String>,
    /// The terminal on it.
    pub assignment: Option<Assignment>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// The worktree its agent works in.
    pub worktree: Option<String>,
    /// Its branch's pull request, as its thread's row last said it.
    pub pull: Option<PullSeen>,
    /// What its verifier last said.
    pub verified: Option<VerifierRun>,
    /// Its place in the merge queue, or its merge.
    pub merge: Option<Merge>,
    /// What the server last did for it around its agent.
    pub step: Option<TaskStep>,
    /// The worker it is pinned to, by its orchestrator or the person's "Run on".
    pub pin: Option<WorkerId>,
    /// How long its agents worked on it, idle waits left out.
    pub spent: Spent,
    /// How many natives its node holds.
    pub natives: NativeCounts,
    /// When it was made.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
    /// The server's automatic give-backs since the person last spoke on it.
    pub give_backs: GiveBacks,
    /// What its work did to the project's tests.
    pub tests: Option<TestDiff>,
}

impl TaskCard {
    /// The most a card takes on the wire, from the bounds on its fields, with room for the
    /// encoding's lengths and tags.
    pub const MAX_BYTES: usize = TITLE_MAX
        + STATUS_MAX
        + KIND_MAX
        + DEPENDS_MAX * 5
        + 2 * SUMMARY_MAX
        + 8 * REF_MAX
        + PULL_MAX_BYTES
        + TestDiff::MAX_BYTES
        + 800;

    /// Whether it waits on the person: its work is ready to merge ([`Self::ready_to_merge`]),
    /// its agent asks them something, or a failure past its give-backs is held for them. What
    /// [`Limits::review`] counts.
    #[must_use]
    pub const fn waits_on_person(&self) -> bool {
        self.ready_to_merge() || matches!(self.state, TaskState::Blocked) || self.give_backs.held
    }

    /// Whether its work was checked and waits for the person's Merge: done, and neither in the
    /// queue nor merged.
    #[must_use]
    pub const fn ready_to_merge(&self) -> bool {
        matches!(self.state, TaskState::Done) && self.merge.is_none()
    }
}

impl TaskCard {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let text = |t: Option<&str>| t.map_or(0, |t| t.len().saturating_add(10));
        [
            text(Some(&self.kind)),
            text(Some(&self.title)),
            text(self.status.as_deref()),
            text(self.branch.as_deref()),
            text(self.worktree.as_deref()),
            self.pull.as_ref().map_or(0, pull_bytes),
            self.verified.as_ref().map_or(0, VerifierRun::approx_bytes),
            self.merge.as_ref().map_or(0, |m| match m {
                Merge::Queued { .. } => 16,
                Merge::Merged { target, head, from, push_failed, .. } => target
                    .len()
                    .saturating_add(head.len())
                    .saturating_add(from.len())
                    .saturating_add(push_failed.as_deref().map_or(0, str::len))
                    .saturating_add(32),
                Merge::Pull { target, head, from, url, .. } => target
                    .len()
                    .saturating_add(head.len())
                    .saturating_add(from.len())
                    .saturating_add(url.len())
                    .saturating_add(32),
            }),
            self.assignment
                .as_ref()
                .map_or(0, |a| a.conversation.as_deref().map_or(0, str::len).saturating_add(64)),
            self.depends_on.len().saturating_mul(5),
            self.tests.as_ref().map_or(0, TestDiff::approx_bytes),
            self.step.as_ref().map_or(0, TaskStep::approx_bytes),
        ]
        .into_iter()
        .fold(128, usize::saturating_add)
    }
}

/// About how many bytes a pull request on a card or the timeline takes on the wire, never less.
fn pull_bytes(pull: &PullSeen) -> usize {
    [&pull.url, &pull.title, &pull.base]
        .into_iter()
        .chain(&pull.failed_first)
        .map(|t| t.len().saturating_add(10))
        .fold(32, usize::saturating_add)
}

/// How many natives a node holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct NativeCounts {
    /// Subagents it has started.
    pub agents: u16,
    /// Of those, the ones still running.
    pub running: u16,
    /// Items on its task list.
    pub todos: u16,
    /// Of those, the ones completed.
    pub done: u16,
}

/// One node of a project's tree in full.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NodeDetail {
    /// The task, or none for the orchestrator's node.
    pub task: Option<Task>,
    /// The natives Claude Code keeps in it.
    pub natives: Natives,
}

/// Something that happened in a project, as its timeline keeps it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// Its place in the project's timeline, from 1.
    pub seq: u64,
    /// When, by the server's clock.
    pub at_ms: WallMs,
    /// The task it concerns; the project itself when absent.
    pub task: Option<TaskId>,
    /// What.
    pub what: Moment,
}

/// What a [`TimelineEntry`] records.
///
/// Claude Code's own subagents and to-dos are not in it: they come and go by the dozen, and
/// the tree shows them ([`NativeChange`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Moment {
    /// The project was made.
    Created,
    /// The orchestrator's terminal was named.
    Orchestrator {
        /// Its terminal.
        term: TermRef,
    },
    /// The project's limits changed.
    Limits {
        /// As they are now.
        limits: Limits,
    },
    /// A task was made.
    TaskCreated {
        /// Its title.
        title: String,
    },
    /// A terminal took the task on.
    Assigned {
        /// Its terminal.
        term: TermRef,
        /// The server started it (`task_spawn`), rather than being told of one that ran.
        spawned: bool,
    },
    /// The task moved.
    State {
        /// From.
        from: TaskState,
        /// To.
        to: TaskState,
    },
    /// Its work's branch changed.
    Branch {
        /// The branch.
        branch: Option<String>,
    },
    /// Its verifier ran, or the person recorded what it said.
    Verified(VerifierRun),
    /// Its pull request was first seen, or came to stand otherwise: its checks started,
    /// passed or failed, changes were asked for, it merged.
    Pull(PullSeen),
    /// The terminal on it closed.
    AgentGone {
        /// The terminal.
        term: TermRef,
    },
    /// The person told the task's agent, or the orchestrator when the entry names no task,
    /// something ([`crate::orchestration::Verb::TaskTell`]).
    Told {
        /// What they said.
        text: String,
    },
    /// Words from the orchestrator or the person.
    Note {
        /// The words.
        text: String,
    },
    /// The task's agent reported.
    Reported {
        /// What it said.
        report: Report,
    },
    /// Reports were delivered to the agent they are for.
    Delivered {
        /// Its terminal.
        term: TermRef,
        /// How many.
        reports: u16,
    },
    /// A step for the task began, finished or failed; its progress between is on its card
    /// alone.
    Step(TaskStep),
    /// The orchestrator said where the goal stands.
    Update(Progress),
}

impl TimelineEntry {
    /// About how many bytes it takes on the wire, never less: what its texts hold, and room
    /// for the rest.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let text = |t: &str| t.len().saturating_add(10);
        let texts = |ts: &[String]| ts.iter().map(|t| text(t)).fold(0_usize, usize::saturating_add);
        let what = match &self.what {
            Moment::TaskCreated { title } => text(title),
            Moment::Branch { branch } => branch.as_deref().map_or(0, text),
            Moment::Verified(run) => run.approx_bytes(),
            Moment::Pull(pull) => pull_bytes(pull),
            Moment::Note { text: words } | Moment::Told { text: words } => text(words),
            Moment::Reported { report } => text(&report.note)
                .saturating_add(texts(&report.artifacts))
                .saturating_add(report.branch.as_deref().map_or(0, text)),
            Moment::Step(step) => step.approx_bytes(),
            Moment::Update(progress) => {
                text(&progress.summary).saturating_add(progress.next.as_deref().map_or(0, text))
            }
            Moment::Created
            | Moment::Orchestrator { .. }
            | Moment::Limits { .. }
            | Moment::Assigned { .. }
            | Moment::State { .. }
            | Moment::AgentGone { .. }
            | Moment::Delivered { .. } => 0,
        };
        what.saturating_add(96)
    }
}

/// A project changed: what changed and what happened.
///
/// For a client to mirror and the timeline to show. Pushed as
/// [`crate::orchestration::Happening::Project`] in a [`crate::orchestration::HubEvent`], whose
/// `seq` orders it after the [`crate::server::FromServer::Projects`] snapshot of a lower `seq`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProjectUpdate {
    /// Which project.
    pub project: ProjectId,
    /// The project as it is now, when this changed it.
    pub record: Option<Project>,
    /// The task's card as it is now, when this changed one.
    pub task: Option<TaskCard>,
    /// A native leaf as it is now, when this changed one.
    pub native: Option<NativeChange>,
    /// What happened, when it is worth the timeline.
    pub entry: Option<TimelineEntry>,
}

impl ProjectUpdate {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let record = self.record.as_ref().map_or(0, Project::approx_bytes);
        let task = if self.task.is_some() { TaskCard::MAX_BYTES } else { 0 };
        let native = if self.native.is_some() { 3 * REF_MAX + SUMMARY_MAX + KIND_MAX } else { 0 };
        let entry = self.entry.as_ref().map_or(0, TimelineEntry::approx_bytes);
        record.saturating_add(task).saturating_add(native).saturating_add(entry).saturating_add(64)
    }
}

impl Project {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        [
            self.title.len(),
            self.repo.len(),
            self.target.len(),
            self.verifier.as_deref().map_or(0, str::len),
            self.metadata.as_deref().map_or(0, str::len),
            self.goal.as_deref().map_or(0, str::len),
            self.progress.as_ref().map_or(0, |p| {
                p.summary.len().saturating_add(p.next.as_deref().map_or(0, str::len))
            }),
            self.repo_id.as_ref().map_or(0, |id| id.keys().map(str::len).sum()),
        ]
        .into_iter()
        .fold(128_usize, |sum, len| sum.saturating_add(len).saturating_add(10))
    }
}

/// A project's tree: the record, every task's card, its timeline from a cursor, and the
/// numbers an agent plans by. A node in full is [`crate::orchestration::Verb::TaskGet`]'s.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProjectStatus {
    /// The project.
    pub project: Project,
    /// Its tasks by number.
    pub tasks: Vec<TaskCard>,
    /// How many natives the orchestrator's node holds.
    pub orchestrator_natives: NativeCounts,
    /// Its timeline from the cursor asked for, oldest first, at most [`TIMELINE_PAGE`].
    pub timeline: Vec<TimelineEntry>,
    /// The cursor to ask from next: one past the last entry returned.
    pub next: u64,
    /// What the person allows.
    pub bounds: Bounds,
    /// What runs now.
    pub live: Live,
}

/// A task's change, from its orchestrator, its own agent or a person. What is absent stays;
/// an empty text clears.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct TaskChange {
    /// A new state, along [`TaskState::may_become`].
    pub state: Option<TaskState>,
    /// What its agent says it is doing.
    pub status: Option<String>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// What its verifier said. Only the person and the merge queue record it.
    pub verified: Option<VerifierRun>,
    /// The commit its work starts from.
    pub base: Option<String>,
    /// Words for the timeline.
    pub note: Option<String>,
    /// New dependencies, in place of the old.
    pub depends_on: Option<Vec<TaskId>>,
    /// Where it runs: what the person's "Run on" sets.
    pub run_on: Option<RunOn>,
    /// A new verifier of its own.
    pub verifier: Option<String>,
    /// New metadata, in place of the old.
    pub metadata: Option<String>,
}

/// Where a task runs, as [`TaskChange::run_on`] says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunOn {
    /// On this worker and no other: its pin.
    Worker(WorkerId),
    /// Wherever the server places it: no pin.
    Anywhere,
}

/// How to start a task's agent: in a git worktree of its own beside a clone of the project's
/// repository, its brief as its first prompt, with Slopty's tools and its role, at the project's
/// [`Autonomy`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskLaunch {
    /// This worker, over the task's pin; the server places it when absent.
    pub pin: Option<WorkerId>,
    /// The agent: Claude Code, Codex, pi or an ACP agent; it goes only to a worker that has it.
    pub agent: crate::thread::AgentId,
}

/// One frame of the projects a client link is sent on connect and after a lag
/// ([`crate::server::FromServer::Projects`]).
///
/// A project too large for one frame is split: a later part carries its record again with more
/// of its tasks.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProjectsPart {
    /// The last hub event the snapshot includes: a client drops any project event at or
    /// below it.
    pub seq: u64,
    /// Whether this is the snapshot's first part: what the client had is replaced.
    pub first: bool,
    /// Whether it is the last.
    pub last: bool,
    /// Projects, or parts of one: tasks of a project already begun are added to it.
    pub projects: Vec<ProjectStatus>,
}

/// What a worker tells the server about an agent beyond its status.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AgentReport {
    /// Where an agent's work lands: its worktree and pull request.
    Branch(AgentBranch),
    /// One of Claude Code's own subagents started in a session.
    SubagentStarted {
        /// The session.
        session: SessionId,
        /// Its id.
        agent: String,
        /// Its type.
        kind: String,
    },
    /// That subagent stopped.
    SubagentStopped {
        /// The session.
        session: SessionId,
        /// Its id.
        agent: String,
        /// Its own transcript on the worker.
        transcript: Option<String>,
        /// The first line of what it answered.
        last: Option<String>,
    },
    /// The permission mode a session's hook said it is in (`default`, `plan`, `acceptEdits`,
    /// `auto`, `dontAsk`, `bypassPermissions`), when it changed.
    PermissionMode {
        /// The session.
        session: SessionId,
        /// The mode, as Claude Code names it.
        mode: String,
    },
    /// An item of Claude Code's own task list was made or completed in a session.
    NativeTask {
        /// The session.
        session: SessionId,
        /// The item.
        task: NativeTask,
    },
    /// A batch of reports ([`crate::server::FromServer::Deliver`]) reached the agent in a
    /// session through its hooks.
    Delivered {
        /// The session.
        session: SessionId,
        /// The batch.
        batch: u64,
    },
    /// What in the command line of the agent running in a session loosens its permissions
    /// (flags, or a `--settings` that allows tools or adds a deciding hook), read off the
    /// process however it was started: bare, through a runtime, or inside a shell's line. Sent
    /// when it changes; empty once nothing does. At most [`LOOSENED_MAX`] items, each at most
    /// [`LOOSENED_ITEM_MAX`] bytes.
    Loosened {
        /// The session.
        session: SessionId,
        /// Each thing that loosens, as a reader would name it.
        found: Vec<String>,
    },
}

impl AgentReport {
    /// The session it is about.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Branch(branch) => branch.session,
            Self::SubagentStarted { session, .. }
            | Self::SubagentStopped { session, .. }
            | Self::PermissionMode { session, .. }
            | Self::Loosened { session, .. }
            | Self::NativeTask { session, .. }
            | Self::Delivered { session, .. } => *session,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each level starts Claude Code in its own mode and allows no mode past it: modes that
    /// ask at every level, `acceptEdits` from edits, `auto` only on its own, bypass never.
    /// Codex asks on request until the project goes on its own.
    #[test]
    fn each_autonomy_allows_its_own_modes_and_none_past_them() {
        let levels = [Autonomy::Ask, Autonomy::Edits, Autonomy::Own];
        for level in levels {
            assert!(level.allows_mode(level.claude_mode()), "{level:?} starts where it may be");
            assert!(SAFE_MODES.iter().all(|m| level.allows_mode(m)), "{level:?}");
            assert!(!level.allows_mode("bypassPermissions"), "{level:?}");
            assert_eq!(level.opens_auto(), level.allows_mode("auto"), "{level:?}");
        }
        let edits: Vec<bool> = levels.map(|l| l.allows_mode("acceptEdits")).to_vec();
        assert_eq!(edits, [false, true, true]);
        let auto: Vec<bool> = levels.map(|l| l.allows_mode("auto")).to_vec();
        assert_eq!(auto, [false, false, true]);
        let codex = levels.map(Autonomy::codex_approval);
        assert_eq!(codex, ["on-request", "on-request", "never"]);
    }

    /// A test file is one under a test directory, one named as a test is named, or one in a
    /// path the project names; anything else is not, however close its name.
    #[test]
    fn a_test_file_is_known_by_its_directory_its_name_or_the_projects_word() {
        let none: &[String] = &[];
        for test in [
            "tests/golden.rs",
            "crates/x/tests/a.rs",
            "web/__tests__/App.tsx",
            "spec/models/user_spec.rb",
            "src/net_test.go",
            "src/App.test.tsx",
            "src/app.spec.ts",
            "pkg/test_parser.py",
            "crates/slopty-server/src/hub/queue/tests.rs",
            "src/a/test.rs",
        ] {
            assert!(is_test_path(test, none), "{test}");
        }
        for not in ["src/testing.rs", "src/attest.rs", "contest/main.c", "README", "src/test"] {
            assert!(!is_test_path(not, none), "{not}");
        }
        let golden = vec!["crates/slopty-e2e/golden".to_owned()];
        assert!(is_test_path("crates/slopty-e2e/golden/board.png", &golden));
        assert!(!is_test_path("crates/slopty-e2e/goldenrod.png", &golden));
    }

    /// What the work did to tests reads in a line: deletions first, named, then changes, then
    /// additions counted; work that touched none says so.
    #[test]
    fn a_test_diff_says_what_was_deleted_first() {
        let diff = TestDiff {
            head: "a".repeat(40),
            deleted: vec!["tests/a.rs".to_owned()],
            changed: vec!["tests/b.rs".to_owned(), "tests/c.rs".to_owned()],
            deleted_count: 1,
            changed_count: 3,
            added_count: 2,
        };
        assert_eq!(
            diff.line(),
            "Tests: 1 deleted (tests/a.rs), 3 changed (tests/b.rs, tests/c.rs and 1 more), 2 added"
        );
        assert_eq!(TestDiff::default().line(), "Tests: none deleted, changed or added");
        assert!(diff.approx_bytes() <= TestDiff::MAX_BYTES);
    }

    /// Give-backs go to the agent three times; past that the next waits on the person.
    #[test]
    fn give_backs_stop_at_three() {
        let at = |count| GiveBacks { count, held: false };
        assert!(at(0).room() && at(2).room());
        assert!(!at(3).room(), "the fourth goes to the person");
    }

    #[test]
    fn a_project_name_is_what_a_branch_and_a_variable_take() {
        for good in ["slopty", "a", "net-2", "0x1", &"a".repeat(40)] {
            assert_eq!(ProjectId::new(good).map(String::from).as_deref(), Ok(good), "{good}");
        }
        for bad in ["", "-a", "a-", "Slopty", "a b", "a/b", "a_b", "é", &"a".repeat(41)] {
            ProjectId::new(bad).unwrap_err();
        }
        let parsed: Result<ProjectId, _> = serde_json::from_str("\"Bad\"");
        assert!(parsed.is_err(), "a bad name does not decode");
    }

    #[test]
    fn a_task_number_reads_with_or_without_its_hash() {
        assert_eq!("3".parse(), Ok(TaskId(3)));
        assert_eq!(" #12".parse(), Ok(TaskId(12)));
        "x".parse::<TaskId>().unwrap_err();
    }

    #[test]
    fn a_merged_task_is_final_and_only_finished_work_merges() {
        use TaskState::*;
        let all = [Planned, Running, Waiting, Blocked, Verifying, Done, Merged, Failed];
        assert!(all.iter().all(|s| !Merged.may_become(*s)));
        let into_merged: Vec<_> = all.into_iter().filter(|s| s.may_become(Merged)).collect();
        assert_eq!(into_merged, [Verifying, Done]);
        assert!(Failed.may_become(Planned) && Done.may_become(Running));
    }

    #[test]
    fn a_task_holds_its_paths_until_it_is_merged_or_given_up() {
        let holding = [
            TaskState::Planned,
            TaskState::Running,
            TaskState::Waiting,
            TaskState::Blocked,
            TaskState::Verifying,
            TaskState::Done,
            TaskState::Merged,
            TaskState::Failed,
        ]
        .into_iter()
        .filter(|s| s.holds_paths())
        .count();
        assert_eq!(holding, 6);
        assert!(!TaskState::Merged.holds_paths() && !TaskState::Failed.holds_paths());
    }

    /// Time spent counts the stretches an agent was at work and leaves out the rest. A stretch
    /// under way counts up to the moment it is read, and a word that does not change the
    /// stretch changes nothing.
    #[test]
    fn spent_counts_the_stretches_at_work() {
        let at = |s: u64| WallMs::from_millis(1_000_000 + s * 1_000);
        let mut spent = Spent::default();
        assert_eq!(spent.follow(true, at(0)), Some(Stretch::Began));
        assert_eq!(spent.follow(true, at(5)), None, "still the same stretch");
        assert_eq!(spent.at(at(30)), 30_000, "the stretch under way counts as it is read");
        assert_eq!(spent.follow(false, at(40)), Some(Stretch::Ended));
        assert_eq!(spent.follow(false, at(100)), None);
        assert_eq!(spent.at(at(500)), 40_000, "a wait is not counted");
        spent.follow(true, at(600));
        spent.follow(false, at(620));
        assert_eq!(spent, Spent { active_ms: 60_000, since_ms: None });
    }
}
