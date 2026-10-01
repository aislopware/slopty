use std::path::PathBuf;

use super::*;

fn git_in(dir: &Path, args: &[&str]) -> String {
    let git = crate::changes::git().expect("git");
    let out = std::process::Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).expect("write");
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-q", "-m", &format!("{file}: {text}")]);
    git_in(dir, &["rev-parse", "HEAD"])
}

/// A reviewer's checkout is the task's head, apart from the clone's own checkout, with the
/// diff from where the work left the target beside it: the task's changes and nothing the
/// target gained since. Made again for a later head, it holds that head's diff alone, and git
/// sees the diff as no change to the work.
#[tokio::test]
async fn a_reviewer_reads_the_task_s_own_diff_in_a_checkout_of_its_own() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let repo = tmp.path().join("demo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    let fork = commit(&repo, "a.txt", "one\n");
    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/1"]);
    let first = commit(&repo, "b.txt", "the task's\n");
    git_in(&repo, &["switch", "-q", "main"]);
    commit(&repo, "c.txt", "main moved on\n");
    let place: PathBuf = tmp.path().join("places/demo-review-1");

    let made = checkout(git, &repo, &place, "slopty/demo/1", "main").await.expect("checked out");
    assert_eq!((made.head.as_str(), made.base.as_str()), (first.as_str(), fork.as_str()));
    assert_eq!(git_in(&place, &["rev-parse", "HEAD"]), first);
    let diff = std::fs::read_to_string(place.join(REVIEW_DIFF)).expect("the diff");
    assert!(diff.starts_with(&format!("git diff {fork}..{first}")), "{diff}");
    assert!(diff.contains("+the task's"), "the task's change: {diff}");
    assert!(!diff.contains("main moved on"), "nothing the target gained since: {diff}");
    assert_eq!(git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the clone's own");

    git_in(&repo, &["switch", "-q", "slopty/demo/1"]);
    let second = commit(&repo, "b.txt", "the task's, fixed\n");
    git_in(&repo, &["switch", "-q", "main"]);
    let again = checkout(git, &repo, &place, &second, "main").await.expect("again");
    let diff = std::fs::read_to_string(place.join(REVIEW_DIFF)).expect("the diff");
    assert_eq!(again.head, second);
    assert!(diff.contains("+the task's, fixed") && !diff.contains("+the task's\n"), "{diff}");
    let status = git_in(&place, &["status", "--porcelain", "--untracked-files=no"]);
    assert!(status.is_empty(), "the work itself unchanged: {status}");
}
