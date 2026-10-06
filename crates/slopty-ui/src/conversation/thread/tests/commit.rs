//! The commit sheet over a thread's tile: the files ticked go in the commit with the person's
//! words, a push follows the commit only once it is made, the pull request stands over the
//! files with a merge offered only while it is ready, and git's refusals read in its words.

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
        branch: Some("feature".to_owned()),
        head: Some("abc".to_owned()),
        upstream: Some("origin/feature".to_owned()),
        ahead: 0,
        behind: 0,
        files: vec![file(".M", "src/lib.rs"), file("??", "notes.md")],
        more: 0,
    }
}

fn pull(merge_state: &str, checks: &[&str]) -> PullStatus {
    PullStatus {
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
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} drawn")).center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// A thread at `/w`, its view open with the commit sheet opened from the "+" menu.
fn opened(cx: &mut TestAppContext) -> (gpui::Entity<ThreadHub>, Sent, &mut VisualTestContext) {
    let (hub, sent) = hub(cx, None);
    let state = fixtures::empty();
    let thread = state.meta.id;
    hub.update(cx, ThreadHub::connected);
    let (_view, cx) = view(cx, &hub, thread);
    hub.update(cx, |hub, cx| hub.frame(thread, snapshot(state, 1), cx));
    cx.run_until_parked();
    click(cx, "thread-attach");
    click(cx, "thread-add-menu-commit");
    (hub, sent, cx)
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
    assert!(cx.debug_bounds("thread-pull").is_some(), "and its number in the composer");
}

/// The merge is offered only while the pull request is ready, for the head on show, by the
/// method chosen; otherwise the sheet says what it waits on.
#[gpui::test]
fn a_merge_is_offered_only_while_ready_for_the_head_on_show(cx: &mut TestAppContext) {
    let (hub, sent, cx) = opened(cx);
    let read = last(&sent, &GitOp::PullStatus);
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(pull("CLEAN", &["IN_PROGRESS"])))));
    assert!(cx.debug_bounds("commit-merge").is_none(), "no merge while checks run");
    assert!(cx.debug_bounds("commit-merge-waits").is_some());
    hub.update(cx, |hub, cx| {
        let _asked = hub.git_op("/w", GitOp::PullStatus, cx);
    });
    let read = last(&sent, &GitOp::PullStatus);
    answer(&hub, cx, read, GitDone::PullStatus(Some(Box::new(pull("CLEAN", &["SUCCESS"])))));
    click(cx, "commit-merge-methods");
    click(cx, "commit-method-rebase");
    click(cx, "commit-delete-branch");
    click(cx, "commit-merge");
    let merge = asks(&sent).into_iter().rev().find_map(|(_, op)| match op {
        GitOp::Merge { .. } => Some(op),
        _ => None,
    });
    assert_eq!(
        merge,
        Some(GitOp::Merge {
            method: "rebase".to_owned(),
            head: Some("c0ffee".to_owned()),
            delete_branch: false,
        })
    );
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
