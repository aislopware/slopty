use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use slopty_proto::git::{Forge, GitOp, PullStanding};

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
    std::fs::write(dir.join("review.json"), REVIEW).expect("written");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{dir}/asked\"\n\
         case \"$1 $2\" in\n\
         'pr view') if [ {mode} = none ]; then echo 'no pull requests found for branch \"feature\"' >&2; exit 1; fi\n\
           cat \"{dir}/view.json\" ;;\n\
         'pr merge') if [ {mode} = refuse ]; then echo 'Pull request #7 is not mergeable: the base branch policy prohibits the merge.' >&2; exit 1; fi\n\
           echo '✓ Squashed and merged pull request #7 (Keep it)' ;;\n\
         'api graphql'*) cat \"{dir}/review.json\" ;;\n\
         'pr create') echo 'https://github.com/o/demo/pull/7' ;;\n\
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
        glab: None,
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
    let none = Programs { git: git.clone(), gh: Some(stand_in(dir.path(), "none")), glab: None };
    let read = apply(&none, &work, GitOp::PullStatus, &[]).await;
    assert_eq!(read, GitOutcome::Done(GitDone::PullStatus(None)));
    let without = Programs { git, gh: None, glab: None };
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
    let programs =
        Programs { git: git.clone(), gh: Some(stand_in(dir.path(), "open")), glab: None };
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

    let strict = Programs { git, gh: Some(stand_in(dir.path(), "refuse")), glab: None };
    let said = apply(&strict, &work, merge("merge"), &[]).await;
    assert!(
        matches!(&said, GitOutcome::Failed { said } if said.contains("base branch policy")),
        "{said:?}"
    );
}

/// What the stand-in glab answers `glab mr view --output json` with: the API's merge request,
/// its fields as GitLab 18 sends them, cut to what is read and a few beside, names made up.
const MR_VIEW: &str = r#"{"id":91,"iid":12,"project_id":4,"title":"Keep it","description":"Why.",
"state":"opened","draft":false,"work_in_progress":false,"source_branch":"feature",
"target_branch":"main","sha":"0123abcd","merge_status":"can_be_merged",
"detailed_merge_status":"ci_still_running","has_conflicts":false,
"web_url":"https://gitlab.example.com/o/demo/-/merge_requests/12",
"head_pipeline":{"id":77,"iid":5,"project_id":4,"sha":"0123abcd","ref":"feature",
"status":"failed","source":"merge_request_event",
"web_url":"https://gitlab.example.com/o/demo/-/pipelines/77",
"detailed_status":{"group":"failed"}}}"#;

/// The jobs of pipeline 77, as `GET projects/:id/pipelines/77/jobs` lists them: one passed,
/// one failed, one allowed to fail, one running and a manual one never started.
const MR_JOBS: &str = r#"[
{"id":1,"name":"test","stage":"test","status":"success","allow_failure":false,
 "web_url":"https://gitlab.example.com/o/demo/-/jobs/1"},
{"id":2,"name":"lint","stage":"test","status":"failed","allow_failure":false,
 "web_url":"https://gitlab.example.com/o/demo/-/jobs/2"},
{"id":3,"name":"audit","stage":"test","status":"failed","allow_failure":true,
 "web_url":"https://gitlab.example.com/o/demo/-/jobs/3"},
{"id":4,"name":"build","stage":"build","status":"running","allow_failure":false,
 "web_url":"https://gitlab.example.com/o/demo/-/jobs/4"},
{"id":5,"name":"deploy","stage":"deploy","status":"manual","allow_failure":true,
 "web_url":"https://gitlab.example.com/o/demo/-/jobs/5"}]"#;

/// Merge request !5, merged, with no pipeline.
const MR_MERGED: &str = r#"{"id":80,"iid":5,"project_id":4,"title":"Kept","state":"merged",
"draft":false,"source_branch":"feature","target_branch":"main","sha":"89abcdef",
"merge_status":"can_be_merged","detailed_merge_status":"not_open","has_conflicts":false,
"web_url":"https://gitlab.example.com/o/demo/-/merge_requests/5","head_pipeline":null}"#;

/// A stand-in for glab in `dir` that records each call's arguments and answers as `mode`
/// says: `open` with [`MR_VIEW`] and [`MR_JOBS`]; `merged` with no open merge request but the
/// merged [`MR_MERGED`]; `none` with neither. No real glab runs, so no one's GitLab sign-in is
/// reached.
fn stand_in_glab(dir: &Path, mode: &str) -> PathBuf {
    let glab = dir.join("glab");
    std::fs::write(dir.join("mr.json"), MR_VIEW).expect("written");
    std::fs::write(dir.join("jobs.json"), MR_JOBS).expect("written");
    std::fs::write(dir.join("merged.json"), MR_MERGED).expect("written");
    std::fs::write(dir.join("discussions.json"), MR_DISCUSSIONS).expect("written");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{dir}/asked\"\n\
         case \"$*\" in\n\
         'mr view --output json') if [ {mode} = open ]; then cat \"{dir}/mr.json\"; else \
           echo 'no open merge request available for \"feature\"' >&2; exit 1; fi ;;\n\
         'mr list --merged'*) if [ {mode} = merged ]; then echo '[{{\"iid\":5}}]'; else echo '[]'; fi ;;\n\
         'mr view 5 --output json') cat \"{dir}/merged.json\" ;;\n\
         'api projects/:id/pipelines/77/jobs?per_page=100') cat \"{dir}/jobs.json\" ;;\n\
         'api projects/:id/merge_requests/7/discussions?per_page=100') cat \"{dir}/discussions.json\" ;;\n\
         'mr create'*) echo 'Creating merge request for feature into main in o/demo'; \
           echo '!12 Keep it (feature)'; echo ' https://gitlab.example.com/o/demo/-/merge_requests/12' ;;\n\
         'mr merge'*) echo '✓ Merged!' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = dir.display()
    );
    std::fs::write(&glab, script).expect("written");
    std::fs::set_permissions(&glab, std::fs::Permissions::from_mode(0o755)).expect("executable");
    glab
}

/// A repository on branch `feature` whose `origin` is on a GitLab host.
fn gitlab_repo(dir: &Path) -> PathBuf {
    let work = repo(dir);
    let git = crate::changes::git().expect("git");
    let url = "https://gitlab.example.com/o/demo.git";
    let ran = std::process::Command::new(git)
        .arg("-C")
        .arg(&work)
        .args(["remote", "add", "origin", url])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git runs");
    assert!(ran.status.success());
    work
}

fn with_glab(glab: PathBuf) -> Programs {
    Programs { git: crate::changes::git().map(Path::to_path_buf), gh: None, glab: Some(glab) }
}

/// A repository whose `origin` is on a GitLab host has merge requests, read with glab in a pull
/// request's words: its pipeline's jobs are its checks (a failure allowed to fail is neutral, a
/// manual job skipped), and it stands on its failed job. gh is never asked.
#[tokio::test]
async fn a_merge_request_is_read_in_a_pull_request_s_words() {
    let dir = tempfile::tempdir().expect("temp");
    let work = gitlab_repo(dir.path());
    let programs = Programs {
        gh: Some(stand_in(dir.path(), "open")),
        ..with_glab(stand_in_glab(dir.path(), "open"))
    };
    let GitOutcome::Done(GitDone::PullStatus(Some(pull))) =
        apply(&programs, &work.to_string_lossy(), GitOp::PullStatus, &[]).await
    else {
        panic!("no merge request read")
    };
    assert_eq!(pull.forge, Forge::GitLab);
    assert_eq!(
        (pull.number, pull.state.as_str(), pull.head.as_str(), pull.base.as_str()),
        (12, "OPEN", "feature", "main")
    );
    assert_eq!(pull.url, "https://gitlab.example.com/o/demo/-/merge_requests/12");
    assert_eq!((pull.mergeable.as_str(), pull.merge_state.as_str()), ("MERGEABLE", "BLOCKED"));
    let checks: Vec<(&str, Option<&str>, &str)> = pull
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.workflow.as_deref(), c.state.as_str()))
        .collect();
    assert_eq!(
        checks,
        [
            ("test", Some("test"), "SUCCESS"),
            ("lint", Some("test"), "FAILURE"),
            ("audit", Some("test"), "NEUTRAL"),
            ("build", Some("build"), "IN_PROGRESS"),
            ("deploy", Some("deploy"), "SKIPPED"),
        ]
    );
    assert_eq!(pull.standing(), PullStanding::Failing);
    let seen = crate::thread::pulls::seen(&pull);
    assert_eq!(seen.line(), "!12: lint failed");
    assert_eq!(
        asked(dir.path()),
        ["mr view --output json", "api projects/:id/pipelines/77/jobs?per_page=100"],
        "glab alone"
    );
}

/// A branch with no open merge request reads its merged one, as gh's view finds a merged pull
/// request; with neither, none. A worker without glab says so.
#[tokio::test]
async fn a_branch_s_merged_request_is_read_and_none_is_none() {
    let dir = tempfile::tempdir().expect("temp");
    let work = gitlab_repo(dir.path()).to_string_lossy().into_owned();
    let merged = with_glab(stand_in_glab(dir.path(), "merged"));
    let GitOutcome::Done(GitDone::PullStatus(Some(pull))) =
        apply(&merged, &work, GitOp::PullStatus, &[]).await
    else {
        panic!("no merged request read")
    };
    assert_eq!((pull.number, pull.state.as_str(), pull.checks.len()), (5, "MERGED", 0));
    assert_eq!(pull.standing(), PullStanding::Merged);
    let calls = asked(dir.path());
    assert_eq!(
        calls.get(1).map(String::as_str),
        Some("mr list --merged --source-branch feature --per-page 1 --output json")
    );

    let none = with_glab(stand_in_glab(dir.path(), "none"));
    assert_eq!(
        apply(&none, &work, GitOp::PullStatus, &[]).await,
        GitOutcome::Done(GitDone::PullStatus(None))
    );
    let without = Programs { glab: None, gh: Some(stand_in(dir.path(), "open")), ..none };
    let missing = apply(&without, &work, GitOp::PullStatus, &[]).await;
    assert!(
        matches!(&missing, GitOutcome::Unavailable { program, .. } if program == "glab"),
        "{missing:?}"
    );
}

/// A repository at `work` with a commit on `feature`, whose pushes go to a bare forge in `dir`
/// while its `origin` reads as it was set: the forge's path.
fn pushing_to_a_forge(dir: &Path, work: &Path) -> PathBuf {
    let git = crate::changes::git().expect("git");
    let in_dir = |at: &Path, args: &[&str]| {
        let ran = std::process::Command::new(git)
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    };
    in_dir(work, &["commit", "-q", "--allow-empty", "-m", "the agent's work"]);
    in_dir(dir, &["init", "-q", "--bare", "forge.git"]);
    let forge = dir.join("forge.git");
    in_dir(work, &["config", "remote.origin.pushurl", &forge.to_string_lossy()]);
    forge
}

/// The commit `feature` is at in the repository at `at`, if it has the branch.
fn feature_at(at: &Path) -> Option<String> {
    let ran = std::process::Command::new(crate::changes::git()?)
        .arg("-C")
        .arg(at)
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/feature"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .ok()?;
    ran.status.success().then(|| String::from_utf8_lossy(&ran.stdout).trim().to_owned())
}

/// A merge request is opened and merged with glab as a pull request is with gh: the branch, on
/// no remote yet as a fresh agent worktree's is, goes up first with its upstream set; then its
/// title, description, target and draft as asked and never a prompt; its merge now (never left
/// to merge itself later), by the method named, at the head the person looked at, removing the
/// branch when asked.
#[tokio::test]
async fn a_merge_request_is_opened_and_merged_with_glab() {
    let dir = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(dir.path()).expect("real");
    let work = gitlab_repo(&root);
    let forge = pushing_to_a_forge(&root, &work);
    assert_eq!(feature_at(&forge), None, "not on the forge yet");
    let work = work.to_string_lossy().into_owned();
    let programs = with_glab(stand_in_glab(&root, "open"));
    let open = GitOp::PullRequest {
        title: "Keep it".to_owned(),
        body: "Why.".to_owned(),
        base: Some("main".to_owned()),
        draft: true,
    };
    assert_eq!(
        apply(&programs, &work, open, &[]).await,
        GitOutcome::Done(GitDone::PullRequest {
            url: "https://gitlab.example.com/o/demo/-/merge_requests/12".to_owned()
        })
    );
    assert_eq!(feature_at(&forge), feature_at(Path::new(&work)), "the branch went up first");
    let merge = GitOp::Merge {
        method: "squash".to_owned(),
        head: Some("0123abcd".to_owned()),
        delete_branch: true,
    };
    let GitOutcome::Done(GitDone::Merged { pull, .. }) = apply(&programs, &work, merge, &[]).await
    else {
        panic!("not merged")
    };
    assert_eq!(pull.map(|p| p.number), Some(12));
    let calls = asked(&root);
    assert_eq!(
        calls.get(..2),
        Some(
            [
                "mr create --yes --title Keep it --description Why. --target-branch main --draft"
                    .to_owned(),
                "mr merge --yes --auto-merge=false --squash --sha 0123abcd --remove-source-branch"
                    .to_owned(),
            ]
            .as_slice()
        )
    );
}

/// What the stand-in gh answers GitHub's GraphQL with: a reviewer asking for changes, a thread
/// resolved and one still open.
const REVIEW: &str = r#"{"data":{"repository":{"pullRequest":{
"reviews":{"nodes":[{"author":{"login":"ada"},"body":"Split the parser out first.",
 "state":"CHANGES_REQUESTED","url":"https://github.com/o/demo/pull/7#pullrequestreview-2"}]},
"reviewThreads":{"totalCount":2,"nodes":[
 {"isResolved":true,"isOutdated":false,"path":"src/a.rs","line":3,"comments":{"totalCount":1,
  "nodes":[{"author":{"login":"sam"},"body":"Typo.","url":null}]}},
 {"isResolved":false,"isOutdated":false,"path":"src/lib.rs","line":42,"comments":{"totalCount":1,
  "nodes":[{"author":{"login":"sam"},"body":"This unwrap panics on an empty file.",
  "url":"https://github.com/o/demo/pull/7#discussion_r2"}]}}]}}}}}"#;

/// The discussions of merge request 7, as `GET projects/:id/merge_requests/7/discussions` lists
/// them: GitLab's own note, a comment that stands alone, a resolved thread and an open one on a
/// line, with a reply.
const MR_DISCUSSIONS: &str = r#"[
{"id":"a","individual_note":true,"notes":[{"body":"added 2 commits","author":{"username":"ada"},
 "system":true,"resolvable":false,"resolved":false}]},
{"id":"b","individual_note":true,"notes":[{"body":"Nice work.","author":{"username":"lin"},
 "system":false,"resolvable":false,"resolved":false}]},
{"id":"c","individual_note":false,"notes":[{"body":"Typo.","author":{"username":"sam"},
 "system":false,"resolvable":true,"resolved":true,
 "position":{"new_path":"src/a.rs","new_line":3,"old_path":"src/a.rs","old_line":3}}]},
{"id":"d","individual_note":false,"notes":[
 {"body":"This unwrap panics on an empty file.","author":{"username":"sam"},"system":false,
  "resolvable":true,"resolved":false,
  "position":{"new_path":"src/lib.rs","new_line":42,"old_path":"src/lib.rs","old_line":40}},
 {"body":"Agreed; return the error.","author":{"username":"ada"},"system":false,
  "resolvable":true,"resolved":false}]}]"#;

/// The review still open on a pull request is read with gh, under the repository gh resolves
/// from the checkout: the reviewer's ask for changes, then the thread not resolved, on its file
/// and line. The resolved thread asks nothing.
#[tokio::test]
async fn a_pull_request_s_open_review_is_read_with_gh() {
    let dir = tempfile::tempdir().expect("temp");
    let programs = Programs {
        git: crate::changes::git().map(Path::to_path_buf),
        gh: Some(stand_in(dir.path(), "open")),
        glab: None,
    };
    let work = repo(dir.path());
    let op = GitOp::PullComments { number: 7 };
    let GitOutcome::Done(GitDone::PullComments(read)) =
        apply(&programs, &work.to_string_lossy(), op, &[]).await
    else {
        panic!("no review read")
    };
    let said: Vec<(Option<&str>, Option<u32>, &str)> = read
        .threads
        .iter()
        .map(|t| (t.path.as_deref(), t.line, t.notes[0].body.as_str()))
        .collect();
    assert_eq!(
        said,
        [
            (None, None, "Split the parser out first."),
            (Some("src/lib.rs"), Some(42), "This unwrap panics on an empty file."),
        ]
    );
    let [graphql] = asked(dir.path()).try_into().expect("one call");
    for field in ["api graphql", "-F owner={owner}", "-F name={repo}", "-F number=7"] {
        assert!(graphql.contains(field), "{field} in {graphql}");
    }
}

/// A merge request's review still open is each discussion that can be resolved and is not, read
/// with glab: GitLab's own notes, a comment standing alone and a resolved thread ask nothing.
#[tokio::test]
async fn a_merge_request_s_open_review_is_read_with_glab() {
    let dir = tempfile::tempdir().expect("temp");
    let work = gitlab_repo(dir.path());
    let programs = with_glab(stand_in_glab(dir.path(), "open"));
    let op = GitOp::PullComments { number: 7 };
    let GitOutcome::Done(GitDone::PullComments(read)) =
        apply(&programs, &work.to_string_lossy(), op, &[]).await
    else {
        panic!("no review read")
    };
    let [open] = read.threads.as_slice() else { panic!("{read:?}") };
    assert_eq!((open.path.as_deref(), open.line), (Some("src/lib.rs"), Some(42)));
    let notes: Vec<(&str, &str)> =
        open.notes.iter().map(|n| (n.author.as_str(), n.body.as_str())).collect();
    assert_eq!(
        notes,
        [("sam", "This unwrap panics on an empty file."), ("ada", "Agreed; return the error.")]
    );
    assert_eq!(asked(dir.path()), ["api projects/:id/merge_requests/7/discussions?per_page=100"]);
}

/// The merge queue lands work on a protected target through the forge: the commit goes up as
/// the task's branch, forced, and a pull request of that branch into the target is opened by
/// the person's own gh, its number read from its page. Asked again, the one already open is
/// found and none is opened twice.
#[tokio::test]
async fn work_on_a_protected_target_goes_up_as_a_pull_request() {
    let dir = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(dir.path()).expect("real");
    let git = crate::changes::git().expect("git");
    let in_dir = |at: &Path, args: &[&str]| {
        let ran = std::process::Command::new(git)
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    };
    let work = repo(&root);
    in_dir(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let head = in_dir(&work, &["rev-parse", "HEAD"]);
    in_dir(&root, &["init", "-q", "--bare", "forge.git"]);
    in_dir(&work, &["remote", "add", "origin", &root.join("forge.git").to_string_lossy()]);
    let gh = root.join("gh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{dir}/asked\"\n\
         case \"$1 $2\" in\n\
         'pr list') if [ -f \"{dir}/opened\" ]; then \
           echo '[{{\"number\":12,\"url\":\"https://github.com/o/demo/pull/12\"}}]'; \
           else echo '[]'; fi ;;\n\
         'pr create') touch \"{dir}/opened\"; echo 'https://github.com/o/demo/pull/12' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = root.display()
    );
    std::fs::write(&gh, script).expect("written");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let programs = Programs { git: Some(git.to_path_buf()), gh: Some(gh), glab: None };
    let landing = Landing { head: &head, branch: "slopty/demo/3", target: "main" };

    let opened = land(&programs, &work, landing, ("Split the parser", "Task #3")).await;
    assert_eq!(opened, Ok((12, "https://github.com/o/demo/pull/12".to_owned())));
    let up = in_dir(&root.join("forge.git"), &["rev-parse", "refs/heads/slopty/demo/3"]);
    assert_eq!(up, head, "the work went up as the task's branch");
    let found = land(&programs, &work, landing, ("Split the parser", "Task #3")).await;
    assert_eq!(found, Ok((12, "https://github.com/o/demo/pull/12".to_owned())));
    let creates = asked(&root).iter().filter(|a| a.starts_with("pr create")).count();
    assert_eq!(creates, 1, "found the second time, not opened again: {:?}", asked(&root));
    assert!(asked(&root).iter().any(|a| a.contains("--head slopty/demo/3 --base main")));

    let flag = Landing { branch: "--upload-pack=x", ..landing };
    assert!(land(&programs, &work, flag, ("t", "b")).await.is_err(), "no branch's name");
}

/// On a GitLab host the same landing goes through the person's own glab: the commit goes up as
/// the task's branch, forced, and a merge request of it into the target is opened, its number
/// read from the page glab names. Asked again, the open one is found and none is opened twice.
/// gh is never asked.
#[tokio::test]
async fn work_on_a_protected_gitlab_target_goes_up_as_a_merge_request() {
    let dir = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(dir.path()).expect("real");
    let git = crate::changes::git().expect("git");
    let in_dir = |at: &Path, args: &[&str]| {
        let ran = std::process::Command::new(git)
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    };
    // `origin` reads as the GitLab host, and pushes go to a bare forge on disk.
    let work = gitlab_repo(&root);
    in_dir(&work, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let head = in_dir(&work, &["rev-parse", "HEAD"]);
    in_dir(&root, &["init", "-q", "--bare", "forge.git"]);
    let forge = root.join("forge.git");
    in_dir(&work, &["config", "remote.origin.pushurl", &forge.to_string_lossy()]);
    let page = "https://gitlab.example.com/o/demo/-/merge_requests/12";
    let glab = root.join("glab");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{dir}/asked\"\n\
         case \"$1 $2\" in\n\
         'mr list') if [ -f \"{dir}/opened\" ]; then \
           echo '[{{\"iid\":12,\"web_url\":\"{page}\"}}]'; else echo '[]'; fi ;;\n\
         'mr create') touch \"{dir}/opened\"; \
           echo 'Creating merge request for slopty/demo/3 into main in o/demo'; \
           echo '!12 Split the parser (slopty/demo/3)'; echo ' {page}' ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = root.display()
    );
    std::fs::write(&glab, script).expect("written");
    std::fs::set_permissions(&glab, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let programs = with_glab(glab);
    let landing = Landing { head: &head, branch: "slopty/demo/3", target: "main" };

    let opened = land(&programs, &work, landing, ("Split the parser", "Task #3")).await;
    assert_eq!(opened, Ok((12, page.to_owned())));
    let up = in_dir(&forge, &["rev-parse", "refs/heads/slopty/demo/3"]);
    assert_eq!(up, head, "the work went up as the task's branch");
    let found = land(&programs, &work, landing, ("Split the parser", "Task #3")).await;
    assert_eq!(found, Ok((12, page.to_owned())));
    let calls = asked(&root);
    let creates: Vec<&String> = calls.iter().filter(|a| a.starts_with("mr create")).collect();
    assert_eq!(creates.len(), 1, "found the second time, not opened again: {calls:?}");
    assert!(creates[0].contains("--target-branch main"), "{creates:?}");
    assert!(creates[0].contains("--source-branch slopty/demo/3"), "{creates:?}");
    assert!(
        calls.iter().any(|a| a.contains("--source-branch slopty/demo/3 --target-branch main")),
        "the open one is looked for by its branches: {calls:?}"
    );
}

/// "Open pull request" on a branch no remote has, as a fresh agent worktree's is: the branch
/// goes up first, its upstream set, and then gh opens the pull request, which it could not do
/// for a branch the forge lacks. Without gh nothing goes up, and the refusal names gh.
#[tokio::test]
async fn a_pull_request_is_opened_once_its_branch_went_up() {
    let dir = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(dir.path()).expect("real");
    let work = repo(&root);
    let git = crate::changes::git().expect("git");
    let added = std::process::Command::new(git)
        .arg("-C")
        .arg(&work)
        .args(["remote", "add", "origin", "https://github.com/o/demo.git"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git runs");
    assert!(added.status.success());
    let forge = pushing_to_a_forge(&root, &work);
    let work_text = work.to_string_lossy().into_owned();
    let open = || GitOp::PullRequest {
        title: "Keep it".to_owned(),
        body: "Why.".to_owned(),
        base: None,
        draft: false,
    };

    let no_gh = Programs { git: Some(git.to_path_buf()), gh: None, glab: None };
    let refused = apply(&no_gh, &work_text, open(), &[]).await;
    assert!(
        matches!(&refused, GitOutcome::Unavailable { program, .. } if program == "gh"),
        "{refused:?}"
    );
    assert_eq!(feature_at(&forge), None, "nothing went up without gh to open it");

    let programs =
        Programs { git: Some(git.to_path_buf()), gh: Some(stand_in(&root, "open")), glab: None };
    assert_eq!(
        apply(&programs, &work_text, open(), &[]).await,
        GitOutcome::Done(GitDone::PullRequest {
            url: "https://github.com/o/demo/pull/7".to_owned()
        })
    );
    assert_eq!(feature_at(&forge), feature_at(&work), "the branch went up first");
    assert_eq!(asked(&root), ["pr create --title Keep it --body Why."]);
}

/// A stand-in for gh `version` in `dir` merging with `--delete-branch` from an agent's
/// worktree whose clone has `main` checked out, as gh's own source does it there
/// (`pkg/cmd/pr/merge/merge.go`, `deleteLocalBranch`). The pull request merges on the forge
/// first. Then gh 2.98 switches the checkout to `main` to delete the local branch, which git
/// refuses, and fails; gh 2.99 and later warn that the branch is checked out in the current
/// worktree, skip the local delete and delete the forge's branch, here with git where gh asks
/// the API. Each call's arguments are recorded; no real gh runs.
fn worktree_gh(dir: &Path, version: &str) -> PathBuf {
    let gh = dir.join("gh");
    let git = crate::changes::git().expect("git");
    let after = if version == "2.98" {
        "\"$git\" checkout -q main || exit 1"
    } else {
        "echo \"! Branch feature is checked out in the current worktree ($(pwd -P)); skipping \
         local delete\" >&2\n\"$git\" push -q origin --delete feature || exit 1"
    };
    let script = format!(
        "#!/bin/sh\nd=\"{dir}\"\ngit=\"{git}\"\nprintf '%s\\n' \"$*\" >> \"$d/asked\"\n\
         case \"$1 $2\" in\n\
         'pr merge') echo MERGED > \"$d/state\"\n\
           echo '✓ Squashed and merged pull request #7 (Keep it)'\n\
           {after} ;;\n\
         'pr view') sed \"s/\\\"OPEN\\\"/\\\"$(cat \"$d/state\")\\\"/\" \"$d/view.json\" ;;\n\
         *) echo \"unexpected: $*\" >&2; exit 2 ;;\n\
         esac\n",
        dir = dir.display(),
        git = git.display()
    );
    std::fs::write(dir.join("view.json"), VIEW).expect("written");
    std::fs::write(dir.join("state"), "OPEN").expect("written");
    std::fs::write(dir.join("asked"), "").expect("written");
    std::fs::write(&gh, script).expect("written");
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("executable");
    gh
}

/// A merge asked to delete its branch, from an agent's worktree whose clone has `main` checked
/// out, reads as merged and leaves the worktree on its branch, whichever gh runs it: gh 2.98
/// fails in its local clean-up once the merge went, and the forge's word that the pull request
/// merged decides; gh 2.99 and later leave the local branch and delete the forge's. A merge gh
/// refuses still fails (`a_merge_goes_as_the_person_said_and_a_refusal_in_gh_s_words`).
#[tokio::test]
async fn a_merge_from_a_worktree_reads_as_merged_and_leaves_the_worktree_on_its_branch() {
    let Some(git) = crate::changes::git() else { return };
    let in_dir = |at: &Path, args: &[&str]| {
        let ran = std::process::Command::new(git)
            .arg("-C")
            .arg(at)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(ran.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    };
    for version in ["2.98", "2.99"] {
        let dir = tempfile::tempdir().expect("temp");
        let root = std::fs::canonicalize(dir.path()).expect("real");
        let clone = root.join("clone");
        std::fs::create_dir_all(&clone).expect("made");
        in_dir(&clone, &["init", "-q", "-b", "main"]);
        in_dir(&clone, &["commit", "-q", "--allow-empty", "-m", "first"]);
        in_dir(&root, &["init", "-q", "--bare", "forge.git"]);
        let forge = root.join("forge.git");
        in_dir(&clone, &["remote", "add", "origin", &forge.to_string_lossy()]);
        let tree = clone.join(".claude/worktrees/fix");
        in_dir(&clone, &["worktree", "add", "-q", "-b", "feature", &tree.to_string_lossy()]);
        in_dir(&tree, &["commit", "-q", "--allow-empty", "-m", "the agent's work"]);
        in_dir(&tree, &["push", "-q", "--set-upstream", "origin", "main", "feature"]);

        let programs = Programs {
            git: Some(git.to_path_buf()),
            gh: Some(worktree_gh(&root, version)),
            glab: None,
        };
        let merge = GitOp::Merge {
            method: "squash".to_owned(),
            head: Some("0123abcd".to_owned()),
            delete_branch: true,
        };
        let done = apply(&programs, &tree.to_string_lossy(), merge, &[]).await;
        let GitOutcome::Done(GitDone::Merged { said, pull }) = &done else {
            panic!("gh {version}: {done:?}")
        };
        assert!(said.contains("Squashed and merged"), "gh {version}: {said}");
        assert_eq!(pull.as_ref().map(|p| p.state.as_str()), Some("MERGED"), "gh {version}");
        assert_eq!(
            asked(&root).first().map(String::as_str),
            Some("pr merge --squash --match-head-commit 0123abcd --delete-branch"),
            "gh {version}"
        );
        let on = |at: &Path| in_dir(at, &["symbolic-ref", "--short", "HEAD"]);
        assert_eq!(on(&tree), "feature", "gh {version}: the worktree on its branch");
        assert_eq!(on(&clone), "main", "gh {version}: the clone as it was");
        assert_eq!(
            feature_at(&forge).is_none(),
            version != "2.98",
            "gh {version}: the forge's branch deleted only where gh got that far"
        );
    }
}
