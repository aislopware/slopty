//! Projects through the hub: verbs answered from the store, a task's terminal placed on the
//! worker it names or the least busy one and put on the task, every start counted against the
//! bounds, and what a worker's link reports moving the task.

use std::collections::BTreeMap;
use std::time::Duration;

use slopty_agent::status::{AgentStatus, BlockReason};
use slopty_agent::vouch::SessionKey;
use slopty_proto::agent::Worktree;
use slopty_proto::orchestration::{BranchBundle, ThreadOf, UploadPart};
use slopty_proto::project::{
    ASKING_ENV, Bounds, Fact, LimitsChange, Moment, PROJECT_ENV, ProjectId, RunOn, TASK_ENV,
    TaskCard, TaskChange, TaskId, TaskLaunch, TaskSpec, TaskState,
};
use slopty_proto::server::Os;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::{PullStands, TableFrame};

use super::tests::{caps, registration, summary};
use super::*;
use crate::project::Policy;

pub(super) fn project() -> ProjectId {
    ProjectId::new("slopty").unwrap()
}

/// A worker on `os` named `name`, running `sessions`.
pub(super) fn worker_on(
    hub: &Hub,
    name: &str,
    os: Os,
    sessions: Vec<SessionSummary>,
) -> (WorkerId, Lease, mpsc::Receiver<FromServer>) {
    let worker = WorkerId::new();
    worker_again(hub, worker, name, os, sessions)
}

pub(super) fn worker_again(
    hub: &Hub,
    worker: WorkerId,
    name: &str,
    os: Os,
    sessions: Vec<SessionSummary>,
) -> (WorkerId, Lease, mpsc::Receiver<FromServer>) {
    let mut registration = registration(worker, sessions);
    registration.name = name.to_owned();
    registration.caps = slopty_proto::server::WorkerCaps { os, ..caps() };
    let (tx, rx) = mpsc::channel(8);
    let ip = IpAddr::from([100, 64, 0, if name == "box" { 9 } else { 7 }]);
    registration.caps.agents = vec![slopty_proto::server::InstalledAgent {
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        version: "2.1.0".to_owned(),
        offers: slopty_proto::thread::Offers::default(),
        managed_hooks_off: false,
    }];
    let lease = hub.register(registration, ip, tx).unwrap();
    (worker, lease, rx)
}

/// A worker's facts that say these agents are installed, and nothing else.
pub(super) fn installed(agents: &[&str]) -> BTreeMap<String, Fact> {
    let agents = agents.iter().map(|a| ((*a).to_owned(), Fact::Text("1.0.0".to_owned())));
    BTreeMap::from([("agents".to_owned(), Fact::Map(agents.collect()))])
}

pub(super) async fn create_with(hub: &Hub, orchestrator: Option<TermRef>, limits: LimitsChange) {
    let made = hub
        .dispatch(Verb::ProjectCreate {
            project: project(),
            title: "Projects".to_owned(),
            repo: "~/src/slopty".to_owned(),
            target: "main".to_owned(),
            verifier: None,
            push: false,
            orchestrator,
            limits,
            metadata: None,
            goal: None,
            autonomy: slopty_proto::project::Autonomy::Ask,
        })
        .await;
    assert!(matches!(made, Outcome::Project(_)), "{made:?}");
}

pub(super) async fn create(hub: &Hub, orchestrator: Option<TermRef>) {
    create_with(hub, orchestrator, LimitsChange::default()).await;
}

/// A task pinned to `pin`, when it names a worker.
pub(super) async fn new_task(hub: &Hub, pin: Option<WorkerId>) -> TaskId {
    let spec = TaskSpec {
        title: "Server".to_owned(),
        brief: "Build it.".to_owned(),
        pin,
        ..TaskSpec::default()
    };
    match hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await {
        Outcome::Task(task) => task.id,
        other => panic!("{other:?}"),
    }
}

pub(super) async fn status(hub: &Hub) -> ProjectStatus {
    let verb = Verb::ProjectStatus { project: project(), since: Some(0), timeout_ms: 0 };
    match hub.dispatch(verb).await {
        Outcome::Project(status) => *status,
        other => panic!("{other:?}"),
    }
}

pub(super) async fn task_now(hub: &Hub, task: TaskId) -> TaskCard {
    status(hub).await.tasks.into_iter().find(|t| t.id == task).unwrap()
}

pub(super) fn claude() -> TaskLaunch {
    TaskLaunch { pin: None, agent: AgentId::named(AgentId::CLAUDE_CODE) }
}

pub(super) fn spawn(hub: &Hub, verb: Verb) -> tokio::task::JoinHandle<Outcome> {
    let hub = hub.clone();
    tokio::spawn(async move { hub.dispatch(verb).await })
}

pub(super) async fn request(rx: &mut mpsc::Receiver<FromServer>) -> (RequestId, Verb) {
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Some(FromServer::Request { id, verb, .. })) => (id, verb),
        other => panic!("no request: {other:?}"),
    }
}

/// The worker opens the terminal it was asked for, under the id the server chose, with an agent
/// in it, and answers.
pub(super) fn opened(lease: &Lease, (id, verb): &(RequestId, Verb)) -> TermRef {
    let (worker, session) = chosen(verb);
    lease.handle(ToServer::SessionChanged(summary(session)));
    lease.handle(agent(session, &AgentStatus::Working));
    let term = TermRef { worker, session };
    lease.handle(ToServer::Reply { id: *id, outcome: Outcome::Opened(term) });
    term
}

/// The worker and the terminal id a start names: the server always chooses the id.
fn chosen(verb: &Verb) -> (WorkerId, SessionId) {
    match verb {
        Verb::SpawnAgent { worker, session: Some(session), .. }
        | Verb::OpenTerminal { worker, session: Some(session), .. } => (*worker, *session),
        other => panic!("not a start under a chosen id: {other:?}"),
    }
}

pub(super) fn refused(outcome: &Outcome, code: ErrorCode) -> &str {
    match outcome {
        Outcome::Error { code: c, message } if *c == code => message,
        other => panic!("not {code:?}: {other:?}"),
    }
}

/// A task pinned to the Linux worker starts there, never on the Mac, told its brief, with the
/// project and the task in its environment; once the worker answers it is the task's terminal,
/// and every link hears so. With the Linux worker gone, the start is refused saying
/// why.
#[tokio::test]
async fn a_pinned_task_is_spawned_on_its_worker_and_put_on_its_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_mac, _mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Some(linux)).await;
    let mut pushed = hub.subscribe();

    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut linux_rx).await;
    let verb = start.1.clone();
    let Verb::SpawnAgent { worker, env, cwd, prompt, .. } = verb else { panic!("{verb:?}") };
    assert_eq!((worker, cwd.as_str()), (linux, "~/src/slopty"), "no clone known: its repo");
    assert_eq!(prompt.as_deref(), Some("Build it."), "told its brief");
    let last = |name: &str| env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    assert_eq!(last(PROJECT_ENV), Some("slopty"));
    assert_eq!(last(TASK_ENV), Some("1"));
    assert!(mac_rx.try_recv().is_err(), "the Mac was asked nothing");

    let term = opened(&linux_lease, &start);
    let Outcome::Task(started) = asked.await.unwrap() else { panic!("not a task") };
    assert_eq!(started.assignment.map(|a| a.term), Some(term));
    assert_eq!(started.state, TaskState::Running);
    let mut assigned = false;
    while let Ok(msg) = pushed.try_recv() {
        if let FromServer::Event(HubEvent { what: Happening::Project(update), .. }) = msg {
            assigned |= matches!(
                update.entry.as_ref().map(|e| &e.what),
                Some(Moment::Assigned { spawned: true, .. })
            );
        }
    }
    assert!(assigned, "every link hears the task has its terminal");
    let again = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude() });
    refused(&again.await, ErrorCode::Conflict);

    drop(linux_lease);
    let other = new_task(&hub, Some(linux)).await;
    let no = hub.dispatch(Verb::TaskSpawn { project: project(), task: other, launch: claude() });
    let no = no.await;
    assert_eq!(refused(&no, ErrorCode::Unplaced), "no worker can take task 2 now: box: not online");
}

/// The worker a start names is the orchestrator's own word: the task starts there whatever
/// worker the task names.
#[tokio::test]
async fn a_start_s_pin_goes_over_the_task_s() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, _linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Some(linux)).await;
    let launch = TaskLaunch { pin: Some(mac), ..claude() };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
    let start = request(&mut mac_rx).await;
    let verb = start.1.clone();
    assert!(matches!(verb, Verb::SpawnAgent { worker, .. } if worker == mac));
    linux_rx.try_recv().unwrap_err();
    opened(&mac_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
}

/// The person's "Run on" pins a task to a worker, and "Anywhere" lets the server choose again;
/// the card says where it is pinned.
#[tokio::test]
async fn a_task_runs_where_the_person_says_and_keeps_why_it_went_there() {
    use slopty_proto::project::RunOn;
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let run_on = |task, run_on| Verb::TaskUpdate {
        project: project(),
        task,
        change: Box::new(TaskChange { run_on: Some(run_on), ..TaskChange::default() }),
    };

    let pinned = new_task(&hub, None).await;
    assert!(matches!(hub.dispatch(run_on(pinned, RunOn::Worker(mac))).await, Outcome::Task(_)));
    assert_eq!(task_now(&hub, pinned).await.pin, Some(mac), "the card says where it is pinned");
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task: pinned, launch: claude() });
    let start = request(&mut mac_rx).await;
    opened(&mac_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    linux_rx.try_recv().unwrap_err();

    let free = new_task(&hub, None).await;
    hub.dispatch(run_on(free, RunOn::Worker(mac))).await;
    hub.dispatch(run_on(free, RunOn::Anywhere)).await;
    assert_eq!(task_now(&hub, free).await.pin, None, "anywhere takes the pin off");
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task: free, launch: claude() });
    let start = request(&mut linux_rx).await;
    assert_eq!(chosen(&start.1).0, linux, "the worker running the fewest agents");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
}

/// Two starts at once never both take the fleet's last place: the second is placed while the
/// first is still starting, and counts it.
#[tokio::test]
async fn concurrent_starts_never_pass_the_fleet_s_bound() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    hub.set_policy(Policy {
        bounds: Bounds { live_agents: 1, ..Bounds::default() },
        ..Policy::default()
    });
    create(&hub, None).await;
    let (a, b) = (new_task(&hub, None).await, new_task(&hub, None).await);
    let first = spawn(&hub, Verb::TaskSpawn { project: project(), task: a, launch: claude() });
    let second = spawn(&hub, Verb::TaskSpawn { project: project(), task: b, launch: claude() });
    let start = request(&mut rx).await;
    // Either may be placed first; the other is refused while it starts.
    let (mut first, mut second) = (first, second);
    let (late, placed, refused_task) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            late = &mut first => (late.unwrap(), second, a),
            late = &mut second => (late.unwrap(), first, b),
        }
    })
    .await
    .unwrap();
    assert!(refused(&late, ErrorCode::Limit).contains("live_agents"), "{late:?}");
    assert!(rx.try_recv().is_err(), "one start went to the worker");
    opened(&lease, &start);
    assert!(matches!(placed.await.unwrap(), Outcome::Task(_)));
    let again = Verb::TaskSpawn { project: project(), task: refused_task, launch: claude() };
    let full = hub.dispatch(again);
    assert!(refused(&full.await, ErrorCode::Limit).contains("runs 1 agents"));
}

/// A caller that leaves while its start is on the way does not leave an agent running for
/// nobody: the start goes on, and what the worker opens is the task's.
#[tokio::test]
async fn a_start_whose_caller_left_still_puts_its_terminal_on_the_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    asked.abort();
    let term = opened(&lease, &start);
    let assigned = async {
        while task_now(&hub, task).await.assignment.is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), assigned).await.expect("the start went on");
    assert_eq!(task_now(&hub, task).await.assignment.map(|a| a.term), Some(term));
    assert_eq!(status(&hub).await.live.project, 1, "and it counts");
}

/// A start whose task was merged meanwhile does not leave its terminal running: the worker is
/// told to close it.
#[tokio::test]
async fn a_start_its_task_can_no_longer_take_is_closed() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    for state in [TaskState::Done, TaskState::Merged] {
        let change = TaskChange { state: Some(state), ..TaskChange::default() };
        let change = Box::new(change);
        let moved = hub.dispatch(Verb::TaskUpdate { project: project(), task, change }).await;
        assert!(matches!(moved, Outcome::Task(_)), "{moved:?}");
    }
    let term = opened(&lease, &start);
    let (close, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term });
    lease.handle(ToServer::Reply { id: close, outcome: Outcome::Done });
    assert!(refused(&asked.await.unwrap(), ErrorCode::Invalid).contains("merged"));
}

/// The person's bounds hold across the fleet: every agent counts, in a project or not, and a
/// start past the fleet's bound is refused naming the setting.
#[tokio::test]
async fn every_agent_counts_against_the_fleet_bound_the_person_set() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    hub.set_policy(Policy {
        bounds: Bounds { live_agents: 1, ..Bounds::default() },
        ..Policy::default()
    });
    create(&hub, None).await;
    let plain = || Verb::SpawnAgent {
        worker: linux,
        cwd: "~".to_owned(),
        prompt: None,
        args: Vec::new(),
        env: Vec::new(),
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    let asked = spawn(&hub, plain());
    let start = request(&mut rx).await;
    opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Opened(_)));
    let said = refused(&hub.dispatch(plain()).await, ErrorCode::Limit).to_owned();
    assert!(said.contains("runs 1 agents") && said.contains("live_agents"), "{said}");
    let task = new_task(&hub, None).await;
    let start = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude() });
    refused(&start.await, ErrorCode::Limit);
}

/// No agent starts another with more than it has: flags that loosen Claude Code's or Codex's
/// permissions are refused on every way to start one that takes them, unless the person allowed
/// them. A task's start takes none: the server makes its arguments.
#[tokio::test]
async fn flags_that_loosen_permissions_need_the_person_s_word() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, _lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let plain = Verb::SpawnAgent {
        worker: linux,
        cwd: "~".to_owned(),
        prompt: None,
        args: vec!["--allowedTools".to_owned(), "Bash".to_owned()],
        env: vec![(PROJECT_ENV.to_owned(), "slopty".to_owned())],
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    refused(&hub.dispatch(plain).await, ErrorCode::Limit);
    let terminal = Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: ["/opt/bin/claude", r#"--settings={"permissions":{"allow":["Bash"]}}"#]
            .map(str::to_owned)
            .to_vec(),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    refused(&hub.dispatch(terminal).await, ErrorCode::Limit);
    for codex in [&["codex", "--yolo"][..], &["/usr/local/bin/codex", "-s", "danger-full-access"]] {
        let terminal = Verb::OpenTerminal {
            worker: linux,
            cwd: None,
            command: codex.iter().map(|w| (*w).to_owned()).collect(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
        };
        let said = refused(&hub.dispatch(terminal).await, ErrorCode::Limit).to_owned();
        assert!(said.contains("permission_flags"), "{codex:?}: {said}");
    }
    assert!(rx.try_recv().is_err(), "nothing reached the worker");

    let allowed = Policy { permission_flags: [project()].into(), ..Policy::default() };
    hub.set_policy(allowed);
    assert!(status(&hub).await.bounds.permission_flags);
    let start = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let (_, verb) = request(&mut rx).await;
    let Verb::SpawnAgent { args, permission_flags, .. } = verb else { panic!("{verb:?}") };
    assert!(permission_flags, "the worker leaves bypass mode unlocked");
    assert!(!args.iter().any(|a| a == "default"), "no mode pinned: {args:?}");
    start.abort();
}

/// A server that restarts learns, when a worker registers again, which of its tasks' terminals
/// ended while it was away: those tasks are free to start again.
#[tokio::test]
async fn a_restarted_server_frees_the_tasks_whose_terminals_ended_while_it_was_away() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let session = SessionId::new();
    let (linux, lease, _rx) = worker_on(&hub, "box", Os::Linux, vec![summary(session)]);
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let term = TermRef { worker: linux, session };
    let assigned = hub.assign_for_test(&project(), task, term);
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    let (file, known) = (hub.projects_file(0), hub.directory());
    drop(lease);
    drop(hub);

    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let (_, lease, mut rx) = worker_again(&hub, linux, "box", Os::Linux, Vec::new());
    let t = task_now(&hub, task).await;
    assert!(t.assignment.as_ref().is_some_and(|a| a.ended_ms.is_some()), "{t:?}");
    let again = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    opened(&lease, &start);
    assert!(matches!(again.await.unwrap(), Outcome::Task(_)));
}

/// What a worker reports of itself is a fact `slopty workers` shows, beside what the server
/// knows of it, whose word wins.
#[tokio::test]
async fn a_worker_s_reported_facts_are_shown_under_the_server_s() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, _rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let labels = BTreeMap::from([("fast-disk".to_owned(), Fact::Bool(true))]);
    lease.handle(ToServer::Facts(BTreeMap::from([
        ("labels".to_owned(), Fact::Map(labels)),
        ("os".to_owned(), Fact::Text("pretend".to_owned())),
    ])));
    let Outcome::Facts(all) = hub.dispatch(Verb::WorkerFacts { worker: Some(linux) }).await else {
        panic!("facts")
    };
    let facts = &all.first().unwrap().facts;
    assert_eq!(facts.get("os"), Some(&Fact::Text("linux".to_owned())), "the server's word wins");
    assert!(facts.contains_key("labels") && facts.contains_key("cpus"));
}

/// The worker's table moving the thread of the agent in `session` to where `status` says, as
/// its codec maps a hook's word into the row: an agent gone takes its row with it.
pub(super) fn agent(session: SessionId, status: &AgentStatus) -> ToServer {
    use slopty_proto::thread::{Cursor, Phase, Request, ThreadId, Wait};
    let thread = ThreadId::from_uuid(*session.as_uuid());
    let cursor = Cursor::default();
    let phase = match status {
        AgentStatus::None => {
            let removed = vec![thread];
            return ToServer::Threads(TableFrame::Delta { cursor, rows: Vec::new(), removed });
        }
        AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => Phase::Idle,
        AgentStatus::Working | AgentStatus::Tool { .. } => Phase::Working,
        AgentStatus::Blocked(_) => Phase::NeedsYou,
        AgentStatus::Done => Phase::Done,
        AgentStatus::Failed { .. } => Phase::Failed,
        AgentStatus::Waiting { .. } => Phase::Waiting,
    };
    let mut row = ladder::tests::row(phase, 1, Some(session));
    row.id = thread;
    let asked = match status {
        AgentStatus::Blocked(BlockReason::Permission { tool }) => Some((Request::APPROVAL, tool)),
        AgentStatus::Blocked(BlockReason::Elicitation) => Some((Request::ELICITATION, &row.title)),
        AgentStatus::Blocked(BlockReason::Question) => Some((Request::QUESTION, &row.title)),
        _ => None,
    };
    if let Some((kind, title)) = asked {
        let title = title.clone();
        row = ladder::tests::asking(row, &title);
        row.requests[0].kind = kind.to_owned();
    }
    if let AgentStatus::Failed { error, .. } = status
        && error == AgentStatus::RATE_LIMIT
    {
        row.status.wait = Some(Wait { kind: Wait::LIMIT.to_owned(), text: String::new() });
    }
    ToServer::Threads(TableFrame::Delta { cursor, rows: vec![row], removed: Vec::new() })
}

/// What the worker's link says of an agent reaches its task: the worktree its status line
/// named before the agent was put on the task, the pull request its thread's row names after,
/// Claude Code's own subagent as a child node, a block, and at last its terminal closing.
#[tokio::test]
async fn a_worker_s_reports_move_the_task_its_agent_works_on() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let session = SessionId::new();
    let (worker, lease, _rx) = worker_on(&hub, "studio", Os::MacOs, vec![summary(session)]);
    let term = TermRef { worker, session };
    let worktree = Worktree {
        name: "rows".to_owned(),
        path: "/w/.claude/worktrees/rows".to_owned(),
        branch: Some("worktree-rows".to_owned()),
        original_cwd: "/w".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session, worktree: Some(worktree) };
    lease.handle(ToServer::Report(AgentReport::Branch(branch.clone())));
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let Outcome::Task(assigned) = hub.assign_for_test(&project(), task, term) else {
        panic!("assigned")
    };
    assert_eq!(assigned.worktree.as_deref(), Some("/w/.claude/worktrees/rows"));

    lease.handle(ToServer::Report(AgentReport::SubagentStarted {
        session,
        agent: "ag1".to_owned(),
        kind: "Explore".to_owned(),
    }));
    let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    let ToServer::Threads(TableFrame::Delta { cursor, mut rows, removed }) =
        agent(session, &blocked)
    else {
        panic!("its row")
    };
    for row in &mut rows {
        row.pull = Some(ladder::tests::pull_seen(PullStands::Waiting));
    }
    lease.handle(ToServer::Threads(TableFrame::Delta { cursor, rows, removed }));
    let s = status(&hub).await;
    let t = s.tasks.first().unwrap();
    assert_eq!(t.pull.as_ref().map(|p| p.number), Some(42));
    assert_eq!(t.natives.agents, 1, "counted on its card");
    let Outcome::Node(node) =
        hub.dispatch(Verb::TaskGet { project: project(), task: Some(task) }).await
    else {
        panic!("its node")
    };
    assert_eq!(node.natives.agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["ag1"]);
    assert_eq!(t.state, TaskState::Blocked);

    lease.handle(ToServer::SessionClosed {
        session,
        reason: slopty_proto::terminal::CloseReason::Exited,
    });
    let t = task_now(&hub, task).await;
    assert!(t.assignment.is_some_and(|a| a.ended_ms.is_some()), "its terminal is gone");
}

/// A keyed change is made once: its repeat answers the same task and makes no second one, and
/// the key with other arguments is refused.
#[tokio::test]
async fn a_keyed_project_change_is_made_once() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let key = IdempotencyKey::new("make-it-once").unwrap();
    let verb = |title: &str| Verb::TaskCreate {
        project: project(),
        spec: Box::new(TaskSpec { title: title.to_owned(), ..TaskSpec::default() }),
    };
    let first = hub.dispatch_keyed(Some(key.clone()), verb("A")).await;
    let second = hub.dispatch_keyed(Some(key.clone()), verb("A")).await;
    assert_eq!(first, second);
    assert_eq!(status(&hub).await.tasks.len(), 1);
    let other = hub.dispatch_keyed(Some(key), verb("B")).await;
    refused(&other, ErrorCode::Invalid);
}

/// A key outlives a restart of the server: a change sent again under it after one is
/// answered as the first was and made once.
#[tokio::test]
async fn a_keyed_change_sent_again_after_a_restart_is_made_once() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let key = IdempotencyKey::new("make-it-once").unwrap();
    let verb = Verb::TaskCreate {
        project: project(),
        spec: Box::new(TaskSpec { title: "A".to_owned(), ..TaskSpec::default() }),
    };
    let first = hub.dispatch_keyed(Some(key.clone()), verb.clone()).await;
    let (file, known) = (hub.projects_file(0), hub.directory());
    drop(hub);

    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let again = hub.dispatch_keyed(Some(key), verb).await;
    assert_eq!(again, first, "answered as the first time");
    assert_eq!(status(&hub).await.tasks.len(), 1, "and made once");
}

/// A status read from the project's cursor waits for the next change, and one with news
/// answers at once.
#[tokio::test]
async fn a_status_read_waits_for_the_project_s_next_change() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let next = status(&hub).await.next;
    let waiting = spawn(
        &hub,
        Verb::ProjectStatus { project: project(), since: Some(next), timeout_ms: 20_000 },
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!waiting.is_finished(), "nothing new yet");
    new_task(&hub, None).await;
    let Outcome::Project(news) =
        tokio::time::timeout(Duration::from_secs(10), waiting).await.unwrap().unwrap()
    else {
        panic!("status")
    };
    assert_eq!(news.timeline.first().map(|e| e.seq), Some(next));
    let missing = ProjectId::new("nope").unwrap();
    let unknown =
        hub.dispatch(Verb::ProjectStatus { project: missing, since: None, timeout_ms: 0 }).await;
    refused(&unknown, ErrorCode::UnknownProject);
}

/// A connecting client is told every project with the fleet, and the last event the snapshot
/// holds: a pushed change at or below it is in the snapshot already.
#[tokio::test]
async fn a_client_is_told_the_projects_with_the_fleet_as_of_one_event() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let mut pushed = hub.subscribe();
    create(&hub, None).await;
    let mut last = 0;
    while let Ok(msg) = pushed.try_recv() {
        if let FromServer::Event(e) = msg {
            last = e.seq;
        }
    }
    let snapshot = hub.state();
    assert_eq!(snapshot.seq, last, "as of the last event logged");
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects.first().map(|p| p.project.id.clone()), Some(project()));
    assert!(snapshot.projects.iter().all(|p| !p.timeline.is_empty()), "with its latest entries");
    new_task(&hub, None).await;
    let Ok(FromServer::Event(after)) = pushed.try_recv() else { panic!("a change is pushed") };
    assert!(after.seq > snapshot.seq, "a change after the snapshot is past its seq");
}

/// A terminal the worker says it opened, with or without an agent in it.
pub(super) fn announce(lease: &Lease, session: SessionId, with_agent: bool) {
    lease.handle(ToServer::SessionChanged(summary(session)));
    if with_agent {
        lease.handle(agent(session, &AgentStatus::Working));
    }
}

/// A start whose answer was lost may still have opened: it counts on, a second start of its
/// task is refused, and the terminal the worker then announces under the id the server chose
/// is put on the task with its conversation. No caller chooses that id.
#[tokio::test]
async fn a_start_whose_answer_was_lost_is_put_on_its_task_when_its_terminal_shows() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    let (_, session) = chosen(&start.1);
    let Verb::SpawnAgent { args, permission_flags, .. } = &start.1 else { panic!() };
    assert!(!permission_flags);
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_eq!(words.get(..2), Some(&["--permission-mode", "default"][..]), "{words:?}");
    let conversation = words.get(3).copied().map(str::to_owned);
    assert_eq!(words.get(2), Some(&"--session-id"));
    assert!(
        words.iter().any(|w| w.starts_with("--append-system-prompt=You are the agent of task 1"))
    );

    let lost = Outcome::Error { code: ErrorCode::Interrupted, message: "lost".to_owned() };
    lease.handle(ToServer::Reply { id: start.0, outcome: lost });
    refused(&asked.await.unwrap(), ErrorCode::Interrupted);
    let again = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude() });
    assert!(refused(&again.await, ErrorCode::Conflict).contains("being started"));
    assert_eq!(status(&hub).await.live.project, 1, "it counts meanwhile");

    announce(&lease, session, true);
    let on = task_now(&hub, task).await.assignment.expect("put on its task");
    assert_eq!(on.term, TermRef { worker: linux, session });
    assert_eq!(on.conversation, conversation);

    let chosen_by_caller = SessionId::new();
    let asked = spawn(
        &hub,
        Verb::OpenTerminal {
            worker: linux,
            cwd: None,
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: Some(chosen_by_caller),
            worktree: None,
        },
    );
    let open = request(&mut rx).await;
    assert_ne!(chosen(&open.1).1, chosen_by_caller, "the server's id, never the caller's");
    asked.abort();
}

/// A permission is the person's: an agent answering one is refused, as is an agent merging a
/// task, recording its verifier or naming a verifier at all. The CLI in a terminal speaks for an
/// agent when an agent runs there, when it works on a project, when the server does not know it,
/// and when an agent opened it or typed into it; in the person's own shell it speaks for the
/// person.
#[tokio::test]
async fn an_agent_never_takes_the_person_s_word_through_any_surface() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (plain, agent_here, typed_into) = (SessionId::new(), SessionId::new(), SessionId::new());
    let sessions = vec![summary(plain), summary(typed_into)];
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, sessions);
    announce(&lease, agent_here, true);
    create(&hub, Some(TermRef { worker, session: agent_here })).await;
    let task = new_task(&hub, None).await;
    let term = TermRef { worker, session: plain };
    let orchestrating = Speaker::Proven(agent_here);
    let answer = Verb::AnswerRequest {
        of: ThreadOf::Term(term),
        ask: slopty_proto::thread::AskId("1".to_owned()),
        choice: "allow".to_owned(),
        message: None,
    };
    let said = hub.dispatch_as(Speaker::Agent, None, answer.clone()).await;
    assert!(refused(&said, ErrorCode::Forbidden).contains("the person's"));
    let merge = || Verb::TaskUpdate {
        project: project(),
        task,
        change: Box::new(TaskChange { state: Some(TaskState::Merged), ..TaskChange::default() }),
    };
    let done = Box::new(TaskChange { state: Some(TaskState::Done), ..TaskChange::default() });
    let as_agent = hub.dispatch_as(
        orchestrating,
        None,
        Verb::TaskUpdate { project: project(), task, change: done },
    );
    assert!(matches!(as_agent.await, Outcome::Task(_)), "an agent says it is done");
    let verified = Box::new(TaskChange {
        verified: Some(slopty_proto::project::VerifierRun {
            passed: true,
            summary: "ok".to_owned(),
            head: "a".repeat(40),
            base: "b".repeat(40),
            exit: Some(0),
            took_ms: 0,
        }),
        ..TaskChange::default()
    });
    let record = Verb::TaskUpdate { project: project(), task, change: verified };
    refused(&hub.dispatch_as(orchestrating, None, record).await, ErrorCode::Forbidden);
    let weaker = || Some("true".to_owned());
    let own_verifier = Box::new(TaskChange { verifier: weaker(), ..TaskChange::default() });
    let spec =
        Box::new(TaskSpec { title: "x".to_owned(), verifier: weaker(), ..TaskSpec::default() });
    for verb in [
        Verb::TaskUpdate { project: project(), task, change: own_verifier },
        Verb::TaskCreate { project: project(), spec },
        Verb::ProjectSet {
            project: project(),
            orchestrator: None,
            verifier: weaker(),
            push: None,
            limits: LimitsChange::default(),
            metadata: None,
            autonomy: None,
        },
    ] {
        let said = hub.dispatch_as(orchestrating, None, verb).await;
        assert!(refused(&said, ErrorCode::Forbidden).contains("verifier"), "{said:?}");
    }

    let input = Verb::SendInput {
        term: TermRef { worker, session: typed_into },
        input: slopty_proto::orchestration::Input::Text("ls\n".to_owned()),
    };
    let typed = spawn_as(&hub, Speaker::Agent, input);
    let _typed = request(&mut rx).await;
    typed.abort();
    for (who, why) in [
        (Speaker::Agent, "an MCP surface"),
        (Speaker::Shell(agent_here), "an agent runs there"),
        (Speaker::Shell(SessionId::new()), "a terminal the server does not know"),
        (Speaker::Shell(typed_into), "an agent typed into it"),
    ] {
        for verb in [merge(), answer.clone()] {
            let said = hub.dispatch_as(who, None, verb).await;
            assert!(
                matches!(said, Outcome::Error { code: ErrorCode::Forbidden, .. }),
                "{why}: {said:?}"
            );
        }
    }
    let merged = hub.dispatch_as(Speaker::Shell(plain), None, merge()).await;
    assert!(matches!(merged, Outcome::Task(_)), "the person's own shell: {merged:?}");
}

/// A task's machine that stops answering mid-task, and stays away, is said on the task's
/// timeline and to its orchestrator, and its agent no longer holds a place against the
/// person's bound at once; back, the task and its orchestrator hear so, once.
#[tokio::test(start_paused = true)]
async fn a_task_s_machine_going_away_is_said_and_frees_its_place() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (orchestrator, working) = (SessionId::new(), SessionId::new());
    let (studio, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    let (mini, mini_lease, _mini_rx) = worker_on(&hub, "mini", Os::MacOs, Vec::new());
    announce(&mini_lease, working, true);
    hub.set_policy(Policy {
        bounds: Bounds { live_agents: 2, ..Bounds::default() },
        ..Policy::default()
    });
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let task = new_task(&hub, None).await;
    let assigned =
        hub.assign_for_test(&project(), task, TermRef { worker: mini, session: working });
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    let next_batch = async |rx: &mut mpsc::Receiver<FromServer>| loop {
        match tokio::time::timeout(Duration::from_mins(10), rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, reports })) => {
                return (session, batch, reports.text());
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    };
    let (session, batch, _role) = next_batch(&mut rx).await;
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    let plain = || Verb::SpawnAgent {
        worker: studio,
        cwd: "~".to_owned(),
        prompt: None,
        args: Vec::new(),
        env: Vec::new(),
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    refused(&hub.dispatch(plain()).await, ErrorCode::Limit);

    drop(mini_lease);
    let (session, batch, said) = next_batch(&mut rx).await;
    assert!(said.contains("task 1's machine mini stopped answering"), "{said}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    let notes = async || {
        let timeline = status(&hub).await.timeline;
        timeline
            .into_iter()
            .filter_map(|e| match e.what {
                Moment::Note { text } if e.task == Some(task) => Some(text),
                _ => None,
            })
            .collect::<Vec<String>>()
    };
    let away = notes().await;
    assert!(away.iter().any(|n| n.starts_with("Its machine mini stopped answering")), "{away:?}");
    assert!(task_now(&hub, task).await.spent.since_ms.is_none(), "its clock stopped");
    let asked = spawn(&hub, plain());
    let (_, verb) = request(&mut rx).await;
    assert!(matches!(verb, Verb::SpawnAgent { .. }), "the place it held is free: {verb:?}");
    asked.abort();

    let (_, _mini_back, _rx_back) =
        worker_again(&hub, mini, "mini", Os::MacOs, vec![summary(working)]);
    let (_, _, said) = next_batch(&mut rx).await;
    assert!(said.contains("task 1's machine mini answers again"), "{said}");
    let back = notes().await;
    assert_eq!(back.iter().filter(|n| n.contains("answers again")).count(), 1, "{back:?}");
    delivering.abort();
}

/// A worker that restarts, as every update does, is back within the grace: its tasks and
/// their orchestrator hear nothing of it, and no task is told to start again elsewhere.
#[tokio::test(start_paused = true)]
async fn a_worker_back_within_the_grace_tells_nobody() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (orchestrator, working) = (SessionId::new(), SessionId::new());
    let (studio, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    let (mini, mini_lease, _mini_rx) = worker_on(&hub, "mini", Os::MacOs, Vec::new());
    announce(&mini_lease, working, true);
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let task = new_task(&hub, None).await;
    let assigned =
        hub.assign_for_test(&project(), task, TermRef { worker: mini, session: working });
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    let next_batch = async |rx: &mut mpsc::Receiver<FromServer>, within: Duration| loop {
        match tokio::time::timeout(within, rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, reports })) => {
                return Some((session, batch, reports.text()));
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    };
    let (session, batch, _role) = next_batch(&mut rx, Duration::from_mins(10)).await.unwrap();
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));

    let clock = task_now(&hub, task).await.spent;
    drop(mini_lease);
    tokio::time::sleep(GONE_AFTER.checked_sub(Duration::from_secs(5)).unwrap()).await;
    let (_, _mini_back, _rx_back) =
        worker_again(&hub, mini, "mini", Os::MacOs, vec![summary(working)]);
    let quiet = next_batch(&mut rx, Duration::from_mins(5)).await;
    assert_eq!(quiet, None, "nothing said of a restart");
    let timeline = status(&hub).await.timeline;
    assert!(
        !timeline
            .iter()
            .any(|e| matches!(&e.what, Moment::Note { text } if text.contains("machine"))),
        "{timeline:?}"
    );
    assert_eq!(task_now(&hub, task).await.spent, clock, "its clock as it was");
    delivering.abort();
}

/// The person pinning a task to a machine, or letting it run anywhere, is told to its
/// orchestrator, naming the machine; an agent's own pin, and a change that leaves the pin, are
/// not.
#[tokio::test(start_paused = true)]
async fn the_person_s_pin_is_told_to_the_orchestrator() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let orchestrator = SessionId::new();
    let (studio, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    let (mini, _mini_lease, _mini_rx) = worker_on(&hub, "mini", Os::MacOs, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let task = new_task(&hub, None).await;
    let next_batch = async |rx: &mut mpsc::Receiver<FromServer>| loop {
        match tokio::time::timeout(Duration::from_mins(10), rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, reports })) => {
                return (session, batch, reports.text());
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    };
    let (session, batch, _role) = next_batch(&mut rx).await;
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    let run_on = |run_on| Verb::TaskUpdate {
        project: project(),
        task,
        change: Box::new(TaskChange { run_on: Some(run_on), ..TaskChange::default() }),
    };
    let noted = Verb::TaskUpdate {
        project: project(),
        task,
        change: Box::new(TaskChange { note: Some("soon".to_owned()), ..TaskChange::default() }),
    };
    assert!(matches!(hub.dispatch(noted).await, Outcome::Task(_)));
    assert!(matches!(hub.dispatch(run_on(RunOn::Worker(mini))).await, Outcome::Task(_)));
    let (session, batch, said) = next_batch(&mut rx).await;
    assert!(said.contains("the person pinned task 1 (Server) to the machine mini"), "{said}");
    assert!(!said.contains("soon"), "a note is no pin: {said}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    assert!(matches!(hub.dispatch(run_on(RunOn::Worker(mini))).await, Outcome::Task(_)));
    assert!(matches!(hub.dispatch(run_on(RunOn::Anywhere)).await, Outcome::Task(_)));
    let (_, _, said) = next_batch(&mut rx).await;
    assert!(said.contains("let task 1 (Server) run on any machine"), "{said}");
    assert!(!said.contains("pinned"), "the same pin again is not told: {said}");
    delivering.abort();
}

/// A machine's settings are the person's: an agent reading or editing a worker's or the
/// server's is refused by the server, through MCP and through the CLI in an agent's terminal
/// alike, and nothing reaches the worker; the person's edit goes on to it.
#[tokio::test]
async fn settings_are_never_an_agent_s() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let agent_here = SessionId::new();
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, agent_here, true);
    let edit = || slopty_proto::settings::SettingEdit {
        table: "worker".to_owned(),
        key: "allow".to_owned(),
        entry: None,
        literal: Some(r#"["0.0.0.0/0"]"#.to_owned()),
    };
    for (who, why) in [
        (Speaker::Agent, "an MCP surface"),
        (Speaker::Shell(agent_here), "the CLI where an agent runs"),
        (Speaker::Shell(SessionId::new()), "a terminal the server does not know"),
    ] {
        for verb in [
            Verb::Settings { of: Some(worker), edits: vec![edit()] },
            Verb::Settings { of: Some(worker), edits: Vec::new() },
            Verb::Settings { of: None, edits: vec![edit()] },
        ] {
            let said = hub.dispatch_as(who, None, verb).await;
            assert!(refused(&said, ErrorCode::Forbidden).contains("never an agent's"), "{why}");
        }
    }
    assert!(rx.try_recv().is_err(), "nothing reached the worker");
    let person =
        spawn_as(&hub, Speaker::Person, Verb::Settings { of: Some(worker), edits: vec![edit()] });
    let (_, asked) = request(&mut rx).await;
    assert!(matches!(asked, Verb::Settings { of: Some(w), .. } if w == worker));
    person.abort();
}

/// The person reads and edits the server's own `[server]` and `[network]` from another device:
/// the edit lands in the file the server follows and the file comes back. An edit outside them,
/// or of a value the key does not take, is refused and writes nothing; a server given no file says
/// so.
#[tokio::test]
async fn the_person_edits_the_server_s_own_settings() {
    use slopty_proto::settings::SettingEdit;
    let edit = |table: &str, key: &str, literal: &str| SettingEdit {
        table: table.to_owned(),
        key: key.to_owned(),
        entry: None,
        literal: Some(literal.to_owned()),
    };
    let allow = || edit("network", "allow", r#"["10.8.0.0/24"]"#);
    let hub = Hub::new("server".to_owned(), Vec::new());
    let said =
        hub.dispatch_as(Speaker::Person, None, Verb::Settings { of: None, edits: vec![allow()] });
    assert!(refused(&said.await, ErrorCode::Unsupported).contains("no settings file"));

    let dir = tempfile::tempdir().expect("temp");
    let path = dir.path().join("settings.toml");
    hub.set_settings_file(path.clone());
    for wrong in [edit("worker", "keep_awake", r#""never""#), edit("network", "allow", "8")] {
        let verb = Verb::Settings { of: None, edits: vec![allow(), wrong] };
        let said = hub.dispatch_as(Speaker::Person, None, verb).await;
        refused(&said, ErrorCode::Invalid);
        assert!(!path.exists(), "nothing written");
    }
    let verb = Verb::Settings { of: None, edits: vec![allow()] };
    let Outcome::Settings(read) = hub.dispatch_as(Speaker::Person, None, verb).await else {
        panic!("the settings are answered")
    };
    assert_eq!(
        (read.path.as_str(), read.tables.as_slice()),
        (&*path.to_string_lossy(), &["server".to_owned(), "network".to_owned()][..])
    );
    assert_eq!(std::fs::read_to_string(&path).expect("written"), read.text);
    assert!(read.text.contains("[network]") && read.text.contains("10.8.0.0/24"), "{}", read.text);
    let Outcome::Settings(again) = hub
        .dispatch_as(Speaker::Person, None, Verb::Settings { of: None, edits: Vec::new() })
        .await
    else {
        panic!("a read is answered")
    };
    assert_eq!(again.text, read.text);
}

fn spawn_as(hub: &Hub, who: Speaker, verb: Verb) -> tokio::task::JoinHandle<Outcome> {
    let hub = hub.clone();
    tokio::spawn(async move { hub.dispatch_as(who, None, verb).await })
}

/// An agent the server started, or one in a terminal an agent opened, that says it runs looser
/// than a mode that asks got there from a settings file or keys typed into its TUI, which an
/// agent may type: its terminal is closed and the timeline says why. The person's own terminal
/// is theirs, and a project the person allows looser modes keeps them.
#[tokio::test]
async fn an_agent_looser_than_allowed_is_closed() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let own = SessionId::new();
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, vec![summary(own)]);
    create(&hub, None).await;
    let mut started = Vec::new();
    for _ in 0..2 {
        let task = new_task(&hub, None).await;
        let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
        let start = request(&mut rx).await;
        announce(&lease, chosen(&start.1).1, true);
        started.push(opened(&lease, &start));
        assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    }
    let mode = |term: TermRef, mode: &str| {
        ToServer::Report(AgentReport::PermissionMode {
            session: term.session,
            mode: mode.to_owned(),
        })
    };
    lease.handle(mode(started[1], "default"));
    lease.handle(mode(started[1], "plan"));
    lease.handle(mode(TermRef { worker: linux, session: own }, "bypassPermissions"));
    assert!(rx.try_recv().is_err(), "modes that ask, and the person's own terminal, stay");
    lease.handle(mode(started[1], "auto"));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term: started[1] }, "typed into its TUI later");
    lease.handle(mode(started[0], "bypassPermissions"));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term: started[0] });
    let timeline = status(&hub).await.timeline;
    assert!(
        timeline.iter().any(
            |e| matches!(&e.what, Moment::Note { text } if text.contains("bypassPermissions mode"))
        ),
        "{timeline:?}"
    );
    let auto_said = "auto mode, where Claude Code's classifier approves what the person never";
    assert!(
        timeline
            .iter()
            .any(|e| matches!(&e.what, Moment::Note { text } if text.contains(auto_said))),
        "auto is said for what it is: {timeline:?}"
    );

    let opened_by_agent = spawn_as(
        &hub,
        Speaker::Agent,
        Verb::OpenTerminal {
            worker: linux,
            cwd: None,
            command: Vec::new(),
            env: Vec::new(),
            name: None,
            size: None,
            session: None,
            worktree: None,
        },
    );
    let (_, verb) = request(&mut rx).await;
    let (_, session) = chosen(&verb);
    let asking = (ASKING_ENV.to_owned(), "1".to_owned());
    assert!(
        matches!(&verb, Verb::OpenTerminal { env, .. } if env.contains(&asking)),
        "a claude typed there is held to asking: {verb:?}"
    );
    opened_by_agent.abort();
    lease.handle(mode(TermRef { worker: linux, session }, "auto"));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term: TermRef { worker: linux, session } }, "an agent's shell");
    let auto = vec!["claude".to_owned(), "--permission-mode".to_owned(), "auto".to_owned()];
    let typed = |env: Vec<(String, String)>| Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: auto.clone(),
        env,
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let by_agent = typed(Vec::new());
    let refused_auto = hub.dispatch_as(Speaker::Agent, None, by_agent).await;
    assert!(
        refused(&refused_auto, ErrorCode::Limit).contains("classifier approves"),
        "{refused_auto:?}"
    );
    let shell = Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: Vec::new(),
        env: vec![(ASKING_ENV.to_owned(), "0".to_owned())],
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let by_person = spawn_as(&hub, Speaker::Person, shell);
    let (_, verb) = request(&mut rx).await;
    assert!(
        matches!(&verb, Verb::OpenTerminal { env, .. } if !env.iter().any(|(k, _)| k == ASKING_ENV)),
        "the person's own terminal is theirs, and no caller's value stands: {verb:?}"
    );
    by_person.abort();

    hub.set_policy(Policy { permission_flags: [project()].into(), ..Policy::default() });
    lease.handle(mode(started[1], "acceptEdits"));
    assert!(rx.try_recv().is_err(), "the person allowed looser modes for the project");
}

/// A `claude` inside a shell's line (`sh -c "cd x && claude --allowedTools Bash"`) is read as
/// the shell runs it, so its start is refused like a bare one. What the start cannot see (a
/// wrapper script, a `claude` typed later into a shell an agent opened) the worker reads off the
/// agent's own command line: that closes its terminal, as a looser mode does, and says why; the
/// person's own terminal and a clean command line stay.
#[tokio::test]
async fn an_agent_looser_by_its_command_line_is_refused_or_closed() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let own = SessionId::new();
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, vec![summary(own)]);
    create(&hub, None).await;
    let line = "cd ~/src && claude --allowedTools Bash".to_owned();
    let sh = |line: String| vec!["/bin/sh".to_owned(), "-c".to_owned(), line];
    let opening = Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: sh(line),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let by_agent = hub.dispatch_as(Speaker::Agent, None, opening).await;
    assert!(refused(&by_agent, ErrorCode::Limit).contains("--allowedTools"), "{by_agent:?}");
    assert!(rx.try_recv().is_err(), "it did not reach the worker");

    let shell = Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: Vec::new(),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let asked = spawn_as(&hub, Speaker::Agent, shell);
    let start = request(&mut rx).await;
    let term = opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Opened(_)), "a plain shell opens");

    let loosened = |term: TermRef, found: &[&str]| {
        let found = found.iter().map(|f| (*f).to_owned()).collect();
        ToServer::Report(AgentReport::Loosened { session: term.session, found })
    };
    lease.handle(loosened(term, &[]));
    lease.handle(loosened(TermRef { worker: linux, session: own }, &["--allowedTools"]));
    assert!(rx.try_recv().is_err(), "nothing loosens, and the person's own terminal stays");
    lease.handle(loosened(term, &["--allowedTools"]));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term }, "a claude typed into an agent's shell later");
}

/// A task's agent reports up its tree: a need reaches the orchestrator's terminal at once as a
/// batch its hooks hand over, the worker's word that it did lands on the timeline, and the
/// orchestrator is told its role once it is named. An agent reports only on its own task, from
/// the terminal its token proves: not with no token, not from a shell, not on another task,
/// not as the orchestrator.
#[tokio::test(start_paused = true)]
async fn a_report_reaches_the_orchestrator_through_its_worker() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let orchestrator = SessionId::new();
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    let orchestrator = TermRef { worker, session: orchestrator };
    create(&hub, Some(orchestrator)).await;
    let deliver = |rx: &mut mpsc::Receiver<FromServer>| {
        let got = rx.try_recv();
        got.ok()
    };
    let next_batch = async |rx: &mut mpsc::Receiver<FromServer>| loop {
        match tokio::time::timeout(Duration::from_mins(10), rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, reports })) => {
                return (session, batch, reports.text());
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    };
    let (session, batch, context) = next_batch(&mut rx).await;
    assert_eq!(session, orchestrator.session);
    assert!(context.contains("You orchestrate the Slopty project slopty"), "{context}");
    // It dispatches: the split goes to the person first, the goal ends with its summary, and
    // the agent choice is said once, in task_start's own description.
    for said in ["you do not code", "plan mode", "with task_tell", "end with its summary"] {
        assert!(context.contains(said), "{said}: {context}");
    }
    assert!(!context.contains("runs Claude Code"), "{context}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));

    let task = new_task(&hub, None).await;
    let other = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    let term = opened(&lease, &start);
    // The worker makes each terminal's token under the key it registered with.
    let token = SessionKey::from_bytes([7; 32]).token(term.session);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    assert!(hub.vouches(term.session, &token), "its own terminal's");
    assert!(!hub.vouches(orchestrator.session, &token), "no other's");

    let report = slopty_proto::project::Report {
        note: "The store keeps every project.".to_owned(),
        artifacts: Vec::new(),
        branch: Some("task-1".to_owned()),
        pr: None,
    };
    let report_as = |speaker, task| {
        let verb = Verb::TaskReport { project: project(), task, report: report.clone() };
        hub.dispatch_as(speaker, None, verb)
    };
    let unproven = report_as(Speaker::Agent, task).await;
    assert!(
        refused(&unproven, ErrorCode::Forbidden).contains(slopty_proto::ctl::SESSION_TOKEN_ENV)
    );
    let shell = report_as(Speaker::Shell(term.session), task).await;
    refused(&shell, ErrorCode::Forbidden);
    let another = report_as(Speaker::Proven(term.session), other).await;
    assert!(refused(&another, ErrorCode::Forbidden).contains("not task 2"), "{another:?}");
    let orchestrating = report_as(Speaker::Proven(orchestrator.session), task).await;
    assert!(refused(&orchestrating, ErrorCode::Forbidden).contains("orchestrator"));
    let said = report_as(Speaker::Proven(term.session), task).await;
    assert!(matches!(said, Outcome::Task(_)), "{said:?}");
    let (session, batch, context) = next_batch(&mut rx).await;
    assert_eq!(session, orchestrator.session);
    assert!(context.contains("task 1: done\n  The store keeps every project."), "{context}");
    assert!(context.contains("branch: task-1"), "{context}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    assert!(deliver(&mut rx).is_none_or(|m| !matches!(m, FromServer::Deliver { .. })));
    let timeline = status(&hub).await.timeline;
    let kinds: Vec<&Moment> = timeline.iter().map(|e| &e.what).collect();
    assert!(kinds.iter().any(|m| matches!(m, Moment::Reported { .. })), "{kinds:?}");
    // The orchestrator's role is standing context, not a report: when it is read follows
    // only from when the agent starts, so it is not an event on the timeline.
    let delivered: Vec<&Moment> =
        kinds.iter().copied().filter(|m| matches!(m, Moment::Delivered { .. })).collect();
    assert_eq!(delivered, [&Moment::Delivered { term: orchestrator, reports: 1 }], "{kinds:?}");
    delivering.abort();
}

/// The next batch of reports sent on `rx`, passing over everything else.
async fn next_batch(rx: &mut mpsc::Receiver<FromServer>) -> (SessionId, u64, String) {
    loop {
        match tokio::time::timeout(Duration::from_mins(10), rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, reports })) => {
                return (session, batch, reports.text());
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    }
}

/// A batch the worker's link could not take when it fell due goes again once the link has
/// room, without another report or a registration to carry it.
#[tokio::test(start_paused = true)]
async fn a_batch_a_full_link_could_not_take_goes_again() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (worker, session) = (WorkerId::new(), SessionId::new());
    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(FromServer::Directory(Vec::new())).unwrap();
    let ip = IpAddr::from([100, 64, 0, 7]);
    let lease = hub.register(registration(worker, Vec::new()), ip, tx.clone()).unwrap();
    announce(&lease, session, true);
    let orchestrator = TermRef { worker, session };
    create(&hub, Some(orchestrator)).await;
    let sent_at = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(
        matches!(rx.recv().await, Some(FromServer::Directory(_))),
        "the link was full when the role fell due"
    );
    let (to, _, context) = next_batch(&mut rx).await;
    assert_eq!(to, session);
    assert!(context.contains("You orchestrate"), "{context}");
    assert!(sent_at.elapsed() >= crate::deliver::RESEND, "{:?}", sent_at.elapsed());
    delivering.abort();
}

/// What was sent to an agent and not handed over outlives a restart of the server: the store
/// keeps it, and the worker is sent it again, under its number, when it registers with the
/// server that came back.
#[tokio::test]
async fn a_batch_not_handed_over_outlives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::DeliveryStore::in_dir(dir.path());
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let keeping = tokio::spawn(hub.keep_deliveries(store.clone()));
    let session = SessionId::new();
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, session, true);
    create(&hub, Some(TermRef { worker, session })).await;
    let (_, batch, context) = next_batch(&mut rx).await;
    let outstanding = async {
        loop {
            let kept = store.load().await.unwrap();
            let mut seen = Deliveries::default();
            seen.adopt(kept, tokio::time::Instant::now(), WallMs::now());
            if !seen.outstanding_on(worker).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), outstanding).await.expect("kept");
    let projects = hub.projects_file(0);
    delivering.abort();
    keeping.abort();
    drop((lease, rx, hub));

    let hub = Hub::new("server".to_owned(), Vec::new());
    hub.adopt_projects(projects);
    hub.adopt_deliveries(store.load().await.unwrap());
    let (_, lease, mut rx) =
        worker_again(&hub, worker, "studio", Os::MacOs, vec![summary(session)]);
    let again = next_batch(&mut rx).await;
    assert_eq!(again, (session, batch, context), "the same batch, under its number");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    let mut left = Deliveries::default();
    left.adopt(hub.deliveries_file(), tokio::time::Instant::now(), WallMs::now());
    assert_eq!(left.outstanding_on(worker), Vec::new(), "handed over");
}

async fn keyed_request(
    rx: &mut mpsc::Receiver<FromServer>,
) -> (RequestId, Option<IdempotencyKey>, Verb) {
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Some(FromServer::Request { id, key, verb })) => (id, key, verb),
        other => panic!("no request: {other:?}"),
    }
}

/// A start repeated under its key with the same arguments gets the first start's answer, and
/// nothing is placed or counted twice; one whose answer may have been lost reaches its worker as
/// the very start the first was, terminal id and all, so the worker's ledger answers it. Under
/// the same key with other arguments, or from another side, it is refused and nothing reaches
/// the worker: a key never hands one caller another's start.
#[tokio::test]
async fn a_start_repeated_under_its_key_is_the_first_start() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let terminal = |cwd: &str| Verb::OpenTerminal {
        worker,
        cwd: Some(cwd.to_owned()),
        command: Vec::new(),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let agent = |prompt: &str| Verb::SpawnAgent {
        worker,
        cwd: "~".to_owned(),
        prompt: Some(prompt.to_owned()),
        args: Vec::new(),
        env: Vec::new(),
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    for (key, first, other) in
        [("open-1", terminal("~/a"), terminal("~/b")), ("spawn-1", agent("go"), agent("stop"))]
    {
        let key = IdempotencyKey::new(key).unwrap();
        let keyed = |verb: Verb| {
            let (hub, key) = (hub.clone(), key.clone());
            tokio::spawn(async move { hub.dispatch_keyed(Some(key), verb).await })
        };
        let asked = keyed(first.clone());
        let (id, sent_key, forwarded) = keyed_request(&mut rx).await;
        assert_eq!(sent_key.as_ref(), Some(&key));
        let term = opened(&lease, &(id, forwarded.clone()));
        assert_eq!(asked.await.unwrap(), Outcome::Opened(term));
        let fleet = hub.inner.state.lock().starting.len();

        let again = keyed(first.clone()).await.unwrap();
        assert_eq!(again, Outcome::Opened(term), "the first answer");
        assert!(rx.try_recv().is_err(), "answered here, not again by the worker");
        assert_eq!(hub.inner.state.lock().starting.len(), fleet, "placed once");

        let differing = keyed(other).await.unwrap();
        assert!(
            matches!(differing, Outcome::Error { code: ErrorCode::Invalid, .. }),
            "{differing:?}"
        );
        let (as_agent, key_again) = (hub.clone(), key.clone());
        let borrowed = as_agent.dispatch_as(Speaker::Agent, Some(key_again), first).await;
        assert!(
            matches!(borrowed, Outcome::Error { code: ErrorCode::Invalid, .. }),
            "another side's key: {borrowed:?}"
        );
        assert!(rx.try_recv().is_err(), "a refused repeat reaches no worker");
    }

    let key = IdempotencyKey::new("open-lost").unwrap();
    let keyed = |verb: Verb| {
        let (hub, key) = (hub.clone(), key.clone());
        tokio::spawn(async move { hub.dispatch_keyed(Some(key), verb).await })
    };
    let asked = keyed(terminal("~/c"));
    let (id, _, forwarded) = keyed_request(&mut rx).await;
    let lost = Outcome::Error { code: ErrorCode::Interrupted, message: "lost".to_owned() };
    lease.handle(ToServer::Reply { id, outcome: lost });
    refused(&asked.await.unwrap(), ErrorCode::Interrupted);
    let again = keyed(terminal("~/c"));
    let (id, _, repeated) = keyed_request(&mut rx).await;
    assert_eq!(repeated, forwarded, "a start maybe lost goes again as it was");
    let term = opened(&lease, &(id, repeated));
    assert_eq!(again.await.unwrap(), Outcome::Opened(term));
}

/// An agent starts nothing with more than it has through the environment either: a variable
/// that moves what runs there, or whom it speaks for, is refused on each way to start that takes
/// one (a task's start takes none), before anything reaches a worker. The person names what they
/// like.
#[tokio::test]
async fn an_agent_names_no_environment_that_steers_what_it_starts() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let orchestrator = SessionId::new();
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker: linux, session: orchestrator })).await;
    let with = |name: &str| vec![(name.to_owned(), "/tmp/elsewhere".to_owned())];
    let terminal = |env| Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: Vec::new(),
        env,
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let agent = |env| Verb::SpawnAgent {
        worker: linux,
        cwd: "~".to_owned(),
        prompt: None,
        args: Vec::new(),
        env,
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    for (verb, name) in [
        (terminal(with("PATH")), "PATH"),
        (terminal(with("zdotdir")), "zdotdir"),
        (terminal(with("DYLD_INSERT_LIBRARIES")), "DYLD_INSERT_LIBRARIES"),
        (agent(with("CLAUDE_CONFIG_DIR")), "CLAUDE_CONFIG_DIR"),
        (agent(with("NODE_OPTIONS")), "NODE_OPTIONS"),
        (agent(with("SLOPTY_TASK")), "SLOPTY_TASK"),
    ] {
        let said = hub.dispatch_as(Speaker::Proven(orchestrator), None, verb).await;
        assert!(refused(&said, ErrorCode::Limit).contains(name), "{name}: {said:?}");
    }
    assert!(rx.try_recv().is_err(), "nothing reached the worker");
    let person = spawn(&hub, terminal(with("PATH")));
    let (_, verb) = request(&mut rx).await;
    assert!(matches!(verb, Verb::OpenTerminal { env, .. } if env == with("PATH")));
    person.abort();
}

/// An agent reaches another through reports, never through its TUI's keys, where `!`, a slash
/// command or a mode switch would act as the person: typing into a terminal where an agent
/// runs is refused, typing into a shell goes.
#[tokio::test]
async fn an_agent_never_types_into_another_agent_s_tui() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (shell, tui) = (SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, vec![summary(shell)]);
    announce(&lease, tui, true);
    let input = |session| Verb::SendInput {
        term: TermRef { worker, session },
        input: slopty_proto::orchestration::Input::Text("/permissions\n".to_owned()),
    };
    let said = hub.dispatch_as(Speaker::Agent, None, input(tui)).await;
    assert!(refused(&said, ErrorCode::Forbidden).contains("task_report"), "{said:?}");
    assert!(rx.try_recv().is_err(), "no key reached the TUI");
    let typed = spawn_as(&hub, Speaker::Agent, input(shell));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, input(shell), "a shell takes an agent's keys");
    typed.abort();
}

/// A terminal an agent opens may run an agent, so it takes a place under the fleet bound from
/// the moment it is asked for, and a second past the bound is refused before it reaches a
/// worker. The person's own terminals are theirs and are not counted.
#[tokio::test]
async fn a_terminal_an_agent_opens_counts_against_the_fleet_bound() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, _lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    hub.set_policy(Policy {
        bounds: Bounds { live_agents: 1, ..Bounds::default() },
        ..Policy::default()
    });
    let shell = || Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: Vec::new(),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let first = spawn_as(&hub, Speaker::Agent, shell());
    let _first_start = request(&mut rx).await;
    let second = hub.dispatch_as(Speaker::Agent, None, shell()).await;
    assert!(refused(&second, ErrorCode::Limit).contains("live_agents"), "{second:?}");
    assert!(rx.try_recv().is_err(), "the second reached no worker");
    let person = spawn(&hub, shell());
    let _person_start = request(&mut rx).await;
    person.abort();
    first.abort();
}

/// A shell opened by the agent speaking from `by`, as the worker opens it.
async fn opened_by(
    hub: &Hub,
    by: SessionId,
    worker: WorkerId,
    lease: &Lease,
    rx: &mut mpsc::Receiver<FromServer>,
) -> TermRef {
    let shell = Verb::OpenTerminal {
        worker,
        cwd: None,
        command: Vec::new(),
        env: Vec::new(),
        name: None,
        size: None,
        session: None,
        worktree: None,
    };
    let asked = spawn_as(hub, Speaker::Proven(by), shell);
    let start = request(rx).await;
    let term = opened(lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Opened(_)));
    term
}

/// The project's orchestrator moved to `term`.
fn orchestrated_by(term: TermRef) -> Verb {
    Verb::ProjectSet {
        project: project(),
        orchestrator: Some(term),
        verifier: None,
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
        autonomy: None,
    }
}

/// An agent puts to work only the terminals its project holds or it opened: never the person's
/// own shell as the orchestrator, and nothing at all unproven. A shell the orchestrator opened
/// is its to give.
#[tokio::test]
async fn an_agent_puts_to_work_only_terminals_its_project_holds() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (person, orchestrator) = (SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, vec![summary(person)]);
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker, session: orchestrator })).await;
    let persons = TermRef { worker, session: person };
    let as_orchestrator = Speaker::Proven(orchestrator);

    let taken = hub.dispatch_as(as_orchestrator, None, orchestrated_by(persons)).await;
    assert!(refused(&taken, ErrorCode::Forbidden).contains("person's own"), "{taken:?}");
    let unproven = hub.dispatch_as(Speaker::Agent, None, orchestrated_by(persons)).await;
    refused(&unproven, ErrorCode::Forbidden);
    let shell = opened_by(&hub, orchestrator, worker, &lease, &mut rx).await;
    let given = hub.dispatch_as(as_orchestrator, None, orchestrated_by(shell)).await;
    assert!(matches!(given, Outcome::Project(_)), "{given:?}");
}

/// While as many tasks wait on the person as the project's review limit, the orchestrator's
/// start is refused in words that say why, and nothing reaches a worker; the person's own start
/// goes. Only the person sets the limit.
#[tokio::test]
async fn an_agent_starts_no_more_work_than_the_person_can_review() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker: linux, session: orchestrator })).await;
    let as_orchestrator = Speaker::Proven(orchestrator);
    let limit = |review| Verb::ProjectSet {
        project: project(),
        orchestrator: None,
        verifier: None,
        push: None,
        limits: LimitsChange { review },
        metadata: None,
        autonomy: None,
    };
    let theirs = hub.dispatch_as(as_orchestrator, None, limit(Some(5))).await;
    assert!(refused(&theirs, ErrorCode::Forbidden).contains("review limit"), "{theirs:?}");
    assert!(matches!(hub.dispatch(limit(Some(1))).await, Outcome::Project(_)));
    let ready = new_task(&hub, None).await;
    let done = Box::new(TaskChange { state: Some(TaskState::Done), ..TaskChange::default() });
    let update = Verb::TaskUpdate { project: project(), task: ready, change: done };
    assert!(matches!(hub.dispatch(update).await, Outcome::Task(_)));

    let next = new_task(&hub, None).await;
    let launch = claude();
    let start = || Verb::TaskSpawn { project: project(), task: next, launch: launch.clone() };
    let held = hub.dispatch_as(as_orchestrator, None, start()).await;
    let said = refused(&held, ErrorCode::Limit);
    assert!(said.contains("task 1 ready to merge") && said.contains("review limit of 1"), "{said}");
    assert!(rx.try_recv().is_err(), "nothing reached the worker");

    let by_person = spawn(&hub, start());
    let _start = request(&mut rx).await;
    by_person.abort();
}

/// The person's allowance of looser permissions is a project's, for its own agents: another
/// project's orchestrator, or an unproven agent, starts nothing loose; the project's own
/// orchestrator does.
#[tokio::test]
async fn a_project_s_looser_permissions_are_its_own_agents_only() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (ours, theirs) = (SessionId::new(), SessionId::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    announce(&lease, ours, true);
    announce(&lease, theirs, true);
    create(&hub, Some(TermRef { worker: linux, session: ours })).await;
    let elsewhere = ProjectId::new("elsewhere").unwrap();
    let made = hub
        .dispatch(Verb::ProjectCreate {
            project: elsewhere,
            title: "Elsewhere".to_owned(),
            repo: "~/src/elsewhere".to_owned(),
            target: "main".to_owned(),
            verifier: None,
            push: false,
            orchestrator: Some(TermRef { worker: linux, session: theirs }),
            limits: LimitsChange::default(),
            metadata: None,
            goal: None,
            autonomy: slopty_proto::project::Autonomy::Ask,
        })
        .await;
    assert!(matches!(made, Outcome::Project(_)), "{made:?}");
    hub.set_policy(Policy { permission_flags: [project()].into(), ..Policy::default() });
    let loose = Verb::SpawnAgent {
        worker: linux,
        cwd: "~".to_owned(),
        prompt: None,
        args: vec!["--allowedTools".to_owned(), "Bash".to_owned()],
        env: Vec::new(),
        size: None,
        session: None,
        permission_flags: false,
        worktree: None,
    };
    for who in [Speaker::Proven(theirs), Speaker::Agent] {
        let said = hub.dispatch_as(who, None, loose.clone()).await;
        assert!(refused(&said, ErrorCode::Limit).contains("--allowedTools"), "{said:?}");
    }
    let task = new_task(&hub, None).await;
    let spawned = Verb::TaskSpawn { project: project(), task, launch: claude() };
    let said = hub.dispatch_as(Speaker::Proven(theirs), None, spawned).await;
    assert!(refused(&said, ErrorCode::Forbidden).contains("elsewhere"), "{said:?}");
    assert!(rx.try_recv().is_err(), "nothing reached the worker");
    let started = spawn_as(&hub, Speaker::Proven(ours), loose);
    let (_, verb) = request(&mut rx).await;
    assert!(matches!(verb, Verb::SpawnAgent { permission_flags: true, .. }), "{verb:?}");
    started.abort();
}

/// The terminals an agent opened stay its project's across a server restart: the store keeps
/// them, so the shell the orchestrator opened is still its to put to work, and the CLI in it
/// still speaks for an agent.
#[tokio::test]
async fn the_terminals_an_agent_opened_are_kept_across_a_restart() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let mut kept = hub.keep_projects();
    let orchestrator = SessionId::new();
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker, session: orchestrator })).await;
    let task = new_task(&hub, None).await;
    let shell = opened_by(&hub, orchestrator, worker, &lease, &mut rx).await;
    let mut watched = Vec::new();
    while let Ok(keep) = kept.try_recv() {
        if let Keep::Watch(w) = keep {
            watched.push(w);
        }
    }
    let opened = Some(Drove::Opened { by: Some(orchestrator) });
    assert!(watched.iter().any(|w| w.term == shell && w.drove == opened), "{watched:?}");
    let (file, known) = (hub.projects_file(0), hub.directory());
    assert!(file.watched.iter().any(|w| w.term == shell && w.drove == opened), "{file:?}");
    drop(lease);
    drop(hub);

    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let sessions = vec![summary(orchestrator), summary(shell.session)];
    let (_, lease, _rx) = worker_again(&hub, worker, "studio", Os::MacOs, sessions);
    announce(&lease, orchestrator, true);
    let merge = Verb::TaskUpdate {
        project: project(),
        task,
        change: Box::new(TaskChange { state: Some(TaskState::Merged), ..TaskChange::default() }),
    };
    let said = hub.dispatch_as(Speaker::Shell(shell.session), None, merge).await;
    refused(&said, ErrorCode::Forbidden);
    let given = hub.dispatch_as(Speaker::Proven(orchestrator), None, orchestrated_by(shell)).await;
    assert!(matches!(given, Outcome::Project(_)), "{given:?}");
}

/// Tasks are one level: a task's agent makes, starts and tells no task, changes only its own,
/// and makes no project.
#[tokio::test]
async fn a_task_s_agent_works_only_on_its_own_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let beside = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let agent = opened(&lease, &request(&mut rx).await);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let as_agent = Speaker::Proven(agent.session);
    let spec =
        TaskSpec { title: "Part".to_owned(), brief: "A part.".to_owned(), ..TaskSpec::default() };
    let tell = Verb::TaskTell { project: project(), task: Some(beside), text: "Go.".to_owned() };
    for verb in [
        Verb::TaskCreate { project: project(), spec: Box::new(spec) },
        Verb::TaskSpawn { project: project(), task: beside, launch: claude() },
        tell,
    ] {
        let said = hub.dispatch_as(as_agent, None, verb).await;
        assert!(refused(&said, ErrorCode::Forbidden).contains("only the project's orchestrator"));
    }
    assert!(rx.try_recv().is_err(), "nothing reached the worker");
    let done = Box::new(TaskChange { state: Some(TaskState::Done), ..TaskChange::default() });
    let change = Verb::TaskUpdate { project: project(), task: beside, change: done };
    refused(&hub.dispatch_as(as_agent, None, change).await, ErrorCode::Forbidden);
    let said = TaskChange { status: Some("reading the store".to_owned()), ..TaskChange::default() };
    let own = Verb::TaskUpdate { project: project(), task, change: Box::new(said) };
    let Outcome::Task(changed) = hub.dispatch_as(as_agent, None, own).await else { panic!("own") };
    assert_eq!(changed.status.as_deref(), Some("reading the store"));
    let made = hub
        .dispatch_as(
            as_agent,
            None,
            Verb::ProjectCreate {
                project: ProjectId::new("mine").unwrap(),
                title: "Mine".to_owned(),
                repo: "~/src/mine".to_owned(),
                target: "main".to_owned(),
                verifier: None,
                push: false,
                orchestrator: None,
                limits: LimitsChange::default(),
                metadata: None,
                goal: None,
                autonomy: slopty_proto::project::Autonomy::Ask,
            },
        )
        .await;
    refused(&made, ErrorCode::Forbidden);
}

/// The orchestrator is told which repository it works in, by the key every clone shares, and
/// where each worker has one: a clone on another machine under another path is the same
/// repository.
#[tokio::test]
async fn the_orchestrator_is_told_where_its_repository_is_cloned() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let in_clone = |session: SessionId, path: &str, origin: Option<&str>| SessionSummary {
        repo: Some(path.to_owned()),
        repo_id: Some(slopty_proto::terminal::RepoId {
            origin: origin.map(str::to_owned),
            root: Some("c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e".to_owned()),
            url: None,
        }),
        ..summary(session)
    };
    let orchestrator = SessionId::new();
    let origin = Some("github.com/aislopware/slopty");
    let (studio, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    lease.handle(ToServer::SessionChanged(in_clone(orchestrator, "/w/slopty", origin)));
    let linux = vec![in_clone(SessionId::new(), "/home/c/slopty", None)];
    let (_linux, _linux_lease, _linux_rx) = worker_on(&hub, "linux", Os::Linux, linux);
    let (_bare, _bare_lease, _bare_rx) = worker_on(&hub, "bare", Os::Linux, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;

    let context = loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(FromServer::Deliver { reports, .. })) => break reports.text(),
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    };
    assert!(
        context.contains(
            "Its repository is github.com/aislopware/slopty on every worker; clones of it are on \
             linux (/home/c/slopty), studio (/w/slopty)."
        ),
        "{context}"
    );
    assert!(context.contains("A worker's `repos` fact names where its clones are."), "{context}");
    let learned = status(&hub).await.project.repo_id;
    assert_eq!(learned.and_then(|id| id.origin).as_deref(), origin, "the project keeps it");
    delivering.abort();
}

/// A task started with no directory goes beside a clone of the project's repository and starts
/// in it: an agent that writes (Claude Code or Codex) in a git worktree of its own the worker
/// makes from the target, named for the task, one that only reads in the clone itself. Named no
/// worker, it goes to one with a clone; with none online and no address to clone from, the refusal
/// says so.
#[tokio::test]
async fn a_task_with_no_directory_goes_beside_a_clone_in_a_worktree_of_its_own() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let in_clone = |session: SessionId, path: &str, origin: Option<&str>| SessionSummary {
        repo: Some(path.to_owned()),
        repo_id: Some(slopty_proto::terminal::RepoId {
            origin: origin.map(str::to_owned),
            root: Some("c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e".to_owned()),
            url: None,
        }),
        ..summary(session)
    };
    let orchestrator = SessionId::new();
    let origin = Some("github.com/aislopware/slopty");
    let studio = vec![in_clone(orchestrator, "/w/slopty", origin)];
    let (studio, studio_lease, mut studio_rx) = worker_on(&hub, "studio", Os::MacOs, studio);
    let linux = vec![in_clone(SessionId::new(), "/home/c/slopty", None)];
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, linux);
    let (bare, _bare_lease, _bare_rx) = worker_on(&hub, "bare", Os::Linux, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let anywhere = claude();

    let writes = new_task(&hub, Some(linux)).await;
    let verb = Verb::TaskSpawn { project: project(), task: writes, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    let Verb::SpawnAgent { cwd, args, worktree, .. } = start.1.clone() else {
        panic!("{:?}", start.1)
    };
    assert_eq!(cwd, "/home/c/slopty", "the clone on the Linux worker, found by its first commit");
    let name = format!("slopty-slopty-{writes}");
    assert!(args.windows(2).any(|w| w == ["--worktree", name.as_str()]), "{args:?}");
    assert_eq!(
        worktree.map(|w| (w.name, w.base)),
        Some((name.clone(), Some("main".to_owned()))),
        "the worker makes it from the target first, for Claude Code to open"
    );
    let role = args.iter().find(|a| a.starts_with("--append-system-prompt=")).unwrap();
    assert!(role.contains(&format!("a git worktree of your own, {name}")), "{role}");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    // Codex's terminal opens in one the worker makes from the target too: its own `--worktree`
    // would start from whatever the clone has checked out.
    linux_lease.handle(ToServer::Facts(installed(&["claude", "codex"])));
    let codex_task = new_task(&hub, Some(linux)).await;
    let launch = TaskLaunch { agent: AgentId::named(AgentId::CODEX), ..anywhere.clone() };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task: codex_task, launch });
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    let Verb::OpenTerminal { cwd, command, worktree, .. } = start.1.clone() else {
        panic!("{:?}", start.1)
    };
    assert_eq!(cwd.as_deref(), Some("/home/c/slopty"));
    assert!(!command.iter().any(|a| a == "--worktree"), "not Codex's own: {command:?}");
    let name = format!("slopty-slopty-{codex_task}");
    assert_eq!(worktree.map(|w| (w.name, w.base)), Some((name.clone(), Some("main".to_owned()))));
    assert!(command[2].contains(&format!("a git worktree of your own, {name}")), "{command:?}");
    // The worker says where it made it, so the task frees it once merged: Codex names none.
    let (worker, session) = chosen(&start.1);
    linux_lease.handle(ToServer::SessionChanged(summary(session)));
    let path = format!("/home/c/slopty/.claude/worktrees/{name}");
    let worktree = Box::new(Worktree {
        name: name.clone(),
        path: path.clone(),
        branch: Some(format!("worktree-{name}")),
        original_cwd: "/home/c/slopty".to_owned(),
        original_branch: Some("main".to_owned()),
    });
    let term = TermRef { worker, session };
    linux_lease
        .handle(ToServer::Reply { id: start.0, outcome: Outcome::OpenedIn { term, worktree } });
    let Outcome::Task(task) = asked.await.unwrap() else { panic!("no task") };
    assert_eq!(task.worktree.as_deref(), Some(path.as_str()), "the task knows its worktree");

    let spec = TaskSpec {
        title: "Read the logs".to_owned(),
        read_only: true,
        pin: Some(linux),
        ..TaskSpec::default()
    };
    let Outcome::Task(reads) =
        hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await
    else {
        panic!("no task")
    };
    let verb = Verb::TaskSpawn { project: project(), task: reads.id, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
    let start = request(&mut linux_rx).await;
    let Verb::SpawnAgent { cwd, args, .. } = start.1.clone() else { panic!("{:?}", start.1) };
    assert_eq!(cwd, "/home/c/slopty");
    assert!(!args.iter().any(|a| a == "--worktree"), "one that only reads needs none: {args:?}");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    let there = new_task(&hub, None).await;
    let verb = Verb::TaskSpawn { project: project(), task: there, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
    let start = request(&mut studio_rx).await;
    let Verb::SpawnAgent { cwd, .. } = start.1.clone() else { panic!("{:?}", start.1) };
    assert_eq!(cwd, "/w/slopty", "beside a clone, on the worker running fewer agents");
    opened(&studio_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    // Pinned to a worker with no clone, and no address to clone from, it is refused rather
    // than started in that worker's home with nothing to work on.
    let homeless = new_task(&hub, Some(bare)).await;
    let verb = Verb::TaskSpawn { project: project(), task: homeless, launch: anywhere.clone() };
    let said = refused(&hub.dispatch(verb).await, ErrorCode::Unplaced).to_owned();
    assert!(said.contains("pin the task to one that has it"), "{said}");

    drop((studio_lease, linux_lease));
    let stranded = new_task(&hub, None).await;
    let verb = Verb::TaskSpawn { project: project(), task: stranded, launch: anywhere };
    let no = hub.dispatch(verb).await;
    let said = refused(&no, ErrorCode::Unplaced);
    assert!(said.contains("no address to clone it from is known"), "{said}");
}

/// A task placed on another machine than the orchestrator's starts its worktree from the target
/// as the orchestrator's clone holds it: with pushing off, what the merge queue merged is only
/// there. The target goes across first as `slopty/<p>/target`, and the worktree starts from
/// that. A target that cannot be sent refuses the start, saying why, rather than starting the
/// agent on a stale copy.
#[tokio::test]
async fn a_task_elsewhere_starts_from_the_target_the_orchestrator_s_clone_holds() {
    use slopty_proto::project::{StepKind, StepState};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let studio = vec![in_repo(orchestrator, "/w/demo", Some("https://example.com/o/demo.git"))];
    let (studio, studio_lease, mut studio_rx) = worker_on(&hub, "studio", Os::MacOs, studio);
    let linux = vec![in_repo(SessionId::new(), "/home/c/demo", None)];
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, linux);
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let launch = claude();

    let task = new_task(&hub, Some(linux)).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: launch.clone() });
    let (id, verb) = request(&mut studio_rx).await;
    let Verb::BundleBranch { repo, branch, target, .. } = verb else { panic!("{verb:?}") };
    assert_eq!(
        (repo.as_str(), branch.as_str(), target.as_deref()),
        ("/w/demo", "main", Some("main"))
    );
    let head = format!("{}9e1f0", "9e1f0c2".repeat(5));
    let name = "main-9e1f0c200000.bundle".to_owned();
    let path = format!("/Users/c/.cache/slopty/bundles/{name}");
    let made =
        BranchBundle { path, name, size: 6, digest: [5; 32], head: head.clone(), base: None };
    answer(&studio_lease, id, Outcome::Bundle(Box::new(made)));
    let (id, verb) = request(&mut studio_rx).await;
    assert!(matches!(verb, Verb::ReadFile { offset: 0, .. }), "{verb:?}");
    answer(&studio_lease, id, Outcome::File { bytes: b"bundle".to_vec(), offset: 0, size: 6 });
    for _part in 0..2 {
        let (id, verb) = request(&mut linux_rx).await;
        assert!(matches!(verb, Verb::Upload { .. }), "{verb:?}");
        answer(&linux_lease, id, Outcome::Done);
    }
    let (id, verb) = request(&mut linux_rx).await;
    let Verb::FetchBundle { repo, into, .. } = verb else { panic!("{verb:?}") };
    assert_eq!((repo.as_str(), into.as_str()), ("/home/c/demo", "slopty/slopty/target"));
    answer(&linux_lease, id, Outcome::Fetched { branch: into, head });
    let start = request(&mut linux_rx).await;
    let Verb::SpawnAgent { cwd, worktree, .. } = start.1.clone() else { panic!("{:?}", start.1) };
    assert_eq!(cwd, "/home/c/demo");
    let base = worktree.and_then(|w| w.base);
    assert_eq!(base.as_deref(), Some("slopty/slopty/target"), "from the merged target");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let step = step_of(&hub, task).await.map(|s| (s.kind, s.worker, s.state));
    let detail = "main as the orchestrator's clone has it, at 9e1f0c2".to_owned();
    assert_eq!(step, Some((StepKind::Clone, linux, StepState::Done { detail })));

    let next = new_task(&hub, Some(linux)).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task: next, launch });
    let (id, verb) = request(&mut studio_rx).await;
    assert!(matches!(verb, Verb::BundleBranch { .. }), "{verb:?}");
    let message = "fatal: bad object refs/heads/main".to_owned();
    answer(&studio_lease, id, Outcome::Error { code: ErrorCode::Failed, message });
    let said = refused(&asked.await.unwrap(), ErrorCode::Failed).to_owned();
    assert!(said.contains("could not start from its target"), "{said}");
    assert!(said.contains("bad object"), "{said}");
    let failed = step_of(&hub, next).await.map(|s| s.state);
    assert!(matches!(failed, Some(StepState::Failed { .. })), "the card says so too: {failed:?}");
    assert!(linux_rx.try_recv().is_err(), "nothing started");
}

/// A shell in the clone at `path` of the repository `origin`, cloned from `url`.
pub(super) fn in_repo(session: SessionId, path: &str, url: Option<&str>) -> SessionSummary {
    SessionSummary {
        repo: Some(path.to_owned()),
        repo_id: Some(slopty_proto::terminal::RepoId {
            origin: Some("example.com/o/demo".to_owned()),
            root: Some("c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e".to_owned()),
            url: url.map(str::to_owned),
        }),
        ..summary(session)
    }
}

/// The task's step as its card has it now.
async fn step_of(hub: &Hub, task: TaskId) -> Option<slopty_proto::project::TaskStep> {
    task_now(hub, task).await.step
}

/// The worker's answer to `verb` request `id`.
pub(super) fn answer(lease: &Lease, id: RequestId, outcome: Outcome) {
    lease.handle(ToServer::Reply { id, outcome });
}

/// The orchestrator's worker is asked for the target before a task starts in a clone on another
/// machine, and the forge has all of it: the task's worktree starts from the target there.
pub(super) async fn forge_has_the_target(lease: &Lease, rx: &mut mpsc::Receiver<FromServer>) {
    let (id, verb) = request(rx).await;
    let Verb::BundleBranch { branch, target, .. } = &verb else { panic!("{verb:?}") };
    assert_eq!((branch.as_str(), target.as_deref()), ("main", Some("main")));
    let message = "main has no commit beyond origin/main".to_owned();
    answer(lease, id, Outcome::Error { code: ErrorCode::NothingNew, message });
}

/// A task with no directory pinned to a worker with no clone of the project's repository gets
/// one there first, from the address the orchestrator's clone names: the card
/// shows how far it is as git says, the timeline its start and end, and the agent then starts
/// in it. A clone that fails is the start's refusal, and the card says why.
#[tokio::test]
async fn a_task_on_a_worker_with_no_clone_gets_one_made_and_shown() {
    use slopty_proto::project::{StepKind, StepState};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let url = "https://example.com/o/demo.git";
    let studio = vec![in_repo(orchestrator, "/w/demo", Some(url))];
    let (studio, studio_lease, mut studio_rx) = worker_on(&hub, "studio", Os::MacOs, studio);
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let anywhere = claude();

    let task = new_task(&hub, Some(linux)).await;
    let verb = Verb::TaskSpawn { project: project(), task, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
    let (id, verb) = request(&mut linux_rx).await;
    let Verb::CloneRepo { url: asked_url, clone, .. } = verb else { panic!("{verb:?}") };
    assert_eq!(asked_url, url);
    let running = |s: Option<slopty_proto::project::TaskStep>| match s.map(|s| (s.kind, s.state)) {
        Some((StepKind::Clone, StepState::Running { phase, percent })) => Some((phase, percent)),
        _ => None,
    };
    assert!(running(step_of(&hub, task).await).is_some(), "the card says it clones");
    let phase = "Receiving objects".to_owned();
    linux_lease.handle(ToServer::Cloning { clone, phase: phase.clone(), percent: Some(42) });
    assert_eq!(running(step_of(&hub, task).await), Some((phase.clone(), Some(42))));
    linux_lease.handle(ToServer::Cloning { clone, phase: phase.clone(), percent: Some(43) });
    assert_eq!(running(step_of(&hub, task).await), Some((phase, Some(42))), "in steps of five");

    let path = "/home/c/slopty/clones/example.com/o/demo".to_owned();
    let repo = in_repo(SessionId::new(), &path, Some(url)).repo_id.unwrap();
    answer(&linux_lease, id, Outcome::Cloned { path: path.clone(), repo });
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    let Verb::SpawnAgent { cwd, args, .. } = start.1.clone() else { panic!("{:?}", start.1) };
    assert_eq!(cwd, path, "the agent starts in the clone made");
    assert!(args.iter().any(|a| a == "--worktree"), "{args:?}");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let done = step_of(&hub, task).await.map(|s| s.state);
    let target = "main from its origin".to_owned();
    assert_eq!(done, Some(StepState::Done { detail: target }), "and then the target is there");
    let steps: Vec<_> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Step(s) => Some(s.state),
            _ => None,
        })
        .collect();
    let made = StepState::Done { detail: path.clone() };
    assert!(
        matches!(
            steps.as_slice(),
            [StepState::Running { .. }, d, StepState::Running { .. }, StepState::Done { .. }]
                if *d == made
        ),
        "{steps:?}"
    );

    // The next task there finds the clone: no second one.
    let next = new_task(&hub, Some(linux)).await;
    let verb = Verb::TaskSpawn { project: project(), task: next, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    assert!(matches!(&start.1, Verb::SpawnAgent { cwd, .. } if *cwd == path), "{:?}", start.1);
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    // A clone that fails: the start is refused saying why, and so is the card.
    let (other, other_lease, mut other_rx) = worker_on(&hub, "other", Os::Linux, Vec::new());
    let failing = new_task(&hub, Some(other)).await;
    let verb = Verb::TaskSpawn { project: project(), task: failing, launch: anywhere };
    let asked = spawn(&hub, verb);
    let (id, verb) = request(&mut other_rx).await;
    assert!(matches!(verb, Verb::CloneRepo { .. }), "{verb:?}");
    let denied = "fatal: Authentication failed for 'https://example.com/o/demo.git/'".to_owned();
    answer(&other_lease, id, Outcome::Error { code: ErrorCode::Failed, message: denied.clone() });
    let said = refused(&asked.await.unwrap(), ErrorCode::Failed).to_owned();
    assert!(said.contains("Authentication failed"), "{said}");
    let failed = step_of(&hub, failing).await.map(|s| (s.kind, s.state));
    assert_eq!(failed, Some((StepKind::Clone, StepState::Failed { why: denied })));
}

/// A task's agent reports done on a worker other than its orchestrator's: its branch comes
/// home as a bundle of its own commits, read in parts there and uploaded here, then fetched
/// into the orchestrator's clone as `slopty/<project>/<task>`, and the timeline says it arrived.
/// A clone that lacks the fork point gets the whole branch instead, and a done reported again
/// mid-trip sends the branch once more after it.
#[tokio::test]
async fn a_finished_task_s_branch_is_brought_to_the_orchestrator_s_clone() {
    use slopty_proto::project::{Report, StepKind, StepState, TaskStep};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let studio = vec![in_repo(orchestrator, "/w/demo", Some("https://example.com/o/demo.git"))];
    let (studio, studio_lease, mut studio_rx) = worker_on(&hub, "studio", Os::MacOs, studio);
    let linux = vec![in_repo(SessionId::new(), "/home/c/demo", None)];
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, linux);
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let task = new_task(&hub, Some(linux)).await;
    let launch = claude();
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    let term = opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    let branch = format!("worktree-slopty-slopty-{task}");
    let report = Report {
        note: "Built it.".to_owned(),
        artifacts: Vec::new(),
        branch: Some(branch.clone()),
        pr: None,
    };
    let verb = Verb::TaskReport { project: project(), task, report };
    let done = async || hub.dispatch_as(Speaker::Proven(term.session), None, verb.clone()).await;
    assert!(matches!(done().await, Outcome::Task(_)));

    let bundle = |target: Option<String>, size: u64| {
        let name = format!("{branch}-4a7aa6d00000.bundle");
        let (path, head) = (
            format!("/home/c/.cache/slopty/bundles/{name}"),
            format!("{}4a7aa", "4a7aa6d".repeat(5)),
        );
        let made = BranchBundle { path, name, size, digest: [3; 32], head, base: None };
        (target, Outcome::Bundle(Box::new(made)))
    };
    // The fork point first; the clone here lacks it; then the whole branch.
    for (want_target, lacking) in [(Some("main".to_owned()), true), (None, false)] {
        let (id, verb) = request(&mut linux_rx).await;
        let Verb::BundleBranch { repo, branch: b, target, .. } = verb else { panic!("{verb:?}") };
        assert_eq!(
            (repo.as_str(), b.as_str(), &target),
            ("/home/c/demo", branch.as_str(), &want_target)
        );
        if lacking {
            // Done again mid-trip: the trip goes once more when this one ends.
            assert!(matches!(done().await, Outcome::Task(_)));
        }
        answer(&linux_lease, id, bundle(target, 6).1);
        let (id, verb) = request(&mut linux_rx).await;
        assert!(matches!(verb, Verb::ReadFile { offset: 0, .. }), "{verb:?}");
        answer(&linux_lease, id, Outcome::File { bytes: b"bundle".to_vec(), offset: 0, size: 6 });
        let (id, verb) = request(&mut studio_rx).await;
        let Verb::Upload { path, part: UploadPart::Bytes { offset: 0, .. }, .. } = verb else {
            panic!("{verb:?}")
        };
        assert!(path.starts_with("~/.cache/slopty/bundles/"), "{path}");
        answer(&studio_lease, id, Outcome::Done);
        let (id, verb) = request(&mut studio_rx).await;
        assert!(
            matches!(verb, Verb::Upload { part: UploadPart::Finish { size: 6, .. }, .. }),
            "{verb:?}"
        );
        answer(&studio_lease, id, Outcome::Done);
        let (id, verb) = request(&mut studio_rx).await;
        let Verb::FetchBundle { repo, into, head, .. } = verb else { panic!("{verb:?}") };
        assert_eq!(repo, "/w/demo", "into the orchestrator's clone");
        assert_eq!(into, format!("slopty/slopty/{task}"), "as a branch only the server names");
        let outcome = if lacking {
            let message = "error: Repository lacks these prerequisite commits".to_owned();
            Outcome::Error { code: ErrorCode::Conflict, message }
        } else {
            Outcome::Fetched { branch: into, head }
        };
        answer(&studio_lease, id, outcome);
    }
    let (id, verb) = request(&mut linux_rx).await;
    assert!(matches!(verb, Verb::BundleBranch { .. }), "the second trip: {verb:?}");
    let arrived: Vec<_> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Step(TaskStep { kind, worker, state: StepState::Done { detail }, .. }) => {
                Some((kind, worker, detail))
            }
            _ => None,
        })
        .collect();
    let detail = format!("{branch} as slopty/slopty/{task} at 4a7aa6d in /w/demo");
    let target = (StepKind::Clone, linux, "main from its origin".to_owned());
    assert_eq!(arrived, [target, (StepKind::Home, studio, detail)], "the branch arrived");
    let nothing = format!("{branch} has no commit beyond main");
    answer(&linux_lease, id, Outcome::Error { code: ErrorCode::Failed, message: nothing.clone() });
    let failed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(TaskStep { state: StepState::Failed { why }, .. }) =
                step_of(&hub, task).await
            {
                return why;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second trip ends");
    assert_eq!(failed, nothing);
}

/// A merge the person asked for while the task's branch was still on its way home outlives a
/// restart of the server: the trip is taken up again, and once the branch is home the merge
/// is queued.
#[tokio::test]
async fn a_merge_waiting_for_its_branch_outlives_a_restart() {
    use slopty_proto::project::{Merge, Report};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let studio_terms =
        || vec![in_repo(orchestrator, "/w/demo", Some("https://example.com/o/demo.git"))];
    let (studio, studio_lease, mut studio_rx) =
        worker_on(&hub, "studio", Os::MacOs, studio_terms());
    let agent = SessionId::new();
    let linux_terms = || vec![in_repo(agent, "/home/c/demo", None)];
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, linux_terms());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let task = new_task(&hub, Some(linux)).await;
    let launch = claude();
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
    forge_has_the_target(&studio_lease, &mut studio_rx).await;
    let start = request(&mut linux_rx).await;
    let term = opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let branch = format!("worktree-slopty-slopty-{task}");
    let report = Report {
        note: "Built it.".to_owned(),
        artifacts: Vec::new(),
        branch: Some(branch.clone()),
        pr: None,
    };
    let verb = Verb::TaskReport { project: project(), task, report };
    let done = hub.dispatch_as(Speaker::Proven(term.session), None, verb).await;
    assert!(matches!(done, Outcome::Task(_)), "{done:?}");
    let (_, verb) = request(&mut linux_rx).await;
    assert!(matches!(verb, Verb::BundleBranch { .. }), "on its way home: {verb:?}");
    let merged = hub.dispatch(Verb::TaskMerge { project: project(), task }).await;
    assert!(matches!(merged, Outcome::Task(_)), "{merged:?}");
    let file = hub.projects_file(0);
    assert_eq!(file.merges, [(project(), task)]);
    let known = hub.directory();
    drop((linux_lease, linux_rx, studio_lease, studio_rx));
    drop(hub);

    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let (_, studio_lease, mut studio_rx) =
        worker_again(&hub, studio, "studio", Os::MacOs, studio_terms());
    let (_, linux_lease, mut linux_rx) = worker_again(&hub, linux, "box", Os::Linux, linux_terms());
    let (id, verb) = request(&mut linux_rx).await;
    let Verb::BundleBranch { target, .. } = verb else { panic!("taken up again: {verb:?}") };
    let name = format!("{branch}-4a7aa6d00000.bundle");
    let path = format!("/home/c/.cache/slopty/bundles/{name}");
    let head = format!("{}4a7aa", "4a7aa6d".repeat(5));
    let made = BranchBundle { path, name, size: 6, digest: [3; 32], head, base: None };
    assert!(target.is_some(), "the fork point first");
    answer(&linux_lease, id, Outcome::Bundle(Box::new(made)));
    let (id, _) = request(&mut linux_rx).await;
    answer(&linux_lease, id, Outcome::File { bytes: b"bundle".to_vec(), offset: 0, size: 6 });
    for _ in 0..2 {
        let (id, verb) = request(&mut studio_rx).await;
        assert!(matches!(verb, Verb::Upload { .. }), "{verb:?}");
        answer(&studio_lease, id, Outcome::Done);
    }
    let (id, verb) = request(&mut studio_rx).await;
    let Verb::FetchBundle { into, head, .. } = verb else { panic!("{verb:?}") };
    answer(&studio_lease, id, Outcome::Fetched { branch: into, head });
    let (id, verb) = request(&mut studio_rx).await;
    assert!(matches!(verb, Verb::TestDiff { .. }), "{verb:?}");
    let unread = Outcome::Error { code: ErrorCode::Failed, message: "no".to_owned() };
    answer(&studio_lease, id, unread);
    let queued = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(task_now(&hub, task).await.merge, Some(Merge::Queued { .. })) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(queued.is_ok(), "the merge is queued once the branch is home");
    assert!(hub.projects_file(0).merges.is_empty(), "and waits no longer");
}

/// Only the person lets a project go. Every client is sent the projects afresh without it, its
/// store forgets it, and its name is free again.
#[tokio::test]
async fn the_person_lets_a_project_go_and_every_client_hears_it() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let mut kept = hub.keep_projects();
    create(&hub, None).await;
    new_task(&hub, None).await;
    let mut pushed = hub.subscribe();
    let delete = || Verb::ProjectDelete { project: project() };
    let theirs = hub.dispatch_as(Speaker::Agent, None, delete()).await;
    refused(&theirs, ErrorCode::Forbidden);
    assert!(matches!(hub.dispatch(delete()).await, Outcome::Done));
    let mut fresh = Vec::new();
    while let Ok(msg) = pushed.try_recv() {
        if let FromServer::Projects(part) = msg {
            fresh.push(*part);
        }
    }
    let [part] = fresh.as_slice() else { panic!("one part: {fresh:?}") };
    assert!(part.first && part.last && part.projects.is_empty(), "{part:?}");
    assert!(matches!(hub.dispatch(Verb::ProjectList).await, Outcome::Projects(p) if p.is_empty()));
    refused(&hub.dispatch(delete()).await, ErrorCode::UnknownProject);

    let mut file = ProjectsFile::default();
    while let Ok(keep) = kept.try_recv() {
        file.apply(&keep);
    }
    assert!(file.projects.is_empty(), "the store's replay forgets it: {file:?}");
    create(&hub, None).await;
}

/// A task never goes to a worker that cannot run its agent: a Codex task only where Codex is
/// installed, pinned or not, and the refusal says so. It opens the person's own `codex` with its
/// role as developer instructions and its brief last, held to asking.
#[tokio::test]
async fn a_codex_task_goes_only_where_codex_is_and_starts_with_its_role() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, _mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    mac_lease.handle(ToServer::Facts(installed(&["claude"])));
    linux_lease.handle(ToServer::Facts(installed(&["codex"])));
    create(&hub, None).await;
    let docs = new_task(&hub, None).await;
    let codex = |pin: Option<WorkerId>| TaskLaunch { pin, agent: AgentId::named(AgentId::CODEX) };
    let spawn_verb =
        |task: TaskId, launch: TaskLaunch| Verb::TaskSpawn { project: project(), task, launch };

    let pinned = hub.dispatch(spawn_verb(docs, codex(Some(mac)))).await;
    let message = refused(&pinned, ErrorCode::Unplaced);
    assert!(message.contains("studio: codex is not installed"), "a pin is no way round: {message}");

    let asked = spawn(&hub, spawn_verb(docs, codex(None)));
    let start = request(&mut linux_rx).await;
    let Verb::OpenTerminal { worker, command, env, .. } = &start.1 else { panic!("{:?}", start.1) };
    assert_eq!(*worker, linux);
    assert_eq!(command[..2], ["codex", "-c"]);
    assert!(
        command[2].starts_with("developer_instructions=\"You are the agent of task"),
        "{command:?}"
    );
    let held = ["--ask-for-approval", "on-request", "--sandbox", "workspace-write"];
    assert_eq!(command[3..7], held, "held to asking, whatever its config.toml says");
    assert_eq!(command[7..], ["Build it."], "no clone known: no worktree");
    assert!(env.iter().any(|(k, v)| k == TASK_ENV && *v == docs.to_string()), "{env:?}");
    let term = opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    // Its row says what Codex runs with: asking, it stays; switched past it, it is closed.
    let mut tui = ladder::tests::row(slopty_proto::thread::Phase::Idle, 1, Some(term.session));
    tui.agent = AgentId::named(AgentId::CODEX);
    tui.meters.mode = Some("on-request".to_owned());
    tui.facts.insert("sandbox".to_owned(), "workspaceWrite".to_owned());
    linux_lease.handle(ladder::tests::snapshot(vec![tui.clone()]));
    let quiet = tokio::time::timeout(Duration::from_millis(100), request(&mut linux_rx)).await;
    assert!(quiet.is_err(), "asking: nothing closes {quiet:?}");
    tui.facts.insert("sandbox".to_owned(), "dangerFullAccess".to_owned());
    linux_lease.handle(ladder::tests::snapshot(vec![tui]));
    let (_, close) = request(&mut linux_rx).await;
    assert_eq!(close, Verb::Close { term });
    let said = status(&hub).await.timeline.into_iter().rev().find_map(|e| match e.what {
        Moment::Note { text } if e.task == Some(docs) => Some(text),
        _ => None,
    });
    assert_eq!(
        said.as_deref(),
        Some(
            "its Codex runs with sandbox dangerFullAccess, looser than the person allows \
             (`[server.projects] permission_flags`), so it was closed"
        )
    );
}

/// A project carries the goal the person handed over and how far its agents go before they
/// ask: the goal kept trimmed, the autonomy changed by the person alone. An orchestrator that
/// tries to set it is refused and changes nothing.
#[tokio::test]
async fn a_project_carries_its_goal_and_the_person_s_autonomy() {
    use slopty_proto::project::Autonomy;
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let (worker, lease, _rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    let made = hub
        .dispatch(Verb::ProjectCreate {
            project: project(),
            title: "Projects".to_owned(),
            goal: Some("  Ship projects mode  ".to_owned()),
            autonomy: Autonomy::Edits,
            repo: "~/src/slopty".to_owned(),
            target: "main".to_owned(),
            verifier: None,
            push: false,
            orchestrator: Some(TermRef { worker, session: orchestrator }),
            limits: LimitsChange::default(),
            metadata: None,
        })
        .await;
    let Outcome::Project(made) = made else { panic!("{made:?}") };
    let kept = (made.project.goal.as_deref(), made.project.autonomy);
    assert_eq!(kept, (Some("Ship projects mode"), Autonomy::Edits), "trimmed");
    let set = |autonomy| Verb::ProjectSet {
        project: project(),
        autonomy,
        orchestrator: None,
        verifier: None,
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
    };
    let said = hub.dispatch_as(Speaker::Proven(orchestrator), None, set(Some(Autonomy::Own))).await;
    assert!(refused(&said, ErrorCode::Forbidden).contains("autonomy"), "{said:?}");
    assert_eq!(status(&hub).await.project.autonomy, Autonomy::Edits, "a refusal changes nothing");
    let Outcome::Project(own) = hub.dispatch(set(Some(Autonomy::Own))).await else { panic!() };
    assert_eq!(own.project.autonomy, Autonomy::Own, "the person's word");
    let Outcome::Project(kept) = hub.dispatch(set(None)).await else { panic!() };
    assert_eq!(kept.project.autonomy, Autonomy::Own, "left out is left alone");
}

/// The orchestrator says where the goal stands: it is kept on the project and its timeline, a
/// task's agent may not say it, and an empty or too long summary is refused.
#[tokio::test]
async fn the_orchestrator_says_where_the_goal_stands() {
    use slopty_proto::project::Progress;
    let hub = Hub::new("server".to_owned(), Vec::new());
    let orchestrator = SessionId::new();
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker, session: orchestrator })).await;
    let say = |summary: &str, done| Verb::ProjectProgress {
        project: project(),
        summary: summary.to_owned(),
        next: Some(" The phone's inbox ".to_owned()),
        done,
    };
    let said = hub.dispatch_as(Speaker::Proven(orchestrator), None, say("Store merged", false));
    let Outcome::Project(now) = said.await else { panic!("not a project") };
    let progress = now.project.progress.expect("kept");
    let kept = (progress.summary.as_str(), progress.next.as_deref(), progress.done);
    assert_eq!(kept, ("Store merged", Some("The phone's inbox"), false));
    let logged = status(&hub).await.timeline.into_iter().rev().find_map(|e| match e.what {
        Moment::Update(p) => Some(p),
        _ => None,
    });
    assert_eq!(logged.map(|p| p.summary), Some("Store merged".to_owned()), "on the timeline");

    refused(&hub.dispatch(say("  ", false)).await, ErrorCode::Invalid);
    let long = "x".repeat(Progress::TEXT_MAX + 1);
    refused(&hub.dispatch(say(&long, false)).await, ErrorCode::Invalid);
    let task = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude() });
    let start = request(&mut rx).await;
    let term = opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let by_task = hub.dispatch_as(Speaker::Proven(term.session), None, say("All done", true));
    assert!(refused(&by_task.await, ErrorCode::Forbidden).contains("orchestrator"));
    assert_eq!(status(&hub).await.project.progress.map(|p| p.done), Some(false));
}
