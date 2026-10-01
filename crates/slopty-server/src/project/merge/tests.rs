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
