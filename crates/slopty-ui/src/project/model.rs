//! The server's projects as this client mirrors them, and what the board derives from one.
//!
//! The server sends every project after a link comes up, in parts
//! ([`ProjectsPart`]), then each change as a [`ProjectUpdate`] numbered in its event log. A part
//! marked `first` replaces what was here. An update at or below the snapshot's `seq` is already
//! in it and is dropped, since a replay after a lag would otherwise put older state over newer.
//!
//! Everything the board draws is derived here, pure: the lanes that answer "what needs me"
//! ([`Board::lanes`]), the dependencies still open ([`Board::waiting_on`]), each task's way to
//! the target ([`Board::pipeline`]) and what the person can do to it ([`Board::actions`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::git::Forge;
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Merge, Native, NativeChange, NativeCounts, Natives, Project, ProjectId, ProjectStatus,
    ProjectUpdate, ProjectsPart, StepKind, StepState, Task, TaskCard, TaskId, TaskState, TaskStep,
    TimelineEntry, VerifierRun,
};
use slopty_proto::thread::wire::{PullSeen, PullStands};

/// `card`'s pull request as a row says it, and whether it holds the merge back: a check failed,
/// changes were asked for, or it conflicts.
#[must_use]
pub fn pull_words(card: &TaskCard) -> Option<(String, bool)> {
    let pull = card.pull.as_ref()?;
    Some((format!("{} {}", short(pull.forge), pull.line()), pull.stands.needs_you()))
}

/// What a request is called on a chip: "PR", a merge request's "MR".
const fn short(forge: Forge) -> &'static str {
    match forge {
        Forge::GitHub => "PR",
        Forge::GitLab => "MR",
    }
}

/// Whether a pull request still open has a failed check: CI to fix.
const fn checks_fail(pull: &PullSeen) -> bool {
    pull.failed > 0 && !matches!(pull.stands, PullStands::Merged | PullStands::Closed)
}

/// Where `merge` put a task, as its row says it; `None` until it merged.
///
/// A push asked for that failed left the target moved here but not on `origin`, and the row
/// says so in git's first line unless `piped`, where its pipeline's Push stage says it.
#[must_use]
pub fn merged_words(merge: &Merge, piped: bool) -> Option<String> {
    let Merge::Merged { target, head, pushed, push_failed, .. } = merge else { return None };
    let at = format!("into {target} at {}", short_commit(head));
    Some(match push_failed {
        Some(why) if !piped => format!("{at}, push failed: {}", crate::kit::first_line(why)),
        None if *pushed => format!("{at}, pushed"),
        Some(_) | None => at,
    })
}

/// A pull request's checks as its row says them, in neutral words, and whether one failed;
/// none while none failed or runs.
fn checks_words(pull: &PullSeen) -> Option<(String, bool)> {
    let plural = |n: u32| if n == 1 { "check" } else { "checks" };
    if checks_fail(pull) {
        let words = match (&pull.failed_first, pull.failed) {
            (Some(first), 1) => format!("{first} failed"),
            (Some(first), n) => format!("{first} and {} more failed", n.saturating_sub(1)),
            (None, n) => format!("{n} {} failed", plural(n)),
        };
        return Some((words, true));
    }
    let running = pull.running;
    (running > 0).then(|| (format!("{running} {} running", plural(running)), false))
}

/// How many timeline entries a board keeps: a screenful many times over, and a bound on a
/// project the client watches for days.
pub const TIMELINE_KEPT: usize = 256;

/// How many of Claude Code's own subagents and to-dos a node keeps, as the server does.
pub const NATIVES_KEPT: usize = 256;

/// Every project the server has, as last heard.
#[derive(Clone, Debug, Default)]
pub struct Projects {
    /// The last event the snapshot holds.
    seq: u64,
    /// Shared with the boards on show: a change copies the one it touches, so handing a board
    /// its project is a pointer, and an unchanged one compares by address.
    boards: BTreeMap<ProjectId, Arc<Board>>,
}

impl Projects {
    /// Take one part of a snapshot. The first part replaces everything; a later one adds the
    /// tasks of a project it carries again. Returns the projects it touched.
    pub fn apply_part(&mut self, part: ProjectsPart) -> Vec<ProjectId> {
        let mut touched: Vec<ProjectId> = Vec::new();
        if part.first {
            touched.extend(self.boards.keys().cloned());
            self.boards.clear();
        }
        self.seq = part.seq;
        for status in part.projects {
            let id = status.project.id.clone();
            if let Some(board) = self.boards.get_mut(&id) {
                Arc::make_mut(board).extend(status);
            } else {
                self.boards.insert(id.clone(), Arc::new(Board::from_status(status)));
            }
            if !touched.contains(&id) {
                touched.push(id);
            }
        }
        touched
    }

    /// Take one change numbered `seq` in the server's log. `None` when the snapshot already
    /// holds it, or it names a project never heard of without saying what it is.
    pub fn apply_update(&mut self, seq: u64, update: ProjectUpdate) -> Option<ProjectId> {
        if seq <= self.seq {
            return None;
        }
        let id = update.project.clone();
        if let Some(board) = self.boards.get_mut(&id) {
            Arc::make_mut(board).apply(update);
        } else {
            let mut board = Board::new(update.record.clone()?);
            board.apply(update);
            self.boards.insert(id.clone(), Arc::new(board));
        }
        Some(id)
    }

    /// Forget everything: the server was let go, or another one is used.
    pub fn clear(&mut self) {
        self.seq = 0;
        self.boards.clear();
    }

    /// One project.
    #[must_use]
    pub fn get(&self, id: &ProjectId) -> Option<&Arc<Board>> {
        self.boards.get(id)
    }

    /// Every project, by name.
    pub fn boards(&self) -> impl Iterator<Item = &Board> {
        self.boards.values().map(AsRef::as_ref)
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.boards.is_empty()
    }

    /// The project whose orchestrator runs in `session`.
    #[must_use]
    pub fn of_orchestrator(&self, session: SessionId) -> Option<&Board> {
        self.boards().find(|b| b.project.orchestrator.is_some_and(|t| t.session == session))
    }

    /// The project and task `session` works on, while its terminal is on the task.
    #[must_use]
    pub fn of_agent(&self, session: SessionId) -> Option<(&Board, TaskId)> {
        self.boards().find_map(|board| Some((board, board.task_of(session)?)))
    }
}

/// Where a task stands on the board: the lanes, left to right.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Lane {
    /// An agent waits on the person: a permission, a question.
    NeedsYou,
    /// Given up; the orchestrator or the person decides what next.
    Failed,
    /// An agent is on it.
    Working,
    /// Made, and nothing runs for it yet.
    UpNext,
    /// Its verifier runs.
    Verifying,
    /// Its verifier passed; it waits for the merge.
    ReadyToMerge,
    /// On the target branch.
    Merged,
}

impl Lane {
    /// Every lane, left to right, the most urgent first.
    pub const ALL: [Self; 7] = [
        Self::NeedsYou,
        Self::Failed,
        Self::Working,
        Self::UpNext,
        Self::Verifying,
        Self::ReadyToMerge,
        Self::Merged,
    ];

    /// The lane a task in `state` is in on its own.
    #[must_use]
    pub const fn of(state: TaskState) -> Self {
        match state {
            TaskState::Blocked => Self::NeedsYou,
            TaskState::Failed => Self::Failed,
            TaskState::Running | TaskState::Waiting => Self::Working,
            TaskState::Planned => Self::UpNext,
            TaskState::Verifying => Self::Verifying,
            TaskState::Done => Self::ReadyToMerge,
            TaskState::Merged => Self::Merged,
        }
    }

    /// Its heading.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Failed => "Failed",
            Self::Working => "Working",
            Self::UpNext => "Up next",
            Self::Verifying => "Verifying",
            Self::ReadyToMerge => "Ready to merge",
            Self::Merged => "Merged",
        }
    }
}

/// What the person does to a task from its card or row ([`Board::actions`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TaskAction {
    /// Show its whole branch against the project's target, before it merges.
    Review,
    /// Ask for its merge.
    Merge,
    /// Check it again from the start, and merge it if it passes.
    Retry,
    /// Start a task not started yet with an agent of the person's, on its pin when it has one
    /// ([`slopty_proto::orchestration::Verb::TaskSpawn`]): the orchestrator need not be there.
    Start,
    /// Tell its agent, as the person, to make its failed verifier pass.
    FixCi,
    /// Tell its agent, as the person, to address what its pull request's review asked for.
    AddressComments,
    /// Tell its agent, as the person, to resolve the conflicts its rebase met.
    ResolveConflicts,
    /// Push its target to the forge: a merge the project did not push (pushing is off unless the
    /// person turns it on), or one whose push failed. The target goes as it is, so every merged
    /// task not pushed yet is pushed with it. The board's head offers it, never a card.
    Push,
    /// Give it up: it holds its paths no more and leaves the merge queue, and it may be
    /// planned again. Its agent, if one runs, is left to the person.
    Cancel,
}

impl TaskAction {
    /// Its button's word.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Review => "Review",
            Self::Merge => "Merge",
            Self::Retry => "Retry",
            Self::Start => "Start",
            Self::FixCi => "Fix CI",
            Self::AddressComments => "Address comments",
            Self::ResolveConflicts => "Resolve conflicts",
            Self::Push => "Push",
            Self::Cancel => "Cancel task",
        }
    }

    /// Whether it is the person's words to the task's agent, rather than a word to the server.
    #[must_use]
    pub const fn tells(self) -> bool {
        matches!(self, Self::FixCi | Self::AddressComments | Self::ResolveConflicts)
    }

    /// Its button's element name, for `task` on the row or card named by `prefix`.
    #[must_use]
    pub fn selector(self, prefix: &str, task: TaskId) -> String {
        let word = match self {
            Self::Review => "review",
            Self::Merge => "merge",
            Self::Retry => "retry",
            Self::Start => "start",
            Self::FixCi => "fix-ci",
            Self::AddressComments => "address-comments",
            Self::ResolveConflicts => "resolve-conflicts",
            Self::Push => "push",
            Self::Cancel => "cancel",
        };
        format!("{prefix}-{word}-{task}")
    }
}

/// Where a node is, as its row's map chip says it ([`Board::place`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The worker.
    pub worker: WorkerId,
    /// How it is there.
    pub how: PlaceHow,
    /// The worktree its work is in, when its agent said.
    pub worktree: Option<String>,
    /// The branch its work is on.
    pub branch: Option<String>,
    /// Why it went there, when it was named: "pinned".
    pub why: Option<String>,
}

/// How a node is on its worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceHow {
    /// Its agent runs there.
    Runs,
    /// Its agent ran there last.
    Ran,
    /// It is pinned there and has not started.
    Pinned,
}

/// A system's name as the board says it.
#[must_use]
pub const fn os_name(os: slopty_proto::server::Os) -> &'static str {
    match os {
        slopty_proto::server::Os::MacOs => "macOS",
        slopty_proto::server::Os::Linux => "Linux",
    }
}

/// One stage of a task's way to the target, as its pipeline row says it ([`Board::pipeline`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage {
    /// Which.
    pub kind: StageKind,
    /// What it says.
    pub words: String,
    /// It holds the merge back until someone acts.
    pub holds: bool,
    /// What it ran failed (the verifier, the pull request's checks, the push), so it says so
    /// in the error ink with its mark. Such a stage always holds.
    pub failed: bool,
}

/// The stages a pipeline row names, in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StageKind {
    /// The branch the work is on.
    Branch,
    /// The project's verifier.
    Verifier,
    /// The merge queue.
    Queue,
    /// The pull request.
    Pull,
    /// Its own checks, as its forge says them.
    Checks,
    /// Its review on the forge, when it asks for changes.
    PullReview,
    /// The agent's own task list.
    ToDos,
    /// The push to `origin` after it merged, when that failed.
    Push,
}

impl StageKind {
    /// The word its element name ends in.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Branch => "branch",
            Self::Verifier => "verifier",
            Self::Queue => "queue",
            Self::Pull => "pull",
            Self::Checks => "checks",
            Self::PullReview => "pull-review",
            Self::ToDos => "todos",
            Self::Push => "push",
        }
    }
}

/// A node of the tree: a task, or the orchestrator for `None`.
pub type Node = Option<TaskId>;

/// One project in full, as the board draws it.
#[derive(Clone, PartialEq, Debug)]
pub struct Board {
    /// The record.
    pub project: Project,
    /// Every task's card, by number.
    pub tasks: BTreeMap<TaskId, TaskCard>,
    /// How many natives the orchestrator's node holds.
    pub orchestrator_natives: NativeCounts,
    /// The native leaves heard of since the snapshot, per node (`None` is the orchestrator's).
    pub natives: BTreeMap<Option<TaskId>, Natives>,
    /// The latest of its timeline, oldest first, at most [`TIMELINE_KEPT`].
    pub timeline: VecDeque<TimelineEntry>,
}

impl Board {
    fn new(project: Project) -> Self {
        Self {
            project,
            tasks: BTreeMap::new(),
            orchestrator_natives: NativeCounts::default(),
            natives: BTreeMap::new(),
            timeline: VecDeque::new(),
        }
    }

    fn from_status(status: ProjectStatus) -> Self {
        let mut board = Self::new(status.project.clone());
        board.extend(status);
        board
    }

    /// A snapshot part's record, cards and timeline over what is here.
    fn extend(&mut self, status: ProjectStatus) {
        self.project = status.project;
        self.orchestrator_natives = status.orchestrator_natives;
        self.tasks.extend(status.tasks.into_iter().map(|card| (card.id, card)));
        for entry in status.timeline {
            self.push_entry(entry);
        }
    }

    fn apply(&mut self, update: ProjectUpdate) {
        if let Some(record) = update.record {
            self.project = record;
        }
        if let Some(card) = update.task {
            self.tasks.insert(card.id, card);
        }
        if let Some(change) = update.native {
            self.apply_native(change);
        }
        if let Some(entry) = update.entry {
            self.push_entry(entry);
        }
    }

    /// An entry at the end, once: a part and an update may both carry it.
    fn push_entry(&mut self, entry: TimelineEntry) {
        if self.timeline.back().is_some_and(|last| last.seq >= entry.seq) {
            if self.timeline.iter().all(|e| e.seq != entry.seq) {
                let at = self.timeline.partition_point(|e| e.seq < entry.seq);
                self.timeline.insert(at, entry);
            }
        } else {
            self.timeline.push_back(entry);
        }
        while self.timeline.len() > TIMELINE_KEPT {
            self.timeline.pop_front();
        }
    }

    /// A native leaf as it is now. The counts on its node move with it: a node's card is not
    /// sent again when only one of its natives changed.
    fn apply_native(&mut self, change: NativeChange) {
        let counts = match change.task {
            None => &mut self.orchestrator_natives,
            Some(task) => match self.tasks.get_mut(&task) {
                Some(card) => &mut card.natives,
                None => return,
            },
        };
        let node = self.natives.entry(change.task).or_default();
        match change.native {
            Native::Agent(agent) => {
                let known = node.agents.iter_mut().find(|a| a.id == agent.id);
                let was_running = known.as_ref().map(|a| a.stopped_ms.is_none());
                let running = agent.stopped_ms.is_none();
                match (was_running, running) {
                    (None, true) => {
                        counts.agents = counts.agents.saturating_add(1);
                        counts.running = counts.running.saturating_add(1);
                    }
                    // Started before the snapshot, which counted it running.
                    (None | Some(true), false) => {
                        counts.running = counts.running.saturating_sub(1);
                    }
                    (Some(false), true) => counts.running = counts.running.saturating_add(1),
                    (Some(true), true) | (Some(false), false) => {}
                }
                match known {
                    Some(slot) => *slot = agent,
                    None => push_bounded(&mut node.agents, agent),
                }
            }
            Native::Todo(todo) => {
                let known = node.tasks.iter_mut().find(|t| t.id == todo.id);
                match (known.as_ref().map(|t| t.done), todo.done) {
                    (None, false) => counts.todos = counts.todos.saturating_add(1),
                    // Made before the snapshot, which counted it open.
                    (None | Some(false), true) => {
                        counts.done = counts.done.saturating_add(1);
                    }
                    (Some(true), false) => counts.done = counts.done.saturating_sub(1),
                    (Some(_), _) => {}
                }
                match known {
                    Some(slot) => *slot = todo,
                    None => push_bounded(&mut node.tasks, todo),
                }
            }
        }
    }

    /// The task whose live terminal is `session`.
    #[must_use]
    pub fn task_of(&self, session: SessionId) -> Option<TaskId> {
        self.tasks.values().find_map(|card| {
            let a = card.assignment.as_ref()?;
            (a.term.session == session && a.open()).then_some(card.id)
        })
    }

    /// Who works in `term`, in words: the orchestrator, a task's agent, or an agent this project
    /// no longer names.
    #[must_use]
    pub fn agent_at(&self, term: TermRef) -> String {
        if self.project.orchestrator == Some(term) {
            return "the orchestrator".to_owned();
        }
        let task =
            self.tasks.values().find(|c| c.assignment.as_ref().is_some_and(|a| a.term == term));
        task.map_or_else(|| "an agent".to_owned(), |c| format!("#{}'s agent", c.id))
    }

    /// The live terminal of a node: the task's, or the orchestrator's for `None`.
    #[must_use]
    pub fn terminal(&self, node: Option<TaskId>) -> Option<(WorkerId, SessionId)> {
        match node {
            None => self.project.orchestrator.map(|t| (t.worker, t.session)),
            Some(task) => {
                let a = self.tasks.get(&task)?.assignment.as_ref()?;
                a.open().then_some((a.term.worker, a.term.session))
            }
        }
    }

    /// The worktree `task`'s work is in, with the machine it is on: where its review reads
    /// its branch, whether or not its agent still runs.
    #[must_use]
    pub fn worktree(&self, task: TaskId) -> Option<(WorkerId, String)> {
        let card = self.tasks.get(&task)?;
        let path = card.worktree.as_ref().filter(|p| !p.trim().is_empty())?;
        Some((card.assignment.as_ref()?.term.worker, path.clone()))
    }

    /// Where `node` runs, ran last, is pinned to or would start, with the worktree and branch
    /// its work is in and why it went there: the board's map of the fleet, row by row.
    #[must_use]
    pub fn place(&self, node: Node) -> Option<Place> {
        let Some(task) = node else {
            let worker = self.project.orchestrator?.worker;
            return Some(Place {
                worker,
                how: PlaceHow::Runs,
                worktree: None,
                branch: None,
                why: None,
            });
        };
        let card = self.tasks.get(&task)?;
        let (worktree, branch) = (card.worktree.clone(), card.branch.clone());
        if let Some(a) = &card.assignment {
            let how = if a.open() { PlaceHow::Runs } else { PlaceHow::Ran };
            return Some(Place { worker: a.term.worker, how, worktree, branch, why: None });
        }
        let why = Some("pinned".to_owned());
        Some(Place { worker: card.pin?, how: PlaceHow::Pinned, worktree, branch, why })
    }

    /// The worker a node runs on, or ran on last.
    #[must_use]
    pub fn worker(&self, node: Option<TaskId>) -> Option<WorkerId> {
        match node {
            None => self.project.orchestrator.map(|t| t.worker),
            Some(task) => self.tasks.get(&task)?.assignment.as_ref().map(|a| a.term.worker),
        }
    }

    /// The lane a task is in on the board: its state's.
    #[must_use]
    pub fn lane(&self, task: TaskId) -> Option<Lane> {
        Some(Lane::of(self.tasks.get(&task)?.state))
    }

    /// The board: each lane that holds a task, left to right. Ready to merge is the merge
    /// queue, so it runs in the queue's order, and Verifying puts the run under way first and
    /// the rest in the order the server takes them. A done task the queue does not hold comes
    /// after those it does. Every other lane goes by number.
    #[must_use]
    pub fn lanes(&self) -> Vec<(Lane, Vec<TaskId>)> {
        let mut by_lane: BTreeMap<Lane, Vec<TaskId>> = BTreeMap::new();
        for id in self.tasks.keys() {
            if let Some(lane) = self.lane(*id) {
                by_lane.entry(lane).or_default().push(*id);
            }
        }
        for (lane, tasks) in &mut by_lane {
            tasks.sort_by_key(|id| {
                let turn = self.turn(*lane, *id);
                (turn.is_none(), turn, *id)
            });
        }
        by_lane.into_iter().collect()
    }

    /// Where `task` stands in `lane`'s order: `None` goes after every task that has a turn.
    fn turn(&self, lane: Lane, task: TaskId) -> Option<(bool, WallMs)> {
        let card = self.tasks.get(&task).filter(|c| Lane::of(c.state) == lane)?;
        match lane {
            // The queue merges the one waiting longest, so it is also the one merging.
            Lane::ReadyToMerge => Some((false, card.merge.as_ref()?.queued()?)),
            Lane::Verifying => {
                let running = card.step.as_ref().is_some_and(TaskStep::running);
                Some((!running, card.updated_ms))
            }
            _ => None,
        }
    }

    /// Where `task` waits in the merge queue: its place from 1, and how many wait.
    #[must_use]
    pub fn queue_place(&self, task: TaskId) -> Option<(usize, usize)> {
        let mut queue: Vec<(WallMs, TaskId)> = self
            .tasks
            .values()
            .filter(|c| c.state == TaskState::Done)
            .filter_map(|c| Some((c.merge.as_ref()?.queued()?, c.id)))
            .collect();
        queue.sort_unstable();
        let at = queue.iter().position(|(_, id)| *id == task)?;
        Some((at.saturating_add(1), queue.len()))
    }

    /// The verifier's last word on `task` while it still speaks to what the task is now: a
    /// pass while the task waits to merge, a failure until it is verified again or merged.
    #[must_use]
    pub fn verdict(&self, task: TaskId) -> Option<&VerifierRun> {
        let card = self.tasks.get(&task)?;
        let run = card.verified.as_ref()?;
        let speaks = if run.passed {
            card.state == TaskState::Done
        } else {
            !matches!(card.state, TaskState::Merged | TaskState::Verifying)
        };
        speaks.then_some(run)
    }

    /// What the person can do to `task` from the board, besides opening its agent:
    /// - review a finished task's whole branch against the target, then merge it, when the queue
    ///   does not hold it, as no check queued it;
    /// - retry what failed on its way to the target (its branch home, its verifier, its merge),
    ///   which checks it afresh.
    ///
    /// While its agent runs, the next step is the person's word to it, which comes first:
    /// - fix CI when its verifier's failure still speaks;
    /// - address the comments when its pull request's review asked for changes;
    /// - resolve the conflicts its rebase onto the target met.
    ///
    /// Checking a failure again unchanged would fail the same way, so a failed verifier or
    /// rebase offers Retry only when no agent runs to fix it. A task that only reads has
    /// nothing to merge, and a merged one nothing at all: a merge not on the forge is pushed
    /// from the board's head, for every such task at once.
    #[must_use]
    pub fn actions(&self, task: TaskId) -> Vec<TaskAction> {
        let Some(card) = self.tasks.get(&task) else { return Vec::new() };
        if card.state == TaskState::Merged || card.read_only {
            return Vec::new();
        }
        let failed = card.step.as_ref().filter(|s| matches!(s.state, StepState::Failed { .. }));
        let live = self.terminal(Some(task)).is_some();
        let mut out = Vec::new();
        let checks_fail = card.pull.as_ref().is_some_and(checks_fail);
        let fix_ci = live && (self.verdict(task).is_some_and(|r| !r.passed) || checks_fail);
        if fix_ci {
            out.push(TaskAction::FixCi);
        }
        let review_asked =
            card.pull.as_ref().is_some_and(|p| p.stands == PullStands::ChangesRequested);
        if live && review_asked {
            out.push(TaskAction::AddressComments);
        }
        let conflicted = failed.is_some_and(|s| s.kind == StepKind::Rebase);
        if live && conflicted {
            out.push(TaskAction::ResolveConflicts);
        }
        if card.state == TaskState::Done && card.merge.is_none() {
            // What merges is shown first, whether or not its agent still runs.
            if self.worktree(task).is_some() {
                out.push(TaskAction::Review);
            }
            out.push(TaskAction::Merge);
        }
        let retried = |kind: StepKind| match kind {
            StepKind::Home | StepKind::Merge => true,
            StepKind::Verify | StepKind::Rebase => !live,
            // A failed send is a start's: the card offers Start, not Retry.
            StepKind::Clone | StepKind::Send => false,
        };
        if failed.is_some_and(|s| retried(s.kind)) && !fix_ci {
            out.push(TaskAction::Retry);
        }
        if self.not_started(card) {
            out.push(TaskAction::Start);
        }
        out
    }

    /// What the person can do to `task` beyond what moves it on ([`Self::actions`]): cancel it
    /// until it is merged or given up. Rare, and never what the task waits for, so the board
    /// offers it only on the task it stands on. Stopping, restarting and moving a task's agent
    /// are the orchestrator's (`task_start`, `task_restart`).
    #[must_use]
    pub fn controls(&self, task: TaskId) -> Vec<TaskAction> {
        let Some(card) = self.tasks.get(&task) else { return Vec::new() };
        let open = !matches!(card.state, TaskState::Merged | TaskState::Failed);
        if open { vec![TaskAction::Cancel] } else { Vec::new() }
    }

    /// What the person says to `task`'s agent for a next step, in their words: what failed or
    /// what was asked, from what the board knows, and what to do about it.
    #[must_use]
    pub fn told(&self, task: TaskId, action: TaskAction) -> Option<String> {
        let card = self.tasks.get(&task)?;
        let target = &self.project.target;
        let then = "commit, and report done again with task_report.";
        match action {
            TaskAction::FixCi => {
                let mut failed = Vec::new();
                if let Some(run) = self.verdict(task).filter(|r| !r.passed) {
                    let command = self.project.verifier.as_deref().unwrap_or("The verifier");
                    let summary = crate::kit::first_line(&run.summary);
                    let what =
                        if summary.is_empty() { String::new() } else { format!(": {summary}") };
                    failed.push(format!(
                        "`{command}` failed on your work at {}{what}.",
                        short_commit(&run.head)
                    ));
                }
                if let Some(pull) = card.pull.as_ref().filter(|p| checks_fail(p)) {
                    let (which, _) = checks_words(pull).unwrap_or_default();
                    let shows = match pull.forge {
                        Forge::GitHub => format!("gh pr checks {}", pull.number),
                        Forge::GitLab => format!("glab mr view {}", pull.number),
                    };
                    failed.push(format!(
                        "{} {}: {which}. `{shows}` shows them.",
                        pull.forge.title(),
                        pull.named()
                    ));
                }
                if failed.is_empty() {
                    return None;
                }
                Some(format!("Fix CI. {} Make it pass, then {then}", failed.join(" ")))
            }
            TaskAction::AddressComments => {
                let mut lines = vec!["Address the review's comments.".to_owned()];
                if let Some(pull) =
                    card.pull.as_ref().filter(|p| p.stands == PullStands::ChangesRequested)
                {
                    lines.push(format!(
                        "{} {} has changes requested: {}",
                        pull.forge.title(),
                        pull.named(),
                        pull.url
                    ));
                }
                lines.push(format!("Then {then}"));
                Some(lines.join("\n"))
            }
            TaskAction::ResolveConflicts => {
                let step = card.step.as_ref().filter(|s| s.kind == StepKind::Rebase)?;
                let StepState::Failed { why } = &step.state else { return None };
                // On another machine the target is in its clone as the queue sent it, under a
                // name of the server's: the clone's own target lacks what the queue merged.
                let onto = if self.remote(task) {
                    let branch = Task::target_branch(&self.project.id);
                    format!("{branch}, the queue's {target} as sent to your clone")
                } else {
                    target.clone()
                };
                Some(format!(
                    "Resolve the conflicts. Your work does not rebase onto {target}: {}. Rebase \
                     onto {onto}, resolve them, then {then}",
                    crate::kit::first_line(why)
                ))
            }
            TaskAction::Review
            | TaskAction::Merge
            | TaskAction::Start
            | TaskAction::Retry
            | TaskAction::Push
            | TaskAction::Cancel => None,
        }
    }

    /// The to-dos still open on `task`'s agent's own list, while its work waits to be checked
    /// or merged: the person sees them before they merge it.
    #[must_use]
    pub fn open_todos(&self, task: TaskId) -> u16 {
        let Some(card) = self.tasks.get(&task) else { return 0 };
        let waiting = matches!(card.state, TaskState::Verifying | TaskState::Done);
        if waiting { card.natives.todos.saturating_sub(card.natives.done) } else { 0 }
    }

    /// `task`'s way to the target in one row, once its work is on it: its branch, what its
    /// verifier said, its place in the queue, its pull request with its own
    /// checks, and the to-dos still open. Empty while it is still being worked on, and once it
    /// has merged.
    #[must_use]
    pub fn pipeline(&self, task: TaskId) -> Vec<Stage> {
        let Some(card) = self.tasks.get(&task) else { return Vec::new() };
        let on_its_way = card.verified.is_some()
            || card.merge.is_some()
            || card.pull.is_some()
            || matches!(card.state, TaskState::Verifying | TaskState::Done);
        let stage = |kind, words: String, holds| Stage { kind, words, holds, failed: false };
        let failure = |kind, words: String| Stage { kind, words, holds: true, failed: true };
        if card.state == TaskState::Merged {
            let failed = card.merge.as_ref().and_then(|m| match m {
                Merge::Merged { push_failed: Some(why), .. } => Some(why),
                Merge::Merged { .. } | Merge::Queued { .. } | Merge::Pull { .. } => None,
            });
            return failed
                .map(|why| {
                    let words = format!("Push failed: {}", crate::kit::first_line(why));
                    failure(StageKind::Push, words)
                })
                .into_iter()
                .collect();
        }
        if !on_its_way || card.read_only {
            return Vec::new();
        }
        let running =
            |kind: StepKind| card.step.as_ref().is_some_and(|s| s.kind == kind && s.running());
        let mut out = Vec::new();
        if let Some(branch) = &card.branch {
            out.push(stage(StageKind::Branch, branch.clone(), false));
        }
        if running(StepKind::Verify) {
            out.push(stage(StageKind::Verifier, "Verifying".to_owned(), false));
        } else if let Some(run) = self.verdict(task) {
            out.push(if run.passed {
                stage(StageKind::Verifier, "Verified".to_owned(), false)
            } else {
                failure(StageKind::Verifier, "Verifier failed".to_owned())
            });
        }
        if running(StepKind::Merge) {
            out.push(stage(StageKind::Queue, "Merging".to_owned(), false));
        } else if let Some((place, _)) = self.queue_place(task) {
            out.push(stage(StageKind::Queue, queue_words(place), false));
        }
        // Its protected target takes it through a pull request, which its thread's watch may
        // not have read yet.
        if let Some(Merge::Pull { number, target, .. }) = &card.merge
            && card.pull.as_ref().is_none_or(|p| p.number != *number)
        {
            out.push(stage(StageKind::Pull, format!("#{number} into {target}"), false));
        }
        // The pull request, its checks and its review apart, so a narrow lane wraps them
        // rather than cutting one long chip.
        if let Some(pull) = &card.pull {
            let named = format!("{} {}", short(pull.forge), pull.named());
            out.push(stage(StageKind::Pull, named, false));
            if let Some((words, failing)) = checks_words(pull) {
                out.push(if failing {
                    failure(StageKind::Checks, words)
                } else {
                    stage(StageKind::Checks, words, false)
                });
            }
            if pull.stands == PullStands::ChangesRequested {
                out.push(stage(StageKind::PullReview, "Changes requested".to_owned(), true));
            }
        }
        match self.open_todos(task) {
            0 => {}
            1 => out.push(stage(StageKind::ToDos, "1 to-do open".to_owned(), true)),
            n => out.push(stage(StageKind::ToDos, format!("{n} to-dos open"), true)),
        }
        out
    }

    /// Whether `card` waits to be started: planned, with no terminal on it now.
    fn not_started(&self, card: &TaskCard) -> bool {
        card.state == TaskState::Planned && self.terminal(Some(card.id)).is_none()
    }

    /// The tasks whose agent waits on the person, by number.
    #[must_use]
    pub fn needs_you(&self) -> Vec<TaskId> {
        self.tasks.values().filter(|c| c.state == TaskState::Blocked).map(|c| c.id).collect()
    }

    /// The tasks `task` depends on that are not done yet: what it still waits on.
    #[must_use]
    pub fn waiting_on(&self, task: TaskId) -> Vec<TaskId> {
        let Some(card) = self.tasks.get(&task) else { return Vec::new() };
        card.depends_on
            .iter()
            .copied()
            .filter(|dep| {
                self.tasks
                    .get(dep)
                    .is_none_or(|d| !matches!(d.state, TaskState::Done | TaskState::Merged))
            })
            .collect()
    }

    /// How many terminals run for the project: its tasks' live ones and its orchestrator's.
    #[must_use]
    pub fn live(&self) -> usize {
        let tasks = self
            .tasks
            .values()
            .filter(|c| c.assignment.as_ref().is_some_and(slopty_proto::project::Assignment::open))
            .count();
        tasks.saturating_add(usize::from(self.project.orchestrator.is_some()))
    }

    /// How many tasks are in each state that counts toward the goal: finished (merged) of all.
    #[must_use]
    pub fn progress(&self) -> (usize, usize) {
        let merged = self.tasks.values().filter(|c| c.state == TaskState::Merged).count();
        (merged, self.tasks.len())
    }

    /// The merged tasks whose merge is not on the forge yet, by number: the project pushes only
    /// when the person turned pushing on, so what the merge queue landed may sit in the
    /// orchestrator's clone alone. One push takes them all.
    #[must_use]
    pub fn unpushed(&self) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|c| matches!(c.merge, Some(Merge::Merged { pushed: false, .. })))
            .map(|c| c.id)
            .collect()
    }

    /// Whether `task` runs on a machine other than the orchestrator's, so its clone holds the
    /// project's target only as the queue sends it there ([`Task::target_branch`]).
    fn remote(&self, task: TaskId) -> bool {
        let at = self.tasks.get(&task).and_then(|c| c.assignment.as_ref()).map(|a| a.term.worker);
        let home = self.project.orchestrator.map(|t| t.worker);
        at.is_some() && home.is_some() && at != home
    }

    /// When anything on the board last changed, by the server's clock.
    #[must_use]
    pub fn updated(&self) -> WallMs {
        let task = self.tasks.values().map(|c| c.updated_ms).max().unwrap_or(WallMs::ZERO);
        let entry = self.timeline.back().map_or(WallMs::ZERO, |e| e.at_ms);
        task.max(entry).max(self.project.created_ms)
    }
}

/// A step the server takes for a task, in words: what, where, and how it went.
#[must_use]
pub fn step_line(step: &TaskStep, name: impl Fn(WorkerId) -> String) -> String {
    let at = name(step.worker);
    let first = |text: &str| crate::kit::first_line(text).to_owned();
    match (step.kind, &step.state) {
        (StepKind::Clone, StepState::Running { phase, percent }) => match percent {
            Some(p) => format!("Cloning on {at}: {} {p}%", first(phase)),
            None => format!("Cloning on {at}"),
        },
        (StepKind::Clone, StepState::Done { .. }) => format!("Cloned on {at}"),
        (StepKind::Clone, StepState::Failed { why }) => {
            format!("Clone on {at} failed: {}", first(why))
        }
        (StepKind::Send, StepState::Running { .. }) => format!("Sending its start to {at}"),
        (StepKind::Send, StepState::Done { detail }) => format!("Sent to {at}: {}", first(detail)),
        (StepKind::Send, StepState::Failed { why }) => {
            format!("Not sent to {at}: {}", first(why))
        }
        (StepKind::Home, StepState::Running { percent, .. }) => match percent {
            Some(p) => format!("Bringing its branch to {at}: {p}%"),
            None => format!("Bringing its branch to {at}"),
        },
        (StepKind::Home, StepState::Done { detail }) => {
            format!("Branch arrived on {at}: {}", first(detail))
        }
        (StepKind::Home, StepState::Failed { why }) => {
            format!("Branch did not reach {at}: {}", first(why))
        }
        (StepKind::Verify, StepState::Running { phase, .. }) => match first(phase).as_str() {
            "" => format!("Verifying on {at}"),
            line => format!("Verifying on {at}: {line}"),
        },
        (StepKind::Verify, StepState::Done { .. }) => format!("Verified on {at}"),
        (StepKind::Verify, StepState::Failed { why }) => {
            format!("Verifier on {at} failed: {}", first(why))
        }
        (StepKind::Merge, StepState::Running { phase, .. }) => {
            format!("Merging on {at}: {}", first(phase))
        }
        (StepKind::Merge, StepState::Done { detail }) => format!("Merged: {}", first(detail)),
        (StepKind::Merge, StepState::Failed { why }) => format!("Not merged: {}", first(why)),
        (StepKind::Rebase, StepState::Running { .. }) => format!("Rebasing on {at}"),
        (StepKind::Rebase, StepState::Done { .. }) => format!("Rebased on {at}"),
        (StepKind::Rebase, StepState::Failed { why }) => format!("Conflicts: {}", first(why)),
    }
}

/// What a verifier said, in a line: where it ran to, and for a failure the last thing it
/// printed, which is where a build or a test run says what broke.
#[must_use]
pub fn verdict_line(run: &VerifierRun) -> String {
    let at = short_commit(&run.head);
    if run.passed {
        return format!("Verifier passed at {at}");
    }
    let last = run.summary.lines().map(str::trim).rfind(|l| !l.is_empty());
    match (last, run.exit) {
        (Some(line), _) => format!("Verifier failed at {at}: {line}"),
        (None, Some(code)) => format!("Verifier failed at {at}: exit {code}"),
        (None, None) => format!("Verifier failed at {at}"),
    }
}

/// What a verifier judged and how it went, after its verdict's word: the commit it ran on,
/// the target's commit that work left from, how it ended and how long it took.
#[must_use]
pub fn verdict_detail(run: &VerifierRun) -> String {
    let mut parts = vec![match run.base.as_str() {
        "" => short_commit(&run.head).to_owned(),
        base => format!("{} over {}", short_commit(&run.head), short_commit(base)),
    }];
    if !run.passed {
        match run.exit {
            Some(code) if code < 0 => parts.push(format!("signal {}", code.unsigned_abs())),
            Some(code) => parts.push(format!("exit {code}")),
            None => {}
        }
    }
    if run.took_ms > 0 {
        parts.push(crate::kit::duration(std::time::Duration::from_millis(run.took_ms)));
    }
    parts.join(" \u{b7} ")
}

/// The last `n` lines a failed verifier printed that say anything, each from its first word:
/// where a build or a test run says what broke, as a glance and not the log, which its
/// terminal keeps whole.
#[must_use]
pub fn verdict_tail(run: &VerifierRun, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = run.summary.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let from = lines.len().saturating_sub(n);
    lines.get(from..).map(<[&str]>::to_vec).unwrap_or_default()
}

/// A place in a queue as people say it: next, 2nd, 3rd.
#[must_use]
pub fn queue_words(place: usize) -> String {
    let suffix = match (place % 10, place % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    if place == 1 { "Next to merge".to_owned() } else { format!("{place}{suffix} to merge") }
}

/// A commit as people read it: its first seven hex digits.
#[must_use]
pub fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// `item` at the end of `list`, the oldest going past [`NATIVES_KEPT`].
fn push_bounded<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > NATIVES_KEPT {
        list.remove(0);
    }
}

/// A task state as a row says it: its lane's heading, so a row and the board never name one
/// state two ways. Waiting is the one finer word, a task at its prompt in "Working".
#[must_use]
pub const fn state_word(state: TaskState) -> &'static str {
    match state {
        TaskState::Waiting => "Waiting",
        _ => Lane::of(state).title(),
    }
}
