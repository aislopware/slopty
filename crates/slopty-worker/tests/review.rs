//! A thread's repository read for review, on a real git repository: its working tree against a
//! commit, a branch's commits, the sides of a file, and what a scope between turns says now that
//! no turn is snapshotted. What is judged is the trees and the files, never the time.

#[cfg(test)]
mod review {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{Against, Expanded, Review, ReviewScope};
    use slopty_proto::thread::{AgentId, Drive, ThreadId, ThreadMeta};
    use slopty_worker::repo::snapshot::Repo;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::review::{NOT_IN_GIT, Reviews};

    fn git() -> PathBuf {
        slopty_worker::changes::git().expect("git on this machine").to_owned()
    }

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new(git()).arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    const A: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n";

    /// A repository with a commit of `a.txt` and `gone.txt`, at `dir/repo`.
    fn repository(dir: &Path) -> PathBuf {
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.name", "Test"]);
        run(&repo, &["config", "user.email", "test@localhost"]);
        std::fs::write(repo.join("a.txt"), A).unwrap();
        std::fs::write(repo.join("gone.txt"), "soon gone\n").unwrap();
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "first"]);
        repo
    }

    /// A thread named by its folder: no agent's, as a test holds it.
    fn thread_in(host: &Host, cwd: &Path) -> ThreadId {
        let id = ThreadId::new();
        let meta = ThreadMeta {
            id,
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: String::new(),
            native: id.to_string(),
            cwd: cwd.to_string_lossy().into_owned(),
            title: String::new(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::OBSERVED),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        };
        host.create(meta).unwrap();
        id
    }

    /// The reviews of a host under `dir` with one thread in `repo`.
    fn reviews_in(dir: &Path, repo: &Path) -> (Reviews, ThreadId) {
        let host = Host::open(&dir.join("threads"), Limits::default()).unwrap();
        let thread = thread_in(&host, repo);
        (Reviews::new(host, Some(git())), thread)
    }

    /// A thread's working tree is reviewed against `HEAD` in the repository its folder is in,
    /// a side of a file comes whole by its blob, a scope between its turns says there is no
    /// snapshot to compare, and a thread outside git has no review.
    #[tokio::test]
    async fn a_threads_repository_is_reviewed_as_it_stands() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        std::fs::write(repo.join("a.txt"), "one\nchanged\n").unwrap();
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let reviews = Reviews::new(host.clone(), Some(git()));
        let thread = thread_in(&host, &repo);
        let review = reviews.review(thread, ReviewScope::WorkingTree(Against::Head)).await;
        assert_eq!(review.absent, None, "{review:?}");
        let file = review.files.iter().find(|f| f.path == "a.txt").expect("a.txt changed");
        let blob = file.from.clone().expect("its committed side");
        match reviews.blob(thread, &blob).await {
            Expanded::Text(text) => assert!(text.starts_with("one"), "{text:?}"),
            other => panic!("{other:?}"),
        }
        let outside = tempfile::tempdir().unwrap();
        let elsewhere = thread_in(&host, outside.path());
        let none = reviews.review(elsewhere, ReviewScope::WorkingTree(Against::Head)).await;
        assert_eq!(none.absent.as_deref(), Some(NOT_IN_GIT));
        assert_eq!(reviews.blob(elsewhere, &blob).await, Expanded::Gone);
    }

    /// A folder's working tree, reviewed with no thread through the person's git op: against
    /// `HEAD` it is what is not committed, a new file too, and the person's index stays as it
    /// was; against the base it is the branch's commits as well, and so against the target
    /// named, by any name; a branch with no base, or a target that is not there, says so; and a
    /// thread's review over the same span reads the same.
    #[tokio::test]
    async fn a_folders_working_tree_is_reviewed_with_no_thread() {
        use slopty_proto::git::{GitDone, GitOp, GitOutcome};
        use slopty_proto::thread::wire::Against;
        use slopty_worker::repo::commit::{Programs, apply};
        use slopty_worker::repo::snapshot::NO_BASE;

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        run(&repo, &["switch", "-q", "-c", "feature"]);
        std::fs::write(repo.join("feature.txt"), "on the branch\n").unwrap();
        run(&repo, &["add", "feature.txt"]);
        run(&repo, &["commit", "-q", "-m", "feature"]);
        std::fs::write(repo.join("a.txt"), A.replace("two", "TWO")).unwrap();
        std::fs::write(repo.join("new.txt"), "not added\n").unwrap();
        let index = std::fs::read(repo.join(".git/index")).unwrap();
        let programs = Programs { git: Some(git()), gh: None, glab: None, path: None };
        let folder = repo.to_string_lossy().into_owned();
        let changes = async |against| match apply(
            &programs,
            &folder,
            GitOp::Changes { against, ignore_whitespace: false },
            &[],
        )
        .await
        {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("{other:?}"),
        };
        let paths =
            |review: &Review| review.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();

        let head = changes(Against::Head).await;
        assert_eq!(head.scope, ReviewScope::WorkingTree(Against::Head));
        assert_eq!(paths(&head), ["a.txt", "new.txt"], "what is not committed, new files too");
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index, "the index untouched");
        let base = changes(Against::Base).await;
        assert_eq!(paths(&base), ["a.txt", "feature.txt", "new.txt"], "the branch's work too");
        let main = changes(Against::Branch("main".to_owned())).await;
        assert_eq!(paths(&main), paths(&base), "the target named, as the base");
        let own = changes(Against::Branch("feature".to_owned())).await;
        assert_eq!(paths(&own), paths(&head), "against its own branch, what is not committed");
        let nowhere = changes(Against::Branch("nope".to_owned())).await;
        assert_eq!(nowhere.absent.as_deref(), Some(NO_BASE), "a branch that is not there");

        let (reviews, thread) = reviews_in(dir.path(), &repo);
        let threads = reviews.review(thread, ReviewScope::WorkingTree(Against::Head)).await;
        assert_eq!(paths(&threads), paths(&head), "a thread's review of the span reads the same");

        let alone = dir.path().join("alone");
        std::fs::create_dir_all(&alone).unwrap();
        run(&alone, &["init", "-q", "-b", "trunk"]);
        run(
            &alone,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "c0",
            ],
        );
        std::fs::write(alone.join("draft.txt"), "draft\n").unwrap();
        let folder = alone.to_string_lossy().into_owned();
        let none = match apply(
            &programs,
            &folder,
            GitOp::Changes { against: Against::Base, ignore_whitespace: false },
            &[],
        )
        .await
        {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("{other:?}"),
        };
        assert_eq!(none.absent.as_deref(), Some(NO_BASE), "no main, no master, no origin");
        let against = Against::Branch("trunk".to_owned());
        let trunk = match apply(
            &programs,
            &folder,
            GitOp::Changes { against, ignore_whitespace: false },
            &[],
        )
        .await
        {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("{other:?}"),
        };
        assert_eq!(paths(&trunk), ["draft.txt"], "a target by any name");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let folder = outside.to_string_lossy().into_owned();
        let refused = apply(
            &programs,
            &folder,
            GitOp::Changes { against: Against::Head, ignore_whitespace: false },
            &[],
        )
        .await;
        assert!(matches!(refused, GitOutcome::Refused { .. }), "{refused:?}");
    }

    /// An edit of the same size, with the file's stat as the index recorded it, is seen in the
    /// working tree's review as git itself sees it: the index file is as old as the entry, so git
    /// reads the file again ("racy git"), and the review's scratch copy of the index keeps that
    /// age rather than looking newer and trusting the stat. The ctime is set aside
    /// (`core.trustctime`), since a test cannot set it back.
    #[tokio::test]
    async fn an_edit_with_the_stat_unchanged_is_still_reviewed() {
        use std::time::SystemTime;

        use slopty_proto::thread::wire::Against;
        use slopty_worker::repo::snapshot::working_tree;

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        run(&repo, &["config", "core.trustctime", "false"]);
        #[expect(clippy::disallowed_methods, reason = "a file's times are set on the system clock")]
        let then = SystemTime::now().checked_sub(Duration::from_secs(3_600)).unwrap();
        let age = |path: &Path| {
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(then)).unwrap();
        };
        age(&repo.join("a.txt"));
        // The entry takes the old stat, long before the index is written, so it is not smudged.
        run(&repo, &["update-index", "--refresh"]);
        std::fs::write(repo.join("a.txt"), A.replace("two", "TWO")).unwrap();
        age(&repo.join("a.txt"));
        age(&repo.join(".git/index"));

        let review = working_tree(&git(), &repo, Against::Head).await.unwrap();
        let paths = review.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>();
        assert_eq!(paths, ["a.txt"], "the edit, though its stat is the entry's");
        let status = run(&repo, &["status", "--porcelain", "--untracked-files=no"]);
        assert_eq!(status, "M a.txt", "as git itself sees it");
    }

    /// A branch checked out nowhere is reviewed as committed (`GitOp::Range`): from where it
    /// left its base to its tip, so neither the base's own later commits nor the working tree
    /// count; `origin`'s copy of either branch is read when it holds the local one; a branch
    /// that is in neither, or two that share no history, say so; nothing in the repository
    /// moves; and a thread's review of the same range reads the same.
    #[tokio::test]
    async fn a_branch_checked_out_nowhere_is_reviewed_as_committed() {
        use slopty_proto::git::{GitDone, GitOp, GitOutcome};
        use slopty_worker::repo::commit::{Programs, apply};
        use slopty_worker::repo::snapshot::{NO_BRANCH, NO_FORK};

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        run(&repo, &["switch", "-q", "-c", "goal"]);
        std::fs::write(repo.join("goal.txt"), "the goal\n").unwrap();
        run(&repo, &["add", "goal.txt"]);
        run(&repo, &["rm", "-q", "gone.txt"]);
        run(&repo, &["commit", "-q", "-m", "goal"]);
        run(&repo, &["switch", "-q", "main"]);
        std::fs::write(repo.join("later.txt"), "main moved on\n").unwrap();
        run(&repo, &["add", "later.txt"]);
        run(&repo, &["commit", "-q", "-m", "later"]);
        std::fs::write(repo.join("a.txt"), A.replace("two", "TWO")).unwrap();
        let (head, index) =
            (run(&repo, &["rev-parse", "HEAD"]), std::fs::read(repo.join(".git/index")).unwrap());
        let programs = Programs { git: Some(git()), gh: None, glab: None, path: None };
        let folder = repo.to_string_lossy().into_owned();
        let range = async |base: &str, head: &str| {
            let op = GitOp::Range { base: base.to_owned(), head: head.to_owned() };
            match apply(&programs, &folder, op, &[]).await {
                GitOutcome::Done(GitDone::Changes(review)) => *review,
                other => panic!("{other:?}"),
            }
        };
        let paths =
            |review: &Review| review.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();

        let goal = range("main", "goal").await;
        let scope = ReviewScope::Range { base: "main".to_owned(), head: "goal".to_owned() };
        assert_eq!(goal.scope, scope);
        assert_eq!(paths(&goal), ["goal.txt", "gone.txt"], "the goal's commits, nothing else");
        assert_eq!(goal.absent, None);
        assert_eq!(run(&repo, &["rev-parse", "HEAD"]), head, "HEAD stays");
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index, "the index untouched");
        assert_eq!(run(&repo, &["status", "--porcelain"]), "M a.txt", "the working tree untouched");

        // Pushed further from another machine: origin's copy holds the local one, so it reads.
        run(&repo, &["switch", "-q", "goal"]);
        std::fs::write(repo.join("pushed.txt"), "from elsewhere\n").unwrap();
        run(&repo, &["add", "pushed.txt"]);
        run(&repo, &["commit", "-q", "-m", "pushed"]);
        run(&repo, &["update-ref", "refs/remotes/origin/goal", "HEAD"]);
        run(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
        run(&repo, &["switch", "-q", "main"]);
        assert_eq!(paths(&range("main", "goal").await), ["goal.txt", "gone.txt", "pushed.txt"]);

        let nowhere = range("main", "nope").await;
        assert_eq!(nowhere.absent.as_deref(), Some(NO_BRANCH), "a branch that is not there");
        assert_eq!(nowhere.files, Vec::new());
        run(&repo, &["switch", "-q", "--orphan", "alone"]);
        run(&repo, &["commit", "-q", "--allow-empty", "-m", "alone"]);
        run(&repo, &["switch", "-q", "-f", "main"]);
        let apart = range("main", "alone").await;
        assert_eq!(apart.absent.as_deref(), Some(NO_FORK), "two histories that never met");

        let (reviews, thread) = reviews_in(dir.path(), &repo);
        let threads = reviews.review(thread, scope).await;
        assert_eq!(
            paths(&threads),
            ["goal.txt", "gone.txt", "pushed.txt"],
            "a thread's reads the same"
        );
    }

    /// A commit picked on a terminal's screen is reviewed as committed (`GitOp::Show`): from its
    /// first parent to it, by its whole or abbreviated hash, so neither later commits nor the
    /// working tree count; a root commit against the empty tree; a hash that names no commit, a
    /// ref and an option say so and run nothing; nothing in the repository moves; and a thread's
    /// review of the same commit reads the same.
    #[tokio::test]
    async fn a_commit_is_reviewed_alone_against_its_parent() {
        use slopty_proto::git::{GitDone, GitOp, GitOutcome};
        use slopty_worker::repo::commit::{Programs, apply};
        use slopty_worker::repo::snapshot::NO_COMMIT;

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let root = run(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("goal.txt"), "the goal\n").unwrap();
        run(&repo, &["add", "goal.txt"]);
        run(&repo, &["rm", "-q", "gone.txt"]);
        run(&repo, &["commit", "-q", "-m", "goal"]);
        let goal = run(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("later.txt"), "moved on\n").unwrap();
        run(&repo, &["add", "later.txt"]);
        run(&repo, &["commit", "-q", "-m", "later"]);
        std::fs::write(repo.join("a.txt"), A.replace("two", "TWO")).unwrap();
        let (head, index) =
            (run(&repo, &["rev-parse", "HEAD"]), std::fs::read(repo.join(".git/index")).unwrap());
        let programs = Programs { git: Some(git()), gh: None, glab: None, path: None };
        let folder = repo.to_string_lossy().into_owned();
        let show = async |commit: &str| {
            let op = GitOp::Show { commit: commit.to_owned() };
            match apply(&programs, &folder, op, &[]).await {
                GitOutcome::Done(GitDone::Changes(review)) => *review,
                other => panic!("{other:?}"),
            }
        };
        let paths =
            |review: &Review| review.files.iter().map(|f| f.path.clone()).collect::<Vec<_>>();

        let short = goal.get(..7).unwrap();
        let one = show(short).await;
        assert_eq!(one.scope, ReviewScope::Commit { commit: short.to_owned() });
        assert_eq!(paths(&one), ["goal.txt", "gone.txt"], "that commit's files, nothing else");
        assert_eq!(one.absent, None);
        assert_eq!(paths(&show(&goal).await), ["goal.txt", "gone.txt"], "by its whole hash too");
        assert_eq!(run(&repo, &["rev-parse", "HEAD"]), head, "HEAD stays");
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index, "the index untouched");
        assert_eq!(run(&repo, &["status", "--porcelain"]), "M a.txt", "the working tree untouched");

        let first = show(&root).await;
        assert_eq!(paths(&first), ["a.txt", "gone.txt"], "a root commit against nothing");

        for not in [
            "deadbeef",
            "main",
            "HEAD~1",
            "--all",
            "abc",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0",
        ] {
            let none = show(not).await;
            assert_eq!(none.absent.as_deref(), Some(NO_COMMIT), "{not:?} names no commit");
            assert_eq!(none.files, Vec::new());
        }

        let (reviews, thread) = reviews_in(dir.path(), &repo);
        let threads =
            reviews.review(thread, ReviewScope::Commit { commit: short.to_owned() }).await;
        assert_eq!(paths(&threads), ["goal.txt", "gone.txt"], "a thread's reads the same");
    }

    /// A review past its line budget spends it in the order the files are read, the
    /// weightiest first and the quiet ones last, so a lock file is the one left without hunks
    /// rather than the source after it; a file past the budget says how many lines it has, and
    /// comes whole when asked by its blobs. A name that is no object id is refused.
    #[tokio::test]
    async fn a_big_review_spends_its_lines_in_reading_order_and_a_file_comes_whole() {
        use slopty_proto::git::{GitDone, GitOp, GitOutcome};
        use slopty_proto::thread::wire::Against;
        use slopty_worker::repo::commit::{Programs, apply};
        use slopty_worker::repo::snapshot::REVIEW_LINES;

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let lines = |n: u32, word: &str| {
            (0..n).fold(String::new(), |mut all, i| {
                all.push_str(word);
                all.push(' ');
                all.push_str(&i.to_string());
                all.push('\n');
                all
            })
        };
        // In path order the lock went first and took the room the source after it needed.
        let budget = REVIEW_LINES;
        let (big, mid, lock) = (budget + 5_000, budget * 3 / 5, budget * 9 / 20);
        std::fs::write(repo.join("big.rs"), lines(big, "big")).unwrap();
        std::fs::write(repo.join("mid.rs"), lines(mid, "mid")).unwrap();
        std::fs::write(repo.join("small.rs"), lines(5, "small")).unwrap();
        std::fs::write(repo.join("Cargo.lock"), lines(lock, "lock")).unwrap();
        let programs = Programs { git: Some(git()), gh: None, glab: None, path: None };
        let folder = repo.to_string_lossy().into_owned();
        let op = GitOp::Changes { against: Against::Head, ignore_whitespace: false };
        let review = match apply(&programs, &folder, op, &[]).await {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("{other:?}"),
        };
        let file = |path: &str| review.files.iter().find(|f| f.path == path).expect(path);
        let shown =
            |path: &str| file(path).patch.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
        assert_eq!((shown("big.rs"), file("big.rs").patch.clipped_lines), (0, big), "past it all");
        assert_eq!(file("big.rs").patch.added, big, "its counts stay");
        assert_eq!(shown("mid.rs"), mid as usize, "the heaviest that fits is shown");
        assert_eq!(shown("small.rs"), 5, "and a light one after it");
        assert_eq!(file("Cargo.lock").patch.clipped_lines, lock, "the lock, read last, waits");

        let whole =
            GitOp::FileDiff { from: None, to: file("big.rs").to.clone(), ignore_whitespace: false };
        let (asked, patch) = match apply(&programs, &folder, whole, &[]).await {
            GitOutcome::Done(GitDone::FileDiff { to, patch, .. }) => (to, *patch),
            other => panic!("{other:?}"),
        };
        assert_eq!(asked, file("big.rs").to, "answered for the blobs asked");
        let lines_whole = patch.hunks.iter().map(|h| h.lines.len()).sum::<usize>();
        assert_eq!((lines_whole, patch.clipped_lines), (big as usize, 0), "the file whole");

        let bad = GitOp::FileDiff {
            from: Some("--output=/tmp/x".to_owned()),
            to: None,
            ignore_whitespace: false,
        };
        let refused = apply(&programs, &folder, bad, &[]).await;
        assert!(matches!(refused, GitOutcome::Failed { .. }), "{refused:?}");
    }

    /// A blob a review named comes whole by its id, for a picture's two sides; one past the cap
    /// and a name that is no object id are refused.
    #[tokio::test]
    async fn a_blob_comes_whole_and_none_past_its_cap() {
        use slopty_proto::git::{BLOB_MAX, GitDone, GitOp, GitOutcome};
        use slopty_worker::repo::commit::{Programs, apply};

        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let png: Vec<u8> = b"\x89PNG\r\n\x1a\n".iter().copied().chain(0..=255).collect();
        std::fs::write(repo.join("logo.png"), &png).unwrap();
        let id = run(&repo, &["hash-object", "-w", "logo.png"]);
        let huge = vec![0_u8; usize::try_from(BLOB_MAX).unwrap() + 1];
        std::fs::write(repo.join("huge.bin"), &huge).unwrap();
        let huge_id = run(&repo, &["hash-object", "-w", "huge.bin"]);
        let programs = Programs { git: Some(git()), gh: None, glab: None, path: None };
        let folder = repo.to_string_lossy().into_owned();

        match apply(&programs, &folder, GitOp::Blob { blob: id.clone() }, &[]).await {
            GitOutcome::Done(GitDone::Blob { blob, bytes }) => {
                assert_eq!((blob, bytes), (id, png), "the bytes, byte for byte");
            }
            other => panic!("{other:?}"),
        }
        for blob in [huge_id, "--output=/tmp/x".to_owned()] {
            let refused = apply(&programs, &folder, GitOp::Blob { blob }, &[]).await;
            assert!(matches!(refused, GitOutcome::Refused { .. }), "{refused:?}");
        }
    }

    /// What a snapshot costs on this repository, cloned: the first (every file hashed), one
    /// with nothing changed, and one after a file changed. A measurement for
    /// `docs/MEASUREMENTS.md`, not a pass or fail.
    #[tokio::test]
    #[ignore = "a measurement: cargo test -p slopty-worker --release --test review -- --ignored --nocapture"]
    async fn snapshot_cost_on_this_repository() {
        let dir = tempfile::tempdir().unwrap();
        let here = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = dir.path().join("clone");
        let clone = Command::new(git())
            .args(["clone", "-q", "--local", "--no-hardlinks"])
            .arg(&here)
            .arg(&root)
            .status()
            .unwrap();
        assert!(clone.success());
        let files = run(&root, &["ls-files"]).lines().count();
        let repo = Repo { git: git(), root: root.clone(), index: dir.path().join("index") };
        let mut timed = Vec::new();
        for label in ["first", "unchanged", "unchanged", "one file changed", "unchanged"] {
            if label == "one file changed" {
                std::fs::write(root.join("README.md"), "changed\n").unwrap();
            }
            let started = tokio::time::Instant::now();
            repo.take().await.unwrap();
            timed.push((label, started.elapsed()));
        }
        eprintln!("snapshot of {files} tracked files:");
        for (label, took) in timed {
            eprintln!("  {label:>18}: {took:?}");
        }
    }

    /// What a review of many changed files costs to read: the whole review, its blobs read by
    /// one `git cat-file --batch`, beside the same blobs read one `git cat-file` each, as the
    /// review read them before. Printed, not judged.
    #[tokio::test]
    #[ignore = "a measurement: cargo test -p slopty-worker --release --test review -- --ignored --nocapture"]
    async fn review_read_cost_of_many_files() {
        const FILES: usize = 300;
        let dir = tempfile::tempdir().unwrap();
        let root = repository(dir.path());
        let repo = Repo { git: git(), root: root.clone(), index: dir.path().join("index") };
        for i in 0..FILES {
            std::fs::write(root.join(format!("f{i}.txt")), A).unwrap();
        }
        let from = repo.take().await.unwrap();
        for i in 0..FILES {
            std::fs::write(root.join(format!("f{i}.txt")), A.replace("six\n", "SIX\n")).unwrap();
        }
        let to = repo.take().await.unwrap();
        let started = tokio::time::Instant::now();
        let files = repo.changes(&from, &to).await.unwrap();
        let review = started.elapsed();
        assert_eq!(files.len(), FILES);
        let started = tokio::time::Instant::now();
        for file in &files {
            for id in file.from.iter().chain(file.to.iter()) {
                repo.object(id).await.unwrap();
            }
        }
        let one_each = started.elapsed();
        eprintln!("a review of {FILES} changed files: {review:?} whole, batched");
        eprintln!("  their {} blobs one cat-file each: {one_each:?}", FILES * 2);
    }
}
