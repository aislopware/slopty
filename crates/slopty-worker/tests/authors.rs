//! Who wrote a line, on a real git repository: two threads' turns are snapshotted as the trees
//! they began and ended with, the person edits between and after them, and one line comes from
//! a commit a project merged with its thread's trailer. What is judged is each line's thread
//! and turn.

#[cfg(test)]
mod authors {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use slopty_agent::observed::{Observed, Out};
    use slopty_core::WallMs;
    use slopty_proto::thread::wire::Authors;
    use slopty_proto::thread::{Action, Changed, ThreadId, Turn, TurnId, TurnState, Usage};
    use slopty_worker::repo::snapshot::Repo;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::authors::{Authorship, NOT_IN_GIT};
    use slopty_worker::thread::log::Limits;

    fn git() -> PathBuf {
        slopty_worker::changes::git().expect("git on this machine").to_owned()
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new(git()).arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    const LINES: [&str; 12] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];

    /// `a.txt` with line `at` (from 1) changed to `to`.
    fn edit(repo: &Path, at: usize, to: &str) {
        let file = repo.join("a.txt");
        let text = std::fs::read_to_string(&file).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        lines[at.saturating_sub(1)] = to.to_owned();
        std::fs::write(&file, format!("{}\n", lines.join("\n"))).unwrap();
    }

    fn thread(host: &Host, session: &str, cwd: &Path) -> ThreadId {
        let cwd = cwd.to_string_lossy();
        let mut observed = Observed::new(session, "", None, &cwd, WallMs::ZERO);
        let Some(Out::Begin(meta)) = observed.drain().into_iter().next() else { panic!() };
        let id = meta.id;
        host.create(*meta).unwrap();
        id
    }

    /// A turn of `thread` that changes line `at` to `to`, snapshotted on each side as the
    /// worker snapshots it, and ended at `ended` seconds.
    async fn turn(
        host: &Host,
        repo: &Repo,
        thread: ThreadId,
        turn: u32,
        (at, to): (usize, &str),
        ended: u64,
    ) {
        let before = repo.take().await.unwrap();
        edit(&repo.root, at, to);
        let after = repo.take().await.unwrap();
        host.apply(
            thread,
            vec![Action::TurnStarted(Turn {
                id: TurnId(turn),
                input: None,
                state: TurnState::Complete,
                started_ms: WallMs::from_millis(ended.saturating_mul(1_000).saturating_sub(500)),
                ended_ms: Some(WallMs::from_millis(ended.saturating_mul(1_000))),
                usage: Usage::default(),
                models: Vec::new(),
                changed: Changed::default(),
                before: Some(before),
                after: Some(after),
            })],
        );
    }

    /// Each line with an author: its number, thread and turn.
    fn lines(authors: &Authors) -> Vec<(u32, ThreadId, Option<TurnId>)> {
        authors
            .runs
            .iter()
            .flat_map(|r| {
                (r.start..r.start.saturating_add(r.lines)).map(move |l| (l, r.thread, r.turn))
            })
            .collect()
    }

    /// A line is the turn's that brought it in, across two threads in one tree; the person's
    /// edits between and after the turns are no one's; a line from before the snapshots is the
    /// thread its commit's trailer names, with the commit; and a turn more is told on the next
    /// ask, the file's stamp with it.
    #[tokio::test]
    async fn every_line_is_the_turn_that_wrote_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        run(&root, &["init", "-q", "-b", "main"]);
        run(&root, &["config", "user.name", "Test"]);
        run(&root, &["config", "user.email", "test@localhost"]);
        std::fs::write(root.join("a.txt"), format!("{}\n", LINES.join("\n"))).unwrap();
        run(&root, &["add", "-A"]);
        run(&root, &["commit", "-q", "-m", "first"]);
        let merged = ThreadId::new();
        edit(&root, 1, "ONE");
        let trailer = format!("Slopty-Thread: {merged}");
        run(&root, &["commit", "-q", "-a", "-m", "merged", "-m", &trailer]);

        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let x = thread(&host, "s1", &root);
        let y = thread(&host, "s2", &root);
        let index = dir.path().join("index");
        let repo = Repo { git: git(), root: root.clone(), index };
        turn(&host, &repo, x, 1, (3, "THREE"), 100).await;
        edit(&root, 5, "five, by hand");
        turn(&host, &repo, y, 1, (7, "SEVEN"), 200).await;
        turn(&host, &repo, x, 2, (9, "NINE"), 300).await;
        edit(&root, 11, "eleven, by hand");

        let authorship = Authorship::new(host.clone(), Some(git()));
        let absolute = root.join("a.txt").to_string_lossy().into_owned();
        let found = authorship.authors(None, &absolute).await;
        assert_eq!(found.absent, None);
        assert!(found.modified_ms.is_some() && found.blob.is_some(), "{found:?}");
        assert_eq!(
            lines(&found),
            [
                (1, merged, None),
                (3, x, Some(TurnId(1))),
                (7, y, Some(TurnId(1))),
                (9, x, Some(TurnId(2)))
            ]
        );
        let first = &found.runs[0];
        assert!(first.commit.as_deref().is_some_and(|c| c.len() == 12), "{first:?}");
        assert_eq!(found.runs[1].at_ms, WallMs::from_millis(100_000));

        // Asked by the thread for its repository's path, after a turn more.
        turn(&host, &repo, y, 2, (12, "TWELVE"), 400).await;
        let again = authorship.authors(Some(y), "a.txt").await;
        assert_eq!(again.path, "a.txt");
        assert_ne!(again.blob, found.blob);
        assert_eq!(lines(&again).last(), Some(&(12, y, Some(TurnId(2)))));
        assert_eq!(lines(&again).len(), 5);
    }

    /// A file outside git, or one that is not there, says why no line has an author.
    #[tokio::test]
    async fn a_file_with_no_history_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let loose = dir.path().join("loose.txt");
        std::fs::write(&loose, "alone\n").unwrap();
        let authorship = Authorship::new(host, Some(git()));
        let found = authorship.authors(None, &loose.to_string_lossy()).await;
        assert_eq!(found.absent.as_deref(), Some(NOT_IN_GIT));
        assert!(found.runs.is_empty(), "no line has an author");
        let relative = authorship.authors(Some(ThreadId::new()), "a.txt").await;
        assert_eq!(relative.absent.as_deref(), Some(NOT_IN_GIT));
    }
}
