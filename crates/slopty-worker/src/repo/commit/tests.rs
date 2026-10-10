use std::path::Path;

use super::*;

/// git in `dir` as a test's own: no global or system config but a name to commit under.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let git = crate::changes::git().expect("git");
    let out = std::process::Command::new(git)
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository with one commit and a bare remote beside it. Its own config names who commits
/// and keeps the worker's git, which reads the person's config, from signing or running hooks
/// from elsewhere.
fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let (work, bare) = (root.join("work"), root.join("remote.git"));
    std::fs::create_dir_all(&work).expect("made");
    git_in(&root, &["init", "--quiet", "--bare", "remote.git"]);
    git_in(&work, &["init", "--quiet"]);
    git_in(&work, &["config", "user.name", "Person"]);
    git_in(&work, &["config", "user.email", "person@example.com"]);
    git_in(&work, &["config", "commit.gpgsign", "false"]);
    git_in(&work, &["config", "core.hooksPath", ".git/hooks"]);
    std::fs::write(work.join("kept.txt"), "kept\n").expect("written");
    std::fs::write(work.join("gone.txt"), "gone\n").expect("written");
    git_in(&work, &["add", "."]);
    git_in(&work, &["commit", "--quiet", "-m", "first"]);
    (dir, work, bare)
}

/// The worker's git and no gh, so no test reaches the person's own GitHub sign-in.
fn git_only() -> Programs {
    Programs { git: crate::changes::git().map(Path::to_path_buf), gh: None, glab: None, path: None }
}

async fn done(repo: &Path, op: GitOp) -> GitDone {
    match apply(&git_only(), &repo.to_string_lossy(), op, &[]).await {
        GitOutcome::Done(done) => done,
        other => panic!("{other:?}"),
    }
}

async fn status_of(repo: &Path) -> GitStatus {
    match done(repo, GitOp::Status).await {
        GitDone::Status(status) => *status,
        other => panic!("{other:?}"),
    }
}

/// A status names each changed file in git's own letters, a rename with where it came from,
/// and the branch with its upstream and how far apart they are.
#[test]
fn a_status_is_read_from_git_s_own_records() {
    let out = "# branch.oid 0123abcd\0# branch.head main\0# branch.upstream origin/main\0\
               # branch.ab +2 -1\0\
               1 .M N... 100644 100644 100644 aaaa bbbb src/lib.rs\0\
               2 R. N... 100644 100644 100644 aaaa bbbb R100 docs/new name.md\0docs/old.md\0\
               u UU N... 100644 100644 100644 100644 aaaa bbbb cccc both.rs\0\
               ? notes/todo.txt\0";
    let status = parse_status("/w/demo", out);
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.head.as_deref(), Some("0123abcd"));
    assert_eq!(status.upstream.as_deref(), Some("origin/main"));
    assert_eq!((status.ahead, status.behind), (2, 1));
    let files: Vec<(&str, Option<&str>, &str)> =
        status.files.iter().map(|f| (f.path.as_str(), f.from.as_deref(), f.xy.as_str())).collect();
    assert_eq!(
        files,
        [
            ("src/lib.rs", None, ".M"),
            ("docs/new name.md", Some("docs/old.md"), "R."),
            ("both.rs", None, "UU"),
            ("notes/todo.txt", None, "??"),
        ]
    );
    let fresh = parse_status("/w/new", "# branch.oid (initial)\0# branch.head (detached)\0");
    assert_eq!((fresh.head, fresh.branch), (None, None));
}

/// The person's commit takes exactly the files chosen, new, changed or gone, and leaves what
/// else was staged staged; their message is the commit's. Then the branch pushes, setting its
/// upstream the first time, and the status says it is level with it.
#[tokio::test]
async fn the_chosen_files_are_committed_with_the_person_s_message_and_pushed() {
    let (_tmp, work, bare) = repo();
    git_in(&work, &["remote", "add", "origin", &bare.to_string_lossy()]);
    std::fs::write(work.join("kept.txt"), "changed\n").expect("written");
    std::fs::remove_file(work.join("gone.txt")).expect("removed");
    std::fs::write(work.join("new.txt"), "new\n").expect("written");
    std::fs::write(work.join("staged.txt"), "staged\n").expect("written");
    git_in(&work, &["add", "staged.txt"]);

    let before = status_of(&work).await;
    let mut files: Vec<(String, String)> =
        before.files.iter().map(|f| (f.path.clone(), f.xy.clone())).collect();
    files.sort();
    let expected =
        [("gone.txt", ".D"), ("kept.txt", ".M"), ("new.txt", "??"), ("staged.txt", "A.")];
    let expected: Vec<(String, String)> =
        expected.iter().map(|(p, xy)| ((*p).to_owned(), (*xy).to_owned())).collect();
    assert_eq!(files, expected);

    let chosen = ["kept.txt", "gone.txt", "new.txt"].map(str::to_owned).to_vec();
    let message = "Keep what matters\n\nThe person's own words.".to_owned();
    let GitDone::Committed { commit, branch, files } =
        done(&work, GitOp::Commit { paths: chosen, message: message.clone() }).await
    else {
        panic!("not a commit")
    };
    assert_eq!((branch.as_deref(), files), (Some("main"), 3));
    assert_eq!(git_in(&work, &["log", "-1", "--format=%B"]).trim(), message);
    assert_eq!(git_in(&work, &["rev-parse", "HEAD"]).trim(), commit);
    let after = status_of(&work).await;
    let left: Vec<&str> = after.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(left, ["staged.txt"], "what else was staged stays staged, uncommitted");

    let pushed = done(&work, GitOp::Push).await;
    let first = GitDone::Pushed {
        remote: "origin".to_owned(),
        branch: "main".to_owned(),
        upstream_set: true,
        pull: None,
    };
    assert_eq!(pushed, first);
    assert_eq!(git_in(&bare, &["rev-parse", "main"]).trim(), commit);
    let level = status_of(&work).await;
    assert_eq!((level.upstream.as_deref(), level.ahead, level.behind), (Some("origin/main"), 0, 0));
    let again = done(&work, GitOp::Push).await;
    assert!(matches!(again, GitDone::Pushed { upstream_set: false, .. }), "{again:?}");
}

/// What cannot be done is said: no files chosen, no message (none is made up), a path out of
/// the repository, a folder in none, no remote; and what git refuses, in git's own words, as a
/// commit hook that fails or a push the remote rejects.
#[tokio::test]
async fn a_refusal_is_said_in_git_s_own_words() {
    let (dir, work, bare) = repo();
    let git = &git_only();
    let commit = |paths: &[&str], message: &str| GitOp::Commit {
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        message: message.to_owned(),
    };
    let refused = |outcome: GitOutcome| match outcome {
        GitOutcome::Refused { why } => why,
        other => panic!("not refused: {other:?}"),
    };
    let failed = |outcome: GitOutcome| match outcome {
        GitOutcome::Failed { said } => said,
        other => panic!("not failed: {other:?}"),
    };
    let at = work.to_string_lossy().into_owned();
    std::fs::write(work.join("kept.txt"), "changed\n").expect("written");
    assert!(refused(apply(git, &at, commit(&[], "m"), &[]).await).contains("choose the files"));
    assert!(
        refused(apply(git, &at, commit(&["kept.txt"], "  "), &[]).await)
            .contains("none is made up")
    );
    assert!(
        refused(apply(git, &at, commit(&["../x"], "m"), &[]).await).contains("not a path inside")
    );
    let nowhere = dir.path().to_string_lossy().into_owned();
    assert!(
        refused(apply(git, &nowhere, GitOp::Status, &[]).await).contains("in no git repository")
    );
    assert!(refused(apply(git, &at, GitOp::Push, &[]).await).contains("no remote"));

    let hook = work.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint: kept.txt is not formatted' >&2\nexit 1\n")
        .expect("written");
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("executable");
    let said = failed(apply(git, &at, commit(&["kept.txt"], "Change it"), &[]).await);
    assert!(said.contains("lint: kept.txt is not formatted"), "{said}");
    std::fs::remove_file(&hook).expect("removed");

    git_in(&work, &["remote", "add", "origin", &bare.to_string_lossy()]);
    assert!(matches!(apply(git, &at, GitOp::Push, &[]).await, GitOutcome::Done(_)));
    let other = dir.path().join("other");
    git_in(dir.path(), &["clone", "--quiet", &bare.to_string_lossy(), "other"]);
    std::fs::write(other.join("theirs.txt"), "theirs\n").expect("written");
    git_in(&other, &["add", "."]);
    git_in(&other, &["commit", "--quiet", "-m", "theirs"]);
    git_in(&other, &["push", "--quiet"]);
    assert!(matches!(
        apply(git, &at, commit(&["kept.txt"], "Ours"), &[]).await,
        GitOutcome::Done(_)
    ));
    let said = failed(apply(git, &at, GitOp::Push, &[]).await);
    assert!(said.contains("rejected"), "{said}");
}

/// A commit runs the person's hooks on their own `PATH`: a pre-commit hook whose tool is found
/// only there (a daemon launchd started has no Homebrew, mise or npm on its own) commits, and
/// fails in git's words without it.
#[tokio::test]
async fn a_hook_s_tool_is_found_on_the_person_s_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, work, _bare) = repo();
    let tools = dir.path().join("tools");
    std::fs::create_dir_all(&tools).expect("made");
    let tool = tools.join("slopty-test-lint");
    std::fs::write(&tool, "#!/bin/sh\nexit 0\n").expect("written");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("executable");
    let hook = work.join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nslopty-test-lint\n").expect("written");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("executable");
    std::fs::write(work.join("kept.txt"), "changed\n").expect("written");
    let commit = || GitOp::Commit { paths: vec!["kept.txt".to_owned()], message: "m".to_owned() };
    let repo = work.to_string_lossy();

    let alone = apply(&git_only(), &repo, commit(), &[]).await;
    assert!(
        matches!(&alone, GitOutcome::Failed { said } if said.contains("slopty-test-lint")),
        "the worker's own PATH has no such tool: {alone:?}"
    );
    let mut path = std::ffi::OsString::from(&tools);
    path.push(":/usr/bin:/bin");
    let person = Programs { path: Some(path), ..git_only() };
    let made = apply(&person, &repo, commit(), &[]).await;
    assert!(matches!(made, GitOutcome::Done(GitDone::Committed { .. })), "{made:?}");
}
