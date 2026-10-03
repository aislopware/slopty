//! What a task's agent came to reaches the node above it through its worker, with no report
//! from the agent; a finished task's agent stops counting and is closed once it rests; and an
//! agent tells the tasks under it in its own words.

use std::time::Duration;

use slopty_proto::agent::BlockReason;
use slopty_proto::project::{
    LimitsChange, Moment, Placement, Report, ReportKind, TaskChange, TaskSpec, TaskState,
};
use slopty_proto::server::Os;
use slopty_proto::thread::Phase;
use slopty_proto::thread::attention::Seat;

use super::ladder::tests::{Client, asking, row, snapshot};
use super::project_tests::{
    agent, announce, claude, create, create_with, new_task, opened, project, refused, request,
    spawn, status, worker_on,
};
use super::settle::SETTLE_AFTER;
use super::*;

/// The next batch the worker's link is sent, skipping everything else: paused, the clock runs
/// on to it.
async fn next_batch(rx: &mut mpsc::Receiver<FromServer>) -> (SessionId, u64, String) {
    loop {
        match tokio::time::timeout(Duration::from_mins(10), rx.recv()).await {
            Ok(Some(FromServer::Deliver { session, batch, context })) => {
                return (session, batch, context);
            }
            Ok(Some(_)) => {}
            other => panic!("no batch: {other:?}"),
        }
    }
}

/// Whether a batch comes within `wait`.
async fn a_batch_within(rx: &mut mpsc::Receiver<FromServer>, wait: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now().checked_add(wait)?;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(FromServer::Deliver { context, .. })) => return Some(context),
            Ok(Some(_)) => {}
            _ => return None,
        }
    }
}

/// A task's agent that ends its turn with no `task_report` is heard of by the orchestrator once
/// the rest settles, with its last words from its thread; one that waits on the person is
/// heard of with what it asks, once the wait outlasts a moment, and never asked to answer it;
/// its own report stands in place of the server's word; and its exit is heard at once.
#[tokio::test(start_paused = true)]
async fn a_task_s_outcome_reaches_the_orchestrator_without_a_report() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (orchestrating, working) = (SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrating, true);
    announce(&lease, working, false);
    let orchestrator = TermRef { worker, session: orchestrating };
    create(&hub, Some(orchestrator)).await;
    let ack = |(session, batch, _): &(SessionId, u64, String)| {
        lease.handle(ToServer::Report(AgentReport::Delivered { session: *session, batch: *batch }));
    };
    let role = next_batch(&mut rx).await;
    ack(&role);
    let task = new_task(&hub, Placement::default()).await;
    let term = TermRef { worker, session: working };
    let assigned = hub.dispatch(Verb::TaskAssign { project: project(), task, term }).await;
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");

    let mut thread = row(Phase::Idle, 1, Some(working));
    thread.last_line = Some("Tests take 4 s; they pass.".to_owned());
    lease.handle(agent(working, AgentStatus::Working));
    lease.handle(agent(working, AgentStatus::Idle));
    // The row with its final line comes after the status that ended the turn.
    lease.handle(snapshot(vec![thread.clone()]));
    let rested = next_batch(&mut rx).await;
    assert_eq!(rested.0, orchestrating);
    assert!(rested.2.contains("task 1 ended its turn without a task_report"), "{}", rested.2);
    assert!(rested.2.contains("Its last words:\n  Tests take 4 s; they pass."), "{}", rested.2);
    ack(&rested);
    let delivered = status(&hub).await.timeline.into_iter().filter(
        |e| matches!(e.what, Moment::Delivered { term, reports: 1 } if term == orchestrator),
    );
    assert_eq!(delivered.count(), 1, "the timeline says it reached the orchestrator");
    let rests = async || {
        let rested = Moment::State { from: TaskState::Running, to: TaskState::Waiting };
        status(&hub).await.timeline.iter().filter(|e| e.what == rested).count()
    };
    assert_eq!(rests().await, 1, "the timeline says the turn ended unreported");

    let bash = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    lease.handle(agent(working, AgentStatus::Working));
    lease.handle(agent(working, bash.clone()));
    lease.handle(agent(working, AgentStatus::Working));
    assert_eq!(a_batch_within(&mut rx, Duration::from_mins(5)).await, None, "answered at once");
    lease.handle(agent(working, bash));
    // The request's card comes after the status that says it waits.
    lease.handle(snapshot(vec![asking(thread.clone(), "Run cargo test?")]));
    let waits = next_batch(&mut rx).await;
    assert!(waits.2.contains("task 1 waits on the person: Run cargo test?"), "{}", waits.2);
    assert!(waits.2.contains("Only the person answers it"), "{}", waits.2);
    ack(&waits);

    lease.handle(snapshot(vec![thread]));
    lease.handle(agent(working, AgentStatus::Working));
    let report = Report {
        kind: ReportKind::NeedsInput,
        note: "Which crate owns the store?".to_owned(),
        artifacts: Vec::new(),
        branch: None,
        pr: None,
    };
    let verb = Verb::TaskReport { project: project(), task, report };
    let said = hub.dispatch_as(Speaker::Proven(working), None, verb).await;
    assert!(matches!(said, Outcome::Task(_)), "{said:?}");
    lease.handle(agent(working, AgentStatus::Idle));
    let own = next_batch(&mut rx).await;
    assert!(own.2.contains("task 1: needs input"), "{}", own.2);
    assert!(!own.2.contains("waits on the person"), "taken back: {}", own.2);
    ack(&own);
    assert_eq!(a_batch_within(&mut rx, Duration::from_mins(5)).await, None, "its own word");
    assert_eq!(rests().await, 1, "a turn that reported is not marked");

    lease.handle(ToServer::SessionClosed {
        session: working,
        reason: slopty_proto::terminal::CloseReason::Exited,
    });
    let gone = next_batch(&mut rx).await;
    assert!(gone.2.contains("task 1's agent exited, or its terminal was closed"), "{}", gone.2);
    assert!(gone.2.contains("Tests take 4 s; they pass."), "{}", gone.2);
    delivering.abort();
}

/// The agent the server started for a task the person merged counts against no limit once it
/// rests, and counts again while it works. Once it has rested long enough with its tile on no
/// client's screen, the server closes its terminal and the timeline says why; on screen, or
/// at work, it is left alone.
#[tokio::test]
async fn a_finished_task_s_agent_stops_counting_and_is_closed_once_it_rests() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    let one = LimitsChange { live_per_project: Some(1), ..LimitsChange::default() };
    create_with(&hub, None, one).await;
    let task = new_task(&hub, Placement::default()).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    let start = request(&mut rx).await;
    let term = opened(&lease, &start);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    lease.handle(agent(term.session, AgentStatus::Idle));
    lease.handle(agent(term.session, AgentStatus::Working));
    for state in [TaskState::Done, TaskState::Merged] {
        let change = Box::new(TaskChange { state: Some(state), ..TaskChange::default() });
        let moved = hub.dispatch(Verb::TaskUpdate { project: project(), task, change }).await;
        assert!(matches!(moved, Outcome::Task(_)), "{moved:?}");
    }
    assert_eq!(status(&hub).await.live.project, 1, "merged, but at work");
    let t0 = tokio::time::Instant::now();
    let later = |from: tokio::time::Instant| from.checked_add(SETTLE_AFTER).unwrap();
    let mut resting = HashMap::new();
    assert_eq!(hub.settle_due(&mut resting, later(t0)), []);

    lease.handle(agent(term.session, AgentStatus::Idle));
    assert_eq!(status(&hub).await.live.project, 0, "at rest, it counts no more");
    let desk = Client::sit(&hub, "mac");
    desk.at(&hub, Seat::Desk, false, vec![term]);
    assert_eq!(hub.settle_due(&mut resting, t0), []);
    assert_eq!(hub.settle_due(&mut resting, later(t0)), [], "its tile is on screen");

    desk.at(&hub, Seat::Desk, true, Vec::new());
    let t1 = later(t0);
    assert_eq!(hub.settle_due(&mut resting, t1), [], "the wait starts again");
    let almost = later(t1).checked_sub(Duration::from_secs(1)).unwrap();
    assert_eq!(hub.settle_due(&mut resting, almost), []);
    assert_eq!(hub.settle_due(&mut resting, later(t1)), [term]);
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term });
    let said = status(&hub).await.timeline.into_iter().rev().find_map(|e| match e.what {
        Moment::Note { text } if e.task == Some(task) => Some(text),
        _ => None,
    });
    let said = said.unwrap_or_default();
    assert!(said.starts_with("The server closed its agent, at rest 10 min after"), "{said}");
}

/// The orchestrator tells a task's agent something, and a task's agent tells a task split from
/// its own: each reaches that agent through its hooks, marked as an agent's words, and the
/// timeline says who told. An agent never tells a task outside its own subtree, the node
/// above it, or a task that waits on the person, and an agent's surface that proves no
/// terminal tells nothing.
#[tokio::test]
async fn an_agent_tells_the_tasks_under_it_in_its_own_words() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (orchestrating, above, below, beside) =
        (SessionId::new(), SessionId::new(), SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrating, true);
    for session in [above, below, beside] {
        announce(&lease, session, false);
    }
    create(&hub, Some(TermRef { worker, session: orchestrating })).await;
    let role = next_batch(&mut rx).await;
    lease.handle(ToServer::Report(AgentReport::Delivered { session: role.0, batch: role.1 }));
    let parent = new_task(&hub, Placement::default()).await;
    let spec = TaskSpec { title: "Child".to_owned(), parent: Some(parent), ..TaskSpec::default() };
    let made = hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await;
    let Outcome::Task(child) = made else { panic!("{made:?}") };
    let sibling = new_task(&hub, Placement::default()).await;
    for (task, session) in [(parent, above), (child.id, below), (sibling, beside)] {
        let term = TermRef { worker, session };
        let assigned = hub.dispatch(Verb::TaskAssign { project: project(), task, term }).await;
        assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    }
    let tell = |who, task, text: &str| {
        let verb = Verb::TaskTell { project: project(), task, text: text.to_owned() };
        hub.dispatch_as(who, None, verb)
    };

    let said = tell(Speaker::Proven(orchestrating), Some(parent), "Also cover the iPad.").await;
    assert_eq!(said, Outcome::Done);
    let (session, batch, context) = next_batch(&mut rx).await;
    assert_eq!(session, above);
    assert!(
        context.contains("Your orchestrator says (an agent, not the person; it answers nothing")
            && context.contains("  Also cover the iPad."),
        "{context}"
    );
    lease.handle(ToServer::Report(AgentReport::Delivered { session, batch }));
    let said = tell(Speaker::Proven(above), Some(child.id), "Start with the layout.").await;
    assert_eq!(said, Outcome::Done);
    let (session, _, context) = next_batch(&mut rx).await;
    assert_eq!(session, below);
    assert!(context.contains("The agent of task 1, which split your task off, says"), "{context}");

    for (who, task, why) in [
        (Speaker::Proven(above), Some(sibling), "not split from its task"),
        (Speaker::Proven(above), None, "the node above it"),
        (Speaker::Proven(below), Some(parent), "its parent"),
        (Speaker::Agent, Some(child.id), "proves no terminal"),
    ] {
        let said = tell(who, task, "Stop.").await;
        refused(&said, ErrorCode::Forbidden);
        assert!(matches!(said, Outcome::Error { .. }), "{why}");
    }
    lease.handle(agent(below, AgentStatus::Blocked(BlockReason::Question)));
    let said = tell(Speaker::Proven(orchestrating), Some(child.id), "Pick the first.").await;
    assert!(refused(&said, ErrorCode::Conflict).contains("waits on the person"), "{said:?}");

    let notes: Vec<String> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Note { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        notes,
        [
            "The orchestrator told it: Also cover the iPad.",
            "Task 1's agent told it: Start with the layout."
        ]
    );
    delivering.abort();
}
