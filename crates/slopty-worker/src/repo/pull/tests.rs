use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use slopty_proto::git::{GitOp, PullStanding};

use super::*;
use crate::repo::commit::{Programs, apply};

/// What the stand-in gh answers `gh pr view` with: a pull request on GitHub with a passed job,
/// a running one and a failed commit status.
const VIEW: &str = r#"{"number":7,"url":"https://github.com/o/demo/pull/7","title":"Keep it",
"state":"OPEN","isDraft":false,"headRefName":"feature","headRefOid":"0123abcd",
"baseRefName":"main","reviewDecision":"REVIEW_REQUIRED","mergeable":"MERGEABLE",
"mergeStateStatus":"BLOCKED","statusCheckRollup":[
{"__typename":"CheckRun","name":"test","workflowName":"CI","status":"COMPLETED",
 "conclusion":"SUCCESS","detailsUrl":"https://github.com/o/demo/actions/runs/1"},
{"__typename":"CheckRun","name":"lint","workflowName":"CI","status":"IN_PROGRESS",
 "conclusion":"","detailsUrl":"https://github.com/o/demo/actions/runs/2"},
{"__typename":"StatusContext","context":"deploy/preview","state":"FAILURE",
 "targetUrl":"https://preview.example/7"}]}"#;

/// A stand-in for gh in `dir` that records each call's arguments, one per line, and answers
/// as `mode` says: `none` for a branch with no pull request, `refuse` for a merge gh refuses,
/// else the pull request in [`VIEW`]. No real gh runs, so no one's GitHub sign-in is reached.
fn stand_in(dir: &Path, mode: &str) -> PathBuf {
    let gh = dir.join("gh");
    std::fs::write(dir.join("view.json"), VIEW).expect("written");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{dir}/asked\"\n\
         case \"$1 $2\" in\n\
         'pr view') if [ {mode} = none ]; then echo 'no pull requests found for branch \"feature\"' >&2; exit 1; fi\n\
           cat \"{dir}/view.json\" ;;\n\
         'pr merge') if [ {mode} = refuse ]; then echo 'Pull request #7 is not mergeable: the base branch policy prohibits the merge.' >&2; exit 1; fi\n\
           echo '✓ Squashed and merged pull request #7 (Keep it)' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = dir.display()
    );
    std::fs::write(&gh, script).expect("written");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("executable");
    gh
}

/// A repository with one commit, so the worker finds its root.
fn repo(dir: &Path) -> PathBuf {
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("made");
    let git = crate::changes::git().expect("git");
    let ran = std::process::Command::new(git)
        .arg("-C")
        .arg(&work)
        .args(["init", "-q", "-b", "feature"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git runs");
    assert!(ran.status.success());
    work
}

fn asked(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("asked")).unwrap_or_default();
    text.lines().map(str::to_owned).collect()
}

/// gh's answer is read in the forge's own words: a job's conclusion, a running job's status, a
/// commit status's state, each with its page; the pull request stands on its failed check.
#[tokio::test]
async fn a_pull_request_s_status_is_read_in_the_forge_s_words() {
    let dir = tempfile::tempdir().expect("temp");
    let programs = Programs {
        git: crate::changes::git().map(Path::to_path_buf),
        gh: Some(stand_in(dir.path(), "open")),
    };
    let work = repo(dir.path());
    let GitOutcome::Done(GitDone::PullStatus(Some(pull))) =
        apply(&programs, &work.to_string_lossy(), GitOp::PullStatus, &[]).await
    else {
        panic!("no pull request read")
    };
    assert_eq!(
        (pull.number, pull.head.as_str(), pull.head_commit.as_str()),
        (7, "feature", "0123abcd")
    );
    assert_eq!((pull.review.as_str(), pull.merge_state.as_str()), ("REVIEW_REQUIRED", "BLOCKED"));
    let checks: Vec<(&str, &str, Option<&str>)> = pull
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.state.as_str(), c.link.as_deref()))
        .collect();
    assert_eq!(
        checks,
        [
            ("test", "SUCCESS", Some("https://github.com/o/demo/actions/runs/1")),
            ("lint", "IN_PROGRESS", Some("https://github.com/o/demo/actions/runs/2")),
            ("deploy/preview", "FAILURE", Some("https://preview.example/7")),
        ]
    );
    assert_eq!(pull.standing(), PullStanding::Failing);
    assert_eq!(asked(dir.path()), [format!("pr view --json {FIELDS}")]);
}

/// A branch with no pull request reads as none, not as a failure; a worker without gh says so.
#[tokio::test]
async fn a_branch_with_no_pull_request_reads_as_none() {
    let dir = tempfile::tempdir().expect("temp");
    let git = crate::changes::git().map(Path::to_path_buf);
    let work = repo(dir.path()).to_string_lossy().into_owned();
    let none = Programs { git: git.clone(), gh: Some(stand_in(dir.path(), "none")) };
    let read = apply(&none, &work, GitOp::PullStatus, &[]).await;
    assert_eq!(read, GitOutcome::Done(GitDone::PullStatus(None)));
    let without = Programs { git, gh: None };
    let missing = apply(&without, &work, GitOp::PullStatus, &[]).await;
    assert!(
        matches!(&missing, GitOutcome::Unavailable { program, .. } if program == "gh"),
        "{missing:?}"
    );
}

/// A merge goes by the method named, only at the head the person looked at, deleting the
/// branch when asked, and says what gh did with the pull request as it stands after; a method
/// gh does not take is refused before gh runs, and gh's own refusal comes back in its words.
#[tokio::test]
async fn a_merge_goes_as_the_person_said_and_a_refusal_in_gh_s_words() {
    let dir = tempfile::tempdir().expect("temp");
    let git = crate::changes::git().map(Path::to_path_buf);
    let work = repo(dir.path()).to_string_lossy().into_owned();
    let programs = Programs { git: git.clone(), gh: Some(stand_in(dir.path(), "open")) };
    let merge = |method: &str| GitOp::Merge {
        method: method.to_owned(),
        head: Some("0123abcd".to_owned()),
        delete_branch: true,
    };
    let GitOutcome::Done(GitDone::Merged { said, pull }) =
        apply(&programs, &work, merge("Squash"), &[]).await
    else {
        panic!("not merged")
    };
    assert!(said.contains("Squashed and merged"), "{said}");
    assert_eq!(pull.map(|p| p.number), Some(7));
    let calls = asked(dir.path());
    assert_eq!(
        calls.first().map(String::as_str),
        Some("pr merge --squash --match-head-commit 0123abcd --delete-branch")
    );

    let refused = apply(&programs, &work, merge("fast-forward"), &[]).await;
    assert!(
        matches!(&refused, GitOutcome::Refused { why } if why.contains("merge, squash, rebase")),
        "{refused:?}"
    );
    assert_eq!(asked(dir.path()).len(), calls.len(), "gh not asked");

    let strict = Programs { git, gh: Some(stand_in(dir.path(), "refuse")) };
    let said = apply(&strict, &work, merge("merge"), &[]).await;
    assert!(
        matches!(&said, GitOutcome::Failed { said } if said.contains("base branch policy")),
        "{said:?}"
    );
}
