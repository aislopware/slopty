use super::CheckBucket::{Failed, Passed, Running, Skipped};
use super::*;

fn check(state: &str) -> PullCheck {
    PullCheck { name: state.to_owned(), workflow: None, state: state.to_owned(), link: None }
}

fn open(checks: &[&str]) -> PullStatus {
    PullStatus {
        forge: Forge::GitHub,
        number: 7,
        url: "https://github.com/o/r/pull/7".to_owned(),
        title: "t".to_owned(),
        state: "OPEN".to_owned(),
        draft: false,
        head: "feature".to_owned(),
        head_commit: "abc".to_owned(),
        base: "main".to_owned(),
        review: String::new(),
        mergeable: "MERGEABLE".to_owned(),
        merge_state: "CLEAN".to_owned(),
        checks: checks.iter().map(|s| check(s)).collect(),
        more_checks: 0,
        methods: ["merge", "squash", "rebase"].map(str::to_owned).to_vec(),
        method: "squash".to_owned(),
    }
}

/// A check's state reads in buckets whatever its case, and one the forge adds later reads as
/// still running rather than as passed.
#[test]
fn a_check_s_state_is_bucketed_and_an_unknown_one_still_runs() {
    let buckets: Vec<CheckBucket> =
        ["SUCCESS", "neutral", "SKIPPED", "TIMED_OUT", "QUEUED", "SOMETHING_NEW"]
            .map(|s| check(s).bucket())
            .to_vec();
    assert_eq!(buckets, [Passed, Passed, Skipped, Failed, Running, Running]);
}

/// A pull request stands where the first fact that holds puts it: merged or closed, then a
/// conflict, a failed check, changes asked for (each even on a draft), a draft, checks still
/// running, ready, or waiting on review. Case does not matter, and a dirty merge state is a
/// conflict.
#[test]
fn a_pull_request_stands_on_its_most_pressing_fact() {
    let with = |f: fn(&mut PullStatus)| {
        let mut status = open(&["SUCCESS", "SKIPPED"]);
        f(&mut status);
        status.standing()
    };
    assert_eq!(with(|_| {}), PullStands::Ready);
    assert_eq!(with(|s| s.state = "MERGED".to_owned()), PullStands::Merged);
    assert_eq!(with(|s| s.state = "closed".to_owned()), PullStands::Closed);
    assert_eq!(with(|s| s.mergeable = "CONFLICTING".to_owned()), PullStands::Conflicted);
    assert_eq!(with(|s| s.merge_state = "DIRTY".to_owned()), PullStands::Conflicted);
    let failed_conflicted = |s: &mut PullStatus| {
        s.checks.push(check("FAILURE"));
        s.mergeable = "CONFLICTING".to_owned();
    };
    assert_eq!(with(failed_conflicted), PullStands::Conflicted, "a conflict first");
    let failed_draft = |s: &mut PullStatus| {
        s.checks.push(check("FAILURE"));
        s.draft = true;
    };
    assert_eq!(with(failed_draft), PullStands::ChecksFailed, "a failing draft needs fixing");
    let changes = |s: &mut PullStatus| s.review = "CHANGES_REQUESTED".to_owned();
    assert_eq!(with(changes), PullStands::ChangesRequested);
    assert_eq!(with(|s| s.draft = true), PullStands::Draft);
    assert_eq!(with(|s| s.checks.push(check("IN_PROGRESS"))), PullStands::Running);
    assert_eq!(with(|s| s.merge_state = "unstable".to_owned()), PullStands::Ready);
    let blocked = |s: &mut PullStatus| {
        s.review = "REVIEW_REQUIRED".to_owned();
        s.merge_state = "BLOCKED".to_owned();
    };
    assert_eq!(with(blocked), PullStands::Waiting);
}
