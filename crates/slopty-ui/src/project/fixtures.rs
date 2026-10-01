//! Projects as the server would send them, for the board's tests.

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Assignment, Bounds, Limits, Live, Merge, NativeCounts, Project, ProjectId, ProjectStatus,
    ProjectUpdate, ProjectsPart, StepKind, StepState, TaskCard, TaskId, TaskState, TaskStep,
    TimelineEntry, VerifierRun,
};

/// The server's clock for every fixture: a fixed instant, so ages read the same every run.
pub(crate) const AT: WallMs = WallMs::from_millis(1_790_000_000_000);

/// `name`, which must be a project name.
pub(crate) fn id(name: &str) -> ProjectId {
    ProjectId::new(name).expect("a project name")
}

/// A project whose orchestrator runs in `orchestrator`, when it has one.
pub(crate) fn project(name: &str, orchestrator: Option<TermRef>) -> Project {
    Project {
        id: id(name),
        title: "Ship the project board".to_owned(),
        repo: "slopty".to_owned(),
        repo_id: None,
        target: "main".to_owned(),
        verifier: Some("cargo gate".to_owned()),
        push: false,
        orchestrator,
        limits: Limits::default(),
        metadata: None,
        created_ms: AT,
    }
}

/// Task `n`, titled, in `state`, split from `parent`.
pub(crate) fn card(n: u32, title: &str, state: TaskState, parent: Option<u32>) -> TaskCard {
    TaskCard {
        id: TaskId(n),
        parent: parent.map(TaskId),
        depends_on: Vec::new(),
        kind: "build".to_owned(),
        title: title.to_owned(),
        read_only: false,
        state,
        status: None,
        assignment: None,
        branch: None,
        worktree: None,
        pr: None,
        verified: None,
        merge: None,
        step: None,
        natives: NativeCounts::default(),
        created_ms: AT,
        updated_ms: AT,
    }
}

/// `card` with its terminal: `session` on `worker`, still live.
pub(crate) fn on(mut card: TaskCard, worker: WorkerId, session: SessionId) -> TaskCard {
    card.assignment = Some(Assignment {
        term: TermRef { worker, session },
        since_ms: AT,
        ended_ms: None,
        conversation: None,
    });
    card.branch = Some(format!("slopty/board/{}", card.id));
    card
}

/// A timeline entry numbered `seq`.
pub(crate) fn entry(
    seq: u64,
    task: Option<u32>,
    what: slopty_proto::project::Moment,
) -> TimelineEntry {
    TimelineEntry { seq, at_ms: AT, task: task.map(TaskId), what }
}

/// A project's status as a snapshot carries it.
pub(crate) fn status(
    project: Project,
    tasks: Vec<TaskCard>,
    timeline: Vec<TimelineEntry>,
) -> ProjectStatus {
    let next = timeline.last().map_or(1, |e| e.seq.saturating_add(1));
    ProjectStatus {
        project,
        tasks,
        orchestrator_natives: NativeCounts::default(),
        timeline,
        next,
        bounds: Bounds::default(),
        live: Live::default(),
    }
}

/// A whole snapshot in one part, including the server's events up to `seq`.
pub(crate) fn snapshot(seq: u64, projects: Vec<ProjectStatus>) -> ProjectsPart {
    ProjectsPart { seq, first: true, last: true, projects }
}

/// A change to one task of `project`.
pub(crate) fn task_changed(
    project: &str,
    card: TaskCard,
    entry: Option<TimelineEntry>,
) -> ProjectUpdate {
    ProjectUpdate { project: id(project), record: None, task: Some(card), native: None, entry }
}

/// A verifier's run on `head`, from `main` at `c08d4c1…`: a pass, or a build that broke.
pub(crate) fn run(passed: bool, head: &str) -> VerifierRun {
    VerifierRun {
        passed,
        summary: if passed {
            "test result: ok. 113 passed".to_owned()
        } else {
            "   Compiling slopty-ui\nerror[E0308]: mismatched types\n  --> src/project/view.rs:12:5\n\nerror: could not compile `slopty-ui`\n".to_owned()
        },
        head: head.to_owned(),
        base: "c08d4c1a9f".to_owned(),
        exit: (!passed).then_some(101),
        took_ms: 72_000,
    }
}

/// The step of `kind` the server takes for a task on `worker`, as it stands, in `term`.
pub(crate) fn step(
    kind: StepKind,
    worker: WorkerId,
    state: StepState,
    term: Option<TermRef>,
) -> TaskStep {
    TaskStep { kind, worker, state, since_ms: AT, term }
}

/// `card` done and in the merge queue since `after` past [`AT`], its verifier passed.
pub(crate) fn queued(mut card: TaskCard, after: u64, head: &str) -> TaskCard {
    card.verified = Some(run(true, head));
    card.merge =
        Some(Merge::Queued { since_ms: WallMs::from_millis(AT.as_millis().saturating_add(after)) });
    card
}
