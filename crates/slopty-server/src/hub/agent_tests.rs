//! The agents' verbs on the hub, against a worker the test answers for.

use std::time::Duration;

use slopty_proto::agent::{AgentStart, PARENT_FACT, SEAT_FACT};
use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{Phase, ThreadId};
use slopty_settings::Autonomy;

use super::ladder::tests::{row, snapshot, started_by};
use super::tests::{registration, summary};
use super::*;

/// A hub with one worker holding the terminal `shell`, where the parent's agent runs.
struct Fleet {
    hub: Hub,
    worker: WorkerId,
    lease: Lease,
    rx: mpsc::Receiver<FromServer>,
    shell: SessionId,
    parent: ThreadRow,
}

impl Fleet {
    fn new() -> Self {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let (worker, shell) = (WorkerId::new(), SessionId::new());
        let (tx, rx) = mpsc::channel(64);
        let lease = hub
            .register(registration(worker, vec![summary(shell)]), [100, 64, 0, 7].into(), tx)
            .unwrap();
        let parent = row(Phase::Working, 1_000, Some(shell));
        lease.handle(snapshot(vec![parent.clone()]));
        Self { hub, worker, lease, rx, shell, parent }
    }

    /// The parent's agent's verb, as its tools send it from its terminal.
    async fn parent_says(&self, verb: Verb) -> Outcome {
        self.hub.dispatch_as(Speaker::Proven(self.shell), None, verb).await
    }

    /// The next request the worker is sent.
    async fn request(&mut self) -> (RequestId, Option<IdempotencyKey>, Verb) {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.rx.recv()).await {
                Ok(Some(FromServer::Request { id, key, verb })) => return (id, key, verb),
                Ok(Some(_)) => {}
                other => panic!("no request: {other:?}"),
            }
        }
    }
}

fn start(worker: WorkerId) -> AgentStart {
    AgentStart {
        worker,
        folder: Some("/src/app".to_owned()),
        prompt: "Fix the login".to_owned(),
        agent: None,
        model: None,
        effort: None,
        worktree: Some("login".to_owned()),
        repo: None,
    }
}

/// An agent's start names its parent, takes its mode from the person's autonomy setting and
/// no harness arguments of the agent's, and a repeat under its key takes the same seat.
#[tokio::test]
async fn a_start_names_its_parent_and_takes_the_person_s_autonomy() {
    let mut fleet = Fleet::new();
    fleet.hub.set_autonomy(Autonomy::Full);
    let key = IdempotencyKey::new("start-1").unwrap();
    let mut seats = Vec::new();
    for _ in 0..2 {
        let asked = tokio::spawn({
            let (hub, shell, key, start) =
                (fleet.hub.clone(), fleet.shell, key.clone(), start(fleet.worker));
            async move {
                hub.dispatch_as(
                    Speaker::Proven(shell),
                    Some(key),
                    Verb::AgentStart(Box::new(start)),
                )
                .await
            }
        });
        let (id, sent, verb) = fleet.request().await;
        assert_eq!(sent, Some(key.clone()), "the key goes down with it");
        let Verb::StartThread { worker, start, seat, facts, system } = verb else {
            panic!("{verb:?}")
        };
        assert_eq!(worker, fleet.worker);
        assert_eq!(
            start.mode.as_deref(),
            Some("bypassPermissions"),
            "full, in Claude Code's words"
        );
        assert!(start.args.is_empty(), "no harness arguments");
        assert_eq!(start.worktree.map(|w| w.name), Some("login".to_owned()));
        assert_eq!(facts, [(PARENT_FACT.to_owned(), fleet.parent.id.to_string())]);
        assert!(system.is_some_and(|s| s.contains(&fleet.parent.id.to_string())), "one line");
        seats.push(seat);
        let child = ThreadId::new();
        let outcome = Outcome::ThreadStarted { thread: child, worktree: None };
        fleet.lease.handle(ToServer::Reply { id, outcome: outcome.clone() });
        assert_eq!(asked.await.unwrap(), outcome);
    }
    assert_eq!(seats[0], seats[1], "a repeat takes the first seat");

    fleet.hub.set_autonomy(Autonomy::Ask);
    let asked = tokio::spawn({
        let (hub, start) = (fleet.hub.clone(), start(fleet.worker));
        async move { hub.dispatch(Verb::AgentStart(Box::new(start))).await }
    });
    let (id, _, verb) = fleet.request().await;
    let Verb::StartThread { start, facts, system, .. } = verb else { panic!("{verb:?}") };
    assert_eq!(start.mode, None, "ask is the harness's own default");
    assert!(facts.is_empty() && system.is_none(), "the person's start has no parent");
    fleet.lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
    let _answered = asked.await;
}

/// A wait on a child ends once it comes to rest; words a parent tells it go to the worker
/// that holds it, marked as an agent's, for the worker to type into its terminal.
#[tokio::test]
async fn a_child_s_rest_ends_a_wait_and_told_words_go_to_its_worker() {
    let mut fleet = Fleet::new();
    let mut child = started_by(row(Phase::Working, 2_000, None), &fleet.parent);
    child.facts.insert(SEAT_FACT.to_owned(), SessionId::new().to_string());
    child.title = "login fixer".to_owned();
    fleet.lease.handle(snapshot(vec![fleet.parent.clone(), child.clone()]));

    let waiting = tokio::spawn({
        let (hub, shell, id) = (fleet.hub.clone(), fleet.shell, child.id);
        async move {
            let verb = Verb::AgentWait { agents: vec![id], all: true, timeout_ms: 60_000 };
            hub.dispatch_as(Speaker::Proven(shell), None, verb).await
        }
    });
    tokio::task::yield_now().await;
    let rested = ThreadRow {
        status: slopty_proto::thread::Status { phase: Phase::Idle, ..child.status.clone() },
        ended: Some(slopty_proto::thread::wire::TurnEnded {
            turn: slopty_proto::thread::TurnId::BEFORE,
            at_ms: WallMs::from_millis(3_000),
            ran_ms: 1_000,
            answered: true,
        }),
        last_line: Some("Fixed it on worktree-login.".to_owned()),
        ..child.clone()
    };
    fleet.lease.handle(snapshot(vec![fleet.parent.clone(), rested]));
    let Outcome::Agents { agents, timed_out } = waiting.await.unwrap() else { panic!() };
    assert!(!timed_out);
    assert_eq!((agents[0].id, agents[0].parent), (child.id, Some(fleet.parent.id)));
    assert!(agents[0].rests);

    let telling = tokio::spawn({
        let (hub, shell, id) = (fleet.hub.clone(), fleet.shell, child.id);
        async move {
            let tell = Verb::AgentTell { agent: id, text: "Now the logout too.".to_owned() };
            hub.dispatch_as(Speaker::Proven(shell), None, tell).await
        }
    });
    let (id, _, verb) = fleet.request().await;
    let Verb::AgentTell { agent, text } = verb else { panic!("{verb:?}") };
    assert_eq!(agent, child.id, "for the worker that holds the child");
    assert!(text.contains("an agent, not the person") && text.contains("Now the logout"), "{text}");
    fleet.lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
    assert_eq!(telling.await.unwrap(), Outcome::Done);
}

/// An agent tells only its own tree and stops only itself and what it started, each one at
/// work stopped on its worker, deepest first.
#[tokio::test]
async fn an_agent_reaches_only_its_own_tree() {
    let mut fleet = Fleet::new();
    let stranger = row(Phase::Working, 1_000, Some(SessionId::new()));
    let child = started_by(row(Phase::Working, 2_000, Some(SessionId::new())), &fleet.parent);
    let grandchild = started_by(row(Phase::Working, 3_000, Some(SessionId::new())), &child);
    let rows = vec![fleet.parent.clone(), stranger.clone(), child.clone(), grandchild.clone()];
    fleet.lease.handle(snapshot(rows));

    let tell = Verb::AgentTell { agent: stranger.id, text: "hi".to_owned() };
    let refused = fleet.parent_says(tell).await;
    assert!(matches!(refused, Outcome::Error { code: ErrorCode::Forbidden, .. }), "{refused:?}");
    let stop = Verb::AgentStop { agent: stranger.id };
    let refused = fleet.parent_says(stop).await;
    assert!(matches!(refused, Outcome::Error { code: ErrorCode::Forbidden, .. }), "{refused:?}");
    let unknown = fleet.parent_says(Verb::AgentStop { agent: ThreadId::new() }).await;
    assert!(matches!(unknown, Outcome::Error { code: ErrorCode::UnknownAgent, .. }), "{unknown:?}");

    let stopping = tokio::spawn({
        let (hub, shell, id) = (fleet.hub.clone(), fleet.shell, child.id);
        async move { hub.dispatch_as(Speaker::Proven(shell), None, Verb::AgentStop { agent: id }).await }
    });
    let mut stopped = Vec::new();
    for _ in 0..2 {
        let (id, _, verb) = fleet.request().await;
        let Verb::AgentStop { agent } = verb else { panic!("{verb:?}") };
        stopped.push(agent);
        fleet.lease.handle(ToServer::Reply { id, outcome: Outcome::Done });
    }
    assert_eq!(stopping.await.unwrap(), Outcome::Done);
    assert_eq!(stopped, [grandchild.id, child.id], "deepest first, each an Esc on its worker");

    let Outcome::Agents { agents, .. } = fleet.hub.dispatch(Verb::Agents).await else { panic!() };
    let parent_of = |id| agents.iter().find(|a| a.id == id).and_then(|a| a.parent);
    assert_eq!(agents.len(), 4);
    assert_eq!(parent_of(grandchild.id), Some(child.id), "a tree at any depth");
}

/// A terminal whose agent runs stays an agent's though a session it ran before ended later
/// (cut short as its worker restarted): the shell there is not the person's, and no agent
/// types into its TUI.
#[tokio::test]
async fn an_ended_session_does_not_hide_the_agent_at_its_terminal() {
    let fleet = Fleet::new();
    let mut ended = row(Phase::Stopped, 2_000, Some(fleet.shell));
    ended.status.liveness = slopty_proto::thread::Liveness::Exited { resumable: true };
    fleet.lease.handle(snapshot(vec![fleet.parent.clone(), ended]));
    assert_eq!(fleet.hub.caller(Speaker::Shell(fleet.shell)), Caller::Agent);
    let term = TermRef { worker: fleet.worker, session: fleet.shell };
    assert!(fleet.hub.types_into_shell(term).is_err(), "an agent's TUI takes no agent's keys");
    assert_eq!(fleet.hub.inner.state.lock().board.thread_at(term), Some(fleet.parent.id));
}
