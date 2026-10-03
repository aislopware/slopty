//! The mirror and what the board derives from it, without a window.

use slopty_core::{SessionId, WallMs, WorkerId};
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
        say(Moment::Needs { names: vec!["Apple work".into(), "Linux first".into()] }),
        "Needs: Apple work, Linux first"
    );
    assert_eq!(say(Moment::Needs { names: Vec::new() }), "Needs nothing of its workers");
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

/// More lanes than columns: neighbouring short lanes share a column, in their order, so the
/// tallest column is as short as it can be and the split is as even as that allows; fewer
/// lanes than columns stand one a column.
#[test]
fn short_lanes_stack_so_every_lane_stands_in_the_first_screenful() {
    use super::view::stack_lanes;
    // The showcase's board: needs you, failed, working, up next, ready to merge, merged.
    let showcase = [8, 3, 9, 19, 10, 3];
    assert_eq!(stack_lanes(&showcase, 4), [2, 1, 1, 2], "the short ones join their neighbours");
    assert_eq!(stack_lanes(&showcase, 6), [1; 6]);
    assert_eq!(stack_lanes(&showcase, 9), [1; 6], "no more columns than lanes");
    assert_eq!(stack_lanes(&showcase, 1), [6], "one column holds them all");
    assert_eq!(stack_lanes(&showcase, 0), [6], "before the first layout");
    assert_eq!(stack_lanes(&[5, 5, 5], 2), [1, 2], "a tie keeps the first split");
    assert_eq!(stack_lanes(&[1, 1, 30], 2), [2, 1], "the tall one stands alone");
    assert_eq!(stack_lanes(&[], 3), Vec::<usize>::new());
    for columns in 1..=7 {
        let sizes = stack_lanes(&[4, 2, 7, 1, 9, 3, 5], columns);
        assert_eq!(sizes.len(), columns);
        assert_eq!(sizes.iter().sum::<usize>(), 7, "every lane once: {sizes:?}");
        assert!(sizes.iter().all(|n| *n > 0), "no empty column: {sizes:?}");
    }
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
    verifying.updated_ms = WallMs::from_millis(AT.as_millis().saturating_add(50));
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

/// A reviewer's word speaks as a verifier's does: an approval while its task waits to merge,
/// changes asked until the work is checked again or merged. Each says the commits it read,
/// who spoke when it was the person, what it found and how long it read; a finding points at
/// its path and line as a compiler does.
#[test]
fn a_review_speaks_while_it_holds_and_says_what_it_found() {
    use super::fixtures::{queued, review};
    use super::model::{finding_place, review_detail, review_line};
    let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
    let approved = {
        let mut c = queued(card(1, "a", TaskState::Done, None), 5, "1111111");
        c.reviewed = Some(review(true, "1111111", Some(term)));
        c
    };
    let asked = {
        let mut c = card(2, "b", TaskState::Waiting, None);
        c.reviewed = Some(review(false, "2222222", Some(term)));
        c
    };
    let again = {
        let mut c = card(3, "c", TaskState::Verifying, None);
        c.reviewed = Some(review(false, "3333333", Some(term)));
        c
    };
    let mirror = one(vec![approved, asked, again]);
    let b = board(&mirror);
    assert!(b.review(TaskId(1)).is_some_and(|r| r.verdict.approved), "while it waits to merge");
    assert!(b.review(TaskId(2)).is_some_and(|r| !r.verdict.approved), "until it is read again");
    assert!(b.review(TaskId(3)).is_none(), "checked again, the old word is gone");

    let asked = review(false, "4a7aa6d0", Some(term));
    assert_eq!(review_detail(&asked), "4a7aa6d over c08d4c1 \u{b7} 1 blocking of 2 \u{b7} 1m 35s");
    let mut yours = review(true, "4a7aa6d0", None);
    yours.verdict.findings.clear();
    yours.took_ms = 0;
    assert_eq!(review_detail(&yours), "4a7aa6d over c08d4c1 \u{b7} by you");
    assert_eq!(
        review_line(&asked),
        "Reviewer asked for changes at 4a7aa6d: Project.review has no golden, so a wire change \
         would pass unseen."
    );
    assert_eq!(review_line(&yours), "You approved 4a7aa6d");
    let places: Vec<Option<String>> = asked.verdict.findings.iter().map(finding_place).collect();
    assert_eq!(
        places,
        [
            Some("crates/slopty-proto/src/project.rs:431".to_owned()),
            Some("docs/decisions/projects.md".to_owned())
        ]
    );
}

/// What the person can do to a task from the board: merge what is done and not queued, retry
/// a step that failed on the way to the target, approve over a reviewer that asked for changes
/// or said nothing once the verifier passed, and nothing to a task that reads or has merged.
#[test]
fn a_task_offers_what_moves_it_on() {
    use super::fixtures::{queued, review, run, step};
    use super::model::TaskAction;

    let worker = WorkerId::new();
    let failed = |kind| StepState::Failed { why: format!("{kind:?} broke") };
    let done = card(1, "Done, nothing queued it", TaskState::Done, None);
    let in_queue = queued(card(2, "In the queue", TaskState::Done, None), 5, "2222222");
    let mut verify = card(3, "Its verifier failed", TaskState::Waiting, None);
    verify.verified = Some(run(false, "3333333"));
    verify.step = Some(step(StepKind::Verify, worker, failed(StepKind::Verify), None));
    let mut asked = card(4, "Changes asked", TaskState::Waiting, None);
    asked.verified = Some(run(true, "4444444"));
    asked.reviewed = Some(review(false, "4444444", None));
    let mut silent = card(5, "The reviewer said nothing", TaskState::Waiting, None);
    silent.verified = Some(run(true, "5555555"));
    silent.step = Some(step(StepKind::Review, worker, failed(StepKind::Review), None));
    let mut unverified = card(6, "Changes asked, not verified", TaskState::Waiting, None);
    unverified.reviewed = Some(review(false, "6666666", None));
    let mut reads = card(7, "Reads only", TaskState::Done, None);
    reads.read_only = true;
    let merged = card(8, "Merged", TaskState::Merged, None);
    let mut clone = card(9, "Its clone failed", TaskState::Waiting, None);
    clone.step = Some(step(StepKind::Clone, worker, failed(StepKind::Clone), None));
    let mut merge = card(10, "Its merge failed", TaskState::Done, None);
    merge.step = Some(step(StepKind::Merge, worker, failed(StepKind::Merge), None));
    merge.merge = Some(slopty_proto::project::Merge::Queued { since_ms: AT });
    let mirror =
        one(vec![done, in_queue, verify, asked, silent, unverified, reads, merged, clone, merge]);
    let b = board(&mirror);
    let of = |n| b.actions(TaskId(n));
    assert_eq!(of(1), [TaskAction::Merge]);
    assert_eq!(of(2), [], "the queue already holds it");
    assert_eq!(of(3), [TaskAction::Retry]);
    assert_eq!(of(4), [TaskAction::Approve]);
    assert_eq!(of(5), [TaskAction::Retry, TaskAction::Approve]);
    assert_eq!(of(6), [], "the project's verifier has not passed it");
    assert_eq!(of(7), [], "a task that only reads has nothing to land");
    assert_eq!(of(8), []);
    assert_eq!(of(9), [], "a clone is the worker's to make again, not the merge queue's");
    assert_eq!(of(10), [TaskAction::Retry]);
    assert_eq!(of(99), [], "no such task");
    assert_eq!(TaskAction::Merge.selector("project-card", TaskId(1)), "project-card-merge-1");
}

/// A plan's estimate is the median time at work of the project's finished tasks of the kind
/// when there are any, else of every kind; with none finished there is no estimate. A proposed task
/// offers its start, and choosing where it starts.
#[test]
fn a_plan_is_estimated_from_the_tasks_that_finished() {
    use slopty_proto::project::{Proposed, Spent};

    use super::model::{Estimate, TaskAction};
    let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
    let worked = |mut c: slopty_proto::project::TaskCard, min: u64| {
        c.spent = Spent { active_ms: min * 60_000, since_ms: None };
        c
    };
    let mut review = worked(card(3, "Read it", TaskState::Done, None), 90);
    review.kind = "review".to_owned();
    let mut proposed = card(4, "Next", TaskState::Planned, None);
    proposed.proposed = Some(Proposed {
        since_ms: AT,
        runs: "claude".to_owned(),
        on: Some(term.worker),
        why: String::new(),
    });
    let tasks = vec![
        worked(card(1, "Build a", TaskState::Done, None), 10),
        worked(card(2, "Build b", TaskState::Merged, None), 20),
        review,
        proposed,
        worked(card(5, "Still going", TaskState::Running, None), 500),
    ];
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(10, vec![status(project("board", None), tasks, Vec::new())]));
    let b = board(&mirror);
    let build = b.estimate("build").expect("two builds finished");
    assert_eq!(build, Estimate { each_ms: 20 * 60_000, from: 2, same_kind: true });
    assert_eq!(build.line(), "about 20 min of work each, from 2 finished of the kind");
    let bench = b.estimate("bench").expect("from every kind");
    assert_eq!((bench.each_ms, bench.from, bench.same_kind), (20 * 60_000, 3, false));
    assert_eq!(b.proposed(), [TaskId(4)]);
    assert_eq!(b.actions(TaskId(4)), [TaskAction::Start, TaskAction::RunOn]);
    let empty = one(Vec::new());
    assert_eq!(board(&empty).estimate("build"), None);
}

/// A recap reads only what came after the last look, tells each kind once with its tasks in
/// the order they last moved, puts what needs the person first, and leaves out an agent that
/// ended once its work merged. A line names up to three tasks and counts the rest.
#[test]
fn a_recap_tells_what_needs_you_first_and_names_its_tasks() {
    use super::fixtures::run;
    use super::recap::{Looked, Recap, RecapKind};

    let mut merged = card(4, "Write the decision", TaskState::Merged, None);
    merged.assignment = None;
    let mirror = one(vec![
        merged,
        card(5, "Check the goldens", TaskState::Waiting, None),
        card(6, "Draw the lanes", TaskState::Waiting, None),
    ]);
    let board = board(&mirror);
    let term = TermRef { worker: WorkerId::new(), session: SessionId::new() };
    let to = |to| Moment::State { from: TaskState::Running, to };
    let timeline = [
        entry(1, Some(5), Moment::Verified(run(false, "1111111"))),
        entry(2, Some(4), to(TaskState::Merged)),
        entry(3, Some(4), Moment::AgentGone { term }),
        entry(4, Some(6), Moment::AgentGone { term }),
        entry(5, Some(5), Moment::Verified(run(false, "2222222"))),
        entry(6, Some(6), Moment::Verified(run(false, "3333333"))),
        entry(7, None, Moment::Note { text: "the project's own".into() }),
    ];
    let since = Looked { seq: 1, at_ms: AT };
    let recap = Recap::of(board, since, &timeline, false).expect("news");
    let kinds: Vec<(RecapKind, Vec<u32>)> =
        recap.lines.iter().map(|l| (l.kind, l.tasks.iter().map(|t| t.0).collect())).collect();
    assert_eq!(
        kinds,
        [
            (RecapKind::VerifyFailed, vec![5, 6]),
            (RecapKind::AgentEnded, vec![6]),
            (RecapKind::Merged, vec![4]),
        ],
        "the failure before the look is not told again; #4's agent ended as it should"
    );
    let texts: Vec<String> = recap.lines.iter().map(|l| l.text(board)).collect();
    assert_eq!(
        texts,
        [
            "Verifier failed on #5 and #6",
            "Agent ended on #6 Draw the lanes",
            "Merged #4 Write the decision"
        ]
    );
    assert!(RecapKind::VerifyFailed.needs_you() && !RecapKind::Merged.needs_you());

    let created: Vec<_> = (10..15)
        .map(|n| entry(u64::from(n), Some(n), Moment::TaskCreated { title: format!("t{n}") }))
        .collect();
    let recap = Recap::of(board, since, &created, false).expect("news");
    assert_eq!(recap.lines[0].text(board), "Created #10, #11, #12 and 2 more");
    assert_eq!(Recap::of(board, Looked { seq: 7, at_ms: AT }, &timeline, false), None);
}

/// A node's time at work counts its subtree's with its own, the orchestrator's share stays
/// apart, and a stretch under way counts to the moment the board reads it. What the agents'
/// threads say adds cost, context and the plan's rate windows, the fullest of each; a node
/// whose thread is not heard shows its time alone.
#[test]
fn time_and_cost_roll_up_the_tree_with_the_orchestrator_apart() {
    use slopty_proto::project::Spent;
    use slopty_proto::thread::{Limit, Meters};

    use super::spend::{MetersBySession, dollars, limit_line, worked};
    let min = |m: u64| m * 60_000;
    let at = |m: u64| WallMs::from_millis(AT.as_millis() + min(m));
    let worker = WorkerId::new();
    let (orchestrator, parent, child, leaf) =
        (SessionId::new(), SessionId::new(), SessionId::new(), SessionId::new());
    let spent = |c: slopty_proto::project::TaskCard, done: u64, since: Option<u64>| {
        let mut c = c;
        c.spent = Spent { active_ms: min(done), since_ms: since.map(at) };
        c
    };
    let mut record = project("board", Some(TermRef { worker, session: orchestrator }));
    record.orchestrator_spent = Spent { active_ms: min(8), since_ms: None };
    let tasks = vec![
        spent(on(card(1, "Parent", TaskState::Running, None), worker, parent), 12, None),
        spent(on(card(2, "Child", TaskState::Running, Some(1)), worker, child), 20, Some(0)),
        spent(on(card(3, "Grandchild", TaskState::Done, Some(2)), worker, leaf), 5, None),
        spent(card(4, "Apart", TaskState::Planned, None), 0, None),
    ];
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(10, vec![status(record, tasks, Vec::new())]));
    let b = board(&mirror);
    assert!(b.at_work(), "#2's clock runs");
    let none = MetersBySession::new();
    let now = at(10);
    let first = b.spend(Some(TaskId(1)), now, &none);
    assert_eq!((first.own_ms, first.subtree_ms), (min(12), min(12 + 30 + 5)));
    assert!(first.has_subtree() && first.own_cost.is_none() && first.subtree_cost.is_none());
    let lone = b.spend(Some(TaskId(3)), now, &none);
    assert!(!lone.has_subtree());
    let mine = b.spend(None, now, &none);
    assert_eq!((mine.own_ms, mine.subtree_ms), (min(8), min(8)), "its own share alone");
    let all = b.project_spend(now, &none);
    assert_eq!((all.orchestrator_ms, all.tasks_ms, all.total_ms()), (min(8), min(47), min(55)));

    let meters = |cost, used, limits: Vec<Limit>| Meters {
        cost_micro_usd: Some(cost),
        context_tokens: Some(used),
        context_window: Some(200_000),
        limits,
        ..Meters::default()
    };
    let limit = |name: &str, used_bp| Limit { name: name.to_owned(), used_bp, resets_ms: None };
    let heard: MetersBySession = [
        (orchestrator, meters(400_000, 40_000, vec![limit("five-hour", 4_200)])),
        (
            child,
            meters(1_250_000, 170_000, vec![limit("five-hour", 4_400), limit("seven-day", 1_800)]),
        ),
        (leaf, meters(50_000, 10_000, Vec::new())),
    ]
    .into_iter()
    .collect();
    let first = b.spend(Some(TaskId(1)), now, &heard);
    assert_eq!((first.own_cost, first.subtree_cost), (None, Some(1_300_000)));
    let second = b.spend(Some(TaskId(2)), now, &heard);
    assert_eq!(second.context_shown(), Some((8_500, true)), "85% warns");
    assert_eq!(b.spend(Some(TaskId(3)), now, &heard).context_shown(), None, "5% is not shown");
    assert_eq!(b.spend(None, now, &heard).context_shown(), Some((2_000, false)));
    let all = b.project_spend(now, &heard);
    assert_eq!((all.orchestrator_cost, all.tasks_cost), (Some(400_000), Some(1_300_000)));
    assert_eq!(all.limits.iter().map(limit_line).collect::<Vec<_>>(), ["5-hour 44%", "weekly 18%"]);
    assert_eq!(
        [worked(0), worked(min(12)), worked(min(64)), dollars(1_700_000), dollars(4_999)],
        ["under 1m", "12m", "1h 4m", "$1.70", "$0.00"]
    );
}

/// While a task's agent runs, its next step is the person's word to it, first on its row: fix
/// CI for a failed verifier, address the comments a review or its pull request asked for,
/// resolve the conflicts its rebase met. Checking it again unchanged would fail the same way,
/// so Retry waits for an agent that is gone. What is said names what failed and what to do.
#[test]
fn a_running_agent_is_told_its_next_step_in_the_person_s_words() {
    use slopty_proto::agent::{PullRequest, Review};
    use slopty_proto::project::Finding;

    use super::fixtures::{review, run, step};
    use super::model::TaskAction;

    let worker = WorkerId::new();
    let live = |n, title, state| on(card(n, title, state, None), worker, SessionId::new());
    let mut ci = live(1, "Its verifier failed", TaskState::Waiting);
    ci.verified = Some(run(false, "9c1e2f3"));
    ci.step =
        Some(step(StepKind::Verify, worker, StepState::Failed { why: "2 errors".into() }, None));
    let mut asked = live(2, "Changes asked", TaskState::Waiting);
    asked.verified = Some(run(true, "4444444"));
    let mut said = review(false, "4444444", None);
    said.verdict.findings = vec![Finding {
        path: Some("crates/a.rs".into()),
        line: Some(12),
        severity: "high".into(),
        blocking: true,
        body: "This unwraps a None.".into(),
    }];
    asked.reviewed = Some(said);
    let mut pr = live(3, "Its pull request", TaskState::Waiting);
    pr.pr = Some(PullRequest {
        number: 9,
        url: "https://github.com/o/r/pull/9".into(),
        review: Some(Review::ChangesRequested),
        merge_request: false,
    });
    let mut conflict = live(4, "Does not rebase", TaskState::Waiting);
    let why = StepState::Failed { why: "conflicts in a.txt".into() };
    conflict.step = Some(step(StepKind::Rebase, worker, why.clone(), None));
    let mut gone = card(5, "Does not rebase, nobody on it", TaskState::Planned, None);
    gone.step = Some(step(StepKind::Rebase, worker, why, None));
    let mirror = one(vec![ci, asked, pr, conflict, gone]);
    let b = board(&mirror);
    let of = |n| b.actions(TaskId(n));
    assert_eq!(of(1), [TaskAction::FixCi], "no Retry while an agent can fix it");
    assert_eq!(of(2), [TaskAction::AddressComments, TaskAction::Approve]);
    assert_eq!(of(3), [TaskAction::AddressComments]);
    assert_eq!(of(4), [TaskAction::ResolveConflicts]);
    assert_eq!(of(5), [TaskAction::Retry, TaskAction::RunOn], "nobody to tell");
    assert!(TaskAction::FixCi.tells() && !TaskAction::Retry.tells());

    let told = |n, action| b.told(TaskId(n), action).expect("words");
    let fix = told(1, TaskAction::FixCi);
    assert!(fix.starts_with("Fix CI. `cargo gate` failed on your work at 9c1e2f3"), "{fix}");
    assert!(fix.ends_with("report done again with task_report."), "{fix}");
    let address = told(2, TaskAction::AddressComments);
    assert!(address.contains("- blocking, crates/a.rs:12: This unwraps a None."), "{address}");
    let pull = told(3, TaskAction::AddressComments);
    assert!(pull.contains("Pull request #9 has changes requested"), "{pull}");
    let resolve = told(4, TaskAction::ResolveConflicts);
    assert!(resolve.contains("does not rebase onto main: conflicts in a.txt"), "{resolve}");
    assert_eq!(b.told(TaskId(4), TaskAction::FixCi), None, "nothing failed to fix");
    assert_eq!(b.told(TaskId(1), TaskAction::Merge), None);
}

/// Once a task's work is on its way, one row says each stage of it: its branch, what its
/// verifier and reviewer said, its place in the queue, its pull request with its own checks,
/// and the to-dos still open, which hold the merge back. Failing checks are CI to fix.
#[test]
fn a_task_s_pipeline_says_each_stage_and_open_to_dos_hold_the_merge() {
    use slopty_proto::agent::PullRequest;
    use slopty_proto::project::{Checks, ChecksState, NativeCounts};

    use super::fixtures::{queued, review};
    use super::model::{StageKind, TaskAction};

    let worker = WorkerId::new();
    let mut ahead = queued(card(1, "Ahead", TaskState::Done, None), 10, "1111111");
    ahead.branch = Some("worktree-ahead".into());
    let mut piped = queued(card(2, "Its pull request", TaskState::Done, None), 20, "2222222");
    piped.reviewed = Some(review(true, "2222222", None));
    piped.pr = Some(PullRequest {
        number: 42,
        url: "https://github.com/o/r/pull/42".into(),
        review: None,
        merge_request: false,
    });
    piped.checks = Some(Checks {
        state: ChecksState::Failing,
        passed: 5,
        failed: 1,
        pending: 0,
        skipped: 0,
        failing: vec!["clippy (macos)".into()],
        why: None,
        at_ms: AT,
    });
    let piped = on(piped, worker, SessionId::new());
    let mut todos =
        on(card(3, "Still has to-dos", TaskState::Done, None), worker, SessionId::new());
    todos.natives = NativeCounts { agents: 0, running: 0, todos: 3, done: 1 };
    let working = card(4, "Still at work", TaskState::Running, None);
    let mirror = one(vec![ahead, piped, todos, working]);
    let b = board(&mirror);

    let said = |n| {
        b.pipeline(TaskId(n)).into_iter().map(|s| (s.kind, s.words, s.holds)).collect::<Vec<_>>()
    };
    assert_eq!(
        said(2),
        [
            (StageKind::Branch, "slopty/board/2".to_owned(), false),
            (StageKind::Verifier, "Verified".to_owned(), false),
            (StageKind::Reviewer, "Approved".to_owned(), false),
            (StageKind::Queue, "2nd to merge".to_owned(), false),
            (StageKind::Pull, "PR #42".to_owned(), false),
            (StageKind::Checks, "1 of 6 checks fail".to_owned(), true),
        ]
    );
    let todo = (StageKind::ToDos, "2 to-dos open".to_owned(), true);
    assert_eq!(said(3), [(StageKind::Branch, "slopty/board/3".to_owned(), false), todo]);
    assert!(said(4).is_empty(), "nothing to say while it is worked on");

    assert_eq!(b.actions(TaskId(2)), [TaskAction::FixCi], "failing checks are CI to fix");
    let fix = b.told(TaskId(2), TaskAction::FixCi).expect("words");
    assert!(fix.contains("Pull request #42's checks failed: clippy (macos)."), "{fix}");
    assert!(!b.actions(TaskId(3)).contains(&TaskAction::Merge), "{:?}", b.actions(TaskId(3)));
    assert_eq!(b.open_todos(TaskId(3)), 2);
}

/// A merge whose push to `origin` failed says so on its row and as a stage that holds, in
/// git's first line, rather than reading as a merge like any other, and offers Push again; one
/// pushed, or never asked to be, has nothing more to say or do once merged.
#[test]
fn a_merge_whose_push_failed_says_so() {
    use slopty_proto::project::Merge;

    use super::model::{StageKind, TaskAction, merged_words};

    let merged = |n, pushed, push_failed: Option<&str>| {
        let mut c = card(n, "Merged", TaskState::Merged, None);
        c.merge = Some(Merge::Merged {
            target: "main".into(),
            head: "abcdef0123".into(),
            at_ms: AT,
            pushed,
            push_failed: push_failed.map(str::to_owned),
        });
        c
    };
    let why = "! [rejected] main -> main (fetch first)\nhint: Updates were rejected";
    let mirror =
        one(vec![merged(1, false, Some(why)), merged(2, true, None), merged(3, false, None)]);
    let b = board(&mirror);
    let said = |n| {
        b.pipeline(TaskId(n)).into_iter().map(|s| (s.kind, s.words, s.holds)).collect::<Vec<_>>()
    };
    let failed = "Push failed: ! [rejected] main -> main (fetch first)".to_owned();
    assert_eq!(said(1), [(StageKind::Push, failed, true)]);
    assert!(said(2).is_empty() && said(3).is_empty());
    let merge = |n| b.tasks.get(&TaskId(n)).and_then(|c| c.merge.as_ref());
    let words = |n| merge(n).and_then(|m| merged_words(m, false));
    assert_eq!(
        words(1).as_deref(),
        Some("into main at abcdef0, push failed: ! [rejected] main -> main (fetch first)")
    );
    assert_eq!(words(2).as_deref(), Some("into main at abcdef0, pushed"));
    assert_eq!(words(3).as_deref(), Some("into main at abcdef0"));
    let beside_its_stage = merge(1).and_then(|m| merged_words(m, true));
    assert_eq!(beside_its_stage.as_deref(), Some("into main at abcdef0"), "said once");
    assert_eq!(b.actions(TaskId(1)), [TaskAction::PushAgain], "pushed again on the person's word");
    assert_eq!(b.actions(TaskId(2)), Vec::<TaskAction>::new(), "pushed: nothing left to do");
    assert_eq!(b.actions(TaskId(3)), Vec::<TaskAction>::new(), "never asked to push");
}

/// Each node says where it is: the orchestrator and a running task where their agents run, an
/// ended one where it ran, then a pin, then a proposal, with the worktree, branch and reason;
/// only a task not started yet can move.
#[test]
fn a_node_says_where_it_runs_and_why_and_whether_it_can_move() {
    use slopty_proto::project::{Placed, Proposed};

    use super::model::{Place, PlaceHow, os_name};

    let (studio, linux) = (WorkerId::new(), WorkerId::new());
    let orchestrator = TermRef { worker: studio, session: SessionId::new() };
    let mut running =
        on(card(1, "Ship the app", TaskState::Running, None), studio, SessionId::new());
    running.worktree = Some("/w/board-1".into());
    if let Some(a) = running.assignment.as_mut() {
        a.placed = Some(Placed { pinned: false, score: 0, why: "Apple work".into() });
    }
    let mut ended = on(card(2, "Write docs", TaskState::Done, None), linux, SessionId::new());
    if let Some(a) = ended.assignment.as_mut() {
        a.ended_ms = Some(AT);
    }
    let mut pinned = card(3, "Pinned", TaskState::Planned, None);
    pinned.pin = Some(linux);
    let mut proposed = card(4, "Proposed", TaskState::Planned, None);
    proposed.proposed = Some(Proposed {
        since_ms: AT,
        runs: "codex".into(),
        on: Some(linux),
        why: "Linux first +20".into(),
    });
    let loose = card(5, "Anywhere", TaskState::Planned, None);
    let mut mirror = Projects::default();
    let tasks = vec![running, ended, pinned, proposed, loose];
    mirror.apply_part(snapshot(
        10,
        vec![status(project("board", Some(orchestrator)), tasks, Vec::new())],
    ));
    let board = board(&mirror);

    let place = |node| board.place(node).map(|p| (p.worker, p.how, p.why));
    assert_eq!(place(None), Some((studio, PlaceHow::Runs, None)));
    assert_eq!(
        board.place(Some(TaskId(1))),
        Some(Place {
            worker: studio,
            how: PlaceHow::Runs,
            worktree: Some("/w/board-1".into()),
            branch: Some("slopty/board/1".into()),
            why: Some("Apple work".into()),
        })
    );
    assert_eq!(place(Some(TaskId(2))), Some((linux, PlaceHow::Ran, None)));
    assert_eq!(place(Some(TaskId(3))), Some((linux, PlaceHow::Pinned, Some("pinned".into()))));
    assert_eq!(
        place(Some(TaskId(4))),
        Some((linux, PlaceHow::Proposed, Some("Linux first +20".into())))
    );
    assert_eq!(place(Some(TaskId(5))), None, "nowhere yet: no place to show");
    let movable: Vec<u32> = (1..=5).filter(|t| board.movable(TaskId(*t))).collect();
    assert_eq!(movable, [3, 4, 5]);
    assert_eq!(os_name(slopty_proto::server::Os::Linux), "Linux");
    assert_eq!(os_name(slopty_proto::server::Os::MacOs), "macOS");
}

/// A need says the paths it covers, or every task, and what it asks of a worker.
#[test]
fn a_need_says_what_it_covers_and_what_it_asks() {
    use slopty_proto::project::{Need, Preference};

    use super::model::need_words;

    let apple = Need {
        name: "Apple work".into(),
        paths: vec!["apps/ios".into(), "crates/ui".into()],
        require: vec![r#"os == "macos""#.into(), "has(toolchains.xcode)".into()],
        prefer: vec![Preference { expr: "cpus".into(), weight: 2 }],
    };
    assert_eq!(
        need_words(&apple),
        (
            "apps/ios, crates/ui".to_owned(),
            r#"Requires os == "macos" and has(toolchains.xcode); prefers cpus +2"#.to_owned()
        )
    );
    let linux = Need {
        name: "Linux first".into(),
        paths: Vec::new(),
        require: Vec::new(),
        prefer: vec![Preference { expr: r#"os == "linux""#.into(), weight: -5 }],
    };
    assert_eq!(
        need_words(&linux),
        ("Every task".to_owned(), r#"Prefers os == "linux" -5"#.to_owned())
    );
}

/// Checks that could not be read say why on the pull request's stage, in the first line, and
/// hold nothing: no Fix CI is offered for a forge that did not answer. The timeline says it as
/// a sentence.
#[test]
fn checks_that_could_not_be_read_say_why_and_ask_nothing() {
    use slopty_proto::agent::PullRequest;
    use slopty_proto::project::{Checks, ChecksState};

    use super::model::{StageKind, TaskAction};

    let mut c =
        on(card(1, "Its pull request", TaskState::Done, None), WorkerId::new(), SessionId::new());
    c.pr = Some(PullRequest {
        number: 42,
        url: "https://github.com/o/r/pull/42".into(),
        review: None,
        merge_request: false,
    });
    let unknown = Checks {
        state: ChecksState::Unknown,
        passed: 0,
        failed: 0,
        pending: 0,
        skipped: 0,
        failing: Vec::new(),
        why: Some("this worker has no gh\nsee https://cli.github.com".into()),
        at_ms: AT,
    };
    c.checks = Some(unknown.clone());
    let mirror = one(vec![c]);
    let b = board(&mirror);
    let checks: Vec<_> = b
        .pipeline(TaskId(1))
        .into_iter()
        .filter(|s| s.kind == StageKind::Checks)
        .map(|s| (s.words, s.holds))
        .collect();
    assert_eq!(checks, [("checks unknown: this worker has no gh".to_owned(), false)]);
    assert!(!b.actions(TaskId(1)).contains(&TaskAction::FixCi), "{:?}", b.actions(TaskId(1)));
    let name = |_: WorkerId| "studio".to_owned();
    let agent = |_: TermRef| "an agent".to_owned();
    assert_eq!(
        moment_line(&entry(1, Some(1), Moment::Checks(unknown)), name, agent),
        "The pull request's checks could not be read: this worker has no gh"
    );
}
