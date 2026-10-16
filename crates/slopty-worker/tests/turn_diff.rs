//! The Changes pane's two choices on a real git repository, through the worker's own git ops:
//! one turn's diff between the trees the worker took when it began and ended, and any diff with
//! whitespace not counting (`git diff -w`). What is judged is the files and lines, never time.

#[cfg(test)]
mod turn_diff {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use slopty_proto::git::{GitDone, GitOp, GitOutcome};
    use slopty_proto::thread::wire::{Against, Review, ReviewScope};
    use slopty_proto::thread::{TreeRef, TurnId};
    use slopty_worker::repo::commit::{Programs, apply};
    use slopty_worker::repo::snapshot::Repo;

    fn git() -> PathBuf {
        slopty_worker::changes::git().expect("git on this machine").to_owned()
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new(git())
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    const A: &str = "fn main() {\n    let x = 1;\n    let y = 2;\n    println!(\"{x}\");\n}\n";

    /// A repository with a commit of `a.rs` and `b.rs`, at `dir/repo`.
    fn repository(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.name", "Test"]);
        run(&repo, &["config", "user.email", "test@localhost"]);
        std::fs::write(repo.join("a.rs"), A).unwrap();
        std::fs::write(repo.join("b.rs"), "one\ntwo\n").unwrap();
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "first"]);
        repo
    }

    fn programs() -> Programs {
        Programs { git: Some(git()), ..Programs::default() }
    }

    /// The working tree's tree now, as the worker snapshots a turn's edge.
    async fn take(dir: &Path, root: &Path) -> TreeRef {
        let repo = Repo { git: git(), root: root.to_owned(), index: dir.join("index") };
        repo.take().await.unwrap()
    }

    async fn review(root: &Path, op: GitOp) -> Review {
        match apply(&programs(), &root.to_string_lossy(), op, &[]).await {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("not a review: {other:?}"),
        }
    }

    fn paths(review: &Review) -> Vec<&str> {
        review.files.iter().map(|file| file.path.as_str()).collect()
    }

    /// A turn's diff holds what changed between its two snapshots alone: not what was changed
    /// before it began nor after it ended. While it runs (no end yet) it runs to the working
    /// tree now. A tree that is no object id is refused.
    #[tokio::test]
    async fn a_turn_shows_what_changed_between_its_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = repository(dir.path());
        std::fs::write(root.join("b.rs"), "one\ntwo\nbefore the turn\n").unwrap();
        let before = take(dir.path(), &root).await;
        std::fs::write(root.join("a.rs"), A.replace("let y = 2;", "let y = 3;")).unwrap();
        std::fs::write(root.join("new.rs"), "made by the turn\n").unwrap();
        let after = take(dir.path(), &root).await;
        std::fs::write(root.join("later.rs"), "after the turn\n").unwrap();

        let turn = TurnId(2);
        let ended = GitOp::Turn {
            turn,
            before: before.clone(),
            after: Some(after.clone()),
            ignore_whitespace: false,
        };
        let ended = review(&root, ended).await;
        assert_eq!(ended.scope, ReviewScope::Turn(turn));
        assert_eq!((ended.from.as_ref(), ended.to.as_ref()), (Some(&before), Some(&after)));
        assert_eq!(paths(&ended), ["a.rs", "new.rs"], "only the turn's own files");
        let a = &ended.files[0].patch;
        assert_eq!((a.added, a.removed), (1, 1), "a.rs: one line changed");

        let running = GitOp::Turn { turn, before, after: None, ignore_whitespace: false };
        let running = review(&root, running).await;
        assert_eq!(paths(&running), ["a.rs", "later.rs", "new.rs"], "a running turn runs to now");

        let forged = TreeRef("HEAD; rm -rf".to_owned());
        let forged = GitOp::Turn { turn, before: forged, after: None, ignore_whitespace: false };
        let refused = apply(&programs(), &root.to_string_lossy(), forged, &[]).await;
        assert!(matches!(refused, GitOutcome::Failed { .. }), "{refused:?}");
    }

    /// With whitespace not counting, a reindented line is no change: a file changed only in
    /// its spaces leaves the review, and a file with a real change keeps that change alone. The
    /// whole file's diff reads the same way, and with whitespace counting every line shows.
    #[tokio::test]
    async fn whitespace_counts_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let root = repository(dir.path());
        let reindented = A
            .replace("    let x = 1;", "\tlet x = 1;")
            .replace("    let y = 2;", "      let y =  2;");
        std::fs::write(root.join("a.rs"), reindented.replace("println!", "eprintln!")).unwrap();
        std::fs::write(root.join("b.rs"), "one  \n  two\n").unwrap();

        let counting = GitOp::Changes { against: Against::Head, ignore_whitespace: false };
        let counting = review(&root, counting).await;
        assert_eq!(paths(&counting), ["a.rs", "b.rs"]);
        let a = &counting.files[0].patch;
        assert_eq!((a.added, a.removed), (3, 3), "a.rs: two reindented lines and one change");

        let ignoring = GitOp::Changes { against: Against::Head, ignore_whitespace: true };
        let ignoring = review(&root, ignoring).await;
        assert_eq!(paths(&ignoring), ["a.rs"], "b.rs changed only its spaces");
        let a = &ignoring.files[0];
        assert_eq!((a.patch.added, a.patch.removed), (1, 1), "a.rs: the one real change");
        let lines: Vec<&str> =
            a.patch.hunks.iter().flat_map(|hunk| hunk.lines.iter()).map(String::as_str).collect();
        assert!(lines.iter().any(|l| l.starts_with('+') && l.contains("eprintln!")), "{lines:?}");
        assert!(lines.contains(&"       let y =  2;"), "context reads as it is now: {lines:?}");

        let whole = |ignore_whitespace| GitOp::FileDiff {
            from: a.from.clone(),
            to: a.to.clone(),
            ignore_whitespace,
        };
        for (ignore, changed) in [(true, (1, 1)), (false, (3, 3))] {
            match apply(&programs(), &root.to_string_lossy(), whole(ignore), &[]).await {
                GitOutcome::Done(GitDone::FileDiff { patch, .. }) => {
                    assert_eq!((patch.added, patch.removed), changed, "whole file, -w {ignore}");
                }
                other => panic!("not a file diff: {other:?}"),
            }
        }
    }
}
