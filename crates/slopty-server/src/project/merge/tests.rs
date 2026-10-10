use std::collections::HashSet;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{LimitsChange, StepKind, TaskChange, TaskSpec};

use super::*;
use crate::project::{Caller, Keep, NewProject, ProjectsFile, Queue, RESUMING, Running};

fn at(ms: u64) -> WallMs {
    WallMs::from_millis(ms.saturating_add(1_790_000_000_000))
}

fn id() -> ProjectId {
    ProjectId::new("demo").unwrap()
}

fn projects(verifier: Option<&str>) -> Projects {
    logged(verifier, &mut Vec::new())
}

/// [`projects`], with every change it made kept in `log` as the store keeps them.
fn logged(verifier: Option<&str>, log: &mut Vec<Change>) -> Projects {
    let mut p = Projects::default();
    let none = HashSet::new();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    let new = NewProject {
        id: id(),
        title: "Demo".to_owned(),
        repo: "demo".to_owned(),
        target: "main".to_owned(),
        verifier: verifier.map(str::to_owned),
        push: false,
        orchestrator: Some(TermRef { worker: WorkerId::new(), session: SessionId::new() }),
        limits: LimitsChange::default(),
        metadata: None,
        goal: None,
        autonomy: slopty_proto::project::Autonomy::Ask,
    };
    log.extend(p.create(new, &running, at(0)).unwrap().1);
    p
}

fn task(p: &mut Projects, title: &str) -> TaskId {
    task_logged(p, title, &mut Vec::new())
}

fn task_logged(p: &mut Projects, title: &str, log: &mut Vec<Change>) -> TaskId {
    let spec = TaskSpec { title: title.to_owned(), ..TaskSpec::default() };
    let (made, changes) = p.create_task(&id(), spec, at(0)).unwrap();
    log.extend(changes);
    made.id
}

fn queued(since: u64) -> Advance {
    Advance {
        state: Some(TaskState::Done),
        merge: Queue::Set(Merge::Queued { since_ms: at(since) }),
        ..Advance::default()
    }
}

fn verifying() -> Advance {
    Advance { state: Some(TaskState::Verifying), ..Advance::default() }
}

/// The queue is its tasks: those done and queued, the longest waiting first, with a task
/// waiting for its verifier taken before any. It reads back the same from the store's file and
/// from its log replayed, and a merge under way when the server stopped waits for its worker,
/// its task still in its place for the lane to merge again.
#[test]
fn the_queue_is_its_tasks_in_the_order_they_joined_and_outlives_a_restart() {
    let mut log = Vec::new();
    let mut p = logged(Some("cargo gate"), &mut log);
    let a = task_logged(&mut p, "A", &mut log);
    let b = task_logged(&mut p, "B", &mut log);
    let c = task_logged(&mut p, "C", &mut log);
    assert_eq!(p.next_job(&id()), None, "nothing to do");
    log.extend(p.advance(&id(), a, verifying(), at(1)).unwrap().1);
    assert_eq!(p.next_job(&id()), Some(Job::Verify(a)));
    log.extend(p.advance(&id(), a, queued(20), at(2)).unwrap().1);
    log.extend(p.advance(&id(), b, verifying(), at(3)).unwrap().1);
    log.extend(p.advance(&id(), b, queued(10), at(4)).unwrap().1);
    assert_eq!(p.queue(&id()), [b, a], "by when each joined, not by number");
    log.extend(p.advance(&id(), c, verifying(), at(5)).unwrap().1);
    assert_eq!(p.next_job(&id()), Some(Job::Verify(c)), "a verifier first");

    let worker = WorkerId::new();
    let merging = TaskStep {
        kind: StepKind::Merge,
        worker,
        state: StepState::Running { phase: "Rebasing onto main".to_owned(), percent: None },
        since_ms: at(6),
        term: None,
        commits: None,
    };
    let under_way = Advance { step: Some(merging), ..Advance::default() };
    log.extend(p.advance(&id(), b, under_way, at(6)).unwrap().1);

    let file = serde_json::to_vec(&p.file(Vec::new(), 0)).unwrap();
    let mut replayed = ProjectsFile::default();
    for change in log.iter().filter(|c| c.durable) {
        replayed.apply(&Keep::Project(Box::new(change.kept.clone())));
    }
    for back in
        [Projects::restore(serde_json::from_slice(&file).unwrap()), Projects::restore(replayed)]
    {
        assert_eq!(back.queue(&id()), [b, a], "the queue as it stood");
        assert_eq!(back.next_job(&id()), Some(Job::Verify(c)));
        let step = back.task(&id(), b).unwrap().step.clone().unwrap();
        let resuming = StepState::Running { phase: RESUMING.to_owned(), percent: None };
        assert_eq!(step.state, resuming, "taken up once its worker is back");
        assert_eq!(back.task(&id(), b).unwrap().merge, Some(Merge::Queued { since_ms: at(10) }));
    }
}

/// The person merges: a task with a verifier is verified first, keeping their Merge so it
/// joins the queue once it passes; one with none, or one ready to merge, joins at once. A merged
/// task, or one that only reads, has nothing to merge. A task moved out of done by anyone but the
/// queue leaves it, and the queue never merges a task that is not done.
#[test]
fn the_person_asks_for_a_merge_and_the_queue_takes_only_what_is_done() {
    let mut p = projects(Some("cargo gate"));
    let a = task(&mut p, "A");
    let (asked, changes) = p.ask_merge(&id(), a, at(1)).unwrap();
    let kept = Some(Merge::Queued { since_ms: at(1) });
    assert_eq!((asked.state, asked.merge), (TaskState::Verifying, kept));
    let entry = changes.iter().find_map(|c| c.kept.entry.as_ref()).unwrap();
    assert_eq!(entry.what, Moment::State { from: TaskState::Planned, to: TaskState::Verifying });
    assert_eq!(p.queue(&id()), Vec::<TaskId>::new(), "not until it passes");
    let passed = Advance { state: Some(TaskState::Done), ..Advance::default() };
    p.advance(&id(), a, passed, at(2)).unwrap();
    assert_eq!(p.queue(&id()), [a], "the person's Merge stood");

    let mut bare = projects(None);
    let b = task(&mut bare, "B");
    let (asked, _) = bare.ask_merge(&id(), b, at(2)).unwrap();
    assert_eq!(asked.state, TaskState::Done);
    assert_eq!(asked.merge, Some(Merge::Queued { since_ms: at(2) }));
    assert_eq!(bare.next_job(&id()), Some(Job::Merge(b)));

    let back = TaskChange { state: Some(TaskState::Running), ..TaskChange::default() };
    bare.update_task(&id(), b, back, Caller::Agent, at(3)).unwrap();
    assert_eq!(bare.task(&id(), b).unwrap().merge, None, "it left the queue");
    assert_eq!(bare.next_job(&id()), None);

    let merged = Advance {
        state: Some(TaskState::Merged),
        merge: Queue::Set(Merge::Merged {
            target: "main".to_owned(),
            head: "d".repeat(40),
            from: "a".repeat(40),
            at_ms: at(4),
            pushed: false,
            push_failed: None,
        }),
        ..Advance::default()
    };
    let refused = bare.advance(&id(), b, merged.clone(), at(4));
    assert!(refused.is_err(), "only a done or verifying task merges");
    bare.advance(&id(), b, queued(5), at(5)).unwrap();
    bare.advance(&id(), b, merged, at(6)).unwrap();
    assert!(bare.ask_merge(&id(), b, at(7)).is_err(), "merged already");
    assert_eq!(bare.queue(&id()), Vec::<TaskId>::new());

    let spec = TaskSpec { title: "Look".to_owned(), read_only: true, ..TaskSpec::default() };
    let look = bare.create_task(&id(), spec, at(8)).unwrap().0.id;
    assert!(bare.ask_merge(&id(), look, at(9)).is_err(), "a reader has nothing to merge");
}

/// Work judged afresh forgets the step that judged the old work, and keeps the trip that
/// brought the new work home: the card still says where the branch landed, as a task with no
/// checks is done the moment it arrives.
#[test]
fn work_judged_afresh_keeps_where_its_branch_came_home() {
    let mut p = projects(None);
    let a = task(&mut p, "A");
    let step = |kind, state| TaskStep {
        kind,
        worker: WorkerId::new(),
        state,
        since_ms: at(1),
        term: None,
        commits: None,
    };
    let fresh = || Advance { fresh: true, ..Advance::default() };
    let failed = StepState::Failed { why: "cargo gate failed".to_owned() };
    p.set_step(&id(), a, step(StepKind::Verify, failed), at(1)).unwrap();
    let (judged, _) = p.advance(&id(), a, fresh(), at(2)).unwrap();
    assert_eq!(judged.step, None, "the old verdict is forgotten");

    let why = "the orchestrator's clone lacks the branch's history".to_owned();
    p.set_step(&id(), a, step(StepKind::Home, StepState::Failed { why }), at(3)).unwrap();
    let (judged, _) = p.advance(&id(), a, fresh(), at(4)).unwrap();
    assert_eq!(judged.step, None, "a trip that failed brought nothing");

    let detail = "worktree-a as slopty/demo/1 at a31deb0 in /w/demo".to_owned();
    let home = step(StepKind::Home, StepState::Done { detail });
    p.set_step(&id(), a, home.clone(), at(5)).unwrap();
    let done = Advance { state: Some(TaskState::Done), ..fresh() };
    let (judged, _) = p.advance(&id(), a, done, at(6)).unwrap();
    assert_eq!(judged.step, Some(home), "where the new work is stays on its card");
}

/// A push takes the whole target: every task merged into it by then, not pushed, goes with
/// it, so what waits to be pushed is read off the cards. One merged later, or into another
/// branch, still waits.
#[test]
fn a_push_takes_every_merge_before_it() {
    let mut p = projects(None);
    let merged = |into: &str, ms: u64| Advance {
        state: Some(TaskState::Merged),
        merge: Queue::Set(Merge::Merged {
            target: into.to_owned(),
            head: "d".repeat(40),
            from: "a".repeat(40),
            at_ms: at(ms),
            pushed: false,
            push_failed: (ms == 2).then(|| "rejected".to_owned()),
        }),
        ..Advance::default()
    };
    let mut tasks = Vec::new();
    for (title, into, ms) in [("A", "main", 1), ("B", "main", 2), ("C", "main", 9), ("D", "dev", 3)]
    {
        let t = task(&mut p, title);
        p.advance(&id(), t, queued(ms), at(ms)).unwrap();
        p.advance(&id(), t, merged(into, ms), at(ms)).unwrap();
        tasks.push(t);
    }
    let changes = p.pushed_with(&id(), "main", at(5));
    assert_eq!(changes.len(), 2, "A and B went");
    let pushed = |t: TaskId| match p.task(&id(), t).unwrap().merge.clone() {
        Some(Merge::Merged { pushed, push_failed, .. }) => (pushed, push_failed),
        other => panic!("{other:?}"),
    };
    let states: Vec<_> = tasks.iter().map(|t| pushed(*t)).collect();
    assert_eq!(states, [(true, None), (true, None), (false, None), (false, None)]);
}
