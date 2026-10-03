//! Projects on the server (`docs/decisions/projects.md`).
//!
//! The records the store keeps and the rules a change keeps: a task that writes owns paths no
//! other live task holds, its dependencies never lead back to it, its tree stays within the
//! project's depth, a merged task stays merged, and one terminal works on a task at a time.
//! How many agents run is counted from the terminals that are live, never from what a task's
//! state says, so nothing that runs escapes the limits.
//!
//! Every change answers with the `Change`s it made, in order: the hub pushes each to clients
//! as a [`ProjectUpdate`] and hands the store what it must keep ([`Kept`]). A refused change
//! leaves everything as it was.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::{AgentBranch, AgentStatus, BlockReason};
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef};
use slopty_proto::project::{
    ARTIFACTS_MAX, AgentReport, Assignment, Budget, CHECK_NAME_MAX, CHECKS_NAMED, CHECKS_WHY_MAX,
    Checks, ChecksState, DEPENDS_MAX, KIND_MAX, Limits, LimitsChange, Live, METADATA_MAX, Matcher,
    Merge, Moment, NOTE_MAX, Native, NativeAgent, NativeChange, Natives, Need, NodeDetail,
    NodeNatives, Placed, Project, ProjectStatus, ProjectUpdate, Proposal, REF_MAX, Report, RunOn,
    STATUS_MAX, SUMMARY_MAX, Spent, StepState, Stretch, TIMELINE_BYTES_KEPT, TIMELINE_PAGE,
    TIMELINE_PAGE_BYTES, Task, TaskChange, TaskId, TaskSpec, TaskState, TaskStep, TimelineEntry,
    VerifierRun,
};
/// What a [`Policy`] is made of, for the binary that reads it from the person's settings.
pub use slopty_proto::project::{Bounds, ProjectId};
use slopty_proto::terminal::RepoId;

use crate::placement;

/// Latest timeline entries a connecting client gets, and a status read with no cursor.
pub const RECENT_ENTRIES: usize = 64;
/// Subagents and task-list items kept per node, the oldest dropped first.
pub const NATIVES_KEPT: usize = 256;
/// Sessions whose natives are kept until a task takes the session on, the oldest dropped first.
const UNCLAIMED_KEPT: usize = 256;
/// The fewest timeline entries a project may keep.
const TIMELINE_LEAST: u32 = 16;

/// A task's pull request, for its checks to be read where its agent worked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrWatch {
    /// Its project.
    pub project: ProjectId,
    /// The task.
    pub task: TaskId,
    /// The worker its agent ran on.
    pub worker: WorkerId,
    /// The worktree it worked in there.
    pub cwd: String,
    /// The pull request's number.
    pub number: u32,
    /// A GitLab merge request.
    pub merge_request: bool,
}

/// Who asks for a change. The person may do anything a verb allows; an agent may not answer
/// a permission, merge a task or record what its verifier said, which are the person's or the
/// merge queue's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Caller {
    /// The person, from a client app or the CLI in a shell no agent runs in.
    Person,
    /// An AI agent: an MCP surface, or the CLI inside an agent's terminal.
    Agent,
}

/// The store's file: every project whole, and the terminals the server watches, as of the
/// `through`th change.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    /// By name.
    pub projects: Vec<Record>,
    /// The terminals the server watches, whatever project they are in.
    pub watched: Vec<Watched>,
    /// How many changes it holds: the store's log goes on from the next.
    pub through: u64,
}

/// What an agent did to a terminal.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Drove {
    /// It typed into it.
    Typed,
    /// It opened it: `by` is the terminal the agent proved it spoke from, when it proved one.
    Opened {
        /// The opener's terminal.
        by: Option<SessionId>,
    },
}

/// A terminal the server watches for as long as it lives, kept across a restart so a server
/// that comes back still holds it to the bounds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Watched {
    /// The terminal.
    pub term: TermRef,
    /// The server started an agent there in `default` mode, which it may not leave.
    pub locked: bool,
    /// What an agent did to it: the CLI in it speaks for an agent, and it counts as one.
    pub drove: Option<Drove>,
}

/// One change the store keeps.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Keep {
    /// A project's.
    Project(Box<Kept>),
    /// A terminal watched, as it is now.
    Watch(Watched),
    /// A terminal no longer watched.
    Unwatch(SessionId),
    /// A project the person let go, with all it held.
    Forget(ProjectId),
}

/// One change a project took, whole: what the store's log keeps, and what a client is pushed
/// as a [`ProjectUpdate`] (with the task as a card).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Kept {
    /// Which project.
    pub project: ProjectId,
    /// The project as it is now, when this changed it.
    pub record: Option<Project>,
    /// The task as it is now, when this changed one.
    pub task: Option<Task>,
    /// A native leaf as it is now, when this changed one.
    pub native: Option<NativeChange>,
    /// What happened, when it is worth the timeline.
    pub entry: Option<TimelineEntry>,
}

/// A change as the model makes it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Change {
    /// The change.
    pub kept: Kept,
    /// Whether the store keeps it. An agent's working and waiting flip at every turn and are
    /// known again from its worker when it registers, so they are pushed and never written.
    pub durable: bool,
}

impl ProjectsFile {
    /// Take a change in, as the hub made it: the store's replica and a replay of its log stay
    /// the hub's state.
    pub fn apply(&mut self, keep: &Keep) {
        self.through = self.through.saturating_add(1);
        match keep {
            Keep::Project(kept) => self.apply_project(kept),
            Keep::Watch(watched) => {
                match self.watched.iter_mut().find(|w| w.term.session == watched.term.session) {
                    Some(held) => *held = *watched,
                    None => self.watched.push(*watched),
                }
            }
            Keep::Unwatch(session) => self.watched.retain(|w| w.term.session != *session),
            Keep::Forget(project) => self.projects.retain(|r| r.project.id != *project),
        }
    }

    fn apply_project(&mut self, kept: &Kept) {
        let at = self.projects.iter().position(|r| r.project.id == kept.project);
        let record = match (at, &kept.record) {
            (Some(i), _) => self.projects.get_mut(i),
            (None, Some(project)) => {
                let at = self.projects.partition_point(|r| r.project.id < project.id);
                self.projects.insert(at, Record::new(project.clone()));
                self.projects.get_mut(at)
            }
            (None, None) => None,
        };
        let Some(record) = record else { return };
        if let Some(project) = &kept.record {
            record.project = project.clone();
            record.trim_timeline();
        }
        if let Some(task) = &kept.task {
            match record.tasks.binary_search_by_key(&task.id, |t| t.id) {
                Ok(i) => {
                    if let Some(t) = record.tasks.get_mut(i) {
                        t.clone_from(task);
                    }
                }
                Err(i) => record.tasks.insert(i, task.clone()),
            }
        }
        if let Some(change) = &kept.native
            && let Some(node) = record.natives_mut(change.task)
        {
            take_leaf(node, &change.native);
        }
        if let Some(entry) = &kept.entry {
            record.next_seq = record.next_seq.max(entry.seq.saturating_add(1));
            record.push_entry(entry.clone());
        }
    }
}

/// One project as the store keeps it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct Record {
    /// The project.
    pub project: Project,
    /// Its tasks by number.
    pub tasks: Vec<Task>,
    /// Each node's natives, for the nodes that have any.
    pub natives: Vec<NodeNatives>,
    /// Its latest timeline entries, oldest first, as many as its limits keep and at most
    /// [`TIMELINE_BYTES_KEPT`] of them.
    pub timeline: VecDeque<TimelineEntry>,
    /// The next entry's number.
    pub next_seq: u64,
    /// What [`Self::timeline`] takes, by [`TimelineEntry::approx_bytes`]; counted again when
    /// the file is read.
    #[serde(skip)]
    pub timeline_bytes: usize,
}

/// A [`Record`] as the file holds it, counted again as it is read.
#[derive(Deserialize)]
struct Stored {
    project: Project,
    tasks: Vec<Task>,
    natives: Vec<NodeNatives>,
    timeline: VecDeque<TimelineEntry>,
    next_seq: u64,
}

impl From<Stored> for Record {
    fn from(stored: Stored) -> Self {
        let Stored { project, tasks, natives, timeline, next_seq } = stored;
        let mut record = Self { project, tasks, natives, timeline, next_seq, timeline_bytes: 0 };
        record.recount();
        record
    }
}

/// What the person allows, from the server's settings.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Policy {
    /// The bounds on every project, and on the fleet.
    pub bounds: Bounds,
    /// The projects whose agents may be started with flags that loosen Claude Code's
    /// permissions.
    pub permission_flags: BTreeSet<ProjectId>,
}

impl Policy {
    /// The bounds as they apply to `project`.
    #[must_use]
    pub fn bounds_for(&self, project: Option<&ProjectId>) -> Bounds {
        let permission_flags = project.is_some_and(|p| self.permission_flags.contains(p));
        Bounds { permission_flags, ..self.bounds }
    }
}

/// What runs now, as the hub sees it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Running<'a> {
    /// Every terminal a worker has open.
    pub terminals: &'a HashSet<TermRef>,
    /// Every one of them an agent runs in.
    pub agents: &'a HashSet<TermRef>,
    /// Starts placed whose terminal is not live yet.
    pub starting: &'a [Starting],
}

/// A start the hub placed, under a terminal id the hub chose. It counts against every limit
/// from the moment it is placed until what it started counts on its own (a task's terminal
/// once live, a plain agent once its agent shows), or its grace ends, so two starts never both
/// take the last place and a start whose answer was lost is still counted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Starting {
    /// Which start.
    pub id: u64,
    /// The terminal it opens.
    pub term: TermRef,
    /// For which project's task; a plain start when absent.
    pub task: Option<(ProjectId, TaskId)>,
    /// Whether an agent is to run in it (a plain agent's start); it counts until one shows.
    pub agent: bool,
    /// When it was placed, or when its worker answered.
    pub since: tokio::time::Instant,
    /// Its worker answered, or its answer was lost: from then it counts for its grace only.
    pub answered: bool,
    /// The conversation a task's agent was started under, for the task that takes it on once
    /// it shows up after a lost answer.
    pub conversation: Option<String>,
    /// Why a task's start went to its worker, for the task that takes it on.
    pub placed: Option<Placed>,
}

/// A new project's fields.
#[derive(Clone, Debug)]
pub(crate) struct NewProject {
    pub id: ProjectId,
    pub title: String,
    pub members: Vec<Matcher>,
    pub repo: String,
    pub target: String,
    pub verifier: Option<String>,
    pub review: Option<String>,
    pub push: bool,
    pub ask_to_start: bool,
    pub orchestrator: Option<TermRef>,
    pub limits: LimitsChange,
    pub metadata: Option<String>,
}

/// A change to a project's own fields.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProjectChange {
    pub members: Option<Vec<Matcher>>,
    pub orchestrator: Option<TermRef>,
    pub verifier: Option<String>,
    pub review: Option<String>,
    pub push: Option<bool>,
    pub ask_to_start: Option<bool>,
    pub limits: LimitsChange,
    pub metadata: Option<String>,
}

/// Who a task is assigned to.
#[derive(Clone, Debug)]
pub(crate) struct Assignee<'a> {
    /// The terminal.
    pub term: TermRef,
    /// Slopty started it for the task.
    pub spawned: bool,
    /// What its status line said of its branch so far.
    pub branch: Option<&'a AgentBranch>,
    /// The Claude Code conversation it was started under, when the server chose it.
    pub conversation: Option<String>,
    /// Why the server started it on its worker, when it did.
    pub placed: Option<Placed>,
}

/// A refused change: its [`Outcome::Error`].
type Refused = Outcome;

/// What a change answers: its result and the changes it made, in order.
pub(crate) type Changed<T> = Result<(T, Vec<Change>), Refused>;

/// Every project, the person's policy, and what Claude Code reported of sessions no task has
/// taken on yet.
#[derive(Debug, Default)]
pub(crate) struct Projects {
    records: BTreeMap<ProjectId, Record>,
    policy: Policy,
    /// Natives of sessions no node holds, the oldest first: a spawned agent's first hooks can
    /// come before its task takes it on.
    unclaimed: VecDeque<(TermRef, Natives)>,
}

fn refuse(code: ErrorCode, message: impl Into<String>) -> Refused {
    Outcome::Error { code, message: message.into() }
}

fn invalid(message: impl Into<String>) -> Refused {
    refuse(ErrorCode::Invalid, message)
}

fn unknown_project(id: &ProjectId) -> Refused {
    refuse(ErrorCode::UnknownProject, format!("no project {id}; project_list names them"))
}

fn unknown_task(project: &ProjectId, id: TaskId) -> Refused {
    refuse(ErrorCode::UnknownTask, format!("no task {id} in project {project}"))
}

impl Record {
    const fn new(project: Project) -> Self {
        Self {
            project,
            tasks: Vec::new(),
            natives: Vec::new(),
            timeline: VecDeque::new(),
            next_seq: 1,
            timeline_bytes: 0,
        }
    }

    fn log(&mut self, task: Option<TaskId>, what: Moment, now: WallMs) -> TimelineEntry {
        let entry = TimelineEntry { seq: self.next_seq, at_ms: now, task, what };
        self.next_seq = self.next_seq.saturating_add(1);
        self.push_entry(entry.clone());
        entry
    }

    fn push_entry(&mut self, entry: TimelineEntry) {
        self.timeline_bytes = self.timeline_bytes.saturating_add(entry.approx_bytes());
        self.timeline.push_back(entry);
        self.trim_timeline();
    }

    /// Drop the oldest entries past the project's `timeline_kept` or [`TIMELINE_BYTES_KEPT`].
    fn trim_timeline(&mut self) {
        let kept = usize::try_from(self.project.limits.timeline_kept).unwrap_or(usize::MAX);
        while self.timeline.len() > kept || self.timeline_bytes > TIMELINE_BYTES_KEPT {
            let Some(gone) = self.timeline.pop_front() else { break };
            self.timeline_bytes = self.timeline_bytes.saturating_sub(gone.approx_bytes());
        }
    }

    /// Count [`Self::timeline_bytes`] again, for a record read from the file.
    fn recount(&mut self) {
        let bytes = self.timeline.iter().map(TimelineEntry::approx_bytes);
        self.timeline_bytes = bytes.fold(0, usize::saturating_add);
        self.trim_timeline();
    }

    fn task(&self, id: TaskId) -> Result<&Task, Refused> {
        let at = self.tasks.binary_search_by_key(&id, |t| t.id).ok();
        at.and_then(|i| self.tasks.get(i)).ok_or_else(|| unknown_task(&self.project.id, id))
    }

    fn task_mut(&mut self, id: TaskId) -> Result<&mut Task, Refused> {
        let at = self.tasks.binary_search_by_key(&id, |t| t.id).ok();
        at.and_then(|i| self.tasks.get_mut(i)).ok_or_else(|| unknown_task(&self.project.id, id))
    }

    fn kept(&self) -> Kept {
        let project = self.project.id.clone();
        Kept { project, record: None, task: None, native: None, entry: None }
    }

    /// The change of `task`, with the entry it made if any.
    fn task_update(&self, task: &Task, entry: Option<TimelineEntry>) -> Change {
        Change { kept: Kept { task: Some(task.clone()), entry, ..self.kept() }, durable: true }
    }

    /// The change of the project record, with the entry it made if any.
    fn record_update(&self, entry: Option<TimelineEntry>) -> Change {
        let kept = Kept { record: Some(self.project.clone()), entry, ..self.kept() };
        Change { kept, durable: true }
    }

    /// The change of a native leaf of `task`'s node.
    fn native_update(&self, task: Option<TaskId>, native: Native) -> Change {
        let kept = Kept { native: Some(NativeChange { task, native }), ..self.kept() };
        Change { kept, durable: true }
    }

    /// The natives of `task`'s node, none when it has none.
    fn natives_of(&self, task: Option<TaskId>) -> &Natives {
        static NONE: Natives = Natives { agents: Vec::new(), tasks: Vec::new() };
        self.natives.iter().find(|n| n.task == task).map_or(&NONE, |n| &n.natives)
    }

    fn natives_mut(&mut self, task: Option<TaskId>) -> Option<&mut Natives> {
        if !self.natives.iter().any(|n| n.task == task) {
            self.natives.push(NodeNatives { task, natives: Natives::default() });
        }
        self.natives.iter_mut().find(|n| n.task == task).map(|n| &mut n.natives)
    }

    fn status(&self, since: Option<u64>, bounds: Bounds, live: Live) -> ProjectStatus {
        let (timeline, more) = match since {
            Some(since) => page(self.timeline.iter().filter(|e| e.seq >= since)),
            None => (recent(&self.timeline), false),
        };
        let next = match timeline.last() {
            Some(last) if more => last.seq.saturating_add(1),
            _ => self.next_seq,
        };
        ProjectStatus {
            project: self.project.clone(),
            tasks: self.tasks.iter().map(|t| t.card(self.natives_of(Some(t.id)))).collect(),
            orchestrator_natives: self.natives_of(None).counts(),
            timeline,
            next,
            bounds,
            live,
        }
    }

    /// The task another of the project's live writing tasks owns a path of `paths` in, the
    /// path it owns, and the one of `paths` it overlaps.
    fn conflict(&self, task: Option<TaskId>, paths: &[String]) -> Option<(TaskId, &str, String)> {
        if paths.is_empty() {
            return None;
        }
        // Each path is folded once, not once per pair.
        let ours: Vec<(String, &String)> = paths.iter().map(|p| (folded(p), p)).collect();
        self.tasks
            .iter()
            .filter(|t| Some(t.id) != task && t.state.holds_paths() && !t.read_only)
            .find_map(|t| {
                t.owns.iter().find_map(|theirs| {
                    let folded_theirs = folded(theirs);
                    ours.iter()
                        .find(|(folded_ours, _)| holds_either(&folded_theirs, folded_ours))
                        .map(|(_, ours)| (t.id, theirs.as_str(), (*ours).clone()))
                })
            })
    }

    /// The terminals of the project that are live: its tasks' and its orchestrator's.
    fn live_terms<'a>(&'a self, terminals: &'a HashSet<TermRef>) -> impl Iterator<Item = TermRef> {
        let tasks = self.tasks.iter().filter_map(open_term);
        tasks.chain(self.project.orchestrator).filter(|t| terminals.contains(t))
    }

    /// The terminals the project takes up: its live ones and its starts.
    fn occupied(&self, running: &Running<'_>) -> HashSet<TermRef> {
        let id = &self.project.id;
        let mut terms: HashSet<TermRef> = self.live_terms(running.terminals).collect();
        let starts =
            running.starting.iter().filter(|s| s.task.as_ref().is_some_and(|(p, _)| p == id));
        terms.extend(starts.map(|s| s.term));
        terms
    }

    fn live(&self, running: &Running<'_>) -> u16 {
        count(self.occupied(running).len())
    }

    /// Whether a task's live terminal is `term`.
    fn live_assignment<'t>(t: &'t Task, terminals: &HashSet<TermRef>) -> Option<&'t Assignment> {
        t.assignment.as_ref().filter(|a| a.open() && terminals.contains(&a.term))
    }

    /// Whether `task` depends, directly or not, on `on`.
    fn depends(&self, task: TaskId, on: TaskId) -> bool {
        let mut seen = HashSet::new();
        let mut next = vec![task];
        while let Some(t) = next.pop() {
            if t == on {
                return true;
            }
            if seen.insert(t)
                && let Ok(t) = self.task(t)
            {
                next.extend(t.depends_on.iter().copied());
            }
        }
        false
    }

    /// Whether `task` is `root` or split from it, however deep.
    fn under(&self, root: TaskId, task: TaskId) -> bool {
        let mut at = Some(task);
        while let Some(t) = at {
            if t == root {
                return true;
            }
            at = self.task(t).ok().and_then(|t| t.parent);
        }
        false
    }

    /// How deep a new task under `parent` is: 1 for a task with no parent.
    fn depth_under(&self, parent: Option<TaskId>) -> usize {
        let mut depth = 1_usize;
        let mut at = parent;
        while let Some(p) = at {
            depth = depth.saturating_add(1);
            at = self.task(p).ok().and_then(|t| t.parent);
        }
        depth
    }
}

/// The terminal a task's open assignment names.
fn open_term(t: &Task) -> Option<TermRef> {
    t.assignment.as_ref().filter(|a| a.open()).map(|a| a.term)
}

fn count(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// The latest [`RECENT_ENTRIES`] of a timeline, within [`TIMELINE_PAGE_BYTES`].
fn recent(timeline: &VecDeque<TimelineEntry>) -> Vec<TimelineEntry> {
    let mut bytes = 0_usize;
    let mut kept: Vec<TimelineEntry> = timeline
        .iter()
        .rev()
        .take(RECENT_ENTRIES)
        .take_while(|e| {
            bytes = bytes.saturating_add(e.approx_bytes());
            bytes <= TIMELINE_PAGE_BYTES
        })
        .cloned()
        .collect();
    kept.reverse();
    kept
}

/// The first entries of `from`, at most [`TIMELINE_PAGE`] and [`TIMELINE_PAGE_BYTES`] of
/// them (and always one), and whether any were left.
fn page<'a>(from: impl Iterator<Item = &'a TimelineEntry>) -> (Vec<TimelineEntry>, bool) {
    let mut out = Vec::new();
    let mut bytes = 0_usize;
    for entry in from {
        bytes = bytes.saturating_add(entry.approx_bytes());
        if out.len() == TIMELINE_PAGE || (bytes > TIMELINE_PAGE_BYTES && !out.is_empty()) {
            return (out, true);
        }
        out.push(entry.clone());
    }
    (out, false)
}

/// A path as a task owns it.
///
/// Relative to the repository's root, in `/`-separated components with no `.` or trailing
/// `/`, in Unicode's composed form (NFC); the root itself is the empty path, which owns
/// everything. A glob owns what its part before the first wildcard does, so it never owns
/// less than it matches.
///
/// # Errors
/// An absolute path, or one that climbs out with `..`.
pub fn owned(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.starts_with('/') || path.starts_with('~') {
        return Err(format!("{path:?} is not relative to the repository's root"));
    }
    let literal = path.find(['*', '?', '[', '{']).map_or(path, |wild| {
        let before = path.get(..wild).unwrap_or_default();
        before.rfind('/').map_or("", |slash| before.get(..slash).unwrap_or_default())
    });
    let mut parts = Vec::new();
    for part in literal.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." {
            return Err(format!("{path:?} climbs out of the repository"));
        }
        parts.push(part);
    }
    let joined = parts.join("/");
    Ok(icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(&joined).into_owned())
}

/// Whether two owned paths ([`owned`]) take in any file both: one is the other or holds it,
/// case aside, as a case-insensitive volume (APFS's default) sees them.
#[must_use]
pub fn overlap(a: &str, b: &str) -> bool {
    holds_either(&folded(a), &folded(b))
}

/// Whether one of two folded owned paths is the other or holds it.
fn holds_either(a: &str, b: &str) -> bool {
    let holds = |outer: &str, inner: &str| {
        outer.is_empty()
            || inner == outer
            || inner.strip_prefix(outer).is_some_and(|rest| rest.starts_with('/'))
    };
    holds(a, b) || holds(b, a)
}

/// The state an agent's status puts the task it works on in; `None` for no agent.
const fn follows(status: &AgentStatus) -> Option<TaskState> {
    match status {
        AgentStatus::None => None,
        AgentStatus::Working | AgentStatus::Tool { .. } => Some(TaskState::Running),
        AgentStatus::Blocked(BlockReason::IdlePrompt)
        | AgentStatus::Idle
        | AgentStatus::Done
        | AgentStatus::Waiting { .. } => Some(TaskState::Waiting),
        AgentStatus::Blocked(_) => Some(TaskState::Blocked),
    }
}

/// `limits` changed by `change`, within `bounds`.
fn limited(limits: Limits, change: LimitsChange, bounds: Bounds) -> Result<Limits, Refused> {
    let within = |name: &str, value: Option<u32>, least: u32, most: u32| match value {
        Some(v) if v < least => Err(invalid(format!("{name} is at least {least}"))),
        Some(v) if v > most => Err(refuse(
            ErrorCode::Limit,
            format!(
                "{name} {v} is over the {most} the person allows (`[server.projects] {name}` in \
                 the server's settings.toml)"
            ),
        )),
        _ => Ok(()),
    };
    within(
        "live_per_worker",
        change.live_per_worker.map(u32::from),
        1,
        bounds.live_per_worker.into(),
    )?;
    within(
        "live_per_project",
        change.live_per_project.map(u32::from),
        1,
        bounds.live_per_project.into(),
    )?;
    within("depth", change.depth.map(u32::from), 1, bounds.depth.into())?;
    within("timeline_kept", change.timeline_kept, TIMELINE_LEAST, bounds.timeline_kept)?;
    let clamp = |v: u16, most: u16| v.min(most);
    Ok(Limits {
        live_per_worker: change
            .live_per_worker
            .unwrap_or_else(|| clamp(limits.live_per_worker, bounds.live_per_worker)),
        live_per_project: change
            .live_per_project
            .unwrap_or_else(|| clamp(limits.live_per_project, bounds.live_per_project)),
        depth: change.depth.unwrap_or_else(|| clamp(limits.depth, bounds.depth)),
        timeline_kept: change
            .timeline_kept
            .unwrap_or_else(|| limits.timeline_kept.min(bounds.timeline_kept)),
        budget: match change.budget {
            Some(budget) if budget.0.is_empty() => None,
            Some(budget) if !budget.fits() => {
                return Err(invalid(format!(
                    "a budget names at most {} meters, each a name with no space and a cap \
                     above nothing; a plan window's at most 10000 (the whole window)",
                    Budget::METERS_MAX
                )));
            }
            Some(budget) => Some(budget),
            None => limits.budget,
        },
    })
}

/// A metadata document as it is kept: a JSON object, compact; `None` for an empty one.
fn metadata(text: Option<String>) -> Result<Option<String>, Refused> {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else { return Ok(None) };
    if text.len() > METADATA_MAX {
        return Err(invalid(format!("metadata is {} bytes, over {METADATA_MAX}", text.len())));
    }
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(doc @ serde_json::Value::Object(_)) => {
            Ok(Some(serde_json::to_string(&doc).map_err(|e| invalid(e.to_string()))?))
        }
        Ok(_) => Err(refuse(ErrorCode::BadExpression, "metadata is a JSON object")),
        Err(e) => Err(refuse(ErrorCode::BadExpression, format!("metadata is not JSON: {e}"))),
    }
}

fn status_text(text: &str) -> Result<Option<String>, Refused> {
    let text = text.trim();
    if text.len() > STATUS_MAX {
        return Err(invalid(format!("a status is at most {STATUS_MAX} bytes")));
    }
    Ok(Some(text.to_owned()).filter(|t| !t.is_empty()))
}

fn words(text: Option<String>) -> Option<String> {
    text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// `text` when it is at most `max` bytes; refused naming `name` otherwise.
fn within(name: &str, text: Option<&str>, max: usize) -> Result<(), Refused> {
    match text {
        Some(t) if t.len() > max => Err(invalid(format!("{name} is at most {max} bytes"))),
        _ => Ok(()),
    }
}

/// A verifier command as it is kept, within [`SUMMARY_MAX`].
/// A project's members, each value trimmed: at most [`Project::MEMBERS_MAX`], each within
/// [`Project::member_fits`], none named twice.
fn members(members: Vec<Matcher>) -> Result<Vec<Matcher>, Refused> {
    if members.len() > Project::MEMBERS_MAX {
        return Err(invalid(format!("a project names at most {} members", Project::MEMBERS_MAX)));
    }
    let mut kept: Vec<Matcher> = Vec::with_capacity(members.len());
    for member in members {
        let member: Matcher =
            member.into_iter().map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect();
        if !Project::member_fits(&member) {
            return Err(invalid(format!(
                "a member names 1 to {} facts, each a key with no space and a value of at most                  {} bytes",
                Project::MATCHER_KEYS_MAX,
                Project::MATCHER_VALUE_MAX
            )));
        }
        if kept.contains(&member) {
            return Err(invalid("a member is named twice"));
        }
        kept.push(member);
    }
    Ok(kept)
}

fn verifier(text: Option<String>) -> Result<Option<String>, Refused> {
    within("a verifier", text.as_deref(), SUMMARY_MAX)?;
    Ok(words(text))
}

/// A reviewer's brief, trimmed: `None` for empty, which asks for no reviewer.
fn review_brief(text: Option<String>) -> Result<Option<String>, Refused> {
    within("a reviewer's brief", text.as_deref(), SUMMARY_MAX)?;
    Ok(words(text))
}

/// A refusal for passing one of the person's bounds.
fn over_bound(name: &str, have: usize, most: u64) -> Refused {
    refuse(
        ErrorCode::Limit,
        format!(
            "{name}: {have} would pass the {most} the person allows (`[server.projects] {name}` in \
             the server's settings.toml)"
        ),
    )
}

/// A title as it is kept: trimmed, not empty, within the person's `title_max`.
fn titled(title: &str, bounds: Bounds) -> Result<String, Refused> {
    let title = title.trim();
    if title.is_empty() {
        return Err(invalid("a title is not empty"));
    }
    if title.len() > usize::try_from(bounds.title_max).unwrap_or(usize::MAX) {
        return Err(over_bound("title_max", title.len(), bounds.title_max.into()));
    }
    Ok(title.to_owned())
}

/// A commit named in hex, as git prints one: 7 to 64 hex digits.
fn is_commit(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every field of a task's change within what a card and the timeline keep.
fn checked_change(change: &TaskChange) -> Result<(), Refused> {
    within("a branch", change.branch.as_deref(), REF_MAX)?;
    within("a note", change.note.as_deref(), SUMMARY_MAX)?;
    within("a verifier", change.verifier.as_deref(), SUMMARY_MAX)?;
    if change.depends_on.as_ref().is_some_and(|d| d.len() > DEPENDS_MAX) {
        return Err(invalid(format!("a task depends on at most {DEPENDS_MAX} tasks")));
    }
    if let Some(base) = &change.base
        && !is_commit(base)
    {
        return Err(invalid(format!("base {base:?} is not a commit in hex")));
    }
    if let Some(VerifierRun { summary, head, base, .. }) = &change.verified {
        within("a verifier's summary", Some(summary), SUMMARY_MAX)?;
        for (name, commit) in [("head", head), ("base", base)] {
            if !is_commit(commit) {
                return Err(invalid(format!(
                    "the verifier's {name} {commit:?} is not a commit in hex"
                )));
            }
        }
    }
    Ok(())
}

impl Projects {
    /// The projects a store kept.
    pub(crate) fn restore(file: ProjectsFile) -> Self {
        let records = file
            .projects
            .into_iter()
            .map(|mut r| {
                r.recount();
                // No step goes on across a restart: one under way when the server stopped
                // says it ended.
                for step in r.tasks.iter_mut().filter_map(|t| t.step.as_mut()) {
                    if matches!(step.state, StepState::Running { .. }) {
                        let why = "the server stopped while it ran".to_owned();
                        step.state = StepState::Failed { why };
                    }
                }
                // Nor a stretch of work: how long the server was away is not known to be
                // work, and the agent's status after its worker registers starts the next.
                r.project.orchestrator_spent.since_ms = None;
                for t in &mut r.tasks {
                    t.spent.since_ms = None;
                }
                (r.project.id.clone(), r)
            })
            .collect();
        Self { records, ..Self::default() }
    }

    /// Every project, as the store keeps it after `through` changes, beside `watched`.
    pub(crate) fn file(&self, watched: Vec<Watched>, through: u64) -> ProjectsFile {
        ProjectsFile { projects: self.records.values().cloned().collect(), watched, through }
    }

    /// The person's policy.
    pub(crate) const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Take up the person's policy: every project's limits come within it.
    pub(crate) fn set_policy(&mut self, policy: Policy) -> Vec<Change> {
        self.policy = policy;
        let bounds = self.policy.bounds;
        let mut updates = Vec::new();
        for record in self.records.values_mut() {
            let within = limited(record.project.limits.clone(), LimitsChange::default(), bounds);
            if let Some(limits) = within.ok().filter(|l| *l != record.project.limits) {
                record.project.limits = limits;
                record.trim_timeline();
                updates.push(record.record_update(None));
            }
        }
        updates
    }

    /// Every project, by name.
    pub(crate) fn list(&self) -> Vec<Project> {
        self.records.values().map(|r| r.project.clone()).collect()
    }

    /// Every project whole with its latest entries, for a client that connects.
    pub(crate) fn snapshot(&self, running: &Running<'_>) -> Vec<ProjectStatus> {
        let fleet = self.fleet(running);
        self.records
            .values()
            .map(|r| {
                let live = Live { fleet, project: r.live(running) };
                r.status(None, self.policy.bounds_for(Some(&r.project.id)), live)
            })
            .collect()
    }

    /// A project whole, its timeline from `since`.
    pub(crate) fn status(
        &self,
        id: &ProjectId,
        since: Option<u64>,
        running: &Running<'_>,
    ) -> Result<ProjectStatus, Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let live = Live { fleet: self.fleet(running), project: record.live(running) };
        Ok(record.status(since, self.policy.bounds_for(Some(id)), live))
    }

    /// Every live agent across the fleet, and every start not yet counted on its own: the
    /// terminals an agent runs in, and every task's or orchestrator's terminal whatever runs in
    /// it.
    pub(crate) fn fleet(&self, running: &Running<'_>) -> u16 {
        count(self.fleet_terms(running).len())
    }

    fn fleet_terms(&self, running: &Running<'_>) -> HashSet<TermRef> {
        let mut terms: HashSet<TermRef> = running.agents.clone();
        for record in self.records.values() {
            terms.extend(record.live_terms(running.terminals));
        }
        terms.extend(running.starting.iter().map(|s| s.term));
        terms
    }

    /// Every live agent and start on `worker`, in a project or not: the person's
    /// `live_per_worker` bounds them all.
    pub(crate) fn live_on_worker(&self, worker: WorkerId, running: &Running<'_>) -> u16 {
        count(self.fleet_terms(running).iter().filter(|t| t.worker == worker).count())
    }

    /// How many of a project's agents run on `worker`, and are being started there.
    pub(crate) fn live_on(&self, id: &ProjectId, worker: WorkerId, running: &Running<'_>) -> u16 {
        let Some(record) = self.records.get(id) else { return 0 };
        count(record.occupied(running).iter().filter(|t| t.worker == worker).count())
    }

    /// One node in full: a task with its natives, or the orchestrator's.
    pub(crate) fn node(&self, id: &ProjectId, task: Option<TaskId>) -> Result<NodeDetail, Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let full = task.map(|t| record.task(t).cloned()).transpose()?;
        Ok(NodeDetail { task: full, natives: record.natives_of(task).clone() })
    }

    /// A change as a client is pushed it: the task as its card.
    pub(crate) fn pushed(&self, kept: &Kept) -> ProjectUpdate {
        let natives = |task: TaskId| {
            self.records
                .get(&kept.project)
                .map(|r| r.natives_of(Some(task)).clone())
                .unwrap_or_default()
        };
        ProjectUpdate {
            project: kept.project.clone(),
            record: kept.record.clone(),
            task: kept.task.as_ref().map(|t| t.card(&natives(t.id))),
            native: kept.native.clone(),
            entry: kept.entry.clone(),
        }
    }

    /// Where each of a project's tasks runs now, for `near` and `avoid`.
    pub(crate) fn peers(
        &self,
        id: &ProjectId,
        running: &Running<'_>,
    ) -> BTreeMap<TaskId, WorkerId> {
        let Some(record) = self.records.get(id) else { return BTreeMap::new() };
        record
            .tasks
            .iter()
            .filter_map(|t| {
                Record::live_assignment(t, running.terminals).map(|a| (t.id, a.term.worker))
            })
            .collect()
    }

    fn record(&mut self, id: &ProjectId) -> Result<&mut Record, Refused> {
        self.records.get_mut(id).ok_or_else(|| unknown_project(id))
    }

    /// A project's record.
    /// Let `id` go with its tasks, its queue and its timeline. Its terminals are not the
    /// store's: they run on, as terminals.
    pub(crate) fn delete(&mut self, id: &ProjectId) -> Result<(), Refused> {
        self.records.remove(id).map(|_| ()).ok_or_else(|| unknown_project(id))
    }

    pub(crate) fn project(&self, id: &ProjectId) -> Result<&Project, Refused> {
        self.records.get(id).map(|r| &r.project).ok_or_else(|| unknown_project(id))
    }

    /// Words for the timeline, from the server itself, on a task or the project.
    pub(crate) fn note(
        &mut self,
        id: &ProjectId,
        task: Option<TaskId>,
        text: &str,
        now: WallMs,
    ) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let entry = record.log(task, Moment::Note { text: clipped(text, SUMMARY_MAX) }, now);
        match task.map(|t| record.task(t).cloned()) {
            Some(Ok(task)) => vec![record.task_update(&task, Some(entry))],
            Some(Err(_)) => Vec::new(),
            None => vec![record.record_update(Some(entry))],
        }
    }

    /// A project's limits.
    pub(crate) fn limits(&self, id: &ProjectId) -> Result<Limits, Refused> {
        self.records.get(id).map(|r| r.project.limits.clone()).ok_or_else(|| unknown_project(id))
    }

    /// A task as it is.
    pub(crate) fn task(&self, id: &ProjectId, task: TaskId) -> Result<&Task, Refused> {
        self.records.get(id).ok_or_else(|| unknown_project(id))?.task(task)
    }

    /// Whether `task` of project `id` is `root` or split from it, however deep.
    pub(crate) fn under(&self, id: &ProjectId, root: TaskId, task: TaskId) -> bool {
        self.records.get(id).is_some_and(|r| r.under(root, task))
    }

    /// Make a project.
    pub(crate) fn create(
        &mut self,
        new: NewProject,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        if self.records.contains_key(&new.id) {
            return Err(refuse(ErrorCode::Conflict, format!("project {} exists already", new.id)));
        }
        let bounds = self.policy.bounds_for(Some(&new.id));
        if self.records.len() >= usize::from(bounds.projects) {
            return Err(over_bound("projects", self.records.len(), bounds.projects.into()));
        }
        let title = titled(&new.title, bounds)?;
        let limits = limited(Limits::default(), new.limits, bounds)?;
        within("a repository", Some(&new.repo), REF_MAX)?;
        within("a target branch", Some(&new.target), REF_MAX)?;
        let project = Project {
            needs: Vec::new(),
            orchestrator_spent: Spent::default(),
            spend: slopty_proto::project::Spend::default(),
            id: new.id.clone(),
            title,
            members: members(new.members)?,
            repo: new.repo.trim().to_owned(),
            repo_id: None,
            target: new.target.trim().to_owned(),
            review: review_brief(new.review)?,
            verifier: verifier(new.verifier)?,
            push: new.push,
            ask_to_start: new.ask_to_start,
            orchestrator: new.orchestrator,
            limits,
            metadata: metadata(new.metadata)?,
            created_ms: now,
        };
        let mut record = Record::new(project);
        let created = record.log(None, Moment::Created, now);
        let mut updates = vec![record.record_update(Some(created))];
        if let Some(term) = new.orchestrator {
            let named = record.log(None, Moment::Orchestrator { term }, now);
            updates.push(record.record_update(Some(named)));
        }
        self.records.insert(new.id.clone(), record);
        let status = self.status(&new.id, Some(0), running)?;
        Ok((status, updates))
    }

    /// Change a project's members, orchestrator, verifier, limits or metadata.
    pub(crate) fn set(
        &mut self,
        id: &ProjectId,
        change: ProjectChange,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        let bounds = self.policy.bounds_for(Some(id));
        let record = self.record(id)?;
        let limits = limited(record.project.limits.clone(), change.limits, bounds)?;
        let metadata = change.metadata.map(|m| metadata(Some(m))).transpose()?;
        let new_verifier = change.verifier.map(|v| verifier(Some(v))).transpose()?;
        let new_review = change.review.map(|r| review_brief(Some(r))).transpose()?;
        let new_members = change.members.map(members).transpose()?;
        let mut updates = Vec::new();
        let mut quiet = false;
        if let Some(members) = new_members {
            quiet |= record.project.members != members;
            record.project.members = members;
        }
        if let Some(verifier) = new_verifier {
            quiet |= record.project.verifier != verifier;
            record.project.verifier = verifier;
        }
        if let Some(review) = new_review {
            quiet |= record.project.review != review;
            record.project.review = review;
        }
        if let Some(metadata) = metadata {
            quiet |= record.project.metadata != metadata;
            record.project.metadata = metadata;
        }
        if let Some(push) = change.push {
            quiet |= record.project.push != push;
            record.project.push = push;
        }
        if let Some(ask) = change.ask_to_start {
            quiet |= record.project.ask_to_start != ask;
            record.project.ask_to_start = ask;
        }
        if limits != record.project.limits {
            record.project.limits = limits.clone();
            record.trim_timeline();
            let entry = record.log(None, Moment::Limits { limits }, now);
            updates.push(record.record_update(Some(entry)));
            quiet = false;
        }
        if let Some(term) = change.orchestrator.filter(|t| record.project.orchestrator != Some(*t))
        {
            record.project.orchestrator = Some(term);
            let entry = record.log(None, Moment::Orchestrator { term }, now);
            updates.push(record.record_update(Some(entry)));
        } else if quiet {
            updates.push(record.record_update(None));
        }
        let status = self.status(id, None, running)?;
        Ok((status, updates))
    }

    /// Say what each kind of `id`'s work needs of its machines, in place of what was said; the
    /// rules were compiled by the caller. A change is on the timeline, since it moves where
    /// work goes from then on.
    pub(crate) fn set_needs(
        &mut self,
        id: &ProjectId,
        needs: Vec<Need>,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        if needs.len() > Need::MAX {
            return Err(invalid(format!("a project names at most {} needs", Need::MAX)));
        }
        let mut names = BTreeSet::new();
        for need in &needs {
            let name = need.name.trim();
            if name.is_empty() || name.len() > Need::NAME_MAX {
                return Err(invalid(format!(
                    "a need's name is 1 to {} bytes: what it is, in a few words",
                    Need::NAME_MAX
                )));
            }
            if !names.insert(name.to_owned()) {
                return Err(invalid(format!("the need {name:?} is named twice")));
            }
            let items = [need.paths.len(), need.require.len(), need.prefer.len()];
            if items.into_iter().any(|n| n > Need::ITEMS_MAX) {
                return Err(invalid(format!(
                    "a need names at most {} paths, rules and preferences each",
                    Need::ITEMS_MAX
                )));
            }
            if need.require.is_empty() && need.prefer.is_empty() {
                return Err(invalid(format!("the need {name:?} requires and prefers nothing")));
            }
            for path in &need.paths {
                within("a need's path", Some(path), REF_MAX)?;
            }
        }
        let needs: Vec<Need> = needs
            .into_iter()
            .map(|need| Need { name: need.name.trim().to_owned(), ..need })
            .collect();
        let record = self.record(id)?;
        let mut updates = Vec::new();
        if record.project.needs != needs {
            let names = needs.iter().map(|n| n.name.clone()).collect();
            record.project.needs = needs;
            let entry = record.log(None, Moment::Needs { names }, now);
            updates.push(record.record_update(Some(entry)));
        }
        let status = self.status(id, None, running)?;
        Ok((status, updates))
    }

    /// The needs of `id` that `task` has, by what it owns.
    pub(crate) fn needs_of(&self, id: &ProjectId, task: TaskId) -> Result<Vec<Need>, Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let owns = &record.task(task)?.owns;
        Ok(record.project.needs.iter().filter(|n| n.applies(owns)).cloned().collect())
    }

    /// Make a task, owning `spec.owns`.
    pub(crate) fn create_task(
        &mut self,
        id: &ProjectId,
        spec: TaskSpec,
        now: WallMs,
    ) -> Changed<Task> {
        let bounds = self.policy.bounds_for(Some(id));
        let record = self.record(id)?;
        if record.tasks.len() >= usize::try_from(bounds.tasks_per_project).unwrap_or(usize::MAX) {
            return Err(over_bound(
                "tasks_per_project",
                record.tasks.len(),
                bounds.tasks_per_project.into(),
            ));
        }
        let title = titled(&spec.title, bounds)?;
        if spec.brief.len() > usize::try_from(bounds.brief_max).unwrap_or(usize::MAX) {
            return Err(over_bound("brief_max", spec.brief.len(), bounds.brief_max.into()));
        }
        if spec.depends_on.len() > DEPENDS_MAX {
            return Err(invalid(format!("a task depends on at most {DEPENDS_MAX} tasks")));
        }
        let kind = spec.kind.trim().to_owned();
        if kind.len() > KIND_MAX {
            return Err(invalid(format!("a kind is at most {KIND_MAX} bytes")));
        }
        if let Some(parent) = spec.parent {
            record.task(parent)?;
        }
        let depth = record.depth_under(spec.parent);
        let most = record.project.limits.depth;
        if depth > usize::from(most) {
            return Err(refuse(
                ErrorCode::Limit,
                format!(
                    "the task would be {depth} deep, past the project's depth of {most}; raise it \
                     with project_update, within the person's bounds"
                ),
            ));
        }
        let mut depends_on = Vec::with_capacity(spec.depends_on.len());
        for on in spec.depends_on {
            record.task(on)?;
            if !depends_on.contains(&on) {
                depends_on.push(on);
            }
        }
        placement::check(&spec.placement, bounds.comprehension_depth)?;
        let owns = claimable(&spec.owns, bounds.owns_max)?;
        if spec.read_only && !owns.is_empty() {
            return Err(invalid("a read-only task owns no paths"));
        }
        if let Some((theirs, path, ours)) = record.conflict(None, &owns) {
            return Err(overlapping(theirs, path, &ours));
        }
        let number = u32::try_from(record.tasks.len()).unwrap_or(u32::MAX).saturating_add(1);
        let task = Task {
            spent: Spent::default(),
            checks: None,
            id: TaskId(number),
            parent: spec.parent,
            depends_on,
            kind,
            title,
            brief: spec.brief,
            owns,
            read_only: spec.read_only,
            placement: spec.placement,
            verifier: verifier(spec.verifier)?,
            metadata: metadata(spec.metadata)?,
            state: TaskState::Planned,
            status: None,
            assignment: None,
            branch: None,
            worktree: None,
            base: None,
            pr: None,
            reviewed: None,
            verified: None,
            merge: None,
            created_ms: now,
            updated_ms: now,
            step: None,
            proposal: None,
        };
        record.tasks.push(task.clone());
        let entry =
            record.log(Some(task.id), Moment::TaskCreated { title: task.title.clone() }, now);
        let update = record.task_update(&task, Some(entry));
        Ok((task, vec![update]))
    }

    /// Take `paths` for a task to own, beside what it owns.
    pub(crate) fn claim(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        paths: &[String],
        now: WallMs,
    ) -> Changed<Task> {
        let owns_max = self.policy.bounds_for(Some(id)).owns_max;
        let record = self.record(id)?;
        let wanted = claimable(paths, owns_max)?;
        let held = record.task(task)?;
        let adding = wanted.iter().filter(|w| !held.owns.iter().any(|o| same_path(o, w))).count();
        if held.owns.len().saturating_add(adding) > usize::from(owns_max) {
            return Err(over_bound(
                "owns_max",
                held.owns.len().saturating_add(adding),
                owns_max.into(),
            ));
        }
        if held.read_only {
            return Err(invalid(format!("task {task} only reads; it owns no paths")));
        }
        if !held.state.holds_paths() {
            return Err(invalid(format!(
                "task {task} is {:?} and holds nothing; plan it again or make a new task",
                held.state
            )));
        }
        if let Some((theirs, path, ours)) = record.conflict(Some(task), &wanted) {
            return Err(overlapping(theirs, path, &ours));
        }
        let t = record.task_mut(task)?;
        let mut taken = Vec::new();
        for path in wanted {
            if !t.owns.iter().any(|o| same_path(o, &path)) {
                t.owns.push(path.clone());
                taken.push(path);
            }
        }
        if taken.is_empty() {
            return Ok((t.clone(), Vec::new()));
        }
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = record.log(Some(task), Moment::Claimed { paths: taken }, now);
        Ok((task_now.clone(), vec![record.task_update(&task_now, Some(entry))]))
    }

    /// Change a task: move it, set its status, dependencies, placement, verifier or metadata,
    /// record its branch or its verifier's word, note something.
    pub(crate) fn update_task(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        change: TaskChange,
        caller: Caller,
        now: WallMs,
    ) -> Changed<Task> {
        if caller == Caller::Agent {
            if change.state == Some(TaskState::Merged) {
                return Err(refuse(
                    ErrorCode::Forbidden,
                    "only the person or the merge queue merges a task; say it is done",
                ));
            }
            if change.verified.is_some() {
                return Err(refuse(
                    ErrorCode::Forbidden,
                    "only the person or the merge queue records what the verifier said; note it \
                     instead",
                ));
            }
        }
        checked_change(&change)?;
        let comprehensions = self.policy.bounds_for(Some(id)).comprehension_depth;
        let record = self.record(id)?;
        let before = record.task(task)?.clone();
        if let Some(to) = change.state.filter(|to| *to != before.state) {
            if !before.state.may_become(to) {
                return Err(invalid(format!(
                    "task {task} cannot go from {:?} to {to:?}: a merged task is final, and only \
                     a done or verifying task merges",
                    before.state
                )));
            }
            if to.holds_paths()
                && !before.state.holds_paths()
                && let Some((theirs, path, ours)) = record.conflict(Some(task), &before.owns)
            {
                return Err(overlapping(theirs, path, &ours));
            }
        }
        if let Some(on) = &change.depends_on {
            for dep in on {
                record.task(*dep)?;
                if *dep == task || record.depends(*dep, task) {
                    return Err(invalid(format!(
                        "task {task} cannot depend on task {dep}: task {dep} already needs task \
                         {task}, and a dependency never leads back"
                    )));
                }
            }
        }
        if let Some(placement) = &change.placement {
            placement::check(placement, comprehensions)?;
        }
        let metadata = change.metadata.map(|m| metadata(Some(m))).transpose()?;
        let status = change.status.as_deref().map(status_text).transpose()?;
        let t = record.task_mut(task)?;
        let mut moments = Vec::new();
        let mut quiet = false;
        if let Some(to) = change.state.filter(|to| *to != t.state) {
            moments.push(Moment::State { from: t.state, to });
            t.state = to;
            // A task moved out of done leaves the queue: what was verified is not what it is
            // now.
            if to != TaskState::Merged && t.merge.as_ref().is_some_and(|m| m.queued().is_some()) {
                t.merge = None;
            }
        }
        if let Some(status) = status.filter(|s| *s != t.status) {
            t.status = status;
            quiet = true;
        }
        if let Some(on) = change.depends_on {
            let mut deduped: Vec<TaskId> = Vec::with_capacity(on.len());
            for dep in on {
                if !deduped.contains(&dep) {
                    deduped.push(dep);
                }
            }
            quiet |= t.depends_on != deduped;
            t.depends_on = deduped;
        }
        if let Some(placement) = change.placement {
            quiet |= t.placement != placement;
            t.placement = placement;
        }
        if let Some(run_on) = change.run_on {
            let pin = match run_on {
                RunOn::Worker(worker) => Some(worker),
                RunOn::Anywhere => None,
            };
            quiet |= t.placement.pin != pin;
            t.placement.pin = pin;
        }
        if let Some(verifier) = change.verifier {
            let verifier = words(Some(verifier));
            quiet |= t.verifier != verifier;
            t.verifier = verifier;
        }
        if let Some(metadata) = metadata {
            quiet |= t.metadata != metadata;
            t.metadata = metadata;
        }
        if let Some(branch) = change.branch.filter(|b| t.branch.as_ref() != Some(b)) {
            t.branch = Some(branch);
            moments.push(Moment::Branch {
                branch: t.branch.clone(),
                pr: t.pr.as_ref().map(|p| p.number),
            });
        }
        if let Some(base) = change.base.filter(|b| t.base.as_ref() != Some(b)) {
            t.base = Some(base);
            quiet = true;
        }
        if let Some(run) = change.verified {
            moments.push(Moment::Verified(run.clone()));
            t.verified = Some(run);
        }
        if let Some(text) = words(change.note) {
            moments.push(Moment::Note { text });
        }
        if moments.is_empty() && !quiet {
            return Ok((t.clone(), Vec::new()));
        }
        t.updated_ms = now;
        let task_now = t.clone();
        let mut updates: Vec<Change> = moments
            .into_iter()
            .map(|what| {
                let entry = record.log(Some(task), what, now);
                record.task_update(&task_now, Some(entry))
            })
            .collect();
        if updates.is_empty() {
            updates.push(record.task_update(&task_now, None));
        }
        Ok((task_now, updates))
    }

    /// A task's agent reports on its work: kept on the timeline, and answered with the task and
    /// the node it is for (its parent task, or the orchestrator's when absent).
    pub(crate) fn report_task(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        report: &Report,
        now: WallMs,
    ) -> Changed<(Task, Option<TaskId>)> {
        within("a report's note", Some(&report.note), NOTE_MAX)?;
        within("a report's branch", report.branch.as_deref(), REF_MAX)?;
        if report.artifacts.len() > ARTIFACTS_MAX {
            return Err(invalid(format!("a report names at most {ARTIFACTS_MAX} artifacts")));
        }
        for artifact in &report.artifacts {
            within("an artifact", Some(artifact), REF_MAX)?;
        }
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        let mut moments = Vec::new();
        if let Some(branch) = report.branch.clone().filter(|b| t.branch.as_ref() != Some(b)) {
            t.branch = Some(branch);
            moments.push(Moment::Branch {
                branch: t.branch.clone(),
                pr: t.pr.as_ref().map(|p| p.number),
            });
        }
        moments.push(Moment::Reported { report: report.clone() });
        t.updated_ms = now;
        let (task_now, parent) = (t.clone(), t.parent);
        let updates = moments
            .into_iter()
            .map(|what| {
                let entry = record.log(Some(task), what, now);
                record.task_update(&task_now, Some(entry))
            })
            .collect();
        Ok(((task_now, parent), updates))
    }

    /// Reports went to `term`, the agent of `node` (a task, or the orchestrator): a moment on
    /// the timeline.
    pub(crate) fn delivered(
        &mut self,
        id: &ProjectId,
        node: Option<TaskId>,
        term: TermRef,
        reports: u16,
        now: WallMs,
    ) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let entry = record.log(node, Moment::Delivered { term, reports }, now);
        match node.map(|t| record.task(t).cloned()) {
            Some(Ok(task)) => vec![record.task_update(&task, Some(entry))],
            Some(Err(_)) => Vec::new(),
            None => vec![record.record_update(Some(entry))],
        }
    }

    /// The live terminal of a node: a task's open assignment, or the orchestrator's.
    pub(crate) fn node_term(
        &self,
        id: &ProjectId,
        node: Option<TaskId>,
        terminals: &HashSet<TermRef>,
    ) -> Option<TermRef> {
        let record = self.records.get(id)?;
        let term = match node {
            Some(task) => open_term(record.task(task).ok()?),
            None => record.project.orchestrator,
        };
        term.filter(|t| terminals.contains(t))
    }

    /// What `term` works on: the project and its task, or none for the project's orchestrator.
    pub(crate) fn working_on(&self, term: TermRef) -> Option<(ProjectId, Option<TaskId>)> {
        if let Some((project, task)) = self.working_in(term) {
            return Some((project.clone(), Some(task)));
        }
        self.records
            .values()
            .find(|r| r.project.orchestrator == Some(term))
            .map(|r| (r.project.id.clone(), None))
    }

    /// Whether a task may be started now: it exists, is not merged, and nothing runs or is
    /// being started for it; and the project has room for one more.
    pub(crate) fn may_start(
        &self,
        id: &ProjectId,
        task: TaskId,
        ignore_dependencies: bool,
        running: &Running<'_>,
    ) -> Result<(), Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let t = record.task(task)?;
        if !ignore_dependencies
            && let Some(dep) = t.depends_on.iter().find_map(|d| {
                record
                    .task(*d)
                    .ok()
                    .filter(|d| !matches!(d.state, TaskState::Done | TaskState::Merged))
            })
        {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} depends on task {}, which is {:?}; start it when that is done, or \
                     say ignore_dependencies",
                    dep.id, dep.state
                ),
            ));
        }
        if !t.state.holds_paths()
            && let Some((theirs, path, ours)) = record.conflict(Some(task), &t.owns)
        {
            return Err(overlapping(theirs, path, &ours));
        }
        if let Some(live) = Record::live_assignment(t, running.terminals) {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} has a terminal already, {}/{}",
                    live.term.worker, live.term.session
                ),
            ));
        }
        if running.starting.iter().any(|s| s.task.as_ref() == Some(&(id.clone(), task))) {
            return Err(refuse(ErrorCode::Conflict, format!("task {task} is being started")));
        }
        if t.state == TaskState::Merged {
            return Err(invalid(format!("task {task} is merged; make a new task")));
        }
        let live = record.live(running);
        let most = record.project.limits.live_per_project;
        if live >= most {
            return Err(refuse(
                ErrorCode::Limit,
                format!(
                    "project {id} runs {live} agents, its live_per_project of {most}; wait for one \
                     to end, or raise it with project_update within the person's bounds"
                ),
            ));
        }
        Ok(())
    }

    /// Whether project `id` has room for the terminal `term` put on one of its tasks: one the
    /// project counts already takes no more, any other takes a place under its
    /// `live_per_project` and its `live_per_worker` on that worker, as a start would. So no
    /// terminal opened outside the counts is assigned past them.
    pub(crate) fn room_for(
        &self,
        id: &ProjectId,
        term: TermRef,
        running: &Running<'_>,
    ) -> Result<(), Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let occupied = record.occupied(running);
        if occupied.contains(&term) {
            return Ok(());
        }
        let limits = &record.project.limits;
        let live = count(occupied.len());
        let here = count(occupied.iter().filter(|t| t.worker == term.worker).count());
        if live >= limits.live_per_project || here >= limits.live_per_worker {
            return Err(refuse(
                ErrorCode::Limit,
                format!(
                    "project {id} runs {live} agents, {here} of them on that worker, at its \
                     live_per_project of {} or live_per_worker of {}; a terminal put on a task \
                     counts as a start does",
                    limits.live_per_project, limits.live_per_worker
                ),
            ));
        }
        Ok(())
    }

    /// Put the terminal `who` names on a task, taking up what its status line said of its
    /// branch and what Claude Code reported in it so far.
    pub(crate) fn assign(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        who: Assignee<'_>,
        terminals: &HashSet<TermRef>,
        now: WallMs,
    ) -> Changed<Task> {
        let Assignee { term, spawned, branch, conversation, placed } = who;
        if let Some((other, on)) = self.working_in(term).filter(|(p, t)| (*p, *t) != (id, task)) {
            return Err(refuse(
                ErrorCode::Conflict,
                format!("that terminal works on task {on} of project {other} already"),
            ));
        }
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        if open_term(t) == Some(term) {
            return Ok((t.clone(), Vec::new()));
        }
        if let Some(live) = Record::live_assignment(t, terminals) {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} has a terminal already, {}/{}; close it first",
                    live.term.worker, live.term.session
                ),
            ));
        }
        if t.state == TaskState::Merged {
            return Err(invalid(format!("task {task} is merged; make a new task")));
        }
        if !t.state.holds_paths() {
            let owns = t.owns.clone();
            if let Some((theirs, path, ours)) = record.conflict(Some(task), &owns) {
                return Err(overlapping(theirs, path, &ours));
            }
        }
        let Ok(t) = record.task_mut(task) else { return Err(unknown_task(id, task)) };
        let mut moments = Vec::new();
        if let Some(gone) = open_term(t) {
            moments.push(Moment::AgentGone { term: gone });
        }
        t.assignment =
            Some(Assignment { term, since_ms: now, ended_ms: None, conversation, placed });
        // Whatever starts it, a start proposed for it is spent.
        t.proposal = None;
        moments.push(Moment::Assigned { term, spawned });
        if !t.state.follows_the_agent() {
            moments.push(Moment::State { from: t.state, to: TaskState::Running });
            t.state = TaskState::Running;
        }
        if let Some(Took::Changed(Some(moment))) = branch.map(|b| take_branch(t, b)) {
            moments.push(moment);
        }
        t.updated_ms = now;
        let task_now = t.clone();
        let mut updates: Vec<Change> = moments
            .into_iter()
            .map(|what| {
                let entry = record.log(Some(task), what, now);
                record.task_update(&task_now, Some(entry))
            })
            .collect();
        if let Some(at) = self.unclaimed.iter().position(|(t, _)| *t == term)
            && let Some((_, natives)) = self.unclaimed.remove(at)
            && let Ok(record) = self.record(id)
            && let Some(node) = record.natives_mut(Some(task))
        {
            let leaves: Vec<Native> = natives
                .agents
                .into_iter()
                .map(Native::Agent)
                .chain(natives.tasks.into_iter().map(Native::Todo))
                .collect();
            let mut changed = Vec::new();
            for leaf in leaves {
                if take_leaf(node, &leaf) {
                    changed.push(leaf);
                }
            }
            for native in changed {
                updates.push(record.native_update(Some(task), native));
            }
        }
        Ok((task_now, updates))
    }

    /// Keep the start the orchestrator proposed for `task`, which waits for the person: a
    /// proposal again replaces the last.
    pub(crate) fn propose(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        proposal: Proposal,
        now: WallMs,
    ) -> Changed<Task> {
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        if t.state == TaskState::Merged {
            return Err(invalid(format!("task {task} is merged; make a new task")));
        }
        if let Some(term) = open_term(t) {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} has a terminal already, {}/{}; close it first",
                    term.worker, term.session
                ),
            ));
        }
        let on = proposal.proposed.on;
        t.proposal = Some(proposal);
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = record.log(Some(task), Moment::Proposed { on }, now);
        let update = record.task_update(&task_now, Some(entry));
        Ok((task_now, vec![update]))
    }

    /// The items still open on `task`'s agent's own task list, by their titles: they keep its
    /// work from merging.
    pub(crate) fn open_todos(&self, id: &ProjectId, task: TaskId) -> Vec<String> {
        let Some(record) = self.records.get(id) else { return Vec::new() };
        let natives = record.natives_of(Some(task));
        natives.tasks.iter().filter(|t| !t.done).map(|t| t.subject.clone()).collect()
    }

    /// Every task whose pull request's checks are worth reading: one with a pull request
    /// still open to merge, and a worktree on the worker its agent ran on to read them in.
    pub(crate) fn pull_requests(&self) -> Vec<PrWatch> {
        self.records
            .iter()
            .flat_map(|(id, r)| {
                r.tasks.iter().filter_map(move |t| {
                    if matches!(t.state, TaskState::Merged | TaskState::Failed) {
                        return None;
                    }
                    let pr = t.pr.as_ref()?;
                    Some(PrWatch {
                        project: id.clone(),
                        task: t.id,
                        worker: t.assignment.as_ref()?.term.worker,
                        cwd: t.worktree.clone()?,
                        number: pr.number,
                        merge_request: pr.merge_request,
                    })
                })
            })
            .collect()
    }

    /// What `task`'s pull request's checks say now. Only a change is worth a word: it goes to
    /// the card, and to the timeline when where they stand together moved.
    pub(crate) fn set_checks(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        mut checks: Checks,
        now: WallMs,
    ) -> Result<Vec<Change>, Refused> {
        checks.at_ms = now;
        checks.failing.truncate(CHECKS_NAMED);
        for name in &mut checks.failing {
            *name = clipped(name, CHECK_NAME_MAX);
        }
        checks.why = checks.why.map(|why| clipped(&why, CHECKS_WHY_MAX));
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        let was = t.checks.as_ref().map(|c| c.state);
        // A forge that stops answering leaves the last reading standing: it is still the most
        // that is known, and a passing card flickering to unknown and back says nothing.
        let read_before = was.is_some_and(|s| s != ChecksState::Unknown);
        if checks.state == ChecksState::Unknown && read_before {
            return Ok(Vec::new());
        }
        if t.checks.as_ref().is_some_and(|c| c.says_as(&checks)) {
            t.checks = Some(checks);
            return Ok(Vec::new());
        }
        let moved = was != Some(checks.state);
        t.checks = Some(checks.clone());
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = moved.then(|| record.log(Some(task), Moment::Checks(checks), now));
        Ok(vec![record.task_update(&task_now, entry)])
    }

    /// The person tells `task`'s agent `text`: kept on the timeline, and handed back trimmed for
    /// the hub to deliver. A task with no agent running has nobody to hear it.
    pub(crate) fn tell(
        &mut self,
        id: &ProjectId,
        task: Option<TaskId>,
        text: &str,
        terminals: &HashSet<TermRef>,
        now: WallMs,
    ) -> Result<(String, Vec<Change>), Refused> {
        let text = text.trim();
        if text.is_empty() {
            return Err(invalid("say something to the agent"));
        }
        within("what the person says", Some(text), NOTE_MAX)?;
        let record = self.record(id)?;
        if let Some(task) = task {
            record.task_mut(task)?;
        }
        if self.node_term(id, task, terminals).is_none() {
            return Err(invalid(match task {
                Some(task) => {
                    format!("task {task} has no agent running to hear it; start one for it first")
                }
                None => format!("{id} has no orchestrator running to hear it"),
            }));
        }
        let record = self.record(id)?;
        let entry = record.log(task, Moment::Told { text: text.to_owned() }, now);
        let kept = Kept { entry: Some(entry), ..record.kept() };
        Ok((text.to_owned(), vec![Change { kept, durable: true }]))
    }

    /// The project and task whose open assignment is `term`.
    fn working_in(&self, term: TermRef) -> Option<(&ProjectId, TaskId)> {
        self.records.iter().find_map(|(id, r)| {
            r.tasks.iter().find(|t| open_term(t) == Some(term)).map(|t| (id, t.id))
        })
    }

    /// An agent's status changed: the task it works on follows it while it runs, and the time
    /// it spends at work is counted on its task, or on its project for an orchestrator. Only a
    /// block is worth the timeline, since working and waiting flip at every turn. A stretch of
    /// work that ended is written, so the time survives a restart; the flips between are only
    /// pushed.
    pub(crate) fn agent_status(
        &mut self,
        term: TermRef,
        status: &AgentStatus,
        now: WallMs,
    ) -> Vec<Change> {
        let to = follows(status);
        let works = Spent::works(status);
        let mut updates = Vec::new();
        for record in self.records.values_mut() {
            if record.project.orchestrator == Some(term)
                && let Some(stretch) = record.project.orchestrator_spent.follow(works, now)
            {
                let durable = stretch == Stretch::Ended;
                updates.push(Change { durable, ..record.record_update(None) });
            }
            let Some(t) = record.tasks.iter_mut().find(|t| open_term(t) == Some(term)) else {
                continue;
            };
            let stretch = t.spent.follow(works, now);
            let moved = to.filter(|to| t.state.follows_the_agent() && t.state != *to);
            if stretch.is_none() && moved.is_none() {
                continue;
            }
            let from = t.state;
            if let Some(to) = moved {
                t.state = to;
                t.updated_ms = now;
            }
            let (task, id) = (t.clone(), t.id);
            let entry = (moved == Some(TaskState::Blocked))
                .then(|| record.log(Some(id), Moment::State { from, to: TaskState::Blocked }, now));
            let durable = entry.is_some() || stretch == Some(Stretch::Ended);
            updates.push(Change { durable, ..record.task_update(&task, entry) });
        }
        updates
    }

    /// What the server does for `task` around its agent moved ([`TaskStep`]): the card shows
    /// it. A step that began, finished or failed goes on the timeline too, its texts clipped;
    /// progress between is the card's alone, and not kept.
    pub(crate) fn set_step(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        mut step: TaskStep,
        now: WallMs,
    ) -> Result<Vec<Change>, Refused> {
        let text = match &mut step.state {
            StepState::Running { phase, .. } => phase,
            StepState::Done { detail } => detail,
            StepState::Failed { why } => why,
        };
        *text = clipped(text, SUMMARY_MAX);
        let record = self.record(id)?;
        let t =
            record.tasks.iter_mut().find(|t| t.id == task).ok_or_else(|| unknown_task(id, task))?;
        let began = !matches!(&t.step, Some(s) if s.kind == step.kind && matches!(s.state, StepState::Running { .. }));
        let logged = match step.state {
            StepState::Running { .. } => began,
            StepState::Done { .. } | StepState::Failed { .. } => true,
        };
        if !began && let Some(was) = &t.step {
            step.since_ms = was.since_ms;
        }
        t.step = Some(step.clone());
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = logged.then(|| record.log(Some(task), Moment::Step(step), now));
        Ok(vec![Change { durable: logged, ..record.task_update(&task_now, entry) }])
    }

    /// The terminal `term` is in the repository `id`: a project it orchestrates that knows no
    /// repository yet learns it, each key clipped like any other ref. Learned once: the
    /// orchestrator walking into another checkout later does not move where tasks go.
    pub(crate) fn repo_seen(&mut self, term: TermRef, id: &RepoId) -> Vec<Change> {
        let clip = |key: &Option<String>| key.as_deref().map(|k| clipped(k, REF_MAX));
        let id = RepoId { origin: clip(&id.origin), root: clip(&id.root), url: clip(&id.url) };
        self.records
            .values_mut()
            .filter(|r| r.project.orchestrator == Some(term) && r.project.repo_id.is_none())
            .map(|record| {
                record.project.repo_id = Some(id.clone());
                record.record_update(None)
            })
            .collect()
    }

    /// The terminal `term` closed: what worked in it is gone.
    pub(crate) fn session_ended(&mut self, term: TermRef, now: WallMs) -> Vec<Change> {
        self.unclaimed.retain(|(t, _)| *t != term);
        let mut updates = Vec::new();
        for record in self.records.values_mut() {
            let ended: Vec<Task> = record
                .tasks
                .iter_mut()
                .filter_map(|t| {
                    let a = t.assignment.as_mut().filter(|a| a.term == term)?;
                    if a.ended_ms.is_some() {
                        return None;
                    }
                    a.ended_ms = Some(now);
                    t.updated_ms = now;
                    t.spent.follow(false, now);
                    Some(t.clone())
                })
                .collect();
            for task in ended {
                let entry = record.log(Some(task.id), Moment::AgentGone { term }, now);
                updates.push(record.task_update(&task, Some(entry)));
            }
            if record.project.orchestrator == Some(term) {
                record.project.orchestrator_spent.follow(false, now);
                let entry = record.log(None, Moment::AgentGone { term }, now);
                updates.push(record.record_update(Some(entry)));
            }
        }
        updates
    }

    /// A worker registered with `sessions` open: every assignment on it to a terminal it no
    /// longer has ended while the server was away.
    pub(crate) fn reconcile(
        &mut self,
        worker: WorkerId,
        sessions: &[SessionId],
        now: WallMs,
    ) -> Vec<Change> {
        let gone: HashSet<TermRef> = self
            .records
            .values()
            .flat_map(|r| r.tasks.iter().filter_map(open_term))
            .filter(|t| t.worker == worker && !sessions.contains(&t.session))
            .collect();
        gone.into_iter().flat_map(|term| self.session_ended(term, now)).collect()
    }

    /// What the worker `worker` reported of what runs in a terminal: its node (a task's
    /// terminal, or a project's orchestrator) takes it in, or it waits for a task to take the
    /// terminal on.
    pub(crate) fn report(
        &mut self,
        worker: WorkerId,
        report: &AgentReport,
        now: WallMs,
    ) -> Vec<Change> {
        let term = TermRef { worker, session: report.session() };
        let mut updates = Vec::new();
        let mut held = false;
        for record in self.records.values_mut() {
            let working = record.tasks.iter().find(|t| open_term(t) == Some(term)).map(|t| t.id);
            let node = match working {
                Some(task) => Some(task),
                None if record.project.orchestrator == Some(term) => None,
                None => continue,
            };
            held = true;
            if let AgentReport::Branch(branch) = report {
                let Some(task) = node else { continue };
                let Ok(t) = record.task_mut(task) else { continue };
                let Took::Changed(moment) = take_branch(t, branch) else { continue };
                t.updated_ms = now;
                let task_now = t.clone();
                let entry = moment.map(|what| record.log(Some(task), what, now));
                updates.push(record.task_update(&task_now, entry));
            } else if let Some(native) =
                record.natives_mut(node).and_then(|natives| leaf(report, natives, now))
            {
                updates.push(record.native_update(node, native));
            }
        }
        let leafless = matches!(
            report,
            AgentReport::Branch(_)
                | AgentReport::PermissionMode { .. }
                | AgentReport::Loosened { .. }
                | AgentReport::Delivered { .. }
        );
        if !held && !leafless {
            self.hold_unclaimed(term, report, now);
        }
        updates
    }

    fn hold_unclaimed(&mut self, term: TermRef, report: &AgentReport, now: WallMs) {
        if !self.unclaimed.iter().any(|(t, _)| *t == term) {
            if self.unclaimed.len() >= UNCLAIMED_KEPT {
                self.unclaimed.pop_front();
            }
            self.unclaimed.push_back((term, Natives::default()));
        }
        if let Some((_, natives)) = self.unclaimed.iter_mut().find(|(t, _)| *t == term) {
            leaf(report, natives, now);
        }
    }
}

/// The paths as tasks own them, each once.
/// Refused before any is read when there are more than `most` (`owns_max`): each is compared
/// with every other.
fn claimable(paths: &[String], most: u16) -> Result<Vec<String>, Refused> {
    if paths.len() > usize::from(most) {
        return Err(over_bound("owns_max", paths.len(), most.into()));
    }
    let mut out: Vec<String> = Vec::with_capacity(paths.len());
    let mut seen: HashSet<String> = HashSet::with_capacity(paths.len());
    for path in paths {
        within("a path", Some(path), REF_MAX)?;
        let path = owned(path).map_err(invalid)?;
        if seen.insert(folded(&path)) {
            out.push(path);
        }
    }
    Ok(out)
}

/// Whether two owned paths name one file, case aside.
fn same_path(a: &str, b: &str) -> bool {
    folded(a) == folded(b)
}

/// A path under Unicode full case folding, as a case-insensitive volume compares names:
/// `Straße` and `STRASSE` are one name, as are `ǅ` and `ǆ`.
fn folded(path: &str) -> String {
    icu_casemap::CaseMapper::new().fold_string(path).into_owned()
}

/// `text` cut to at most `max` bytes, at a character boundary: what a worker reports is kept
/// to the bounds a card is held to.
pub(crate) fn clipped(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

fn overlapping(theirs: TaskId, path: &str, ours: &str) -> Refused {
    let shown =
        |p: &str| if p.is_empty() { "the whole repository".to_owned() } else { p.to_owned() };
    refuse(
        ErrorCode::Conflict,
        format!(
            "{} overlaps {}, which task {theirs} owns; split the work another way, make this task \
             read-only, or wait until task {theirs} is merged",
            shown(ours),
            shown(path)
        ),
    )
}

/// What a report did to a node.
enum Took {
    /// Nothing changed.
    Unchanged,
    /// The node changed, with the moment worth the timeline, if any.
    Changed(Option<Moment>),
}

/// Take a status line's branch into `t`; a new branch or pull request is worth the timeline.
fn take_branch(t: &mut Task, branch: &AgentBranch) -> Took {
    let worktree = branch.worktree.as_ref().map(|w| clipped(&w.path, REF_MAX));
    let named = branch
        .worktree
        .as_ref()
        .and_then(|w| w.branch.as_deref().map(|b| clipped(b, REF_MAX)))
        .or_else(|| t.branch.clone());
    let pr = branch.pr.clone().map(|mut pr| {
        pr.url = clipped(&pr.url, REF_MAX);
        pr
    });
    if t.worktree == worktree && t.branch == named && t.pr == pr {
        return Took::Unchanged;
    }
    let before = (t.branch.clone(), t.pr.as_ref().map(|p| p.number));
    t.worktree = worktree;
    t.branch = named;
    t.pr = pr;
    let now = (t.branch.clone(), t.pr.as_ref().map(|p| p.number));
    Took::Changed((now != before).then_some(Moment::Branch { branch: now.0, pr: now.1 }))
}

/// Take a subagent's or a task-list item's report into `natives`: the leaf as it is now, when
/// it changed.
fn leaf(report: &AgentReport, natives: &mut Natives, now: WallMs) -> Option<Native> {
    let reported = match report {
        AgentReport::Branch(_)
        | AgentReport::PermissionMode { .. }
        | AgentReport::Loosened { .. }
        | AgentReport::Delivered { .. } => return None,
        AgentReport::SubagentStarted { agent, kind, .. } => Native::Agent(NativeAgent {
            id: clipped(agent, REF_MAX),
            kind: clipped(kind, KIND_MAX),
            started_ms: now,
            stopped_ms: None,
            transcript: None,
            last: None,
        }),
        AgentReport::SubagentStopped { agent, transcript, last, .. } => {
            let known = natives.agents.iter().find(|a| a.id == *agent);
            Native::Agent(NativeAgent {
                id: clipped(agent, REF_MAX),
                kind: known.map(|a| a.kind.clone()).unwrap_or_default(),
                started_ms: known.map_or(now, |a| a.started_ms),
                stopped_ms: Some(now),
                transcript: transcript.as_deref().map(|t| clipped(t, REF_MAX)),
                last: last.as_deref().map(|l| clipped(l, SUMMARY_MAX)),
            })
        }
        AgentReport::NativeTask { task, .. } => {
            let mut task = task.clone();
            task.id = clipped(&task.id, REF_MAX);
            task.subject = clipped(&task.subject, SUMMARY_MAX);
            if task.subject.is_empty()
                && let Some(known) = natives.tasks.iter().find(|t| t.id == task.id)
            {
                task.subject.clone_from(&known.subject);
            }
            Native::Todo(task)
        }
    };
    take_leaf(natives, &reported).then_some(reported)
}

/// Put `leaf` in `natives` in place of the one of its id; whether that changed anything.
fn take_leaf(natives: &mut Natives, leaf: &Native) -> bool {
    match leaf {
        Native::Agent(agent) => {
            if let Some(known) = natives.agents.iter_mut().find(|a| a.id == agent.id) {
                // A second start of a running subagent is the same subagent.
                let same =
                    *known == *agent || (agent.stopped_ms.is_none() && known.stopped_ms.is_none());
                if !same {
                    *known = agent.clone();
                }
                return !same;
            }
            if natives.agents.len() >= NATIVES_KEPT {
                let at = natives.agents.iter().position(|a| a.stopped_ms.is_some()).unwrap_or(0);
                natives.agents.remove(at);
            }
            natives.agents.push(agent.clone());
        }
        Native::Todo(task) => {
            if let Some(known) = natives.tasks.iter_mut().find(|t| t.id == task.id) {
                let same = *known == *task;
                if !same {
                    *known = task.clone();
                }
                return !same;
            }
            if natives.tasks.len() >= NATIVES_KEPT {
                natives.tasks.remove(0);
            }
            natives.tasks.push(task.clone());
        }
    }
    true
}

#[cfg(test)]
mod cost;
mod merge;
pub(crate) use merge::{Advance, Job, Queue, bounded as bounded_review};
#[cfg(test)]
mod tests;
