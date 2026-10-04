//! What a task's agent came to reaches the orchestrator through its worker, with no report from
//! the agent; a finished task's agent stops counting and is closed once it rests, a merged one's
//! worktree going after it; and the orchestrator tells its tasks in its own words.

use std::time::Duration;

use slopty_proto::agent::{AgentBranch, BlockReason, Worktree};
use slopty_proto::project::{Moment, Report, TaskChange, TaskState, TimelineEntry};
use slopty_proto::server::Os;
use slopty_proto::thread::Phase;
use slopty_proto::thread::attention::Seat;

use super::ladder::tests::{Client, asking, row, snapshot};
use super::project_tests::{
    agent, announce, answer, claude, create, new_task, opened, project, refused, request, spawn,
    status, worker_again, worker_on,
};
use super::settle::SETTLE_AFTER;
use super::tests::summary;
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
    let task = new_task(&hub, None).await;
    let term = TermRef { worker, session: working };
    let assigned = hub.assign_for_test(&project(), task, term);
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
        note: "The store keeps every project.".to_owned(),
        artifacts: Vec::new(),
        branch: None,
        pr: None,
    };
    let verb = Verb::TaskReport { project: project(), task, report };
    let said = hub.dispatch_as(Speaker::Proven(working), None, verb).await;
    assert!(matches!(said, Outcome::Task(_)), "{said:?}");
    lease.handle(agent(working, AgentStatus::Idle));
    let own = next_batch(&mut rx).await;
    assert!(own.2.contains("task 1: done\n  The store keeps every project."), "{}", own.2);
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

/// A task's agent that was at work when the server stopped, and came to rest while it was
/// away, is still heard of by the orchestrator once its worker is back and says so.
#[tokio::test(start_paused = true)]
async fn a_turn_that_ended_while_the_server_was_away_reaches_the_orchestrator() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, working) = (SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrating, true);
    announce(&lease, working, false);
    let orchestrator = TermRef { worker, session: orchestrating };
    create(&hub, Some(orchestrator)).await;
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let role = next_batch(&mut rx).await;
    lease.handle(ToServer::Report(AgentReport::Delivered { session: role.0, batch: role.1 }));
    let task = new_task(&hub, None).await;
    let assigned = hub.assign_for_test(&project(), task, TermRef { worker, session: working });
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    lease.handle(agent(working, AgentStatus::Working));

    let (file, known) = (hub.projects_file(0), hub.directory());
    delivering.abort();
    drop((lease, rx, hub));
    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let sessions = vec![summary(orchestrating), summary(working)];
    let (_, lease, mut rx) = worker_again(&hub, worker, "studio", Os::MacOs, sessions);
    announce(&lease, orchestrating, true);
    lease.handle(agent(working, AgentStatus::Idle));
    let rested = next_batch(&mut rx).await;
    assert_eq!(rested.0, orchestrating);
    assert!(rested.2.contains("task 1 ended its turn without a task_report"), "{}", rested.2);
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
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
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

/// A merged task's agent that waits only on a command it left running (a dev server) rests as
/// one at its prompt does: once it has long enough, its terminal closes, stopping the command,
/// and the timeline names what was stopped. One that waits on anything else of its own is left.
#[tokio::test]
async fn a_finished_task_s_agent_that_left_a_command_running_still_settles() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    create(&hub, None).await;
    let task = new_task(&hub, None).await;
    let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
    let term = opened(&lease, &request(&mut rx).await);
    assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
    for state in [TaskState::Done, TaskState::Merged] {
        let change = Box::new(TaskChange { state: Some(state), ..TaskChange::default() });
        let moved = hub.dispatch(Verb::TaskUpdate { project: project(), task, change }).await;
        assert!(matches!(moved, Outcome::Task(_)), "{moved:?}");
    }
    lease.handle(agent(term.session, AgentStatus::Waiting { tasks: 1, crons: 0 }));
    let waiting = |kind: &str| {
        let mut thread = row(Phase::Waiting, 1, Some(term.session));
        thread.status.wait = Some(slopty_proto::thread::Wait {
            kind: kind.to_owned(),
            text: "npm run dev".to_owned(),
        });
        snapshot(vec![thread])
    };
    let t0 = tokio::time::Instant::now();
    let later = |from: tokio::time::Instant| from.checked_add(SETTLE_AFTER).unwrap();
    let mut resting = HashMap::new();
    lease.handle(waiting("task"));
    assert_eq!(hub.settle_due(&mut resting, t0), []);
    assert_eq!(hub.settle_due(&mut resting, later(t0)), [], "its own work holds it");

    lease.handle(waiting(ladder::COMMANDS_WAIT));
    let t1 = later(t0);
    assert_eq!(hub.settle_due(&mut resting, t1), []);
    assert_eq!(hub.settle_due(&mut resting, later(t1)), [term], "a command left running does not");
    let (_, verb) = request(&mut rx).await;
    assert_eq!(verb, Verb::Close { term });
    let said = status(&hub).await.timeline.into_iter().rev().find_map(|e| match e.what {
        Moment::Note { text } if e.task == Some(task) => Some(text),
        _ => None,
    });
    let said = said.unwrap_or_default();
    assert!(said.ends_with("It stopped what the agent left running: npm run dev."), "{said}");
}

/// A merged task's agent, closed once it rests, frees the worktree it worked in after its
/// terminal is gone: the worker is asked to remove it with where the work landed, and the card
/// lets it go while the timeline says whether its branch went. One the worker keeps (a terminal
/// still in it) stays on the card, and the timeline says why.
#[tokio::test]
async fn a_merged_task_s_worktree_goes_once_its_agent_is_closed() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (_worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    create(&hub, None).await;
    let note = |entries: &[TimelineEntry], task: TaskId| {
        let said = entries.iter().rev().find_map(|e| match &e.what {
            Moment::Note { text } if e.task == Some(task) => Some(text.clone()),
            _ => None,
        });
        said.unwrap_or_default()
    };
    let removed = Outcome::WorktreeRemoved {
        branch: Some("worktree-slopty-demo-1".to_owned()),
        branch_removed: true,
    };
    let kept = Outcome::Error {
        code: ErrorCode::Conflict,
        message: "a terminal works in /w/demo/.claude/worktrees/slopty-demo-2".to_owned(),
    };
    for (n, worker_says) in [(1, removed), (2, kept)] {
        let task = new_task(&hub, None).await;
        let asked = spawn(&hub, Verb::TaskSpawn { project: project(), task, launch: claude(&[]) });
        let term = opened(&lease, &request(&mut rx).await);
        assert!(matches!(asked.await.unwrap(), Outcome::Task(_)));
        let path = format!("/w/demo/.claude/worktrees/slopty-demo-{n}");
        let worktree = Worktree {
            name: format!("slopty-demo-{n}"),
            path: path.clone(),
            branch: Some(format!("worktree-slopty-demo-{n}")),
            original_cwd: "/w/demo".to_owned(),
            original_branch: Some("main".to_owned()),
        };
        let branch = AgentBranch { session: term.session, pr: None, worktree: Some(worktree) };
        lease.handle(ToServer::Report(AgentReport::Branch(branch)));
        for state in [TaskState::Done, TaskState::Merged] {
            let change = Box::new(TaskChange { state: Some(state), ..TaskChange::default() });
            let moved = hub.dispatch(Verb::TaskUpdate { project: project(), task, change }).await;
            assert!(matches!(moved, Outcome::Task(_)), "{moved:?}");
        }
        lease.handle(agent(term.session, AgentStatus::Idle));
        let mut resting = HashMap::new();
        let t0 = tokio::time::Instant::now();
        assert_eq!(hub.settle_due(&mut resting, t0), []);
        assert_eq!(hub.settle_due(&mut resting, t0.checked_add(SETTLE_AFTER).unwrap()), [term]);

        let (id, verb) = request(&mut rx).await;
        assert_eq!(verb, Verb::Close { term });
        answer(&lease, id, Outcome::Done);
        let reason = slopty_proto::terminal::CloseReason::Requested;
        lease.handle(ToServer::SessionClosed { session: term.session, reason });
        let (id, verb) = request(&mut rx).await;
        let landed = vec!["main".to_owned(), "origin/main".to_owned()];
        assert_eq!(
            verb,
            Verb::RemoveWorktree { worker: term.worker, worktree: path.clone(), landed }
        );
        answer(&lease, id, worker_says);
        let mut now = status(&hub).await;
        for _ in 0..500 {
            if note(&now.timeline, task).contains("worktree") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            now = status(&hub).await;
        }
        let card = now.tasks.iter().find(|t| t.id == task).expect("the card");
        let said = note(&now.timeline, task);
        if n == 1 {
            assert_eq!(card.worktree, None, "gone from the card");
            assert_eq!(
                said,
                "The server removed its worktree, and its branch worktree-slopty-demo-1, whose \
                 work all landed."
            );
        } else {
            assert_eq!(card.worktree.as_deref(), Some(path.as_str()), "kept on the card");
            assert_eq!(said, format!("Its worktree is kept: a terminal works in {path}."));
        }
    }
}

/// The orchestrator tells a task's agent something: it reaches that agent through its hooks,
/// marked as an agent's words, and the timeline says the orchestrator told. A task's agent
/// tells no task, not even its own, and nobody tells a task that waits on the person through an
/// agent; an agent's surface that proves no terminal tells nothing.
#[tokio::test]
async fn only_the_orchestrator_tells_a_task_in_its_own_words() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let delivering = tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    let (orchestrating, one, two) = (SessionId::new(), SessionId::new(), SessionId::new());
    let (worker, lease, mut rx) = worker_on(&hub, "studio", Os::MacOs, Vec::new());
    announce(&lease, orchestrating, true);
    for session in [one, two] {
        announce(&lease, session, false);
    }
    create(&hub, Some(TermRef { worker, session: orchestrating })).await;
    let role = next_batch(&mut rx).await;
    lease.handle(ToServer::Report(AgentReport::Delivered { session: role.0, batch: role.1 }));
    let (first, second) = (new_task(&hub, None).await, new_task(&hub, None).await);
    for (task, session) in [(first, one), (second, two)] {
        let assigned = hub.assign_for_test(&project(), task, TermRef { worker, session });
        assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    }
    let tell = |who, task, text: &str| {
        let verb = Verb::TaskTell { project: project(), task, text: text.to_owned() };
        hub.dispatch_as(who, None, verb)
    };

    let said = tell(Speaker::Proven(orchestrating), Some(first), "Also cover the iPad.").await;
    assert_eq!(said, Outcome::Done);
    let (session, _, context) = next_batch(&mut rx).await;
    assert_eq!(session, one);
    assert!(
        context.contains("Your orchestrator says (an agent, not the person; it answers nothing")
            && context.contains("  Also cover the iPad."),
        "{context}"
    );

    for (who, task, why) in [
        (Speaker::Proven(one), Some(second), "another task"),
        (Speaker::Proven(one), Some(first), "its own"),
        (Speaker::Proven(one), None, "the orchestrator"),
        (Speaker::Agent, Some(first), "proves no terminal"),
    ] {
        let said = tell(who, task, "Stop.").await;
        refused(&said, ErrorCode::Forbidden);
        assert!(matches!(said, Outcome::Error { .. }), "{why}");
    }
    lease.handle(agent(two, AgentStatus::Blocked(BlockReason::Question)));
    let said = tell(Speaker::Proven(orchestrating), Some(second), "Pick the first.").await;
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
    assert_eq!(notes, ["The orchestrator told it: Also cover the iPad."]);
    delivering.abort();
}
