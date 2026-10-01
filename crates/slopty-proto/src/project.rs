//! Projects: one goal worked on by many agents across the fleet (`docs/decisions/projects.md`).
//!
//! A [`Project`] lives on the server. Its [`Task`]s form a tree by [`Task::parent`] (who split
//! it off, to any depth the project allows) and a graph by [`Task::depends_on`]. Each owns the
//! paths it may write ([`Task::owns`]) unless it only reads, says where it may run as
//! expressions over the workers' [`Facts`] ([`Placement`]), and, once something runs for it,
//! names that terminal ([`Assignment`]): Claude Code, another agent's CLI or a plain command
//! ([`Runner`]). Claude Code's own subagents and task list inside a session show as [`Natives`]
//! of its node. Everything that happens is kept in the project's timeline ([`TimelineEntry`])
//! and pushed to every client as a [`ProjectUpdate`], so the tree is followed as it grows, never
//! run where nobody can see it.
//!
//! Every limit is a number the project sets ([`Limits`]) under the bounds the person sets for
//! the whole fleet ([`Bounds`]); agents read both and cannot raise the second.
//!
//! A worker reports what the server cannot see from agent status alone as [`AgentReport`]s:
//! where an agent's work lands, and the subagents and tasks Claude Code keeps inside a session.
//! It reports what it is and has as [`Facts`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};

use crate::agent::{AgentBranch, PullRequest};
use crate::orchestration::{Size, TermRef};

/// The variable naming the server, `host[:port]`, in every session a worker runs: `slopty mcp`
/// and the CLI inside it find the server with no flag.
pub const SERVER_ENV: &str = "SLOPTY_SERVER";
/// The variable naming the project, in the session of an agent spawned for one of its tasks.
pub const PROJECT_ENV: &str = "SLOPTY_PROJECT";
/// The variable naming the task, in the session of an agent spawned for it.
pub const TASK_ENV: &str = "SLOPTY_TASK";
/// Claude Code's flags known to give an agent nothing the person would be asked for.
///
/// From the CLI reference, checked against 2.1.285. Any other flag loosens, or may, and is refused
/// unless the person allows it for a project (`[server.projects] permission_flags`): a new flag is
/// judged before it is let through, never after. [`PERMISSION_MODE_FLAG`], `--settings` and
/// `--mcp-config` are judged by their values instead.
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

/// The longest placement expression, in bytes: a rule, not a program.
pub const EXPR_MAX: usize = 1024;
/// The most rules one [`Placement`] holds, of each kind.
pub const RULES_MAX: usize = 32;
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
/// whatever its [`Limits::timeline_kept`]: the oldest go first.
pub const TIMELINE_BYTES_KEPT: usize = 8 << 20;
/// The longest report note, in bytes.
pub const NOTE_MAX: usize = SUMMARY_MAX;
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
    /// Named parts (`agents`, `toolchains`, `labels`, `probes`).
    Map(BTreeMap<String, Self>),
}

/// What a worker is and has, by name: an open map any placement expression reads.
///
/// The server fills in what it knows itself (`name`, `worker`, `os`, `cpus`, `load`,
/// `online`, `live_agents`, `repos`); the worker reports the rest (`docs/decisions/projects.md`
/// lists the built-in names), including the person's own `labels` and `probes`.
pub type Facts = BTreeMap<String, Fact>;

/// One worker's facts.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct WorkerFacts {
    /// Which worker.
    pub worker: WorkerId,
    /// Its facts, the server's and its own together.
    pub facts: Facts,
}

/// How many agents may run, set per project by whoever runs it, within the [`Bounds`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Limits {
    /// Most live agents of this project on any one worker.
    pub live_per_worker: u16,
    /// Most live agents of this project in all.
    pub live_per_project: u16,
    /// How deep its tree of tasks may go: 1 is tasks with no parent only.
    pub depth: u16,
    /// How many timeline entries it keeps.
    pub timeline_kept: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { live_per_worker: 4, live_per_project: 12, depth: 8, timeline_kept: 4096 }
    }
}

/// A change to a project's [`Limits`]. What is absent stays.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LimitsChange {
    /// A new [`Limits::live_per_worker`].
    pub live_per_worker: Option<u16>,
    /// A new [`Limits::live_per_project`].
    pub live_per_project: Option<u16>,
    /// A new [`Limits::depth`].
    pub depth: Option<u16>,
    /// A new [`Limits::timeline_kept`].
    pub timeline_kept: Option<u32>,
}

/// What the person allows across the fleet, from the server's settings (`[server.projects]`).
///
/// Agents read them in [`ProjectStatus::bounds`] to plan, and no verb raises them: they bound
/// what agents can start, so they are the person's, not the agents'.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bounds {
    /// Most live agents across the fleet, in a project or not.
    pub live_agents: u16,
    /// Highest [`Limits::live_per_worker`] a project may set.
    pub live_per_worker: u16,
    /// Highest [`Limits::live_per_project`] a project may set.
    pub live_per_project: u16,
    /// Deepest [`Limits::depth`] a project may set.
    pub depth: u16,
    /// Most [`Limits::timeline_kept`] a project may set.
    pub timeline_kept: u32,
    /// Whether an agent started for this project may be given flags that loosen Claude
    /// Code's permissions (`--dangerously-skip-permissions`, `--permission-mode`), or run in
    /// bypass mode.
    pub permission_flags: bool,
    /// Most projects the server keeps.
    pub projects: u16,
    /// Most tasks one project holds.
    pub tasks_per_project: u32,
    /// Longest project or task title, in bytes.
    pub title_max: u32,
    /// Longest task brief, in bytes.
    pub brief_max: u32,
    /// Most paths one task owns.
    pub owns_max: u16,
    /// Deepest nesting of comprehensions (`all`, `exists`, `exists_one`, `map`, `filter`) in
    /// one placement rule: each level multiplies what a rule may cost.
    pub comprehension_depth: u8,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            live_agents: 24,
            live_per_worker: 8,
            live_per_project: 24,
            depth: 16,
            timeline_kept: 65_536,
            permission_flags: false,
            projects: 64,
            tasks_per_project: 512,
            title_max: 256,
            brief_max: 64 * 1024,
            owns_max: 64,
            comprehension_depth: 1,
        }
    }
}

impl Bounds {
    /// What no setting may pass: a project's tree fits one link frame whatever the person
    /// allows ([`TaskCard::MAX_BYTES`] times this `tasks_per_project`), and a rule's cost stays
    /// bounded.
    pub const CEILING: Self = Self {
        live_agents: 1024,
        live_per_worker: 256,
        live_per_project: 1024,
        depth: 64,
        timeline_kept: 1 << 20,
        permission_flags: true,
        projects: 256,
        tasks_per_project: 1024,
        title_max: 1024,
        brief_max: 1 << 20,
        owns_max: 256,
        comprehension_depth: 2,
    };

    /// Whether every bound is within [`Self::CEILING`].
    ///
    /// # Errors
    /// The first bound over its ceiling, by its setting name, with both numbers.
    pub fn check(&self) -> Result<(), String> {
        let c = Self::CEILING;
        let pairs = [
            ("live_agents", u64::from(self.live_agents), u64::from(c.live_agents)),
            ("live_per_worker", u64::from(self.live_per_worker), u64::from(c.live_per_worker)),
            ("live_per_project", u64::from(self.live_per_project), u64::from(c.live_per_project)),
            ("depth", u64::from(self.depth), u64::from(c.depth)),
            ("timeline_kept", u64::from(self.timeline_kept), u64::from(c.timeline_kept)),
            ("projects", u64::from(self.projects), u64::from(c.projects)),
            (
                "tasks_per_project",
                u64::from(self.tasks_per_project),
                u64::from(c.tasks_per_project),
            ),
            ("title_max", u64::from(self.title_max), u64::from(c.title_max)),
            ("brief_max", u64::from(self.brief_max), u64::from(c.brief_max)),
            ("owns_max", u64::from(self.owns_max), u64::from(c.owns_max)),
            (
                "comprehension_depth",
                u64::from(self.comprehension_depth),
                u64::from(c.comprehension_depth),
            ),
        ];
        match pairs.into_iter().find(|(_, set, most)| set > most) {
            Some((name, set, most)) => Err(format!("{name} = {set} is over its ceiling of {most}")),
            None => Ok(()),
        }
    }
}

/// How many agents run now, against the [`Limits`] and [`Bounds`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Live {
    /// Across the fleet: every live agent the server knows, and those being started.
    pub fleet: u16,
    /// This project's: its tasks' terminals and its orchestrator.
    pub project: u16,
}

/// A project: its goal's home on the server.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Project {
    /// Its name.
    pub id: ProjectId,
    /// What it is for, in a line.
    pub title: String,
    /// The repository its tasks work in, as the orchestrator names it (a path or a URL).
    pub repo: String,
    /// The branch finished work lands on.
    pub target: String,
    /// The command that says a task's work is right (`cargo gate`), when there is one.
    pub verifier: Option<String>,
    /// The terminal of the agent the person talks to, which splits the goal into tasks.
    pub orchestrator: Option<TermRef>,
    /// Its limits.
    pub limits: Limits,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
}

/// Another task or a worker, for a task to run beside or away from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Peer {
    /// Where this task of the project runs.
    Task(TaskId),
    /// This worker.
    Worker(WorkerId),
}

/// A preference among the workers that may run a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Preference {
    /// A CEL expression over a worker's facts: true, or a number, scores it.
    pub expr: String,
    /// Points a worker gets when it holds (times the number, for a number); negative to steer
    /// away.
    pub weight: i32,
}

/// Where a task may run and where it had better.
///
/// Rules over the workers' [`Facts`] in CEL, the Common Expression Language
/// (`os == "linux" && cpus >= 16`, `has(probes.cuda)`,
/// `"wasm32-unknown-unknown" in rust_targets`).
///
/// A pinned worker is always the one, whatever the rules say; the orchestrator reading
/// [`WorkerFacts`] and pinning is as good a way to place as any.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Placement {
    /// This worker and no other.
    pub pin: Option<WorkerId>,
    /// Each must hold on a worker for it to run the task.
    pub require: Vec<String>,
    /// Each that holds adds its weight to a worker's score.
    pub prefer: Vec<Preference>,
    /// Run beside these: a worker that runs one scores [`Placement::PEER_WEIGHT`].
    pub near: Vec<Peer>,
    /// Keep away from these: a worker that runs one loses [`Placement::PEER_WEIGHT`].
    pub avoid: Vec<Peer>,
}

impl Placement {
    /// Points a worker gains for each peer it runs of [`Self::near`], or loses for each of
    /// [`Self::avoid`].
    pub const PEER_WEIGHT: i32 = 100;
}

/// Why a worker may or may not run a task, and how it scored.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Reason {
    /// The rule: an expression, or `pin`, `online`, `live_per_worker`, `near`, `avoid`.
    pub rule: String,
    /// Whether it held.
    pub held: bool,
    /// Points it added to the score.
    pub points: i64,
    /// Why, when it did not hold or did not evaluate: the error, the cap reached.
    pub detail: String,
}

/// One worker, ranked for a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    /// The worker.
    pub worker: WorkerId,
    /// Its name.
    pub name: String,
    /// Whether it may run the task: every requirement holds and it has room.
    pub fits: bool,
    /// Its score from the preferences, the higher the better.
    pub score: i64,
    /// Each rule, in the order it was checked.
    pub reasons: Vec<Reason>,
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

/// What runs in a task's terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Runner {
    /// Claude Code, with Slopty's tools and hooks, and a first prompt typed once it is ready.
    Claude {
        /// The first prompt.
        prompt: Option<String>,
        /// Arguments after `claude`.
        args: Vec<String>,
    },
    /// A program and its arguments: another agent's CLI, a build, a benchmark, a script. The
    /// login shell when empty.
    Command {
        /// The program and its arguments.
        argv: Vec<String>,
    },
}

/// The terminal working on a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Assignment {
    /// Its terminal.
    pub term: TermRef,
    /// Since when, by the server's clock.
    pub since_ms: WallMs,
    /// When its terminal closed; open while it runs.
    pub ended_ms: Option<WallMs>,
    /// The Claude Code conversation the server started it under (`--session-id`), known
    /// before its first hook; none for a command, or a terminal it was told of.
    pub conversation: Option<String>,
}

impl Assignment {
    /// Whether its terminal still runs, as far as the server was told.
    #[must_use]
    pub const fn open(&self) -> bool {
        self.ended_ms.is_none()
    }
}

/// What a task's agent says of its work, for whoever split it off: its parent task's agent,
/// or the project's orchestrator.
///
/// When it is delivered follows its kind: a need or a block at once, a finish once it has
/// settled (a later report of the task replaces it), a checkpoint with the next delivery.
/// Delivery is through the receiving agent's own hooks, never typed into its terminal.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    /// What sort of report.
    pub kind: ReportKind,
    /// What it says, in a few lines.
    pub note: String,
    /// What it made: paths, commits, links.
    pub artifacts: Vec<String>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// The pull request it opened.
    pub pr: Option<u32>,
}

/// The kinds of [`Report`], which decide when it is delivered.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ReportKind {
    /// Progress worth knowing, not worth an interruption: delivered with the next one.
    Checkpoint,
    /// It needs an answer to go on: delivered at once.
    NeedsInput,
    /// It cannot go on: delivered at once, interrupting at most every few minutes per task.
    Stuck,
    /// It finished: delivered once it has settled.
    Done,
}

/// What a verifier said of a task's work, at the commits it ran on: a result counts for that
/// head only, so a later commit is verified again.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VerifierRun {
    /// Whether it passed.
    pub passed: bool,
    /// What it said, in a few lines: the failing check, or the summary.
    pub summary: String,
    /// The commit it verified, in hex.
    pub head: String,
    /// The commit the task's work was on top of then, in hex.
    pub base: String,
}

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
    /// The task it was split from; one of the orchestrator's own when absent.
    pub parent: Option<TaskId>,
    /// Tasks whose work it needs first. They and their own never lead back to it.
    pub depends_on: Vec<TaskId>,
    /// What sort of work it is, in the orchestrator's own words (`build`, `review`, `bench`).
    pub kind: String,
    /// What it is, in a line.
    pub title: String,
    /// What its agent is told to do.
    pub brief: String,
    /// The repository paths it alone may write, relative to the repository's root; a
    /// directory owns everything under it.
    pub owns: Vec<String>,
    /// It only reads, so it owns no paths and never waits on anyone's.
    pub read_only: bool,
    /// Where it may run.
    pub placement: Placement,
    /// Its own verifier, over the project's.
    pub verifier: Option<String>,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
}

/// A task: a node of a project's tree and graph.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Task {
    /// Its number in the project.
    pub id: TaskId,
    /// The task it was split from; one of the orchestrator's own when absent.
    pub parent: Option<TaskId>,
    /// Tasks whose work it needs first.
    pub depends_on: Vec<TaskId>,
    /// What sort of work it is.
    pub kind: String,
    /// What it is, in a line.
    pub title: String,
    /// What its agent is told to do.
    pub brief: String,
    /// The repository paths it alone may write.
    pub owns: Vec<String>,
    /// It only reads.
    pub read_only: bool,
    /// Where it may run.
    pub placement: Placement,
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
    /// The commit its work started from, in hex; kept in its worker's mirror as
    /// [`Task::base_ref`], so a diff, a verification again or a rebase outlives a restart.
    pub base: Option<String>,
    /// The pull request open for its branch.
    pub pr: Option<PullRequest>,
    /// What its verifier last said.
    pub verified: Option<VerifierRun>,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
}

impl Task {
    /// The git ref its base commit is kept under in a mirror of `project`'s repository.
    #[must_use]
    pub fn base_ref(&self, project: &ProjectId) -> String {
        format!("refs/slopty/{project}/{}/base", self.id)
    }

    /// Its line in the tree, with its node's natives counted.
    #[must_use]
    pub fn card(&self, natives: &Natives) -> TaskCard {
        TaskCard {
            id: self.id,
            parent: self.parent,
            depends_on: self.depends_on.clone(),
            kind: self.kind.clone(),
            title: self.title.clone(),
            read_only: self.read_only,
            state: self.state,
            status: self.status.clone(),
            assignment: self.assignment.clone(),
            branch: self.branch.clone(),
            worktree: self.worktree.clone(),
            pr: self.pr.clone(),
            verified: self.verified.clone(),
            natives: natives.counts(),
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
        }
    }
}

/// A task as the tree shows it: everything but its brief, paths, placement, verifier and
/// metadata, which [`crate::orchestration::Verb::TaskGet`] fetches.
///
/// Every field is bounded ([`Bounds::CEILING`]'s `title_max`, [`STATUS_MAX`], [`KIND_MAX`],
/// [`DEPENDS_MAX`], [`SUMMARY_MAX`], [`REF_MAX`]), so a card is at most
/// [`TaskCard::MAX_BYTES`] on the wire and a project's cards fit one link frame.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskCard {
    /// Its number.
    pub id: TaskId,
    /// The task it was split from.
    pub parent: Option<TaskId>,
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
    /// Its pull request.
    pub pr: Option<PullRequest>,
    /// What its verifier last said.
    pub verified: Option<VerifierRun>,
    /// How many natives its node holds.
    pub natives: NativeCounts,
    /// When it was made.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
}

impl TaskCard {
    /// The most a card takes on the wire, from the bounds on its fields, with room for the
    /// encoding's lengths and tags.
    pub const MAX_BYTES: usize = Bounds::CEILING.title_max as usize
        + STATUS_MAX
        + KIND_MAX
        + DEPENDS_MAX * 5
        + SUMMARY_MAX
        + 6 * REF_MAX
        + 512;
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
            self.pr.as_ref().map_or(0, |pr| pr.url.len().saturating_add(32)),
            self.verified.as_ref().map_or(0, |v| {
                v.summary
                    .len()
                    .saturating_add(v.head.len())
                    .saturating_add(v.base.len())
                    .saturating_add(32)
            }),
            self.assignment
                .as_ref()
                .map_or(0, |a| a.conversation.as_deref().map_or(0, str::len).saturating_add(64)),
            self.depends_on.len().saturating_mul(5),
        ]
        .into_iter()
        .fold(128, usize::saturating_add)
    }
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
    /// A task took paths to own.
    Claimed {
        /// The paths it took now, beside those it owned already.
        paths: Vec<String>,
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
    /// Its work's branch, worktree or pull request changed.
    Branch {
        /// The branch.
        branch: Option<String>,
        /// The pull request's number.
        pr: Option<u32>,
    },
    /// Its verifier ran.
    Verified {
        /// Whether it passed.
        passed: bool,
        /// What it said.
        summary: String,
    },
    /// The terminal on it closed.
    AgentGone {
        /// The terminal.
        term: TermRef,
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
            Moment::Claimed { paths } => texts(paths),
            Moment::Branch { branch, .. } => branch.as_deref().map_or(0, text),
            Moment::Verified { summary, .. } => text(summary),
            Moment::Note { text: words } => text(words),
            Moment::Reported { report } => text(&report.note)
                .saturating_add(texts(&report.artifacts))
                .saturating_add(report.branch.as_deref().map_or(0, text)),
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
    /// A new placement, in place of the old.
    pub placement: Option<Placement>,
    /// A new verifier of its own.
    pub verifier: Option<String>,
    /// New metadata, in place of the old.
    pub metadata: Option<String>,
}

/// How to start what runs for a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskLaunch {
    /// This worker, over the task's placement; the server places it when absent.
    pub pin: Option<WorkerId>,
    /// Working directory on the worker (usually a repository); the worker's home when empty.
    pub cwd: String,
    /// What runs.
    pub run: Runner,
    /// Extra environment.
    pub env: Vec<(String, String)>,
    /// The grid until a client shows it.
    pub size: Option<Size>,
    /// Start it though a task it depends on is not done yet.
    pub ignore_dependencies: bool,
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
}
