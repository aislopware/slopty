//! Projects through the hub: verbs answered from the store, a task's terminal placed on the
//! worker it names or the least busy one and put on the task, every start counted against the
//! bounds, and what a worker's link reports moving the task.

use std::collections::BTreeMap;
use std::time::Duration;

use slopty_agent::status::{AgentStatus, BlockReason};
use slopty_agent::vouch::SessionKey;
use slopty_proto::agent::{AgentKind, Worktree};
use slopty_proto::orchestration::{BranchBundle, ThreadOf, UploadPart};
use slopty_proto::project::{
    Bounds, Fact, LimitsChange, Moment, PROJECT_ENV, ProjectId, Runner, TASK_ENV, TaskCard,
    TaskChange, TaskId, TaskLaunch, TaskSpec, TaskState,
};
use slopty_proto::server::Os;
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
        agent: slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::CLAUDE_CODE),
        version: "2.1.0".to_owned(),
        offers: slopty_proto::thread::Offers::default(),
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
            members: Vec::new(),
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

pub(super) fn claude(args: &[&str]) -> TaskLaunch {
    TaskLaunch {
        pin: None,
        cwd: "~/src/slopty".to_owned(),
        run: Runner::Claude {
            prompt: Some("Read your brief.".to_owned()),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        },
        env: vec![("SLOPTY_TASK".to_owned(), "99".to_owned())],
        size: None,
        ignore_dependencies: false,
    }
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

/// A task pinned to the Linux worker starts there, never on the Mac, with the project and the
/// task in its environment over the caller's; once the worker answers it is the task's
/// terminal, and every link hears so. With the Linux worker gone, the start is refused saying
/// why.
#[tokio::test]
async fn a_pinned_task_is_spawned_on_its_worker_and_put_on_its_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_mac, _mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Some(linux)).await;
    let mut pushed = hub.subscribe();

    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    let start = request(&mut linux_rx).await;
    let verb = start.1.clone();
    let Verb::SpawnAgent { worker, agent, env, cwd, prompt, .. } = verb else { panic!("{verb:?}") };
    assert_eq!((worker, agent, cwd.as_str()), (linux, AgentKind::ClaudeCode, "~/src/slopty"));
    assert_eq!(prompt.as_deref(), Some("Read your brief."));
    let last = |name: &str| env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    assert_eq!(last(PROJECT_ENV), Some("slopty"));
    assert_eq!(last(TASK_ENV), Some("1"), "the server's task wins over the caller's");
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
    let again = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    refused(&again.await, ErrorCode::Conflict);

    drop(linux_lease);
    let other = new_task(&hub, Some(linux)).await;
    let no = hub.dispatch(Verb::TaskSpawn { project: project(), task: other, launch: claude(&[]) });
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
    let launch = TaskLaunch { pin: Some(mac), ..claude(&[]) };
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
    let asked =
        spawn(&hub, Verb::TaskSpawn { project: project(), task: pinned, launch: claude(&[]) });
    let start = request(&mut mac_rx).await;
    opened(&mac_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    linux_rx.try_recv().unwrap_err();

    let free = new_task(&hub, None).await;
    hub.dispatch(run_on(free, RunOn::Worker(mac))).await;
    hub.dispatch(run_on(free, RunOn::Anywhere)).await;
    assert_eq!(task_now(&hub, free).await.pin, None, "anywhere takes the pin off");
    let asked =
        spawn(&hub, Verb::TaskSpawn { project: project(), task: free, launch: claude(&[]) });
    let start = request(&mut linux_rx).await;
    assert_eq!(chosen(&start.1).0, linux, "the worker running the fewest agents");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
}

/// A task runs any command, not only Claude Code: a benchmark opens in a terminal of its own,
/// named for the task, with the project and task in its environment, and is the task's.
#[tokio::test]
async fn a_command_task_is_placed_run_and_put_on_its_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let launch = TaskLaunch {
        run: Runner::Command { argv: vec!["cargo".to_owned(), "bench".to_owned()] },
        cwd: String::new(),
        ..claude(&[])
    };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
    let start = request(&mut rx).await;
    let verb = start.1.clone();
    let Verb::OpenTerminal { worker, cwd, command, env, name, .. } = verb else {
        panic!("{verb:?}")
    };
    assert_eq!((worker, cwd), (linux, None));
    assert_eq!(command, ["cargo", "bench"]);
    assert_eq!(name.as_deref(), Some("slopty #1"));
    assert!(env.contains(&(TASK_ENV.to_owned(), "1".to_owned())));
    let term = opened(&lease, &start);
    let Outcome::Task(started) = asked.await.unwrap() else { panic!("not a task") };
    assert_eq!(started.assignment.map(|a| a.term), Some(term));
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
    let first = spawn(&hub, Verb::TaskSpawn { project: project(), task: a, launch: claude(&[]) });
    let second = spawn(&hub, Verb::TaskSpawn { project: project(), task: b, launch: claude(&[]) });
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
    let again = Verb::TaskSpawn { project: project(), task: refused_task, launch: claude(&[]) };
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
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
        agent: AgentKind::ClaudeCode,
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
    let start = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    refused(&start.await, ErrorCode::Limit);
}

/// No agent starts another with more than it has: flags that loosen Claude Code's permissions
/// are refused on every way to start one, unless the person allowed them for the project.
#[tokio::test]
async fn flags_that_loosen_permissions_need_the_person_s_word() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, _lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let loose = claude(&["--dangerously-skip-permissions"]);
    let start = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: loose.clone() });
    assert!(refused(&start.await, ErrorCode::Limit).contains("permission_flags"));
    let command = TaskLaunch {
        run: Runner::Command {
            argv: ["claude", "--permission-mode", "bypassPermissions"].map(str::to_owned).to_vec(),
        },
        ..claude(&[])
    };
    let start = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: command });
    refused(&start.await, ErrorCode::Limit);
    let plain = Verb::SpawnAgent {
        worker: linux,
        agent: AgentKind::ClaudeCode,
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
    assert!(rx.try_recv().is_err(), "nothing reached the worker");

    let allowed = Policy { permission_flags: [project()].into(), ..Policy::default() };
    hub.set_policy(allowed);
    assert!(status(&hub).await.bounds.permission_flags);
    let start = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: loose });
    let (_, verb) = request(&mut rx).await;
    let Verb::SpawnAgent { args, permission_flags, .. } = verb else { panic!("{verb:?}") };
    assert!(permission_flags, "the worker leaves bypass mode unlocked");
    assert_eq!(args.last().map(String::as_str), Some("--dangerously-skip-permissions"));
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
    let again = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
    let again = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
            members: None,
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
        let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
    lease.handle(mode(started[1], "acceptEdits"));
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
    opened_by_agent.abort();
    lease.handle(mode(TermRef { worker: linux, session }, "auto"));
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term: TermRef { worker: linux, session } }, "an agent's shell");

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
    let task = new_task(&hub, None).await;
    let line = "cd ~/src && claude --allowedTools Bash".to_owned();
    let sh = |line: String| vec!["/bin/sh".to_owned(), "-c".to_owned(), line];
    let launch = TaskLaunch { run: Runner::Command { argv: sh(line.clone()) }, ..claude(&[]) };
    let wrapped = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch }).await;
    assert!(refused(&wrapped, ErrorCode::Limit).contains("--allowedTools"), "{wrapped:?}");
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
    assert!(rx.try_recv().is_err(), "neither reached the worker");

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
            Ok(Some(FromServer::Deliver { session, batch, context })) => {
                return (session, batch, context);
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    };
    let (session, batch, context) = next_batch(&mut rx).await;
    assert_eq!(session, orchestrator.session);
    assert!(context.contains("You orchestrate the Slopty project slopty"), "{context}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));

    let task = new_task(&hub, None).await;
    let other = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
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
        agent: AgentKind::ClaudeCode,
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
/// that moves what runs there, or whom it speaks for, is refused on each way to start, before
/// anything reaches a worker. The person names what they like.
#[tokio::test]
async fn an_agent_names_no_environment_that_steers_what_it_starts() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let orchestrator = SessionId::new();
    announce(&lease, orchestrator, true);
    create(&hub, Some(TermRef { worker: linux, session: orchestrator })).await;
    let task = new_task(&hub, None).await;
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
        agent: AgentKind::ClaudeCode,
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
        (Verb::TaskSpawn { project: project(), task, launch: claude(&[]) }, "SLOPTY_TASK"),
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
        members: None,
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
        members: None,
    };
    let theirs = hub.dispatch_as(as_orchestrator, None, limit(Some(5))).await;
    assert!(refused(&theirs, ErrorCode::Forbidden).contains("review limit"), "{theirs:?}");
    assert!(matches!(hub.dispatch(limit(Some(1))).await, Outcome::Project(_)));
    let ready = new_task(&hub, None).await;
    let done = Box::new(TaskChange { state: Some(TaskState::Done), ..TaskChange::default() });
    let update = Verb::TaskUpdate { project: project(), task: ready, change: done };
    assert!(matches!(hub.dispatch(update).await, Outcome::Task(_)));

    let next = new_task(&hub, None).await;
    let launch = TaskLaunch { env: Vec::new(), ..claude(&[]) };
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
            members: Vec::new(),
        })
        .await;
    assert!(matches!(made, Outcome::Project(_)), "{made:?}");
    hub.set_policy(Policy { permission_flags: [project()].into(), ..Policy::default() });
    let loose = Verb::SpawnAgent {
        worker: linux,
        agent: AgentKind::ClaudeCode,
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
    let launch = TaskLaunch { env: Vec::new(), ..claude(&["--allowedTools", "Bash"]) };
    let spawned = Verb::TaskSpawn { project: project(), task, launch };
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
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    let agent = opened(&lease, &request(&mut rx).await);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let as_agent = Speaker::Proven(agent.session);
    let spec =
        TaskSpec { title: "Part".to_owned(), brief: "A part.".to_owned(), ..TaskSpec::default() };
    let tell = Verb::TaskTell { project: project(), task: Some(beside), text: "Go.".to_owned() };
    for verb in [
        Verb::TaskCreate { project: project(), spec: Box::new(spec) },
        Verb::TaskSpawn { project: project(), task: beside, launch: claude(&[]) },
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
                members: Vec::new(),
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
            Ok(Some(FromServer::Deliver { context, .. })) => break context,
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
    let (_bare, _bare_lease, _bare_rx) = worker_on(&hub, "bare", Os::Linux, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let anywhere = TaskLaunch { cwd: String::new(), ..claude(&[]) };

    let writes = new_task(&hub, Some(linux)).await;
    let verb = Verb::TaskSpawn { project: project(), task: writes, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
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
    let launch =
        TaskLaunch { run: Runner::Codex { prompt: None, args: Vec::new() }, ..anywhere.clone() };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task: codex_task, launch });
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

    drop((studio_lease, linux_lease));
    let stranded = new_task(&hub, None).await;
    let verb = Verb::TaskSpawn { project: project(), task: stranded, launch: anywhere };
    let no = hub.dispatch(verb).await;
    let said = refused(&no, ErrorCode::Unplaced);
    assert!(said.contains("no address to clone it from is known"), "{said}");
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
    let (studio, _studio_lease, _studio_rx) = worker_on(&hub, "studio", Os::MacOs, studio);
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, Some(TermRef { worker: studio, session: orchestrator })).await;
    let anywhere = TaskLaunch { cwd: String::new(), ..claude(&[]) };

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
    let start = request(&mut linux_rx).await;
    let Verb::SpawnAgent { cwd, args, .. } = start.1.clone() else { panic!("{:?}", start.1) };
    assert_eq!(cwd, path, "the agent starts in the clone made");
    assert!(args.iter().any(|a| a == "--worktree"), "{args:?}");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    let done = step_of(&hub, task).await.map(|s| s.state);
    assert_eq!(done, Some(StepState::Done { detail: path.clone() }));
    let steps: Vec<_> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Step(s) => Some(s.state),
            _ => None,
        })
        .collect();
    assert!(
        matches!(steps.as_slice(), [StepState::Running { .. }, StepState::Done { .. }]),
        "{steps:?}"
    );

    // The next task there finds the clone: no second one.
    let next = new_task(&hub, Some(linux)).await;
    let verb = Verb::TaskSpawn { project: project(), task: next, launch: anywhere.clone() };
    let asked = spawn(&hub, verb);
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
    let launch = TaskLaunch { cwd: String::new(), ..claude(&[]) };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
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
    assert_eq!(arrived, [(StepKind::Home, studio, detail)], "the branch arrived");
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
/// role as developer instructions, its brief last; a flag
/// that would loosen what it asks the person is refused unless the person allows it.
#[tokio::test]
async fn a_codex_task_goes_only_where_codex_is_and_starts_with_its_role() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, _mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    mac_lease.handle(ToServer::Facts(installed(&["claude"])));
    linux_lease.handle(ToServer::Facts(installed(&["codex"])));
    create(&hub, None).await;
    let docs = new_task(&hub, None).await;
    let codex = |args: &[&str], pin: Option<WorkerId>| TaskLaunch {
        pin,
        run: Runner::Codex {
            prompt: Some("Read your brief.".to_owned()),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        },
        ..claude(&[])
    };
    let spawn_verb =
        |task: TaskId, launch: TaskLaunch| Verb::TaskSpawn { project: project(), task, launch };

    let pinned = hub.dispatch(spawn_verb(docs, codex(&[], Some(mac)))).await;
    let message = refused(&pinned, ErrorCode::Unplaced);
    assert!(message.contains("studio: codex is not installed"), "a pin is no way round: {message}");
    let loose = codex(&["--dangerously-bypass-approvals-and-sandbox"], None);
    let answer = hub.dispatch(spawn_verb(docs, loose)).await;
    let message = refused(&answer, ErrorCode::Limit);
    assert!(
        message.starts_with("--dangerously-bypass-approvals-and-sandbox may give"),
        "{message}"
    );

    let asked = spawn(&hub, spawn_verb(docs, codex(&["-m", "o3"], None)));
    let start = request(&mut linux_rx).await;
    let Verb::OpenTerminal { worker, command, env, .. } = &start.1 else { panic!("{:?}", start.1) };
    assert_eq!(*worker, linux);
    assert_eq!(command[..2], ["codex", "-c"]);
    assert!(
        command[2].starts_with("developer_instructions=\"You are the agent of task"),
        "{command:?}"
    );
    assert_eq!(command[3..], ["-m", "o3", "Read your brief."], "a named cwd: no worktree");
    assert!(env.iter().any(|(k, v)| k == TASK_ENV && *v == docs.to_string()), "{env:?}");
    opened(&linux_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
}

/// A member: what a tile must have to be in the project.
fn member(pairs: &[(&str, &str)]) -> slopty_proto::project::Matcher {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// A project is a name and its members: one with no orchestrator, no repository and no target
/// is kept, and listed with its members, its values trimmed.
#[tokio::test]
async fn a_project_without_an_orchestrator_is_kept_and_listed() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let notes = member(&[("machine", "studio"), ("cwd", " ~/notes ")]);
    let made = hub
        .dispatch(Verb::ProjectCreate {
            project: project(),
            title: "Notes".to_owned(),
            members: vec![notes, member(&[("machine", "devbox"), ("cwd", "/w/notes")])],
            repo: String::new(),
            target: String::new(),
            verifier: None,
            push: false,
            orchestrator: None,
            limits: LimitsChange::default(),
            metadata: None,
        })
        .await;
    assert!(matches!(made, Outcome::Project(_)), "{made:?}");
    let Outcome::Projects(listed) = hub.dispatch(Verb::ProjectList).await else { panic!() };
    let [notes] = listed.as_slice() else { panic!("{listed:?}") };
    assert_eq!((notes.orchestrator, notes.repo.as_str()), (None, ""));
    assert_eq!(notes.members.len(), 2);
    let cwd = notes.members.first().and_then(|m| m.get("cwd")).map(String::as_str);
    assert_eq!(cwd, Some("~/notes"), "trimmed");
}

/// Members are set whole and kept when a change leaves them out; an empty member, which would
/// match nothing, and one named twice are refused and change nothing.
#[tokio::test]
async fn members_name_clones_and_folders() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let set = |members: Option<Vec<slopty_proto::project::Matcher>>| Verb::ProjectSet {
        project: project(),
        members,
        orchestrator: None,
        verifier: None,
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
    };
    let api = member(&[("repo", "github.com/aislopware/api")]);
    let site = member(&[("repo", "github.com/aislopware/site")]);
    let Outcome::Project(two) = hub.dispatch(set(Some(vec![api.clone(), site]))).await else {
        panic!("set")
    };
    assert_eq!(two.project.members.len(), 2);
    let Outcome::Project(kept) = hub.dispatch(set(None)).await else { panic!("kept") };
    assert_eq!(kept.project.members.len(), 2, "left out is left alone");
    refused(&hub.dispatch(set(Some(vec![member(&[])]))).await, ErrorCode::Invalid);
    refused(&hub.dispatch(set(Some(vec![api.clone(), api]))).await, ErrorCode::Invalid);
    assert_eq!(status(&hub).await.project.members.len(), 2, "a refusal changes nothing");
}
