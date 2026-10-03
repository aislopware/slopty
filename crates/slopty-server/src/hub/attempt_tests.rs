//! A task tried by several agents at once through the hub: the attempts spread over the
//! workers, an attempt's agent picks none, and the pick closes the others and frees their
//! worktrees.

use slopty_proto::agent::Worktree;
use slopty_proto::project::{ATTEMPT_KIND, Placement, Runner, SEAT_FACT, TaskLaunch, TaskState};
use slopty_proto::server::Os;
use slopty_proto::thread::{AgentId, Phase, ThreadId};

use super::ladder::tests::{row, snapshot};
use super::project_tests::{
    answer, claude, create, installed, new_task, project, refused, request, spawn, status,
    worker_on,
};
use super::*;

/// pi, as a thread.
fn pi() -> TaskLaunch {
    let run = Runner::Agent {
        agent: AgentId::named(AgentId::PI),
        prompt: Some("Read your brief.".to_owned()),
        model: None,
        args: Vec::new(),
    };
    TaskLaunch { run, ..claude(&[]) }
}

/// The worker starts the thread it was asked for in a worktree of its own, its row seated in
/// the worker's table; answers its seat and the worktree's path.
fn started(lease: &Lease, (id, verb): (RequestId, Verb)) -> (TermRef, String) {
    let Verb::StartThread { worker, seat, .. } = verb else { panic!("{verb:?}") };
    let path = format!("/w/slopty/.worktrees/{seat}");
    let worktree = Worktree {
        name: seat.to_string(),
        path: path.clone(),
        branch: Some(format!("worktree-{seat}")),
        original_cwd: "~/src/slopty".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let thread = ThreadId::new();
    answer(lease, id, Outcome::ThreadStarted { thread, worktree: Some(Box::new(worktree)) });
    let mut seated = row(Phase::Working, 1, None);
    seated.id = thread;
    seated.agent = AgentId::named(AgentId::PI);
    seated.facts.insert(SEAT_FACT.to_owned(), seat.to_string());
    lease.handle(snapshot(vec![seated]));
    (TermRef { worker, session: seat }, path)
}

/// Two attempts at a task go one to each worker. The task itself starts no agent, an
/// attempt's agent may not pick, and an attempt not picked does not merge. The person's pick
/// closes the other attempt's agent and frees its worktree, keeping what did not land, and
/// says on its card why it was given up; the worker of the one picked hears nothing.
#[tokio::test]
async fn a_task_is_tried_on_each_machine_and_the_attempt_picked_is_kept() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mac, mac_lease, mut mac_rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let (linux, linux_lease, mut linux_rx) = worker_on(&hub, "box", Os::Linux, Vec::new());
    mac_lease.handle(ToServer::Facts(installed(&["pi"])));
    linux_lease.handle(ToServer::Facts(installed(&["pi"])));
    create(&hub, None).await;
    let task = new_task(&hub, Placement::default()).await;

    let verb = Verb::TaskAttempts { project: project(), task, launches: vec![pi(), pi()] };
    let asked = spawn(&hub, verb);
    let (on_mac, _) = started(&mac_lease, request(&mut mac_rx).await);
    let (on_linux, _) = started(&linux_lease, request(&mut linux_rx).await);
    let answered = asked.await.unwrap();
    let Outcome::Task(tried) = answered else { panic!("{answered:?}") };
    let attempts = tried.attempts.expect("its attempts");
    assert_eq!((attempts.tried.len(), attempts.picked), (2, None));
    assert_eq!(tried.state, TaskState::Running);
    let cards = status(&hub).await.tasks;
    let at = |term: TermRef| {
        let card = cards.iter().find(|c| c.assignment.as_ref().is_some_and(|a| a.term == term));
        let card = card.expect("an attempt on each worker");
        assert_eq!((card.kind.as_str(), card.parent), (ATTEMPT_KIND, Some(task)));
        card.id
    };
    let (lost, kept) = (at(on_mac), at(on_linux));
    assert_eq!((on_mac.worker, on_linux.worker), (mac, linux));

    let itself = hub.dispatch(Verb::TaskSpawn { project: project(), task, launch: pi() }).await;
    assert!(refused(&itself, ErrorCode::Invalid).contains("is tried by attempts"));
    let pick = |attempt| Verb::TaskPick { project: project(), attempt };
    let by_an_attempt = hub.dispatch_as(Speaker::Proven(on_mac.session), None, pick(kept)).await;
    let said = refused(&by_an_attempt, ErrorCode::Forbidden);
    assert!(said.contains("neither it nor split from it"), "{said}");
    let merge = hub.dispatch(Verb::TaskMerge { project: project(), task: lost }).await;
    assert!(refused(&merge, ErrorCode::Invalid).contains("not picked"));

    let Outcome::Task(picked) = hub.dispatch(pick(kept)).await else { panic!("not a task") };
    assert_eq!(picked.attempts.and_then(|a| a.picked), Some(kept));
    let (id, close) = request(&mut mac_rx).await;
    assert_eq!(close, Verb::Close { term: on_mac }, "the attempt given up is closed");
    answer(&mac_lease, id, Outcome::Done);
    let (id, remove) = request(&mut mac_rx).await;
    let Verb::RemoveWorktree { worker, worktree, landed } = remove else { panic!("{remove:?}") };
    assert_eq!(
        (worker, worktree.as_str()),
        (mac, format!("/w/slopty/.worktrees/{}", on_mac.session).as_str())
    );
    assert_eq!(landed, ["main", "origin/main"], "its branch is kept unless it landed");
    answer(
        &mac_lease,
        id,
        Outcome::WorktreeRemoved { branch: Some("b".to_owned()), branch_removed: false },
    );
    let card = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let card = status(&hub).await.tasks.into_iter().find(|t| t.id == lost).unwrap();
            if card.worktree.is_none() {
                return card;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("its worktree freed");
    assert_eq!(card.state, TaskState::Failed);
    assert!(card.status.is_some_and(|s| s.contains(&format!("attempt {kept} lands"))));
    assert!(linux_rx.try_recv().is_err(), "the attempt picked goes on");
}
