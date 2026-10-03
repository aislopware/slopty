use std::collections::HashSet;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{LimitsChange, StepKind, TaskChange, TaskSpec};

use super::*;
use crate::project::{Caller, Keep, NewProject, ProjectsFile, Queue, Running};

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
    reviewed(verifier, None, log)
}

/// [`logged`], with a reviewer's brief when there is one.
fn reviewed(verifier: Option<&str>, review: Option<&str>, log: &mut Vec<Change>) -> Projects {
    let mut p = Projects::default();
    let none = HashSet::new();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    let new = NewProject {
        id: id(),
        title: "Demo".to_owned(),
        repo: "demo".to_owned(),
        target: "main".to_owned(),
        review: review.map(str::to_owned),
        verifier: verifier.map(str::to_owned),
        push: false,
        ask_to_start: false,
        orchestrator: Some(TermRef { worker: WorkerId::new(), session: SessionId::new() }),
        limits: LimitsChange::default(),
        metadata: None,
        members: Vec::new(),
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
/// from its log replayed, and a merge under way when the server stopped says it ended, its
/// task still in its place.
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
        assert_eq!(
            step.state,
            StepState::Failed { why: "the server stopped while it ran".to_owned() },
            "no merge goes on across a restart"
        );
        assert_eq!(back.task(&id(), b).unwrap().merge, Some(Merge::Queued { since_ms: at(10) }));
    }
}

/// The person asks for a merge: a task with a verifier is verified first, one with none joins
/// the queue at once. A merged task, or one that only reads, has nothing to merge. A task
/// moved out of done by anyone but the queue leaves it, and the queue never merges a task
/// that is not done.
#[test]
fn the_person_asks_for_a_merge_and_the_queue_takes_only_what_is_done() {
    let mut p = projects(Some("cargo gate"));
    let a = task(&mut p, "A");
    let (asked, changes) = p.ask_merge(&id(), a, at(1)).unwrap();
    assert_eq!((asked.state, asked.merge), (TaskState::Verifying, None));
    let entry = changes.iter().find_map(|c| c.kept.entry.as_ref()).unwrap();
    assert_eq!(entry.what, Moment::State { from: TaskState::Planned, to: TaskState::Verifying });

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
            at_ms: at(4),
            pushed: false,
        }),
        ..Advance::default()
    };
    let refused = bare.advance(&id(), b, merged.clone(), at(4));
    assert!(refused.is_err(), "only a done or verifying task merges");
    bare.advance(&id(), b, queued(5), at(5)).unwrap();
    bare.advance(&id(), b, merged, at(6)).unwrap();
    assert!(bare.ask_merge(&id(), b, at(7)).is_err(), "merged already");
    assert!(bare.queue(&id()).is_empty());

    let spec = TaskSpec { title: "Look".to_owned(), read_only: true, ..TaskSpec::default() };
    let look = bare.create_task(&id(), spec, at(8)).unwrap().0.id;
    assert!(bare.ask_merge(&id(), look, at(9)).is_err(), "a reader has nothing to merge");
}

/// With a reviewer asked for, a task's verifier passing leaves it to be read next, at once
/// for a task with no verifier. A reviewer at work holds only its own task, and the lane
/// verifies the next beside it. A review under way when the server stopped holds its task
/// for the person, never started again on its own. A review keeps at most a card's worth:
/// what blocks first, the rest counted.
#[test]
fn a_reviewer_reads_each_task_after_its_verifier_and_holds_only_its_own() {
    use slopty_proto::project::{FINDINGS_MAX, Finding, ReviewRun, ReviewVerdict, Reviewer};
    let mut log = Vec::new();
    let mut p = reviewed(Some("cargo gate"), Some("goldens"), &mut log);
    let a = task_logged(&mut p, "A", &mut log);
    let b = task_logged(&mut p, "B", &mut log);
    log.extend(p.advance(&id(), a, verifying(), at(1)).unwrap().1);
    assert_eq!(p.next_job(&id()), Some(Job::Verify(a)));
    let pass = VerifierRun {
        passed: true,
        summary: String::new(),
        head: "a".repeat(40),
        base: "b".repeat(40),
        exit: Some(0),
        took_ms: 1,
    };
    let passed = Advance { verified: Some(pass), ..Advance::default() };
    log.extend(p.advance(&id(), a, passed, at(2)).unwrap().1);
    assert_eq!(p.next_job(&id()), Some(Job::Review(a)), "read once verified");
    let reading = TaskStep {
        kind: StepKind::Review,
        worker: WorkerId::new(),
        state: StepState::Running { phase: "Reading".to_owned(), percent: None },
        since_ms: at(3),
        term: Some(TermRef { worker: WorkerId::new(), session: SessionId::new() }),
    };
    let at_work = Advance { step: Some(reading), ..Advance::default() };
    log.extend(p.advance(&id(), a, at_work, at(3)).unwrap().1);
    assert_eq!(p.next_job(&id()), None, "nothing for the lane while it reads");
    log.extend(p.advance(&id(), b, verifying(), at(4)).unwrap().1);
    assert_eq!(p.next_job(&id()), Some(Job::Verify(b)), "the lane goes on beside it");

    let mut replayed = ProjectsFile::default();
    for change in log.iter().filter(|c| c.durable) {
        replayed.apply(&Keep::Project(Box::new(change.kept.clone())));
    }
    let back = Projects::restore(replayed);
    let step = back.task(&id(), a).unwrap().step.clone().unwrap();
    assert!(matches!(step.state, StepState::Failed { .. }), "{step:?}");
    assert_eq!(back.next_job(&id()), Some(Job::Verify(b)), "held for the person, not read again");

    let mut bare = reviewed(None, Some("goldens"), &mut Vec::new());
    let c = task(&mut bare, "C");
    bare.advance(&id(), c, verifying(), at(5)).unwrap();
    assert_eq!(bare.next_job(&id()), Some(Job::Review(c)), "nothing to verify first");

    let finding = |n: usize, blocking: bool| Finding {
        path: Some(format!("src/{n}.rs")),
        line: None,
        severity: if blocking { "blocker" } else { "nit" }.to_owned(),
        blocking,
        body: "x".repeat(2000),
    };
    let findings = (0..10).map(|n| finding(n, n >= 8)).collect();
    let verdict = ReviewVerdict { approved: false, summary: "y".repeat(5000), findings };
    let run = ReviewRun {
        verdict,
        more: 0,
        head: String::new(),
        base: String::new(),
        by: Reviewer::Person,
        took_ms: 0,
    };
    let kept = bounded(run);
    assert_eq!((kept.verdict.findings.len(), kept.more), (FINDINGS_MAX, 2));
    assert!(kept.verdict.findings[..2].iter().all(|f| f.blocking), "what blocks is kept first");
    assert!(kept.approx_bytes() <= ReviewRun::MAX_BYTES, "{}", kept.approx_bytes());
}
