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
        Action, Changed, IntentId, Liveness, Phase, Status, ThreadId, ThreadState, Turn, TurnId,
        TurnState, Usage,
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
    /// kept in the thread's refs; the person's index is left alone. Its review has every file
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
        rig.end(1);
        let state = rig.until(|s| s.turns.first().is_some_and(|t| t.after.is_some())).await;
        let after = state.turns.first().unwrap().after.clone().unwrap();
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
    /// what is left to review, a hunk or a file at a time.
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
        rig.end(1);
        rig.until(|s| s.turns.first().is_some_and(|t| t.after.is_some())).await;
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

        let gone = Intent::Revert(pick(&review, "new.txt", vec![]));
        assert_eq!(rig.pick(IntentId::new(), &gone).await, Outcome::Done);
        assert!(!repo.join("new.txt").exists(), "an added file put back is no file");
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
