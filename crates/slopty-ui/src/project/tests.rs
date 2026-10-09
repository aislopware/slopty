//! The mirror and what the board derives from it, without a window.

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Moment, Native, NativeAgent, NativeChange, NativeTask, ProjectUpdate, ProjectsPart, StepKind,
    StepState, TaskId, TaskState,
};

use super::fixtures::{self, AT, card, entry, on, project, snapshot, status, task_changed};
use super::model::{Lane, Projects, TIMELINE_KEPT, state_word};

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
            vec![card(1, "a", TaskState::Planned)],
            Vec::new(),
        )],
    };
    let rest = ProjectsPart {
        seq: 10,
        first: false,
        last: true,
        projects: vec![status(
            project("board", None),
            vec![card(2, "b", TaskState::Planned)],
            Vec::new(),
        )],
    };
    mirror.apply_part(first);
    mirror.apply_part(rest);
    assert!(mirror.get(&fixtures::id("old")).is_none(), "the first part replaces");
    assert_eq!(board(&mirror).tasks.len(), 2, "a later part adds to the project it began");

    let stale = task_changed("board", card(1, "a", TaskState::Failed), None);
    assert_eq!(mirror.apply_update(10, stale), None, "at the snapshot's seq: already in it");
    assert_eq!(board(&mirror).tasks[&TaskId(1)].state, TaskState::Planned);
    let fresh = task_changed("board", card(1, "a", TaskState::Running), None);
    assert_eq!(mirror.apply_update(11, fresh), Some(fixtures::id("board")));
    assert_eq!(board(&mirror).tasks[&TaskId(1)].state, TaskState::Running);

    let unknown = task_changed("never-heard", card(1, "x", TaskState::Running), None);
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

/// Each task stands in the lane its own state puts it in; lanes with nothing are left out.
#[test]
fn a_task_stands_in_its_own_lane() {
    let mirror = one(vec![
        card(1, "done", TaskState::Done),
        card(2, "running", TaskState::Running),
        card(3, "blocked", TaskState::Blocked),
        card(4, "alone", TaskState::Merged),
        card(5, "next", TaskState::Planned),
    ]);
    let b = board(&mirror);
    let lanes: Vec<(Lane, Vec<u32>)> =
        b.lanes().into_iter().map(|(l, t)| (l, t.into_iter().map(|t| t.0).collect())).collect();
    assert_eq!(
        lanes,
        [
            (Lane::NeedsYou, vec![3]),
            (Lane::Working, vec![2]),
            (Lane::UpNext, vec![5]),
            (Lane::ReadyToMerge, vec![1]),
            (Lane::Merged, vec![4]),
        ]
    );
}

/// A task waits on the dependencies not yet done or merged.
#[test]
fn a_task_waits_on_what_is_not_done() {
    let mut blocked = card(3, "after", TaskState::Planned);
    blocked.depends_on = vec![TaskId(1), TaskId(2), TaskId(9)];
    let mirror = one(vec![
        card(1, "done", TaskState::Merged),
        card(2, "running", TaskState::Running),
        blocked,
    ]);
    assert_eq!(board(&mirror).waiting_on(TaskId(3)), [TaskId(2), TaskId(9)]);
}

/// A native leaf's change moves its node's counts: a subagent that starts and stops, a
/// to-do made and done, and one that started before the snapshot counted it.
#[test]
fn a_native_moves_its_nodes_counts() {
    let (worker, session) = (WorkerId::new(), SessionId::new());
    let mut running = on(card(1, "wire", TaskState::Running), worker, session);
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
    let mut gone = on(card(2, "gone", TaskState::Done), worker, ended);
    if let Some(a) = gone.assignment.as_mut() {
        a.ended_ms = Some(AT);
    }
    let mut mirror = Projects::default();
    mirror.apply_part(snapshot(
        1,
        vec![status(
            project("board", Some(TermRef { worker, session: orchestrator })),
            vec![on(card(1, "live", TaskState::Running), worker, agent), gone],
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

/// Every word the board says of a state, a lane or itself is sentence case.
#[test]
fn every_board_word_is_sentence_case() {
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
        super::view::ORCHESTRATOR,
        super::view::PROJECT_GONE,
    ]) {
        let mut chars = text.chars();
        assert!(chars.next().is_some_and(char::is_uppercase), "starts lowercase: {text:?}");
        let rest: String = chars.collect();
        assert!(!rest.chars().any(char::is_uppercase), "title case: {text:?}");
    }
}

/// A row names a task's state as the board's lane heading does: a planned task is "Up next" in
/// both, and only a waiting task says more than its lane.
#[test]
fn a_row_names_a_state_as_its_lane_does() {
    assert_eq!(state_word(TaskState::Planned), "Up next");
    for state in [
        TaskState::Planned,
        TaskState::Running,
        TaskState::Blocked,
        TaskState::Verifying,
        TaskState::Done,
        TaskState::Merged,
        TaskState::Failed,
    ] {
        assert_eq!(state_word(state), Lane::of(state).title(), "{state:?}");
    }
    assert_eq!(state_word(TaskState::Waiting), "Waiting");
}

/// A board unchanged by an update keeps its address, so handing it over again costs a pointer.
#[test]
fn an_update_copies_only_the_board_it_touches() {
    let mut mirror = one(vec![card(1, "Wire the board", TaskState::Running)]);
    let mut later = snapshot(10, vec![status(project("other", None), Vec::new(), Vec::new())]);
    later.first = false;
    let touched = mirror.apply_part(later);
    assert_eq!(touched, [fixtures::id("other")]);
    let held = mirror.get(&fixtures::id("board")).cloned();
    let untouched = mirror.get(&fixtures::id("other")).cloned();
    let changed = task_changed("board", card(1, "Wire the board", TaskState::Blocked), None);
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
        let mut c = queued(card(4, "d", TaskState::Done), 5, "4444444");
        let phase = StepState::Running { phase: "Rebasing onto main".into(), percent: None };
        c.step = Some(step(StepKind::Merge, worker, phase, None));
        c
    };
    let mut verifying = card(6, "f", TaskState::Verifying);
    let line = StepState::Running { phase: "Compiling".into(), percent: None };
    verifying.step = Some(step(StepKind::Verify, worker, line, None));
    verifying.updated_ms = WallMs::from_millis(AT.as_millis().saturating_add(50));
    let mut failed = card(7, "g", TaskState::Waiting);
    failed.verified = Some(run(false, "7777777abc"));
    let mirror = one(vec![
        queued(card(1, "a", TaskState::Done), 20, "1111111"),
        queued(card(2, "b", TaskState::Done), 10, "2222222"),
        card(3, "c", TaskState::Done),
        merging,
        card(5, "e", TaskState::Verifying),
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
        let mut c = card(1, "a", TaskState::Verifying);
        c.verified = Some(run(false, "1111111"));
        c
    }]);
    assert!(board(&again).verdict(TaskId(1)).is_none(), "judged again, the old word is gone");
    again.apply_update(
        11,
        task_changed(
            "board",
            {
                let mut c = card(1, "a", TaskState::Running);
                c.verified = Some(run(true, "1111111"));
                c
            },
            None,
        ),
    );
    assert!(board(&again).verdict(TaskId(1)).is_none(), "a pass for work since moved on");
}

/// What the person can do to a task from the board: merge what is done and not queued, retry
/// a step that failed on the way to the target, and nothing to a task that reads or has merged.
#[test]
fn a_task_offers_what_moves_it_on() {
    use super::fixtures::{queued, run, step};
    use super::model::TaskAction;

    let worker = WorkerId::new();
    let failed = |kind| StepState::Failed { why: format!("{kind:?} broke") };
    let done = card(1, "Done, nothing queued it", TaskState::Done);
    let in_queue = queued(card(2, "In the queue", TaskState::Done), 5, "2222222");
    let mut verify = card(3, "Its verifier failed", TaskState::Waiting);
    verify.verified = Some(run(false, "3333333"));
    verify.step = Some(step(StepKind::Verify, worker, failed(StepKind::Verify), None));
    let mut reads = card(7, "Reads only", TaskState::Done);
    reads.read_only = true;
    let merged = card(8, "Merged", TaskState::Merged);
    let mut clone = card(9, "Its clone failed", TaskState::Waiting);
    clone.step = Some(step(StepKind::Clone, worker, failed(StepKind::Clone), None));
    let mut merge = card(10, "Its merge failed", TaskState::Done);
    merge.step = Some(step(StepKind::Merge, worker, failed(StepKind::Merge), None));
    merge.merge = Some(slopty_proto::project::Merge::Queued { since_ms: AT });
    let mirror = one(vec![done, in_queue, verify, reads, merged, clone, merge]);
    let b = board(&mirror);
    let of = |n| b.actions(TaskId(n));
    assert_eq!(of(1), [TaskAction::Merge]);
    assert_eq!(of(2), [], "the queue already holds it");
    assert_eq!(of(3), [TaskAction::Retry]);
    assert_eq!(of(7), [], "a task that only reads has nothing to land");
    assert_eq!(of(8), []);
    assert_eq!(of(9), [], "a clone is the worker's to make again, not the merge queue's");
    assert_eq!(of(10), [TaskAction::Retry]);
    assert_eq!(of(99), [], "no such task");
    assert_eq!(TaskAction::Merge.selector("project-card", TaskId(1)), "project-card-merge-1");
}

/// A recap reads only what came after the last look, tells each kind once with its tasks in
/// the order they last moved, puts what needs the person first, and leaves out an agent that
/// ended once its work merged. A line names up to three tasks and counts the rest.
#[test]
fn a_recap_tells_what_needs_you_first_and_names_its_tasks() {
    use super::fixtures::run;
    use super::recap::{Looked, Recap, RecapKind};

    let mut merged = card(4, "Write the decision", TaskState::Merged);
    merged.assignment = None;
    let mirror = one(vec![
        merged,
        card(5, "Check the goldens", TaskState::Waiting),
        card(6, "Draw the lanes", TaskState::Waiting),
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

/// While a task's agent runs, its next step is the person's word to it, first on its row: fix
/// CI for a failed verifier, address the comments its pull request asked for,
/// resolve the conflicts its rebase met. Checking it again unchanged would fail the same way,
/// so Retry waits for an agent that is gone. What is said names what failed and what to do.
#[test]
fn a_running_agent_is_told_its_next_step_in_the_person_s_words() {
    use slopty_proto::thread::wire::PullStands;

    use super::fixtures::{pull, run, step};
    use super::model::TaskAction;

    let worker = WorkerId::new();
    let live = |n, title, state| on(card(n, title, state), worker, SessionId::new());
    let mut ci = live(1, "Its verifier failed", TaskState::Waiting);
    ci.verified = Some(run(false, "9c1e2f3"));
    ci.step =
        Some(step(StepKind::Verify, worker, StepState::Failed { why: "2 errors".into() }, None));
    let mut pr = live(3, "Its pull request", TaskState::Waiting);
    pr.pull = Some(pull(9, PullStands::ChangesRequested, (0, None)));
    let mut conflict = live(4, "Does not rebase", TaskState::Waiting);
    let why = StepState::Failed { why: "conflicts in a.txt".into() };
    conflict.step = Some(step(StepKind::Rebase, worker, why.clone(), None));
    let mut gone = card(5, "Does not rebase, nobody on it", TaskState::Planned);
    gone.step = Some(step(StepKind::Rebase, worker, why, None));
    let mirror = one(vec![ci, pr, conflict, gone]);
    let b = board(&mirror);
    let of = |n| b.actions(TaskId(n));
    assert_eq!(of(1), [TaskAction::FixCi], "no Retry while an agent can fix it");
    assert_eq!(of(3), [TaskAction::AddressComments]);
    assert_eq!(of(4), [TaskAction::ResolveConflicts]);
    assert_eq!(of(5), [TaskAction::Retry, TaskAction::RunOn], "nobody to tell");
    assert!(TaskAction::FixCi.tells() && !TaskAction::Retry.tells());

    let told = |n, action| b.told(TaskId(n), action).expect("words");
    let fix = told(1, TaskAction::FixCi);
    assert!(fix.starts_with("Fix CI. `cargo gate` failed on your work at 9c1e2f3"), "{fix}");
    assert!(fix.ends_with("report done again with task_report."), "{fix}");
    let pull = told(3, TaskAction::AddressComments);
    assert!(pull.contains("Pull request #9 has changes requested"), "{pull}");
    let resolve = told(4, TaskAction::ResolveConflicts);
    assert!(resolve.contains("does not rebase onto main: conflicts in a.txt"), "{resolve}");
    assert_eq!(b.told(TaskId(4), TaskAction::FixCi), None, "nothing failed to fix");
    assert_eq!(b.told(TaskId(1), TaskAction::Merge), None);
}

/// Once a task's work is on its way, one row says each stage of it: its branch, what its
/// verifier said, its place in the queue, its pull request with its own checks,
/// and the to-dos still open, said beside its Merge rather than holding it back. Failing
/// checks are CI to fix.
#[test]
fn a_task_s_pipeline_says_each_stage_and_its_open_to_dos() {
    use slopty_proto::project::NativeCounts;
    use slopty_proto::thread::wire::PullStands;

    use super::fixtures::{pull, queued};
    use super::model::{StageKind, TaskAction};

    let worker = WorkerId::new();
    let mut ahead = queued(card(1, "Ahead", TaskState::Done), 10, "1111111");
    ahead.branch = Some("worktree-ahead".into());
    let mut piped = queued(card(2, "Its pull request", TaskState::Done), 20, "2222222");
    piped.pull = Some(pull(42, PullStands::ChecksFailed, (1, Some("clippy (macos)"))));
    let piped = on(piped, worker, SessionId::new());
    let mut todos = on(card(3, "Still has to-dos", TaskState::Done), worker, SessionId::new());
    todos.natives = NativeCounts { agents: 0, running: 0, todos: 3, done: 1 };
    let working = card(4, "Still at work", TaskState::Running);
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
            (StageKind::Queue, "2nd to merge".to_owned(), false),
            (StageKind::Pull, "PR #42".to_owned(), false),
            (StageKind::Checks, "clippy (macos) failed".to_owned(), true),
        ]
    );
    let todo = (StageKind::ToDos, "2 to-dos open".to_owned(), true);
    assert_eq!(said(3), [(StageKind::Branch, "slopty/board/3".to_owned(), false), todo]);
    assert!(said(4).is_empty(), "nothing to say while it is worked on");
    let failed: Vec<_> =
        b.pipeline(TaskId(2)).into_iter().filter(|s| s.failed).map(|s| s.kind).collect();
    assert_eq!(failed, [StageKind::Checks], "only the failing checks say it in the error ink");
    assert!(b.pipeline(TaskId(3)).iter().all(|s| !s.failed), "to-dos hold, but nothing failed");

    assert_eq!(b.actions(TaskId(2)), [TaskAction::FixCi], "failing checks are CI to fix");
    let fix = b.told(TaskId(2), TaskAction::FixCi).expect("words");
    assert!(fix.contains("Pull request #42: clippy (macos) failed."), "{fix}");
    assert!(fix.contains("`gh pr checks 42` shows them"), "{fix}");
    assert_eq!(b.actions(TaskId(3)), [TaskAction::Merge], "to-dos are said, not a hold");
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
        let mut c = card(n, "Merged", TaskState::Merged);
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
    assert!(b.pipeline(TaskId(1)).iter().all(|s| s.failed), "a failed push is a failure");
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
/// ended one where it ran, then a pin, with the worktree and branch; only a task not started
/// yet can move.
#[test]
fn a_node_says_where_it_runs_and_whether_it_can_move() {
    use super::model::{Place, PlaceHow, os_name};

    let (studio, linux) = (WorkerId::new(), WorkerId::new());
    let orchestrator = TermRef { worker: studio, session: SessionId::new() };
    let mut running = on(card(1, "Ship the app", TaskState::Running), studio, SessionId::new());
    running.worktree = Some("/w/board-1".into());
    let mut ended = on(card(2, "Write docs", TaskState::Done), linux, SessionId::new());
    if let Some(a) = ended.assignment.as_mut() {
        a.ended_ms = Some(AT);
    }
    let mut pinned = card(3, "Pinned", TaskState::Planned);
    pinned.pin = Some(linux);
    let loose = card(4, "Anywhere", TaskState::Planned);
    let mut mirror = Projects::default();
    let tasks = vec![running, ended, pinned, loose];
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
            why: None,
        })
    );
    assert_eq!(place(Some(TaskId(2))), Some((linux, PlaceHow::Ran, None)));
    assert_eq!(place(Some(TaskId(3))), Some((linux, PlaceHow::Pinned, Some("pinned".into()))));
    assert_eq!(place(Some(TaskId(4))), None, "nowhere yet: no place to show");
    let movable: Vec<u32> = (1..=4).filter(|t| board.movable(TaskId(*t))).collect();
    assert_eq!(movable, [3, 4]);
    assert_eq!(os_name(slopty_proto::server::Os::Linux), "Linux");
    assert_eq!(os_name(slopty_proto::server::Os::MacOs), "macOS");
}

/// A pull request's checks still running say how many and hold nothing, and failures on one
/// already merged are no CI to fix: only an open pull request's failed check asks for Fix CI.
#[test]
fn running_checks_hold_nothing_and_a_merged_pull_request_asks_for_no_fix() {
    use slopty_proto::thread::wire::PullStands;

    use super::fixtures::pull;
    use super::model::{StageKind, TaskAction};

    let worker = WorkerId::new();
    let mut running = on(card(1, "Checks run", TaskState::Done), worker, SessionId::new());
    let mut seen = pull(42, PullStands::Running, (0, None));
    seen.running = 3;
    running.pull = Some(seen);
    let mut merged = on(card(2, "Merged on the forge", TaskState::Done), worker, SessionId::new());
    merged.pull = Some(pull(43, PullStands::Merged, (1, Some("lint"))));
    let mirror = one(vec![running, merged]);
    let b = board(&mirror);
    let checks = |n| -> Vec<_> {
        b.pipeline(TaskId(n))
            .into_iter()
            .filter(|s| s.kind == StageKind::Checks)
            .map(|s| (s.words, s.holds))
            .collect()
    };
    assert_eq!(checks(1), [("3 checks running".to_owned(), false)]);
    assert_eq!(checks(2), [], "a merged pull request's checks are done with");
    for n in [1, 2] {
        assert!(!b.actions(TaskId(n)).contains(&TaskAction::FixCi), "{:?}", b.actions(TaskId(n)));
    }
}

/// A task an agent has worked on can begin again: "Start fresh" on the agent it ran last, and
/// "Give to another agent…" among those its machine can start, which goes to the workspace as
/// that agent. A task that never ran and a merged one offer neither.
#[gpui::test]
fn a_task_starts_fresh_or_goes_to_another_agent(cx: &mut gpui::TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::Modifiers;
    use slopty_proto::thread::AgentId;

    use super::model::TaskAction;
    use super::view::{ProjectEvent, ProjectView, Seen, WorkerSeen};

    let worker = WorkerId::new();
    let ran = on(card(3, "Ran", TaskState::Waiting), worker, SessionId::new());
    let never = card(4, "Never ran", TaskState::Planned);
    let merged = on(card(5, "Merged", TaskState::Merged), worker, SessionId::new());
    let mirror = one(vec![ran, never, merged]);
    let b = board(&mirror);
    let fresh = [TaskAction::StartFresh, TaskAction::GiveTo];
    assert!(fresh.iter().all(|a| b.controls(TaskId(3)).contains(a)), "{:?}", b.controls(TaskId(3)));
    assert!(fresh.iter().all(|a| !b.controls(TaskId(4)).contains(a)), "it never ran");
    assert!(fresh.iter().all(|a| !b.controls(TaskId(5)).contains(a)), "its work is in");

    let codex = AgentId::named(AgentId::CODEX);
    let agents = vec![AgentId::named(AgentId::CLAUDE_CODE), codex.clone()];
    let seen = Seen {
        board: mirror.get(&fixtures::id("board")).cloned(),
        workers: [(worker, WorkerSeen { name: "box".to_owned(), os: None, form: None, agents })]
            .into(),
        ..Seen::default()
    };
    cx.update(gpui_kit::init);
    let (view, cx) = cx.add_window_view(|_w, cx| {
        let mut view = ProjectView::new(fixtures::id("board"), slopty_theme::Theme::default(), cx);
        view.set_seen(seen, cx);
        view
    });
    let heard: Rc<RefCell<Vec<ProjectEvent>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_w, cx| {
        cx.subscribe(&view, move |_v, event: &ProjectEvent, _cx| {
            into.borrow_mut().push(event.clone());
        })
        .detach();
    });
    view.update(cx, |v, cx| v.select_by(1, cx));
    cx.run_until_parked();
    let click = |cx: &mut gpui::VisualTestContext, s: &'static str| {
        let at = cx.debug_bounds(s).unwrap_or_else(|| panic!("{s} drawn")).center();
        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
    };
    click(cx, "project-card-start-fresh-3");
    assert_eq!(*heard.borrow(), [ProjectEvent::Act(TaskId(3), TaskAction::StartFresh)]);
    heard.borrow_mut().clear();
    click(cx, "project-card-give-to-3");
    assert!(heard.borrow().is_empty(), "the board picks the agent itself");
    assert!(cx.debug_bounds("project-card-give-3-0").is_some(), "Claude Code, on box");
    click(cx, "project-card-give-3-1");
    assert_eq!(*heard.borrow(), [ProjectEvent::GiveTo(TaskId(3), codex)]);
    assert!(cx.debug_bounds("project-card-give-3").is_none(), "the picker shuts");
}
