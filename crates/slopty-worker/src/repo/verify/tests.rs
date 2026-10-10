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

/// A clone with `main` at two commits, and a task's branch off its first with one more.
fn clone_with_a_task(root: &Path) -> (PathBuf, String, String) {
    let repo = root.join("demo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    commit(&repo, ".gitignore", "target/\n");
    let first = commit(&repo, "a.txt", "one\n");
    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/1"]);
    let task = commit(&repo, "b.txt", "the task's\n");
    git_in(&repo, &["switch", "-q", "main"]);
    (repo, first, task)
}

/// The project's checkout is made once, at the commit asked for and detached, with the fork
/// point from the target; a later run reuses it, at its own commit, keeping what git ignores
/// (a build's output) and nothing else left behind. Neither the clone's checkout nor its
/// branches move.
#[tokio::test]
async fn a_project_verifies_in_one_checkout_of_its_own_kept_warm() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, task) = clone_with_a_task(&root);
    let place = place(&root.join("verify"), "demo").expect("a name");

    let made = checkout(git, &repo, &place, "slopty/demo/1", "main").await.expect("checked out");
    assert_eq!((made.head.as_str(), made.base.as_str()), (task.as_str(), first.as_str()));
    assert_eq!(std::fs::read_to_string(place.join("b.txt")).expect("read"), "the task's\n");
    assert_eq!(git_in(&place, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD", "detached");

    std::fs::create_dir_all(place.join("target")).expect("mkdir");
    std::fs::write(place.join("target/warm"), "built").expect("write");
    std::fs::write(place.join("stray.txt"), "left by a run").expect("write");
    let again = checkout(git, &repo, &place, &first, "main").await.expect("reused");
    assert_eq!(again.head, first);
    assert!(!place.join("b.txt").exists(), "at its own commit");
    assert!(!place.join("stray.txt").exists(), "nothing an earlier run left");
    assert!(place.join("target/warm").exists(), "what git ignores stays warm");
    assert_eq!(git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert_eq!(git_in(&repo, &["rev-parse", "slopty/demo/1"]), task);

    let unrelated = checkout(git, &repo, &place, "nope", "main").await;
    assert!(matches!(unrelated, Err(Failed::Other(why)) if why.contains("not a commit")));
    assert!(super::place(&root, "../up").is_err() && super::place(&root, ".hidden").is_err());
}

/// A head already on top of the target is taken as it is; one behind it is rebased onto it in
/// the project's checkout, leaving the rebased commit checked out there; one that conflicts
/// names the paths and leaves no rebase stopped behind.
#[tokio::test]
async fn the_queue_rebases_in_the_project_s_checkout_and_names_conflicts() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, _first, task) = clone_with_a_task(&root);
    let place = place(&root.join("verify"), "demo").expect("a name");

    let up_to_date = rebase(git, &repo, &place, (&task, "main", None), &[], Some(&task)).await;
    let up_to_date = up_to_date.expect("as it is");
    assert_eq!((&up_to_date.head, &up_to_date.from), (&task, &task), "main is where it left it");
    assert!(up_to_date.verified, "the very commit verified");

    let moved = commit(&repo, "c.txt", "main moved on\n");
    let rebased =
        rebase(git, &repo, &place, ("slopty/demo/1", "main", None), &[], Some(&task)).await;
    let rebased = rebased.expect("rebased");
    assert_eq!(rebased.from, task, "the branch given, as its commit");
    assert!(!rebased.verified, "main's change is in its tree");
    assert_eq!(rebased.onto, moved);
    assert_ne!(rebased.head, task);
    assert_eq!(git_in(&repo, &["rev-parse", &format!("{}^", rebased.head)]), moved);
    assert_eq!(git_in(&place, &["rev-parse", "HEAD"]), rebased.head, "left for the verifier");
    assert_eq!(git_in(&repo, &["rev-parse", "slopty/demo/1"]), task, "no branch moves");

    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/2", &moved]);
    let clashing = commit(&repo, "a.txt", "the other task's\n");
    git_in(&repo, &["switch", "-q", "main"]);
    commit(&repo, "a.txt", "main's own\n");
    let conflict = rebase(git, &repo, &place, (&clashing, "main", None), &[], None).await;
    assert_eq!(conflict, Err(Failed::Conflict(vec!["a.txt".to_owned()])));
    let stopped = git_in(&place, &["status", "--porcelain=v2", "--branch"]);
    assert!(!stopped.contains("rebase"), "{stopped}");
    let again = rebase(git, &repo, &place, (&rebased.head, "main", None), &[], None).await;
    assert!(again.is_ok(), "the checkout serves the next rebase: {again:?}");
}

/// A task that started on another's work, which reached the target as other commits (its
/// agent resolved a conflict), is rebased from the commit it started on: only its own commits
/// are picked, where the plain rebase would pick that work again and conflict. A head that does
/// not hold that commit takes the plain rebase.
#[tokio::test]
async fn work_started_on_another_s_is_rebased_from_where_it_started() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, _) = clone_with_a_task(&root);
    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/2", &first]);
    let started_on = commit(&repo, "a.txt", "the first task's\n");
    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/3"]);
    let own = commit(&repo, "c.txt", "the second task's\n");
    git_in(&repo, &["switch", "-q", "main"]);
    commit(&repo, "a.txt", "main's own\n");
    let merged = commit(&repo, "a.txt", "main's own and the first task's\n");
    let place = place(&root.join("verify"), "demo").expect("a name");

    let plain = rebase(git, &repo, &place, (&own, "main", None), &[], None).await;
    assert_eq!(plain, Err(Failed::Conflict(vec!["a.txt".to_owned()])), "that work picked again");

    let after = Some(started_on.as_str());
    let made = rebase(git, &repo, &place, (&own, "main", after), &[], None).await.expect("rebased");
    assert_eq!(git_in(&repo, &["rev-parse", &format!("{}^", made.head)]), merged, "its one commit");
    let tree = git_in(&repo, &["show", &format!("{}:a.txt", made.head)]);
    assert_eq!(tree, "main's own and the first task's", "the work as it merged");

    let task = git_in(&repo, &["rev-parse", "slopty/demo/1"]);
    let unrelated = rebase(git, &repo, &place, (&task, "main", after), &[], None).await;
    let unrelated = unrelated.expect("the plain rebase");
    assert_eq!(git_in(&repo, &["rev-parse", &format!("{}^", unrelated.head)]), merged);
}

/// Every commit the queue rebases carries the task and the thread it came from as trailers,
/// once, even one already on top of the target, whose tree stays the one verified so it need
/// not run again; a commit that has them already does not take them twice. A trailer that is
/// not a plain token and a one-line value is refused before anything runs.
#[tokio::test]
async fn rebased_commits_carry_where_they_came_from() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, _) = clone_with_a_task(&root);
    git_in(&repo, &["switch", "-q", "slopty/demo/1"]);
    let task = commit(&repo, "d.txt", "a second commit\n");
    git_in(&repo, &["switch", "-q", "main"]);
    git_in(&repo, &["reset", "-q", "--hard", &first]);
    let place = place(&root.join("verify"), "demo").expect("a name");
    let trailers = [
        ("Slopty-Task".to_owned(), "demo#1".to_owned()),
        ("Slopty-Thread".to_owned(), "0190d6f2-7c1a-7e00-8000-000000000001".to_owned()),
    ];

    let made = rebase(git, &repo, &place, (&task, "main", None), &trailers, Some(&task)).await;
    let made = made.expect("rebased with its trailers");
    assert_ne!(made.head, task, "its messages changed");
    assert!(made.verified, "its tree is the one verified");
    let log = git_in(
        &repo,
        &["log", "--format=%(trailers:only,unfold)%x00", &format!("{first}..{}", made.head)],
    );
    let each: Vec<&str> = log.split('\0').map(str::trim).filter(|t| !t.is_empty()).collect();
    let want = "Slopty-Task: demo#1\nSlopty-Thread: 0190d6f2-7c1a-7e00-8000-000000000001";
    assert_eq!(each, [want, want], "both commits, each once");

    let again = rebase(git, &repo, &place, (&made.head, "main", None), &trailers, None).await;
    let again = again.expect("rebased again");
    let log = git_in(&repo, &["log", "-1", "--format=%(trailers:only,unfold)", &again.head]);
    assert_eq!(log.trim(), want, "not added twice");

    let quoted = [("Slopty-Task".to_owned(), "it's".to_owned())];
    let refused = rebase(git, &repo, &place, (&task, "main", None), &quoted, None).await;
    assert!(matches!(refused, Err(Failed::Other(why)) if why.contains("not a trailer")));
}

/// What a task's work did to the tests is read from where it left the target to its head: the
/// tests it deleted, changed or renamed, and added, by their paths and the project's own test
/// paths, never what the target did since.
#[tokio::test]
async fn a_task_s_test_diff_names_what_it_did_to_the_tests() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let repo = root.join("demo");
    std::fs::create_dir_all(repo.join("tests")).expect("mkdir");
    std::fs::create_dir_all(repo.join("qa")).expect("mkdir");
    std::fs::create_dir_all(repo.join("src")).expect("mkdir");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    for (file, text) in [
        ("tests/flaky.rs", "flaky\n"),
        ("tests/kept.rs", "kept\n"),
        ("src/lib_test.go", "go test\n"),
        ("qa/smoke.sh", "smoke\n"),
        ("src/lib.rs", "code\n"),
    ] {
        commit(&repo, file, text);
    }
    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/1"]);
    git_in(&repo, &["rm", "-q", "tests/flaky.rs", "qa/smoke.sh"]);
    std::fs::write(repo.join("src/lib_test.go"), "go test, weaker\n").expect("write");
    std::fs::write(repo.join("src/lib.rs"), "code changed\n").expect("write");
    std::fs::write(repo.join("tests/new.rs"), "new\n").expect("write");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-q", "-m", "the task"]);
    git_in(&repo, &["switch", "-q", "main"]);
    commit(&repo, "tests/main_only.rs", "the target's own\n");

    let extra = ["qa".to_owned()];
    let tests = test_diff(git, &repo, "slopty/demo/1", "main", &extra).await.expect("read");
    assert_eq!(tests.deleted, ["qa/smoke.sh", "tests/flaky.rs"]);
    assert_eq!(tests.changed, ["src/lib_test.go"]);
    assert_eq!((tests.deleted_count, tests.changed_count, tests.added_count), (2, 1, 1));
    assert_eq!(tests.head, git_in(&repo, &["rev-parse", "slopty/demo/1"]));
    let plain = test_diff(git, &repo, "slopty/demo/1", "main", &[]).await.expect("read");
    assert_eq!(plain.deleted, ["tests/flaky.rs"], "qa holds tests only when the project says");
    let none = test_diff(git, &repo, "main", "main", &[]).await.expect("read");
    assert!(none.is_empty(), "{none:?}");
}

/// The target moves only from the commit asked, to a commit after it: by compare-and-swap
/// where nothing has it checked out, through `merge --ff-only` where the person has it, which
/// keeps their changes and refuses one the move would overwrite. A push asked for goes to
/// `origin` as a fast-forward.
#[tokio::test]
async fn the_target_moves_only_forward_and_never_under_the_person_s_changes() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, task) = clone_with_a_task(&root);
    git_in(&root, &["clone", "-q", "--bare", "demo", "forge.git"]);
    git_in(&repo, &["remote", "add", "origin", &root.join("forge.git").to_string_lossy()]);

    // The person has main checked out, with a change of their own beside the move.
    std::fs::write(repo.join("mine.txt"), "unsaved\n").expect("write");
    let moved = fast_forward(git, &repo, "main", &first, &task, true).await.expect("moved");
    assert_eq!(moved, Moved { head: task.clone(), pushed: true, push_failed: None });
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), task);
    assert_eq!(std::fs::read_to_string(repo.join("b.txt")).expect("read"), "the task's\n");
    assert_eq!(std::fs::read_to_string(repo.join("mine.txt")).expect("read"), "unsaved\n");
    assert_eq!(git_in(&root.join("forge.git"), &["rev-parse", "main"]), task, "pushed");

    let stale = fast_forward(git, &repo, "main", &first, &task, false).await;
    assert_eq!(stale, Err(Failed::Moved(task.clone())), "not from where it is now");
    let backwards = fast_forward(git, &repo, "main", &task, &first, false).await;
    assert!(matches!(backwards, Err(Failed::Other(why)) if why.contains("does not descend")));

    git_in(&repo, &["switch", "-q", "-c", "slopty/demo/3"]);
    let next = commit(&repo, "b.txt", "more\n");
    git_in(&repo, &["switch", "-q", "main"]);
    std::fs::write(repo.join("b.txt"), "the person's edit\n").expect("write");
    let blocked = fast_forward(git, &repo, "main", &task, &next, false).await;
    assert!(matches!(blocked, Err(Failed::Other(_))), "{blocked:?}");
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), task, "nothing moved");
    assert_eq!(std::fs::read_to_string(repo.join("b.txt")).expect("read"), "the person's edit\n");

    // Checked out nowhere: the ref moves by compare-and-swap.
    git_in(&repo, &["checkout", "-q", "--detach"]);
    let moved = fast_forward(git, &repo, "main", &task, &next, false).await.expect("moved");
    assert_eq!((moved.head.as_str(), moved.pushed), (next.as_str(), false));
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), next);
}

/// A forge that protects the target refuses the push in its own words: the move is refused as
/// [`Failed::Protected`] before anything moves here, so the clone's target stays where the
/// forge's is and the work can go up as a pull request instead.
#[tokio::test]
async fn a_protected_target_refuses_the_push_and_nothing_moves() {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, task) = clone_with_a_task(&root);
    git_in(&root, &["clone", "-q", "--bare", "demo", "forge.git"]);
    git_in(&repo, &["remote", "add", "origin", &root.join("forge.git").to_string_lossy()]);
    let hook = root.join("forge.git/hooks/pre-receive");
    let refuse = "#!/bin/sh\nwhile read old new ref; do\n  if [ \"$ref\" = refs/heads/main ]; then\n    \
                  echo 'GH006: Protected branch update failed for refs/heads/main.' >&2; exit 1\n  \
                  fi\ndone\n";
    std::fs::write(&hook, refuse).expect("hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("executable");

    let refused = fast_forward(git, &repo, "main", &first, &task, true).await;
    assert!(
        matches!(&refused, Err(Failed::Protected(why)) if why.contains("GH006")),
        "{refused:?}"
    );
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), first, "nothing moved here");
    assert_eq!(git_in(&root.join("forge.git"), &["rev-parse", "main"]), first, "nor there");
    let local = fast_forward(git, &repo, "main", &first, &task, false).await.expect("moved");
    assert_eq!(local.head, task, "a merge kept here is no push, and nothing refuses it");
}

/// Once a pull request merged on the forge, the clone's target is brought up to `origin`'s:
/// fetched, then fast-forwarded where it is checked out, so the person's checkout has the merge
/// too. Asked again it stays; a branch ahead of `origin`'s stays; one with commits `origin`'s
/// lacks is not moved and says so.
#[tokio::test]
async fn the_clone_s_target_catches_up_with_origin_s() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, first, _task) = clone_with_a_task(&root);
    git_in(&root, &["clone", "-q", "--bare", "demo", "forge.git"]);
    let forge = root.join("forge.git");
    git_in(&repo, &["remote", "add", "origin", &forge.to_string_lossy()]);
    // Someone else's checkout, where the forge's merges land.
    git_in(&root, &["clone", "-q", &forge.to_string_lossy(), "elsewhere"]);
    let elsewhere = root.join("elsewhere");
    let merged = commit(&elsewhere, "c.txt", "merged on the forge\n");
    git_in(&elsewhere, &["push", "-q", "origin", "main"]);

    let caught = catch_up(git, &repo, "main").await.expect("caught up");
    assert_eq!(caught.head, merged);
    assert!(!caught.pushed);
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), merged);
    assert!(repo.join("c.txt").is_file(), "the checkout that has main moved with it");
    let again = catch_up(git, &repo, "main").await.expect("still there");
    assert_eq!(again.head, merged, "already there, it stays");

    let ahead = commit(&repo, "d.txt", "here only\n");
    let kept = catch_up(git, &repo, "main").await.expect("ahead");
    assert_eq!(kept.head, ahead, "ahead of origin's, it stays");

    let theirs = commit(&elsewhere, "e.txt", "the forge moved on\n");
    git_in(&elsewhere, &["push", "-q", "origin", "main"]);
    let diverged = catch_up(git, &repo, "main").await;
    assert_eq!(diverged, Err(Failed::Diverged(theirs)), "commits origin's lacks");
    assert_eq!(git_in(&repo, &["rev-parse", "main"]), ahead, "nothing moved");
    assert_ne!(first, merged);
}

/// A project let go takes its checkout with it, by force: what a verifier built and left
/// behind goes, and the clone forgets the worktree. One already gone is answered as gone, and
/// one whose clone is gone too goes as a plain directory.
#[tokio::test]
async fn a_let_go_project_s_checkout_goes_by_force() {
    let Some(git) = crate::changes::git() else { return };
    let tmp = tempfile::tempdir().expect("temp");
    let root = std::fs::canonicalize(tmp.path()).expect("real");
    let (repo, _first, _task) = clone_with_a_task(&root);
    let place = place(&root.join("verify"), "demo").expect("a name");
    checkout(git, &repo, &place, "slopty/demo/1", "main").await.expect("checked out");
    std::fs::create_dir_all(place.join("target")).expect("mkdir");
    std::fs::write(place.join("target/warm"), "built").expect("write");
    std::fs::write(place.join("stray.txt"), "left by a run").expect("write");
    std::fs::write(place.join("b.txt"), "changed by a run").expect("write");

    assert_eq!(drop_checkout(git, &place).await, Ok(true));
    assert!(!place.exists(), "gone, with what was built");
    let listed = git_in(&repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("verify"), "the clone forgets it: {listed}");
    assert_eq!(drop_checkout(git, &place).await, Ok(false), "gone already");

    checkout(git, &repo, &place, "slopty/demo/1", "main").await.expect("checked out");
    std::fs::remove_dir_all(&repo).expect("the clone goes");
    assert_eq!(drop_checkout(git, &place).await, Ok(true));
    assert!(!place.exists(), "a plain directory now");
}
