use slopty_core::SessionId;
use slopty_proto::agent::Worktree;
use slopty_proto::project::{GiveBacks, NativeTask, VerifierRun};

use super::*;

fn now() -> WallMs {
    WallMs::from_millis(1_790_000_000_000)
}

fn id() -> ProjectId {
    ProjectId::new("slopty").unwrap()
}

fn term() -> TermRef {
    TermRef { worker: WorkerId::new(), session: SessionId::new() }
}

/// What runs, as the hub would say.
#[derive(Default)]
struct Fleet {
    terminals: HashSet<TermRef>,
    agents: HashSet<TermRef>,
    starting: Vec<Starting>,
}

impl Fleet {
    fn running(&self) -> Running<'_> {
        Running { terminals: &self.terminals, agents: &self.agents, starting: &self.starting }
    }

    /// A terminal open on a worker, with an agent in it.
    fn open(&mut self) -> TermRef {
        let t = term();
        self.terminals.insert(t);
        self.agents.insert(t);
        t
    }
}

fn new_project(orchestrator: Option<TermRef>, limits: LimitsChange) -> NewProject {
    NewProject {
        id: id(),
        title: "Projects".to_owned(),
        repo: "~/src/slopty".to_owned(),
        target: "main".to_owned(),
        verifier: Some("cargo gate".to_owned()),
        push: false,
        orchestrator,
        limits,
        metadata: None,
        members: Vec::new(),
    }
}

fn project(orchestrator: Option<TermRef>) -> Projects {
    let mut projects = Projects::default();
    let fleet = Fleet::default();
    projects
        .create(new_project(orchestrator, LimitsChange::default()), &fleet.running(), now())
        .unwrap();
    projects
}

fn spec(title: &str) -> TaskSpec {
    TaskSpec { title: title.to_owned(), ..TaskSpec::default() }
}

fn task(projects: &mut Projects, title: &str) -> TaskId {
    projects.create_task(&id(), spec(title), now()).unwrap().0.id
}

fn code(refused: &Outcome) -> ErrorCode {
    match refused {
        Outcome::Error { code, .. } => *code,
        other => panic!("not refused: {other:?}"),
    }
}

fn message(refused: &Outcome) -> &str {
    match refused {
        Outcome::Error { message, .. } => message,
        other => panic!("not refused: {other:?}"),
    }
}

fn status(projects: &Projects) -> ProjectStatus {
    projects.status(&id(), Some(0), &Fleet::default().running()).unwrap()
}

fn get(projects: &Projects, task: TaskId) -> Task {
    projects.task(&id(), task).unwrap().clone()
}

/// The changes as the store keeps them.
fn kept(changes: &[Change]) -> Vec<Kept> {
    changes.iter().map(|c| c.kept.clone()).collect()
}

fn who(term: TermRef, spawned: bool, branch: Option<&AgentBranch>) -> Assignee<'_> {
    Assignee { term, spawned, branch, conversation: None, thread: None }
}

fn assign(p: &mut Projects, task: TaskId, at: TermRef) -> Changed<Task> {
    p.assign(&id(), task, who(at, true, None), &HashSet::from([at]), now())
}

fn to(state: TaskState) -> TaskChange {
    TaskChange { state: Some(state), ..TaskChange::default() }
}

/// A merged task is final and only finished work merges; a task given up may be planned
/// again.
#[test]
fn a_task_moves_along_its_lifecycle() {
    let mut p = project(None);
    let a = task(&mut p, "A");
    let refused =
        p.update_task(&id(), a, to(TaskState::Merged), Caller::Person, now()).unwrap_err();
    assert!(message(&refused).contains("only a done or verifying task merges"));
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    p.update_task(&id(), a, to(TaskState::Merged), Caller::Person, now()).unwrap();
    for back in [TaskState::Planned, TaskState::Running, TaskState::Failed] {
        let refused = p.update_task(&id(), a, to(back), Caller::Person, now()).unwrap_err();
        assert!(message(&refused).contains("final"), "{back:?}");
    }

    let b = task(&mut p, "B");
    p.update_task(&id(), b, to(TaskState::Failed), Caller::Person, now()).unwrap();
    p.update_task(&id(), b, to(TaskState::Planned), Caller::Person, now()).unwrap();
    assert_eq!(get(&p, b).state, TaskState::Planned);
}

/// Tasks are numbered in order within their project, one level of them: a title says
/// something, and a project that is not there makes none.
#[test]
fn a_task_is_numbered_in_order_within_its_project() {
    let mut p = project(None);
    let (first, second) = (task(&mut p, "Split"), task(&mut p, "Leaf"));
    assert_eq!((first, second), (TaskId(1), TaskId(2)));
    assert_eq!(code(&p.create_task(&id(), spec(" "), now()).unwrap_err()), ErrorCode::Invalid);
    let elsewhere = ProjectId::new("other").unwrap();
    assert_eq!(
        code(&p.create_task(&elsewhere, spec("x"), now()).unwrap_err()),
        ErrorCode::UnknownProject
    );
}

/// Dependencies form a graph that never leads back: a task depends on tasks that exist, never
/// on itself, and never on one that already needs it, however far round.
#[test]
fn dependencies_are_a_graph_that_refuses_a_cycle() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A"), task(&mut p, "B"));
    let c = TaskSpec { depends_on: vec![b, a, b], ..spec("C") };
    let c = p.create_task(&id(), c, now()).unwrap().0;
    assert_eq!(c.depends_on, [b, a], "each once, in order");
    let unknown = TaskSpec { depends_on: vec![TaskId(9)], ..spec("D") };
    assert_eq!(code(&p.create_task(&id(), unknown, now()).unwrap_err()), ErrorCode::UnknownTask);

    let on = |deps: Vec<TaskId>| TaskChange { depends_on: Some(deps), ..TaskChange::default() };
    p.update_task(&id(), b, on(vec![a]), Caller::Person, now()).unwrap();
    for (task, deps) in [(a, vec![a]), (a, vec![b]), (a, vec![c.id]), (b, vec![c.id])] {
        let refused =
            p.update_task(&id(), task, on(deps.clone()), Caller::Person, now()).unwrap_err();
        assert!(message(&refused).contains("never leads back"), "{task} on {deps:?}");
    }
    assert_eq!(get(&p, a).depends_on, [], "a refused change changes nothing");
    let (_, quiet) = p.update_task(&id(), a, on(Vec::new()), Caller::Person, now()).unwrap();
    assert!(quiet.is_empty(), "the same dependencies are no change");
}

/// A task says what it is in the orchestrator's words and keeps what its agents leave on it;
/// its agent says what it is doing.
#[test]
fn a_task_keeps_its_kind_metadata_and_status_checked() {
    let mut p = project(None);
    let bench = TaskSpec {
        kind: " bench ".to_owned(),
        metadata: Some(r#"{ "iterations": 5, "tags": ["cold"] }"#.to_owned()),
        verifier: Some("cargo bench".to_owned()),
        ..spec("Bench")
    };
    let t = p.create_task(&id(), bench, now()).unwrap().0;
    assert_eq!(t.kind, "bench");
    assert_eq!(t.metadata.as_deref(), Some(r#"{"iterations":5,"tags":["cold"]}"#));
    assert_eq!(t.verifier.as_deref(), Some("cargo bench"));

    for (bad, why) in [("[1, 2]", "a JSON object"), ("{ oops", "not JSON")] {
        let change = TaskChange { metadata: Some(bad.to_owned()), ..TaskChange::default() };
        let refused = p.update_task(&id(), t.id, change, Caller::Person, now()).unwrap_err();
        assert_eq!(code(&refused), ErrorCode::BadExpression);
        assert!(message(&refused).contains(why), "{bad}");
    }

    let said = TaskChange { status: Some("profiling the hub".to_owned()), ..TaskChange::default() };
    let (task, updates) = p.update_task(&id(), t.id, said.clone(), Caller::Person, now()).unwrap();
    assert_eq!(task.status.as_deref(), Some("profiling the hub"));
    assert!(matches!(kept(&updates).as_slice(), [Kept { entry: None, task: Some(_), .. }]));
    assert!(
        p.update_task(&id(), t.id, said, Caller::Person, now()).unwrap().1.is_empty(),
        "no change"
    );
    let cleared = TaskChange { status: Some(String::new()), ..TaskChange::default() };
    assert_eq!(p.update_task(&id(), t.id, cleared, Caller::Person, now()).unwrap().0.status, None);
}

/// A task with an agent follows its status: working runs it, a permission blocks it (and that
/// alone is worth the timeline), an idle prompt has it wait. A state the orchestrator set
/// (verifying) is not the agent's to move, and a closed terminal ends the assignment.
#[test]
fn a_task_follows_its_agent_until_someone_else_moves_it() {
    let mut p = project(None);
    let t = task(&mut p, "Work");
    let at = term();
    let (assigned, updates) = assign(&mut p, t, at).unwrap();
    assert_eq!(assigned.state, TaskState::Running);
    assert_eq!(updates.len(), 2, "assigned, then running: {updates:?}");

    let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    let moved = p.agent_status(at, &blocked, now());
    let moved = kept(&moved);
    let [Kept { task: Some(task), entry: Some(entry), .. }] = moved.as_slice() else {
        panic!("{moved:?}")
    };
    assert_eq!(task.state, TaskState::Blocked);
    assert_eq!(entry.what, Moment::State { from: TaskState::Running, to: TaskState::Blocked });
    let idle = p.agent_status(at, &AgentStatus::Idle, now());
    assert!(matches!(kept(&idle).as_slice(), [Kept { entry: None, .. }]), "quiet: {idle:?}");
    assert_eq!(get(&p, t).state, TaskState::Waiting);
    assert!(p.agent_status(at, &AgentStatus::Done, now()).is_empty(), "already waiting");
    assert!(p.agent_status(term(), &AgentStatus::Working, now()).is_empty(), "another agent");

    let verifying = TaskChange {
        state: Some(TaskState::Verifying),
        verified: Some(VerifierRun {
            passed: false,
            summary: "2 errors".to_owned(),
            head: "a".repeat(40),
            base: "b".repeat(40),
            exit: Some(0),
            took_ms: 0,
        }),
        ..TaskChange::default()
    };
    let (_, updates) = p.update_task(&id(), t, verifying, Caller::Person, now()).unwrap();
    assert_eq!(updates.len(), 2, "moved, verified");
    let working = p.agent_status(at, &AgentStatus::Working, now());
    assert!(
        matches!(working.as_slice(), [Change { durable: true, kept: Kept { entry: None, .. } }]),
        "its time is counted, kept with no entry: {working:?}"
    );
    assert_eq!(get(&p, t).state, TaskState::Verifying);

    let gone = p.session_ended(at, now());
    assert!(matches!(
        kept(&gone).as_slice(),
        [Kept { entry: Some(TimelineEntry { what: Moment::AgentGone { .. }, .. }), .. }]
    ));
    assert!(get(&p, t).assignment.is_some_and(|a| a.ended_ms == Some(now())));
}

/// Time at work is counted per task and, apart, for the orchestrator: a stretch runs from
/// working to idle or blocked, the wait between is left out, and an ended stretch is written,
/// as is a task's begun one, so a restart knows which of its turns were under way; the
/// orchestrator's begun one is only pushed. A closed terminal ends the stretch it was in.
#[test]
fn time_at_work_is_counted_per_task_and_apart_for_the_orchestrator() {
    let orchestrator = term();
    let mut p = project(Some(orchestrator));
    let t = task(&mut p, "Work");
    let at = term();
    assign(&mut p, t, at).unwrap();
    let ms = |s: u64| WallMs::from_millis(now().as_millis() + s * 1_000);
    let durable = |changes: &[Change]| changes.iter().map(|c| c.durable).collect::<Vec<_>>();

    assert_eq!(durable(&p.agent_status(at, &AgentStatus::Working, ms(0))), [true]);
    let tool = AgentStatus::Tool { tool: "Bash".to_owned() };
    assert!(p.agent_status(at, &tool, ms(10)).is_empty(), "the same stretch");
    assert_eq!(durable(&p.agent_status(orchestrator, &AgentStatus::Working, ms(20))), [false]);
    assert_eq!(durable(&p.agent_status(at, &AgentStatus::Idle, ms(60))), [true], "kept");
    let blocked = AgentStatus::Blocked(BlockReason::Question);
    assert_eq!(durable(&p.agent_status(orchestrator, &blocked, ms(50))), [true]);
    assert_eq!(get(&p, t).spent, Spent { active_ms: 60_000, since_ms: None });
    let record = &status(&p).project;
    assert_eq!(record.orchestrator_spent, Spent { active_ms: 30_000, since_ms: None });

    p.agent_status(at, &AgentStatus::Working, ms(600));
    let card = status(&p).tasks.into_iter().find(|c| c.id == t).unwrap();
    assert_eq!(card.spent.since_ms, Some(ms(600)), "the card carries the stretch under way");
    assert_eq!(card.spent.at(ms(605)), 65_000);
    let back = Projects::restore(p.file(Vec::new(), 0));
    assert_eq!(
        get(&back, t).spent,
        Spent { active_ms: 60_000, since_ms: None },
        "a restart ends it"
    );
    p.session_ended(at, ms(630));
    assert_eq!(get(&p, t).spent, Spent { active_ms: 90_000, since_ms: None }, "it ended there");
}

/// One terminal per task and one task per terminal: a second live terminal for a task, or a
/// terminal already on another task, is refused; the same one again is no change; a terminal
/// that is no longer live leaves the task free.
#[test]
fn a_terminal_works_on_one_task_and_a_task_has_one_live_terminal() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A"), task(&mut p, "B"));
    let at = term();
    assign(&mut p, a, at).unwrap();
    assert_eq!(assign(&mut p, a, at).unwrap().1, Vec::<Change>::new());
    let other = term();
    let both = HashSet::from([at, other]);
    let second = p.assign(&id(), a, who(other, false, None), &both, now());
    assert_eq!(code(&second.unwrap_err()), ErrorCode::Conflict);
    assert_eq!(code(&assign(&mut p, b, at).unwrap_err()), ErrorCode::Conflict);
    let (taken, updates) = assign(&mut p, a, other).unwrap();
    assert_eq!(taken.assignment.map(|a| a.term), Some(other), "the old terminal is not live");
    assert!(
        updates
            .iter()
            .any(|u| matches!(&u.kept.entry, Some(e) if e.what == Moment::AgentGone { term: at })),
        "{updates:?}"
    );
}

/// What runs is counted by the terminals that are live, whatever a task's state says: an agent
/// that marks its own task done still counts while its terminal lives, a closed orchestrator no
/// longer does, and a start counts from the moment it is placed.
#[test]
fn live_agents_are_counted_by_their_terminals_not_their_tasks_states() {
    let mut fleet = Fleet::default();
    let orchestrator = fleet.open();
    let mut p = project(Some(orchestrator));
    let (a, b) = (task(&mut p, "A"), task(&mut p, "B"));
    let at = fleet.open();
    p.assign(&id(), a, who(at, true, None), &fleet.terminals, now()).unwrap();
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    let live = p.status(&id(), None, &fleet.running()).unwrap().live;
    assert_eq!((live.project, live.fleet), (2, 2), "the orchestrator and a done task's agent");

    fleet.terminals.remove(&orchestrator);
    fleet.agents.remove(&orchestrator);
    let live = p.status(&id(), None, &fleet.running()).unwrap().live;
    assert_eq!(live.project, 1, "a closed orchestrator no longer counts");
    p.may_start(&id(), b, false, &fleet.running()).unwrap();
    let plain = fleet.open();
    assert_eq!(p.fleet(&fleet.running()), 2, "an agent outside any project counts in the fleet");
    fleet.starting.push(Starting {
        id: 1,
        term: TermRef { worker: plain.worker, session: SessionId::new() },
        task: Some((id(), b)),
        agent: true,
        since: tokio::time::Instant::now(),
        answered: false,
        conversation: None,
    });
    let there = p.live_on_worker(plain.worker, &fleet.running());
    assert_eq!(there, 2, "the agent there and a start in flight");
    let twice = p.may_start(&id(), b, false, &fleet.running()).unwrap_err();
    assert!(message(&twice).contains("being started"));
}

/// A server that was away learns which of a worker's terminals ended meanwhile when the worker
/// registers again, and their tasks are free.
#[test]
fn a_worker_s_terminals_that_ended_while_the_server_was_away_end_their_assignments() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A"), task(&mut p, "B"));
    let worker = WorkerId::new();
    let (kept, lost) = (SessionId::new(), SessionId::new());
    let on = |session| TermRef { worker, session };
    let terminals = HashSet::from([on(kept), on(lost)]);
    p.assign(&id(), a, who(on(kept), true, None), &terminals, now()).unwrap();
    p.assign(&id(), b, who(on(lost), true, None), &terminals, now()).unwrap();
    let restored = Projects::restore(p.file(Vec::new(), 0));
    let mut p = restored;
    let updates = p.reconcile(worker, &[kept], now());
    assert_eq!(updates.len(), 1, "{updates:?}");
    assert!(get(&p, b).assignment.is_some_and(|a| a.ended_ms.is_some()));
    assert!(get(&p, a).assignment.is_some_and(|a| a.ended_ms.is_none()));
    assert!(
        p.reconcile(WorkerId::new(), &[], now()).is_empty(),
        "another worker's are not touched"
    );
}

/// Claude Code's own subagents and task list land as leaves of the node whose session runs
/// them, a task's terminal or the orchestrator, each change as that leaf alone and never on
/// the timeline. What a session reports before a task takes it on is kept for that task.
#[test]
fn subagents_and_to_dos_are_leaves_sent_as_deltas_and_kept_until_a_task_takes_the_session() {
    let orchestrator = term();
    let mut p = project(Some(orchestrator));
    let t = task(&mut p, "Work");
    let at = term();
    let started = |session: SessionId, agent: &str| AgentReport::SubagentStarted {
        session,
        agent: agent.to_owned(),
        kind: "Explore".to_owned(),
    };
    assert!(p.report(at.worker, &started(at.session, "early"), now()).is_empty(), "held");
    let (_, updates) = assign(&mut p, t, at).unwrap();
    let adopted: Vec<&NativeChange> =
        updates.iter().filter_map(|u| u.kept.native.as_ref()).collect();
    assert!(
        matches!(adopted.as_slice(), [NativeChange { task: Some(n), native: Native::Agent(a) }] if *n == t && a.id == "early"),
        "{updates:?}"
    );

    let updates = kept(&p.report(at.worker, &started(at.session, "ag1"), now()));
    let [Kept { native: Some(change), entry: None, task: None, record: None, .. }] =
        updates.as_slice()
    else {
        panic!("{updates:?}")
    };
    assert_eq!(change.task, Some(t));
    assert!(p.report(at.worker, &started(at.session, "ag1"), now()).is_empty(), "started once");
    let stopped = AgentReport::SubagentStopped {
        session: at.session,
        agent: "ag1".to_owned(),
        transcript: Some("/t/ag1.jsonl".to_owned()),
        last: Some("Found it.".to_owned()),
    };
    let updates = kept(&p.report(at.worker, &stopped, now()));
    let [Kept { native: Some(NativeChange { native: Native::Agent(ag1), .. }), .. }] =
        updates.as_slice()
    else {
        panic!("{updates:?}")
    };
    assert_eq!(
        (ag1.kind.as_str(), ag1.stopped_ms, ag1.transcript.as_deref()),
        ("Explore", Some(now()), Some("/t/ag1.jsonl"))
    );
    let todo = NativeTask { id: "1".to_owned(), subject: "Read".to_owned(), done: false };
    let report = AgentReport::NativeTask { session: at.session, task: todo };
    assert_eq!(p.report(at.worker, &report, now()).len(), 1);

    let own = started(orchestrator.session, "o1");
    let updates = kept(&p.report(orchestrator.worker, &own, now()));
    assert!(matches!(
        updates.as_slice(),
        [Kept { native: Some(NativeChange { task: None, .. }), .. }]
    ));
    let s = status(&p);
    let node = |task| p.node(&id(), task).unwrap();
    assert_eq!(node(Some(t)).natives.agents.len(), 2);
    assert_eq!(node(Some(t)).natives.tasks.len(), 1);
    assert_eq!(node(None).natives.agents.first().map(|a| a.id.as_str()), Some("o1"));
    assert!(s.timeline.iter().all(|e| !matches!(e.what, Moment::Note { .. })));
    let kinds: Vec<&Moment> = s.timeline.iter().map(|e| &e.what).collect();
    assert!(
        matches!(
            kinds.as_slice(),
            [
                Moment::Created,
                Moment::Orchestrator { .. },
                Moment::TaskCreated { .. },
                Moment::Assigned { .. },
                Moment::State { .. }
            ]
        ),
        "no subagent on the timeline: {kinds:?}"
    );
}

/// A status line's worktree lands on the task its agent works on, whether it came before the
/// assignment or after, and a new branch is worth the timeline.
#[test]
fn a_branch_lands_on_the_task_its_agent_works_on() {
    let mut p = project(None);
    let t = task(&mut p, "Work");
    let at = term();
    let worktree = Worktree {
        name: "rows".to_owned(),
        path: "/w/.claude/worktrees/rows".to_owned(),
        branch: Some("worktree-rows".to_owned()),
        original_cwd: "/w".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session: at.session, worktree: Some(worktree.clone()) };
    let terminals = HashSet::from([at]);
    let (assigned, _) =
        p.assign(&id(), t, who(at, false, Some(&branch)), &terminals, now()).unwrap();
    assert_eq!(assigned.branch.as_deref(), Some("worktree-rows"));
    assert_eq!(assigned.worktree.as_deref(), Some("/w/.claude/worktrees/rows"));
    assert_eq!(
        p.report(at.worker, &AgentReport::Branch(branch.clone()), now()),
        Vec::<Change>::new()
    );
    let renamed = Worktree { branch: Some("worktree-cards".to_owned()), ..worktree };
    let moved = AgentBranch { worktree: Some(renamed), ..branch };
    let updates = kept(&p.report(at.worker, &AgentReport::Branch(moved), now()));
    let [Kept { entry: Some(entry), .. }] = updates.as_slice() else { panic!("{updates:?}") };
    assert_eq!(entry.what, Moment::Branch { branch: Some("worktree-cards".to_owned()) });
}

/// A task's card follows the pull request its thread's row names at the task's seat, cut to a
/// card's bounds: first seen and each move of where it stands go on the timeline, words alone
/// and its going away on the card only, and a seat on another worker or of no task is nobody's.
#[test]
fn a_task_s_card_follows_its_thread_s_pull_request() {
    use slopty_proto::git::Forge;
    use slopty_proto::thread::wire::{PullSeen, PullStands};

    let mut p = project(None);
    let t = task(&mut p, "Work");
    let at = term();
    assign(&mut p, t, at).unwrap();
    let seen = |stands, title: &str| PullSeen {
        forge: Forge::GitLab,
        number: 7,
        url: "https://gitlab.com/o/r/-/merge_requests/7".to_owned(),
        title: title.to_owned(),
        base: "main".to_owned(),
        stands,
        failed: u32::from(stands == PullStands::ChecksFailed),
        failed_first: (stands == PullStands::ChecksFailed).then(|| "lint".to_owned()),
        running: 0,
    };
    let said = |p: &mut Projects, worker, pull: Option<PullSeen>| {
        kept(&p.pulls_seen(worker, &[(at.session, pull)], now()).0)
    };
    let card = |p: &Projects| p.task(&id(), t).unwrap().pull.clone();

    let running = seen(PullStands::Running, "Rows");
    let [Kept { entry: Some(entry), .. }] = &*said(&mut p, at.worker, Some(running.clone())) else {
        panic!("first seen is news")
    };
    assert_eq!(entry.what, Moment::Pull(running.clone()));
    assert_eq!(said(&mut p, at.worker, Some(running)), [], "the same again is nothing");
    let failed = seen(PullStands::ChecksFailed, "Rows");
    let [Kept { entry: Some(entry), .. }] = &*said(&mut p, at.worker, Some(failed.clone())) else {
        panic!("a move is news")
    };
    assert_eq!(entry.what, Moment::Pull(failed));
    let long = seen(PullStands::ChecksFailed, &"r".repeat(TITLE_MAX * 2));
    let [Kept { entry: None, .. }] = &*said(&mut p, at.worker, Some(long)) else {
        panic!("words alone go on the card")
    };
    assert_eq!(card(&p).map(|c| c.title.len()), Some(TITLE_MAX), "cut to a card's bounds");
    assert_eq!(said(&mut p, WorkerId::new(), None), [], "another worker's seat");
    let [Kept { entry: None, .. }] = &*said(&mut p, at.worker, None) else {
        panic!("gone, quietly")
    };
    assert_eq!(card(&p), None);
}

/// A project's review limit is at least one, and the person's policy says which projects'
/// agents may loosen their permissions.
#[test]
fn limits_and_the_person_s_bounds_hold() {
    let mut p = Projects::default();
    let fleet = Fleet::default();
    let zero = LimitsChange { review: Some(0) };
    assert_eq!(
        code(&p.create(new_project(None, zero), &fleet.running(), now()).unwrap_err()),
        ErrorCode::Invalid
    );
    let (made, _) =
        p.create(new_project(None, LimitsChange::default()), &fleet.running(), now()).unwrap();
    assert_eq!(made.project.limits, Limits::default());
    assert_eq!(made.bounds, Bounds::default());

    let tight = Bounds { live_agents: 2, ..Bounds::default() };
    let permitted = BTreeSet::from([id()]);
    p.set_policy(Policy { bounds: tight, permission_flags: permitted });
    let s = status(&p);
    assert_eq!(s.bounds.live_agents, 2);
    assert!(s.bounds.permission_flags, "this project may loosen its agents' permissions");
}

/// What the store writes reads back the same, timeline numbering included.
#[test]
fn the_file_holds_everything_and_reads_back_the_same() {
    let mut p = project(Some(term()));
    let t = task(&mut p, "Work");
    let at = term();
    assign(&mut p, t, at).unwrap();
    let report = AgentReport::SubagentStarted {
        session: at.session,
        agent: "a".to_owned(),
        kind: "Plan".to_owned(),
    };
    p.report(at.worker, &report, now());
    let file = p.file(Vec::new(), 0);
    let json = serde_json::to_vec(&file).unwrap();
    let read: ProjectsFile = serde_json::from_slice(&json).unwrap();
    assert_eq!(read, file);
    let back = Projects::restore(read);
    assert_eq!(status(&back), status(&p));
    assert_eq!(status(&back).next, 6);
}

/// What a project and its tasks carry is bounded as it comes in: text past its bound and a
/// report too large are refused, so no task or page outgrows a frame.
#[test]
fn what_a_project_holds_is_bounded_as_it_comes_in() {
    use slopty_proto::project::{ARTIFACTS_MAX, BRIEF_MAX, NOTE_MAX, REF_MAX, Report};

    let mut p = Projects::default();
    let long_repo =
        NewProject { repo: "r".repeat(REF_MAX + 1), ..new_project(None, LimitsChange::default()) };
    let refused = p.create(long_repo, &Fleet::default().running(), now()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Invalid, "{}", message(&refused));
    let mut p = project(None);
    let t = task(&mut p, "T");
    let long_brief = TaskSpec { brief: "b".repeat(BRIEF_MAX + 1), ..spec("Long") };
    let refused = p.create_task(&id(), long_brief, now()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Limit, "{}", message(&refused));

    let report = |note: String, artifacts: usize| Report {
        note,
        artifacts: vec!["target/x".to_owned(); artifacts],
        branch: None,
        pr: None,
    };
    let long = report("n".repeat(NOTE_MAX + 1), 0);
    assert_eq!(code(&p.report_task(&id(), t, &long, now()).unwrap_err()), ErrorCode::Invalid);
    let crowded = report("ok".to_owned(), ARTIFACTS_MAX + 1);
    assert_eq!(code(&p.report_task(&id(), t, &crowded, now()).unwrap_err()), ErrorCode::Invalid);
    let (_, updates) = p.report_task(&id(), t, &report("ok".to_owned(), 2), now()).unwrap();
    assert!(matches!(
        kept(&updates).as_slice(),
        [Kept { entry: Some(TimelineEntry { what: Moment::Reported { .. }, .. }), .. }]
    ));
}

/// A timeline is kept within its bytes as well as its count, and a page of it is cut by
/// bytes, so a project of long notes pages instead of outgrowing a frame.
#[test]
fn a_timeline_is_kept_and_paged_within_its_bytes() {
    use slopty_proto::project::{NOTE_MAX, TIMELINE_BYTES_KEPT, TIMELINE_PAGE_BYTES};

    let mut p = project(None);
    let t = task(&mut p, "Notes");
    let note = "n".repeat(NOTE_MAX);
    let enough = TIMELINE_BYTES_KEPT / NOTE_MAX + 64;
    for _ in 0..enough {
        let change = TaskChange { note: Some(note.clone()), ..TaskChange::default() };
        p.update_task(&id(), t, change, Caller::Person, now()).unwrap();
    }
    let kept_bytes: usize =
        p.file(Vec::new(), 0).projects[0].timeline.iter().map(TimelineEntry::approx_bytes).sum();
    assert!(kept_bytes <= TIMELINE_BYTES_KEPT, "{kept_bytes}");
    let page = status(&p);
    let page_bytes: usize = page.timeline.iter().map(TimelineEntry::approx_bytes).sum();
    assert!(page_bytes <= TIMELINE_PAGE_BYTES, "{page_bytes}");
    assert!(
        page.next < p.status(&id(), None, &Fleet::default().running()).unwrap().next,
        "more to page"
    );
}

/// A step's start and end go on the timeline and its progress between on the card alone, its
/// start time kept, though the commits it works on are kept the moment they come. A server
/// that stopped mid-step keeps it under way as it loads, and hands it, as it stood, to its
/// worker once that is back, and only once.
#[test]
fn a_step_is_shown_as_it_goes_and_is_taken_up_after_a_restart() {
    use slopty_proto::project::{Commits, StepKind, StepState, TaskStep};
    let mut p = project(None);
    let a = task(&mut p, "A");
    let (worker, since) = (WorkerId::new(), now());
    let step = |state, since_ms| TaskStep {
        kind: StepKind::Clone,
        worker,
        state,
        since_ms,
        term: None,
        commits: None,
    };
    let running = |percent| StepState::Running { phase: "Receiving objects".to_owned(), percent };
    let steps_logged = |p: &Projects| {
        status(p).timeline.iter().filter(|e| matches!(e.what, Moment::Step(_))).count()
    };
    let began = p.set_step(&id(), a, step(running(None), since), now()).unwrap();
    assert!(began.iter().all(|c| c.durable), "a start is kept");
    let later = WallMs::from_millis(since.as_millis().saturating_add(5_000));
    let moved = p.set_step(&id(), a, step(running(Some(40)), later), later).unwrap();
    assert!(moved.iter().all(|c| !c.durable), "progress is not kept");
    assert_eq!(get(&p, a).step.map(|s| (s.state, s.since_ms)), Some((running(Some(40)), since)));
    assert_eq!(steps_logged(&p), 1);
    let commits = Commits { head: "a".repeat(40), base: "b".repeat(40) };
    let with = TaskStep { commits: Some(commits.clone()), ..step(running(Some(50)), later) };
    let kept = p.set_step(&id(), a, with, later).unwrap();
    assert!(kept.iter().all(|c| c.durable), "the commits it works on are kept");
    let moved = p.set_step(&id(), a, step(running(Some(60)), later), later).unwrap();
    assert!(moved.iter().all(|c| !c.durable), "and carried, not kept again");
    assert_eq!(get(&p, a).step.and_then(|s| s.commits), Some(commits.clone()));

    let mut p = Projects::restore(p.file(Vec::new(), 0));
    let resuming = StepState::Running { phase: RESUMING.to_owned(), percent: None };
    assert_eq!(get(&p, a).step.map(|s| (s.state, s.since_ms)), Some((resuming, since)));
    assert_eq!(p.resumable(WorkerId::new()), [], "only its own worker takes it up");
    let stood = TaskStep { commits: Some(commits), ..step(running(Some(60)), since) };
    assert_eq!(p.resumable(worker), [(id(), a, stood)]);
    assert_eq!(p.resumable(worker), [], "once");
    let mut moved_on = Projects::restore(p.file(Vec::new(), 0));
    moved_on.set_step(&id(), a, step(running(None), later), later).unwrap();
    assert_eq!(moved_on.resumable(worker), [], "a step moved on from leaves nothing to take up");
    let done = StepState::Done { detail: "/home/c/slopty/clones/example.com/o/demo".to_owned() };
    p.set_step(&id(), a, step(done, later), later).unwrap();
    assert_eq!(steps_logged(&p), 2, "an end is logged");
}

/// What the orchestrator heard of each task, the takings back left out.
fn upshots(p: &mut Projects) -> Vec<(TaskId, Upshot)> {
    p.heard()
        .into_iter()
        .filter(|h| h.upshot != Upshot::Moved)
        .map(|h| (h.task, h.upshot))
        .collect()
}

fn report_of() -> Report {
    Report { note: "said".to_owned(), artifacts: Vec::new(), branch: None, pr: None }
}

/// A task's agent that ends its turn without a word of its own is heard of by the
/// orchestrator, once per turn; a report is its own word. Waiting on the person is heard once per
/// wait, and taken back when it works again. Its exit is heard once, and a terminal whose agent
/// never showed says nothing of one.
#[test]
fn the_orchestrator_hears_what_a_task_s_agent_came_to_when_it_said_nothing() {
    let mut p = project(Some(term()));
    let t = task(&mut p, "Work");
    let at = term();
    assign(&mut p, t, at).unwrap();
    p.agent_status(at, &AgentStatus::None, now());
    assert_eq!(upshots(&mut p), [], "no agent seen there, so none left");

    p.agent_status(at, &AgentStatus::Working, now());
    p.agent_status(at, &AgentStatus::Tool { tool: "Bash".to_owned() }, now());
    p.agent_status(at, &AgentStatus::Idle, now());
    assert_eq!(upshots(&mut p), [(t, Upshot::Rested)]);
    p.agent_status(at, &AgentStatus::Done, now());
    assert_eq!(upshots(&mut p), [], "one rest per turn");

    p.agent_status(at, &AgentStatus::Working, now());
    p.report_task(&id(), t, &report_of(), now()).unwrap();
    p.agent_status(at, &AgentStatus::Blocked(BlockReason::IdlePrompt), now());
    assert_eq!(upshots(&mut p), [], "it said its own word");

    let bash = BlockReason::Permission { tool: "Bash".to_owned() };
    p.agent_status(at, &AgentStatus::Working, now());
    p.agent_status(at, &AgentStatus::Blocked(bash.clone()), now());
    assert_eq!(upshots(&mut p), [(t, Upshot::Waits(bash))]);
    p.agent_status(at, &AgentStatus::Blocked(BlockReason::Question), now());
    assert_eq!(upshots(&mut p), [], "still the one wait");
    p.agent_status(at, &AgentStatus::Working, now());
    let back = p.heard();
    assert!(back.iter().any(|h| h.task == t && h.upshot == Upshot::Moved), "{back:?}");
    p.agent_status(at, &AgentStatus::Waiting { tasks: 2, crons: 0 }, now());
    assert_eq!(upshots(&mut p), [], "its own background work is no rest");

    p.agent_status(at, &AgentStatus::None, now());
    assert_eq!(upshots(&mut p), [(t, Upshot::Exited)]);
    p.session_ended(at, now());
    assert_eq!(upshots(&mut p), [], "heard once");
}

/// A turn under way when the server stopped is taken up again from the store: one that ended
/// while it was away is heard once the agent's worker says it rests, unless the agent said its
/// own word in it. A task the merge queue or the person moved on is not heard of.
#[test]
fn a_turn_that_ended_while_the_server_was_away_is_still_heard() {
    let mut p = Projects::default();
    let made = p.create(
        new_project(Some(term()), LimitsChange::default()),
        &Fleet::default().running(),
        now(),
    );
    let mut log = made.unwrap().1;
    let mut made = |p: &mut Projects, title: &str| {
        let (t, changes) = p.create_task(&id(), spec(title), now()).unwrap();
        log.extend(changes);
        t.id
    };
    let (quiet, said, moved) = (made(&mut p, "Quiet"), made(&mut p, "Said"), made(&mut p, "Moved"));
    let (a, b, c) = (term(), term(), term());
    for (t, at) in [(quiet, a), (said, b), (moved, c)] {
        log.extend(assign(&mut p, t, at).unwrap().1);
        log.extend(p.agent_status(at, &AgentStatus::Working, now()));
    }
    let began: Vec<&Change> = log.iter().filter(|c| c.kept.task.is_some()).collect();
    assert!(began.iter().all(|c| c.durable), "a stretch that began is written");
    log.extend(p.report_task(&id(), said, &report_of(), now()).unwrap().1);
    log.extend(
        p.update_task(&id(), moved, to(TaskState::Failed), Caller::Person, now()).unwrap().1,
    );
    p.heard();

    let mut file = ProjectsFile::default();
    for change in log.iter().filter(|c| c.durable) {
        file.apply(&Keep::Project(Box::new(change.kept.clone())));
    }
    let mut back = Projects::restore(file);
    for at in [a, b, c] {
        back.agent_status(at, &AgentStatus::Idle, now());
    }
    assert_eq!(upshots(&mut back), [(quiet, Upshot::Rested)], "the others said or moved on");
}

/// The agent of a task merged or given up counts against no limit while it rests, though its
/// terminal is open, and counts again as soon as it works: giving up its own task frees no
/// agent that goes on working.
#[test]
fn a_finished_task_s_agent_counts_only_while_it_works() {
    let mut fleet = Fleet::default();
    let mut p = project(None);
    let a = task(&mut p, "A");
    let at = fleet.open();
    // The person's own terminal, told of rather than started by the server.
    p.assign(&id(), a, who(at, false, None), &fleet.terminals, now()).unwrap();
    p.agent_status(at, &AgentStatus::Working, now());
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    p.update_task(&id(), a, to(TaskState::Merged), Caller::Person, now()).unwrap();
    assert_eq!(p.fleet(&fleet.running()), 1, "it still works");

    p.agent_status(at, &AgentStatus::Idle, now());
    let live = p.status(&id(), None, &fleet.running()).unwrap().live;
    assert_eq!((live.project, live.fleet), (0, 0), "at rest, merged");
    assert_eq!(p.live_on_worker(at.worker, &fleet.running()), 0);
    assert_eq!(
        p.finished_agents(&fleet.terminals, |_| false),
        [],
        "the person's own terminal stays"
    );

    p.agent_status(at, &AgentStatus::Working, now());
    assert_eq!(p.status(&id(), None, &fleet.running()).unwrap().live.project, 1, "at work again");
}

/// The orchestrator tells only one of its tasks, never itself, and never one that waits on the
/// person; the timeline says the orchestrator told, apart from the person's words. The
/// person's word starts a task's give-backs again.
#[test]
fn the_orchestrator_tells_only_a_task_that_does_not_wait_on_the_person() {
    let mut p = project(Some(term()));
    let t = task(&mut p, "Work");
    let at = term();
    assign(&mut p, t, at).unwrap();
    let live = HashSet::from([at]);
    let tell =
        |p: &mut Projects, task, by| p.tell(&id(), (task, by), "Cover the iPad.", &live, now());

    let (words, changes) = tell(&mut p, Some(t), Teller::Orchestrator).unwrap();
    assert_eq!(words, "Cover the iPad.");
    let what = kept(&changes).first().and_then(|k| k.entry.clone()).map(|e| e.what);
    let said = Moment::Note { text: "The orchestrator told it: Cover the iPad.".to_owned() };
    assert_eq!(what, Some(said));
    let itself = tell(&mut p, None, Teller::Orchestrator).unwrap_err();
    assert!(message(&itself).contains("one of its tasks"), "{}", message(&itself));

    let bash = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    p.agent_status(at, &bash, now());
    let refused = tell(&mut p, Some(t), Teller::Orchestrator).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Conflict);
    assert!(message(&refused).contains("waits on the person"), "{}", message(&refused));

    p.records.get_mut(&id()).unwrap().task_mut(t).unwrap().give_backs =
        GiveBacks { count: 3, held: true };
    let person = tell(&mut p, Some(t), Teller::Person).unwrap().1;
    let what = kept(&person).first().and_then(|k| k.entry.clone()).map(|e| e.what);
    assert_eq!(what, Some(Moment::Told { text: "Cover the iPad.".to_owned() }), "the person may");
    assert_eq!(get(&p, t).give_backs, GiveBacks::default(), "their word starts it again");
    assert_eq!(
        kept(&person).first().and_then(|k| k.task.clone()).map(|t| t.give_backs),
        Some(GiveBacks::default())
    );
}

/// While as many tasks wait on the person as the project's review limit (ready to merge, asking
/// them something, or held past their give-backs), no agent starts more work: the refusal says
/// how many wait and which. Merging one makes room. Only the person sets the limit, and never
/// below one.
#[test]
fn a_project_at_its_review_limit_starts_no_more_agent_work() {
    let mut fleet = Fleet::default();
    let mut p = Projects::default();
    let two = LimitsChange { review: Some(2) };
    p.create(new_project(None, two), &fleet.running(), now()).unwrap();
    let (a, b, c) = (task(&mut p, "A"), task(&mut p, "B"), task(&mut p, "C"));
    p.room_to_review(&id()).unwrap();
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    let at = fleet.open();
    assign(&mut p, b, at).unwrap();
    let bash = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    p.agent_status(at, &bash, now());
    let refused = p.room_to_review(&id()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Limit);
    let said = message(&refused);
    assert!(
        said.contains(
            "2 tasks waiting on the person (task 1 ready to merge, task 2 asks the \
                       person), its review limit of 2"
        ),
        "{said}"
    );
    p.may_start(&id(), c, false, &fleet.running()).unwrap();
    p.ask_merge(&id(), a, now()).unwrap();
    p.room_to_review(&id()).unwrap();
}
