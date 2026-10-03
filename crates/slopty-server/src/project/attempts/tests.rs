use std::collections::HashSet;

use slopty_core::{SessionId, WorkerId};
use slopty_proto::agent::{AgentBranch, Worktree};
use slopty_proto::orchestration::Outcome;
use slopty_proto::project::{Attempts, LimitsChange, VerifierRun};

use super::*;
use crate::project::{Advance, Assignee, NewProject, Queue, Running};

fn at(ms: u64) -> WallMs {
    WallMs::from_millis(ms.saturating_add(1_790_000_000_000))
}

fn id() -> ProjectId {
    ProjectId::new("demo").unwrap()
}

fn projects() -> Projects {
    let mut p = Projects::default();
    let none = HashSet::new();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    let new = NewProject {
        id: id(),
        title: "Demo".to_owned(),
        repo: "demo".to_owned(),
        target: "main".to_owned(),
        review: None,
        verifier: None,
        push: false,
        ask_to_start: false,
        orchestrator: Some(TermRef { worker: WorkerId::new(), session: SessionId::new() }),
        limits: LimitsChange::default(),
        metadata: None,
        members: Vec::new(),
    };
    p.create(new, &running, at(0)).unwrap();
    p
}

fn message(refused: Outcome) -> String {
    match refused {
        Outcome::Error { message, .. } => message,
        other => panic!("not refused: {other:?}"),
    }
}

fn get(p: &Projects, task: TaskId) -> Task {
    p.task(&id(), task).unwrap().clone()
}

fn checked() -> Advance {
    let run = VerifierRun {
        passed: true,
        summary: "ok".to_owned(),
        head: "a".repeat(40),
        base: "b".repeat(40),
        exit: Some(0),
        took_ms: 1_000,
    };
    Advance {
        state: Some(TaskState::Done),
        verified: Some(run),
        merge: Queue::Set(Merge::Queued { since_ms: at(3) }),
        ..Advance::default()
    }
}

/// The agent of `task` in a worktree of its own at `path`.
fn in_worktree(p: &mut Projects, task: TaskId, path: &str) -> TermRef {
    let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
    let worktree = Worktree {
        name: format!("slopty-demo-{task}"),
        path: path.to_owned(),
        branch: Some(format!("worktree-slopty-demo-{task}")),
        original_cwd: "/w/demo".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session: term.session, pr: None, worktree: Some(worktree) };
    let who = Assignee {
        term,
        spawned: true,
        branch: Some(&branch),
        conversation: None,
        placed: None,
        thread: None,
    };
    p.assign(&id(), task, who, &HashSet::from([term]), at(2)).unwrap();
    term
}

/// Attempts carry their task's brief and verifier and own nothing, the task holding its
/// paths; an attempt neither claims paths nor has attempts of its own, and a task has at most
/// [`ATTEMPTS_MAX`]. Verified, an attempt not picked stays out of the queue and cannot be
/// merged. Picking one gives up the other, whose worktree is freed keeping the branch, and
/// queues the one picked, its work checked already; once it merges, so is its task. A pick
/// is made once.
#[test]
fn an_attempt_waits_outside_the_queue_until_picked_and_merging_it_merges_its_task() {
    let mut p = projects();
    let spec = TaskSpec {
        title: "Server".to_owned(),
        brief: "Build it.".to_owned(),
        owns: vec!["crates/slopty-server".to_owned()],
        verifier: Some("cargo nextest run".to_owned()),
        ..TaskSpec::default()
    };
    let task = p.create_task(&id(), spec, at(0)).unwrap().0.id;
    let (tried, _) = p.attempt(&id(), task, 2, at(1)).unwrap();
    let [a, b] = tried[..] else { panic!("{tried:?}") };
    let made = get(&p, a);
    assert_eq!((made.kind.as_str(), made.parent), (ATTEMPT_KIND, Some(task)));
    assert_eq!(
        (made.brief.as_str(), made.verifier.as_deref()),
        ("Build it.", Some("cargo nextest run"))
    );
    assert!(made.owns.is_empty(), "every attempt writes the same paths, so none owns them");
    assert_eq!(made.title, "Attempt 1: Server");
    assert_eq!(get(&p, task).state, TaskState::Running);
    assert_eq!(get(&p, task).attempts, Some(Attempts { tried: vec![a, b], picked: None }));

    let claimed = p.claim(&id(), a, &["crates/slopty-server/src".to_owned()], at(2));
    assert!(message(claimed.unwrap_err()).contains("holds the paths"));
    let nested = p.attempt(&id(), a, 1, at(2)).unwrap_err();
    assert!(message(nested).contains(&format!("is an attempt at task {task}")));
    let many = p.attempt(&id(), task, ATTEMPTS_MAX - 1, at(2)).unwrap_err();
    assert!(message(many).contains(&format!("1 to {ATTEMPTS_MAX} attempts")));

    let lost_term = in_worktree(&mut p, b, "/w/demo/.claude/worktrees/b");
    for attempt in [a, b] {
        p.advance(&id(), attempt, checked(), at(3)).unwrap();
        assert_eq!(get(&p, attempt).state, TaskState::Done);
    }
    assert!(p.queue(&id()).is_empty(), "no attempt joins the queue unpicked");
    assert!(message(p.may_merge(&id(), a).unwrap_err()).contains("not picked"));

    let (picked, _) = p.pick(&id(), a, at(4)).unwrap();
    assert_eq!(picked.task.attempts.as_ref().and_then(|t| t.picked), Some(a));
    assert_eq!(picked.lost, [(b, Some((lost_term, true)))]);
    assert!(picked.queued, "its work was checked already");
    assert_eq!(p.queue(&id()), [a]);
    let lost = get(&p, b);
    assert_eq!(lost.state, TaskState::Failed);
    assert!(lost.status.is_some_and(|s| s.contains(&format!("attempt {a} lands"))));
    let freed = p.to_free(&id(), b);
    let landed = vec!["main".to_owned(), "origin/main".to_owned()];
    assert_eq!(freed, Some(("/w/demo/.claude/worktrees/b".to_owned(), landed)));
    assert!(message(p.advance(&id(), b, checked(), at(5)).unwrap_err()).contains("given up"));
    assert!(message(p.pick(&id(), b, at(5)).err().unwrap()).contains("picked already"));
    assert!(p.pick(&id(), a, at(5)).unwrap().0.lost.is_empty(), "picked again: nothing more");
    assert!(message(p.attempt(&id(), task, 1, at(5)).unwrap_err()).contains("picked already"));

    let merge = Merge::Merged {
        target: "main".to_owned(),
        head: "c".repeat(40),
        at_ms: at(6),
        pushed: false,
        push_failed: None,
    };
    let landing = Advance {
        state: Some(TaskState::Merged),
        merge: Queue::Set(merge.clone()),
        ..Advance::default()
    };
    let (_, changes) = p.advance(&id(), a, landing, at(6)).unwrap();
    assert_eq!(changes.len(), 2, "the attempt and its task");
    let landed = get(&p, task);
    assert_eq!((landed.state, landed.merge), (TaskState::Merged, Some(merge)));
}

/// A task with an agent of its own is tried by no attempt, nor is a task tried started
/// itself.
#[test]
fn attempts_try_a_task_no_agent_works_on() {
    let mut p = projects();
    let spec = TaskSpec { title: "Server".to_owned(), ..TaskSpec::default() };
    let worked = p.create_task(&id(), spec.clone(), at(0)).unwrap().0.id;
    in_worktree(&mut p, worked, "/w/demo/w");
    assert!(message(p.attempt(&id(), worked, 2, at(1)).unwrap_err()).contains("agent of its own"));

    let tried = p.create_task(&id(), spec, at(0)).unwrap().0.id;
    p.attempt(&id(), tried, 1, at(1)).unwrap();
    let none = HashSet::new();
    let running = Running { terminals: &none, agents: &none, starting: &[] };
    let start = p.may_start(&id(), tried, false, &running).unwrap_err();
    assert!(message(start).contains("is tried by attempts"));
}
