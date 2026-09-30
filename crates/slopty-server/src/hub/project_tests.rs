//! Projects through the hub: verbs answered from the store, a task's terminal placed by rules
//! over the workers' facts and put on the task, every start counted against the bounds, and
//! what a worker's link reports moving the task.

use std::collections::BTreeMap;
use std::time::Duration;

use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, BlockReason, PullRequest, Worktree};
use slopty_proto::project::{
    AGENT_TOKEN_ENV, Bounds, Fact, LimitsChange, Moment, PROJECT_ENV, Placement, ProjectId, Runner,
    TASK_ENV, TaskCard, TaskChange, TaskId, TaskLaunch, TaskSpec, TaskState,
};
use slopty_proto::server::Os;

use super::tests::{caps, registration, summary};
use super::*;
use crate::project::Policy;
use crate::vouch::AgentKey;

fn project() -> ProjectId {
    ProjectId::new("slopty").unwrap()
}

/// A worker on `os` named `name`, running `sessions`.
fn worker_on(
    hub: &Hub,
    name: &str,
    os: Os,
    sessions: Vec<SessionSummary>,
) -> (WorkerId, Lease, mpsc::Receiver<FromServer>) {
    let worker = WorkerId::new();
    worker_again(hub, worker, name, os, sessions)
}

fn worker_again(
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
    let lease = hub.register(registration, ip, tx).unwrap();
    (worker, lease, rx)
}

async fn create_with(hub: &Hub, orchestrator: Option<TermRef>, limits: LimitsChange) {
    let made = hub
        .dispatch(Verb::ProjectCreate {
            project: project(),
            title: "Projects".to_owned(),
            repo: "~/src/slopty".to_owned(),
            target: "main".to_owned(),
            verifier: None,
            orchestrator,
            limits,
            metadata: None,
        })
        .await;
    assert!(matches!(made, Outcome::Project(_)), "{made:?}");
}

async fn create(hub: &Hub, orchestrator: Option<TermRef>) {
    create_with(hub, orchestrator, LimitsChange::default()).await;
}

async fn new_task(hub: &Hub, placement: Placement) -> TaskId {
    let spec = TaskSpec {
        title: "Server".to_owned(),
        brief: "Build it.".to_owned(),
        placement,
        ..TaskSpec::default()
    };
    match hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await {
        Outcome::Task(task) => task.id,
        other => panic!("{other:?}"),
    }
}

fn linux_only() -> Placement {
    Placement { require: vec![r#"os == "linux""#.to_owned()], ..Placement::default() }
}

async fn status(hub: &Hub) -> ProjectStatus {
    let verb = Verb::ProjectStatus { project: project(), since: Some(0), timeout_ms: 0 };
    match hub.dispatch(verb).await {
        Outcome::Project(status) => *status,
        other => panic!("{other:?}"),
    }
}

async fn task_now(hub: &Hub, task: TaskId) -> TaskCard {
    status(hub).await.tasks.into_iter().find(|t| t.id == task).unwrap()
}

fn claude(args: &[&str]) -> TaskLaunch {
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

fn spawn(hub: &Hub, verb: Verb) -> tokio::task::JoinHandle<Outcome> {
    let hub = hub.clone();
    tokio::spawn(async move { hub.dispatch(verb).await })
}

async fn request(rx: &mut mpsc::Receiver<FromServer>) -> (RequestId, Verb) {
    match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
        Ok(Some(FromServer::Request { id, verb, .. })) => (id, verb),
        other => panic!("no request: {other:?}"),
    }
}

/// The worker opens the terminal it was asked for, under the id the server chose, with an agent
/// in it, and answers.
fn opened(lease: &Lease, (id, verb): &(RequestId, Verb)) -> TermRef {
    let (worker, session) = chosen(verb);
    let started = AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Working,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::ZERO,
    };
    let with_agent =
        SessionSummary { agent: Some(SessionAgent::from(&started)), ..summary(session) };
    lease.handle(ToServer::SessionChanged(with_agent));
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

fn refused(outcome: &Outcome, code: ErrorCode) -> &str {
    match outcome {
        Outcome::Error { code: c, message } if *c == code => message,
        other => panic!("not {code:?}: {other:?}"),
    }
}

/// A Linux-only task starts on the Linux worker, never the Mac, with the project and the task
/// in its environment over the caller's; once the worker answers it is the task's terminal, and
/// every link hears so. With the Linux worker gone, the start is refused saying why of each.
#[tokio::test]
async fn a_linux_only_task_is_spawned_on_the_linux_worker_and_put_on_its_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_mac, _mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, linux_only()).await;
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
    let other = new_task(&hub, linux_only()).await;
    let no = hub.dispatch(Verb::TaskSpawn { project: project(), task: other, launch: claude(&[]) });
    let no = no.await;
    let said = refused(&no, ErrorCode::Unplaced);
    assert!(said.contains("box: not online"), "{said}");
    assert!(said.contains("studio: false here"), "{said}");
}

/// A pin is the orchestrator's own word: the task starts there whatever its rules say.
#[tokio::test]
async fn a_pinned_start_goes_where_it_is_pinned_over_every_rule() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (_linux, _linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, linux_only()).await;
    let launch = TaskLaunch { pin: Some(mac), ..claude(&[]) };
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch });
    let start = request(&mut mac_rx).await;
    let verb = start.1.clone();
    assert!(matches!(verb, Verb::SpawnAgent { worker, .. } if worker == mac));
    linux_rx.try_recv().unwrap_err();
    opened(&mac_lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
}

/// A task runs any command, not only Claude Code: a benchmark opens in a terminal of its own,
/// named for the task, with the project and task in its environment, and is the task's.
#[tokio::test]
async fn a_command_task_is_placed_run_and_put_on_its_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, linux_only()).await;
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

/// Two starts at once never both take the last place on a worker: the second is placed while
/// the first is still starting, and counts it.
#[tokio::test]
async fn concurrent_starts_never_pass_the_project_s_cap_per_worker() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let one = LimitsChange { live_per_worker: Some(1), ..LimitsChange::default() };
    create_with(&hub, None, one).await;
    let (a, b) =
        (new_task(&hub, Placement::default()).await, new_task(&hub, Placement::default()).await);
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
    assert!(refused(&late, ErrorCode::Unplaced).contains("live_per_worker"), "{late:?}");
    assert!(rx.try_recv().is_err(), "one start went to the worker");
    opened(&lease, &start);
    assert!(matches!(placed.await.unwrap(), Outcome::Task(_)));
    let again = Verb::TaskSpawn { project: project(), task: refused_task, launch: claude(&[]) };
    let full = hub.dispatch(again);
    assert!(refused(&full.await, ErrorCode::Unplaced).contains("box: runs 1"));
}

/// A caller that leaves while its start is on the way does not leave an agent running for
/// nobody: the start goes on, and what the worker opens is the task's.
#[tokio::test]
async fn a_start_whose_caller_left_still_puts_its_terminal_on_the_task() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
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
    let task = new_task(&hub, Placement::default()).await;
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
    };
    let asked = spawn(&hub, plain());
    let start = request(&mut rx).await;
    opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Opened(_)));
    let said = refused(&hub.dispatch(plain()).await, ErrorCode::Limit).to_owned();
    assert!(said.contains("runs 1 agents") && said.contains("live_agents"), "{said}");
    let task = new_task(&hub, Placement::default()).await;
    let start = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    refused(&start.await, ErrorCode::Limit);
    let greedy = LimitsChange { live_per_project: Some(500), ..LimitsChange::default() };
    let set = Verb::ProjectSet {
        project: project(),
        orchestrator: None,
        verifier: None,
        limits: greedy,
        metadata: None,
    };
    assert!(refused(&hub.dispatch(set).await, ErrorCode::Limit).contains("the person allows"));
}

/// An orchestrator that starts an agent and puts it on a task at once is not refused because
/// its worker has not announced the terminal yet, and the start counts against the project
/// from then. A terminal nobody opened is still unknown.
#[tokio::test]
async fn a_terminal_just_opened_is_assigned_before_its_worker_announces_it() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
    let asked = spawn(
        &hub,
        Verb::SpawnAgent {
            worker: linux,
            agent: AgentKind::ClaudeCode,
            cwd: "~".to_owned(),
            prompt: None,
            args: Vec::new(),
            env: Vec::new(),
            size: None,
            session: None,
            permission_flags: false,
        },
    );
    let start = request(&mut rx).await;
    let (_, session) = chosen(&start.1);
    let term = TermRef { worker: linux, session };
    lease.handle(ToServer::Reply { id: start.0, outcome: Outcome::Opened(term) });
    assert!(matches!(asked.await.unwrap(), Outcome::Opened(t) if t == term));

    let assigned = hub.dispatch(Verb::TaskAssign { project: project(), task, term }).await;
    let Outcome::Task(on) = &assigned else { panic!("{assigned:?}") };
    assert_eq!(on.assignment.as_ref().map(|a| a.term), Some(term));
    assert_eq!(status(&hub).await.live.project, 1, "counted before it is announced");
    let nobody = TermRef { worker: linux, session: SessionId::new() };
    let unknown = hub.dispatch(Verb::TaskAssign { project: project(), task, term: nobody }).await;
    refused(&unknown, ErrorCode::UnknownTerminal);
}

/// No agent starts another with more than it has: flags that loosen Claude Code's permissions
/// are refused on every way to start one, unless the person allowed them for the project.
#[tokio::test]
async fn flags_that_loosen_permissions_need_the_person_s_word() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, _lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
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
    let task = new_task(&hub, Placement::default()).await;
    let term = TermRef { worker: linux, session };
    let assigned = hub.dispatch(Verb::TaskAssign { project: project(), task, term }).await;
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

/// What a worker reports of itself is a fact placement reads, beside what the server knows of
/// it; a suggestion ranks every worker with its reasons, and changes nothing.
#[tokio::test]
async fn a_suggestion_ranks_the_workers_by_their_reported_facts_with_reasons() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_studio, _studio_lease, _) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, lease, _rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let labels = BTreeMap::from([("fast-disk".to_owned(), Fact::Bool(true))]);
    let probes = BTreeMap::from([("cuda".to_owned(), Fact::Text("GPU 0".to_owned()))]);
    lease.handle(ToServer::Facts(BTreeMap::from([
        ("labels".to_owned(), Fact::Map(labels)),
        ("probes".to_owned(), Fact::Map(probes)),
        ("os".to_owned(), Fact::Text("pretend".to_owned())),
    ])));
    let Outcome::Facts(all) = hub.dispatch(Verb::WorkerFacts { worker: Some(linux) }).await else {
        panic!("facts")
    };
    let facts = &all.first().unwrap().facts;
    assert_eq!(facts.get("os"), Some(&Fact::Text("linux".to_owned())), "the server's word wins");
    assert!(facts.contains_key("labels") && facts.contains_key("cpus"));

    create(&hub, None).await;
    let placement = Placement {
        require: vec![r#"labels["fast-disk"]"#.to_owned()],
        prefer: vec![slopty_proto::project::Preference {
            expr: "has(probes.cuda)".to_owned(),
            weight: 5,
        }],
        ..Placement::default()
    };
    let verb =
        Verb::PlacementSuggest { project: Some(project()), task: None, placement: Some(placement) };
    let Outcome::Suggestions(ranked) = hub.dispatch(verb).await else { panic!("suggestions") };
    let names: Vec<(&str, bool, i64)> =
        ranked.iter().map(|s| (s.name.as_str(), s.fits, s.score)).collect();
    assert_eq!(names, [("box", true, 5), ("studio", false, 0)]);
    let studio = &ranked[1];
    assert!(studio.reasons.iter().any(|r| !r.held && r.detail.contains("labels")), "{studio:?}");
    let bad = Placement { require: vec!["os ==".to_owned()], ..Placement::default() };
    let verb = Verb::PlacementSuggest { project: None, task: None, placement: Some(bad) };
    refused(&hub.dispatch(verb).await, ErrorCode::BadExpression);
}

fn agent(session: SessionId, status: AgentStatus) -> ToServer {
    ToServer::Agent(AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status,
        agent_session: None,
        detail: None,
        attention: true,
        source: AgentSource::Hook,
        since_ms: WallMs::ZERO,
    })
}

/// What the worker's link says of an agent reaches its task: the worktree its status line
/// named before the agent was put on the task, a pull request after, Claude Code's own
/// subagent as a child node, a block, and at last its terminal closing.
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
    let branch = AgentBranch { session, pr: None, worktree: Some(worktree) };
    lease.handle(ToServer::Report(AgentReport::Branch(branch.clone())));
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
    let unknown = TermRef { worker, session: SessionId::new() };
    let no = hub.dispatch(Verb::TaskAssign { project: project(), task, term: unknown }).await;
    refused(&no, ErrorCode::UnknownTerminal);
    let Outcome::Task(assigned) =
        hub.dispatch(Verb::TaskAssign { project: project(), task, term }).await
    else {
        panic!("assigned")
    };
    assert_eq!(assigned.worktree.as_deref(), Some("/w/.claude/worktrees/rows"));

    let pr = PullRequest {
        number: 9,
        url: "https://github.com/o/r/pull/9".to_owned(),
        review: None,
        merge_request: false,
    };
    lease.handle(ToServer::Report(AgentReport::Branch(AgentBranch { pr: Some(pr), ..branch })));
    lease.handle(ToServer::Report(AgentReport::SubagentStarted {
        session,
        agent: "ag1".to_owned(),
        kind: "Explore".to_owned(),
    }));
    let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    lease.handle(agent(session, blocked));
    let s = status(&hub).await;
    let t = s.tasks.first().unwrap();
    assert_eq!(t.pr.as_ref().map(|p| p.number), Some(9));
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
    new_task(&hub, Placement::default()).await;
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
    new_task(&hub, Placement::default()).await;
    let Ok(FromServer::Event(after)) = pushed.try_recv() else { panic!("a change is pushed") };
    assert!(after.seq > snapshot.seq, "a change after the snapshot is past its seq");
}

/// A terminal the worker says it opened, with or without an agent in it.
fn announce(lease: &Lease, session: SessionId, agent: bool) {
    let running = AgentEvent {
        session,
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Working,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::ZERO,
    };
    let agent = agent.then(|| SessionAgent::from(&running));
    lease.handle(ToServer::SessionChanged(SessionSummary { agent, ..summary(session) }));
}

/// A start whose answer was lost may still have opened: it counts on, a second start of its
/// task is refused, and the terminal the worker then announces under the id the server chose
/// is put on the task with its conversation. No caller chooses that id.
#[tokio::test]
async fn a_start_whose_answer_was_lost_is_put_on_its_task_when_its_terminal_shows() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (linux, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
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
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
    let term = TermRef { worker, session: plain };
    let answer = Verb::AnswerPermission {
        term,
        ask: 1,
        verdict: slopty_proto::conversation::Verdict::Allow,
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
        Speaker::Agent,
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
        }),
        ..TaskChange::default()
    });
    let record = Verb::TaskUpdate { project: project(), task, change: verified };
    refused(&hub.dispatch_as(Speaker::Agent, None, record).await, ErrorCode::Forbidden);
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
            limits: LimitsChange::default(),
            metadata: None,
        },
    ] {
        let said = hub.dispatch_as(Speaker::Agent, None, verb).await;
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
        let task = new_task(&hub, Placement::default()).await;
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
    let task = new_task(&hub, Placement::default()).await;
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
#[tokio::test]
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
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
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

    hub.set_agent_key(AgentKey::from_bytes([7; 32]));
    let task = new_task(&hub, Placement::default()).await;
    let other = new_task(&hub, Placement::default()).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    let start = request(&mut rx).await;
    let Verb::SpawnAgent { env, .. } = &start.1 else { panic!("{:?}", start.1) };
    let token = env.iter().find(|(k, _)| k == AGENT_TOKEN_ENV).map(|(_, v)| v.clone());
    let token = token.expect("a task's terminal is given its token");
    let term = opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    assert!(hub.vouches(term.session, &token), "its own terminal's");
    assert!(!hub.vouches(orchestrator.session, &token), "no other's");

    let report = slopty_proto::project::Report {
        kind: slopty_proto::project::ReportKind::NeedsInput,
        note: "Which crate owns the store?".to_owned(),
        artifacts: Vec::new(),
        branch: Some("task-1".to_owned()),
        pr: None,
    };
    let report_as = |speaker, task| {
        let verb = Verb::TaskReport { project: project(), task, report: report.clone() };
        hub.dispatch_as(speaker, None, verb)
    };
    let unproven = report_as(Speaker::Agent, task).await;
    assert!(refused(&unproven, ErrorCode::Forbidden).contains(AGENT_TOKEN_ENV));
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
    assert!(context.contains("task 1: needs input\n  Which crate owns the store?"), "{context}");
    assert!(context.contains("branch: task-1"), "{context}");
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    assert!(deliver(&mut rx).is_none_or(|m| !matches!(m, FromServer::Deliver { .. })));
    let timeline = status(&hub).await.timeline;
    let kinds: Vec<&Moment> = timeline.iter().map(|e| &e.what).collect();
    assert!(kinds.iter().any(|m| matches!(m, Moment::Reported { .. })), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|m| matches!(m, Moment::Delivered { reports: 1, .. })).count(),
        2,
        "{kinds:?}"
    );
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
    let (linux, _lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
    let with = |name: &str| vec![(name.to_owned(), "/tmp/elsewhere".to_owned())];
    let terminal = |env| Verb::OpenTerminal {
        worker: linux,
        cwd: None,
        command: Vec::new(),
        env,
        name: None,
        size: None,
        session: None,
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
    };
    for (verb, name) in [
        (terminal(with("PATH")), "PATH"),
        (terminal(with("zdotdir")), "zdotdir"),
        (terminal(with("DYLD_INSERT_LIBRARIES")), "DYLD_INSERT_LIBRARIES"),
        (agent(with("CLAUDE_CONFIG_DIR")), "CLAUDE_CONFIG_DIR"),
        (agent(with("NODE_OPTIONS")), "NODE_OPTIONS"),
        (Verb::TaskSpawn { project: project(), task, launch: claude(&[]) }, "SLOPTY_TASK"),
    ] {
        let said = hub.dispatch_as(Speaker::Agent, None, verb).await;
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

/// A terminal put on a task takes a place under the project's limits as a start does, so an
/// agent cannot run more than the project allows by assigning terminals opened elsewhere; one
/// the project counts already takes no more. A terminal being started for one task is not
/// taken by another.
#[tokio::test]
async fn an_assign_takes_a_place_under_the_project_s_limits() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (worker, lease, mut rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    let one = LimitsChange { live_per_project: Some(1), ..LimitsChange::default() };
    create_with(&hub, None, one).await;
    let (first, second) =
        (new_task(&hub, Placement::default()).await, new_task(&hub, Placement::default()).await);
    let asked =
        spawn(&hub, Verb::TaskSpawn { project: project(), task: first, launch: claude(&[]) });
    let start = request(&mut rx).await;
    let (_, starting) = chosen(&start.1);
    let starting = TermRef { worker, session: starting };
    let hijack = Verb::TaskAssign { project: project(), task: second, term: starting };
    assert!(refused(&hub.dispatch(hijack).await, ErrorCode::Conflict).contains("being started"));
    let counted = opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));

    let elsewhere = SessionId::new();
    announce(&lease, elsewhere, true);
    let elsewhere = TermRef { worker, session: elsewhere };
    let past = Verb::TaskAssign { project: project(), task: second, term: elsewhere };
    assert!(refused(&hub.dispatch(past).await, ErrorCode::Limit).contains("live_per_project"));
    let again = Verb::TaskAssign { project: project(), task: first, term: counted };
    assert!(matches!(hub.dispatch(again).await, Outcome::Task(_)), "counted already");
}
