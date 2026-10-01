//! File tiles following the disk (`slopty_worker::fswatch`), on the real file system: every
//! kind of save reaches the follower once, a burst is one report, a directory deleted and made
//! again or a symlink's target is followed, and what the kernel cannot watch is polled. Folder
//! tiles the same way: each change of a folder's entries is one report, a write inside a file
//! none, and a folder deleted and made again is followed.
//!
//! The measurement beside them, edit → report against `FSEvents` and a bare kqueue:
//!
//! ```sh
//! cargo nextest run -p slopty-worker --release --test fswatch --run-ignored only --no-capture
//! ```

#[cfg(test)]
#[expect(clippy::arithmetic_side_effects, reason = "durations of a few seconds")]
mod follow {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use slopty_testkit::stats::Spread;
    use slopty_worker::fswatch::{
        CONTENT_HOLD, Changes, HOLD, Limits, Status, follow, follow_folders,
    };
    use tokio::sync::watch;

    /// Longer than any report takes on a loaded machine, short of a stuck test.
    const WITHIN: Duration = Duration::from_secs(3);

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Follow `paths`, once the follower has taken them.
    async fn following(paths: &[&Path], limits: Limits) -> (watch::Sender<Vec<String>>, Changes) {
        let (lists, rx) = watch::channel(Vec::new());
        let changes = follow(rx, limits);
        let mut status = changes.status();
        lists.send_replace(paths.iter().map(|p| key(p)).collect());
        status.wait_for(|s| s.lists >= 1).await.unwrap();
        (lists, changes)
    }

    fn status(changes: &Changes) -> Status {
        changes.status().borrow().clone()
    }

    async fn next(changes: &mut Changes, within: Duration) -> Option<Vec<String>> {
        tokio::time::timeout(within, changes.next()).await.ok().flatten()
    }

    /// No report comes within `window`.
    async fn quiet(changes: &mut Changes, window: Duration) -> bool {
        next(changes, window).await.is_none()
    }

    fn write_in_place(path: &Path, text: &str) {
        let mut file =
            std::fs::OpenOptions::new().write(true).truncate(true).create(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn rename_over(path: &Path, text: &str) {
        let tmp = path.with_extension("tmp~");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    /// The four ways a file changes under a tile, in the order a round does them.
    const KINDS: [&str; 4] = ["write in place", "rename over", "delete", "create"];

    /// Change `path` the way `KINDS[kind]` says.
    fn change(kind: &str, path: &Path, round: usize) {
        if kind == KINDS[0] {
            write_in_place(path, &format!("edit {round}\n"));
        } else if kind == KINDS[1] {
            rename_over(path, &format!("renamed {round}\n"));
        } else if kind == KINDS[2] {
            std::fs::remove_file(path).unwrap();
        } else {
            std::fs::write(path, format!("made {round}\n")).unwrap();
        }
    }

    /// `rounds` of every kind of change, each reported once: edit → report, per kind.
    async fn rounds(rounds: usize) -> Vec<(&'static str, Vec<Duration>)> {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.md");
        std::fs::write(&path, "first\n").unwrap();
        let (_lists, mut changes) = following(&[&path], Limits::default()).await;
        assert!(status(&changes).events, "no kernel events");
        assert!(status(&changes).polled.is_empty(), "a local file is polled");
        let mut taken: Vec<(&str, Vec<Duration>)> =
            KINDS.iter().map(|k| (*k, Vec::new())).collect();
        for round in 0..rounds {
            for (kind, samples) in &mut taken {
                let at = Instant::now();
                change(kind, &path, round);
                let got = next(&mut changes, WITHIN).await;
                samples.push(at.elapsed());
                assert_eq!(got, Some(vec![key(&path)]), "{kind} in round {round}");
                assert!(quiet(&mut changes, HOLD * 2).await, "{kind} reported twice");
            }
        }
        taken
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_kind_of_save_reaches_the_follower_once() {
        for (kind, samples) in rounds(5).await {
            let spread = Spread::of_durations(&samples).unwrap().per(1_000);
            println!("{kind}: edit → report µs {spread}");
            // Events, not a poll: the old poll took up to a second.
            assert!(spread.p50 < 100_000, "{kind}: p50 {} µs", spread.p50);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_writes_is_one_report() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("burst.log");
        std::fs::write(&path, "old\n").unwrap();
        let (_lists, mut changes) = following(&[&path], Limits::default()).await;

        let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
        for line in 0..200 {
            writeln!(file, "line {line}").unwrap();
        }
        drop(file);
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]));
        assert!(quiet(&mut changes, HOLD * 2).await, "a burst reported twice");

        // A writer that keeps at it, a line a millisecond or so: reported at its first write,
        // then at most once a hold, and once more after its last write.
        let at = Instant::now();
        let mut reports = 0_u32;
        for line in 0..100 {
            let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(file, "more {line}").unwrap();
            if let Some(paths) = next(&mut changes, Duration::from_millis(1)).await {
                assert_eq!(paths, vec![key(&path)]);
                reports += 1;
            }
        }
        let writing = at.elapsed();
        while let Some(paths) = next(&mut changes, HOLD * 2).await {
            assert_eq!(paths, vec![key(&path)]);
            reports += 1;
        }
        let most = u32::try_from(writing.as_millis() / HOLD.as_millis()).unwrap() + 2;
        assert!((1..=most).contains(&reports), "{reports} reports over {writing:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_truncate_waits_for_the_writes_after_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("slow.txt");
        std::fs::write(&path, "before\n").unwrap();
        let (_lists, mut changes) = following(&[&path], Limits::default()).await;

        let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
        tokio::time::sleep(HOLD / 3).await;
        file.write_all(b"after\n").unwrap();
        drop(file);
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\n");
        assert!(quiet(&mut changes, HOLD * 2).await, "the empty file was reported too");

        // A file really emptied is reported: at once where the kernel says its writer closed
        // it (inotify's `CLOSE_WRITE`), once the hold is out where it cannot (kqueue).
        write_in_place(&path, "");
        let at = Instant::now();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]));
        let waited = at.elapsed();
        if cfg!(target_vendor = "apple") {
            assert!(waited >= HOLD / 2, "reported before the hold: {waited:?}");
        } else {
            assert!(waited < HOLD / 2, "held though its writer closed it: {waited:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_directory_deleted_and_made_again_is_followed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("build/out");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("report.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (_lists, mut changes) = following(&[&path], Limits::default()).await;

        std::fs::remove_dir_all(root.path().join("build")).unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]), "delete");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(quiet(&mut changes, HOLD * 2).await, "an empty directory reported");
        std::fs::write(&path, "two\n").unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]), "made again");
        write_in_place(&path, "three\n");
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&path)]), "then written");
        assert!(status(&changes).polled.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlink_follows_its_target() {
        let root = tempfile::tempdir().unwrap();
        let (here, there) = (root.path().join("here"), root.path().join("there"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&there).unwrap();
        let (real, other) = (there.join("real.md"), there.join("other.md"));
        std::fs::write(&real, "real\n").unwrap();
        std::fs::write(&other, "other\n").unwrap();
        let link = here.join("link.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (_lists, mut changes) = following(&[&link], Limits::default()).await;

        write_in_place(&real, "real, edited\n");
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&link)]), "target written");
        rename_over(&real, "real, saved\n");
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&link)]), "target replaced");

        // The link pointed elsewhere, as `ln -sfn` does it.
        let tmp = here.join("link.tmp");
        std::os::unix::fs::symlink(&other, &tmp).unwrap();
        std::fs::rename(&tmp, &link).unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&link)]), "retargeted");
        write_in_place(&other, "other, edited\n");
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&link)]), "new target");
        write_in_place(&real, "old target\n");
        assert!(quiet(&mut changes, HOLD * 2).await, "the old target still followed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn past_the_watch_limit_a_file_is_polled() {
        let root = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = ["a", "b", "c"]
            .iter()
            .map(|d| {
                let dir = root.path().join(d);
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("f.txt");
                std::fs::write(&path, "x").unwrap();
                path
            })
            .collect();
        let poll = Duration::from_millis(1500);
        let limits = Limits { watches: 2, poll };
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let (lists, mut changes) = following(&refs, limits).await;
        let polled = status(&changes).polled;
        assert!(polled.contains(&key(&paths[2])), "{polled:?}");
        assert!(!polled.contains(&key(&paths[0])), "{polled:?}");

        // The polled file is still seen, within a period.
        write_in_place(&paths[2], "polled\n");
        let at = Instant::now();
        assert_eq!(next(&mut changes, poll * 2).await, Some(vec![key(&paths[2])]));
        assert!(at.elapsed() <= poll * 2);

        // A path dropped from the list is no longer reported, and frees its watches.
        lists.send_replace(vec![key(&paths[2])]);
        changes.status().wait_for(|s| s.lists >= 2).await.unwrap();
        assert!(status(&changes).polled.is_empty(), "{:?}", status(&changes).polled);
        write_in_place(&paths[0], "dropped\n");
        write_in_place(&paths[2], "followed\n");
        assert_eq!(next(&mut changes, poll / 2).await, Some(vec![key(&paths[2])]));
        assert!(quiet(&mut changes, HOLD * 2).await);
    }

    /// Run as a user, a directory without read permission cannot be watched (as root on Linux
    /// it can, so this holds on the Mac only).
    #[cfg(target_vendor = "apple")]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unwatchable_directory_is_polled() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("locked");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, "x").unwrap();
        // Search and write, no read: the file can be stat'd and written, the directory not
        // opened.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).unwrap();
        let poll = Duration::from_millis(300);
        let (_lists, mut changes) = following(&[&path], Limits { poll, ..Limits::default() }).await;
        assert_eq!(status(&changes).polled, vec![key(&path)]);
        write_in_place(&path, "seen by the poll\n");
        assert_eq!(next(&mut changes, poll * 4).await, Some(vec![key(&path)]));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_follower_ends_with_its_list() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("f.txt");
        std::fs::write(&path, "x").unwrap();
        let (lists, mut changes) = following(&[&path], Limits::default()).await;
        drop(lists);
        assert_eq!(tokio::time::timeout(WITHIN, changes.next()).await, Ok(None));
    }

    /// Follow the folders `paths`, once the follower has taken them.
    async fn following_folders(paths: &[&Path]) -> (watch::Sender<Vec<String>>, Changes) {
        let (lists, rx) = watch::channel(Vec::new());
        let changes = follow_folders(rx, Limits::default());
        let mut status = changes.status();
        lists.send_replace(paths.iter().map(|p| key(p)).collect());
        status.wait_for(|s| s.lists >= 1).await.unwrap();
        (lists, changes)
    }

    /// The ways a folder's entries change under its tile, in the order a round does them.
    const ENTRY_KINDS: [&str; 5] =
        ["file made", "file renamed", "file deleted", "dir made", "dir deleted"];

    fn change_entries(kind: &str, dir: &Path, round: usize) {
        let (file, moved, sub) = (dir.join("new.txt"), dir.join("moved.txt"), dir.join("sub"));
        match kind {
            "file made" => std::fs::write(&file, format!("{round}\n")).unwrap(),
            "file renamed" => std::fs::rename(&file, &moved).unwrap(),
            "file deleted" => std::fs::remove_file(&moved).unwrap(),
            "dir made" => std::fs::create_dir_all(&sub).unwrap(),
            _ => std::fs::remove_dir(&sub).unwrap(),
        }
    }

    /// `rounds` of every change of entries, each reported once: change → report, per kind.
    async fn folder_rounds(rounds: usize) -> Vec<(&'static str, Vec<Duration>)> {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let (_lists, mut changes) = following_folders(&[&dir]).await;
        assert!(status(&changes).events, "no kernel events");
        assert!(status(&changes).polled.is_empty(), "a local folder is polled");
        let mut taken: Vec<(&str, Vec<Duration>)> =
            ENTRY_KINDS.iter().map(|k| (*k, Vec::new())).collect();
        for round in 0..rounds {
            for (kind, samples) in &mut taken {
                let at = Instant::now();
                change_entries(kind, &dir, round);
                let got = next(&mut changes, WITHIN).await;
                samples.push(at.elapsed());
                assert_eq!(got, Some(vec![key(&dir)]), "{kind} in round {round}");
                assert!(quiet(&mut changes, HOLD * 2).await, "{kind} reported twice");
            }
        }
        taken
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_change_of_a_folders_entries_is_one_report() {
        for (kind, samples) in folder_rounds(3).await {
            let spread = Spread::of_durations(&samples).unwrap().per(1_000);
            println!("{kind}: change → report µs {spread}");
            assert!(spread.p50 < 100_000, "{kind}: p50 {} µs", spread.p50);
        }
    }

    /// A write inside one of a folder's files changes the size its listing shows: the folder is
    /// reported, after [`CONTENT_HOLD`], and a file that keeps growing reports it at that rate
    /// at most. So is an entry made inside one of its subfolders, whose count the listing shows
    /// (macOS; on Linux the subfolder's own entries are not watched).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_inside_a_file_of_the_folder_is_reported_for_its_size() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("logs");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("log.txt");
        std::fs::write(&file, "one\n").unwrap();
        let (_lists, mut changes) = following_folders(&[&dir]).await;
        let at = Instant::now();
        write_in_place(&file, "two, longer\n");
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&dir)]), "the size moved");
        let took = at.elapsed();
        assert!(took >= CONTENT_HOLD, "paced, not at once: {took:?}");
        assert!(quiet(&mut changes, CONTENT_HOLD * 2).await, "one write, one report");

        let started = Instant::now();
        let mut reports = 0_u32;
        while started.elapsed() < CONTENT_HOLD * 4 {
            let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            f.write_all(b"a line\n").unwrap();
            drop(f);
            if next(&mut changes, Duration::from_millis(10)).await.is_some() {
                reports += 1;
            }
        }
        assert!((1..=5).contains(&reports), "a growing log is paced: {reports} reports");
        while next(&mut changes, CONTENT_HOLD * 2).await.is_some() {}

        if cfg!(target_os = "macos") {
            std::fs::write(dir.join("sub/new.txt"), "x").unwrap();
            let got = next(&mut changes, WITHIN).await;
            assert_eq!(got, Some(vec![key(&dir)]), "a subfolder's count moved");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_folder_deleted_and_made_again_is_followed() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("build");
        std::fs::create_dir_all(&dir).unwrap();
        let (_lists, mut changes) = following_folders(&[&dir]).await;
        std::fs::remove_dir(&dir).unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&dir)]), "gone");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&dir)]), "back");
        std::fs::write(dir.join("out.o"), "x").unwrap();
        assert_eq!(next(&mut changes, WITHIN).await, Some(vec![key(&dir)]), "watched anew");
    }

    /// Folder tiles' change → report (MEASUREMENTS.md, "folder tiles on kernel events").
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement, run by hand"]
    async fn folder_change_to_report() {
        println!("| change | p50 µs | p95 µs | p99 µs | max µs |");
        println!("| --- | --- | --- | --- | --- |");
        for (kind, samples) in folder_rounds(200).await {
            let s = Spread::of_durations(&samples).unwrap().per(1_000);
            println!("| {kind} | {} | {} | {} | {} |", s.p50, s.p95, s.p99, s.max);
        }
    }

    /// The numbers behind the choice (MEASUREMENTS.md, "file tiles on kernel events").
    ///
    /// The follower's edit → report; on macOS also the bare kqueue event under it, and
    /// `FSEvents` through `notify` with its flags (latency 0, `NoDefer`, `FileEvents`). Then the
    /// CPU each spends while another process writes 5 000 times under the watched directory.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement, run by hand"]
    async fn edit_to_report_against_fsevents_and_bare_kqueue() {
        const ROUNDS: usize = 200;
        println!("| path | change | p50 µs | p95 µs | p99 µs | max µs |");
        println!("| --- | --- | --- | --- | --- | --- |");
        let table = |who: &str, taken: Vec<(&str, Vec<Duration>)>| {
            for (kind, samples) in taken {
                let s = Spread::of_durations(&samples).unwrap().per(1_000);
                println!("| {who} | {kind} | {} | {} | {} | {} |", s.p50, s.p95, s.p99, s.max);
            }
        };
        table("follower (report)", rounds(ROUNDS).await);
        #[cfg(target_vendor = "apple")]
        {
            table("bare kqueue (event)", raw::kqueue_rounds(ROUNDS).await);
            table("FSEvents via notify (event)", raw::fsevents_rounds(ROUNDS).await);
        }

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("notes.md");
        std::fs::create_dir_all(root.path().join("noise")).unwrap();
        std::fs::write(&path, "x").unwrap();
        let cpu = raw::cpu();
        {
            let (_lists, _changes) = following(&[&path], Limits::default()).await;
            raw::noise(root.path());
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        println!(
            "follower CPU over 5 000 writes beside the file: {:?}",
            raw::cpu().saturating_sub(cpu)
        );
        #[cfg(target_vendor = "apple")]
        {
            let cpu = raw::cpu();
            raw::fsevents_noise(root.path()).await;
            println!("FSEvents CPU over the same: {:?}", raw::cpu().saturating_sub(cpu));
        }
    }

    /// The bare backends and the CPU clock, for the measurement only.
    mod raw {
        use std::path::Path;
        use std::time::Duration;
        #[cfg(target_vendor = "apple")]
        use std::time::Instant;

        #[cfg(target_vendor = "apple")]
        use tokio::sync::mpsc;

        #[cfg(target_vendor = "apple")]
        use super::{KINDS, change};

        pub fn cpu() -> Duration {
            // SAFETY: `rusage` is plain old data, for which all zeroes is a value.
            let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
            // SAFETY: `getrusage` (sys/resource.h) fills the struct it is given.
            let got = unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut usage) };
            assert_eq!(got, 0);
            let micros = |t: libc::timeval| {
                Duration::from_secs(t.tv_sec.try_into().unwrap())
                    + Duration::from_micros(t.tv_usec.try_into().unwrap())
            };
            micros(usage.ru_utime) + micros(usage.ru_stime)
        }

        /// Another process writes 5 000 times under `dir/noise`.
        pub fn noise(dir: &Path) {
            let script = format!(
                "i=0; while [ $i -lt 5000 ]; do echo n > '{}'/n$((i % 50)); i=$((i+1)); done",
                dir.join("noise").display()
            );
            assert!(
                std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(script)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        #[cfg(target_vendor = "apple")]
        async fn measure(
            dir: &Path,
            events: &mut mpsc::UnboundedReceiver<Instant>,
            rounds: usize,
        ) -> Vec<(&'static str, Vec<Duration>)> {
            let path = dir.join("f.txt");
            let mut taken: Vec<(&str, Vec<Duration>)> =
                KINDS.iter().map(|k| (*k, Vec::new())).collect();
            for round in 0..rounds {
                for (kind, samples) in &mut taken {
                    tokio::time::sleep(Duration::from_millis(3)).await;
                    while events.try_recv().is_ok() {}
                    let at = Instant::now();
                    change(kind, &path, round);
                    let got = tokio::time::timeout(Duration::from_secs(2), events.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    samples.push(got.saturating_duration_since(at));
                }
            }
            taken
        }

        #[cfg(target_vendor = "apple")]
        pub async fn fsevents_rounds(rounds: usize) -> Vec<(&'static str, Vec<Duration>)> {
            use notify::Watcher as _;
            let root = tempfile::tempdir().unwrap();
            let dir = std::fs::canonicalize(root.path()).unwrap();
            let path = dir.join("f.txt");
            std::fs::write(&path, "x").unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let target = path.clone();
            let mut watcher = notify::RecommendedWatcher::new(
                move |e: notify::Result<notify::Event>| {
                    if e.is_ok_and(|e| e.paths.contains(&target)) {
                        tx.send(Instant::now()).unwrap_or_default();
                    }
                },
                notify::Config::default(),
            )
            .unwrap();
            watcher.watch(&dir, notify::RecursiveMode::NonRecursive).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            measure(&dir, &mut rx, rounds).await
        }

        #[cfg(target_vendor = "apple")]
        pub async fn fsevents_noise(dir: &Path) {
            use notify::Watcher as _;
            let dir = std::fs::canonicalize(dir).unwrap();
            let mut watcher = notify::RecommendedWatcher::new(
                |_: notify::Result<notify::Event>| {},
                notify::Config::default(),
            )
            .unwrap();
            watcher.watch(&dir, notify::RecursiveMode::NonRecursive).unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            noise(&dir);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        /// kqueue on the directory and the file, as the follower registers them, timed at the
        /// event with no quiet window and no stamp.
        #[cfg(target_vendor = "apple")]
        pub async fn kqueue_rounds(rounds: usize) -> Vec<(&'static str, Vec<Duration>)> {
            use std::os::fd::AsRawFd as _;
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().to_path_buf();
            let path = dir.join("f.txt");
            std::fs::write(&path, "x").unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let file_events = libc::NOTE_WRITE
                | libc::NOTE_EXTEND
                | libc::NOTE_ATTRIB
                | libc::NOTE_DELETE
                | libc::NOTE_RENAME;
            let open = |p: &Path| {
                let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
                // SAFETY: `open` (fcntl.h) with a NUL-terminated path.
                unsafe { libc::open(c.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) }
            };
            let add = move |kq: i32, fd: i32, flags: u32| {
                let change = libc::kevent {
                    ident: usize::try_from(fd).unwrap(),
                    filter: libc::EVFILT_VNODE,
                    flags: libc::EV_ADD | libc::EV_CLEAR,
                    fflags: flags,
                    data: 0,
                    udata: std::ptr::null_mut(),
                };
                // SAFETY: `kevent` (sys/event.h) reads one change and writes no events.
                unsafe {
                    libc::kevent(
                        kq,
                        &raw const change,
                        1,
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null(),
                    );
                }
            };
            // SAFETY: `kqueue` (sys/event.h) takes no arguments.
            let kq = unsafe { libc::kqueue() };
            let dir_fd = std::fs::File::open(&dir).unwrap();
            add(kq, dir_fd.as_raw_fd(), libc::NOTE_WRITE);
            let watched = path.clone();
            std::thread::spawn(move || {
                let mut file_fd = open(&watched);
                add(kq, file_fd, file_events);
                // SAFETY: `kevent` is plain old data; zeroed is a valid empty event list.
                let mut events: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
                loop {
                    // SAFETY: `kevent` writes at most 16 events into the 16 given.
                    let n = unsafe {
                        libc::kevent(
                            kq,
                            std::ptr::null(),
                            0,
                            events.as_mut_ptr(),
                            16,
                            std::ptr::null(),
                        )
                    };
                    if n < 0 || tx.send(Instant::now()).is_err() {
                        return;
                    }
                    // SAFETY: `close` (unistd.h) on the descriptor this thread opened.
                    unsafe {
                        libc::close(file_fd);
                    }
                    file_fd = open(&watched);
                    if file_fd >= 0 {
                        add(kq, file_fd, file_events);
                    }
                }
            });
            tokio::time::sleep(Duration::from_millis(50)).await;
            let taken = measure(&dir, &mut rx, rounds).await;
            drop(dir_fd);
            taken
        }
    }
}
