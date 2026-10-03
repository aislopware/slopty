//! Any agent runs a task as a thread of its worker's thread host: placed where it is
//! installed, started at a seat the server chose, known by that seat when its tools speak, its
//! state following its thread's row, and ended when the row goes.

use slopty_proto::project::{Placement, Runner, SEAT_FACT, TASK_ENV, TaskLaunch, TaskState};
use slopty_proto::server::Os;
use slopty_proto::thread::wire::TableFrame;
use slopty_proto::thread::{AgentId, Cursor, Phase, ThreadId};

use super::ladder::tests::{asking, row, snapshot};
use super::project_tests::{
    answer, claude, create, installed, new_task, project, refused, request, spawn, status,
    worker_on,
};
use super::*;

/// A launch of `agent` as a thread, in a folder of its own so it needs no clone.
fn as_thread(agent: AgentId, args: &[&str]) -> TaskLaunch {
    let run = Runner::Agent {
        agent,
        prompt: Some("Read your brief.".to_owned()),
        model: Some("sonnet".to_owned()),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
    };
    TaskLaunch { run, ..claude(&[]) }
}

/// The card of `task`, as the project's status shows it now.
async fn card(hub: &Hub, task: TaskId) -> slopty_proto::project::TaskCard {
    status(hub).await.tasks.into_iter().find(|t| t.id == task).expect("the card")
}

/// pi goes only where it is installed, with no flag the server cannot judge, and starts as a
/// thread seated where the server chose: its role and the task's variables go with it. Its
/// tools are known by that seat, it counts as a live agent once its row is in the table, its
/// task follows the row's phase (a request open blocks it on the person), and the row gone
/// ends its assignment. An ACP agent no worker has is no start.
#[tokio::test]
async fn any_agent_runs_a_task_as_a_thread() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_mac, mac_lease, _mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    mac_lease.handle(ToServer::Facts(installed(&["claude"])));
    linux_lease.handle(ToServer::Facts(installed(&["pi"])));
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;
    let spawn_verb = |launch| Verb::TaskSpawn { project: project(), task, launch };
    let pi = || AgentId::named(AgentId::PI);

    let gemini = hub.dispatch(spawn_verb(as_thread(AgentId::acp("gemini"), &[]))).await;
    let message = refused(&gemini, ErrorCode::Unplaced);
    assert!(message.contains("box: gemini is not installed"), "{message}");
    let loose = hub.dispatch(spawn_verb(as_thread(pi(), &["--yolo"]))).await;
    let message = refused(&loose, ErrorCode::Limit);
    assert!(message.starts_with("--yolo may give"), "{message}");

    let asked = spawn(&hub, spawn_verb(as_thread(pi(), &[])));
    let (id, verb) = request(&mut linux_rx).await;
    let Verb::StartThread { worker, start, seat, env, role, worktree } = verb else {
        panic!("{verb:?}")
    };
    assert_eq!(worker, linux);
    assert_eq!((start.agent.clone(), start.model.as_deref()), (pi(), Some("sonnet")));
    assert_eq!(start.cwd, "~/src/slopty");
    assert_eq!(worktree, None, "a named folder: no worktree");
    assert!(env.iter().any(|(k, v)| k == TASK_ENV && *v == task.to_string()), "{env:?}");
    assert!(role.is_some_and(|r| r.starts_with("You are the agent of task")));
    let thread = ThreadId::new();
    answer(&linux_lease, id, Outcome::ThreadStarted { thread, worktree: None });
    let Outcome::Task(started) = asked.await.unwrap() else { panic!("not a task") };
    let assignment = started.assignment.expect("assigned");
    let term = TermRef { worker: linux, session: seat };
    assert_eq!((assignment.term, assignment.thread), (term, Some(thread)));
    let mine = hub.dispatch(Verb::WorkingOn { session: seat }).await;
    assert_eq!(mine, Outcome::WorkingOn(Some((project(), Some(task)))), "its tools are known");

    let mut seated = row(Phase::Working, 1, None);
    seated.id = thread;
    seated.agent = pi();
    seated.facts.insert(SEAT_FACT.to_owned(), seat.to_string());
    linux_lease.handle(snapshot(vec![seated.clone()]));
    let now = status(&hub).await;
    assert_eq!(now.live.project, 1, "it counts as a live agent");
    assert_eq!(card(&hub, task).await.state, TaskState::Running);

    let mut needs = asking(seated.clone(), "Run cargo test?");
    needs.status.phase = Phase::NeedsYou;
    needs.requests[0].kind = slopty_proto::thread::Request::APPROVAL.to_owned();
    linux_lease.handle(snapshot(vec![needs]));
    assert_eq!(card(&hub, task).await.state, TaskState::Blocked, "it waits on the person");
    let mut rests = seated.clone();
    rests.status.phase = Phase::Idle;
    linux_lease.handle(snapshot(vec![rests]));
    assert_eq!(card(&hub, task).await.state, TaskState::Waiting);

    let gone =
        TableFrame::Delta { cursor: Cursor::default(), rows: Vec::new(), removed: vec![thread] };
    linux_lease.handle(ToServer::Threads(gone));
    let ended = card(&hub, task).await.assignment.expect("still named");
    assert!(ended.ended_ms.is_some(), "its row gone, its assignment ended");
    assert_eq!(status(&hub).await.live.project, 0);
}
