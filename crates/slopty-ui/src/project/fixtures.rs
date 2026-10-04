//! Projects as the server would send them, for the board's tests.

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Assignment, Bounds, Finding, Limits, Live, Merge, NativeCounts, Project, ProjectId,
    ProjectStatus, ProjectUpdate, ProjectsPart, ReviewRun, ReviewVerdict, Reviewer, StepKind,
    StepState, TaskCard, TaskId, TaskState, TaskStep, TimelineEntry, VerifierRun,
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
        spend: slopty_proto::project::Spend::default(),
        orchestrator_spent: slopty_proto::project::Spent::default(),
        id: id(name),
        title: "Ship the project board".to_owned(),
        repo: "slopty".to_owned(),
        repo_id: None,
        target: "main".to_owned(),
        review: None,
        verifier: Some("cargo gate".to_owned()),
        push: false,
        ask_to_start: false,
        orchestrator,
        limits: Limits::default(),
        metadata: None,
        needs: Vec::new(),
        schedules: Vec::new(),
        scripts: Vec::new(),
        created_ms: AT,
        members: Vec::new(),
    }
}

/// Task `n`, titled, in `state`, split from `parent`.
pub(crate) fn card(n: u32, title: &str, state: TaskState, parent: Option<u32>) -> TaskCard {
    TaskCard {
        checks: None,
        spent: slopty_proto::project::Spent::default(),
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
        reviewed: None,
        verified: None,
        merge: None,
        step: None,
        pin: None,
        proposed: None,
        natives: NativeCounts::default(),
        created_ms: AT,
        updated_ms: AT,
        attempts: None,
    }
}

/// `card` with its terminal: `session` on `worker`, still live.
pub(crate) fn on(mut card: TaskCard, worker: WorkerId, session: SessionId) -> TaskCard {
    card.assignment = Some(Assignment {
        term: TermRef { worker, session },
        thread: None,
        since_ms: AT,
        ended_ms: None,
        conversation: None,
        placed: None,
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
    TaskStep { kind, worker, state, since_ms: AT, term, commits: None }
}

/// `card` done and in the merge queue since `after` past [`AT`], its verifier passed.
pub(crate) fn queued(mut card: TaskCard, after: u64, head: &str) -> TaskCard {
    card.verified = Some(run(true, head));
    card.merge =
        Some(Merge::Queued { since_ms: WallMs::from_millis(AT.as_millis().saturating_add(after)) });
    card
}

/// A review of `head` over the [`run`]s' base: an approval, or changes asked over a missing
/// golden with a note beside it, by a reviewer of its own in `term`, else by the person.
pub(crate) fn review(approved: bool, head: &str, term: Option<TermRef>) -> ReviewRun {
    let finding = |blocking: bool, path: Option<&str>, line, body: &str| Finding {
        path: path.map(str::to_owned),
        line,
        severity: if blocking { "blocker" } else { "nit" }.to_owned(),
        blocking,
        body: body.to_owned(),
    };
    let findings = if approved {
        vec![finding(false, None, None, "The commit message could say why.")]
    } else {
        vec![
            finding(
                true,
                Some("crates/slopty-proto/src/project.rs"),
                Some(431),
                "Project.review has no golden, so a wire change would pass unseen.",
            ),
            finding(
                false,
                Some("docs/decisions/projects.md"),
                None,
                "Says verifier where it means reviewer.",
            ),
        ]
    };
    ReviewRun {
        verdict: ReviewVerdict {
            approved,
            summary: if approved { "Reads well." } else { "One blocker." }.to_owned(),
            findings,
        },
        more: 0,
        head: head.to_owned(),
        base: "c08d4c1a9f".to_owned(),
        by: term.map_or(Reviewer::Person, Reviewer::Agent),
        took_ms: 95_000,
    }
}
