//! Projects: one goal worked on by many agents across the fleet (`docs/decisions/projects.md`).
//!
//! A [`Project`] lives on the server. Its [`Task`]s form a tree by [`Task::parent`] (who split
//! it off, to any depth the project allows) and a graph by [`Task::depends_on`]. Each owns the
//! paths it may write ([`Task::owns`]) unless it only reads, says where it may run as
//! expressions over the workers' [`Facts`] ([`Placement`]), and, once something runs for it,
//! names that terminal, or the thread any agent runs as ([`Assignment`]): Claude Code, Codex,
//! pi, an ACP agent, another agent's CLI or a plain command ([`Runner`]). Claude Code's own
//! subagents and task list inside a session show as [`Natives`] of its node. Everything that
//! happens is kept in the project's timeline ([`TimelineEntry`]) and pushed to every client as a
//! [`ProjectUpdate`], so the tree is followed as it grows, never run where nobody can see it.
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

use crate::agent::{AgentBranch, AgentStatus, PullRequest};
use crate::orchestration::{Size, TermRef};
use crate::terminal::RepoId;

mod review;
pub use review::{
    FINDING_MAX, FINDING_PATH_MAX, FINDINGS_MAX, Finding, REVIEW_DIFF, REVIEW_SUMMARY_MAX,
    ReviewRun, ReviewVerdict, Reviewer,
};

/// The variable naming the server, `host[:port]`, in every session a worker runs: `slopty mcp`
/// and the CLI inside it find the server with no flag.
pub const SERVER_ENV: &str = "SLOPTY_SERVER";
/// The variable naming the project, in the session of an agent spawned for one of its tasks.
pub const PROJECT_ENV: &str = "SLOPTY_PROJECT";
/// The variable naming the task, in the session of an agent spawned for it.
pub const TASK_ENV: &str = "SLOPTY_TASK";
/// The fact on a task's thread row ([`crate::thread::wire::ThreadRow::facts`]) naming the
/// seat it was started at ([`Assignment::thread`]), so the server knows the row as the task's
/// whatever terminal it has.
pub const SEAT_FACT: &str = "slopty.seat";
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
/// How many failing checks of a pull request a card names ([`Checks::failing`]).
pub const CHECKS_NAMED: usize = 5;
/// The longest name of a check a card keeps.
pub const CHECK_NAME_MAX: usize = 128;
/// The longest reason a pull request's checks could not be read ([`Checks::why`]), in bytes.
pub const CHECKS_WHY_MAX: usize = 256;
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

/// How many agents may run and what they may spend, set per project by whoever runs it,
/// within the [`Bounds`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Limits {
    /// Most live agents of this project on any one worker.
    pub live_per_worker: u16,
    /// Most live agents of this project in all.
    pub live_per_project: u16,
    /// How deep its tree of tasks may go: 1 is tasks with no parent only.
    pub depth: u16,
    /// How many timeline entries it keeps.
    pub timeline_kept: u32,
    /// What its agents may spend before it places no new work and asks the person.
    pub budget: Option<Budget>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            live_per_worker: 4,
            live_per_project: 12,
            depth: 8,
            timeline_kept: 4096,
            budget: None,
        }
    }
}

/// What a project's agents may spend, by meter: an open map of a meter's name to its cap.
///
/// [`Budget::USD`] caps the estimated cost, in millionths of a US dollar as the agents' meters
/// say it. Any other name is a plan's rate window as the agents name it
/// ([`crate::thread::Limit::name`]: `five-hour`, `seven-day`), capped in hundredths of a percent
/// of it. At the cap the project starts no task and holds its orchestrator's tells until the
/// person raises the cap or stops it; turns under way finish, so it may pass the cap by up to
/// one turn per live agent.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Budget(pub BTreeMap<String, u64>);

impl Budget {
    /// The most meters one budget names.
    pub const METERS_MAX: usize = 8;
    /// The share of a cap at which the person hears it comes near, in hundredths of a percent.
    pub const NEAR_BP: u64 = 8_000;
    /// The estimated cost's meter.
    pub const USD: &'static str = "usd";

    /// Whether it may be set: up to [`Self::METERS_MAX`] meters, each named within
    /// [`crate::items::FACT_KEY_MAX`] characters with no space, each cap above nothing, and a
    /// window's no more than the whole of it.
    #[must_use]
    pub fn fits(&self) -> bool {
        self.0.len() <= Self::METERS_MAX
            && self.0.iter().all(|(name, cap)| {
                (1..=crate::items::FACT_KEY_MAX).contains(&name.chars().count())
                    && !name.chars().any(|c| c.is_whitespace() || c.is_control())
                    && *cap > 0
                    && (name == Self::USD || *cap <= 10_000)
            })
    }

    /// How `spend` stands against it: each capped meter's use as a share of its cap, in
    /// hundredths of a percent, fullest first.
    #[must_use]
    pub fn against(&self, spend: &Spend) -> Vec<(String, u64)> {
        let mut shares: Vec<(String, u64)> = self
            .0
            .iter()
            .filter_map(|(name, cap)| {
                let used = if name == Self::USD {
                    spend.cost_micro_usd
                } else {
                    u64::from(*spend.windows.get(name)?)
                };
                Some((name.clone(), used.saturating_mul(10_000).checked_div(*cap)?))
            })
            .collect();
        shares.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        shares
    }

    /// `text` as a cap of `meter`, as a person writes it: dollars to the cent for
    /// [`Self::USD`] (`12.50`, `$12.50`), a percent to the hundredth for a plan window (`80`,
    /// `80%`), at most the whole window. `None` for anything else, nothing included.
    #[must_use]
    pub fn cap_of(meter: &str, text: &str) -> Option<u64> {
        let text = text.trim();
        if meter == Self::USD {
            hundredths(text.strip_prefix('$').unwrap_or(text)).map(|c| c.saturating_mul(10_000))
        } else {
            hundredths(text.strip_suffix('%').unwrap_or(text)).filter(|bp| *bp <= 10_000)
        }
    }

    /// `amount` of `meter` for people: dollars to the cent for [`Self::USD`] (`$3.50`), a
    /// percent for a plan window (`80.00%`).
    #[must_use]
    pub fn figure(meter: &str, amount: u64) -> String {
        if meter == Self::USD {
            let cents = amount / 10_000;
            format!("${}.{:02}", cents / 100, cents % 100)
        } else {
            format!("{}.{:02}%", amount / 100, amount % 100)
        }
    }

    /// The meter at or past its cap, the fullest first, when one is.
    #[must_use]
    pub fn reached(&self, spend: &Spend) -> Option<String> {
        self.against(spend).into_iter().find(|(_, bp)| *bp >= 10_000).map(|(name, _)| name)
    }
}

/// A positive decimal with up to two places, in hundredths.
fn hundredths(text: &str) -> Option<u64> {
    let (whole, part) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    if whole.is_empty() || !digits(whole) || part.len() > 2 || !digits(part) {
        return None;
    }
    let part: u64 = format!("{part:0<2}").parse().ok()?;
    let n = whole.parse::<u64>().ok()?.checked_mul(100)?.checked_add(part)?;
    (n > 0).then_some(n)
}

/// What a project's agents spent by their own meters.
///
/// The server tallies it from every thread that ever worked for it: each agent's session, its
/// subagents', and every agent a task was given again. Estimates, as the agents say them.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Spend {
    /// The estimated cost, in millionths of a US dollar.
    pub cost_micro_usd: u64,
    /// Each plan rate window its agents report, by name, at the fullest any of them says, in
    /// hundredths of a percent.
    pub windows: BTreeMap<String, u32>,
}

/// A change to a project's [`Limits`]. What is absent stays.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LimitsChange {
    /// A new [`Limits::live_per_worker`].
    pub live_per_worker: Option<u16>,
    /// A new [`Limits::live_per_project`].
    pub live_per_project: Option<u16>,
    /// A new [`Limits::depth`].
    pub depth: Option<u16>,
    /// A new [`Limits::timeline_kept`].
    pub timeline_kept: Option<u32>,
    /// A new [`Limits::budget`]; an empty one takes the budget away.
    pub budget: Option<Budget>,
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

/// What a project's member is known by.
///
/// Fact keys a tile has (`repo`, `machine`, `cwd`, any other) to the value it must have, or for
/// a path the directory it must be in. A tile matches when every key does; an empty one
/// matches nothing.
pub type Matcher = BTreeMap<String, String>;

/// A project: its goal's home on the server.
///
/// A project is a name and its members; what orchestrates it (its repository, target branch,
/// verifier, orchestrator and tasks) is a part it may have.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Project {
    /// Its name.
    pub id: ProjectId,
    /// What it is for, in a line.
    pub title: String,
    /// What else is in it beside its repository's clones: a matcher each, so one project may
    /// hold two repositories, or one folder name on two machines. At most
    /// [`Project::MEMBERS_MAX`], each within [`Project::member_fits`].
    pub members: Vec<Matcher>,
    /// The repository its tasks work in, as the orchestrator names it (a path or a URL).
    pub repo: String,
    /// Which repository that is on every machine ([`RepoId`]), learned from where its
    /// orchestrator works: a task started with no directory goes beside a clone of it.
    pub repo_id: Option<RepoId>,
    /// The branch finished work lands on.
    pub target: String,
    /// The command that says a task's work is right (`cargo gate`), when there is one.
    pub verifier: Option<String>,
    /// What the person asks a fresh-context reviewer to look for in each task's work once its
    /// verifier passes, when a reviewer reads it before it merges ([`ReviewRun`]).
    pub review: Option<String>,
    /// Whether the merge queue pushes the target branch to its clone's `origin` after each
    /// merge. Off unless the person turns it on: publishing is theirs to choose.
    pub push: bool,
    /// Whether a task waits for the person to start it: its orchestrator's start only proposes
    /// it ([`Task::proposal`]), and the person starts it, or every one proposed, from the board.
    /// Only the person sets it, since starting is what spends their machines and quota.
    pub ask_to_start: bool,
    /// The terminal of the agent the person talks to, which splits the goal into tasks.
    pub orchestrator: Option<TermRef>,
    /// How long the orchestrator worked, idle waits left out: its share, apart from its
    /// tasks'.
    pub orchestrator_spent: Spent,
    /// What every agent that worked for it spent by its own meters, against
    /// [`Limits::budget`].
    pub spend: Spend,
    /// Its limits.
    pub limits: Limits,
    /// Anything its agents keep with it: the text of a JSON object.
    pub metadata: Option<String>,
    /// What each kind of its work needs of the machine it runs on ([`Need`]).
    pub needs: Vec<Need>,
    /// The tasks it runs on a schedule the person set, at most [`SCHEDULES_MAX`].
    pub schedules: Vec<Schedule>,
    /// The person's named commands for it (dev, test, build), at most [`SCRIPTS_MAX`].
    pub scripts: Vec<Script>,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
}

impl Project {
    /// The most facts one member's matcher names.
    pub const MATCHER_KEYS_MAX: usize = 8;
    /// The longest value a matcher names, in bytes: a path's length.
    pub const MATCHER_VALUE_MAX: usize = 1024;
    /// The most members a project names.
    pub const MEMBERS_MAX: usize = 32;

    /// Whether `matcher` may be a member: one to [`Self::MATCHER_KEYS_MAX`] keys, each within
    /// [`crate::items::FACT_KEY_MAX`] characters with no space in it, each value not blank and
    /// at most [`Self::MATCHER_VALUE_MAX`] bytes.
    #[must_use]
    pub fn member_fits(matcher: &Matcher) -> bool {
        (1..=Self::MATCHER_KEYS_MAX).contains(&matcher.len())
            && matcher.iter().all(|(key, value)| {
                (1..=crate::items::FACT_KEY_MAX).contains(&key.chars().count())
                    && !key.chars().any(|c| c.is_whitespace() || c.is_control())
                    && !value.trim().is_empty()
                    && value.len() <= Self::MATCHER_VALUE_MAX
                    && !value.chars().any(char::is_control)
            })
    }
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
    /// The rule: an expression, or `pin`, `online`, `live_per_worker`, `agent` (the agent the
    /// start runs is installed there), `near`, `avoid`.
    pub rule: String,
    /// Whether it held.
    pub held: bool,
    /// Points it added to the score.
    pub points: i64,
    /// Why, when it did not hold or did not evaluate: the error, the cap reached.
    pub detail: String,
    /// The project's need it comes from, by name ([`Need::name`]), when it is one of a need's
    /// rules: what the board says in its place.
    pub need: Option<String>,
}

/// What a kind of work needs of the machine it runs on: the project's say over where its tasks
/// go.
///
/// A task owning any of `paths` gets `require` and `prefer` beside its own rules, every task
/// when `paths` is empty. "Apple work" over the app's crates requires `os == "macos"`, and
/// "Linux first" over everything prefers `os == "linux"`, so work that needs no Mac goes to
/// a Linux worker. The board names the need where a rule of it decided.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Need {
    /// What it is, in a few words: "Apple work".
    pub name: String,
    /// The paths whose owners have it, as [`TaskSpec::owns`] names paths; every task when
    /// empty.
    pub paths: Vec<String>,
    /// Each must hold on a worker for such a task (CEL, as [`Placement::require`]).
    pub require: Vec<String>,
    /// Each that holds scores a worker for such a task.
    pub prefer: Vec<Preference>,
}

impl Need {
    /// The most paths and rules one need names, each.
    pub const ITEMS_MAX: usize = 32;
    /// The most needs a project names.
    pub const MAX: usize = 16;
    /// The longest name, in bytes.
    pub const NAME_MAX: usize = 64;

    /// Whether a task owning `owns` has it: an owned path is one of `paths`, within one, or
    /// holds one.
    #[must_use]
    pub fn applies(&self, owns: &[String]) -> bool {
        let fold = |p: &str| p.trim().trim_matches('/').to_owned();
        let holds = |outer: &str, inner: &str| {
            outer.is_empty()
                || inner == outer
                || inner.strip_prefix(outer).is_some_and(|rest| rest.starts_with('/'))
        };
        self.paths.is_empty()
            || self.paths.iter().any(|need| {
                let need = fold(need);
                owns.iter().map(|o| fold(o)).any(|own| holds(&need, &own) || holds(&own, &need))
            })
    }

    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let texts = self.paths.iter().chain(&self.require).map(|t| t.len().saturating_add(2));
        let prefs = self.prefer.iter().map(|p| p.expr.len().saturating_add(8));
        texts.chain(prefs).fold(self.name.len().saturating_add(16), usize::saturating_add)
    }
}

impl Reason {
    /// What the board calls it: its need's name, else the rule itself.
    #[must_use]
    pub fn said(&self) -> &str {
        self.need.as_deref().unwrap_or(&self.rule)
    }
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

impl Suggestion {
    /// [`Self::why`] for a worker that fits when nothing but room decided it.
    pub const UNDECIDED: &str = "it has room, and nothing is preferred";
    /// The longest [`Self::why`], in bytes.
    pub const WHY_MAX: usize = STATUS_MAX;

    /// What decides it, in a line. For a worker that fits: its pin, then what scored (the
    /// most points first), then what it was required to hold. For one that does not: what
    /// keeps it out.
    #[must_use]
    pub fn why(&self) -> String {
        const BUILT_IN: [&str; 5] =
            ["pin", "online", "live_per_worker", "fleet live_per_worker", "agent"];
        let built_in = |r: &Reason| BUILT_IN.contains(&r.rule.as_str());
        let mut parts: Vec<String> = Vec::new();
        if self.fits {
            if self.reasons.iter().any(|r| r.rule == "pin" && r.held) {
                parts.push("pinned".to_owned());
            }
            let mut scored: Vec<&Reason> = self.reasons.iter().filter(|r| r.points != 0).collect();
            scored.sort_by_key(|r| std::cmp::Reverse(r.points));
            parts.extend(scored.into_iter().map(|r| {
                let sign = if r.points > 0 { "+" } else { "\u{2212}" };
                format!("{} {sign}{}", r.said(), r.points.unsigned_abs())
            }));
            parts.extend(
                self.reasons
                    .iter()
                    .filter(|r| r.held && r.points == 0 && !built_in(r))
                    .map(|r| r.said().to_owned()),
            );
            if parts.is_empty() {
                parts.push(Self::UNDECIDED.to_owned());
            }
        } else {
            parts.extend(self.reasons.iter().filter(|r| !r.held).map(|r| {
                match (built_in(r), &r.need, r.detail.is_empty()) {
                    (true, _, false) => r.detail.clone(),
                    (_, Some(need), _) => format!("fails {need} ({})", r.rule),
                    (_, None, true) => format!("{} does not hold", r.rule),
                    (false, None, false) => format!("{}: {}", r.rule, r.detail),
                }
            }));
        }
        // A need's rules say its name once.
        let mut said = std::collections::HashSet::new();
        parts.retain(|part| said.insert(part.clone()));
        parts.truncate(3);
        let mut why = parts.join(", ");
        if why.len() > Self::WHY_MAX {
            let mut cut = Self::WHY_MAX.saturating_sub(3);
            while !why.is_char_boundary(cut) {
                cut = cut.saturating_sub(1);
            }
            why.truncate(cut);
            why.push('\u{2026}');
        }
        why
    }
}

/// Why the server started a task's terminal where it did, as it ranked the workers then.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Placed {
    /// It was pinned there, by its orchestrator or the person.
    pub pinned: bool,
    /// Its score from the preferences.
    pub score: i64,
    /// What decided it, in a line ([`Suggestion::why`]).
    pub why: String,
}

impl Placed {
    /// How `suggestion` placed it.
    #[must_use]
    pub fn of(suggestion: &Suggestion) -> Self {
        let pinned = suggestion.reasons.iter().any(|r| r.rule == "pin" && r.held);
        Self { pinned, score: suggestion.score, why: suggestion.why() }
    }
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
    /// The person's own Codex, with Slopty's tools on its MCP servers and its brief as its
    /// first prompt; it goes only where `codex` is installed.
    Codex {
        /// The first prompt.
        prompt: Option<String>,
        /// Arguments after `codex`, before the prompt.
        args: Vec<String>,
    },
    /// Any agent, run as a thread of the worker's thread host
    /// ([`crate::orchestration::Verb::StartThread`]): pi, an ACP agent, or Claude Code and
    /// Codex driven that way. Its adapter gives it Slopty's tools and its role through the
    /// agent's own doors; it goes only where the agent is installed.
    Agent {
        /// The agent.
        agent: crate::thread::AgentId,
        /// The first prompt.
        prompt: Option<String>,
        /// The model, by the agent's own id.
        model: Option<String>,
        /// More arguments for the agent, checked by its adapter.
        args: Vec<String>,
    },
}

/// The terminal, or the thread, working on a task.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Assignment {
    /// Its terminal, or for a thread its seat: the id its Slopty tools speak as, and the
    /// terminal its agent runs in when it runs in one.
    pub term: TermRef,
    /// The thread its agent runs as, for a task started as one ([`Runner::Agent`]). Whether
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
    /// Why the server put it on its worker; none for a terminal it was told of.
    pub placed: Option<Placed>,
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
        /// When, by the server's clock.
        at_ms: WallMs,
        /// Whether the target was pushed to its clone's `origin` too.
        pushed: bool,
        /// Why a push asked for did not happen, in git's words: the target moved all the same,
        /// and the person pushes again.
        push_failed: Option<String>,
    },
}

impl Merge {
    /// When it joined the queue, while it waits there.
    #[must_use]
    pub const fn queued(&self) -> Option<WallMs> {
        match self {
            Self::Queued { since_ms } => Some(*since_ms),
            Self::Merged { .. } => None,
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
    /// What its reviewer last said.
    pub reviewed: Option<ReviewRun>,
    /// Its place in the merge queue, or the merge that put its work on the target.
    pub merge: Option<Merge>,
    /// What the server last did for it around its agent: a clone made, its branch brought
    /// home, verified or merged.
    pub step: Option<TaskStep>,
    /// A start its orchestrator proposed, which waits for the person ([`Project::ask_to_start`]).
    pub proposal: Option<Proposal>,
    /// How long its agents worked on it, idle waits left out.
    pub spent: Spent,
    /// What its pull request's own checks last said, while it has one.
    pub checks: Option<Checks>,
    /// When it was made, by the server's clock.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
    /// Its attempts, when several agents try it at once
    /// ([`crate::orchestration::Verb::TaskAttempts`]).
    pub attempts: Option<Attempts>,
}

/// The kind of a task that is one attempt at its parent: several try the same brief, each in
/// a worktree of its own, and the one picked lands ([`Attempts`]).
pub const ATTEMPT_KIND: &str = "attempt";

/// Most attempts at one task.
pub const ATTEMPTS_MAX: usize = 6;

/// A task tried by several agents at once, each attempt a sub-task of its own.
///
/// Each attempt ([`ATTEMPT_KIND`]) runs on its own worker or model. Every attempt is verified
/// (and read by the reviewer) as a task is, but only the one picked joins the merge queue; the
/// others stop, their agents closed and their worktrees freed, their branches kept.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Attempts {
    /// The attempts, in the order they were made; at most [`ATTEMPTS_MAX`].
    pub tried: Vec<TaskId>,
    /// The one picked to land, once the person or the orchestrator picked it
    /// ([`crate::orchestration::Verb::TaskPick`]).
    pub picked: Option<TaskId>,
}

/// The most schedules a project keeps.
pub const SCHEDULES_MAX: usize = 16;
/// The most scripts a project keeps.
pub const SCRIPTS_MAX: usize = 32;
/// The longest script name, in bytes ([`Script::name`]).
pub const SCRIPT_NAME_MAX: usize = 32;
/// The longest script command line, in bytes ([`Script::command`]).
pub const SCRIPT_COMMAND_MAX: usize = 4096;

/// A command the person named for a project: `dev`, `test`, `build`, anything.
///
/// Run, it opens a terminal of the person's own in the project's folder or a task's worktree,
/// on the worker they choose (`docs/decisions/projects.md`, "A project keeps the person's
/// scripts").
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Script {
    /// Its name: letters, digits, `-`, `_` and `.`, at most [`SCRIPT_NAME_MAX`] bytes, unique
    /// in its project.
    pub name: String,
    /// The command line, as the person would type it in their shell, at most
    /// [`SCRIPT_COMMAND_MAX`] bytes.
    pub command: String,
    /// Where under the folder it runs in, relative (`web`); the folder itself when absent.
    pub dir: Option<String>,
}

impl Script {
    /// Why `self` cannot be kept, in words; none when it can.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        let named = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if self.name.is_empty()
            || self.name.len() > SCRIPT_NAME_MAX
            || !self.name.chars().all(named)
        {
            return Some(format!(
                "a script's name is 1 to {SCRIPT_NAME_MAX} letters, digits, '-', '_' or '.'; \
                 {:?} is not",
                self.name
            ));
        }
        if self.command.trim().is_empty() {
            return Some(format!("script {} runs no command", self.name));
        }
        if self.command.len() > SCRIPT_COMMAND_MAX {
            return Some(format!("a script's command is at most {SCRIPT_COMMAND_MAX} bytes"));
        }
        let inside = |dir: &str| {
            let path = std::path::Path::new(dir);
            !dir.is_empty()
                && path.components().all(|c| matches!(c, std::path::Component::Normal(_)))
        };
        match &self.dir {
            Some(dir) if !inside(dir) => {
                Some(format!("a script runs in a folder under the project's, not {dir:?}"))
            }
            _ => None,
        }
    }
}
/// The longest schedule rule kept, in bytes ([`ScheduleSpec::when`]).
pub const WHEN_MAX: usize = 128;
/// The longest time zone name kept, in bytes ([`ScheduleSpec::zone`]).
pub const ZONE_MAX: usize = 64;

/// What a schedule makes and starts each time it runs, and when.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ScheduleSpec {
    /// The task each run makes, as [`crate::orchestration::Verb::TaskCreate`] would: it
    /// hangs from no other and depends on none.
    pub task: TaskSpec,
    /// What starts for it, as [`crate::orchestration::Verb::TaskSpawn`] would start it.
    pub launch: TaskLaunch,
    /// When it runs: five cron fields (minute, hour, day of the month, month, day of the
    /// week), or `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`.
    pub when: String,
    /// The IANA time zone [`Self::when`] is read in, the person's own; the server's own when
    /// empty.
    pub zone: String,
    /// It runs only when the person says ([`crate::orchestration::Verb::ScheduleRun`]).
    pub paused: bool,
}

/// One run of a schedule: the task it made, or why it made none or could not start it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ScheduleRun {
    /// When, by the server's clock.
    pub at_ms: WallMs,
    /// The task it made.
    pub task: Option<TaskId>,
    /// Why it made no task, or could not start the one it made.
    pub why: Option<String>,
}

/// A task the server makes and starts on a schedule the person set.
///
/// A nightly dependency bump, a weekly audit. Only the person sets one, since every run spends
/// the plan; it runs under the project's placement, limits and budget as any start does.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Schedule {
    /// Its number within the project.
    pub id: u32,
    /// What it makes, and when.
    pub spec: ScheduleSpec,
    /// When it runs next, by the server's clock; none while paused.
    pub next_ms: Option<WallMs>,
    /// Its last run.
    pub last: Option<ScheduleRun>,
    /// When it was set.
    pub created_ms: WallMs,
}

impl Schedule {
    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        let ScheduleSpec { task, launch, when, zone, .. } = &self.spec;
        let texts = [&task.title, &task.brief, &task.kind, &launch.cwd, when, zone]
            .into_iter()
            .map(String::len)
            .chain(task.owns.iter().map(String::len))
            .chain(task.verifier.as_deref().map(str::len))
            .chain(task.placement.require.iter().map(String::len))
            .chain(task.placement.prefer.iter().map(|p| p.expr.len()))
            .chain(launch.env.iter().map(|(k, v)| k.len().saturating_add(v.len())))
            .chain(self.last.as_ref().and_then(|l| l.why.as_deref()).map(str::len));
        let run = match &launch.run {
            Runner::Claude { prompt, args } | Runner::Codex { prompt, args } => prompt
                .as_deref()
                .map_or(0, str::len)
                .saturating_add(args.iter().map(String::len).sum()),
            Runner::Command { argv } => argv.iter().map(String::len).sum(),
            Runner::Agent { agent, prompt, model, args } => agent
                .0
                .len()
                .saturating_add(prompt.as_deref().map_or(0, str::len))
                .saturating_add(model.as_deref().map_or(0, str::len))
                .saturating_add(args.iter().map(String::len).sum()),
        };
        texts.fold(run.saturating_add(160), |sum, len| sum.saturating_add(len).saturating_add(5))
    }
}

/// How long an agent worked: the stretches it was at work, the waits between left out.
///
/// The server follows the agent's status ([`Spent::works`]): a stretch begins when it starts
/// working and ends when it stops, so a wait at the prompt or on the person is not counted.
/// The stretch under way is counted by whoever reads it ([`Spent::at`]), so a running clock
/// needs no message per second.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Spent {
    /// The stretches that ended, in milliseconds.
    pub active_ms: u64,
    /// When the stretch under way began, while the agent works.
    pub since_ms: Option<WallMs>,
}

/// What a pull request's own checks say, as its forge reports them (`gh pr checks`, a merge
/// request's pipeline).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Checks {
    /// Where they stand together.
    pub state: ChecksState,
    /// How many passed.
    pub passed: u16,
    /// How many failed or were cancelled.
    pub failed: u16,
    /// How many still run or wait to.
    pub pending: u16,
    /// How many were skipped.
    pub skipped: u16,
    /// The names of those that failed, at most [`CHECKS_NAMED`], each at most
    /// [`CHECK_NAME_MAX`] bytes.
    pub failing: Vec<String>,
    /// Why they could not be read, for [`ChecksState::Unknown`]: the forge's command missing
    /// or not signed in, in its words, at most [`CHECKS_WHY_MAX`] bytes.
    pub why: Option<String>,
    /// When the forge said so, by the server's clock.
    pub at_ms: WallMs,
}

impl Checks {
    /// The most it takes on the wire.
    pub const MAX_BYTES: usize = CHECKS_NAMED * (CHECK_NAME_MAX + 2) + CHECKS_WHY_MAX + 44;

    /// About how many bytes it takes on the wire, never less.
    #[must_use]
    pub fn approx_bytes(&self) -> usize {
        self.failing
            .iter()
            .map(|n| n.len().saturating_add(10))
            .fold(40, usize::saturating_add)
            .saturating_add(self.why.as_deref().map_or(0, str::len))
    }

    /// Whether it says the same as `other`, whenever each was read.
    #[must_use]
    pub fn says_as(&self, other: &Self) -> bool {
        Self { at_ms: other.at_ms, ..self.clone() } == *other
    }
}

/// Where a pull request's checks stand together.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ChecksState {
    /// It has none.
    None,
    /// Some still run, and none has failed.
    Pending,
    /// Every one passed or was skipped.
    Passing,
    /// At least one failed.
    Failing,
    /// The forge could not be asked: its command is missing or not signed in on the machine
    /// the work is on ([`Checks::why`]). Asked again later; nothing is known meanwhile.
    Unknown,
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
    /// Whether an agent with `status` is at work: thinking, running a tool, or waiting on
    /// background work it started. Idle at its prompt, done, blocked on the person, or holding
    /// only scheduled prompts, it is not.
    #[must_use]
    pub const fn works(status: &AgentStatus) -> bool {
        match status {
            AgentStatus::Working | AgentStatus::Tool { .. } => true,
            AgentStatus::Waiting { tasks, .. } => *tasks > 0,
            AgentStatus::None
            | AgentStatus::Idle
            | AgentStatus::Blocked(_)
            | AgentStatus::Done
            | AgentStatus::Failed { .. } => false,
        }
    }

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

/// A start the orchestrator proposed for a task, held until the person starts it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Proposal {
    /// How to start it, as the orchestrator asked.
    pub launch: TaskLaunch,
    /// What the board shows of it.
    pub proposed: Proposed,
}

/// A proposed start, as a card shows it: what would run, and where the server would put it
/// if it started now.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Proposed {
    /// When it was proposed.
    pub since_ms: WallMs,
    /// What would run: `claude`, or the program's name.
    pub runs: String,
    /// The worker it would go to now, when one fits.
    pub on: Option<WorkerId>,
    /// Why there, or why nowhere, in a line ([`Suggestion::why`]).
    pub why: String,
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
    /// A reviewer with fresh context reading the task's work, in a session of its own.
    Review,
    /// The merge queue's rebase of the task's work onto the target. It is a step of its own
    /// only when it fails, and then it conflicts: the work goes back to its agent to resolve.
    Rebase,
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
    /// The git ref its base commit is kept under in a mirror of `project`'s repository.
    #[must_use]
    pub fn base_ref(&self, project: &ProjectId) -> String {
        format!("refs/slopty/{project}/{}/base", self.id)
    }

    /// The branch its work lands as in the orchestrator's clone when it was done on another
    /// machine: `slopty/<project>/<task>`, a name only the server sets.
    #[must_use]
    pub fn home_branch(project: &ProjectId, task: TaskId) -> String {
        format!("slopty/{project}/{task}")
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
            reviewed: self.reviewed.clone(),
            merge: self.merge.clone(),
            step: self.step.clone(),
            proposed: self.proposal.as_ref().map(|p| p.proposed.clone()),
            pin: self.placement.pin,
            spent: self.spent,
            checks: self.checks.clone(),
            natives: natives.counts(),
            created_ms: self.created_ms,
            updated_ms: self.updated_ms,
            attempts: self.attempts.clone(),
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
    /// What its reviewer last said.
    pub reviewed: Option<ReviewRun>,
    /// Its place in the merge queue, or its merge.
    pub merge: Option<Merge>,
    /// What the server last did for it around its agent.
    pub step: Option<TaskStep>,
    /// The worker its placement is pinned to, by its orchestrator or the person's "Run on".
    pub pin: Option<WorkerId>,
    /// A start its orchestrator proposed, waiting for the person.
    pub proposed: Option<Proposed>,
    /// How long its agents worked on it, idle waits left out.
    pub spent: Spent,
    /// What its pull request's own checks last said, while it has one.
    pub checks: Option<Checks>,
    /// How many natives its node holds.
    pub natives: NativeCounts,
    /// When it was made.
    pub created_ms: WallMs,
    /// When it last changed.
    pub updated_ms: WallMs,
    /// Its attempts, when several agents try it at once.
    pub attempts: Option<Attempts>,
}

impl TaskCard {
    /// The most a card takes on the wire, from the bounds on its fields, with room for the
    /// encoding's lengths and tags.
    pub const MAX_BYTES: usize = Bounds::CEILING.title_max as usize
        + STATUS_MAX
        + KIND_MAX
        + DEPENDS_MAX * 5
        + 2 * SUMMARY_MAX
        + 8 * REF_MAX
        + ReviewRun::MAX_BYTES
        + 2 * Suggestion::WHY_MAX
        + KIND_MAX
        + Checks::MAX_BYTES
        + ATTEMPTS_MAX * 5
        + 800;
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
            self.verified.as_ref().map_or(0, VerifierRun::approx_bytes),
            self.reviewed.as_ref().map_or(0, ReviewRun::approx_bytes),
            self.checks.as_ref().map_or(0, Checks::approx_bytes),
            self.merge.as_ref().map_or(0, |m| match m {
                Merge::Queued { .. } => 16,
                Merge::Merged { target, head, push_failed, .. } => target
                    .len()
                    .saturating_add(head.len())
                    .saturating_add(push_failed.as_deref().map_or(0, str::len))
                    .saturating_add(32),
            }),
            self.assignment.as_ref().map_or(0, |a| {
                let placed = a.placed.as_ref().map_or(0, |p| p.why.len().saturating_add(16));
                a.conversation
                    .as_deref()
                    .map_or(0, str::len)
                    .saturating_add(placed)
                    .saturating_add(64)
            }),
            self.depends_on.len().saturating_mul(5),
            self.attempts.as_ref().map_or(0, |a| a.tried.len().saturating_mul(5).saturating_add(8)),
            self.step.as_ref().map_or(0, TaskStep::approx_bytes),
            self.proposed
                .as_ref()
                .map_or(0, |p| p.why.len().saturating_add(p.runs.len()).saturating_add(48)),
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
    /// Its orchestrator proposed its start, which waits for the person.
    Proposed {
        /// The worker it would go to then.
        on: Option<WorkerId>,
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
    /// Its verifier ran, or the person recorded what it said.
    Verified(VerifierRun),
    /// Its pull request's checks came to stand otherwise: started, passed or failed.
    Checks(Checks),
    /// A reviewer, or the person, said whether the work may merge.
    Reviewed(ReviewRun),
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
    /// What the project's work needs of its machines was said ([`Need`]): the needs' names,
    /// none when they were all taken away.
    Needs {
        /// Each need's name.
        names: Vec<String>,
    },
    /// Its agents' spend came near a cap of its budget ([`Budget::NEAR_BP`]) or reached it,
    /// where it starts no task until the person raises the cap.
    Budget {
        /// The meter ([`Budget::USD`], or a plan window's name).
        meter: String,
        /// How much of its cap is spent, in hundredths of a percent.
        share_bp: u64,
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
            Moment::Claimed { paths } | Moment::Needs { names: paths } => texts(paths),
            Moment::Branch { branch, .. } => branch.as_deref().map_or(0, text),
            Moment::Verified(run) => run.approx_bytes(),
            Moment::Checks(checks) => checks.approx_bytes(),
            Moment::Reviewed(run) => run.approx_bytes(),
            Moment::Note { text: words } | Moment::Told { text: words } => text(words),
            Moment::Reported { report } => text(&report.note)
                .saturating_add(texts(&report.artifacts))
                .saturating_add(report.branch.as_deref().map_or(0, text)),
            Moment::Step(step) => step.approx_bytes(),
            Moment::Budget { meter, .. } => text(meter),
            Moment::Created
            | Moment::Orchestrator { .. }
            | Moment::Limits { .. }
            | Moment::Assigned { .. }
            | Moment::Proposed { .. }
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
            self.repo_id.as_ref().map_or(0, |id| id.keys().map(str::len).sum()),
            self.needs.iter().map(Need::approx_bytes).sum(),
            self.schedules.iter().map(Schedule::approx_bytes).sum(),
            self.spend.windows.keys().map(|k| k.len().saturating_add(5)).sum(),
            self.limits
                .budget
                .as_ref()
                .map_or(0, |b| b.0.keys().map(|k| k.len().saturating_add(9)).sum()),
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
    /// Where it runs, over its placement's pin: what the person's "Run on" sets.
    pub run_on: Option<RunOn>,
    /// A new verifier of its own.
    pub verifier: Option<String>,
    /// New metadata, in place of the old.
    pub metadata: Option<String>,
}

/// Where a task runs, as [`TaskChange::run_on`] says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RunOn {
    /// On this worker and no other: its placement's pin.
    Worker(WorkerId),
    /// Wherever its placement's rules choose: no pin.
    Anywhere,
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

    /// A budget weighs each meter it caps: the cost in millionths of a dollar, a plan window in
    /// hundredths of a percent; a meter its agents never reported weighs nothing. The fullest
    /// comes first, and a cap is reached at its whole.
    #[test]
    fn a_budget_weighs_each_capped_meter_and_says_which_is_reached() {
        let budget = Budget(BTreeMap::from([
            (Budget::USD.to_owned(), 50_000_000),
            ("five-hour".to_owned(), 8_000),
            ("seven-day".to_owned(), 5_000),
        ]));
        let spend = Spend {
            cost_micro_usd: 40_000_000,
            windows: BTreeMap::from([("five-hour".to_owned(), 8_800)]),
        };
        assert_eq!(
            budget.against(&spend),
            [("five-hour".to_owned(), 11_000), (Budget::USD.to_owned(), 8_000)]
        );
        assert_eq!(budget.reached(&spend).as_deref(), Some("five-hour"));
        let under = Spend { cost_micro_usd: 49_999_999, windows: BTreeMap::new() };
        assert_eq!(budget.reached(&under), None);
        assert_eq!(Budget::cap_of(Budget::USD, "$12.5"), Some(12_500_000));
        assert_eq!(Budget::cap_of("five-hour", "80%"), Some(8_000));
        assert_eq!(Budget::cap_of("five-hour", "33.33"), Some(3_333));
        for bad in ["0", "1.234", "-3", "ten", "", "101"] {
            assert_eq!(Budget::cap_of("five-hour", bad), None, "{bad}");
        }
        assert_eq!(Budget::figure(Budget::USD, 1_234_567), "$1.23");
        assert_eq!(Budget::figure("five-hour", 8_000), "80.00%");
        assert!(budget.fits());
        for bad in [
            Budget(BTreeMap::from([(Budget::USD.to_owned(), 0)])),
            Budget(BTreeMap::from([("five-hour".to_owned(), 10_001)])),
            Budget(BTreeMap::from([("two words".to_owned(), 1)])),
            Budget((0..=Budget::METERS_MAX).map(|n| (format!("m{n}"), 1)).collect()),
        ] {
            assert!(!bad.fits(), "{bad:?}");
        }
    }

    /// A member names one to a few facts, each a key with no space and a value that is not
    /// blank; anything else is refused.
    #[test]
    fn a_member_names_facts_within_bounds() {
        let member = |pairs: &[(&str, &str)]| -> Matcher {
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
        };
        assert!(Project::member_fits(&member(&[("repo", "github.com/o/api")])));
        assert!(Project::member_fits(&member(&[("machine", "studio"), ("cwd", "~/notes")])));
        assert!(!Project::member_fits(&member(&[])), "an empty one would match nothing");
        assert!(!Project::member_fits(&member(&[("two words", "v")])));
        assert!(!Project::member_fits(&member(&[("cwd", " ")])));
        let long = "/".repeat(Project::MATCHER_VALUE_MAX + 1);
        assert!(!Project::member_fits(&member(&[("cwd", &long)])));
        let many: Matcher =
            (0..=Project::MATCHER_KEYS_MAX).map(|n| (format!("k{n}"), "v".to_owned())).collect();
        assert!(!Project::member_fits(&many));
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

    /// Time spent counts the stretches an agent was at work and leaves out its waits: idle at
    /// its prompt, blocked on the person, or holding only a scheduled prompt. A stretch under
    /// way counts up to the moment it is read, and a status that does not change the stretch
    /// changes nothing.
    #[test]
    fn spent_counts_the_stretches_at_work() {
        use crate::agent::BlockReason;
        let at = |s: u64| WallMs::from_millis(1_000_000 + s * 1_000);
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
            assert_eq!(Spent::works(status), works, "{status:?}");
        }
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

    /// A worker's ranking in a line: its pin, what scored the most first, then what it was
    /// required to hold; for a worker that does not fit, what keeps it out. A long line is cut
    /// at a character, never inside one.
    #[test]
    fn a_ranking_says_what_decides_it() {
        let reason = |rule: &str, held: bool, points: i64, detail: &str| Reason {
            rule: rule.to_owned(),
            held,
            points,
            detail: detail.to_owned(),
            need: None,
        };
        let ranked = |fits: bool, reasons: Vec<Reason>| Suggestion {
            worker: WorkerId::nil(),
            name: "studio".to_owned(),
            fits,
            score: reasons.iter().map(|r| r.points).sum(),
            reasons,
        };
        let fits = ranked(
            true,
            vec![
                reason("online", true, 0, ""),
                reason("live_per_worker", true, 0, ""),
                reason(r#"os == "macos""#, true, 0, ""),
                reason("has(probes.cuda)", true, 10, ""),
                reason("near #2", true, 100, ""),
                reason("avoid #3", false, 0, ""),
            ],
        );
        assert_eq!(fits.why(), r#"near #2 +100, has(probes.cuda) +10, os == "macos""#);
        let pinned =
            ranked(true, vec![reason("pin", true, 0, ""), reason("avoid #1", false, -100, "")]);
        assert_eq!(pinned.why(), "pinned, avoid #1 \u{2212}100");
        assert_eq!(Placed::of(&pinned), Placed { pinned: true, score: -100, why: pinned.why() });
        let bare = ranked(true, vec![reason("online", true, 0, "")]);
        assert_eq!(bare.why(), "it has room, and nothing is preferred");
        let out = ranked(
            false,
            vec![
                reason("online", false, 0, "not online"),
                reason(r#"os == "linux""#, false, 0, "false here"),
            ],
        );
        assert_eq!(out.why(), r#"not online, os == "linux": false here"#);
        let long = ranked(true, vec![reason(&"é".repeat(Suggestion::WHY_MAX), true, 0, "")]);
        let why = long.why();
        assert!(why.len() <= Suggestion::WHY_MAX && why.ends_with('\u{2026}'), "{}", why.len());

        let of_need = |rule: &str, held: bool, points: i64, need: &str| Reason {
            need: Some(need.to_owned()),
            ..reason(rule, held, points, "false here")
        };
        let linux = ranked(
            true,
            vec![of_need(r#"os == "linux""#, true, 20, "Linux first"), reason("x", true, 0, "")],
        );
        assert_eq!(linux.why(), "Linux first +20, x");
        let apple = ranked(
            false,
            vec![
                of_need(r#"os == "macos""#, false, 0, "Apple work"),
                of_need("has(toolchains.xcode)", false, 0, "Apple work"),
            ],
        );
        assert_eq!(
            apple.why(),
            r#"fails Apple work (os == "macos"), fails Apple work (has(toolchains.xcode))"#
        );
        let no_codex = ranked(false, vec![reason("agent", false, 0, "codex is not installed")]);
        assert_eq!(no_codex.why(), "codex is not installed");
        let codex = ranked(true, vec![reason("agent", true, 0, ""), reason("x", true, 0, "")]);
        assert_eq!(codex.why(), "x", "an agent installed says nothing a worker does not share");
    }

    /// A need is a task's when it owns one of the need's paths, a path within one, or one that
    /// holds one; a need of no paths is every task's.
    #[test]
    fn a_need_follows_the_paths_a_task_owns() {
        let need = |paths: &[&str]| Need {
            name: "Apple work".to_owned(),
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            require: vec![r#"os == "macos""#.to_owned()],
            prefer: Vec::new(),
        };
        let owns = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
        let apple = need(&["crates/slopty-ui", "apps/slopty/"]);
        assert!(apple.applies(&owns(&["crates/slopty-ui/src/project/view.rs"])));
        assert!(apple.applies(&owns(&["docs", "apps/slopty"])));
        assert!(apple.applies(&owns(&["crates"])), "a path that holds the need's");
        assert!(!apple.applies(&owns(&["crates/slopty-ui-kit"])), "a sibling is not within");
        assert!(!apple.applies(&owns(&["crates/slopty-server"])));
        assert!(!apple.applies(&[]), "a task that owns nothing has no path's need");
        assert!(need(&[]).applies(&owns(&["anything"])) && need(&[]).applies(&[]));
    }
}
