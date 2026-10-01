//! The mirror and what the board derives from it, without a window.

use slopty_core::{SessionId, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Moment, Native, NativeAgent, NativeChange, NativeTask, ProjectUpdate, ProjectsPart, StepKind,
    StepState, TaskId, TaskState,
};

use super::fixtures::{self, AT, card, entry, on, project, snapshot, status, task_changed};
use super::model::{Lane, Projects, TIMELINE_KEPT, TreeRow, moment_line, state_word};

fn one(tasks: Vec<slopty_proto::project::TaskCard>) -> Projects {
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(10, vec![status(project("board", None), tasks, Vec::new())]));
    mirror
}

fn board(mirror: &Projects) -> &super::model::Board {
    mirror.get(&fixtures::id("board")).expect("the project")
}

/// The first part of a snapshot replaces what the client had; a later part adds tasks to a
/// project it already began; a change the snapshot holds is dropped, and a later one lands.
#[test]
fn a_snapshot_replaces_and_an_older_change_is_dropped() {
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(5, vec![status(project("old", None), Vec::new(), Vec::new())]));
    let first = ProjectsPart {
        seq: 10,
        first: true,
        last: false,
        projects: vec![status(
            project("board", None),
            vec![card(1, "a", TaskState::Planned, None)],
            Vec::new(),
        )],
    };
    let rest = ProjectsPart {
        seq: 10,
        first: false,
        last: true,
        projects: vec![status(
            project("board", None),
            vec![card(2, "b", TaskState::Planned, None)],
            Vec::new(),
        )],
    };
    mirror.apply_part(first);
    mirror.apply_part(rest);
    assert!(mirror.get(&fixtures::id("old")).is_none(), "the first part replaces");
    assert_eq!(board(&mirror).tasks.len(), 2, "a later part adds to the project it began");

    let stale = task_changed("board", card(1, "a", TaskState::Failed, None), None);
    assert_eq!(mirror.apply_update(10, stale), None, "at the snapshot's seq: already in it");
    assert_eq!(board(&mirror).tasks[&TaskId(1)].state, TaskState::Planned);
    let fresh = task_changed("board", card(1, "a", TaskState::Running, None), None);
    assert_eq!(mirror.apply_update(11, fresh), Some(fixtures::id("board")));
    assert_eq!(board(&mirror).tasks[&TaskId(1)].state, TaskState::Running);

    let unknown = task_changed("never-heard", card(1, "x", TaskState::Running, None), None);
    assert_eq!(
        mirror.apply_update(12, unknown),
        None,
        "a stranger without its record is not made up"
    );
    let made = ProjectUpdate {
        project: fixtures::id("new"),
        record: Some(project("new", None)),
        task: None,
        native: None,
        entry: Some(entry(1, None, Moment::Created)),
    };
    assert!(mirror.apply_update(13, made).is_some());
    assert_eq!(mirror.get(&fixtures::id("new")).map(|b| b.timeline.len()), Some(1));
}

/// The timeline keeps each entry once, in order, and its latest [`TIMELINE_KEPT`].
#[test]
fn the_timeline_keeps_each_entry_once_and_its_latest() {
    let mut mirror = one(Vec::new());
    let push = |mirror: &mut Projects, seq: u64, n: u64| {
        let update = ProjectUpdate {
            project: fixtures::id("board"),
            record: None,
            task: None,
            native: None,
            entry: Some(entry(n, None, Moment::Note { text: format!("note {n}") })),
        };
        mirror.apply_update(seq, update);
    };
    push(&mut mirror, 11, 1);
    push(&mut mirror, 12, 1);
    assert_eq!(board(&mirror).timeline.len(), 1, "once");
    for n in 2..=(TIMELINE_KEPT as u64 + 10) {
        push(&mut mirror, 11 + n, n);
    }
    let kept = &board(&mirror).timeline;
    assert_eq!(kept.len(), TIMELINE_KEPT);
    assert_eq!(kept.back().map(|e| e.seq), Some(TIMELINE_KEPT as u64 + 10));
    assert!(kept.iter().zip(kept.iter().skip(1)).all(|(a, b)| a.seq < b.seq), "in order");
}

/// The tree is depth first from the orchestrator, children by number; a task whose parent is
/// not on the board hangs from the orchestrator rather than falling out.
#[test]
fn the_tree_runs_depth_first_from_the_orchestrator() {
    let mirror = one(vec![
        card(1, "wire", TaskState::Running, None),
        card(2, "store", TaskState::Done, None),
        card(3, "goldens", TaskState::Planned, Some(1)),
        card(4, "fuzz", TaskState::Planned, Some(1)),
        card(5, "orphan", TaskState::Planned, Some(99)),
    ]);
    let rows: Vec<(Option<u32>, usize, bool)> = board(&mirror)
        .tree()
        .into_iter()
        .map(|r: TreeRow| (r.task.map(|t| t.0), r.depth, r.last))
        .collect();
    assert_eq!(
        rows,
        [
            (None, 0, true),
            (Some(1), 1, false),
            (Some(3), 2, false),
            (Some(4), 2, true),
            (Some(2), 1, false),
            (Some(5), 1, true),
        ]
    );
}

/// A parent stands in the lane of its most urgent descendant: a finished parent whose child
/// waits on the person is under "Needs you" with it. Lanes with nothing are left out.
#[test]
fn a_parent_stands_where_its_most_urgent_descendant_does() {
    let mirror = one(vec![
        card(1, "parent", TaskState::Done, None),
        card(2, "child", TaskState::Running, Some(1)),
        card(3, "grandchild", TaskState::Blocked, Some(2)),
        card(4, "alone", TaskState::Merged, None),
        card(5, "next", TaskState::Planned, None),
    ]);
    let b = board(&mirror);
    let lanes: Vec<(Lane, Vec<u32>)> =
        b.lanes().into_iter().map(|(l, t)| (l, t.into_iter().map(|t| t.0).collect())).collect();
    assert_eq!(
        lanes,
        [(Lane::NeedsYou, vec![1, 2, 3]), (Lane::UpNext, vec![5]), (Lane::Merged, vec![4])]
    );
    assert_eq!(b.needs_you(), [TaskId(3)], "the band names only the one that waits");
}

/// A task waits on the dependencies not yet done or merged.
#[test]
fn a_task_waits_on_what_is_not_done() {
    let mut blocked = card(3, "after", TaskState::Planned, None);
    blocked.depends_on = vec![TaskId(1), TaskId(2), TaskId(9)];
    let mirror = one(vec![
        card(1, "done", TaskState::Merged, None),
        card(2, "running", TaskState::Running, None),
        blocked,
    ]);
    assert_eq!(board(&mirror).waiting_on(TaskId(3)), [TaskId(2), TaskId(9)]);
}

/// A native leaf's change moves its node's counts: a subagent that starts and stops, a
/// to-do made and done, and one that started before the snapshot counted it.
#[test]
fn a_native_moves_its_nodes_counts() {
    let (worker, session) = (WorkerId::new(), SessionId::new());
    let mut running = on(card(1, "wire", TaskState::Running, None), worker, session);
    running.natives.agents = 1;
    running.natives.running = 1;
    let mut mirror = one(vec![running]);
    let native = |seq: u64, mirror: &mut Projects, native: Native| {
        let update = ProjectUpdate {
            project: fixtures::id("board"),
            record: None,
            task: None,
            native: Some(NativeChange { task: Some(TaskId(1)), native }),
            entry: None,
        };
        mirror.apply_update(seq, update);
    };
    let agent = |id: &str, stopped: bool| {
        Native::Agent(NativeAgent {
            id: id.to_owned(),
            kind: "Explore".to_owned(),
            started_ms: AT,
            stopped_ms: stopped.then_some(AT),
            transcript: None,
            last: None,
        })
    };
    native(11, &mut mirror, agent("new", false));
    let counts = |m: &Projects| board(m).tasks[&TaskId(1)].natives;
    assert_eq!((counts(&mirror).agents, counts(&mirror).running), (2, 2));
    native(12, &mut mirror, agent("new", true));
    native(13, &mut mirror, agent("before", true));
    assert_eq!((counts(&mirror).agents, counts(&mirror).running), (2, 0));
    let todo = |done| Native::Todo(NativeTask { id: "t".into(), subject: "Read".into(), done });
    native(14, &mut mirror, todo(false));
    native(15, &mut mirror, todo(true));
    assert_eq!((counts(&mirror).todos, counts(&mirror).done), (1, 1));
    assert_eq!(board(&mirror).natives[&Some(TaskId(1))].agents.len(), 2, "the leaves are kept");
}

/// Which session belongs to which node: the orchestrator's, a task's live terminal, and none
/// once a terminal has ended; and who works in a terminal, in words.
#[test]
fn a_session_finds_its_node() {
    let (worker, orchestrator, agent, ended) =
        (WorkerId::new(), SessionId::new(), SessionId::new(), SessionId::new());
    let mut gone = on(card(2, "gone", TaskState::Done, None), worker, ended);
    if let Some(a) = gone.assignment.as_mut() {
        a.ended_ms = Some(AT);
    }
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(
        1,
        vec![status(
            project("board", Some(TermRef { worker, session: orchestrator })),
            vec![on(card(1, "live", TaskState::Running, None), worker, agent), gone],
            Vec::new(),
        )],
    ));
    assert!(mirror.of_orchestrator(orchestrator).is_some());
    assert_eq!(mirror.of_agent(agent).map(|(_, t)| t), Some(TaskId(1)));
    assert!(mirror.of_agent(ended).is_none(), "an ended terminal is not the task's any more");
    let b = board(&mirror);
    assert_eq!(b.terminal(None), Some((worker, orchestrator)));
    assert_eq!(b.terminal(Some(TaskId(2))), None);
    assert_eq!(b.worker(Some(TaskId(2))), Some(worker), "where it ran last still shows");
    assert_eq!(b.live(), 2, "the orchestrator and the live task");
    let at = |session| b.agent_at(TermRef { worker, session });
    assert_eq!(at(orchestrator), "the orchestrator");
    assert_eq!(at(agent), "#1's agent");
    assert_eq!(at(ended), "#2's agent", "a report may reach an agent before its task moves on");
    assert_eq!(at(SessionId::new()), "an agent");
}

/// Every moment reads as a sentence, with the worker's name where it has one.
#[test]
fn every_moment_reads_as_a_sentence() {
    let (worker, session) = (WorkerId::new(), SessionId::new());
    let term = TermRef { worker, session };
    let name = |_: WorkerId| "studio".to_owned();
    let agent =
        |t: TermRef| if t == term { "the orchestrator".to_owned() } else { "an agent".to_owned() };
    let say = |what| moment_line(&entry(1, Some(1), what), name, agent);
    assert_eq!(
        say(Moment::Delivered { term, reports: 1 }),
        "A report delivered to the orchestrator"
    );
    assert_eq!(
        say(Moment::Delivered { term, reports: 3 }),
        "3 reports delivered to the orchestrator"
    );
    assert_eq!(say(Moment::Assigned { term, spawned: true }), "Started on studio");
    assert_eq!(
        say(Moment::State { from: TaskState::Running, to: TaskState::Blocked }),
        "Needs you"
    );
    assert_eq!(
        say(Moment::Verified(slopty_proto::project::VerifierRun {
            passed: false,
            summary: "Compiling a\nerror: could not compile `a`\n\n".into(),
            head: "abcdef0123".into(),
            base: "0123456789".into(),
            exit: Some(101),
            took_ms: 9000,
        })),
        "Verifier failed at abcdef0: error: could not compile `a`"
    );
    assert_eq!(
        say(Moment::Branch { branch: Some("slopty/board/1".into()), pr: Some(7) }),
        "On slopty/board/1, pull request #7"
    );
    assert_eq!(
        say(Moment::Claimed { paths: vec!["crates/a".into(), "crates/b".into(), "docs".into()] }),
        "Owns crates/a and 2 more"
    );
    let step = |kind, state| {
        Moment::Step(slopty_proto::project::TaskStep {
            kind,
            worker,
            state,
            since_ms: AT,
            term: None,
        })
    };
    let running = |phase: &str, percent| StepState::Running { phase: phase.into(), percent };
    assert_eq!(say(step(StepKind::Clone, running("Starting", None))), "Cloning on studio");
    assert_eq!(
        say(step(StepKind::Clone, running("Receiving objects", Some(45)))),
        "Cloning on studio: Receiving objects 45%"
    );
    assert_eq!(
        say(step(StepKind::Clone, StepState::Failed { why: "fatal: denied\nmore".into() })),
        "Clone on studio failed: fatal: denied"
    );
    assert_eq!(
        say(step(StepKind::Home, running("Sending", Some(40)))),
        "Bringing its branch to studio: 40%"
    );
    let detail = "worktree-slopty-board-1 as slopty/board/1 at 4a7aa6d in /w/board".to_owned();
    assert_eq!(
        say(step(StepKind::Home, StepState::Done { detail })),
        "Branch arrived on studio: worktree-slopty-board-1 as slopty/board/1 at 4a7aa6d in /w/board"
    );
    let words = [
        TaskState::Planned,
        TaskState::Running,
        TaskState::Waiting,
        TaskState::Blocked,
        TaskState::Verifying,
        TaskState::Done,
        TaskState::Merged,
        TaskState::Failed,
    ]
    .map(state_word);
    let lanes = Lane::ALL.map(Lane::title);
    for text in words.iter().chain(&lanes).chain(&[
        super::view::NO_TASKS,
        super::view::NEEDS_YOU,
        super::view::ORCHESTRATOR,
        super::view::CREATED,
        super::view::PROJECT_GONE,
    ]) {
        let mut chars = text.chars();
        assert!(chars.next().is_some_and(char::is_uppercase), "starts lowercase: {text:?}");
        let rest: String = chars.collect();
        assert!(!rest.chars().any(char::is_uppercase), "title case: {text:?}");
    }
}

/// As many lanes stand side by side as fit at the zoom they are drawn at, one at the least and
/// never more than there are lanes.
#[test]
fn as_many_lanes_stand_across_as_fit_at_the_zoom() {
    use super::view::lanes_across;
    assert_eq!(lanes_across(480.0, 1.0), 2);
    assert_eq!(lanes_across(400.0, 1.0), 1);
    assert_eq!(lanes_across(0.0, 1.0), 1, "before the first layout");
    assert_eq!(lanes_across(480.0, 2.0), 1, "a zoomed board's lanes are wider");
    assert_eq!(lanes_across(960.0, 2.0), 2);
    assert_eq!(lanes_across(1200.0, 1.0), 5);
    assert_eq!(lanes_across(10_000.0, 1.0), 7, "one column per lane at most");
}

/// A board unchanged by an update keeps its address, so handing it over again costs a pointer.
#[test]
fn an_update_copies_only_the_board_it_touches() {
    let mut mirror = one(vec![card(1, "Wire the board", TaskState::Running, None)]);
    let mut later = snapshot(10, vec![status(project("other", None), Vec::new(), Vec::new())]);
    later.first = false;
    let touched = mirror.apply_part(later);
    assert_eq!(touched, [fixtures::id("other")]);
    let held = mirror.get(&fixtures::id("board")).cloned();
    let untouched = mirror.get(&fixtures::id("other")).cloned();
    let changed = task_changed("board", card(1, "Wire the board", TaskState::Blocked, None), None);
    assert!(mirror.apply_update(11, changed).is_some());
    let (Some(held), Some(untouched)) = (held, untouched) else {
        panic!("both projects are mirrored")
    };
    let now = mirror.get(&fixtures::id("board")).cloned();
    assert!(now.is_some_and(|b| !std::sync::Arc::ptr_eq(&b, &held)), "the touched board is a copy");
    assert!(
        held.tasks.get(&TaskId(1)).is_some_and(|c| c.state == TaskState::Running),
        "a held board stays as handed"
    );
    let still = mirror.get(&fixtures::id("other")).cloned();
    assert!(
        still.is_some_and(|b| std::sync::Arc::ptr_eq(&b, &untouched)),
        "the other board is shared still"
    );
}

/// Ready to merge is the merge queue: it runs in the order tasks joined it, so the one merging
/// first, with a done task the queue does not hold after them. Verifying puts the run under way
/// first. A pass speaks while its task waits to merge and a failure until the task is verified
/// again or merged; each says the commits it judged, how it ended and its last lines.
#[test]
fn the_queue_runs_in_its_order_and_a_verdict_speaks_while_it_holds() {
    use super::fixtures::{queued, run, step};
    use super::model::{queue_words, verdict_detail, verdict_tail};
    let worker = WorkerId::new();
    let merging = {
        let mut c = queued(card(4, "d", TaskState::Done, None), 5, "4444444");
        let phase = StepState::Running { phase: "Rebasing onto main".into(), percent: None };
        c.step = Some(step(StepKind::Merge, worker, phase, None));
        c
    };
    let mut verifying = card(6, "f", TaskState::Verifying, None);
    let line = StepState::Running { phase: "Compiling".into(), percent: None };
    verifying.step = Some(step(StepKind::Verify, worker, line, None));
    verifying.updated_ms = slopty_core::WallMs::from_millis(AT.as_millis().saturating_add(50));
    let mut failed = card(7, "g", TaskState::Waiting, None);
    failed.verified = Some(run(false, "7777777abc"));
    let mirror = one(vec![
        queued(card(1, "a", TaskState::Done, None), 20, "1111111"),
        queued(card(2, "b", TaskState::Done, None), 10, "2222222"),
        card(3, "c", TaskState::Done, None),
        merging,
        card(5, "e", TaskState::Verifying, None),
        verifying,
        failed,
    ]);
    let b = board(&mirror);
    let lanes = b.lanes();
    let lane = |l: Lane| lanes.iter().find(|(x, _)| *x == l).map(|(_, t)| t.clone()).unwrap();
    assert_eq!(lane(Lane::ReadyToMerge), [TaskId(4), TaskId(2), TaskId(1), TaskId(3)]);
    assert_eq!(lane(Lane::Verifying), [TaskId(6), TaskId(5)], "the run under way first");
    assert_eq!(b.queue_place(TaskId(2)), Some((2, 3)));
    assert_eq!(b.queue_place(TaskId(3)), None, "done, and not asked to merge");
    assert_eq!(
        (queue_words(1), queue_words(2), queue_words(13)),
        ("Next to merge".to_owned(), "2nd to merge".to_owned(), "13th to merge".to_owned())
    );

    assert!(b.verdict(TaskId(1)).is_some_and(|r| r.passed), "a pass while it waits to merge");
    assert!(b.verdict(TaskId(7)).is_some_and(|r| !r.passed), "a failure until it is judged again");
    let pass = run(true, "4a7aa6d0");
    assert_eq!(verdict_detail(&pass), "4a7aa6d over c08d4c1 \u{b7} 1m 12s");
    let fail = run(false, "4a7aa6d0");
    assert_eq!(verdict_detail(&fail), "4a7aa6d over c08d4c1 \u{b7} exit 101 \u{b7} 1m 12s");
    assert_eq!(
        verdict_tail(&fail, 2),
        ["--> src/project/view.rs:12:5", "error: could not compile `slopty-ui`"],
        "its last lines that say anything"
    );

    let mut again = one(vec![{
        let mut c = card(1, "a", TaskState::Verifying, None);
        c.verified = Some(run(false, "1111111"));
        c
    }]);
    assert!(board(&again).verdict(TaskId(1)).is_none(), "judged again, the old word is gone");
    again.apply_update(
        11,
        task_changed(
            "board",
            {
                let mut c = card(1, "a", TaskState::Running, None);
                c.verified = Some(run(true, "1111111"));
                c
            },
            None,
        ),
    );
    assert!(board(&again).verdict(TaskId(1)).is_none(), "a pass for work since moved on");
}
