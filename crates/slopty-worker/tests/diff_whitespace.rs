//! The Changes pane's whitespace choice on a real git repository, through the worker's own git
//! ops: any diff with whitespace not counting (`git diff -w`), and a turn's diff refused, since
//! no turn is snapshotted. What is judged is the files and lines, never time.

#[cfg(test)]
mod diff_whitespace {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use slopty_proto::git::{GitDone, GitOp, GitOutcome};
    use slopty_proto::thread::wire::{Against, Review};
    use slopty_worker::repo::commit::{Programs, apply};

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

    async fn review(root: &Path, op: GitOp) -> Review {
        match apply(&programs(), &root.to_string_lossy(), op, &[]).await {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("not a review: {other:?}"),
        }
    }

    fn paths(review: &Review) -> Vec<&str> {
        review.files.iter().map(|file| file.path.as_str()).collect()
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
