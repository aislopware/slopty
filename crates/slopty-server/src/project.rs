//! Projects on the server (`docs/decisions/projects.md`).
//!
//! The records the store keeps and the rules a change keeps: a task's dependencies never lead
//! back to it, a merged task stays merged, and one terminal works on a task at a time. Tasks are
//! the orchestrator's, one level: no task is split from another.
//! How many agents run is counted from the terminals that are live, never from what a task's
//! state says alone, so nothing that runs escapes the person's bounds: only the agent of a task
//! merged or given up counts no more while it rests, and counts again as soon as it works.
//!
//! Every change answers with the `Change`s it made, in order: the hub pushes each to clients
//! as a [`ProjectUpdate`] and hands the store what it must keep ([`Kept`]). A refused change
//! leaves everything as it was.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use slopty_agent::status::{AgentStatus, BlockReason};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentBranch;
use slopty_proto::orchestration::{
    ErrorCode, IdempotencyKey, KEY_LIFETIME, Outcome, TermRef, Verb,
};
use slopty_proto::project::{
    ARTIFACTS_MAX, AgentReport, Assignment, BRIEF_MAX, DEPENDS_MAX, GiveBacks, KIND_MAX, Limits,
    LimitsChange, Live, METADATA_MAX, Matcher, Merge, Moment, NOTE_MAX, Native, NativeAgent,
    NativeChange, Natives, NodeDetail, NodeNatives, PROJECTS_MAX, Project, ProjectStatus,
    ProjectUpdate, REF_MAX, Report, RunOn, STATUS_MAX, SUMMARY_MAX, Spent, StepState, Stretch,
    TASKS_MAX, TESTS_NAMED, TIMELINE_BYTES_KEPT, TIMELINE_KEPT, TIMELINE_PAGE, TIMELINE_PAGE_BYTES,
    TITLE_MAX, Task, TaskChange, TaskId, TaskSpec, TaskState, TaskStep, TestDiff, TimelineEntry,
    VerifierRun,
};
/// What a [`Policy`] is made of, for the binary that reads it from the person's settings.
pub use slopty_proto::project::{Bounds, ProjectId};
use slopty_proto::terminal::RepoId;
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::wire::{PullSeen, PullStands};

/// Latest timeline entries a connecting client gets, and a status read with no cursor.
pub const RECENT_ENTRIES: usize = 64;
/// Subagents and task-list items kept per node, the oldest dropped first.
pub const NATIVES_KEPT: usize = 256;
/// Sessions whose natives are kept until a task takes the session on, the oldest dropped first.
const UNCLAIMED_KEPT: usize = 256;

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

/// Who tells a node's agent something ([`Projects::tell`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Teller {
    /// The person.
    Person,
    /// The project's orchestrator, telling one of its tasks.
    Orchestrator,
}

/// The store's file: every project whole, and the terminals the server watches, as of the
/// `through`th change.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    /// By name.
    pub projects: Vec<Record>,
    /// The terminals the server watches, whatever project they are in.
    pub watched: Vec<Watched>,
    /// The keys changes and starts were made under lately, oldest first, so a caller that
    /// sends one again after a restart is answered as the first time and nothing is done twice.
    pub keys: Vec<KeptKey>,
    /// The merges the person asked for that wait for their task's branch to come home.
    pub merges: Vec<(ProjectId, TaskId)>,
    /// What projects let go left for workers to remove, until each worker answers.
    pub cleanups: Vec<Cleanup>,
    /// How many changes it holds: the store's log goes on from the next.
    pub through: u64,
}

/// What a project let go leaves for a worker to remove, kept until the worker answers: one away
/// then is asked again once it registers, so nothing is left behind on a machine that was off.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Cleanup {
    /// A task's worktree, or the project's verify checkout
    /// ([`slopty_proto::project::VERIFY_PLACES`]), with where the work landed
    /// ([`Verb::RemoveWorktree`]).
    Worktree {
        /// Where.
        worker: WorkerId,
        /// The worktree.
        worktree: String,
        /// Where its work landed.
        landed: Vec<String>,
    },
    /// The branches the server named in a clone ([`Verb::DropBranches`]).
    Branches {
        /// Where.
        worker: WorkerId,
        /// The clone.
        repo: String,
        /// The branches.
        branches: Vec<String>,
    },
}

impl Cleanup {
    /// The worker it is for.
    #[must_use]
    pub const fn worker(&self) -> WorkerId {
        match self {
            Self::Worktree { worker, .. } | Self::Branches { worker, .. } => *worker,
        }
    }

    /// The same, for `worker` instead: the machine came back under a new id.
    #[must_use]
    pub const fn on(mut self, to: WorkerId) -> Self {
        match &mut self {
            Self::Worktree { worker, .. } | Self::Branches { worker, .. } => *worker = to,
        }
        self
    }

    /// The verb that asks it of its worker.
    #[must_use]
    pub fn verb(&self) -> Verb {
        match self.clone() {
            Self::Worktree { worker, worktree, landed } => {
                Verb::RemoveWorktree { worker, worktree, landed }
            }
            Self::Branches { worker, repo, branches } => {
                Verb::DropBranches { worker, repo, branches }
            }
        }
    }
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
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Keep {
    /// A project's.
    Project(Box<Kept>),
    /// A terminal watched, as it is now.
    Watch(Watched),
    /// A terminal no longer watched.
    Unwatch(SessionId),
    /// A project the person let go, with all it held.
    Forget(ProjectId),
    /// A key a change or a start was made under, as it stands now.
    Key(Box<KeptKey>),
    /// A merge the person asked for now waits for its task's branch to come home (`waits`), or
    /// no longer does.
    Merge {
        /// The task's project.
        project: ProjectId,
        /// The task.
        task: TaskId,
        /// Whether it waits.
        waits: bool,
    },
    /// A cleanup now waits for its worker's answer (`waits`), or no longer does.
    Cleanup {
        /// What.
        cleanup: Cleanup,
        /// Whether it waits.
        waits: bool,
    },
}

/// The most keys the store holds: the hub remembers [`KEYS_REMEMBERED`] changes and as many
/// starts.
pub const KEYS_KEPT: usize = KEYS_REMEMBERED.saturating_mul(2);

/// Keyed changes, and keyed starts, the hub remembers each, the oldest dropped first: an
/// orchestrator makes a few tasks a minute, so this outlasts [`KEY_LIFETIME`] with room to
/// spare.
pub const KEYS_REMEMBERED: usize = 1024;

/// A key a change or a start was made under, kept so a repeat of its verb, even after a
/// restart, answers as the first did.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct KeptKey {
    /// The key.
    pub key: IdempotencyKey,
    /// The digest of the verb as its caller sent it.
    pub digest: [u8; 32],
    /// What a repeat answers.
    pub first: First,
    /// When it was first used.
    pub at: WallMs,
}

/// What a key was used for, and what a repeat under it does.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum First {
    /// A project change.
    Change(Remembered),
    /// A start of an agent or a task's terminal.
    Start(StartKept),
}

/// What a keyed change answered, as a repeat of it answers.
///
/// The answer is kept as what it names (a task, a project) and read again for a repeat: a
/// whole project's status or a task's brief held for each of a thousand keys would be
/// gigabytes.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Remembered {
    /// This answer, which is small: done, an error, a terminal.
    Outcome(Outcome),
    /// The task as it is when asked again.
    Task(ProjectId, TaskId),
    /// The project's status as it is when asked again.
    Status(ProjectId),
}

/// What a keyed start keeps for a repeat.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum StartKept {
    /// The start as forwarded, not yet answered for sure.
    Forwarded(Box<Verb>),
    /// Its worker's answer.
    Answered(Outcome),
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
    /// Whether the store keeps it. What flips within an agent's turn is known again from its
    /// worker when it registers, so it is pushed and never written; where a task's turn began
    /// and ended is written, for a restart to take its turn up.
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
            Keep::Forget(project) => {
                self.projects.retain(|r| r.project.id != *project);
                self.merges.retain(|(p, _)| p != project);
            }
            Keep::Key(kept) => self.apply_key(kept),
            Keep::Merge { project, task, waits } => {
                let merge = (project.clone(), *task);
                self.merges.retain(|m| *m != merge);
                if *waits {
                    self.merges.push(merge);
                }
            }
            Keep::Cleanup { cleanup, waits } => {
                self.cleanups.retain(|c| c != cleanup);
                if *waits {
                    self.cleanups.push(cleanup.clone());
                }
            }
        }
    }

    /// Hold `kept` in place of what its key held, and let go of the keys past their lifetime
    /// as of it, and of the oldest past [`KEYS_KEPT`].
    fn apply_key(&mut self, kept: &KeptKey) {
        match self.keys.iter_mut().find(|k| k.key == kept.key) {
            Some(held) => held.clone_from(kept),
            None => self.keys.push(kept.clone()),
        }
        let lifetime = u64::try_from(KEY_LIFETIME.as_millis()).unwrap_or(u64::MAX);
        let since = kept.at.as_millis().saturating_sub(lifetime);
        self.keys.retain(|k| k.at.as_millis() >= since);
        let past = self.keys.len().saturating_sub(KEYS_KEPT);
        self.keys.drain(..past);
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
    /// Its latest timeline entries, oldest first, at most [`TIMELINE_KEPT`] of them and
    /// [`TIMELINE_BYTES_KEPT`] in all.
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
    pub push: bool,
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
    pub push: Option<bool>,
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
    /// The thread its agent runs as, for a task started as one.
    pub thread: Option<ThreadId>,
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
    /// Each task's agent's turn, and what the nodes above are to hear of it.
    turns: turns::Turns,
    /// The steps under way when the server stopped, as they stood, until their worker is back
    /// to take them up ([`Self::resumable`]).
    restarted: HashMap<(ProjectId, TaskId), TaskStep>,
    /// The tasks whose machine went away while their agent worked, until it is back
    /// ([`Self::machine_seen`]).
    away: HashSet<(ProjectId, TaskId)>,
    /// The tasks whose worktree is being freed, until how it went is heard ([`Self::freed`]):
    /// one is asked of its worker once at a time.
    freeing: HashSet<(ProjectId, TaskId)>,
    /// The tasks whose worktree its worker kept, for something in it not committed or a
    /// terminal at work in it: not asked again on their own, as the person frees one from the
    /// worktree list, but letting the project go asks once more ([`Self::worktrees_of`]).
    kept: HashSet<(ProjectId, TaskId)>,
}

/// Which way a worker's link went, for its tasks ([`Projects::machine_seen`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Seen {
    /// It stopped answering.
    Away,
    /// It answers again.
    Back,
}

/// What a step under way when the server stopped says until its worker is back to take it up.
pub(crate) const RESUMING: &str = "Taken up again once its worker is back";

fn refuse(code: ErrorCode, message: impl Into<String>) -> Refused {
    Outcome::Error { code, message: message.into() }
}

fn invalid(message: impl Into<String>) -> Refused {
    refuse(ErrorCode::Invalid, message)
}

fn unknown_project(id: &ProjectId) -> Refused {
    refuse(ErrorCode::UnknownProject, format!("no project {id}; `slopty project list` names them"))
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

    /// Drop the oldest entries past [`TIMELINE_KEPT`] or [`TIMELINE_BYTES_KEPT`].
    fn trim_timeline(&mut self) {
        while self.timeline.len() > TIMELINE_KEPT || self.timeline_bytes > TIMELINE_BYTES_KEPT {
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

    /// The terminals of the project that are live and count: its tasks' and its
    /// orchestrator's, but not those of tasks [`finished`].
    fn live_terms<'a>(&'a self, terminals: &'a HashSet<TermRef>) -> impl Iterator<Item = TermRef> {
        let tasks = self.tasks.iter().filter(|t| !finished(t)).filter_map(open_term);
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

    /// The tasks that wait on the person ([`Task::waits_on_person`]), each with why, in a few
    /// words.
    fn waiting_on_person(&self) -> Vec<(TaskId, &'static str)> {
        self.tasks
            .iter()
            .filter(|t| t.waits_on_person())
            .map(|t| {
                let why = if t.ready_to_merge() {
                    "ready to merge"
                } else if t.give_backs.held {
                    "given back as often as it may"
                } else {
                    "asks the person"
                };
                (t.id, why)
            })
            .collect()
    }

    /// Whether an agent may start more of the project's work: not while as many tasks wait on
    /// the person as its review limit, since work they cannot look at only piles up.
    fn room_to_review(&self) -> Result<(), Refused> {
        let waiting = self.waiting_on_person();
        let most = self.project.limits.review;
        if waiting.len() < usize::from(most) {
            return Ok(());
        }
        let named: Vec<String> =
            waiting.iter().take(4).map(|(task, why)| format!("task {task} {why}")).collect();
        let more = waiting.len().saturating_sub(named.len());
        let more = if more > 0 { format!(" and {more} more") } else { String::new() };
        Err(refuse(
            ErrorCode::Limit,
            format!(
                "project {} has {} tasks waiting on the person ({}{more}), its review limit of \
                 {most}; no agent starts more work until the person merges, answers or takes \
                 one back, and only they raise the limit",
                self.project.id,
                waiting.len(),
                named.join(", ")
            ),
        ))
    }
}

/// The terminal a task's open assignment names.
fn open_term(t: &Task) -> Option<TermRef> {
    t.assignment.as_ref().filter(|a| a.open()).map(|a| a.term)
}

/// Whether a task's work is over (merged, or given up) and its agent is not at work: its
/// terminal counts against no limit, though it may still be open. An agent that works again
/// counts again, so giving up its own task frees no agent that goes on working.
/// Where `t`'s work landed, for its worktree's branch to go with it: the task's own commit the
/// merge took and the merge's head, or the project's target, and the target on `origin`. The
/// own commit is what a worker other than the orchestrator's holds, as the merge happened in
/// the orchestrator's clone.
fn landed_of(record: &Record, t: &Task) -> Vec<String> {
    let (took, target) = match &t.merge {
        Some(Merge::Merged { target, head, from, .. }) => {
            (vec![from.clone(), head.clone()], target.clone())
        }
        _ => (Vec::new(), record.project.target.clone()),
    };
    let origin = format!("origin/{target}");
    took.into_iter().chain([target, origin]).collect()
}

const fn finished(t: &Task) -> bool {
    matches!(t.state, TaskState::Merged | TaskState::Failed) && t.spent.since_ms.is_none()
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

/// The state an agent's status puts the task it works on in; `None` for no agent.
const fn follows(status: &AgentStatus) -> Option<TaskState> {
    match status {
        AgentStatus::None => None,
        AgentStatus::Working | AgentStatus::Tool { .. } => Some(TaskState::Running),
        AgentStatus::Blocked(BlockReason::IdlePrompt)
        | AgentStatus::Idle
        | AgentStatus::Done
        | AgentStatus::Failed { .. }
        | AgentStatus::Waiting { .. } => Some(TaskState::Waiting),
        AgentStatus::Blocked(_) => Some(TaskState::Blocked),
    }
}

/// `limits` changed by `change`. The review limit is the person's alone to set
/// ([`LimitsChange::review`]), which the hub sees to.
fn limited(limits: Limits, change: LimitsChange) -> Result<Limits, Refused> {
    if change.review == Some(0) {
        return Err(invalid(
            "review is at least 1: a project with nothing to review starts nothing",
        ));
    }
    Ok(Limits { review: change.review.unwrap_or(limits.review) })
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

/// A refusal for passing one of the server's bounds: `what` is `have` of `most`.
fn over_bound(what: &str, have: usize, most: usize) -> Refused {
    refuse(ErrorCode::Limit, format!("{what}: {have} would pass the {most} the server allows"))
}

/// A title as it is kept: trimmed, not empty, within [`TITLE_MAX`].
fn titled(title: &str) -> Result<String, Refused> {
    let title = title.trim();
    if title.is_empty() {
        return Err(invalid("a title is not empty"));
    }
    if title.len() > TITLE_MAX {
        return Err(over_bound("a title's bytes", title.len(), TITLE_MAX));
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
        let mut restarted = HashMap::new();
        let mut turns = turns::Turns::default();
        let records = file
            .projects
            .into_iter()
            .map(|mut r| {
                r.recount();
                // A step under way when the server stopped is taken up again once its worker
                // is back ([`Self::resumable`]); until then it says so.
                for t in &mut r.tasks {
                    let Some(step) = t.step.as_mut().filter(|s| s.running()) else { continue };
                    restarted.insert((r.project.id.clone(), t.id), step.clone());
                    step.state = StepState::Running { phase: RESUMING.to_owned(), percent: None };
                }
                // Nor a stretch of work: how long the server was away is not known to be
                // work, and the agent's status after its worker registers starts the next. A
                // task's turn under way is still one, so a turn that ended while the server was
                // away is heard once its worker says so.
                r.project.orchestrator_spent.since_ms = None;
                for t in &mut r.tasks {
                    if let Some(since) = t.spent.since_ms.take()
                        && t.state == TaskState::Running
                        && let Some(term) = open_term(t)
                    {
                        let answered = r.timeline.iter().any(|e| {
                            e.task == Some(t.id)
                                && e.at_ms >= since
                                && matches!(&e.what, Moment::Reported { .. })
                        });
                        turns.resume(term, answered);
                    }
                }
                (r.project.id.clone(), r)
            })
            .collect();
        Self { records, turns, restarted, ..Self::default() }
    }

    /// The steps under way on `worker` when the server stopped, as they stood then, each once:
    /// its worker is back to take them up.
    pub(crate) fn resumable(&mut self, worker: WorkerId) -> Vec<(ProjectId, TaskId, TaskStep)> {
        self.restarted
            .extract_if(|_, step| step.worker == worker)
            .map(|((project, task), step)| (project, task, step))
            .collect()
    }

    /// Every project, as the store keeps it after `through` changes, beside `watched`.
    pub(crate) fn file(&self, watched: Vec<Watched>, through: u64) -> ProjectsFile {
        let projects = self.records.values().cloned().collect();
        ProjectsFile {
            projects,
            watched,
            keys: Vec::new(),
            merges: Vec::new(),
            cleanups: Vec::new(),
            through,
        }
    }

    /// The person's policy.
    pub(crate) const fn policy(&self) -> &Policy {
        &self.policy
    }

    /// Take up the person's policy.
    pub(crate) fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
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
        for record in self.records.values() {
            for term in record.tasks.iter().filter(|t| finished(t)).filter_map(open_term) {
                terms.remove(&term);
            }
        }
        terms.extend(running.starting.iter().map(|s| s.term));
        terms
    }

    /// Every live agent and start on `worker`, in a project or not: what its `live_agents` fact
    /// says, and what placement spreads starts by.
    pub(crate) fn live_on_worker(&self, worker: WorkerId, running: &Running<'_>) -> u16 {
        count(self.fleet_terms(running).iter().filter(|t| t.worker == worker).count())
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

    fn record(&mut self, id: &ProjectId) -> Result<&mut Record, Refused> {
        self.records.get_mut(id).ok_or_else(|| unknown_project(id))
    }

    /// A project's record.
    /// Let `id` go with its tasks, its queue and its timeline. Its terminals are not the
    /// store's: they run on, as terminals.
    pub(crate) fn delete(&mut self, id: &ProjectId) -> Result<(), Refused> {
        self.records.remove(id).ok_or_else(|| unknown_project(id))?;
        self.freeing.retain(|(p, _)| p != id);
        self.kept.retain(|(p, _)| p != id);
        Ok(())
    }

    pub(crate) fn project(&self, id: &ProjectId) -> Result<&Project, Refused> {
        self.records.get(id).map(|r| &r.project).ok_or_else(|| unknown_project(id))
    }

    /// The ids of `id`'s tasks.
    pub(crate) fn project_tasks(&self, id: &ProjectId) -> Result<Vec<TaskId>, Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        Ok(record.tasks.iter().map(|t| t.id).collect())
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

    /// A task as it is.
    pub(crate) fn task(&self, id: &ProjectId, task: TaskId) -> Result<&Task, Refused> {
        self.records.get(id).ok_or_else(|| unknown_project(id))?.task(task)
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
        if self.records.len() >= PROJECTS_MAX {
            return Err(over_bound("projects", self.records.len(), PROJECTS_MAX));
        }
        let title = titled(&new.title)?;
        let limits = limited(Limits::default(), new.limits)?;
        within("a repository", Some(&new.repo), REF_MAX)?;
        within("a target branch", Some(&new.target), REF_MAX)?;
        let project = Project {
            orchestrator_spent: Spent::default(),
            id: new.id.clone(),
            title,
            members: members(new.members)?,
            repo: new.repo.trim().to_owned(),
            repo_id: None,
            target: new.target.trim().to_owned(),
            verifier: verifier(new.verifier)?,
            push: new.push,
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
        let record = self.record(id)?;
        let limits = limited(record.project.limits, change.limits)?;
        let metadata = change.metadata.map(|m| metadata(Some(m))).transpose()?;
        let new_verifier = change.verifier.map(|v| verifier(Some(v))).transpose()?;
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
        if let Some(metadata) = metadata {
            quiet |= record.project.metadata != metadata;
            record.project.metadata = metadata;
        }
        if let Some(push) = change.push {
            quiet |= record.project.push != push;
            record.project.push = push;
        }
        if limits != record.project.limits {
            record.project.limits = limits;
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

    /// Make a task.
    pub(crate) fn create_task(
        &mut self,
        id: &ProjectId,
        spec: TaskSpec,
        now: WallMs,
    ) -> Changed<Task> {
        let record = self.record(id)?;
        if record.tasks.len() >= TASKS_MAX {
            return Err(over_bound("a project's tasks", record.tasks.len(), TASKS_MAX));
        }
        let title = titled(&spec.title)?;
        if spec.brief.len() > BRIEF_MAX {
            return Err(over_bound("a brief's bytes", spec.brief.len(), BRIEF_MAX));
        }
        if spec.depends_on.len() > DEPENDS_MAX {
            return Err(invalid(format!("a task depends on at most {DEPENDS_MAX} tasks")));
        }
        let kind = spec.kind.trim().to_owned();
        if kind.len() > KIND_MAX {
            return Err(invalid(format!("a kind is at most {KIND_MAX} bytes")));
        }
        let mut depends_on = Vec::with_capacity(spec.depends_on.len());
        for on in spec.depends_on {
            record.task(on)?;
            if !depends_on.contains(&on) {
                depends_on.push(on);
            }
        }
        let number = u32::try_from(record.tasks.len()).unwrap_or(u32::MAX).saturating_add(1);
        let task = Task {
            spent: Spent::default(),
            id: TaskId(number),
            depends_on,
            kind,
            title,
            brief: spec.brief,
            read_only: spec.read_only,
            pin: spec.pin,
            verifier: verifier(spec.verifier)?,
            metadata: metadata(spec.metadata)?,
            state: TaskState::Planned,
            status: None,
            assignment: None,
            branch: None,
            worktree: None,
            base: None,
            pull: None,
            verified: None,
            merge: None,
            created_ms: now,
            updated_ms: now,
            step: None,
            give_backs: GiveBacks::default(),
            tests: None,
        };
        record.tasks.push(task.clone());
        let entry =
            record.log(Some(task.id), Moment::TaskCreated { title: task.title.clone() }, now);
        let update = record.task_update(&task, Some(entry));
        Ok((task, vec![update]))
    }

    /// Change a task: move it, set its status, dependencies, pin, verifier or metadata, record
    /// its branch or its verifier's word, note something. The
    /// person's change is their word on it: a failure held past its give-backs is theirs now,
    /// and its count starts again.
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
        let record = self.record(id)?;
        let before = record.task(task)?.clone();
        if let Some(to) = change.state.filter(|to| *to != before.state)
            && !before.state.may_become(to)
        {
            return Err(invalid(format!(
                "task {task} cannot go from {:?} to {to:?}: a merged task is final, and only a \
                 done or verifying task merges",
                before.state
            )));
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
        if let Some(run_on) = change.run_on {
            let pin = match run_on {
                RunOn::Worker(worker) => Some(worker),
                RunOn::Anywhere => None,
            };
            quiet |= t.pin != pin;
            t.pin = pin;
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
            moments.push(Moment::Branch { branch: t.branch.clone() });
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
        if caller == Caller::Person && t.give_backs != GiveBacks::default() {
            t.give_backs = GiveBacks::default();
            quiet = true;
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

    /// A task's agent reports on its work, for the orchestrator: kept on the timeline, and
    /// answered with the task.
    pub(crate) fn report_task(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        report: &Report,
        now: WallMs,
    ) -> Changed<Task> {
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
            moments.push(Moment::Branch { branch: t.branch.clone() });
        }
        moments.push(Moment::Reported { report: report.clone() });
        t.updated_ms = now;
        let task_now = t.clone();
        let updates = moments
            .into_iter()
            .map(|what| {
                let entry = record.log(Some(task), what, now);
                record.task_update(&task_now, Some(entry))
            })
            .collect();
        self.answered(id, task);
        Ok((task_now, updates))
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

    /// Whether an agent may start more of project `id`'s work now: not while as many of its
    /// tasks wait on the person as its review limit, since more work than they can look at
    /// piles up unread. The person's own starts, and starts proposed to them, are not held.
    pub(crate) fn room_to_review(&self, id: &ProjectId) -> Result<(), Refused> {
        self.records.get(id).ok_or_else(|| unknown_project(id))?.room_to_review()
    }

    /// Whether a task may be started now: it exists, is not merged, each task it depends on
    /// has delivered ([`delivered`]), and nothing runs or is being started for it; and the
    /// project has room for one more.
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
            && let Some(dep) =
                t.depends_on.iter().find_map(|d| record.task(*d).ok().filter(|d| !delivered(d)))
        {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} depends on task {}, which is {:?}; start it once that is merged, \
                     or say ignore_dependencies",
                    dep.id, dep.state
                ),
            ));
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
        let Assignee { term, spawned, branch, conversation, thread } = who;
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
        let mut moments = Vec::new();
        if let Some(gone) = open_term(t) {
            moments.push(Moment::AgentGone { term: gone });
        }
        t.assignment =
            Some(Assignment { term, thread, since_ms: now, ended_ms: None, conversation, spawned });
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

    /// What the thread rows on `worker` say of their branches' pull requests, each by the seat
    /// its thread runs at: the card of the task assigned that seat follows its thread's
    /// ([`Task::pull`]), cut to a card's bounds. A pull request first seen, or come to stand
    /// otherwise, is worth the timeline; one gone, or with only its words changed, the card
    /// alone. With the changes, the tasks whose pull request was seen merged just now
    /// ([`Merge::Pull`] become [`Merge::Merged`]), whose clone's target is behind the forge's.
    pub(crate) fn pulls_seen(
        &mut self,
        worker: WorkerId,
        seen: &[(SessionId, Option<PullSeen>)],
        now: WallMs,
    ) -> (Vec<Change>, Vec<(ProjectId, TaskId)>) {
        let mut updates = Vec::new();
        let mut merged = Vec::new();
        for record in self.records.values_mut() {
            let mut changed = Vec::new();
            for t in &mut record.tasks {
                let Some(term) = t.assignment.as_ref().map(|a| a.term) else { continue };
                if term.worker != worker {
                    continue;
                }
                let Some((_, pull)) = seen.iter().find(|(seat, _)| *seat == term.session) else {
                    continue;
                };
                let pull = pull.clone().map(|mut p| {
                    p.url = clipped(&p.url, REF_MAX);
                    p.title = clipped(&p.title, TITLE_MAX);
                    p.base = clipped(&p.base, REF_MAX);
                    p.failed_first = p.failed_first.map(|f| clipped(&f, REF_MAX));
                    p
                });
                if t.pull == pull {
                    continue;
                }
                let standing = |p: &Option<PullSeen>| p.as_ref().map(|p| (p.number, p.stands));
                let moved = pull.is_some() && standing(&t.pull) != standing(&pull);
                t.pull.clone_from(&pull);
                t.updated_ms = now;
                let landed = landed(t, pull.as_ref(), now);
                changed.push((t.id, pull.filter(|_| moved), landed));
            }
            for (task, moment, landed) in changed {
                let entry = moment.map(|pull| record.log(Some(task), Moment::Pull(pull), now));
                let entry = match landed {
                    Some(moved) => {
                        if matches!(moved, Moment::State { to: TaskState::Merged, .. }) {
                            merged.push((record.project.id.clone(), task));
                        }
                        Some(record.log(Some(task), moved, now))
                    }
                    None => entry,
                };
                if let Ok(task_now) = record.task(task).cloned() {
                    updates.push(record.task_update(&task_now, entry));
                }
            }
        }
        (updates, merged)
    }

    /// What `task`'s work did to the project's tests, read once its branch came home: on its
    /// card, within the bounds a card keeps.
    pub(crate) fn set_tests(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        mut tests: TestDiff,
        now: WallMs,
    ) -> Vec<Change> {
        tests.head = clipped(&tests.head, REF_MAX);
        for paths in [&mut tests.deleted, &mut tests.changed] {
            paths.truncate(TESTS_NAMED);
            for path in paths.iter_mut() {
                *path = clipped(path, REF_MAX);
            }
        }
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Ok(t) = record.task_mut(task) else { return Vec::new() };
        if t.tests.as_ref() == Some(&tests) {
            return Vec::new();
        }
        t.tests = Some(tests);
        t.updated_ms = now;
        let t = t.clone();
        vec![record.task_update(&t, None)]
    }

    /// `by` tells `task`'s agent `text`, or the orchestrator when it is absent: kept on the
    /// timeline, and handed back trimmed for the hub to deliver. A task with no agent running
    /// has nobody to hear it.
    ///
    /// The orchestrator tells only its tasks, never itself, and never one that waits on the
    /// person: what it says must not pass for an answer to the person's permission or
    /// question. The person's word is theirs on a failure held past its give-backs: its count
    /// starts again. The timeline keeps who told.
    pub(crate) fn tell(
        &mut self,
        id: &ProjectId,
        (task, by): (Option<TaskId>, Teller),
        text: &str,
        terminals: &HashSet<TermRef>,
        now: WallMs,
    ) -> Result<(String, Vec<Change>), Refused> {
        let text = text.trim();
        if text.is_empty() {
            return Err(invalid("say something to the agent"));
        }
        within("what is said", Some(text), NOTE_MAX)?;
        let record = self.record(id)?;
        if let Some(task) = task {
            let t = record.task_mut(task)?;
            if by != Teller::Person && t.state == TaskState::Blocked {
                return Err(refuse(
                    ErrorCode::Conflict,
                    format!(
                        "task {task} waits on the person, for a permission or a question only \
                         they answer; tell it once it moves on"
                    ),
                ));
            }
        }
        if by == Teller::Orchestrator && task.is_none() {
            return Err(invalid("the orchestrator tells one of its tasks; name it"));
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
        let what = match by {
            Teller::Person => Moment::Told { text: text.to_owned() },
            Teller::Orchestrator => {
                Moment::Note { text: format!("The orchestrator told it: {text}") }
            }
        };
        let entry = record.log(task, what, now);
        let reset = task.filter(|_| by == Teller::Person).and_then(|task| {
            let t = record.task_mut(task).ok()?;
            (t.give_backs != GiveBacks::default()).then(|| {
                t.give_backs = GiveBacks::default();
                t.updated_ms = now;
                t.clone()
            })
        });
        let change = match reset {
            Some(t) => record.task_update(&t, Some(entry)),
            None => Change { kept: Kept { entry: Some(entry), ..record.kept() }, durable: true },
        };
        Ok((text.to_owned(), vec![change]))
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
    /// work that began or ended is written, so the time survives a restart and a server that
    /// comes back knows which turns were under way ([`Self::restore`]); the flips between are
    /// only pushed.
    pub(crate) fn agent_status(
        &mut self,
        term: TermRef,
        status: &AgentStatus,
        now: WallMs,
    ) -> Vec<Change> {
        let to = follows(status);
        let works = status.works();
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
            let durable = entry.is_some() || stretch.is_some();
            updates.push(Change { durable, ..record.task_update(&task, entry) });
        }
        self.turn(term, status);
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
        // A step the task has moved on to leaves nothing from before a restart to take up.
        self.restarted.remove(&(id.clone(), task));
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
            if step.commits.is_none() {
                step.commits.clone_from(&was.commits);
            }
        }
        // The commits a step works on are what it is taken up on after a restart: kept.
        let commits_came = step.commits.is_some()
            && t.step.as_ref().and_then(|s| s.commits.as_ref()) != step.commits.as_ref();
        t.step = Some(step.clone());
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = logged.then(|| record.log(Some(task), Moment::Step(step), now));
        Ok(vec![Change { durable: logged || commits_came, ..record.task_update(&task_now, entry) }])
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
        self.exited(term);
        self.forget_turn(term);
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

    /// `task`'s agent ended its turn without a word of its own ([`Upshot::Rested`]): the
    /// timeline says the task moved from running to waiting, which it keeps quiet for a turn
    /// that reported.
    pub(crate) fn rested(&mut self, id: &ProjectId, task: TaskId, now: WallMs) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Ok(t) = record.task(task).cloned() else { return Vec::new() };
        if t.state != TaskState::Waiting {
            return Vec::new();
        }
        let moved = Moment::State { from: TaskState::Running, to: TaskState::Waiting };
        let entry = record.log(Some(task), moved, now);
        vec![record.task_update(&t, Some(entry))]
    }

    /// The live terminals the server started for tasks that are [`finished`], or merged or
    /// given up with an agent that only keeps commands it `left_running` (a dev server), with
    /// their project and task: each closes once its agent has rested long enough.
    pub(crate) fn finished_agents(
        &self,
        terminals: &HashSet<TermRef>,
        left_running: impl Fn(TermRef) -> bool,
    ) -> Vec<(ProjectId, TaskId, TermRef)> {
        let over = |t: &Task| matches!(t.state, TaskState::Merged | TaskState::Failed);
        self.records
            .values()
            .flat_map(|r| {
                r.tasks.iter().filter(|t| over(t)).filter_map(|t| {
                    let a = t.assignment.as_ref().filter(|a| a.open() && a.spawned)?;
                    let rests = finished(t) || left_running(a.term);
                    (rests && terminals.contains(&a.term))
                        .then(|| (r.project.id.clone(), t.id, a.term))
                })
            })
            .collect()
    }

    /// Project `id`'s target `target` reached `origin` as it stood at `at`: every task merged
    /// into it by then whose card still says it was not pushed went with it, as a push takes
    /// the whole branch. Their cards say so, so what waits to be pushed is read off them: the
    /// merged tasks still not pushed.
    pub(crate) fn pushed_with(&mut self, id: &ProjectId, target: &str, at: WallMs) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let mut went = Vec::new();
        for t in &mut record.tasks {
            if let Some(Merge::Merged { target: into, at_ms, pushed, push_failed, .. }) =
                &mut t.merge
                && into == target
                && !*pushed
                && *at_ms <= at
            {
                *pushed = true;
                *push_failed = None;
                went.push(t.clone());
            }
        }
        went.iter().map(|t| record.task_update(t, None)).collect()
    }

    /// The server closes the agent of `task`, which is finished and rested `rested_mins`, with
    /// what it `left` running, which stops with it: the timeline says why, before the
    /// terminal's end says it is gone.
    pub(crate) fn settled(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        (rested_mins, left): (u64, Option<String>),
        now: WallMs,
    ) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Ok(t) = record.task(task).cloned() else { return Vec::new() };
        let over = if t.state == TaskState::Merged { "merged" } else { "given up" };
        let mut text = format!(
            "The server closed its agent, at rest {rested_mins} min after the task was {over}; \
             its session can be taken up again."
        );
        if let Some(left) = left.map(|l| clipped(l.trim(), SUMMARY_MAX)).filter(|l| !l.is_empty()) {
            text = format!("{text} It stopped what the agent left running: {left}.");
        }
        let entry = record.log(Some(task), Moment::Note { text }, now);
        vec![record.task_update(&t, Some(entry))]
    }

    /// What frees a settled task's worktree once its agent is closed: the worktree its agent
    /// reported, and where its work landed (the merge queue's head, the target, the target on
    /// `origin`). Only a merged task's: one given up may be tried again, and its agent would
    /// remake a worktree gone, its branch reset to the base with it.
    pub(crate) fn take_free(
        &mut self,
        id: &ProjectId,
        task: TaskId,
    ) -> Option<(String, Vec<String>)> {
        let record = self.records.get(id)?;
        let t = record.task(task).ok().filter(|t| t.state == TaskState::Merged)?;
        let free = (t.worktree.clone()?, landed_of(record, t));
        let at = (id.clone(), task);
        (!self.kept.contains(&at) && self.freeing.insert(at)).then_some(free)
    }

    /// Freeing `task`'s worktree did not get as far as its worker's answer: its agent's
    /// terminal did not close, or the worker went away. It is asked again, by the settle loop
    /// or once the worker is back ([`Self::unfreed`]).
    pub(crate) fn unfree(&mut self, id: &ProjectId, task: TaskId) {
        self.freeing.remove(&(id.clone(), task));
    }

    /// The merged tasks on `worker` whose worktree is still there though their agent's
    /// terminal is no longer open (`terminals`): the person closed it, or it ended while the
    /// server was away, so the settle loop never closes it ([`Self::finished_agents`]). Each
    /// with its project, its terminal, its worktree and where its work landed, once.
    pub(crate) fn unfreed(
        &mut self,
        worker: WorkerId,
        terminals: &HashSet<TermRef>,
    ) -> Vec<(ProjectId, TaskId, TermRef, String, Vec<String>)> {
        let mut found = Vec::new();
        for record in self.records.values() {
            for t in record.tasks.iter().filter(|t| t.state == TaskState::Merged) {
                let (Some(worktree), Some(a)) = (&t.worktree, &t.assignment) else { continue };
                if a.term.worker != worker || terminals.contains(&a.term) {
                    continue;
                }
                let free = (record.project.id.clone(), t.id, a.term, worktree.clone());
                found.push((free, landed_of(record, t)));
            }
        }
        found
            .into_iter()
            .filter(|((id, task, ..), _)| {
                let at = (id.clone(), *task);
                !self.kept.contains(&at) && self.freeing.insert(at)
            })
            .map(|((id, task, term, worktree), landed)| (id, task, term, worktree, landed))
            .collect()
    }

    /// What letting project `id` go leaves on the workers: each task's worktree its agent
    /// reported, on the worker it ran on, with where its work would have landed, but those being
    /// freed already; one its worker kept before is asked again. Freeing one keeps anything not
    /// committed in it, and the branch of work that did not land.
    pub(crate) fn worktrees_of(&self, id: &ProjectId) -> Vec<(WorkerId, String, Vec<String>)> {
        let Some(record) = self.records.get(id) else { return Vec::new() };
        record
            .tasks
            .iter()
            .filter(|t| !self.freeing.contains(&(id.clone(), t.id)))
            .filter_map(|t| {
                let (worktree, a) = (t.worktree.clone()?, t.assignment.as_ref()?);
                Some((a.term.worker, worktree, landed_of(record, t)))
            })
            .collect()
    }

    /// What became of freeing `task`'s worktree `worktree`: gone, with whether its branch went
    /// too, or kept and why. A worktree gone leaves the card; the timeline says either way.
    pub(crate) fn freed(
        &mut self,
        id: &ProjectId,
        (task, worktree): (TaskId, &str),
        went: Result<(Option<String>, bool), String>,
        now: WallMs,
    ) -> Vec<Change> {
        // One kept is not asked again on its own: the person frees it from the worktree list.
        let at = (id.clone(), task);
        self.freeing.remove(&at);
        if went.is_err() {
            self.kept.insert(at);
        } else {
            self.kept.remove(&at);
        }
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Ok(t) = record.task_mut(task) else { return Vec::new() };
        let text = match went {
            Ok((branch, removed)) => {
                if t.worktree.as_deref() == Some(worktree) {
                    t.worktree = None;
                }
                match (branch, removed) {
                    (Some(branch), true) => format!(
                        "The server removed its worktree, and its branch {branch}, whose work all \
                         landed."
                    ),
                    (Some(branch), false) => format!(
                        "The server removed its worktree; its branch {branch} is kept, with \
                         work that did not land."
                    ),
                    (None, _) => "The server removed its worktree.".to_owned(),
                }
            }
            Err(why) => format!("Its worktree is kept: {}.", why.trim_end_matches('.')),
        };
        let t = t.clone();
        let entry = record.log(Some(task), Moment::Note { text }, now);
        vec![record.task_update(&t, Some(entry))]
    }

    /// The machine of worker `worker`, named `name`, went away or came back. Every task whose
    /// agent works there (an open assignment, its work not over) says so on its timeline;
    /// going away stops its clock, as the agent's work cannot be seen. Returns the changes, and
    /// the tasks told, for their orchestrators to hear of. Back, only the tasks told it went
    /// away are told.
    pub(crate) fn machine_seen(
        &mut self,
        worker: WorkerId,
        name: &str,
        seen: Seen,
        now: WallMs,
    ) -> (Vec<Change>, Vec<(ProjectId, TaskId)>) {
        let (mut updates, mut told) = (Vec::new(), Vec::new());
        for record in self.records.values_mut() {
            let id = record.project.id.clone();
            let on: Vec<TaskId> = record
                .tasks
                .iter()
                .filter(|t| !finished(t) && open_term(t).is_some_and(|term| term.worker == worker))
                .map(|t| t.id)
                .collect();
            for task in on {
                let text = match seen {
                    Seen::Away if self.away.insert((id.clone(), task)) => format!(
                        "Its machine {name} stopped answering. Its agent's work waits for it to \
                         come back, or the task can be started again elsewhere."
                    ),
                    Seen::Back if self.away.remove(&(id.clone(), task)) => {
                        format!("Its machine {name} answers again.")
                    }
                    Seen::Away | Seen::Back => continue,
                };
                let Ok(t) = record.task_mut(task) else { continue };
                if seen == Seen::Away {
                    t.spent.follow(false, now);
                }
                t.updated_ms = now;
                let t = t.clone();
                let entry = record.log(Some(task), Moment::Note { text }, now);
                updates.push(record.task_update(&t, Some(entry)));
                told.push((id.clone(), task));
            }
        }
        (updates, told)
    }

    /// A worker registered with `sessions` open: every assignment on it to a terminal it no
    /// longer has ended while the server was away. A thread's is judged by the worker's
    /// thread table instead ([`Self::threads_on`]), since its seat need be no terminal.
    pub(crate) fn reconcile(
        &mut self,
        worker: WorkerId,
        sessions: &[SessionId],
        now: WallMs,
    ) -> Vec<Change> {
        let gone: HashSet<TermRef> = self
            .records
            .values()
            .flat_map(|r| r.tasks.iter())
            .filter(|t| t.assignment.as_ref().is_some_and(|a| a.thread.is_none()))
            .filter_map(open_term)
            .filter(|t| t.worker == worker && !sessions.contains(&t.session))
            .collect();
        gone.into_iter().flat_map(|term| self.session_ended(term, now)).collect()
    }

    /// The seat `session` of a task's thread still assigned, whatever its worker's table says
    /// yet.
    pub(crate) fn thread_seat(&self, session: SessionId) -> Option<TermRef> {
        self.records
            .values()
            .flat_map(|r| r.tasks.iter())
            .filter_map(|t| t.assignment.as_ref().filter(|a| a.open() && a.thread.is_some()))
            .find(|a| a.term.session == session)
            .map(|a| a.term)
    }

    /// Every open assignment on `worker` to a thread: its seat, the thread, and since when.
    pub(crate) fn threads_on(&self, worker: WorkerId) -> Vec<(TermRef, ThreadId, WallMs)> {
        self.records
            .values()
            .flat_map(|r| r.tasks.iter())
            .filter_map(|t| t.assignment.as_ref().filter(|a| a.open()))
            .filter(|a| a.term.worker == worker)
            .filter_map(|a| Some((a.term, a.thread?, a.since_ms)))
            .collect()
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

/// `text` cut to at most `max` bytes, at a character boundary: what a worker reports is kept
/// to the bounds a card is held to.
pub(crate) fn clipped(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// What a report did to a node.
enum Took {
    /// Nothing changed.
    Unchanged,
    /// The node changed, with the moment worth the timeline, if any.
    Changed(Option<Moment>),
}

/// Whether what a task that others depend on made is there for them: its work merged into
/// the target, where a dependent's worktree starts from; or, for one that only reads, its
/// agent done. A task done and not merged has work no other clone holds yet.
const fn delivered(t: &Task) -> bool {
    matches!(t.state, TaskState::Merged) || t.read_only && matches!(t.state, TaskState::Done)
}

/// Take a status line's worktree into `t`; a new branch is worth the timeline.
fn take_branch(t: &mut Task, branch: &AgentBranch) -> Took {
    let worktree = branch.worktree.as_ref().map(|w| clipped(&w.path, REF_MAX));
    let named = branch
        .worktree
        .as_ref()
        .and_then(|w| w.branch.as_deref().map(|b| clipped(b, REF_MAX)))
        .or_else(|| t.branch.clone());
    if t.worktree == worktree && t.branch == named {
        return Took::Unchanged;
    }
    let moved = t.branch != named;
    t.worktree = worktree;
    t.branch = named;
    Took::Changed(moved.then(|| Moment::Branch { branch: t.branch.clone() }))
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

/// `t`, waiting in its pull request ([`Merge::Pull`]), as `pull` (its thread's) stands now:
/// merged there, it is merged; closed without a merge, it waits for the person's Merge again.
/// The move, for the timeline.
fn landed(t: &mut Task, pull: Option<&PullSeen>, now: WallMs) -> Option<Moment> {
    let Some(Merge::Pull { target, head, from, number, .. }) = &t.merge else { return None };
    let pull = pull.filter(|p| p.number == *number)?;
    match pull.stands {
        PullStands::Merged if t.state.may_become(TaskState::Merged) => {
            let merged = Merge::Merged {
                target: target.clone(),
                head: head.clone(),
                from: from.clone(),
                at_ms: now,
                pushed: true,
                push_failed: None,
            };
            let from = t.state;
            t.merge = Some(merged);
            t.state = TaskState::Merged;
            Some(Moment::State { from, to: TaskState::Merged })
        }
        PullStands::Closed => {
            t.merge = None;
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod cost;
mod merge;
mod turns;
pub(crate) use merge::{Advance, Job, Queue};
pub(crate) use turns::{Heard, Upshot};
#[cfg(test)]
mod tests;
