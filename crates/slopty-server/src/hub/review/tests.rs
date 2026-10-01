//! The reviewer through the hub, with the scripted worker of the lane's tests: started on the
//! verified work in a checkout of its own, its verdict deciding the merge, the person's word
//! over it, and a reviewer that ends with nothing to say. The checkout's git is proved in
//! `slopty-worker`'s `repo::review`, the whole way in `apps/slopty-cli/tests/projects.rs`.

use std::time::Duration;

use slopty_proto::project::{
    Finding, LimitsChange, Moment, REVIEW_DIFF, ReviewVerdict, Reviewer, StepKind, StepState,
    TaskId, TaskState,
};

use super::super::project_tests::{answer, project, status, task_now};
use super::super::queue::tests::{Studio, commit, done, fleet, until_state};
use super::super::tests::summary;
use super::super::*;

const BRIEF: &str = "A wire change comes with its goldens";

/// The project of the lane's tests, with a reviewer asked for.
async fn reviewed_fleet(hub: &Hub) -> (Studio, TermRef, TaskId, TermRef) {
    let fleet = fleet(hub).await;
    let set = Verb::ProjectSet {
        project: project(),
        orchestrator: None,
        verifier: None,
        review: Some(BRIEF.to_owned()),
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
    };
    assert!(matches!(hub.dispatch(set).await, Outcome::Project(_)));
    fleet
}

/// The verifier the lane asks for passes on `a` over `b`, and its terminal closes.
async fn verified(studio: &mut Studio) {
    let asked = studio.request().await;
    studio.ran(&asked, 0, ('a', 'b'));
    let (id, verb) = studio.past_screens(&["test result: ok", ""]).await;
    assert!(matches!(verb, Verb::Close { .. }), "{verb:?}");
    answer(&studio.lease, id, Outcome::Done);
}

/// The reviewer's checkout and start, answered: its terminal, and what it was started with.
async fn reviewer_started(studio: &mut Studio) -> (TermRef, Vec<String>, String, String) {
    let (id, verb) = studio.request().await;
    let Verb::ReviewCheckout { worker, repo, worktree, head, target } = &verb else {
        panic!("{verb:?}")
    };
    assert_eq!(
        (repo.as_str(), worktree.as_str(), head.clone(), target.as_str()),
        ("/w/demo", "slopty-review-1", commit('a'), "main"),
        "the commit the verifier passed, in a checkout of the task's own"
    );
    let worker = *worker;
    let path = "/home/c/slopty/verify/slopty-review-1".to_owned();
    let out = Outcome::CheckedOut { path: path.clone(), head: commit('a'), base: commit('b') };
    answer(&studio.lease, id, out);
    let (id, verb) = studio.request().await;
    let Verb::SpawnAgent { cwd, prompt, args, session, env, .. } = verb else { panic!("{verb:?}") };
    assert_eq!(cwd, path);
    assert!(env.contains(&("SLOPTY_TASK".to_owned(), "1".to_owned())), "{env:?}");
    let term = TermRef { worker, session: session.expect("a session chosen") };
    answer(&studio.lease, id, Outcome::Opened(term));
    (term, args, prompt.unwrap_or_default(), path)
}

fn blocked() -> ReviewVerdict {
    ReviewVerdict {
        approved: false,
        summary: "The new field has no golden.".to_owned(),
        findings: vec![
            Finding {
                path: None,
                line: None,
                severity: "nit".to_owned(),
                blocking: false,
                body: "A doc says verifier where it means reviewer.".to_owned(),
            },
            Finding {
                path: Some("crates/slopty-proto/src/project.rs".to_owned()),
                line: Some(431),
                severity: "blocker".to_owned(),
                blocking: true,
                body: "Project.review has no golden.".to_owned(),
            },
        ],
    }
}

fn approve() -> ReviewVerdict {
    ReviewVerdict { approved: true, summary: "Fine.".to_owned(), findings: Vec::new() }
}

async fn review_as(hub: &Hub, speaker: Speaker, task: TaskId, verdict: ReviewVerdict) -> Outcome {
    let verb = Verb::TaskReview { project: project(), task, verdict };
    hub.dispatch_as(speaker, None, verb).await
}

/// A verified task is read by a reviewer of its own: started on the verified commit in a
/// checkout apart, read-only, told its role, the person's brief and where the diff is, and
/// named on the task's step while it reads. The lane does not wait on it. Only that reviewer
/// or the person may answer. Its block gives the task back to its agent with what blocks, the
/// most important first, its session kept to read. The work done again is verified and read
/// afresh, the old reviewer let go, and the person's approval over the new reviewer sends it
/// through the queue.
#[tokio::test]
async fn a_reviewer_reads_the_verified_work_and_its_verdict_decides_the_merge() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = reviewed_fleet(&hub).await;
    done(&hub, task, agent).await;
    verified(&mut studio).await;
    let (reviewer, args, prompt, _) = reviewer_started(&mut studio).await;
    let flag = args.iter().position(|a| a == "--disallowedTools").expect("read-only");
    assert_eq!(args[flag + 1], "Edit,Write,NotebookEdit");
    let role = args.iter().find(|a| a.starts_with("--append-system-prompt=")).expect("a role");
    assert!(role.contains("You review task 1") && role.contains(BRIEF), "{role}");
    assert!(role.contains("review_report") && role.contains(REVIEW_DIFF), "{role}");
    assert!(prompt.contains(REVIEW_DIFF) && prompt.contains("bbbbbbb..aaaaaaa"), "{prompt}");
    let card = task_now(&hub, task).await;
    let step = card.step.clone().expect("a step");
    assert_eq!(
        (step.kind, step.term, card.state),
        (StepKind::Review, Some(reviewer), TaskState::Verifying)
    );
    assert!(card.verified.is_some_and(|r| r.passed), "the pass it reads, on the card");

    let theirs = review_as(&hub, Speaker::Proven(agent.session), task, approve()).await;
    assert!(
        matches!(&theirs, Outcome::Error { code: ErrorCode::Forbidden, .. }),
        "the task's own agent does not judge its work: {theirs:?}"
    );
    let said = review_as(&hub, Speaker::Proven(reviewer.session), task, blocked()).await;
    assert!(matches!(said, Outcome::Task(_)), "{said:?}");
    let card = task_now(&hub, task).await;
    assert_eq!((card.state, card.merge.clone()), (TaskState::Waiting, None));
    let run = card.reviewed.clone().expect("the review");
    assert_eq!(
        (run.by, run.head.clone(), run.base.clone()),
        (Reviewer::Agent(reviewer), commit('a'), commit('b'))
    );
    assert!(run.verdict.findings[0].blocking, "what blocks comes first");
    let step = card.step.expect("a step");
    assert_eq!((step.kind, step.term), (StepKind::Review, Some(reviewer)), "kept to read");
    let told = studio.told(agent.session, "What blocks the merge").await;
    for words in [
        "A reviewer with fresh context",
        "aaaaaaa",
        "project.rs:431",
        "Also noted",
        "report done again",
    ] {
        assert!(told.contains(words), "{words:?} in {told}");
    }

    done(&hub, task, agent).await;
    // The old reviewer is let go as the new work's verifier starts, in either order.
    let (mut closed, mut verify) = (false, None);
    while !closed || verify.is_none() {
        let (id, verb) = studio.request().await;
        match verb {
            Verb::Close { term } if term == reviewer => {
                closed = true;
                answer(&studio.lease, id, Outcome::Done);
            }
            verb @ Verb::Verify { .. } => verify = Some((id, verb)),
            other => panic!("{other:?}"),
        }
    }
    let verify = verify.expect("the verifier");
    studio.ran(&verify, 0, ('a', 'b'));
    let (id, verb) = studio.past_screens(&["test result: ok", ""]).await;
    assert!(matches!(verb, Verb::Close { .. }), "{verb:?}");
    answer(&studio.lease, id, Outcome::Done);
    let (second, ..) = reviewer_started(&mut studio).await;
    assert_ne!(second, reviewer, "read afresh");
    assert!(task_now(&hub, task).await.reviewed.is_none(), "the old word no longer speaks");

    let over = review_as(&hub, Speaker::Person, task, approve()).await;
    assert!(matches!(over, Outcome::Task(_)), "{over:?}");
    // The reviewer the person spoke over is closed as the queue takes the work, in either order.
    let (mut closed, mut rebased) = (false, false);
    while !closed || !rebased {
        let (id, verb) = studio.request().await;
        match verb {
            Verb::Close { term } if term == second => {
                closed = true;
                answer(&studio.lease, id, Outcome::Done);
            }
            Verb::Rebase { .. } => {
                rebased = true;
                let rebased = Outcome::Rebased { head: commit('a'), onto: commit('b') };
                answer(&studio.lease, id, rebased);
            }
            other => panic!("{other:?}"),
        }
    }
    let (id, verb) = studio.request().await;
    assert!(matches!(verb, Verb::FastForward { .. }), "{verb:?}");
    let moved = Outcome::FastForwarded { head: commit('a'), pushed: false, push_failed: None };
    answer(&studio.lease, id, moved);
    until_state(&hub, task, TaskState::Merged).await;
    let by: Vec<Reviewer> = status(&hub)
        .await
        .timeline
        .into_iter()
        .filter_map(|e| match e.what {
            Moment::Reviewed(run) => Some(run.by),
            _ => None,
        })
        .collect();
    assert_eq!(by, [Reviewer::Agent(reviewer), Reviewer::Person], "each word on the timeline");
}

/// A reviewer that ends with nothing said leaves its task waiting on the person, and the lane
/// does not start another on its own. The person cannot approve work its verifier has not
/// passed; `task merge` has it checked again from the start.
#[tokio::test]
async fn a_reviewer_that_ends_without_a_verdict_leaves_it_to_the_person() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (mut studio, _orchestrator, task, agent) = reviewed_fleet(&hub).await;
    done(&hub, task, agent).await;
    verified(&mut studio).await;
    let (reviewer, ..) = reviewer_started(&mut studio).await;
    studio.lease.handle(ToServer::SessionChanged(summary(reviewer.session)));
    studio.lease.handle(ToServer::SessionClosed {
        session: reviewer.session,
        reason: slopty_proto::terminal::CloseReason::Exited,
    });
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let step = task_now(&hub, task).await.step.expect("a step");
            if let StepState::Failed { why } = step.state {
                return why;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(ended.await.unwrap(), "The reviewer ended without a verdict");
    assert_eq!(task_now(&hub, task).await.state, TaskState::Verifying);
    let nothing = tokio::time::timeout(Duration::from_millis(300), studio.rx.recv()).await;
    assert!(
        !matches!(nothing, Ok(Some(FromServer::Request { verb: Verb::ReviewCheckout { .. }, .. }))),
        "no reviewer started again on its own"
    );

    let merge = hub.dispatch(Verb::TaskMerge { project: project(), task }).await;
    assert!(matches!(merge, Outcome::Task(_)), "{merge:?}");
    let asked = studio.request().await;
    assert!(matches!(asked.1, Verb::Verify { .. }), "checked from the start: {:?}", asked.1);
    let refused = review_as(&hub, Speaker::Person, task, approve()).await;
    assert!(
        matches!(&refused, Outcome::Error { code: ErrorCode::Invalid, message } if message.contains("verifier")),
        "{refused:?}"
    );
}
