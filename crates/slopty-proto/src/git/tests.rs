use super::CheckBucket::{Failed, Passed, Running, Skipped};
use super::*;

fn check(state: &str) -> PullCheck {
    PullCheck { name: state.to_owned(), workflow: None, state: state.to_owned(), link: None }
}

fn open(checks: &[&str]) -> PullStatus {
    PullStatus {
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

/// A pull request stands on its worst fact: merged or closed first, then a failed check, a
/// conflict, changes asked for, a draft, checks still running, ready, or waiting on review.
#[test]
fn a_pull_request_stands_on_its_most_pressing_fact() {
    let with = |f: fn(&mut PullStatus)| {
        let mut status = open(&["SUCCESS", "SKIPPED"]);
        f(&mut status);
        status.standing()
    };
    assert_eq!(with(|_| {}), PullStanding::Ready);
    assert_eq!(with(|s| s.state = "MERGED".to_owned()), PullStanding::Merged);
    assert_eq!(with(|s| s.state = "CLOSED".to_owned()), PullStanding::Closed);
    assert_eq!(with(|s| s.checks.push(check("FAILURE"))), PullStanding::Failing);
    assert_eq!(with(|s| s.mergeable = "CONFLICTING".to_owned()), PullStanding::Conflicting);
    let changes = |s: &mut PullStatus| s.review = "CHANGES_REQUESTED".to_owned();
    assert_eq!(with(changes), PullStanding::ChangesRequested);
    assert_eq!(with(|s| s.draft = true), PullStanding::Draft);
    assert_eq!(with(|s| s.checks.push(check("IN_PROGRESS"))), PullStanding::Running);
    let blocked = |s: &mut PullStatus| {
        s.review = "REVIEW_REQUIRED".to_owned();
        s.merge_state = "BLOCKED".to_owned();
    };
    assert_eq!(with(blocked), PullStanding::Waiting);
    assert!(PullStanding::Failing < PullStanding::Ready, "the most pressing ranks first");
}
