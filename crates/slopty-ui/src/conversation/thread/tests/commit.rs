//! The commit sheet over a thread's tile: the files ticked go in the commit with the person's
//! words, a push follows the commit only once it is made, the pull request stands over the
//! files with its merge as the forge's own summary allows it, and git's refusals read in its words.

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use slopty_proto::git::{GitDone, GitFile, GitOp, GitOutcome, GitStatus, PullCheck, PullStatus};
use slopty_proto::{ClientMsg, RequestId};

use super::{Sent, hub, snapshot, view};
use crate::conversation::thread::fixtures;
use crate::conversation::thread::hub::ThreadHub;

/// The git ops asked so far, with their numbers.
fn asks(sent: &Sent) -> Vec<(RequestId, GitOp)> {
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Git { request, repo, op } => {
                assert_eq!(repo, "/w", "the thread's folder");
                Some((*request, op.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The number of the last `op` asked.
fn last(sent: &Sent, op: &GitOp) -> RequestId {
    asks(sent).into_iter().rev().find(|(_, o)| o == op).map(|(r, _)| r).expect("asked")
}

fn status() -> GitStatus {
    let file =
        |xy: &str, path: &str| GitFile { path: path.to_owned(), from: None, xy: xy.to_owned() };
    GitStatus {
        root: "/w".to_owned(),
        forge: None,
        branch: Some("feature".to_owned()),
        head: Some("abc".to_owned()),
        upstream: Some("origin/feature".to_owned()),
        merge_base: None,
        ahead: 0,
        behind: 0,
        files: vec![file(".M", "src/lib.rs"), file("??", "notes.md")],
        more: 0,
    }
}

fn pull(merge_state: &str, checks: &[&str]) -> PullStatus {
    PullStatus {
        forge: slopty_proto::git::Forge::GitHub,
        number: 7,
        url: "https://github.com/o/r/pull/7".to_owned(),
        title: "Add the sheet".to_owned(),
        state: "OPEN".to_owned(),
        draft: false,
        head: "feature".to_owned(),
        head_commit: "c0ffee".to_owned(),
        base: "main".to_owned(),
        review: String::new(),
        mergeable: "MERGEABLE".to_owned(),
        merge_state: merge_state.to_owned(),
        checks: checks
            .iter()
            .map(|state| PullCheck {
                name: format!("ci {state}"),
                workflow: Some("CI".to_owned()),
                state: (*state).to_owned(),
                link: None,
            })
            .collect(),
        more_checks: 0,
        methods: ["merge", "squash", "rebase"].map(str::to_owned).to_vec(),
        method: "squash".to_owned(),
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// A thread at `/w`, its view open with the commit sheet opened from the "+" menu.
fn opened(cx: &mut TestAppContext) -> (gpui::Entity<ThreadHub>, Sent, &mut VisualTestContext) {
    opened_owning(cx, Some(&["src/lib.rs", "notes.md"]))
}

/// The sheet over a thread that ran one turn, whose review over all its turns names `own`, or
/// cannot tell (`None`).
fn opened_owning<'a>(
    cx: &'a mut TestAppContext,
    own: Option<&[&str]>,
) -> (gpui::Entity<ThreadHub>, Sent, &'a mut VisualTestContext) {
    opened_as(cx, own, false)
}

/// [`opened_owning`], its agent taking a message where `queues`.
fn opened_as<'a>(
    cx: &'a mut TestAppContext,
    own: Option<&[&str]>,
    queues: bool,
) -> (gpui::Entity<ThreadHub>, Sent, &'a mut VisualTestContext) {
    use slopty_proto::thread::wire::{FileDiff, FileKind, Review, ReviewScope, ThreadFrame};
    use slopty_proto::thread::{TurnId, TurnState};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    state.turns.push(turn(1, TurnState::Complete));
    if queues {
        state.meta.caps = vec![slopty_proto::thread::Cap::named(slopty_proto::thread::Cap::QUEUE)];
    }
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    let asked = sent.borrow().iter().any(|m| {
        matches!(
            m,
            ClientMsg::Thread(slopty_proto::thread::wire::ThreadRequest::Review {
                scope: ReviewScope::Since(TurnId(1)),
                ..
            })
        )
    });
    assert!(asked, "the thread's review over all its turns is asked as the sheet opens");
    let file = |path: &str| FileDiff {
        path: path.to_owned(),
        old_path: None,
        from: None,
        to: Some("b0".to_owned()),
        kind: FileKind::Text,
        modes: None,
        patch: slopty_proto::thread::Patch::default(),
    };
    let review = Review {
        scope: ReviewScope::Since(TurnId(1)),
        from: None,
        to: None,
        files: own.unwrap_or_default().iter().map(|p| file(p)).collect(),
        absent: own.is_none().then(|| "no snapshot".to_owned()),
    };
    hub.update(cx, |hub, cx| hub.frame(thread, ThreadFrame::Review(Box::new(review)), cx));
    cx.run_until_parked();
    (hub, sent, cx)
}

/// A thread's sheet ticks only the files the thread's review names, so another thread's work
/// in the checkout is listed but left out; the person can still tick it. A review that cannot
/// tell ticks every file.
#[gpui::test]
fn a_threads_sheet_ticks_only_its_own_files(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened_owning(cx, Some(&["src/lib.rs"]));
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    cx.simulate_input("Mine");
    click(cx, "commit-commit");
    let mine = GitOp::Commit { paths: vec!["src/lib.rs".to_owned()], message: "Mine".to_owned() };
    assert!(asks(&sent).iter().any(|(_, op)| *op == mine), "only its own: {:?}", asks(&sent));
}

#[gpui::test]
fn a_sheet_that_cannot_tell_ticks_every_file(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened_owning(cx, None);
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    cx.simulate_input("All");
    click(cx, "commit-commit");
    let paths = vec!["src/lib.rs".to_owned(), "notes.md".to_owned()];
    let all = GitOp::Commit { paths, message: "All".to_owned() };
    assert!(asks(&sent).iter().any(|(_, op)| *op == all), "{:?}", asks(&sent));
}

fn answer(
    hub: &gpui::Entity<ThreadHub>,
    cx: &mut VisualTestContext,
    request: RequestId,
    done: GitDone,
) {
    hub.update(cx, |hub, cx| hub.git_done(request, GitOutcome::Done(done), cx));
    cx.run_until_parked();
}

/// The sheet asks the status and the pull request as it opens; the files ticked go in the
/// commit with the person's words, and "Commit and push" pushes only once the commit is made,
/// then the message is gone and the status is asked again.
#[gpui::test]
fn the_files_ticked_are_committed_and_pushed_after(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    assert!(cx.debug_bounds("commit-sheet").is_some(), "the sheet opens over the tile");
    let ops: Vec<GitOp> = asks(&sent).into_iter().map(|(_, op)| op).collect();
    assert_eq!(ops, [GitOp::Status, GitOp::PullStatus]);
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    assert!(cx.debug_bounds("commit-file-1").is_some(), "both files listed");
    click(cx, "commit-file-1");
    click(cx, "commit-and-push");
    assert!(
        !asks(&sent).iter().any(|(_, op)| matches!(op, GitOp::Commit { .. })),
        "no commit without the person's words"
    );
    cx.simulate_input("Fix the build");
    click(cx, "commit-and-push");
    let commit =
        GitOp::Commit { paths: vec!["src/lib.rs".to_owned()], message: "Fix the build".to_owned() };
    let request = last(&sent, &commit);
    assert!(!asks(&sent).iter().any(|(_, op)| *op == GitOp::Push), "no push before the commit");
    assert!(cx.debug_bounds("commit-busy").is_some(), "committing, said");
    let committed = GitDone::Committed { commit: "abcdef123".to_owned(), branch: None, files: 1 };
    answer(&hub, cx, request, committed);
    let after: Vec<GitOp> = asks(&sent).into_iter().map(|(_, op)| op).rev().take(2).collect();
    assert_eq!(after, [GitOp::Status, GitOp::Push], "the push once committed, then the status");
    let pushed = GitDone::Pushed {
        remote: "origin".to_owned(),
        branch: "feature".to_owned(),
        upstream_set: false,
        pull: Some(Box::new(pull("CLEAN", &["SUCCESS"]))),
    };
    answer(&hub, cx, last(&sent, &GitOp::Push), pushed);
    assert!(cx.debug_bounds("commit-said").is_some(), "pushed, said");
    assert!(cx.debug_bounds("commit-pull-standing").is_some(), "the pull request the push carried");
    assert!(cx.debug_bounds("thread-pull").is_none(), "the tile's header says it, not the foot");
}

/// The merge follows the forge's own summary, for the head on show, by the method chosen: a
/// failing check the base does not require (`UNSTABLE`) warns and still merges now.
#[gpui::test]
fn a_merge_goes_now_where_the_forge_allows_it_for_the_head_on_show(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let read = last(&sent, &GitOp::PullStatus);
    let unstable = pull("UNSTABLE", &["SUCCESS", "FAILURE"]);
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(unstable))));
    assert!(cx.debug_bounds("commit-merge-warn").is_some(), "the optional failure, warned");
    click(cx, "commit-merge-methods");
    click(cx, "commit-method-rebase");
    click(cx, "commit-delete-branch");
    click(cx, "commit-merge");
    assert_eq!(
        merges(&sent).last(),
        Some(&GitOp::Merge {
            method: "rebase".to_owned(),
            head: Some("c0ffee".to_owned()),
            delete_branch: false,
            auto: false,
        })
    );
}

/// The merges asked so far, in order.
fn merges(sent: &Sent) -> Vec<GitOp> {
    asks(sent)
        .into_iter()
        .map(|(_, op)| op)
        .filter(|op| matches!(op, GitOp::Merge { .. }))
        .collect()
}

/// A merge the forge blocks only on checks still running is offered as "Merge when ready"
/// (auto-merge); once gh took it, the row says what it waits on and offers it no more.
#[gpui::test]
fn a_merge_blocked_on_running_checks_is_offered_when_ready(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let read = last(&sent, &GitOp::PullStatus);
    let blocked = pull("BLOCKED", &["IN_PROGRESS"]);
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(blocked.clone()))));
    assert!(cx.debug_bounds("commit-merge-waits").is_some(), "what it waits on, said");
    click(cx, "commit-merge");
    let auto = GitOp::Merge {
        method: "squash".to_owned(),
        head: Some("c0ffee".to_owned()),
        delete_branch: true,
        auto: true,
    };
    assert_eq!(merges(&sent), std::slice::from_ref(&auto));
    let said = "Pull request #7 will be automatically merged via squash when all requirements \
                are met"
        .to_owned();
    let merged = GitDone::Merged { said, pull: Some(Box::new(blocked)) };
    answer(&hub, cx, last(&sent, &auto), merged);
    assert!(cx.debug_bounds("commit-merged").is_some(), "gh's words");
    assert!(cx.debug_bounds("commit-merge").is_none(), "not offered twice");
}

/// A required check that failed, or a conflict, leaves nothing to press: the sheet says what
/// the merge waits on, and the next steps above offer the fix.
#[gpui::test]
fn a_required_failure_leaves_the_merge_waiting(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let read = last(&sent, &GitOp::PullStatus);
    let blocked = pull("BLOCKED", &["FAILURE"]);
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(blocked))));
    assert!(cx.debug_bounds("commit-merge").is_none(), "no merge past a required failure");
    assert!(cx.debug_bounds("commit-merge-waits").is_some());
}

/// A draft offers "Ready for review", which asks the worker to mark it ready, and no merge.
#[gpui::test]
fn a_draft_is_marked_ready_from_the_sheet(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let read = last(&sent, &GitOp::PullStatus);
    let draft = PullStatus { draft: true, ..pull("DRAFT", &["SUCCESS"]) };
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(draft))));
    assert!(cx.debug_bounds("commit-merge").is_none(), "no merge of a draft");
    click(cx, "commit-mark-ready");
    assert!(asks(&sent).iter().any(|(_, op)| *op == GitOp::MarkReady), "{:?}", asks(&sent));
}

/// The gate over the forge's summaries: what merges now and with which warning, what waits on
/// something that clears by itself, and what waits on someone.
#[test]
fn the_merge_gate_reads_the_forge_s_summary() {
    use crate::conversation::thread::commit::{MergeGate, merge_gate};

    let now = |warn: Option<&str>| MergeGate::Now { warn: warn.map(str::to_owned) };
    let when = |waits: &str| MergeGate::WhenReady { waits: waits.to_owned() };
    let waits = |why: &str| MergeGate::Waits(why.to_owned());
    assert_eq!(merge_gate(&pull("CLEAN", &["SUCCESS"])), now(None));
    assert_eq!(merge_gate(&pull("HAS_HOOKS", &[])), now(None));
    let two = pull("UNSTABLE", &["FAILURE", "TIMED_OUT", "IN_PROGRESS"]);
    assert_eq!(merge_gate(&two), now(Some("Merges with 2 checks failing")));
    let one = pull("UNSTABLE", &["SUCCESS", "IN_PROGRESS"]);
    assert_eq!(merge_gate(&one), now(Some("Merges while 1 check still runs")));
    let asked = PullStatus { review: "CHANGES_REQUESTED".to_owned(), ..pull("CLEAN", &[]) };
    assert_eq!(merge_gate(&asked), now(Some("Merges with changes requested")));
    assert_eq!(merge_gate(&pull("BLOCKED", &["IN_PROGRESS"])), when("checks running"));
    let review = PullStatus { review: "REVIEW_REQUIRED".to_owned(), ..pull("BLOCKED", &[]) };
    assert_eq!(merge_gate(&review), when("a review"));
    assert_eq!(merge_gate(&pull("BLOCKED", &["FAILURE"])), waits("checks failing"));
    let blocked_asked =
        PullStatus { review: "CHANGES_REQUESTED".to_owned(), ..pull("BLOCKED", &[]) };
    assert_eq!(merge_gate(&blocked_asked), waits("changes requested"));
    assert_eq!(merge_gate(&pull("BEHIND", &[])), waits("behind main"));
    assert_eq!(merge_gate(&pull("DIRTY", &[])), waits("conflicts with main"));
    let conflicting = PullStatus { mergeable: "CONFLICTING".to_owned(), ..pull("CLEAN", &[]) };
    assert_eq!(merge_gate(&conflicting), waits("conflicts with main"));
    assert_eq!(merge_gate(&pull("UNKNOWN", &[])), waits("GitHub to finish checking it"));
    let draft = PullStatus { draft: true, ..pull("CLEAN", &[]) };
    assert_eq!(merge_gate(&draft), MergeGate::Draft);
}

/// A repository whose `origin` is on GitLab speaks of merge requests: before one is open the
/// sheet offers to open a merge request, and once one is, its number is written `!12` in the
/// sheet, and read as a merge request.
#[gpui::test]
fn a_gitlab_repository_s_sheet_speaks_of_merge_requests(cx: &mut TestAppContext) {
    use slopty_proto::git::Forge;

    let (hub, sent, cx) = opened(cx);
    cx.update(|window, _cx| window.set_a11y_active(true));
    let gitlab = GitStatus { forge: Some(Forge::GitLab), ..status() };
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(gitlab)));
    answer(&hub, cx, last(&sent, &GitOp::PullStatus), GitDone::PullStatus(None));
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        tree.iter().any(|n| n.is("Button", Some("Open merge request"))),
        "the way to open one, in GitLab's words"
    );
    let mr = PullStatus {
        forge: Forge::GitLab,
        number: 12,
        url: "https://gitlab.example.com/o/r/-/merge_requests/12".to_owned(),
        ..pull("CLEAN", &["SUCCESS"])
    };
    hub.update(cx, |hub, cx| {
        let _asked = hub.git_op("/w", GitOp::PullStatus, cx);
    });
    answer(&hub, cx, last(&sent, &GitOp::PullStatus), GitDone::PullStatus(Some(Box::new(mr))));
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(tree.iter().any(|n| n.is("Link", Some("Merge request 12"))), "{tree:#?}");
}

/// What git said when it refused is shown as it said it, and Close, or a press on the tile
/// round the sheet, takes the sheet away.
#[gpui::test]
fn a_refusal_reads_in_git_s_words_and_the_sheet_closes(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    cx.simulate_input("Fix");
    click(cx, "commit-commit");
    let commit = asks(&sent).into_iter().rev().find(|(_, op)| matches!(op, GitOp::Commit { .. }));
    let (request, _) = commit.expect("asked");
    let said = "pre-commit: cargo fmt would change src/lib.rs".to_owned();
    hub.update(cx, |hub, cx| hub.git_done(request, GitOutcome::Failed { said }, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("commit-failed").is_some(), "git's words under the buttons");
    click(cx, "commit-close");
    assert!(cx.debug_bounds("commit-sheet").is_none(), "closed");
}

/// A turn of the agent's, `state` as given.
fn turn(id: u32, state: slopty_proto::thread::TurnState) -> slopty_proto::thread::Turn {
    use slopty_proto::thread::{Changed, Turn, TurnId, Usage};
    Turn {
        id: TurnId(id),
        input: None,
        state,
        started_ms: slopty_core::WallMs::ZERO,
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

/// "Ask `<agent>` to commit" sends the thread's agent one message, queued after its turn so the
/// work in hand is not cut into, and says it waits. The repository is read again only once the
/// turn the message went into has ended; the agent's own commit is then what the sheet shows.
#[gpui::test]
fn the_agent_is_asked_to_commit_and_the_sheet_reads_again_after_its_turn(cx: &mut TestAppContext) {
    use slopty_proto::thread::wire::Intent;
    use slopty_proto::thread::{Cap, Delivery, TurnState};

    use crate::conversation::thread::commit::ASK_TO_COMMIT;

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)];
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    let read = asks(&sent).iter().filter(|(_, op)| *op == GitOp::Status).count();

    click(cx, "commit-ask");
    let said: Vec<Intent> = sent
        .borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(slopty_proto::thread::wire::ThreadRequest::Intent {
                intent, ..
            }) => Some(intent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        [Intent::Send {
            text: ASK_TO_COMMIT.to_owned(),
            delivery: Delivery::Queue,
            attachments: Vec::new(),
        }],
        "one message, after the turn"
    );
    assert!(cx.debug_bounds("commit-asked").is_some(), "the sheet says it waits");

    let statuses = |sent: &Sent| asks(sent).iter().filter(|(_, op)| *op == GitOp::Status).count();
    state.turns = vec![turn(1, TurnState::Active)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 2), cx));
    cx.run_until_parked();
    assert_eq!(statuses(&sent), read, "not read while the agent works");
    state.turns = vec![turn(1, TurnState::Complete)];
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 3), cx));
    cx.run_until_parked();
    assert_eq!(statuses(&sent), read.saturating_add(1), "read again once its turn ended");
    assert!(cx.debug_bounds("commit-asked").is_none(), "and waits no more");
}

/// An ask the worker turns down says why under the buttons, and the ask can go again.
#[gpui::test]
fn an_ask_to_commit_turned_down_says_why(cx: &mut TestAppContext) {
    use slopty_proto::thread::Cap;
    use slopty_proto::thread::wire::{IntentDone, Outcome, ThreadRequest};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::QUEUE)];
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    click(cx, "commit-ask");
    let id = sent
        .borrow()
        .iter()
        .find_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent { id, .. }) => Some(*id),
            _ => None,
        })
        .expect("asked");
    let reason = "The agent is not running".to_owned();
    hub.update(cx, |hub, cx| {
        hub.done(&IntentDone { id, outcome: Outcome::Refused { reason } }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("commit-ask-refused").is_some(), "why, under the buttons");
    assert!(cx.debug_bounds("commit-asked").is_none());
}

/// A thread whose agent takes no message has no one to ask: the sheet offers only the
/// person's commit.
#[gpui::test]
fn an_agent_that_takes_no_message_is_not_asked(cx: &mut TestAppContext) {
    let (_hub, _sent, cx) = opened(cx);
    assert!(cx.debug_bounds("commit-sheet").is_some());
    assert!(cx.debug_bounds("commit-ask").is_none(), "nothing to ask");
}

/// The messages sent to the thread's agent, in order.
fn told(sent: &Sent) -> Vec<String> {
    use slopty_proto::thread::wire::{Intent, ThreadRequest};
    sent.borrow()
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent {
                intent: Intent::Send { text, .. }, ..
            }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// A thread whose agent queues, its commit sheet open over its pull request.
fn opened_with_agent(
    cx: &mut TestAppContext,
) -> (gpui::Entity<ThreadHub>, Sent, &mut VisualTestContext) {
    use slopty_proto::thread::Cap;
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    state.meta.caps = vec![Cap::named(Cap::QUEUE)];
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    (hub, sent, cx)
}

/// Over a pull request whose checks failed, the sheet offers to ask the thread's agent to fix
/// them: one message, after its turn, naming each check that failed with its workflow and page
/// (not the ones that passed) and how the forge shows why. The sheet then waits on the agent.
#[gpui::test]
fn the_sheet_asks_the_agent_to_fix_the_checks_naming_them(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened_with_agent(cx);
    let mut failing = pull("BEHIND", &["SUCCESS", "FAILURE", "FAILURE"]);
    failing.checks[1].link = Some("https://github.com/o/r/actions/runs/9".to_owned());
    failing.checks[2].name = "lint".to_owned();
    answer(&hub, cx, last(&sent, &GitOp::PullStatus), GitDone::PullStatus(Some(Box::new(failing))));
    click(cx, "commit-fix-checks");
    let [fix] = told(&sent).try_into().expect("one message");
    assert!(fix.starts_with("These checks failed on pull request #7:"), "{fix}");
    assert!(fix.contains("- ci FAILURE (CI): https://github.com/o/r/actions/runs/9"), "{fix}");
    assert!(fix.contains("- lint (CI)") && !fix.contains("ci SUCCESS"), "{fix}");
    assert!(fix.contains("`gh pr checks 7`"), "{fix}");
    assert!(cx.debug_bounds("commit-asked").is_some(), "the sheet waits on the agent");
}

/// The review still open, read with the open pull request, is a step of its own: one message
/// naming each point where it is, the reviewer's own words first and every note in order. A
/// branch behind its base is offered a catch-up beside it.
#[gpui::test]
fn the_sheet_asks_the_agent_to_address_the_review_and_to_catch_up(cx: &mut TestAppContext) {
    use slopty_proto::git::{PullComments, PullNote, PullThread};

    let (hub, sent, cx) = opened_with_agent(cx);
    answer(
        &hub,
        cx,
        last(&sent, &GitOp::PullStatus),
        GitDone::PullStatus(Some(Box::new(pull("BEHIND", &["SUCCESS"])))),
    );
    assert!(cx.debug_bounds("commit-fix-checks").is_none(), "no check failed");
    let note =
        |author: &str, body: &str| PullNote { author: author.to_owned(), body: body.to_owned() };
    let comments = PullComments {
        number: 7,
        threads: vec![
            PullThread {
                path: None,
                line: None,
                outdated: false,
                url: None,
                notes: vec![note("ada", "Split the parser out first.")],
            },
            PullThread {
                path: Some("src/lib.rs".to_owned()),
                line: Some(42),
                outdated: true,
                url: None,
                notes: vec![note("sam", "This unwrap panics."), note("ada", "Agreed.")],
            },
        ],
        more: 0,
    };
    let review = GitOp::PullComments { number: 7 };
    answer(&hub, cx, last(&sent, &review), GitDone::PullComments(Box::new(comments)));

    click(cx, "commit-address-review");
    let [address] = told(&sent).try_into().expect("one message");
    assert!(address.starts_with("Address the review still open on pull request #7"), "{address}");
    assert!(address.contains("1. The review:\n   ada: Split the parser out first."), "{address}");
    assert!(
        address.contains(
            "2. src/lib.rs, line 42 (the code has changed since):\n   sam: This unwrap panics.\n   \
             ada: Agreed."
        ),
        "{address}"
    );
    assert!(cx.debug_bounds("commit-bring-up-to-date").is_some(), "behind main");
}

/// Where no agent takes a message the sheet offers no next step, however the pull request
/// stands.
#[gpui::test]
fn no_next_step_without_an_agent_to_take_it(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    answer(
        &hub,
        cx,
        last(&sent, &GitOp::PullStatus),
        GitDone::PullStatus(Some(Box::new(pull("BEHIND", &["FAILURE"])))),
    );
    assert!(cx.debug_bounds("commit-pull").is_some());
    assert!(cx.debug_bounds("commit-next-steps").is_none());
}

/// Once the pull request of an agent's worktree merged, the sheet offers to remove the
/// worktree, which the tile hands to the workspace; never while the pull request is open, nor
/// in a folder that is no agent's worktree, and not once it went.
#[gpui::test]
fn a_merged_pull_request_offers_to_remove_its_worktree(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::conversation::thread::ThreadViewEvent;

    const ROOT: &str = "/r/.claude/worktrees/fix";
    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    ROOT.clone_into(&mut state.meta.cwd);
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    let heard: Rc<RefCell<Vec<String>>> = Rc::default();
    let into = Rc::clone(&heard);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_v, event: &ThreadViewEvent, _cx| {
            if let ThreadViewEvent::EndAndRemove(root) = event {
                into.borrow_mut().push(root.clone());
            }
        })
        .detach();
    });
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    let read = |sent: &Sent| {
        sent.borrow()
            .iter()
            .rev()
            .find_map(|m| match m {
                ClientMsg::Git { request, op: GitOp::PullStatus, .. } => Some(*request),
                _ => None,
            })
            .expect("the pull request read")
    };
    answer(&hub, cx, read(&sent), GitDone::PullStatus(Some(Box::new(pull("CLEAN", &[])))));
    assert!(cx.debug_bounds("commit-remove-worktree").is_none(), "not while it is open");

    hub.update(cx, |hub, cx| {
        let _asked = hub.git_op(ROOT, GitOp::PullStatus, cx);
    });
    let merged = PullStatus { state: "MERGED".to_owned(), ..pull("CLEAN", &[]) };
    answer(&hub, cx, read(&sent), GitDone::PullStatus(Some(Box::new(merged))));
    cx.update(|window, _cx| {
        window.set_a11y_active(true);
        window.refresh();
    });
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let end = crate::conversation::thread::commit::end_and_remove("Claude Code");
    assert!(
        tree.iter().any(|n| n.label.as_deref() == Some(end.as_str())),
        "its agent still runs there, so the press ends it first: {tree:#?}"
    );
    click(cx, "commit-remove-worktree");
    assert_eq!(*heard.borrow(), [ROOT], "handed to the workspace, by its root");

    // The workspace asks it of the worker; once it went, the press goes too.
    let asked = hub.update(cx, |hub, cx| hub.git_op(ROOT, GitOp::RemoveWorktree, cx));
    let freed = GitDone::WorktreeRemoved { branch: Some("fix".to_owned()), branch_removed: true };
    answer(&hub, cx, asked.expect("linked"), freed);
    assert!(cx.debug_bounds("commit-remove-worktree").is_none(), "gone once it went");
}

/// A merged pull request in a folder that is no agent's worktree offers nothing to remove.
#[gpui::test]
fn a_merged_pull_request_outside_a_worktree_offers_no_removal(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let merged = PullStatus { state: "MERGED".to_owned(), ..pull("CLEAN", &[]) };
    answer(&hub, cx, last(&sent, &GitOp::PullStatus), GitDone::PullStatus(Some(Box::new(merged))));
    assert!(cx.debug_bounds("commit-pull-standing").is_some(), "the merged pull request shows");
    assert!(cx.debug_bounds("commit-remove-worktree").is_none());
}

/// The worker reads the pull request on its own clock and says it on the thread's row: while the
/// sheet is up, a row whose pull request moved (merged on the forge's page) has the sheet ask
/// its own again at once; a table that leaves it as it was asks nothing.
#[gpui::test]
fn the_sheet_asks_again_when_the_rows_pull_request_moves(cx: &mut TestAppContext) {
    use slopty_proto::git::Forge;
    use slopty_proto::thread::Cursor;
    use slopty_proto::thread::wire::{PullSeen, PullStands, TableFrame};

    let (hub, sent) = hub(cx, None);
    let mut state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state.clone(), 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    let reads = |sent: &Sent| asks(sent).iter().filter(|(_, op)| *op == GitOp::PullStatus).count();
    let opened = reads(&sent);
    let table = |state: &slopty_proto::thread::ThreadState, seq, pull: Option<PullSeen>| {
        let mut row = state.row(slopty_core::WallMs::ZERO);
        row.pull = pull;
        TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, rows: vec![row] }
    };
    let seen = |stands| PullSeen {
        forge: Forge::GitHub,
        number: 7,
        url: "https://github.com/o/r/pull/7".to_owned(),
        title: "Fix".to_owned(),
        base: "main".to_owned(),
        stands,
        failed: 0,
        failed_first: None,
        running: 0,
    };
    state.meta.title = "edit".to_owned();
    hub.update(cx, |hub, cx| hub.table(&table(&state, 1, None), cx));
    cx.run_until_parked();
    assert_eq!(reads(&sent), opened, "no pull request on the row, nothing moved");
    hub.update(cx, |hub, cx| hub.table(&table(&state, 2, Some(seen(PullStands::Running))), cx));
    cx.run_until_parked();
    assert_eq!(reads(&sent), opened.saturating_add(1), "one came to the row");
    hub.update(cx, |hub, cx| hub.table(&table(&state, 3, Some(seen(PullStands::Running))), cx));
    cx.run_until_parked();
    assert_eq!(reads(&sent), opened.saturating_add(1), "the same again asks nothing");
    hub.update(cx, |hub, cx| hub.table(&table(&state, 4, Some(seen(PullStands::Merged))), cx));
    cx.run_until_parked();
    assert_eq!(reads(&sent), opened.saturating_add(2), "merged elsewhere, asked again");
}

/// A commit refused by a hook says git's words, and "Ask `<agent>` to fix" tells the agent
/// them whole, to fix what stopped it and leave the commit to the person.
#[gpui::test]
fn a_failed_commit_offers_to_ask_the_agent_to_fix_it(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened_as(cx, Some(&["src/lib.rs", "notes.md"]), true);
    answer(&hub, cx, last(&sent, &GitOp::Status), GitDone::Status(Box::new(status())));
    cx.simulate_input("Fix");
    click(cx, "commit-commit");
    let commit = asks(&sent).into_iter().rev().find(|(_, op)| matches!(op, GitOp::Commit { .. }));
    let (request, _) = commit.expect("asked");
    let said = "pre-commit: cargo fmt would change src/lib.rs".to_owned();
    hub.update(cx, |hub, cx| hub.git_done(request, GitOutcome::Failed { said: said.clone() }, cx));
    cx.run_until_parked();
    click(cx, "commit-ask-fix");
    assert_eq!(told(&sent), [crate::conversation::thread::commit::fix_commit_words(&said)]);
    assert!(told(&sent)[0].contains(&said), "git's words whole");
}

/// A sheet holding words stays up through a stray press round it and Esc; Close takes it away,
/// and the next sheet on the folder opens on the same words. An empty one goes on either.
#[gpui::test]
fn a_sheet_with_words_stays_and_its_words_come_back(cx: &mut TestAppContext) {
    let (_hub, _sent, cx) = opened(cx);
    cx.simulate_input("Half a message");
    let sheet = cx.debug_bounds("commit-sheet").expect("the sheet");
    let outside = gpui::point(sheet.left() + gpui::px(2.0), sheet.bottom() + gpui::px(8.0));
    cx.simulate_click(outside, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("commit-sheet").is_some(), "a stray press keeps the words up");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("commit-sheet").is_some(), "so does Esc");
    click(cx, "commit-close");
    assert!(cx.debug_bounds("commit-sheet").is_none(), "Close closes");
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    cx.update(|window, _cx| {
        window.set_a11y_active(true);
        window.refresh();
    });
    cx.run_until_parked();
    let nodes = cx.update(|window, _cx| crate::a11y::tree(window));
    assert!(
        nodes.iter().any(|n| n.value.as_deref() == Some("Half a message")),
        "the words came back: {nodes:#?}"
    );
    cx.simulate_keystrokes("cmd-a backspace");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("commit-sheet").is_none(), "an empty sheet goes on Esc");
}
