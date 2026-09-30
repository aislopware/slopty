//! The server's projects as this client mirrors them, and what the board derives from one.
//!
//! The server sends every project's tree after a link comes up, in parts
//! ([`ProjectsPart`]), then each change as a [`ProjectUpdate`] numbered in its event log. A part
//! marked `first` replaces what was here. An update at or below the snapshot's `seq` is already
//! in it and is dropped, since a replay after a lag would otherwise put older state over newer.
//!
//! Everything the board draws is derived here, pure: the tree of who split what from whom
//! ([`Board::tree`]), the lanes that answer "what needs me" ([`Board::lanes`], the worst of a
//! task's subtree deciding a parent's), the dependencies still open ([`Board::waiting_on`]) and
//! each timeline entry in words ([`moment_line`]).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Moment, Native, NativeChange, NativeCounts, Natives, Project, ProjectId, ProjectStatus,
    ProjectUpdate, ProjectsPart, ReportKind, TaskCard, TaskId, TaskState, TimelineEntry,
};

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

/// Where a node of the tree stands on the board: the lanes, left to right.
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
    /// Every lane, left to right. The order is also the urgency a parent takes from its
    /// subtree: the first lane any of them is in.
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

/// One line of the tree, in the order it is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TreeRow {
    /// The task, or `None` for the orchestrator at the root.
    pub task: Option<TaskId>,
    /// How far in: 0 for the orchestrator, 1 for what it split off itself.
    pub depth: usize,
    /// Whether it is the last of its parent's children, so its guide ends at it.
    pub last: bool,
}

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

    /// The worker a node runs on, or ran on last.
    #[must_use]
    pub fn worker(&self, node: Option<TaskId>) -> Option<WorkerId> {
        match node {
            None => self.project.orchestrator.map(|t| t.worker),
            Some(task) => self.tasks.get(&task)?.assignment.as_ref().map(|a| a.term.worker),
        }
    }

    /// The tasks that were split from `parent` (the orchestrator's own for `None`), by number.
    /// A task whose parent is not on the board hangs from the orchestrator, so nothing falls
    /// out of the tree.
    fn children(&self, parent: Option<TaskId>) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|card| {
                let hangs_from = card.parent.filter(|p| self.tasks.contains_key(p));
                hangs_from == parent
            })
            .map(|card| card.id)
            .collect()
    }

    /// The tree, depth first: the orchestrator, then what each node split off, by number.
    #[must_use]
    pub fn tree(&self) -> Vec<TreeRow> {
        let mut rows = vec![TreeRow { task: None, depth: 0, last: true }];
        let mut stack: Vec<(TaskId, usize, bool)> = Vec::new();
        let push_children = |stack: &mut Vec<(TaskId, usize, bool)>, parent, depth| {
            let children = self.children(parent);
            let n = children.len();
            for (i, child) in children.into_iter().enumerate().rev() {
                stack.push((child, depth, i.saturating_add(1) == n));
            }
        };
        push_children(&mut stack, None, 1);
        // A cycle of parents cannot come from the server, but a bound keeps a bad one finite.
        while let Some((task, depth, last)) = stack.pop() {
            if rows.len() > self.tasks.len() {
                break;
            }
            rows.push(TreeRow { task: Some(task), depth, last });
            push_children(&mut stack, Some(task), depth.saturating_add(1));
        }
        rows
    }

    /// The lane a task is in on the board: the first lane of it and everything split from
    /// it, so a parent stands where its most urgent descendant does.
    #[must_use]
    pub fn lane(&self, task: TaskId) -> Option<Lane> {
        let mut lane = Lane::of(self.tasks.get(&task)?.state);
        let mut todo = self.children(Some(task));
        let mut seen = 0_usize;
        while let Some(next) = todo.pop() {
            seen = seen.saturating_add(1);
            if seen > self.tasks.len() {
                break;
            }
            if let Some(card) = self.tasks.get(&next) {
                lane = lane.min(Lane::of(card.state));
            }
            todo.extend(self.children(Some(next)));
        }
        Some(lane)
    }

    /// The board: each lane that holds a task, left to right, its tasks by number.
    #[must_use]
    pub fn lanes(&self) -> Vec<(Lane, Vec<TaskId>)> {
        let mut by_lane: BTreeMap<Lane, Vec<TaskId>> = BTreeMap::new();
        for id in self.tasks.keys() {
            if let Some(lane) = self.lane(*id) {
                by_lane.entry(lane).or_default().push(*id);
            }
        }
        by_lane.into_iter().collect()
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

    /// When anything on the board last changed, by the server's clock.
    #[must_use]
    pub fn updated(&self) -> WallMs {
        let task = self.tasks.values().map(|c| c.updated_ms).max().unwrap_or(WallMs::ZERO);
        let entry = self.timeline.back().map_or(WallMs::ZERO, |e| e.at_ms);
        task.max(entry).max(self.project.created_ms)
    }
}

/// `entry` in words, with the worker names `name` gives and the agents `agent` names by their
/// terminals. The task's number leads, where there is one, since the row draws it apart.
#[must_use]
pub fn moment_line(
    entry: &TimelineEntry,
    name: impl Fn(WorkerId) -> String,
    agent: impl Fn(TermRef) -> String,
) -> String {
    match &entry.what {
        Moment::Created => "Project created".to_owned(),
        Moment::Orchestrator { term } => format!("Orchestrator on {}", name(term.worker)),
        Moment::Limits { limits } => format!(
            "Limits: {} agents in all, {} per worker",
            limits.live_per_project, limits.live_per_worker
        ),
        Moment::TaskCreated { title } => format!("Created: {title}"),
        Moment::Claimed { paths } => match paths.as_slice() {
            [] => "Owns nothing".to_owned(),
            [one] => format!("Owns {one}"),
            [first, rest @ ..] => format!("Owns {first} and {} more", rest.len()),
        },
        Moment::Assigned { term, spawned: true } => {
            format!("Started on {}", name(term.worker))
        }
        Moment::Assigned { term, spawned: false } => {
            format!("Taken on in a terminal on {}", name(term.worker))
        }
        Moment::State { to, .. } => state_word(*to).to_owned(),
        Moment::Branch { branch, pr } => match (branch, pr) {
            (Some(branch), Some(pr)) => format!("On {branch}, pull request #{pr}"),
            (Some(branch), None) => format!("On {branch}"),
            (None, Some(pr)) => format!("Pull request #{pr}"),
            (None, None) => "Left its branch".to_owned(),
        },
        Moment::Verified { passed: true, .. } => "Verifier passed".to_owned(),
        Moment::Verified { passed: false, summary } => match crate::kit::first_line(summary) {
            "" => "Verifier failed".to_owned(),
            line => format!("Verifier failed: {line}"),
        },
        Moment::AgentGone { .. } => "Agent ended".to_owned(),
        Moment::Note { text } => crate::kit::first_line(text).to_owned(),
        Moment::Reported { report } => {
            let kind = match report.kind {
                ReportKind::Checkpoint => "Checkpoint",
                ReportKind::NeedsInput => "Needs an answer",
                ReportKind::Stuck => "Stuck",
                ReportKind::Done => "Reported done",
            };
            match crate::kit::first_line(&report.note) {
                "" => kind.to_owned(),
                line => format!("{kind}: {line}"),
            }
        }
        Moment::Delivered { term, reports: 1 } => format!("A report delivered to {}", agent(*term)),
        Moment::Delivered { term, reports } => {
            format!("{reports} reports delivered to {}", agent(*term))
        }
    }
}

/// `item` at the end of `list`, the oldest going past [`NATIVES_KEPT`].
fn push_bounded<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > NATIVES_KEPT {
        list.remove(0);
    }
}

/// A task state as a row says it.
#[must_use]
pub const fn state_word(state: TaskState) -> &'static str {
    match state {
        TaskState::Planned => "Planned",
        TaskState::Running => "Working",
        TaskState::Waiting => "Waiting",
        TaskState::Blocked => "Needs you",
        TaskState::Verifying => "Verifying",
        TaskState::Done => "Ready to merge",
        TaskState::Merged => "Merged",
        TaskState::Failed => "Failed",
    }
}

/// A task state as a status mark draws it.
#[must_use]
pub const fn state_status(state: TaskState) -> crate::icons::Status {
    use crate::icons::Status;
    match state {
        TaskState::Planned => Status::Idle,
        TaskState::Running | TaskState::Verifying => Status::Working,
        TaskState::Waiting => Status::Running,
        TaskState::Blocked => Status::NeedsYou,
        TaskState::Done | TaskState::Merged => Status::Done,
        TaskState::Failed => Status::Failed,
    }
}
