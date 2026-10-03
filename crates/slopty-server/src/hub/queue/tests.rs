//! The lane through the hub, with a scripted worker standing in for the orchestrator's: a task
//! reported done is verified in the project's checkout and merged; a failure, a conflict and a
//! person's checkout in the way each end as they should. The git behind each verb is proved in
//! `slopty-worker`'s `repo::verify` and end to end in `apps/slopty-cli/tests/projects.rs`.

use std::collections::HashMap;
use std::time::Duration;

use slopty_proto::orchestration::{Line, Screen};
use slopty_proto::project::{
    Checks, ChecksState, LimitsChange, Merge, Moment, NativeTask, Placement, Report, ReportKind,
    StepKind, StepState, TaskId, TaskState,
};
use slopty_proto::server::Os;

use super::super::project_tests::{
    answer, create, in_repo, new_task, project, status, task_now, worker_again, worker_on,
};
use super::super::tests::summary;
use super::super::*;
use crate::project::RESUMING;

const URL: &str = "https://example.com/o/demo.git";
const TREE: &str = "/w/demo/.claude/worktrees/slopty-slopty-1";
pub(in crate::hub) const BRANCH: &str = "worktree-slopty-slopty-1";

pub(in crate::hub) fn commit(c: char) -> String {
    std::iter::repeat_n(c, 40).collect()
}

/// The orchestrator's worker as the lane sees it: its requests, and what it delivered to its
/// agents' hooks on the way.
pub(in crate::hub) struct Studio {
    pub lease: Lease,
    pub rx: mpsc::Receiver<FromServer>,
    pub delivered: Vec<(SessionId, String)>,
}

impl Studio {
    /// The next request, keeping any delivery that comes first.
    pub(in crate::hub) async fn request(&mut self) -> (RequestId, Verb) {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), self.rx.recv()).await {
                Ok(Some(FromServer::Request { id, verb, .. })) => return (id, verb),
                Ok(Some(FromServer::Deliver { session, context, .. })) => {
                    self.delivered.push((session, context));
                }
                Ok(Some(_)) => {}
                other => panic!("no request: {other:?}"),
            }
        }
    }

    /// The next request but a read of a verifier's screen, each answered with `lines`.
    pub(in crate::hub) async fn past_screens(&mut self, lines: &[&str]) -> (RequestId, Verb) {
        loop {
            let (id, verb) = self.request().await;
            if !matches!(verb, Verb::ReadScreen { .. }) {
                return (id, verb);
            }
            answer(&self.lease, id, screen(lines));
        }
    }

    /// Deliveries until one to `session` holds `words`.
    pub(in crate::hub) async fn told(&mut self, session: SessionId, words: &str) -> String {
        let found = |d: &[(SessionId, String)]| {
            d.iter().find(|(s, c)| *s == session && c.contains(words)).map(|(_, c)| c.clone())
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(context) = found(&self.delivered) {
                    return context;
                }
                if let Some(FromServer::Deliver { session, context, .. }) = self.rx.recv().await {
                    self.delivered.push((session, context));
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("nothing delivered to {session} saying {words:?}"))
    }

    /// The verifier the lane asked for runs in its terminal, and has exited with `status`
    /// before the worker answers: what it judged.
    pub(in crate::hub) fn ran(
        &self,
        (id, verb): &(RequestId, Verb),
        status: i32,
        judged: (char, char),
    ) -> TermRef {
        let Verb::Verify { worker, session, .. } = verb else { panic!("{verb:?}") };
        let exited = SessionSummary { state: SessionState::Exited { status }, ..summary(*session) };
        self.lease.handle(ToServer::SessionChanged(exited));
        let term = TermRef { worker: *worker, session: *session };
        let (head, base) = (commit(judged.0), commit(judged.1));
        answer(&self.lease, *id, Outcome::Verifying { term, head, base });
        term
    }
}

pub(in crate::hub) fn screen(lines: &[&str]) -> Outcome {
    let lines =
        (0..).zip(lines).map(|(index, text)| Line { index, text: (*text).to_owned() }).collect();
    Outcome::Screen(Screen {
        lines,
        cursor: (0, 0),
        title: String::new(),
        cwd: None,
        alternate: false,
    })
}

/// A studio with the orchestrator in its clone and a task's agent in a worktree of it, the
/// project's verifier named, the task on that agent; its id and the agent's terminal.
pub(in crate::hub) async fn fleet(hub: &Hub) -> (Studio, TermRef, TaskId, TermRef) {
    let (orchestrator, agent) = (SessionId::new(), SessionId::new());
    let sessions =
        vec![in_repo(orchestrator, "/w/demo", Some(URL)), in_repo(agent, TREE, Some(URL))];
    let (worker, lease, rx) = worker_on(hub, "studio", Os::MacOs, sessions);
    let orchestrator = TermRef { worker, session: orchestrator };
    create(hub, Some(orchestrator)).await;
    let set = Verb::ProjectSet {
        project: project(),
        orchestrator: None,
        review: None,
        verifier: Some("cargo gate".to_owned()),
        push: Some(true),
        ask_to_start: None,
        limits: LimitsChange::default(),
        metadata: None,
        members: None,
    };
    assert!(matches!(hub.dispatch(set).await, Outcome::Project(_)));
    let task = new_task(hub, Placement::default()).await;
    let agent = TermRef { worker, session: agent };
    let assigned = hub.dispatch(Verb::TaskAssign { project: project(), task, term: agent }).await;
    assert!(matches!(assigned, Outcome::Task(_)), "{assigned:?}");
    tokio::spawn(Hub::deliver_reports(hub.downgrade()));
    (Studio { lease, rx, delivered: Vec::new() }, orchestrator, task, agent)
}

/// The agent says it is done, with its branch.
pub(in crate::hub) async fn done(hub: &Hub, task: TaskId, agent: TermRef) {
    let report = Report {
        kind: ReportKind::Done,
        note: "Built it.".to_owned(),
        artifacts: Vec::new(),
        branch: Some(BRANCH.to_owned()),
        pr: None,
    };
    let verb = Verb::TaskReport { project: project(), task, report };
    let reported = hub.dispatch_as(Speaker::Proven(agent.session), None, verb).await;
    assert!(matches!(reported, Outcome::Task(_)), "{reported:?}");
}

pub(in crate::hub) async fn until_state(hub: &Hub, task: TaskId, want: TaskState) {
    let reached = tokio::time::timeout(Duration::from_secs(10), async {
        while task_now(hub, task).await.state != want {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    reached.await.unwrap_or_else(|_| panic!("task {task} never {want:?}"));
}

/// A task reported done, its branch in a worktree of the orchestrator's clone, is verified in
/// the project's own checkout of that clone: a terminal the card names while it runs, whose
/// last line it shows, closed once it passed. The pass puts it in the queue; the queue finds
/// the target where the work left it, so the rebase leaves the commit verified and nothing
/// runs again; the target is fast-forwarded and pushed, and the timeline says each step.
#[tokio::test]
async fn a_task_done_is_verified_in_the_orchestrator_s_clone_and_merged_by_fast_forward() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, orchestrator, task, agent) = fleet(&hub).await;
    done(&hub, task, agent).await;

    let (id, verb) = studio.request().await;
    let Verb::Verify { repo, worktree, head, target, command, session, title, worker } = &verb
    else {
        panic!("{verb:?}")
    };
    assert_eq!(
        (repo.as_str(), worktree.as_str(), head.as_str(), target.as_str(), command.as_str()),
        ("/w/demo", "slopty", BRANCH, "main", "cargo gate"),
        "the task's own branch, in the project's checkout of the orchestrator's clone"
    );
    assert_eq!(title, &format!("Verifier for slopty #{task}"));
    let term = TermRef { worker: *worker, session: *session };
    studio.lease.handle(ToServer::SessionChanged(summary(*session)));
    answer(&studio.lease, id, Outcome::Verifying { term, head: commit('a'), base: commit('b') });
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::ReadScreen { term: t } if t == term), "{verb:?}");
    answer(&studio.lease, id, screen(&["   Compiling slopty-server", "", ""]));
    let shown = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let step = task_now(&hub, task).await.step.unwrap();
            if let StepState::Running { phase, .. } = &step.state
                && !phase.is_empty()
            {
                return (step.kind, step.term, phase.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let progress = (StepKind::Verify, Some(term), "Compiling slopty-server".to_owned());
    assert_eq!(shown.await.unwrap(), progress, "live, with the terminal to open");
    assert_eq!(task_now(&hub, task).await.state, TaskState::Verifying);

    let exited = SessionSummary { state: SessionState::Exited { status: 0 }, ..summary(*session) };
    studio.lease.handle(ToServer::SessionChanged(exited));
    let (id, verb) = studio.past_screens(&["test result: ok. 12 passed", ""]).await;
    assert!(matches!(verb, Verb::Close { term: t } if t == term), "a pass closes it: {verb:?}");
    answer(&studio.lease, id, Outcome::Done);

    let (id, verb) = studio.request().await;
    let Verb::Rebase { head, onto, .. } = &verb else { panic!("{verb:?}") };
    assert_eq!((head.as_str(), onto.as_str()), (commit('a').as_str(), "main"), "what was verified");
    answer(&studio.lease, id, Outcome::Rebased { head: commit('a'), onto: commit('b') });
    let (id, verb) = studio.request().await;
    let Verb::FastForward { repo, target, from, to, push, .. } = &verb else { panic!("{verb:?}") };
    assert_eq!(
        (repo.as_str(), target.as_str(), from.clone(), to.clone(), *push),
        ("/w/demo", "main", commit('b'), commit('a'), true),
        "no second run: the commit verified is the one the target takes"
    );
    let moved = Outcome::FastForwarded { head: commit('a'), pushed: true, push_failed: None };
    answer(&studio.lease, id, moved);
    until_state(&hub, task, TaskState::Merged).await;

    let card = task_now(&hub, task).await;
    let Some(Merge::Merged { target, head, pushed, .. }) = card.merge else { panic!("{card:?}") };
    assert_eq!((target.as_str(), head, pushed), ("main", commit('a'), true));
    let run = card.verified.unwrap();
    assert_eq!(
        (run.passed, run.head, run.base, run.exit),
        (true, commit('a'), commit('b'), Some(0))
    );
    assert_eq!(run.summary, "test result: ok. 12 passed");
    let moments: Vec<String> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter(|e| e.task == Some(task))
        .filter_map(|e| match e.what {
            Moment::Step(s) => Some(format!(
                "{:?} {}",
                s.kind,
                match s.state {
                    StepState::Running { .. } => "began".to_owned(),
                    StepState::Done { detail } => detail,
                    StepState::Failed { why } => why,
                }
            )),
            Moment::Verified(run) => Some(format!("verified {}", run.passed)),
            _ => None,
        })
        .collect();
    assert_eq!(
        moments,
        ["Verify began", "verified true", "Merge began", "Merge main at aaaaaaa, pushed to origin"]
    );
    let merged = studio.told(orchestrator.session, "merged into main").await;
    assert!(merged.contains(&format!("task {task} merged into main at aaaaaaa")), "{merged}");
}

/// The hub after a restart of the server, from what `hub` kept: the studio of [`fleet`]
/// registers again, its orchestrator and agent still in their places beside `running`.
pub(in crate::hub) fn restarted(
    hub: Hub,
    studio: Studio,
    (orchestrator, agent): (TermRef, TermRef),
    running: &[SessionId],
) -> (Hub, Studio) {
    let (file, known) = (hub.projects_file(0), hub.directory());
    drop(studio);
    drop(hub);
    let hub = Hub::new("server".to_owned(), known);
    hub.adopt_projects(file);
    let mut sessions = vec![
        in_repo(orchestrator.session, "/w/demo", Some(URL)),
        in_repo(agent.session, TREE, Some(URL)),
    ];
    sessions.extend(running.iter().map(|s| summary(*s)));
    let (_, lease, rx) = worker_again(&hub, orchestrator.worker, "studio", Os::MacOs, sessions);
    (hub, Studio { lease, rx, delivered: Vec::new() })
}

/// A verifier still running when the server stopped is followed again once its worker is
/// back, not run a second time: the step waits for the worker meanwhile, keeps when it began
/// and the commits it checks, and the verdict it ends with is judged as any.
#[tokio::test]
async fn a_verifier_left_running_by_a_restart_is_followed_to_its_verdict() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, orchestrator, task, agent) = fleet(&hub).await;
    done(&hub, task, agent).await;
    let (id, verb) = studio.request().await;
    let Verb::Verify { worker, session, .. } = verb else { panic!("{verb:?}") };
    let term = TermRef { worker, session };
    studio.lease.handle(ToServer::SessionChanged(summary(session)));
    answer(&studio.lease, id, Outcome::Verifying { term, head: commit('a'), base: commit('b') });
    let since = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let step = task_now(&hub, task).await.step.unwrap();
            if step.commits.is_some() {
                return step.since_ms;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let (file, known) = (hub.projects_file(0), hub.directory());
    let away = Hub::new("server".to_owned(), known);
    away.adopt_projects(file);
    let step = task_now(&away, task).await.step.unwrap();
    let resuming = StepState::Running { phase: RESUMING.to_owned(), percent: None };
    assert_eq!((step.state, step.term), (resuming, Some(term)), "waiting for its worker");
    drop(away);

    let (hub, mut studio) = restarted(hub, studio, (orchestrator, agent), &[session]);
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::ReadScreen { term: t } if t == term), "no second run: {verb:?}");
    answer(&studio.lease, id, screen(&["   Compiling demo"]));
    let exited = SessionSummary { state: SessionState::Exited { status: 0 }, ..summary(session) };
    studio.lease.handle(ToServer::SessionChanged(exited));
    let (id, verb) = studio.past_screens(&["test result: ok. 12 passed", ""]).await;
    assert!(matches!(verb, Verb::Close { term: t } if t == term), "a pass closes it: {verb:?}");
    answer(&studio.lease, id, Outcome::Done);
    let (_, verb) = studio.request().await;
    assert!(matches!(verb, Verb::Rebase { .. }), "then the queue: {verb:?}");
    let card = task_now(&hub, task).await;
    let run = card.verified.unwrap();
    assert_eq!((run.passed, run.head, run.base), (true, commit('a'), commit('b')));
    let entries = status(&hub).await.timeline;
    let began = entries.iter().filter(
        |e| matches!(&e.what, Moment::Step(s) if s.kind == StepKind::Verify && s.since_ms == since),
    );
    assert_eq!(began.count(), 1, "one run, begun once");
}

/// A verifier that fails gives the task back: it waits at its agent's prompt again, out of the
/// queue, its verdict and last lines on the card, its terminal kept for the whole output. The
/// agent is told through its hooks what ran, on which commits and what it printed; the
/// orchestrator hears it was given back. The next run closes the kept terminal first.
#[tokio::test]
async fn a_failed_verifier_goes_back_to_its_agent_through_its_hooks() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, orchestrator, task, agent) = fleet(&hub).await;
    done(&hub, task, agent).await;
    let asked = studio.request().await;
    let term = studio.ran(&asked, 101, ('a', 'b'));
    let tail =
        ["   Compiling demo", "error[E0308]: mismatched types", "error: could not compile `demo`"];
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::ReadScreen { term: t } if t == term), "{verb:?}");
    answer(&studio.lease, id, screen(&tail));
    until_state(&hub, task, TaskState::Waiting).await;

    let card = task_now(&hub, task).await;
    assert_eq!(card.merge, None, "out of the queue");
    let run = card.verified.clone().unwrap();
    assert_eq!(
        (run.passed, run.exit, run.head, run.base),
        (false, Some(101), commit('a'), commit('b'))
    );
    assert_eq!(run.summary, tail.join("\n"), "its last lines");
    let step = card.step.unwrap();
    assert_eq!(step.kind, StepKind::Verify);
    assert_eq!(step.term, Some(term), "kept for the whole output");
    assert_eq!(step.state, StepState::Failed { why: "error: could not compile `demo`".to_owned() });

    let told = studio.told(agent.session, "The verifier `cargo gate` failed").await;
    for words in [
        "your branch at aaaaaaa, which left main at bbbbbbb",
        "exit 101",
        "mismatched types",
        "report done again",
    ] {
        assert!(told.contains(words), "{words:?} in {told}");
    }
    let above = studio.told(orchestrator.session, &format!("task {task} was given back")).await;
    assert!(above.contains("its agent was told"), "{above}");

    done(&hub, task, agent).await;
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::Close { term: t } if t == term), "the kept one goes: {verb:?}");
    answer(&studio.lease, id, Outcome::Done);
    let (_, verb) = studio.request().await;
    assert!(matches!(verb, Verb::Verify { .. }), "then the new head is verified: {verb:?}");
}

/// The queue takes a passing task through: what its rebase made is verified again, since it is
/// not the commit verified; a target that moved meanwhile has the work rebased on top of it
/// and verified once more. A person's checkout whose changes are in the way holds the queue,
/// the task keeping its place with the reason on its step.
#[tokio::test]
async fn the_queue_verifies_a_rebased_head_again_and_holds_for_the_person_s_changes() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    done(&hub, task, agent).await;
    let mut judged = ('a', 'b');
    let asked = studio.request().await;
    studio.ran(&asked, 0, judged);
    let (id, verb) = studio.past_screens(&["ok"]).await;
    assert!(matches!(verb, Verb::Close { .. }), "{verb:?}");
    answer(&studio.lease, id, Outcome::Done);
    for (rebased, onto, moved) in [('c', 'd', true), ('e', 'f', false)] {
        let (id, verb) = studio.request().await;
        let Verb::Rebase { head, .. } = &verb else { panic!("{verb:?}") };
        assert_eq!(head, &commit(judged.0), "the last commit verified goes on");
        answer(&studio.lease, id, Outcome::Rebased { head: commit(rebased), onto: commit(onto) });
        let asked = studio.request().await;
        let Verb::Verify { head, .. } = &asked.1 else { panic!("{:?}", asked.1) };
        assert_eq!(head, &commit(rebased), "what the rebase made is verified");
        studio.ran(&asked, 0, (rebased, onto));
        let (id, verb) = studio.past_screens(&["ok"]).await;
        assert!(matches!(verb, Verb::Close { .. }), "{verb:?}");
        answer(&studio.lease, id, Outcome::Done);
        judged = (rebased, onto);
        let (id, verb) = studio.request().await;
        let Verb::FastForward { from, to, .. } = &verb else { panic!("{verb:?}") };
        assert_eq!((from, to), (&commit(onto), &commit(rebased)));
        let outcome = if moved {
            let message = format!("the branch moved to {}", commit('9'));
            Outcome::Error { code: ErrorCode::Conflict, message }
        } else {
            let message = "error: Your local changes to the following files would be \
                           overwritten by merge: a.txt"
                .to_owned();
            Outcome::Error { code: ErrorCode::Failed, message }
        };
        answer(&studio.lease, id, outcome);
    }
    let held = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let card = task_now(&hub, task).await;
            if let Some(step) = card.step.filter(|s| matches!(s.state, StepState::Failed { .. })) {
                return (card.state, card.merge.and_then(|m| m.queued()).is_some(), step);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let (state, queued, step) = held.await.unwrap();
    assert_eq!(
        (state, queued, step.kind),
        (TaskState::Done, true, StepKind::Merge),
        "its place kept"
    );
    assert!(
        matches!(&step.state, StepState::Failed { why } if why.contains("would be overwritten")),
        "{step:?}"
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), studio.request()).await;
    assert!(quiet.is_err(), "the lane waits: {quiet:?}");
    let run = task_now(&hub, task).await.verified.unwrap();
    assert_eq!((run.head, run.base), (commit('e'), commit('f')), "the verdict on the last rebase");
}

/// A head that does not rebase onto the target goes back to its agent with the paths that
/// conflict, out of the queue.
#[tokio::test]
async fn a_conflict_goes_back_to_the_agent_with_its_paths() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    done(&hub, task, agent).await;
    let asked = studio.request().await;
    studio.ran(&asked, 0, ('a', 'b'));
    let (id, _close) = studio.past_screens(&["ok"]).await;
    answer(&studio.lease, id, Outcome::Done);
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::Rebase { .. }), "{verb:?}");
    let message = "conflicts in a.txt, src/lib.rs".to_owned();
    answer(&studio.lease, id, Outcome::Error { code: ErrorCode::Conflict, message });
    until_state(&hub, task, TaskState::Waiting).await;
    let card = task_now(&hub, task).await;
    assert_eq!(card.merge, None);
    let step = card.step.unwrap();
    assert_eq!(
        (step.kind, step.state),
        (StepKind::Rebase, StepState::Failed { why: "conflicts in a.txt, src/lib.rs".to_owned() })
    );
    let told = studio.told(agent.session, "does not rebase onto main").await;
    assert!(told.contains("conflicts in a.txt, src/lib.rs"), "{told}");
}

/// The person's next step reaches the task's own agent through its hooks, at once and in
/// their words, and the timeline keeps it; their words to the orchestrator reach it the same
/// way. The orchestrator's words reach it marked as its own, never as the person's, an agent
/// that proves no terminal tells nothing, and a task with no agent running has nobody to tell.
#[tokio::test]
async fn the_person_s_words_reach_the_task_s_agent() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, orchestrator, task, agent) = fleet(&hub).await;
    let refused = |o: &Outcome, want: ErrorCode| {
        assert!(matches!(o, Outcome::Error { code, .. } if *code == want), "{o:?}");
    };
    let tell =
        |text: &str| Verb::TaskTell { project: project(), task: Some(task), text: text.to_owned() };
    let words = "Resolve the conflicts with main, then report done again.";
    assert_eq!(hub.dispatch(tell(words)).await, Outcome::Done);
    let told = studio.told(agent.session, "The person says:").await;
    assert!(told.contains(words), "{told}");
    let s = status(&hub).await;
    assert!(
        s.timeline.iter().any(|e| e.what == Moment::Told { text: words.to_owned() }),
        "{:?}",
        s.timeline
    );

    let to_orchestrator = Verb::TaskTell {
        project: project(),
        task: None,
        text: "Split the board work in two.".to_owned(),
    };
    assert_eq!(hub.dispatch(to_orchestrator).await, Outcome::Done);
    let told = studio.told(orchestrator.session, "The person says:").await;
    assert!(told.contains("Split the board work in two."), "{told}");
    let s = status(&hub).await;
    assert!(
        s.timeline.iter().any(|e| e.task.is_none() && matches!(e.what, Moment::Told { .. })),
        "{:?}",
        s.timeline
    );

    let by_orchestrator =
        hub.dispatch_as(Speaker::Proven(orchestrator.session), None, tell("Go on.")).await;
    assert_eq!(by_orchestrator, Outcome::Done);
    let told = studio.told(agent.session, "Your orchestrator says").await;
    assert!(told.contains("an agent, not the person") && told.contains("  Go on."), "{told}");
    refused(&hub.dispatch_as(Speaker::Agent, None, tell("Go")).await, ErrorCode::Forbidden);
    refused(&hub.dispatch(tell("  ")).await, ErrorCode::Invalid);
    studio.lease.handle(ToServer::SessionClosed {
        session: agent.session,
        reason: slopty_proto::terminal::CloseReason::Exited,
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while task_now(&hub, task).await.assignment.is_some_and(|a| a.open()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("its terminal closes");
    refused(&hub.dispatch(tell("Fix CI")).await, ErrorCode::Invalid);
}

/// Work whose agent still has to-dos open on its own list is not merged: the queue gives it
/// back to its agent naming them, before it rebases anything.
#[tokio::test]
async fn open_to_dos_keep_work_from_merging() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    let todo = |id: &str, subject: &str, done| NativeTask {
        id: id.to_owned(),
        subject: subject.to_owned(),
        done,
    };
    for item in [todo("1", "Write the tests", false), todo("2", "Build it", true)] {
        let report = AgentReport::NativeTask { session: agent.session, task: item };
        studio.lease.handle(ToServer::Report(report));
    }
    done(&hub, task, agent).await;
    let asked = studio.request().await;
    studio.ran(&asked, 0, ('a', 'b'));
    let (id, _close) = studio.past_screens(&["ok"]).await;
    answer(&studio.lease, id, Outcome::Done);
    until_state(&hub, task, TaskState::Waiting).await;
    let card = task_now(&hub, task).await;
    let step = card.step.unwrap();
    assert_eq!(
        (step.kind, step.state),
        (
            StepKind::Merge,
            StepState::Failed { why: "1 to-do still open on its task list".to_owned() }
        )
    );
    let told = studio.told(agent.session, "your task list still has 1 to-do open").await;
    assert!(told.contains("(Write the tests)"), "{told}");
    assert!(!told.contains("Build it"), "{told}");
    let rebased = tokio::time::timeout(Duration::from_millis(300), studio.request()).await;
    assert!(rebased.is_err(), "nothing is rebased: {rebased:?}");
}

/// A task's pull request's own checks are read on the worker its agent ran on, in its
/// worktree, and put on its card; the timeline says where they stand when that moves, and a
/// read that says the same again is no change.
#[tokio::test]
async fn a_pull_request_s_checks_are_read_where_its_work_is() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    let worktree = slopty_proto::agent::Worktree {
        name: "slopty-slopty-1".to_owned(),
        path: TREE.to_owned(),
        branch: Some(BRANCH.to_owned()),
        original_cwd: "/w/demo".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let pr = slopty_proto::agent::PullRequest {
        number: 7,
        url: "https://example.com/o/demo/pull/7".to_owned(),
        review: None,
        merge_request: false,
    };
    let branch = AgentBranch { session: agent.session, pr: Some(pr), worktree: Some(worktree) };
    studio.lease.handle(ToServer::Report(AgentReport::Branch(branch)));
    let card = task_now(&hub, task).await;
    assert_eq!((card.pr.map(|p| p.number), card.worktree.as_deref()), (Some(7), Some(TREE)));

    let failing = Checks {
        state: ChecksState::Failing,
        passed: 3,
        failed: 1,
        pending: 0,
        skipped: 1,
        failing: vec!["lint".to_owned()],
        why: None,
        at_ms: WallMs::now(),
    };
    for round in 0..2 {
        let watcher = hub.clone();
        let read = tokio::spawn(async move {
            let mut due = HashMap::new();
            watcher.read_due_checks(&mut due).await;
            due.len()
        });
        let (id, verb) = studio.request().await;
        let Verb::PullChecks { worker, cwd, number, merge_request } = verb else {
            panic!("round {round}: {verb:?}")
        };
        assert_eq!((worker, cwd.as_str(), number, merge_request), (agent.worker, TREE, 7, false));
        let again = Checks { at_ms: WallMs::now(), ..failing.clone() };
        answer(&studio.lease, id, Outcome::Checks(again));
        assert_eq!(read.await.unwrap(), 1, "due again later");
    }
    let card = task_now(&hub, task).await;
    let kept = card.checks.expect("its checks");
    assert_eq!((kept.state, kept.failing), (ChecksState::Failing, vec!["lint".to_owned()]));
    let s = status(&hub).await;
    let said: Vec<_> = s.timeline.iter().filter(|e| matches!(e.what, Moment::Checks(_))).collect();
    assert_eq!(said.len(), 1, "{said:?}");
}

/// Checks that cannot be read (the forge's command missing or not signed in where the work
/// is) put that on the card, once, and are asked again later; a reading replaces them, and a
/// forge that stops answering afterwards leaves that reading standing.
#[tokio::test]
async fn checks_that_cannot_be_read_say_why_and_never_hide_a_reading() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    let pr = slopty_proto::agent::PullRequest {
        number: 7,
        url: "https://example.com/o/demo/pull/7".to_owned(),
        review: None,
        merge_request: false,
    };
    let worktree = slopty_proto::agent::Worktree {
        name: "slopty-slopty-1".to_owned(),
        path: TREE.to_owned(),
        branch: Some(BRANCH.to_owned()),
        original_cwd: "/w/demo".to_owned(),
        original_branch: Some("main".to_owned()),
    };
    let branch = AgentBranch { session: agent.session, pr: Some(pr), worktree: Some(worktree) };
    studio.lease.handle(ToServer::Report(AgentReport::Branch(branch)));
    let _card = task_now(&hub, task).await;
    let passing = Checks {
        state: ChecksState::Passing,
        passed: 2,
        failed: 0,
        pending: 0,
        skipped: 0,
        failing: Vec::new(),
        why: None,
        at_ms: WallMs::now(),
    };
    let missing = || Outcome::Error {
        code: ErrorCode::Unsupported,
        message: "this worker has no gh".to_owned(),
    };
    for (round, said) in
        [missing(), missing(), Outcome::Checks(passing.clone()), missing()].into_iter().enumerate()
    {
        let watcher = hub.clone();
        let read = tokio::spawn(async move {
            let mut due = HashMap::new();
            watcher.read_due_checks(&mut due).await;
            due.len()
        });
        let (id, verb) = studio.request().await;
        assert!(matches!(verb, Verb::PullChecks { number: 7, .. }), "round {round}: {verb:?}");
        answer(&studio.lease, id, said);
        assert_eq!(read.await.unwrap(), 1, "round {round}: due again later");
        let kept = task_now(&hub, task).await.checks.expect("its checks");
        if round < 2 {
            assert_eq!(
                (kept.state, kept.why.as_deref()),
                (ChecksState::Unknown, Some("this worker has no gh")),
                "round {round}"
            );
        } else {
            assert_eq!((kept.state, kept.why), (ChecksState::Passing, None), "round {round}");
        }
    }
    let s = status(&hub).await;
    let said: Vec<_> = s
        .timeline
        .iter()
        .filter_map(|e| match &e.what {
            Moment::Checks(c) => Some(c.state),
            _ => None,
        })
        .collect();
    assert_eq!(said, [ChecksState::Unknown, ChecksState::Passing]);
}

/// A merge whose push failed is pushed again on the person's word: the target as the
/// orchestrator's clone has it, from where it is, with how it went on the card and the
/// timeline. Pushed, it is answered as it is with nothing asked of the worker; an agent may
/// not push, and a task not merged has nothing to push.
#[tokio::test]
async fn a_merge_whose_push_failed_is_pushed_again_on_the_person_s_word() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = fleet(&hub).await;
    let push = move || Verb::TaskPush { project: project(), task };
    let unmerged = hub.dispatch(push()).await;
    assert!(matches!(unmerged, Outcome::Error { code: ErrorCode::Invalid, .. }), "{unmerged:?}");
    done(&hub, task, agent).await;
    let asked = studio.request().await;
    studio.ran(&asked, 0, ('a', 'b'));
    let (id, verb) = studio.past_screens(&["ok"]).await;
    assert!(matches!(verb, Verb::Close { .. }), "{verb:?}");
    answer(&studio.lease, id, Outcome::Done);
    let (id, _rebase) = studio.request().await;
    answer(&studio.lease, id, Outcome::Rebased { head: commit('a'), onto: commit('b') });
    let (id, _forward) = studio.request().await;
    let rejected = Some("rejected: fetch first".to_owned());
    let moved = Outcome::FastForwarded { head: commit('a'), pushed: false, push_failed: rejected };
    answer(&studio.lease, id, moved);
    until_state(&hub, task, TaskState::Merged).await;

    let by_agent = hub.dispatch_as(Speaker::Proven(agent.session), None, push()).await;
    assert!(matches!(by_agent, Outcome::Error { code: ErrorCode::Forbidden, .. }), "{by_agent:?}");
    let merge_of = |outcome: Outcome| match outcome {
        Outcome::Task(card) => match card.merge {
            Some(Merge::Merged { head, pushed, push_failed, .. }) => (head, pushed, push_failed),
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    for (round, (said, want)) in [
        (Some("could not read Username"), (false, Some("could not read Username"))),
        (None, (true, None)),
    ]
    .into_iter()
    .enumerate()
    {
        let pushing = hub.clone();
        let pushed = tokio::spawn(async move { pushing.dispatch(push()).await });
        let (id, verb) = studio.request().await;
        let Verb::FastForward { repo, target, from, to, push, .. } = &verb else {
            panic!("round {round}: {verb:?}")
        };
        assert_eq!(
            (repo.as_str(), target.as_str(), from.as_str(), to.as_str(), *push),
            ("/w/demo", "main", "refs/heads/main", "refs/heads/main", true),
            "round {round}: the target from where it is"
        );
        let went = Outcome::FastForwarded {
            head: commit('c'),
            pushed: said.is_none(),
            push_failed: said.map(str::to_owned),
        };
        answer(&studio.lease, id, went);
        let (head, pushed, failed) = merge_of(pushed.await.unwrap());
        assert_eq!(
            (head, (pushed, failed.as_deref())),
            (commit('a'), want),
            "round {round}: the merge is the same one"
        );
    }
    let again = merge_of(hub.dispatch(push()).await);
    assert_eq!(again, (commit('a'), true, None));
    let nothing = tokio::time::timeout(Duration::from_millis(200), studio.request()).await;
    assert!(nothing.is_err(), "nothing asked once pushed: {nothing:?}");
    let steps: Vec<String> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Step(s) => match s.state {
                StepState::Done { detail } => Some(detail),
                StepState::Failed { why } => Some(why),
                StepState::Running { .. } => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        steps.get(steps.len().saturating_sub(2)..),
        Some(
            &[
                "main not pushed: could not read Username".to_owned(),
                "main at ccccccc pushed to origin".to_owned()
            ][..]
        )
    );
}
