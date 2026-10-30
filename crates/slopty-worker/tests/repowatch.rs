//! A repository's watch (`slopty_worker::repowatch`) on the real file system and a real git: a
//! write in a folder nobody listed, a stage and a commit in a terminal and an agent's edit in
//! place are each reported within half a second, and a repository made in a folder that had
//! none is noticed (what git ignores and an idle repository are no report: the follower's own
//! test, on tokio's paused clock). How it fails: a burst of a
//! thousand writes is reported and comes to rest, a repository deleted is said and one made again
//! in its place followed, a git directory replaced is followed afresh, and past its watches a
//! repository is looked at every few seconds, still saying nothing while idle.
//!
//! The numbers it prints are the change → report latencies (MEASUREMENTS.md, "Watches wait on
//! the worker's events"):
//!
//! ```sh
//! cargo nextest run -p slopty-worker --test repowatch --no-capture
//! ```

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "durations of a few seconds")]
mod follow {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use slopty_worker::repowatch::{HOLD, Limits, Moves, follow, follow_within};
    use tokio::sync::watch;
    use tokio::time::Instant;

    /// A change is reported within this, or the watch is no use to a person looking at it.
    const PROMPT: Duration = Duration::from_millis(500);
    /// Only a guard against a watch that never comes up: fseventsd starts one stream at a time
    /// for the whole system, 0.4 to 1 s each on macOS 27, so a start can wait behind others.
    const UP: Duration = Duration::from_secs(90);

    fn git(repo: &Path, args: &[&str]) {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Person")
            .env("GIT_AUTHOR_EMAIL", "person@example.com")
            .env("GIT_COMMITTER_NAME", "Person")
            .env("GIT_COMMITTER_EMAIL", "person@example.com")
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
    }

    /// A repository with a commit: a source tree three folders deep, an ignored `target/` and
    /// log, and a `docs/` folder.
    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(dir.path()).unwrap().join("repo");
        std::fs::create_dir_all(repo.join("src/deep/inner")).unwrap();
        std::fs::create_dir_all(repo.join("target/debug")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::write(repo.join(".gitignore"), "target/\n*.log\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        std::fs::write(repo.join("src/deep/inner/mod.rs"), "// inner\n").unwrap();
        std::fs::write(repo.join("docs/notes.md"), "notes\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        (dir, repo)
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    async fn next(moves: &mut Moves, within: Duration) -> Option<Vec<String>> {
        tokio::time::timeout(within, moves.next()).await.ok().flatten()
    }

    /// Follow `named`, once its watch is up: the first report, which every watch makes as it
    /// comes up, taken.
    async fn following(named: &[&Path]) -> (watch::Sender<Vec<String>>, Moves) {
        let (lists, rx) = watch::channel(Vec::new());
        let mut moves = follow(rx);
        lists.send_replace(named.iter().map(|p| key(p)).collect());
        let up = next(&mut moves, UP).await.expect("the watch comes up and says so");
        assert_eq!(up, named.iter().map(|p| key(p)).collect::<Vec<_>>());
        // A report the start-up made of what the setup wrote just before is let through.
        while next(&mut moves, HOLD * 2).await.is_some() {}
        (lists, moves)
    }

    /// Time from `change` to the report naming `named`, which must come within [`PROMPT`]; the
    /// reports after it, of the same burst, are let through.
    async fn reported(
        moves: &mut Moves,
        named: &Path,
        what: &str,
        change: impl FnOnce(),
    ) -> Duration {
        let at = Instant::now();
        change();
        let got = next(moves, PROMPT).await.unwrap_or_else(|| panic!("{what}: not reported"));
        let took = at.elapsed();
        assert_eq!(got, vec![key(named)], "{what}");
        while next(moves, HOLD * 2).await.is_some() {}
        println!("MEASURE repowatch {what} → report: {took:?}");
        took
    }

    /// Nothing is reported within `within` of `change`.
    async fn unreported(moves: &mut Moves, what: &str, within: Duration, change: impl FnOnce()) {
        change();
        assert_eq!(next(moves, within).await, None, "{what}: reported, though git ignores it");
    }

    /// What a person looking at a repository's status and diffs needs to hear of, however deep
    /// and whoever made it, is each reported once a burst and within half a second. That an
    /// idle repository and what git ignores make no report is the follower's own test, on
    /// tokio's paused clock
    /// (`repowatch::tests::an_idle_repository_and_what_git_ignores_say_nothing`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_is_reported_on_each_change_git_sees() {
        let (_dir, repo) = repository();
        let workspace = repo.join("docs");
        let (_lists, mut moves) = following(&[&workspace]).await;

        reported(&mut moves, &workspace, "a new file in a folder never listed", || {
            std::fs::write(repo.join("src/deep/inner/new.rs"), "// new\n").unwrap();
        })
        .await;
        reported(&mut moves, &workspace, "an agent's edit in place", || {
            let path = repo.join("src/lib.rs");
            let mut file =
                std::fs::OpenOptions::new().write(true).truncate(true).open(path).unwrap();
            file.write_all(b"pub fn two() {}\n").unwrap();
        })
        .await;
        reported(&mut moves, &workspace, "git add in a terminal", || git(&repo, &["add", "-A"]))
            .await;
        reported(&mut moves, &workspace, "git commit in a terminal", || {
            git(&repo, &["commit", "-q", "-m", "second"]);
        })
        .await;
        reported(&mut moves, &workspace, "a branch switched", || {
            git(&repo, &["switch", "-q", "-c", "feature"]);
        })
        .await;
        reported(&mut moves, &workspace, "a new folder with a file", || {
            std::fs::create_dir_all(repo.join("src/fresh")).unwrap();
            std::fs::write(repo.join("src/fresh/a.rs"), "// a\n").unwrap();
        })
        .await;
        // The folder made just now is followed too.
        reported(&mut moves, &workspace, "a write in a folder made since", || {
            std::fs::write(repo.join("src/fresh/a.rs"), "// a, again\n").unwrap();
        })
        .await;
    }

    /// A folder in no repository is followed for one made there: a `git init` is reported, and
    /// the new repository followed from then on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_made_in_a_folder_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let folder = std::fs::canonicalize(dir.path()).unwrap().join("plain");
        std::fs::create_dir_all(&folder).unwrap();
        let (lists, rx) = watch::channel(Vec::new());
        let mut moves = follow(rx);
        let mut status = moves.status();
        lists.send_replace(vec![key(&folder)]);
        status.wait_for(|s| s.lists >= 1).await.unwrap();
        assert_eq!(status.borrow().outside, vec![key(&folder)]);
        // The folder follower comes up at once on kqueue or inotify.
        tokio::time::sleep(HOLD).await;
        git(&folder, &["init", "-q", "-b", "main"]);
        let made = next(&mut moves, UP).await.expect("the new repository is said");
        assert_eq!(made, vec![key(&folder)]);
        status.wait_for(|s| s.up == vec![key(&folder)]).await.unwrap();
        while next(&mut moves, HOLD * 2).await.is_some() {}
        reported(&mut moves, &folder, "a file in the new repository", || {
            std::fs::write(folder.join("a.txt"), "a\n").unwrap();
        })
        .await;
    }

    /// A repository no folder of the list names any more is let go: its changes say nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_left_out_of_the_list_is_let_go() {
        let (_dir, repo) = repository();
        let (lists, mut moves) = following(&[&repo]).await;
        let mut status = moves.status();
        lists.send_replace(Vec::new());
        status.wait_for(|s| s.lists >= 2 && s.repos.is_empty()).await.unwrap();
        unreported(&mut moves, "a change in a repository let go", PROMPT, || {
            std::fs::write(repo.join("src/lib.rs"), "pub fn three() {}\n").unwrap();
        })
        .await;
    }

    /// The reports of `moves` until it has been quiet for `quiet`, counted.
    async fn count_until_quiet(moves: &mut Moves, quiet: Duration) -> usize {
        let mut heard = 0;
        while next(moves, quiet).await.is_some() {
            heard += 1;
        }
        heard
    }

    /// A build or a checkout writing a thousand files is reported, and so is an agent streaming a
    /// log a thousand writes long, and both come to rest. How many reports a burst is (one each
    /// [`HOLD`]) is the follower's own pacing, held on tokio's paused clock in
    /// `repowatch::tests::a_second_of_steady_changes_is_a_report_each_hold`: here the bound came
    /// from the measured write time, which the events lag under load.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_a_thousand_writes_is_reported() {
        let (_dir, repo) = repository();
        let (_lists, mut moves) = following(&[&repo]).await;
        std::fs::create_dir_all(repo.join("src/many")).unwrap();
        let at = Instant::now();
        for n in 0..1_000 {
            std::fs::write(repo.join(format!("src/many/f{n}.rs")), format!("// {n}\n")).unwrap();
        }
        let wrote = at.elapsed();
        let first = next(&mut moves, PROMPT + wrote).await.expect("the burst is reported");
        let took = at.elapsed();
        assert_eq!(first, vec![key(&repo)]);
        let more = count_until_quiet(&mut moves, HOLD * 3).await;
        println!(
            "MEASURE repowatch 1000 files written in {wrote:?}: first report {took:?}, {} reports",
            more + 1
        );

        // A thousand writes into one file, an agent streaming a log it tracks.
        let at = Instant::now();
        let mut file = std::fs::File::create(repo.join("docs/stream.md")).unwrap();
        for n in 0..1_000 {
            writeln!(file, "line {n}").unwrap();
            file.flush().unwrap();
        }
        drop(file);
        let wrote = at.elapsed();
        assert!(next(&mut moves, PROMPT + wrote).await.is_some(), "the stream is reported");
        let more = count_until_quiet(&mut moves, HOLD * 3).await;
        println!("MEASURE repowatch 1000 writes to one file in {wrote:?}: {} reports", more + 1);
    }

    /// A repository deleted is said, its folder is then followed for one made there, and a
    /// repository cloned or made again in its place is followed afresh.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repository_deleted_is_said_and_one_made_in_its_place_followed() {
        let (dir, repo) = repository();
        let (_lists, mut moves) = following(&[&repo]).await;
        let mut status = moves.status();
        reported(&mut moves, &repo, "the repository deleted", || {
            std::fs::remove_dir_all(&repo).unwrap();
        })
        .await;
        status.wait_for(|s| s.outside == vec![key(&repo)] && s.repos.is_empty()).await.unwrap();

        // Made again where it was, as a clone would: one report says so once it is followed.
        let made = dir.path().join("made");
        std::fs::create_dir_all(made.join("src")).unwrap();
        std::fs::write(made.join("src/lib.rs"), "pub fn again() {}\n").unwrap();
        git(&made, &["init", "-q", "-b", "main"]);
        git(&made, &["add", "."]);
        git(&made, &["commit", "-q", "-m", "again"]);
        std::fs::rename(&made, &repo).unwrap();
        let said = next(&mut moves, UP).await.expect("the repository made again is said");
        assert_eq!(said, vec![key(&repo)]);
        status.wait_for(|s| s.up == vec![key(&repo)]).await.unwrap();
        while next(&mut moves, HOLD * 2).await.is_some() {}
        reported(&mut moves, &repo, "a write in the repository made again", || {
            std::fs::write(repo.join("src/lib.rs"), "pub fn and_again() {}\n").unwrap();
        })
        .await;
    }

    /// A git directory replaced under the working tree (`rm -rf .git && git init`) is said, and
    /// the new one followed: its first commit and a branch made in it are reported.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_git_directory_replaced_is_followed_afresh() {
        let (_dir, repo) = repository();
        let (_lists, mut moves) = following(&[&repo]).await;
        let mut status = moves.status();
        let lists = status.borrow().lists;
        reported(&mut moves, &repo, "the git directory replaced", || {
            std::fs::remove_dir_all(repo.join(".git")).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
        })
        .await;
        // Followed again once the report found the new git directory there.
        status.wait_for(|s| s.lists > lists && s.up == vec![key(&repo)]).await.unwrap();
        while next(&mut moves, HOLD * 2).await.is_some() {}
        reported(&mut moves, &repo, "a stage in the new git directory", || {
            git(&repo, &["add", "."]);
        })
        .await;
        reported(&mut moves, &repo, "a commit in the new git directory", || {
            git(&repo, &["commit", "-q", "-m", "anew"]);
        })
        .await;
        reported(&mut moves, &repo, "a branch made in the new git directory", || {
            git(&repo, &["switch", "-q", "-c", "fresh"]);
        })
        .await;
    }

    /// Past the watches a repository may hold, it is looked at every [`Limits::poll`] instead:
    /// still one report as it is taken up, none while idle, and a change within a look. It looks
    /// every 300 ms, a tenth of [`POLL`], so the test waits a tenth as long.
    ///
    /// [`POLL`]: slopty_worker::repowatch::POLL
    #[tokio::test(flavor = "multi_thread")]
    async fn past_its_watches_a_repository_is_looked_at_and_says_nothing_idle() {
        const LOOK: Duration = Duration::from_millis(300);
        let (_dir, repo) = repository();
        let (lists, rx) = watch::channel(Vec::new());
        let mut moves = follow_within(rx, Limits { watches: 0, poll: LOOK });
        let mut status = moves.status();
        lists.send_replace(vec![key(&repo)]);
        let up = next(&mut moves, UP).await.expect("it is taken up and says so");
        assert_eq!(up, vec![key(&repo)]);
        status.wait_for(|s| s.polled == vec![key(&repo)]).await.unwrap();
        while next(&mut moves, HOLD * 2).await.is_some() {}
        assert_eq!(next(&mut moves, LOOK * 2 + HOLD).await, None, "idle: two looks, no report");
        let at = Instant::now();
        std::fs::write(repo.join("src/deep/inner/mod.rs"), "// inner, edited\n").unwrap();
        let said = next(&mut moves, LOOK + PROMPT).await.expect("a look finds the edit");
        assert_eq!(said, vec![key(&repo)]);
        println!("MEASURE repowatch looked at: edit → report {:?}", at.elapsed());
        // Edited again: a file already modified, whose stamp is all that moves.
        std::fs::write(repo.join("src/deep/inner/mod.rs"), "// inner, edited twice\n").unwrap();
        let said = next(&mut moves, LOOK + PROMPT).await.expect("a look finds the second edit");
        assert_eq!(said, vec![key(&repo)]);
    }
}
