use slopty_core::SessionId;
use slopty_proto::agent::Worktree;
use slopty_proto::project::{NativeTask, Placement, VerifierRun};

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
        orchestrator,
        limits,
        metadata: None,
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

fn spec(title: &str, owns: &[&str]) -> TaskSpec {
    TaskSpec {
        title: title.to_owned(),
        owns: owns.iter().map(|p| (*p).to_owned()).collect(),
        ..TaskSpec::default()
    }
}

fn task(projects: &mut Projects, title: &str, owns: &[&str]) -> TaskId {
    projects.create_task(&id(), spec(title, owns), now()).unwrap().0.id
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
    Assignee { term, spawned, branch, conversation: None }
}

fn assign(p: &mut Projects, task: TaskId, at: TermRef) -> Changed<Task> {
    p.assign(&id(), task, who(at, true, None), &HashSet::from([at]), now())
}

fn to(state: TaskState) -> TaskChange {
    TaskChange { state: Some(state), ..TaskChange::default() }
}

#[test]
fn a_path_is_owned_relative_to_the_root_and_compared_as_apfs_compares_names() {
    assert_eq!(owned("./crates/slopty-server/").unwrap(), "crates/slopty-server");
    assert_eq!(owned("crates//a/./b.rs").unwrap(), "crates/a/b.rs");
    assert_eq!(owned(".").unwrap(), "", "the root owns everything");
    assert_eq!(owned("crates/*/Cargo.toml").unwrap(), "crates", "a glob owns what it may match");
    assert_eq!(owned("docs/*.md").unwrap(), "docs");
    owned("/etc/passwd").unwrap_err();
    owned("../elsewhere").unwrap_err();
    owned("~/x").unwrap_err();
    assert_eq!(owned("e\u{301}te\u{301}.md").unwrap(), "\u{e9}t\u{e9}.md", "composed, as NFC");

    assert!(overlap("crates/a", "crates/a/src/lib.rs"));
    assert!(overlap("crates/a/src/lib.rs", "crates/a"));
    assert!(overlap("", "anything"));
    assert!(!overlap("crates/a", "crates/ab"), "a shared prefix is not a shared file");
    assert!(!overlap("crates/a/x.rs", "crates/a/y.rs"));
    assert!(overlap("Crates/A", "crates/a/x.rs"), "case aside, as a default APFS volume");
    let (composed, decomposed) =
        (owned("\u{e9}t\u{e9}").unwrap(), owned("e\u{301}te\u{301}").unwrap());
    assert!(overlap(&composed, &decomposed), "one name however it was spelled");
}

/// Two live writing tasks never own one file: a create or a claim that overlaps is refused
/// and changes nothing, a task that only reads owns nothing and overlaps nobody, and a task
/// merged or given up lets its paths go.
#[test]
fn a_claim_that_overlaps_a_live_task_is_refused_until_that_task_is_done_with() {
    let mut p = project(None);
    let server = task(&mut p, "Server store", &["crates/slopty-server"]);
    let refused = p.create_task(&id(), spec("Hub", &["crates/slopty-server/src/hub.rs"]), now());
    let refused = refused.unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Conflict);
    let said = message(&refused);
    assert!(said.contains("task 1") && said.contains("crates/slopty-server"), "{said}");
    assert_eq!(status(&p).tasks.len(), 1, "nothing made");

    let reader = TaskSpec { read_only: true, ..spec("Review the server", &[]) };
    let reader = p.create_task(&id(), reader, now()).unwrap().0.id;
    let claim = p.claim(&id(), reader, &["crates/slopty-server".to_owned()], now());
    assert!(message(&claim.unwrap_err()).contains("only reads"));
    let owning_reader = TaskSpec { read_only: true, ..spec("x", &["docs"]) };
    assert_eq!(code(&p.create_task(&id(), owning_reader, now()).unwrap_err()), ErrorCode::Invalid);

    let tools = task(&mut p, "Tools", &["crates/slopty-tools"]);
    let over = p.claim(&id(), tools, &["crates/slopty-server/src/lib.rs".to_owned()], now());
    assert_eq!(code(&over.unwrap_err()), ErrorCode::Conflict);
    assert_eq!(get(&p, tools).owns, ["crates/slopty-tools"], "a refused claim takes nothing");
    let (claimed, updates) = p.claim(&id(), tools, &["docs/".to_owned()], now()).unwrap();
    assert_eq!(claimed.owns, ["crates/slopty-tools", "docs"]);
    assert!(matches!(
        kept(&updates).as_slice(),
        [Kept { entry: Some(TimelineEntry { what: Moment::Claimed { .. }, .. }), .. }]
    ));
    let (_, again) = p.claim(&id(), tools, &["DOCS".to_owned()], now()).unwrap();
    assert!(again.is_empty(), "a path owned already, in any case, is no change");
    let whole = p.claim(&id(), tools, &[".".to_owned()], now());
    assert_eq!(code(&whole.unwrap_err()), ErrorCode::Conflict, "the root overlaps everything");

    p.update_task(&id(), server, to(TaskState::Done), Caller::Person, now()).unwrap();
    p.update_task(&id(), server, to(TaskState::Merged), Caller::Person, now()).unwrap();
    p.create_task(&id(), spec("Hub", &["crates/slopty-server/src/hub.rs"]), now()).unwrap();
    let late = p.claim(&id(), server, &["x".to_owned()], now());
    assert_eq!(code(&late.unwrap_err()), ErrorCode::Invalid, "a merged task claims nothing");
}

/// A merged task is final and only finished work merges; a task given up and planned again
/// takes its paths back only when nobody took them meanwhile.
#[test]
fn a_task_moves_along_its_lifecycle_and_takes_its_paths_back_only_when_they_are_free() {
    let mut p = project(None);
    let a = task(&mut p, "A", &["crates/a"]);
    let refused =
        p.update_task(&id(), a, to(TaskState::Merged), Caller::Person, now()).unwrap_err();
    assert!(message(&refused).contains("only a done or verifying task merges"));
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    p.update_task(&id(), a, to(TaskState::Merged), Caller::Person, now()).unwrap();
    for back in [TaskState::Planned, TaskState::Running, TaskState::Failed] {
        let refused = p.update_task(&id(), a, to(back), Caller::Person, now()).unwrap_err();
        assert!(message(&refused).contains("final"), "{back:?}");
    }

    let b = task(&mut p, "B", &["crates/b"]);
    p.update_task(&id(), b, to(TaskState::Failed), Caller::Person, now()).unwrap();
    let c = task(&mut p, "C", &["crates/b/src"]);
    let back = p.update_task(&id(), b, to(TaskState::Planned), Caller::Person, now()).unwrap_err();
    assert_eq!(code(&back), ErrorCode::Conflict, "task {c} owns part of it now");
    assert_eq!(get(&p, b).state, TaskState::Failed, "a refused move changes nothing");
    p.update_task(&id(), c, to(TaskState::Failed), Caller::Person, now()).unwrap();
    p.update_task(&id(), b, to(TaskState::Planned), Caller::Person, now()).unwrap();
}

#[test]
fn a_task_is_made_under_a_known_parent_numbered_in_order_within_the_project_s_depth() {
    let mut p = Projects::default();
    let shallow = LimitsChange { depth: Some(2), ..LimitsChange::default() };
    p.create(new_project(None, shallow), &Fleet::default().running(), now()).unwrap();
    let first = task(&mut p, "Split", &[]);
    let child = p.create_task(&id(), TaskSpec { parent: Some(first), ..spec("Leaf", &[]) }, now());
    let (child, _) = child.unwrap();
    assert_eq!((first, child.id, child.parent), (TaskId(1), TaskId(2), Some(TaskId(1))));
    let deeper = TaskSpec { parent: Some(child.id), ..spec("Deeper", &[]) };
    let refused = p.create_task(&id(), deeper, now()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Limit);
    assert!(message(&refused).contains("3 deep, past the project's depth of 2"));
    let orphan = TaskSpec { parent: Some(TaskId(9)), ..spec("x", &[]) };
    assert_eq!(code(&p.create_task(&id(), orphan, now()).unwrap_err()), ErrorCode::UnknownTask);
    assert_eq!(code(&p.create_task(&id(), spec(" ", &[]), now()).unwrap_err()), ErrorCode::Invalid);
    let elsewhere = ProjectId::new("other").unwrap();
    assert_eq!(
        code(&p.create_task(&elsewhere, spec("x", &[]), now()).unwrap_err()),
        ErrorCode::UnknownProject
    );
}

/// Dependencies form a graph that never leads back: a task depends on tasks that exist, never
/// on itself, and never on one that already needs it, however far round.
#[test]
fn dependencies_are_a_graph_that_refuses_a_cycle() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A", &[]), task(&mut p, "B", &[]));
    let c = TaskSpec { depends_on: vec![b, a, b], ..spec("C", &[]) };
    let c = p.create_task(&id(), c, now()).unwrap().0;
    assert_eq!(c.depends_on, [b, a], "each once, in order");
    let unknown = TaskSpec { depends_on: vec![TaskId(9)], ..spec("D", &[]) };
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
/// its agent says what it is doing. A rule that is not CEL is refused before it is kept.
#[test]
fn a_task_keeps_its_kind_metadata_status_and_placement_checked() {
    let mut p = project(None);
    let bench = TaskSpec {
        kind: " bench ".to_owned(),
        metadata: Some(r#"{ "iterations": 5, "tags": ["cold"] }"#.to_owned()),
        placement: Placement { require: vec!["cpus >= 16".to_owned()], ..Placement::default() },
        verifier: Some("cargo bench".to_owned()),
        ..spec("Bench", &[])
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
    let rule = Placement { require: vec!["os ==".to_owned()], ..Placement::default() };
    let refused = p.create_task(&id(), TaskSpec { placement: rule, ..spec("x", &[]) }, now());
    assert_eq!(code(&refused.unwrap_err()), ErrorCode::BadExpression);

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
    let t = task(&mut p, "Work", &[]);
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
        }),
        ..TaskChange::default()
    };
    let (_, updates) = p.update_task(&id(), t, verifying, Caller::Person, now()).unwrap();
    assert_eq!(updates.len(), 2, "moved, verified");
    assert!(p.agent_status(at, &AgentStatus::Working, now()).is_empty());
    assert_eq!(get(&p, t).state, TaskState::Verifying);

    let gone = p.session_ended(at, now());
    assert!(matches!(
        kept(&gone).as_slice(),
        [Kept { entry: Some(TimelineEntry { what: Moment::AgentGone { .. }, .. }), .. }]
    ));
    assert!(get(&p, t).assignment.is_some_and(|a| a.ended_ms == Some(now())));
}

/// One terminal per task and one task per terminal: a second live terminal for a task, or a
/// terminal already on another task, is refused; the same one again is no change; a terminal
/// that is no longer live leaves the task free.
#[test]
fn a_terminal_works_on_one_task_and_a_task_has_one_live_terminal() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A", &[]), task(&mut p, "B", &[]));
    let at = term();
    assign(&mut p, a, at).unwrap();
    assert!(assign(&mut p, a, at).unwrap().1.is_empty());
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
    let mut p = Projects::default();
    let two = LimitsChange { live_per_project: Some(2), ..LimitsChange::default() };
    p.create(new_project(Some(orchestrator), two), &fleet.running(), now()).unwrap();
    let (a, b) = (task(&mut p, "A", &[]), task(&mut p, "B", &[]));
    let at = fleet.open();
    p.assign(&id(), a, who(at, true, None), &fleet.terminals, now()).unwrap();
    p.update_task(&id(), a, to(TaskState::Done), Caller::Person, now()).unwrap();
    let live = p.status(&id(), None, &fleet.running()).unwrap().live;
    assert_eq!((live.project, live.fleet), (2, 2), "the orchestrator and a done task's agent");
    let full = p.may_start(&id(), b, false, &fleet.running()).unwrap_err();
    assert_eq!(code(&full), ErrorCode::Limit);
    assert!(message(&full).contains("live_per_project of 2"), "{}", message(&full));

    fleet.terminals.remove(&orchestrator);
    fleet.agents.remove(&orchestrator);
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
    assert_eq!(p.live_on(&id(), plain.worker, &fleet.running()), 1, "a start in flight counts");
    let twice = p.may_start(&id(), b, false, &fleet.running()).unwrap_err();
    assert!(message(&twice).contains("being started"));
}

/// A server that was away learns which of a worker's terminals ended meanwhile when the worker
/// registers again, and their tasks are free.
#[test]
fn a_worker_s_terminals_that_ended_while_the_server_was_away_end_their_assignments() {
    let mut p = project(None);
    let (a, b) = (task(&mut p, "A", &[]), task(&mut p, "B", &[]));
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
    let t = task(&mut p, "Work", &[]);
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

/// A status line's worktree and pull request land on the task its agent works on, whether they
/// came before the assignment or after.
#[test]
fn a_branch_lands_on_the_task_its_agent_works_on() {
    let mut p = project(None);
    let t = task(&mut p, "Work", &[]);
    let at = term();
    let worktree = Worktree {
        name: "rows".to_owned(),
        path: "/w/.claude/worktrees/rows".to_owned(),
        branch: Some("worktree-rows".to_owned()),
        original_cwd: "/w".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session: at.session, pr: None, worktree: Some(worktree) };
    let terminals = HashSet::from([at]);
    let (assigned, _) =
        p.assign(&id(), t, who(at, false, Some(&branch)), &terminals, now()).unwrap();
    assert_eq!(assigned.branch.as_deref(), Some("worktree-rows"));
    assert_eq!(assigned.worktree.as_deref(), Some("/w/.claude/worktrees/rows"));
    assert!(p.report(at.worker, &AgentReport::Branch(branch.clone()), now()).is_empty());
    let pr = slopty_proto::agent::PullRequest {
        number: 7,
        url: "https://github.com/o/r/pull/7".to_owned(),
        review: None,
        merge_request: false,
    };
    let opened = AgentBranch { pr: Some(pr), ..branch };
    let updates = kept(&p.report(at.worker, &AgentReport::Branch(opened), now()));
    let [Kept { entry: Some(entry), .. }] = updates.as_slice() else { panic!("{updates:?}") };
    assert_eq!(
        entry.what,
        Moment::Branch { branch: Some("worktree-rows".to_owned()), pr: Some(7) }
    );
}

/// A project's limits stay within the person's bounds: a limit past one is refused naming the
/// setting, and bounds lowered later bring every project's limits down with them.
#[test]
fn limits_stay_within_the_person_s_bounds() {
    let mut p = Projects::default();
    let fleet = Fleet::default();
    let greedy = LimitsChange { live_per_worker: Some(9), ..LimitsChange::default() };
    let refused = p.create(new_project(None, greedy), &fleet.running(), now()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Limit);
    assert!(message(&refused).contains("[server.projects] live_per_worker"));
    let zero = LimitsChange { depth: Some(0), ..LimitsChange::default() };
    assert_eq!(
        code(&p.create(new_project(None, zero), &fleet.running(), now()).unwrap_err()),
        ErrorCode::Invalid
    );
    let (made, _) =
        p.create(new_project(None, LimitsChange::default()), &fleet.running(), now()).unwrap();
    assert_eq!(made.project.limits, Limits::default());
    assert_eq!(made.bounds, Bounds::default());

    let tight = Bounds { live_per_worker: 2, timeline_kept: 16, ..Bounds::default() };
    let permitted = BTreeSet::from([id()]);
    let updates = p.set_policy(Policy { bounds: tight, permission_flags: permitted });
    assert_eq!(updates.len(), 1);
    let s = status(&p);
    assert_eq!((s.project.limits.live_per_worker, s.project.limits.timeline_kept), (2, 16));
    assert!(s.bounds.permission_flags, "this project may loosen its agents' permissions");
    let t = task(&mut p, "Notes", &[]);
    for i in 0..40 {
        let note = TaskChange { note: Some(format!("{i}")), ..TaskChange::default() };
        p.update_task(&id(), t, note, Caller::Person, now()).unwrap();
    }
    let all = status(&p);
    assert_eq!(all.timeline.len(), 16, "the timeline keeps what the limit says");
    assert_eq!(all.timeline.last().map(|e| e.seq), Some(all.next - 1));
}

/// What the store writes reads back the same, timeline numbering included.
#[test]
fn the_file_holds_everything_and_reads_back_the_same() {
    let mut p = project(Some(term()));
    let t = task(&mut p, "Work", &["crates/a"]);
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

/// A task given up lets its paths go; starting it again, or putting a terminal on it, takes
/// them back only when nobody took them meanwhile, as planning it again does.
#[test]
fn a_task_given_up_is_not_reopened_over_paths_another_task_took() {
    let mut p = project(None);
    let b = task(&mut p, "B", &["crates/b"]);
    p.update_task(&id(), b, to(TaskState::Failed), Caller::Person, now()).unwrap();
    let c = task(&mut p, "C", &["crates/b/src"]);
    let fleet = Fleet::default();
    let start = p.may_start(&id(), b, false, &fleet.running()).unwrap_err();
    assert_eq!(code(&start), ErrorCode::Conflict, "task {c} owns part of it now");
    assert_eq!(code(&assign(&mut p, b, term()).unwrap_err()), ErrorCode::Conflict);
    assert_eq!(get(&p, b).state, TaskState::Failed, "nothing moved");
    p.update_task(&id(), c, to(TaskState::Failed), Caller::Person, now()).unwrap();
    p.may_start(&id(), b, false, &fleet.running()).unwrap();
    assert_eq!(assign(&mut p, b, term()).unwrap().0.state, TaskState::Running);
}

/// What a project and its tasks carry is bounded as it comes in: text past its bound, a claim
/// of more paths than a task may own (checked before any is read), and a report too large are
/// refused, so no task or page outgrows a frame.
#[test]
fn what_a_project_holds_is_bounded_as_it_comes_in() {
    use slopty_proto::project::{ARTIFACTS_MAX, NOTE_MAX, REF_MAX, Report, ReportKind};

    let mut p = Projects::default();
    let long_repo =
        NewProject { repo: "r".repeat(REF_MAX + 1), ..new_project(None, LimitsChange::default()) };
    let refused = p.create(long_repo, &Fleet::default().running(), now()).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Invalid, "{}", message(&refused));
    let mut p = project(None);
    let t = task(&mut p, "T", &[]);
    let most = usize::from(Bounds::default().owns_max);
    let many: Vec<String> = (0..=most).map(|n| format!("crates/{n}")).collect();
    let over = p.claim(&id(), t, &many, now()).unwrap_err();
    assert_eq!(code(&over), ErrorCode::Limit, "{}", message(&over));
    let huge: Vec<String> = vec!["x".to_owned(); 1 << 16];
    assert_eq!(code(&p.claim(&id(), t, &huge, now()).unwrap_err()), ErrorCode::Limit);

    let report = |note: String, artifacts: usize| Report {
        kind: ReportKind::Checkpoint,
        note,
        artifacts: vec!["target/x".to_owned(); artifacts],
        branch: None,
        pr: None,
    };
    let long = report("n".repeat(NOTE_MAX + 1), 0);
    assert_eq!(code(&p.report_task(&id(), t, &long, now()).unwrap_err()), ErrorCode::Invalid);
    let crowded = report("ok".to_owned(), ARTIFACTS_MAX + 1);
    assert_eq!(code(&p.report_task(&id(), t, &crowded, now()).unwrap_err()), ErrorCode::Invalid);
    let ((_, parent), updates) =
        p.report_task(&id(), t, &report("ok".to_owned(), 2), now()).unwrap();
    assert_eq!(parent, None, "a top task reports to the orchestrator");
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
    let t = task(&mut p, "Notes", &[]);
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
/// start time kept; and a server that stopped mid-step says, as it loads, that the step ended.
#[test]
fn a_step_is_shown_as_it_goes_and_ends_with_the_server() {
    use slopty_proto::project::{StepKind, StepState, TaskStep};
    let mut p = project(None);
    let a = task(&mut p, "A", &[]);
    let (worker, since) = (WorkerId::new(), now());
    let step = |state, since_ms| TaskStep { kind: StepKind::Clone, worker, state, since_ms };
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

    let mut p = Projects::restore(p.file(Vec::new(), 0));
    let why = "the server stopped while it ran".to_owned();
    assert_eq!(get(&p, a).step.map(|s| s.state), Some(StepState::Failed { why }));
    let done = StepState::Done { detail: "/home/c/slopty/clones/example.com/o/demo".to_owned() };
    p.set_step(&id(), a, step(done, later), later).unwrap();
    assert_eq!(steps_logged(&p), 2, "an end is logged");
}
