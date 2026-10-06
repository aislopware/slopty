//! Turn snapshots and the review over them, on a real git repository: a thread's turn edges
//! are applied to the host as an adapter applies them, the working tree changes between, and
//! what is judged is the trees, the refs and the files, never the time.

#[cfg(test)]
mod review {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use slopty_agent::observed::{Observed, Out};
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{Intent, Outcome, Pick, Review, ReviewScope};
    use slopty_proto::thread::{
        Action, Changed, IntentId, Liveness, Phase, Status, ThreadId, ThreadState, TreeRef, Turn,
        TurnId, TurnState, Usage,
    };
    use slopty_worker::repo::snapshot::Repo;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::review::{NOT_IN_GIT, Snapshots};

    /// Long enough for any git on a loaded machine; the tests judge what comes.
    const BOUND: Duration = Duration::from_secs(30);

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

    struct Rig {
        host: Host,
        snapshots: Snapshots,
        thread: ThreadId,
        _observer: tokio::task::JoinHandle<()>,
    }

    impl Rig {
        fn new(dir: &Path, cwd: &Path) -> Self {
            let host = Host::open(&dir.join("threads"), Limits::default()).unwrap();
            let cwd = cwd.to_string_lossy();
            let mut observed = Observed::new("s1", "", None, &cwd, WallMs::ZERO);
            let Some(Out::Begin(meta)) = observed.drain().into_iter().next() else { panic!() };
            let thread = meta.id;
            host.create(*meta).unwrap();
            let snapshots = Snapshots::new(host.clone(), &dir.join("snapshots"), Some(git()));
            let observer = snapshots.spawn();
            Self { host, snapshots, thread, _observer: observer }
        }

        fn apply(&self, action: Action) {
            self.host.apply(self.thread, vec![action]);
        }

        fn status(&self, phase: Phase) {
            let status =
                Status { phase, wait: None, liveness: Liveness::Live, since_ms: WallMs::ZERO };
            self.apply(Action::Status(status));
        }

        fn begin(&self, turn: u32) {
            self.apply(Action::TurnStarted(Turn {
                id: TurnId(turn),
                input: None,
                state: TurnState::Active,
                started_ms: WallMs::ZERO,
                ended_ms: None,
                usage: Usage::default(),
                models: Vec::new(),
                changed: Changed::default(),
                before: None,
                after: None,
            }));
        }

        fn end(&self, turn: u32) {
            self.apply(Action::TurnEnded {
                turn: TurnId(turn),
                state: TurnState::Complete,
                usage: Usage::default(),
                ended_ms: WallMs::ZERO,
            });
        }

        /// Wait until the thread satisfies `done`, woken by each batch it applies.
        async fn until(&self, done: impl Fn(&ThreadState) -> bool) -> ThreadState {
            let mut feed = self.host.watch(self.thread).unwrap();
            let waited = tokio::time::timeout(BOUND, async {
                loop {
                    let state = self.host.state(self.thread).unwrap().0;
                    if done(&state) {
                        return state;
                    }
                    let _batch = feed.recv().await;
                }
            });
            waited.await.expect("the thread came to the state awaited")
        }

        async fn pick(&self, id: IntentId, intent: &Intent) -> Outcome {
            self.snapshots.pick(self.thread, id, intent).await.unwrap()
        }
    }

    fn file<'a>(
        review: &'a Review,
        path: &str,
    ) -> Option<&'a slopty_proto::thread::wire::FileDiff> {
        review.files.iter().find(|f| f.path == path)
    }

    fn pick(review: &Review, path: &str, hunks: Vec<u32>) -> Pick {
        let diff = file(review, path).unwrap();
        Pick { path: path.to_owned(), from: diff.from.clone(), stamp: diff.to.clone(), hunks }
    }

    /// A turn's start is snapshotted when the agent starts working, its end when it ends, both
    /// kept in the thread's refs, and the turn told again mid-way keeps its start; the person's
    /// index is left alone. Its review has every file
    /// it touched (a change, a new file, a removed one), the change in two hunks.
    #[tokio::test]
    async fn a_turn_is_snapshotted_at_its_edges_and_reviewed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let rig = Rig::new(dir.path(), &repo);
        rig.status(Phase::Working);
        rig.begin(1);
        let state = rig.until(|s| s.turns.first().is_some_and(|t| t.before.is_some())).await;
        let before = state.turns.first().unwrap().before.clone().unwrap();
        assert_eq!(before.0, run(&repo, &["rev-parse", "HEAD^{tree}"]), "nothing changed yet");

        let changed = A.replace("two\n", "TWO\n").replace("eleven\n", "ELEVEN\nmore\n");
        std::fs::write(repo.join("a.txt"), &changed).unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();
        std::fs::remove_file(repo.join("gone.txt")).unwrap();
        rig.begin(1);
        rig.end(1);
        let state = rig.until(|s| s.turns.first().is_some_and(|t| t.after.is_some())).await;
        let after = state.turns.first().unwrap().after.clone().unwrap();
        assert_eq!(state.turns.first().unwrap().before, Some(before.clone()), "told again");
        let thread = rig.thread;
        let pinned = |edge: &str| {
            run(&repo, &["rev-parse", &format!("refs/slopty/threads/{thread}/1-{edge}^{{tree}}")])
        };
        assert_eq!((pinned("before"), pinned("after")), (before.0.clone(), after.0.clone()));
        assert_eq!(run(&repo, &["diff", "--cached", "--name-only"]), "", "the person's index");

        let review = rig.snapshots.review(rig.thread, ReviewScope::Turn(TurnId(1))).await;
        assert_eq!(review.absent, None);
        let mut paths: Vec<&str> = review.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["a.txt", "gone.txt", "new.txt"]);
        let a = file(&review, "a.txt").unwrap();
        assert_eq!(a.patch.hunks.len(), 2, "{:#?}", a.patch);
        assert_eq!((a.patch.added, a.patch.removed), (3, 2));
        assert_eq!(file(&review, "new.txt").unwrap().from, None);
        assert_eq!(file(&review, "gone.txt").unwrap().to, None);
    }

    /// A hunk put back leaves the other; a revert of a file that changed since is refused,
    /// and the same intent again is its first outcome with nothing done. What is kept leaves
    /// what is left to review, a hunk or a file at a time, and the thread is to review until
    /// everything is kept.
    #[tokio::test]
    async fn changes_are_kept_and_put_back_by_hunk_and_by_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let rig = Rig::new(dir.path(), &repo);
        rig.status(Phase::Working);
        rig.begin(1);
        rig.until(|s| s.turns.first().is_some_and(|t| t.before.is_some())).await;
        let changed = A.replace("two\n", "TWO\n").replace("eleven\n", "ELEVEN\nmore\n");
        std::fs::write(repo.join("a.txt"), &changed).unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();
        assert!(!rig.host.state(rig.thread).unwrap().0.to_review, "nothing ended yet");
        rig.end(1);
        rig.until(|s| s.turns.first().is_some_and(|t| t.after.is_some())).await;
        rig.until(|s| s.to_review).await;
        let review = rig.snapshots.review(rig.thread, ReviewScope::Turn(TurnId(1))).await;

        let back = IntentId::new();
        let first = Intent::Revert(pick(&review, "a.txt", vec![0]));
        assert_eq!(rig.pick(back, &first).await, Outcome::Done);
        let a = std::fs::read_to_string(repo.join("a.txt")).unwrap();
        assert_eq!(a, A.replace("eleven\n", "ELEVEN\nmore\n"), "the first hunk went back");
        assert_eq!(rig.pick(back, &first).await, Outcome::Done, "the same intent again");
        let stale = rig.pick(IntentId::new(), &first).await;
        assert!(matches!(stale, Outcome::Refused { .. }), "the file changed since: {stale:?}");
        assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), a, "nothing done");

        let left = rig.snapshots.review(rig.thread, ReviewScope::Kept).await;
        assert_eq!(file(&left, "a.txt").unwrap().patch.hunks.len(), 1);
        let keep = Intent::Keep(pick(&left, "a.txt", vec![0]));
        assert_eq!(rig.pick(IntentId::new(), &keep).await, Outcome::Done);
        let left = rig.snapshots.review(rig.thread, ReviewScope::Kept).await;
        assert!(file(&left, "a.txt").is_none(), "all of a.txt is kept: {left:#?}");
        let again = rig.pick(IntentId::new(), &keep).await;
        assert!(matches!(again, Outcome::Refused { .. }), "kept otherwise since: {again:?}");
        let whole = Intent::Keep(pick(&left, "new.txt", vec![]));
        assert_eq!(rig.pick(IntentId::new(), &whole).await, Outcome::Done);
        let left = rig.snapshots.review(rig.thread, ReviewScope::Kept).await;
        assert!(left.files.is_empty(), "nothing left to review: {left:#?}");
        let to_review = || rig.host.state(rig.thread).unwrap().0.to_review;
        assert!(!to_review(), "all of it kept, the thread is no longer to review");

        let gone = Intent::Revert(pick(&review, "new.txt", vec![]));
        assert_eq!(rig.pick(IntentId::new(), &gone).await, Outcome::Done);
        assert!(!repo.join("new.txt").exists(), "an added file put back is no file");
        assert!(to_review(), "what was kept is gone from the tree: a change to review");
    }

    /// A thread outside git takes no snapshot and its review says why.
    #[tokio::test]
    async fn a_thread_outside_git_has_no_review() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let rig = Rig::new(dir.path(), &plain);
        let review = rig.snapshots.review(rig.thread, ReviewScope::Kept).await;
        assert_eq!(review.absent.as_deref(), Some(NOT_IN_GIT));
    }

    /// The change a review showed is kept for the agent's own review as one commit on a base
    /// of its own, so `base...head` and the head commit both say exactly it. Each review
    /// overwrites the one pair; a push of the person's branch carries none of it, refs or
    /// commits; and a thread forgotten takes every ref of its own with it.
    #[tokio::test]
    async fn an_agents_review_names_the_change_as_one_pair_that_never_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repository(dir.path());
        let remote = dir.path().join("remote.git");
        run(dir.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        run(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        let rig = Rig::new(dir.path(), &repo);
        rig.status(Phase::Working);
        rig.begin(1);
        rig.until(|s| s.turns.first().is_some_and(|t| t.before.is_some())).await;
        std::fs::write(repo.join("a.txt"), A.replace("two\n", "TWO\n")).unwrap();
        rig.end(1);
        let state = rig.until(|s| s.turns.first().is_some_and(|t| t.after.is_some())).await;
        let turn = state.turns.first().unwrap();
        let (from, to) = (turn.before.clone().unwrap(), turn.after.clone().unwrap());

        let first = rig.snapshots.review_range(rig.thread, &from, &to).await.unwrap();
        let tree = |rev: &str| run(&repo, &["rev-parse", &format!("{rev}^{{tree}}")]);
        assert_eq!((tree(&first.base), tree(&first.head)), (from.0.clone(), to.0.clone()));
        assert_eq!(run(&repo, &["rev-parse", &format!("{}^", first.head)]), first.base);
        assert_eq!(run(&repo, &["diff", "--name-only", &first.dots()]), "a.txt");
        assert_eq!(first.dots(), format!("{}...{}", first.base, first.head));
        let again = rig.snapshots.review_range(rig.thread, &to, &to).await.unwrap();
        let thread = rig.thread;
        let pair = |name: &str| {
            run(&repo, &["rev-parse", &format!("refs/slopty/threads/{thread}/review-{name}")])
        };
        assert_eq!((pair("base"), pair("head")), (again.base, again.head), "overwritten");
        assert!(
            rig.snapshots.review_range(rig.thread, &from, &TreeRef("0".repeat(40))).await.is_err(),
            "no such tree"
        );

        run(&repo, &["push", "-q", "origin", "main"]);
        assert_eq!(run(&remote, &["for-each-ref", "--format=%(refname)"]), "refs/heads/main");
        let carried = Command::new(git())
            .arg("-C")
            .arg(&remote)
            .args(["cat-file", "-e", &first.head])
            .output()
            .unwrap();
        assert!(!carried.status.success(), "the review's commit stays home");

        let state = rig.host.state(rig.thread).unwrap().0;
        rig.snapshots.forget(&state).await;
        let left = run(&repo, &["for-each-ref", "--format=%(refname)", "refs/slopty/"]);
        assert_eq!(left, "", "every ref of the thread goes with it");
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
        let programs = Programs { git: Some(git()), gh: None };
        let folder = repo.to_string_lossy().into_owned();
        let changes = async |against| match apply(
            &programs,
            &folder,
            GitOp::Changes { against },
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

        let rig = Rig::new(dir.path(), &repo);
        let threads =
            rig.snapshots.review(rig.thread, ReviewScope::WorkingTree(Against::Head)).await;
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
        let none =
            match apply(&programs, &folder, GitOp::Changes { against: Against::Base }, &[]).await {
                GitOutcome::Done(GitDone::Changes(review)) => *review,
                other => panic!("{other:?}"),
            };
        assert_eq!(none.absent.as_deref(), Some(NO_BASE), "no main, no master, no origin");
        let against = Against::Branch("trunk".to_owned());
        let trunk = match apply(&programs, &folder, GitOp::Changes { against }, &[]).await {
            GitOutcome::Done(GitDone::Changes(review)) => *review,
            other => panic!("{other:?}"),
        };
        assert_eq!(paths(&trunk), ["draft.txt"], "a target by any name");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let folder = outside.to_string_lossy().into_owned();
        let refused =
            apply(&programs, &folder, GitOp::Changes { against: Against::Head }, &[]).await;
        assert!(matches!(refused, GitOutcome::Refused { .. }), "{refused:?}");
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
            let started = std::time::Instant::now();
            repo.take().await.unwrap();
            timed.push((label, started.elapsed()));
        }
        eprintln!("snapshot of {files} tracked files:");
        for (label, took) in timed {
            eprintln!("  {label:>18}: {took:?}");
        }
    }
}
