//! A check's crates checked on this Mac beside the runners, at a priority below the person's
//! session.
//!
//! `cargo xtask remote -p <crate>…` checks the commit it made of the tree (`HEAD` and the lane's
//! own paths). Its crates' fmt, host clippy, rustdoc, shear, typos and tests run here, with clippy
//! for both iOS triples in a full check, where an incremental build returns in a minute or two and
//! no queue stands in front of it; what needs Linux (its clippy and tests) and the e2e cases and
//! probes run on a runner, side by side (`docs/decisions/tooling.md`, "Checks run where they
//! return soonest" and "A lane's check returns in minutes").
//!
//! Each check works in a lane directory, `target/local/<dir>/`: the commit's tree, synced as the
//! gate syncs its snapshot so that unchanged files keep their times and cargo rebuilds only what
//! changed, and a target dir beside it. A check holds its directory ([`hold`]) only while it
//! works there; a new directory starts as a clone of a warm one ([`seed_dir`]); and idle ones go
//! whole ([`evict_idle`]), the warmest kept. The other lanes' unfinished edits in the checkout
//! never reach a check.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

/// What a local check found and what it cost.
pub struct Outcome {
    /// Whether every step passed.
    pub passed: bool,
    /// From the sync of the tree to the last step.
    pub took: Duration,
    /// The highest thermal pressure level seen meanwhile (0 nominal … 4), `None` unread.
    pub thermal: Option<u8>,
    /// The highest one-minute load average seen meanwhile.
    pub load: f64,
    /// The first line of what failed, when something did.
    pub failed: Option<String>,
}

impl Outcome {
    /// A check of no crates: nothing ran, nothing failed.
    pub const fn nothing() -> Self {
        Self { passed: true, took: Duration::ZERO, thermal: None, load: 0.0, failed: None }
    }
}

/// How far a local check goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// fmt, host clippy, rustdoc, shear, typos and the tests.
    Quick,
    /// The quick check and clippy for both iOS triples, which a runner ran after a cold setup
    /// and host clippy and rustdoc again, about 9 minutes for 3 of work.
    Full,
    /// A warm start for the next check after main's lock moved: the quick check's builds, the
    /// tests built and not run.
    Warm,
}

/// The directory `name`'s local checks use.
pub fn dir(root: &Utf8Path, name: &str) -> Utf8PathBuf {
    root.join("target").join("local").join(name)
}

/// Check `crates` at commit `sha` in the lane directory `held` holds, to `depth`: fmt, host clippy,
/// rustdoc, shear and typos, iOS clippy in a full check, then the tests as a Mac runner runs
/// them, the spawned binaries built first. Prints each step as `xtask check` does. `moved_lock`
/// says the commit moves the lock, which keeps the directory from being warmed at `HEAD`'s. A
/// warm makes its hold stoppable once its tree is synced, and builds only those of `crates` the
/// commit still has.
pub fn check(
    root: &Utf8Path,
    held: &mut Held,
    sha: &str,
    crates: &[String],
    depth: Depth,
    moved_lock: bool,
) -> Result<Outcome> {
    let started = Instant::now();
    // At `nice 10`, which every build and test it starts inherits: the person's own session comes
    // first, and a check takes what it leaves. A warm too: at 15 (or 20) it took half the CPU
    // against a check at 10, as much as at 10 (2026-10-31), so it gives way by stopping when a
    // check wants its directory instead.
    rustix::process::setpriority_process(None, 10).context("lower the check's priority")?;
    room_for_build(root)?;
    let dir = dir(root, held.name());
    let tree = sync_tree(root, &dir, sha)?;
    let warmed: Vec<String>;
    let crates = if depth == Depth::Warm {
        held.stoppable(stop_the_warm)?;
        let packages = crate::tools::packages_in(&tree)?;
        warmed =
            crates.iter().filter(|c| packages.iter().any(|p| &p.name == *c)).cloned().collect();
        if warmed.is_empty() {
            return Ok(Outcome::nothing());
        }
        &*warmed
    } else {
        crates
    };
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    target_by_config(&dir)?;
    // `.cargo/config.toml` names it relative to the tree: the checkout's is the one every build
    // shares.
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    crate::tools::use_zig_for(&sh, &tree)?;
    sh.set_var(LOCAL, "1");
    // nextest's setup script runs `$SLOPTY_XTASK spawned-bins`, else `cargo xtask`, which built a
    // whole xtask in every lane's directory (~740 MB each) after any change to xtask or the lock.
    // This binary is built; a lane that changes the spawned list is checked by the runners too.
    sh.set_var("SLOPTY_XTASK", std::env::current_exe().context("the xtask binary")?);
    let watch = Watch::start();
    let names: Vec<&str> = crates.iter().map(String::as_str).collect();
    // A warm builds what the next check reuses (clippy, rustdoc, the tests' builds): fmt, shear
    // and typos leave nothing, and a `HEAD` that fails one stopped the warm before clippy.
    let part =
        if depth == Depth::Warm { crate::check::Part::Builds } else { crate::check::Part::Lint };
    let lint = crate::check::run(&sh, crates, true, part).and_then(|()| {
        if depth == Depth::Full { crate::check::clippy_ios(&sh, &names) } else { Ok(()) }
    });
    let tests = lint.as_ref().ok().map(|()| {
        if depth == Depth::Warm {
            build_tests(&sh, &names)
        } else {
            crate::check::run(&sh, crates, true, crate::check::Part::Tests)
        }
    });
    let (thermal, load) = watch.finish();
    if let Err(e) = &lint {
        println!("✘ local lint: {e:#}");
    }
    if let Some(Err(e)) = &tests {
        println!("✘ local tests: {e:#}");
    }
    let failed = match (&lint, &tests) {
        (Err(e), _) | (_, Some(Err(e))) => {
            Some(e.to_string().lines().next().unwrap_or("").to_owned())
        }
        _ => None,
    };
    let passed = failed.is_none() && tests.is_some();
    State::read(&dir).after(crates, sha, moved_lock).write(&dir);
    Ok(Outcome { passed, took: started.elapsed(), thermal, load, failed })
}

/// Sync lane directory `dir`'s tree to commit `sha` (`gate::snapshot_commit`), safe against a
/// sync stopped halfway. The sync takes the tree's manifest (`tree.index`) for each file's
/// content and writes a file only where the commit's blob differs from the manifest's, so a sync
/// stopped after writing files and before the manifest left files no manifest names. A marker,
/// `tree.syncing`, stands while a sync runs; one that finds it left behind first drops from the
/// manifest every file whose content is not the blob the manifest names, and those are written
/// again. `Cargo.lock` is verified every time, since a check's cargo rewrites it in the tree
/// when a member's manifest moved the resolution.
pub fn sync_tree(root: &Utf8Path, dir: &Utf8Path, sha: &str) -> Result<Utf8PathBuf> {
    let marker = dir.join("tree.syncing");
    if marker.exists() {
        let dropped = verify_manifest(root, dir, None)?;
        println!(
            "  an earlier sync of {dir} stopped halfway: {dropped} of its files written again"
        );
    } else {
        verify_manifest(root, dir, Some("Cargo.lock"))?;
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {dir}"))?;
    std::fs::write(&marker, "").with_context(|| format!("write {marker}"))?;
    let tree = crate::gate::snapshot_commit(root, dir, sha)?;
    std::fs::remove_file(&marker).with_context(|| format!("remove {marker}"))?;
    Ok(tree)
}

/// Drop from lane directory `dir`'s tree manifest each entry (only `only`'s, when named) whose
/// file is gone or holds another blob than the entry names, or is a link; how many went.
fn verify_manifest(root: &Utf8Path, dir: &Utf8Path, only: Option<&str>) -> Result<usize> {
    use std::io::Write as _;
    let path = dir.join("tree.index");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(0) };
    let tree = dir.join("tree");
    let entries: Vec<(&str, &str)> = text.lines().filter_map(|line| line.split_once(' ')).collect();
    let checked: Vec<(&str, &str)> = entries
        .iter()
        .copied()
        .filter(|(_, rel)| only.is_none_or(|only| *rel == only))
        .filter(|(key, rel)| {
            !key.starts_with("120000:")
                && std::fs::symlink_metadata(tree.join(rel)).is_ok_and(|m| m.is_file())
        })
        .collect();
    let hashed: HashMap<&str, String> = if checked.is_empty() {
        HashMap::new()
    } else {
        let mut child = std::process::Command::new("git")
            .args(["hash-object", "--no-filters", "--stdin-paths"])
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .context("spawn git hash-object")?;
        let paths = checked.iter().fold(String::new(), |mut s, (_, rel)| {
            s.push_str(tree.join(rel).as_str());
            s.push('\n');
            s
        });
        let mut stdin = child.stdin.take().context("git hash-object stdin")?;
        let feeder = std::thread::spawn(move || stdin.write_all(paths.as_bytes()));
        let out = child.wait_with_output().context("git hash-object")?;
        feeder
            .join()
            .map_err(|_panicked| anyhow::anyhow!("git hash-object's feeder panicked"))??;
        ensure!(out.status.success(), "git hash-object over {tree}: {}", out.status);
        let blobs = String::from_utf8(out.stdout).context("git hash-object's output")?;
        checked.iter().map(|(_, rel)| *rel).zip(blobs.lines().map(str::to_owned)).collect()
    };
    let kept: Vec<(&str, &str)> = entries
        .iter()
        .copied()
        .filter(|(key, rel)| {
            only.is_some_and(|only| *rel != only)
                || hashed
                    .get(rel)
                    .is_some_and(|blob| key.split_once(':').is_some_and(|(_, sha)| sha == *blob))
        })
        .collect();
    let dropped = entries.len().saturating_sub(kept.len());
    if dropped > 0 {
        let manifest = kept.iter().fold(String::new(), |mut s, (key, rel)| {
            s.push_str(key);
            s.push(' ');
            s.push_str(rel);
            s.push('\n');
            s
        });
        let fresh = dir.join("tree.index.new");
        std::fs::write(&fresh, manifest).with_context(|| format!("write {fresh}"))?;
        std::fs::rename(&fresh, &path).with_context(|| format!("replace {path}"))?;
    }
    Ok(dropped)
}

/// The tests of `crates` built and not run, as nextest builds them (`cargo test --no-run` with
/// the same packages and triple), the binaries they spawn first.
fn build_tests(sh: &Shell, crates: &[&str]) -> Result<()> {
    let target = crate::gate::mac_target();
    if let Some(spawned) = crate::gate::spawned_selection(Some(crates)) {
        let bins = crate::gate::spawned_bin_args();
        crate::tools::quiet_step(
            "spawned binaries",
            cmd!(sh, "cargo build --profile test {target...} {spawned...} {bins...}"),
        )?;
    }
    let built: Vec<String> = crates
        .iter()
        .chain(&[crate::tools::WORKSPACE_HACK])
        .flat_map(|name| ["-p".to_owned(), (*name).to_owned()])
        .collect();
    crate::tools::quiet_step("tests built", cmd!(sh, "cargo test --no-run {built...} {target...}"))
}

/// Before a build, under the volume's floor and its headroom: the idle lane directories go whole
/// first, which frees exactly what they hold, then a prune pass; still under the floor itself,
/// the check is refused, as the gate's is (`prune::ensure_room`), since a lane's cold app check
/// writes about 30 GB. With the floor at 50 the pass ran before most checks, about 5 s of each,
/// and evicted units idle for an hour, so 17-23 % of steps recompiled dependencies
/// (`.research/check-speed-2026-10-31/A-anatomy.md` §3.2); at 30 and its headroom it runs only
/// when the volume is short. The pass after a check runs once its verdict is out (`remote`'s
/// after-pass).
fn room_for_build(root: &Utf8Path) -> Result<()> {
    let limits = crate::prune::Limits::from_env()?;
    let roomy = limits.with_headroom()?.floor;
    if crate::prune::free_space(root)? >= roomy {
        return Ok(());
    }
    evict_idle(root, EVICTED_SHORT);
    if crate::prune::free_space(root)? < roomy {
        crate::prune::auto();
    }
    let free = crate::prune::free_space(root)?;
    ensure!(
        free >= limits.floor,
        "{} GB free, under the volume's floor of {} GB with the idle lane directories evicted and \
         a prune pass run: a build here could fill it (`cargo xtask prune --dry-run` says where \
         the space went)",
        free / 1_000_000_000,
        limits.floor / 1_000_000_000
    );
    Ok(())
}

/// What a lane directory's checks left for the next ones, in `<dir>/state.json`: the crates
/// checked there lately, newest first, and whether the last check moved the lock.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct State {
    /// The crates its checks named, newest first, at most [`State::KEPT`].
    pub packages: Vec<String>,
    /// Whether its last check's commit moved the lock: its directory holds that lock's builds,
    /// which a warm at `HEAD`'s would undo.
    pub moved_lock: bool,
    /// The commit its last check checked.
    pub sha: String,
}

impl State {
    /// The crates a state keeps.
    const KEPT: usize = 24;

    /// `dir`'s state, or none.
    pub fn read(dir: &Utf8Path) -> Self {
        std::fs::read_to_string(dir.join("state.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The state once a check of commit `sha`, which moved the lock or not, took the directory:
    /// said at once, so a new `-lock` clone never carries its source's word on the lock it holds
    /// ([`lock_dirs`]), even when its check stops before building.
    #[must_use]
    pub fn at(mut self, sha: &str, moved_lock: bool) -> Self {
        self.moved_lock = moved_lock;
        sha.clone_into(&mut self.sha);
        self
    }

    /// The state after a check of `crates` at commit `sha` that moved the lock or not.
    fn after(mut self, crates: &[String], sha: &str, moved_lock: bool) -> Self {
        self.packages.retain(|p| !crates.contains(p));
        self.packages.splice(0..0, crates.iter().cloned());
        self.packages.truncate(Self::KEPT);
        self.at(sha, moved_lock)
    }

    /// Written to `dir` whole or not at all.
    pub fn write(&self, dir: &Utf8Path) {
        let fresh = dir.join("state.json.new");
        if let Ok(text) = serde_json::to_string(self)
            && std::fs::write(&fresh, text).is_ok()
        {
            let _replaced = std::fs::rename(&fresh, dir.join("state.json"));
        }
    }
}

/// The lane directories a warm should bring to `HEAD`'s lock `head_lock` (the bytes of its
/// `Cargo.lock`), each with the crates to warm: used within [`WARM_WITHIN`], built at another
/// lock, and not a lock-moving check's (`<name>-lock`), which holds the lock its check moved.
pub fn warm_due(root: &Utf8Path, head_lock: &[u8]) -> Vec<(String, Vec<String>)> {
    let now = SystemTime::now();
    lane_dirs(root)
        .into_iter()
        .filter(|(name, _)| !name.ends_with(LOCK_SUFFIX))
        .filter(|(_, dir)| {
            last_used(dir)
                .is_some_and(|at| now.duration_since(at).unwrap_or_default() < WARM_WITHIN)
        })
        .filter(|(_, dir)| std::fs::read(dir.join("tree/Cargo.lock")).is_ok_and(|l| l != head_lock))
        .filter_map(|(name, dir)| {
            let state = State::read(&dir);
            (!state.moved_lock && !state.packages.is_empty()).then_some((name, state.packages))
        })
        .collect()
}

/// How recently a directory was used for a warm to be worth its CPU: one a lane checked in
/// within a working session.
const WARM_WITHIN: Duration = Duration::from_hours(6);

/// What ends the name of the directory a lock-moving check works in (`remote`'s `Workdir`).
pub const LOCK_SUFFIX: &str = "-lock";

/// A lane directory held: one check works in it at a time, since each syncs the tree to its own
/// commit under whatever another is building there. Dropped, it lets the directory go, its lock
/// file emptied first, so no holder's word outlives its hold.
#[derive(Debug)]
pub struct Held {
    /// The directory's name under `target/local/`.
    name: String,
    /// The directory.
    dir: Utf8PathBuf,
    /// The lock file, locked.
    file: std::fs::File,
    /// How long it waited for another check to let the directory go, and for the directory a
    /// new one was cloned from.
    waited: Duration,
    /// When a check last used the directory, which a hold other than a check's leaves as it was.
    used: Option<SystemTime>,
    /// A warm's watch for a check that wants the directory ([`Held::stoppable`]): its end, and
    /// its thread.
    watch: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
}

impl Held {
    /// How long it waited for another check to let the directory go, and for the directory a
    /// new one was cloned from.
    pub const fn waited(&self) -> Duration {
        self.waited
    }

    /// Count `more` as waited too: the wait for a clone's source ([`seed_dir`]).
    pub const fn waited_too(&mut self, more: Duration) {
        self.waited = self.waited.saturating_add(more);
    }

    /// The directory's name under `target/local/`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Mark its holder a warm, which gives way to a check: once a check waits for the directory
    /// (its wish held on `<dir>/.wanted`, [`hold`]), `stop` runs, within [`WATCHED`]. Only once
    /// the warm's tree is synced: the sync trusts the tree's manifest for a file's content, so a
    /// warm stopped between writing files and writing the manifest left the next check compiling
    /// another commit's file under its own verdict (2026-10-31).
    pub fn stoppable(&mut self, stop: impl FnOnce() + Send + 'static) -> Result<()> {
        use std::io::{Seek as _, Write as _};
        self.file.set_len(0)?;
        self.file.rewind()?;
        write!(self.file, "{} {WARM}", std::process::id())?;
        if let Some(used) = self.used {
            self.file.set_modified(used)?;
        }
        let done = Arc::new(AtomicBool::new(false));
        let ended = Arc::clone(&done);
        let dir = self.dir.clone();
        let watcher = std::thread::spawn(move || watch(&dir, &ended, stop));
        self.watch = Some((done, watcher));
        Ok(())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Some((done, watcher)) = self.watch.take() {
            done.store(true, Ordering::Release);
            let _ended = watcher.join();
        }
        // Emptied before the lock goes: a holder's mark stays no longer than its hold, so a
        // check never reads a dead warm's pid (or one reused since) as its holder's.
        let _emptied = self.file.set_len(0);
        if let Some(used) = self.used {
            let _kept = self.file.set_modified(used);
        }
    }
}

/// How often a warm looks whether a check wants its directory.
const WATCHED: Duration = Duration::from_millis(250);

/// A warm's watch over lane directory `dir`: `stop` once a check wants it, unless `ended` first.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; a warm's watch")]
fn watch(dir: &Utf8Path, ended: &AtomicBool, stop: impl FnOnce()) {
    while !ended.load(Ordering::Acquire) {
        if wanted(dir) {
            stop();
            return;
        }
        std::thread::sleep(WATCHED);
    }
}

/// Stop this warm whole: the after-pass leads a process group of its own, its cargo and rustc in
/// it (`remote`'s after-pass). What its builds finished stays built; its tree was synced before
/// it became stoppable.
fn stop_the_warm() {
    println!("  a check wants this directory: the warm stops");
    let _stopped = rustix::process::kill_current_process_group(rustix::process::Signal::TERM);
}

/// Who takes a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    /// A check: it waits for another holder to let go, or with `no_wait` is refused at once
    /// unless the holder is a warm, and while it waits it keeps out every holder but a check and
    /// stops a warm, which gives way to any check.
    Check {
        /// Refuse rather than wait for a check, a clone or an eviction.
        no_wait: bool,
    },
    /// Anything else (a warm, a clone taken of the directory, its eviction): it takes a free
    /// directory no check waits for, or none, a holder a check waits for and never stops. A warm
    /// becomes stoppable only once its tree is synced ([`Held::stoppable`]).
    Free,
}

/// The lock on lane directory `name` (`target/local/<name>/.lock`), held until dropped, as `who`
/// takes it. The lock file holds its holder's pid, with `busy` for a hold that is not a check's
/// and `warm` for a warm that gives way. A check that waits says who holds the directory, and
/// holds `<dir>/.wanted` shared meanwhile: a warm that sees it stops, and no other hold takes
/// the directory before the check. Both are kernel locks, gone with their holder.
pub fn hold(root: &Utf8Path, name: &str, who: Who) -> Result<Held> {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    let dir = dir(root, name);
    let path = dir.join(".lock");
    let asked = Instant::now();
    let mut said = false;
    let mut wanting: Option<std::fs::File> = None;
    loop {
        std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
        let opened = std::fs::File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            // Evicted between the two: made again.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("open {path}")),
        };
        if !try_lock_settled(&file) {
            ensure!(matches!(who, Who::Check { .. }), "target/local/{name} is in use");
            let holder = std::fs::read_to_string(&path).unwrap_or_default();
            let mut words = holder.split_whitespace();
            let pid = words.next().unwrap_or("unknown");
            let what = match words.next() {
                Some(WARM) => "a warm, asked to stop",
                Some(_) => "a clone, an eviction or a warm's sync",
                None => "a check",
            };
            ensure!(
                what.starts_with("a warm") || who != (Who::Check { no_wait: true }),
                "target/local/{name} is in use by pid {pid} ({what}): wait for it (no \
                 --no-wait), or check under another name"
            );
            if wanting.is_none() {
                wanting = Some(want(&dir)?);
            }
            if !said {
                println!("  waiting for target/local/{name}: pid {pid} works there ({what})");
                said = true;
            }
            file.lock().with_context(|| format!("lock {path}"))?;
        }
        // The directory may have gone while this waited (an idle one evicted whole): the lock
        // held must be the one at the path.
        let same = std::fs::metadata(&path).is_ok_and(|now| {
            file.metadata().is_ok_and(|held| held.ino() == now.ino() && held.dev() == now.dev())
        });
        if !same {
            wanting = None;
            continue;
        }
        ensure!(
            matches!(who, Who::Check { .. }) || !wanted(&dir),
            "a check waits for target/local/{name}"
        );
        let used = file.metadata().and_then(|m| m.modified()).ok();
        let mut held = Held {
            name: name.to_owned(),
            dir,
            file,
            waited: asked.elapsed(),
            // Any hold but a check's leaves the directory's time as it was: it is when a check
            // last used it.
            used: if matches!(who, Who::Check { .. }) { None } else { used },
            watch: None,
        };
        held.file.set_len(0)?;
        match who {
            Who::Check { .. } => write!(held.file, "{}", std::process::id())?,
            Who::Free => write!(held.file, "{} {BUSY}", std::process::id())?,
        }
        if let Some(used) = held.used {
            held.file.set_modified(used)?;
        }
        drop(wanting);
        if said {
            println!("  took target/local/{name} after {}s", held.waited.as_secs());
        }
        return Ok(held);
    }
}

/// Mark a check's wish for lane directory `dir`: `<dir>/.wanted` held shared until dropped.
fn want(dir: &Utf8Path) -> Result<std::fs::File> {
    let path = dir.join(WANTED);
    let file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {path}"))?;
    file.lock_shared().with_context(|| format!("lock {path}"))?;
    Ok(file)
}

/// Whether a check waits for lane directory `dir`: one holds `<dir>/.wanted`.
fn wanted(dir: &Utf8Path) -> bool {
    std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(WANTED))
        .is_ok_and(|file| !try_lock_settled(&file))
}

/// Whether `file`'s lock was taken, tried for a few milliseconds: a lock let go stays held while
/// any process of the holder's is being spawned, whose new process holds every open file until
/// its program starts (a parallel test's `git`, a check's `gh` beside its build).
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; a lock's settle")]
fn try_lock_settled(file: &std::fs::File) -> bool {
    for _ in 0..10 {
        if file.try_lock().is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The word a warm's lock file holds beside its pid.
const WARM: &str = "warm";

/// The word the lock file of a hold that is neither a check's nor a warm's holds beside its pid.
const BUSY: &str = "busy";

/// The file a check that waits for a lane directory holds ([`hold`]).
const WANTED: &str = ".wanted";

/// When a check last took lane directory `dir`: its lock file's time, which each check rewrites
/// and a warm leaves.
fn last_used(dir: &Utf8Path) -> Option<SystemTime> {
    std::fs::metadata(dir.join(".lock")).and_then(|m| m.modified()).ok()
}

/// Every lane directory under `target/local/`, by name.
fn lane_dirs(root: &Utf8Path) -> Vec<(String, Utf8PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root.join("target").join("local")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .map(|name| {
            let dir = dir(root, &name);
            (name, dir)
        })
        .collect()
}

/// The parts of a lane directory a clone takes: the tree, the manifest its sync reads and the
/// mark of a sync stopped halfway, and the target dir. Not the submodules' checkouts: each is a
/// git worktree whose `.git` names the source's git dir, so the two lanes moved each other's
/// `HEAD` and index (lane X, 2026-10-31), and the sync makes the clone its own. Not the state:
/// the clone's check says its own ([`State::at`]).
const CLONED: [&str; 4] = ["tree", "tree.index", "tree.syncing", "target"];

/// Stands in a lane directory from before a clone into it until its tree's units are dropped
/// ([`seed_dir`]).
const SEEDING: &str = "seeding";

/// Give lane directory `name` a warm start where it has no target dir, or where a clone into it
/// stopped halfway: a clone of a warm lane directory (its tree and target dir), taken under that
/// directory's lock so it is a state between two of its checks; returns how long it waited for
/// one. The source is the one whose build keeps most of what the check's crates build on at its
/// lock `lock` (the bytes of its `Cargo.lock`) once its tree's units go ([`sources`]): one no
/// check holds that is as good, else the best once its check lets it go. APFS `clonefile` takes
/// a 20 GB target dir in under a second and no space until either side writes (2026-10-31).
///
/// The registry's and git's units stay warm: they are keyed by paths the same in every lane. The
/// tree's own go ([`forget_tree_units`]): built from the source's tree, they carry its absolute
/// paths, which cargo's fingerprints leave out, so xtask's tests in a clone of M read M's tree
/// (`env!("CARGO_MANIFEST_DIR")`, 2026-10-31), and one built after a tree was written looked fresh
/// against it (lane F's `slopty-platform` in lane P's build, 2026-10-30). A clone stopped before
/// they went left them in place for good, so a mark stands until they have, and the next check
/// clones again.
pub fn seed_dir(root: &Utf8Path, name: &str, lock: &[u8], crates: &[String]) -> Result<Duration> {
    let dir = dir(root, name);
    let seeding = dir.join(SEEDING);
    let halfway = seeding.exists();
    if dir.join("target").exists() && !halfway {
        return Ok(Duration::ZERO);
    }
    let asked = Instant::now();
    let found = loop {
        let sources = sources(root, name, lock, crates);
        let Some((best, first)) = sources.first() else { break None };
        // A source no check holds that is as good as the best, else the best once its check lets
        // it go, a few minutes at most: a clone of one mid-build could take a unit's new
        // fingerprint beside its old artifact, and a worse one costs a build of what the best
        // had (a GUI lane's clone of an xtask one built gpui again, 2026-10-31).
        let free = sources.iter().take_while(|(kept, _)| kept == best).find_map(|(_, source)| {
            hold(root, source, Who::Free).ok().map(|held| (source.clone(), held))
        });
        let (source, held) = match free {
            Some(free) => free,
            None => (first.clone(), hold(root, first, Who::Check { no_wait: false })?),
        };
        if self::dir(root, &source).join("target").is_dir() {
            break Some((source, held));
        }
        // Evicted while this waited: the hold made an empty directory in its place.
        drop(held);
        evict(root, &source, "evicted while a clone waited for it");
    };
    let waited = asked.elapsed();
    let Some((source, _held)) = found else {
        if halfway {
            for part in CLONED {
                clear(&dir.join(part))?;
            }
            std::fs::remove_file(&seeding).with_context(|| format!("remove {seeding}"))?;
        }
        println!("  target/local/{name} starts cold: no lane directory to clone");
        return Ok(waited);
    };
    let started = Instant::now();
    let from = self::dir(root, &source);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {dir}"))?;
    std::fs::write(&seeding, "").with_context(|| format!("write {seeding}"))?;
    for part in CLONED {
        let (from, to) = (from.join(part), dir.join(part));
        clear(&to)?;
        if std::fs::symlink_metadata(&from).is_ok() {
            clone_whole(&from, &to)?;
        }
    }
    let forgot = forget_tree_units(&dir.join("target"), &dir.join("tree"));
    std::fs::remove_file(&seeding).with_context(|| format!("remove {seeding}"))?;
    println!(
        "  target/local/{name} cloned from target/local/{source}{}, {forgot} units of its tree's \
         crates dropped ({:.1}s)",
        if halfway { " again, the last clone stopped halfway" } else { "" },
        started.elapsed().as_secs_f64()
    );
    Ok(waited)
}

/// Delete `path`, a directory whole or a file, if it is there.
fn clear(path: &Utf8Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(path).with_context(|| format!("clear {path}"))
        }
        Ok(_) => std::fs::remove_file(path).with_context(|| format!("clear {path}")),
        Err(_) => Ok(()),
    }
}

/// The directories `name` may be cloned from, best first ([`seed_dir`]), each with how many of
/// the registry's and git's packages `crates` build on at the lock `lock` ([`needed`]) its build
/// keeps once its tree's units go: those its own lock holds at the same version and source and
/// its target dir has units of. The one used last goes first among equals. A count of the
/// check's own crates built misled: those are the very units a clone drops, and a lock-moving
/// lane's directory that built more of them at another gpui ranked above one at the check's
/// lock.
fn sources(root: &Utf8Path, name: &str, lock: &[u8], crates: &[String]) -> Vec<(usize, String)> {
    let needed = needed(lock, crates);
    let mut found: Vec<(usize, Option<SystemTime>, String)> = lane_dirs(root)
        .into_iter()
        // A clone stopped halfway holds another tree's units still.
        .filter(|(other, dir)| {
            other != name && dir.join("target").is_dir() && !dir.join(SEEDING).exists()
        })
        .map(|(other, dir)| {
            let locked: HashSet<(String, String, String)> =
                std::fs::read_to_string(dir.join("tree/Cargo.lock"))
                    .map(|text| lock_packages(&text).into_iter().filter_map(Locked::key).collect())
                    .unwrap_or_default();
            let built = unit_names(&dir.join("target"));
            let kept =
                needed.iter().filter(|p| locked.contains(*p) && built.contains(&p.0)).count();
            (kept, last_used(&dir), other)
        })
        .collect();
    found.sort_by_key(|&(kept, used, _)| std::cmp::Reverse((kept, used)));
    found.into_iter().map(|(kept, _, other)| (kept, other)).collect()
}

/// A package a lock file holds.
struct Locked {
    name: String,
    version: String,
    /// Its registry or git repository; none for the workspace's own and path packages.
    source: Option<String>,
    /// Its dependencies, as the lock names them: `<name>`, or `<name> <version>` where several
    /// versions are locked.
    dependencies: Vec<String>,
}

impl Locked {
    /// Its name, version and source, when it has a source.
    fn key(self) -> Option<(String, String, String)> {
        Some((self.name, self.version, self.source?))
    }
}

/// The packages lock file `text` holds.
fn lock_packages(text: &str) -> Vec<Locked> {
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let field = |p: &toml::Table, key: &str| p.get(key)?.as_str().map(str::to_owned);
    table
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .filter_map(|p| {
            Some(Locked {
                name: field(p, "name")?,
                version: field(p, "version")?,
                source: field(p, "source"),
                dependencies: p
                    .get("dependencies")
                    .and_then(toml::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            })
        })
        .collect()
}

/// The registry's and git's packages, each by name, version and source, that `crates` build on
/// at the lock `lock` (the bytes of a `Cargo.lock`): every package their dependencies (dev and
/// build ones too) reach, less the workspace's and path packages.
fn needed(lock: &[u8], crates: &[String]) -> HashSet<(String, String, String)> {
    let packages = std::str::from_utf8(lock).map(lock_packages).unwrap_or_default();
    let mut by_name: HashMap<&str, Vec<&Locked>> = HashMap::new();
    for package in &packages {
        by_name.entry(&package.name).or_default().push(package);
    }
    let mut next: Vec<&Locked> =
        packages.iter().filter(|p| p.source.is_none() && crates.contains(&p.name)).collect();
    let mut seen: HashSet<(&str, &str, Option<&str>)> = HashSet::new();
    let mut needed = HashSet::new();
    while let Some(package) = next.pop() {
        if !seen.insert((&package.name, &package.version, package.source.as_deref())) {
            continue;
        }
        if let Some(source) = &package.source {
            needed.insert((package.name.clone(), package.version.clone(), source.clone()));
        }
        for dependency in &package.dependencies {
            let mut words = dependency.split(' ');
            let (Some(name), version) = (words.next(), words.next()) else { continue };
            let locked = by_name.get(name).into_iter().flatten();
            if let Some(found) = locked.into_iter().find(|p| version.is_none_or(|v| p.version == v))
            {
                next.push(found);
            }
        }
    }
    needed
}

/// The packages target dir `target` has units of, for any triple: each unit a `.fingerprint`
/// directory named `<package>-<16 hex digits>`.
fn unit_names(target: &Utf8Path) -> HashSet<String> {
    let mut profiles = vec![target.join("debug")];
    if let Ok(triples) = target.read_dir_utf8() {
        profiles.extend(triples.filter_map(Result::ok).map(|t| t.path().join("debug")));
    }
    profiles
        .iter()
        .filter_map(|profile| profile.join(".fingerprint").read_dir_utf8().ok())
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|unit| unit_package(unit.file_name()).map(str::to_owned))
        .collect()
}

/// The package a unit's `.fingerprint` directory, `<package>-<16 hex digits>`, is of.
fn unit_package(unit: &str) -> Option<&str> {
    let (name, hash) = unit.rsplit_once('-')?;
    (hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(name)
}

/// Clone `from` to `to`, which must not exist: a directory whole, each file sharing its blocks
/// with `from`'s until either side writes, its times kept.
#[cfg(target_os = "macos")]
fn clone_whole(from: &Utf8Path, to: &Utf8Path) -> Result<()> {
    let src = std::ffi::CString::new(from.as_str()).context("a path with a NUL")?;
    let dst = std::ffi::CString::new(to.as_str()).context("a path with a NUL")?;
    // SAFETY: `clonefile(2)` reads two NUL-terminated paths, which outlive the call.
    let status = unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) };
    ensure!(status == 0, "clone {from} to {to}: {}", std::io::Error::last_os_error());
    Ok(())
}

/// Copy `from` to `to`, which must not exist, with every file's time: off a Mac no lane
/// directory is cloned, and this keeps the code path the same.
#[cfg(not(target_os = "macos"))]
fn clone_whole(from: &Utf8Path, to: &Utf8Path) -> Result<()> {
    let copied = std::process::Command::new("cp").arg("-a").arg(from).arg(to).status()?;
    ensure!(copied.success(), "copy {from} to {to}: {copied}");
    Ok(())
}

/// How long a lane directory may go unused before it goes whole: a lane that has not checked
/// for a day is done, and a new one starts from a clone.
const EVICTED: Duration = Duration::from_hours(24);

/// How long a lane directory may go unused before it goes when the volume is under its floor and
/// headroom: a lane checks every few minutes while it works, so one idle this long is done or
/// waiting, and gives its space before a prune pass takes units idle for an hour from the lanes
/// that work.
pub const EVICTED_SHORT: Duration = Duration::from_hours(3);

/// Delete every lane directory unused for `idle` (and every one unused for [`EVICTED`]) that no
/// check holds, the one used last that has a target dir kept as the next clone's source, and say
/// how many went. A directory goes whole, which frees exactly the blocks it does not share with
/// a clone: a prune pass sizes units by their files and so counts a clone's shared blocks as
/// freed when they are not.
pub fn evict_idle(root: &Utf8Path, idle: Duration) -> usize {
    let now = SystemTime::now();
    let mut dirs: Vec<(Option<SystemTime>, String, Utf8PathBuf)> =
        lane_dirs(root).into_iter().map(|(name, dir)| (last_used(&dir), name, dir)).collect();
    dirs.sort_by_key(|&(used, ..)| std::cmp::Reverse(used));
    let kept = dirs.iter().position(|(_, _, dir)| dir.join("target").is_dir());
    // What an earlier pass moved aside and could not delete.
    for left in std::fs::read_dir(graveyard(root)).into_iter().flatten().filter_map(Result::ok) {
        if let Ok(left) = Utf8PathBuf::from_path_buf(left.path()) {
            delete(&left);
        }
    }
    let mut gone = 0_usize;
    for (index, (at, name, _)) in dirs.iter().enumerate() {
        let unused = at.map_or(Duration::MAX, |at| now.duration_since(at).unwrap_or_default());
        if Some(index) == kept || unused < idle.min(EVICTED) {
            continue;
        }
        if evict(root, name, &format!("unused for {}h", unused.as_secs() / 3600)) {
            gone = gone.saturating_add(1);
        }
    }
    gone
}

/// Delete lane directory `name` whole unless a check holds it, saying `why`; whether it went.
/// It is moved aside under its lock, then deleted: a check waiting for it finds it gone and
/// starts a new one at the path ([`hold`]).
pub fn evict(root: &Utf8Path, name: &str, why: &str) -> bool {
    let Ok(held) = hold(root, name, Who::Free) else { return false };
    let aside = graveyard(root).join(format!("{name}-{}", std::process::id()));
    let moved = std::fs::create_dir_all(graveyard(root))
        .and_then(|()| std::fs::rename(dir(root, name), &aside));
    drop(held);
    let gone = moved.is_ok() && delete(&aside);
    if gone {
        println!("  target/local/{name} went whole: {why}");
    }
    gone
}

/// Where evicted lane directories wait to be deleted.
fn graveyard(root: &Utf8Path) -> Utf8PathBuf {
    root.join("target").join("local").join(".evicted")
}

/// The lane directories whose last check moved the lock, each with that check's commit: once
/// `HEAD` holds what it moved, the lane's checks work in its shared directory again and this one
/// only takes space.
pub fn lock_dirs(root: &Utf8Path) -> Vec<(String, String)> {
    lane_dirs(root)
        .into_iter()
        .filter_map(|(name, dir)| {
            let state = State::read(&dir);
            (state.moved_lock && !state.sha.is_empty()).then_some((name, state.sha))
        })
        .collect()
}

/// Delete directory `path` and all it holds, write-protected entries too (a guest's extracted
/// archive), saying why when it cannot; whether it went.
fn delete(path: &Utf8Path) -> bool {
    if std::fs::remove_dir_all(path).is_ok() {
        return true;
    }
    let writable = std::process::Command::new("chmod").args(["-R", "u+w"]).arg(path).status();
    match writable.map_err(anyhow::Error::from).and_then(|_| Ok(std::fs::remove_dir_all(path)?)) {
        Ok(()) => true,
        Err(e) => {
            println!("  {path} not deleted: {e:#}");
            false
        }
    }
}

/// Give lane `dir` a warm `sub` target dir where it has none: a copy-on-write clone (APFS
/// `clonefile`, near instant and no space until either side writes) of the one another lane
/// used last, its tree's units dropped ([`forget_tree_units`]). The e2e guest's build dir only;
/// a lane directory's own is cloned with its tree ([`seed_dir`]).
pub fn seed(dir: &Utf8Path, sub: &str) {
    let target = dir.join(sub);
    if target.exists() {
        return;
    }
    let Some(lanes) = dir.parent() else { return };
    let Ok(entries) = std::fs::read_dir(lanes) else { return };
    let warmest = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(sub))
        .filter(|path| path.join("debug").exists() || has_triple(path))
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max_by_key(|(at, _)| *at);
    let Some((_, from)) = warmest else { return };
    let started = Instant::now();
    let cloned = std::process::Command::new("/bin/cp")
        .args(["-c", "-R"])
        .arg(&from)
        .arg(target.as_std_path())
        .status();
    match cloned {
        Ok(status) if status.success() => {
            let forgot = forget_tree_units(&target, &dir.join("tree"));
            println!(
                "  {sub} seeded from {} by a clone, {forgot} of its tree's units dropped ({:.1}s)",
                from.display(),
                started.elapsed().as_secs_f64()
            );
        }
        outcome => {
            println!("  {sub} not seeded from {} ({outcome:?}): built cold", from.display());
            let _gone = std::fs::remove_dir_all(target.as_std_path());
        }
    }
}

/// Drop from `target`, cloned from another lane, every unit of a package that is not the
/// registry's or git's in `tree`'s locks (the workspace's and the fuzz crate's), and say how many
/// went: the workspace's crates, the vendored ones, and any the other lane built under a name its
/// tree no longer has (a crate renamed since). The other lane built them from its own tree, and
/// cargo takes a unit newer than its sources as fresh: one built there after this lane's tree was
/// written stood in for this tree's (lane F's `slopty-platform`, with its unfinished edits, in
/// lane P's iOS build, 2026-10-30). The registry's and git's units, the same in every lane, stay
/// warm.
fn forget_tree_units(target: &Utf8Path, tree: &Utf8Path) -> usize {
    let locked: Vec<Locked> = ["Cargo.lock", "fuzz/Cargo.lock"]
        .iter()
        .filter_map(|lock| std::fs::read_to_string(tree.join(lock)).ok())
        .flat_map(|text| lock_packages(&text))
        .collect();
    let paths: HashSet<&str> =
        locked.iter().filter(|p| p.source.is_none()).map(|p| p.name.as_str()).collect();
    let kept: HashSet<&str> = locked
        .iter()
        .filter(|p| p.source.is_some() && !paths.contains(p.name.as_str()))
        .map(|p| p.name.as_str())
        .collect();
    // `<target>/<profile>/.fingerprint` and `<target>/<triple>/<profile>/.fingerprint`.
    let profiles =
        std::fs::read_dir(target).into_iter().flatten().filter_map(Result::ok).flat_map(|entry| {
            let path = entry.path();
            let nested = std::fs::read_dir(&path).into_iter().flatten().filter_map(Result::ok);
            std::iter::once(path).chain(nested.map(|inner| inner.path())).collect::<Vec<_>>()
        });
    profiles
        .filter_map(|profile| std::fs::read_dir(profile.join(".fingerprint")).ok())
        .flat_map(|units| units.filter_map(Result::ok))
        .filter(|unit| {
            unit.file_name()
                .to_str()
                .and_then(unit_package)
                .is_some_and(|package| !kept.contains(package))
        })
        .filter(|unit| std::fs::remove_dir_all(unit.path()).is_ok())
        .count()
}

/// Whether `target` holds a build for a target triple (`<triple>/debug`).
fn has_triple(target: &std::path::Path) -> bool {
    std::fs::read_dir(target).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| entry.path().join("debug").exists())
    })
}

/// Point cargo at `<dir>/target` through `<dir>/.cargo/config.toml`, which cargo finds above the
/// lane's tree, and never through `CARGO_TARGET_DIR`: sccache keys every unit by each `CARGO_*`
/// variable in rustc's environment, so a variable per lane kept every lane from reading the
/// dependencies another had compiled (0.25 % Rust hits over 12,772 compiles, 2026-10-16;
/// `docs/decisions/tooling.md`, "One compile cache key space").
pub fn target_by_config(dir: &Utf8Path) -> Result<()> {
    let config = dir.join(".cargo");
    std::fs::create_dir_all(&config).with_context(|| format!("create {config}"))?;
    let path = config.join("config.toml");
    let text = "# Written by `xtask local`: the lane's target dir, kept out of rustc's environment.\n\
                [build]\ntarget-dir = \"target\"\n";
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
        std::fs::write(&path, text).with_context(|| format!("write {path}"))?;
    }
    Ok(())
}

/// Set in a local check's environment: `xtask check` then tests with the profile a busy Mac
/// takes (`land`), which leaves out the tests that time themselves against the machine's load.
pub const LOCAL: &str = "SLOPTY_LOCAL_CHECK";

/// Samples the Mac's thermal pressure and load while a check runs.
struct Watch {
    stop: Arc<AtomicBool>,
    sampler: std::thread::JoinHandle<(Option<u8>, f64)>,
}

impl Watch {
    #[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; sampling")]
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let sampler = std::thread::spawn(move || {
            let (mut thermal, mut load) = (None::<u8>, 0.0_f64);
            while !stopped.load(Ordering::Relaxed) {
                if let Some(level) = thermal_level() {
                    thermal = Some(thermal.map_or(level, |t| t.max(level)));
                }
                load = load.max(load_average().unwrap_or(0.0));
                std::thread::sleep(Duration::from_secs(2));
            }
            (thermal, load)
        });
        Self { stop, sampler }
    }

    fn finish(self) -> (Option<u8>, f64) {
        self.stop.store(true, Ordering::Relaxed);
        self.sampler.join().unwrap_or((None, 0.0))
    }
}

/// The system's thermal pressure level, as `ProcessInfo.thermalState` reads it: 0 nominal,
/// 1 moderate, 2 heavy, 3 trapping, 4 sleeping (`OSThermalNotification.h`).
pub fn thermal_level() -> Option<u8> {
    let sh = Shell::new().ok()?;
    let out =
        cmd!(sh, "notifyutil -g com.apple.system.thermalpressurelevel").quiet().read().ok()?;
    out.split_whitespace().last()?.parse().ok()
}

/// The one-minute load average.
pub fn load_average() -> Option<f64> {
    let sh = Shell::new().ok()?;
    let out = cmd!(sh, "sysctl -n vm.loadavg").quiet().read().ok()?;
    out.trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-local-{name}-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp dir");
        Utf8PathBuf::from_path_buf(dir).expect("a UTF-8 temp dir")
    }

    /// A lock file: the workspace's `slopty-shell` builds on gpui at revision `gpui`, and
    /// through it `metal`, and on `serde`; `xtask` on `serde` alone.
    fn lock(gpui: &str) -> String {
        format!(
            r#"version = 4

[[package]]
name = "gpui"
version = "0.2.0"
source = "git+https://github.com/aislopware/gpui-fast#{gpui}"
dependencies = ["metal", "serde 1.0.0"]

[[package]]
name = "metal"
version = "0.30.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "0.9.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "slopty-shell"
version = "0.1.0"
dependencies = ["gpui", "serde 1.0.0"]

[[package]]
name = "xtask"
version = "0.1.0"
dependencies = ["serde 1.0.0"]
"#
        )
    }

    /// The units of every package but the registry's and git's in the tree's locks go, in every
    /// profile and triple: the workspace's, the fuzz crate's and a renamed crate's old name; the
    /// registry's stay, and so does a file that is not a unit.
    #[test]
    fn a_clone_forgets_the_trees_units_and_keeps_the_dependencies() {
        let root = scratch("seed-forget");
        let tree = root.join("tree");
        std::fs::create_dir_all(tree.join("fuzz")).expect("a tree");
        std::fs::write(tree.join("Cargo.lock"), lock("1")).expect("a lock");
        let fuzz = "[[package]]\nname = \"libfuzzer-sys\"\nversion = \"0.4.0\"\nsource = \
                    \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\n\
                    name = \"slopty-fuzz\"\nversion = \"0.1.0\"\n";
        std::fs::write(tree.join("fuzz/Cargo.lock"), fuzz).expect("the fuzz crate's lock");
        let target = root.join("target");
        let host = target.join("debug/.fingerprint");
        let sim = target.join("aarch64-apple-ios-sim/debug/.fingerprint");
        let units = [
            (host.join("slopty-shell-0a1b2c3d4e5f6a7b"), false),
            (host.join("xtask-00ff00ff00ff00ff"), false),
            (host.join("slopty-fuzz-1234abcd1234abcd"), false),
            (host.join("slopty-old-name-1234abcd1234abcd"), false),
            (sim.join("slopty-shell-a7b6c5d4e3f2a1b0"), false),
            (host.join("serde-0a1b2c3d4e5f6a7b"), true),
            (sim.join("gpui-a7b6c5d4e3f2a1b0"), true),
            (host.join("libfuzzer-sys-a7b6c5d4e3f2a1b0"), true),
        ];
        for (unit, _) in &units {
            std::fs::create_dir_all(unit).expect("a unit dir");
        }
        std::fs::write(host.join("slopty-shell-notes"), "").expect("a stray file");
        assert_eq!(forget_tree_units(&target, &tree), 5);
        for (unit, stays) in units {
            assert_eq!(unit.exists(), stays, "{unit}");
        }
        assert!(host.join("slopty-shell-notes").exists());
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// What a check's crates build on is every registry and git package they reach, at the
    /// version the lock names.
    #[test]
    fn a_check_needs_what_its_crates_reach_at_their_lock() {
        let key = |name: &str, version: &str, source: &str| {
            (name.to_owned(), version.to_owned(), source.to_owned())
        };
        let registry = "registry+https://github.com/rust-lang/crates.io-index";
        let xtask = needed(lock("1").as_bytes(), &["xtask".to_owned()]);
        assert_eq!(xtask, HashSet::from([key("serde", "1.0.0", registry)]));
        let shell = needed(lock("1").as_bytes(), &["slopty-shell".to_owned()]);
        assert_eq!(shell.len(), 3, "{shell:?}");
        assert!(shell.contains(&key(
            "gpui",
            "0.2.0",
            "git+https://github.com/aislopware/gpui-fast#1"
        )));
    }

    /// A lane directory with a tree at the lock of gpui revision `gpui` and a target dir with
    /// units of `built`, last used `ago`, whose checks named the workspace's crates of `built`.
    fn lane(root: &Utf8Path, name: &str, gpui: &str, built: &[&str], ago: Duration) {
        let dir = dir(root, name);
        std::fs::create_dir_all(dir.join("tree")).expect("a tree");
        std::fs::create_dir_all(dir.join("target/debug")).expect("a target dir");
        std::fs::write(dir.join("tree/Cargo.lock"), lock(gpui)).expect("a lock");
        std::fs::write(dir.join("target/debug/unit"), name).expect("a unit");
        for package in built {
            let unit = dir.join(format!(
                "target/aarch64-apple-darwin/debug/.fingerprint/{package}-00112233aabbccdd"
            ));
            std::fs::create_dir_all(unit).expect("a built package");
        }
        let checked: Vec<String> = built
            .iter()
            .filter(|p| p.contains("slopty") || **p == "xtask")
            .map(|p| (*p).to_owned())
            .collect();
        State::default().after(&checked, "abc", false).write(&dir);
        drop(hold(root, name, Who::Free).expect("free"));
        let at = SystemTime::now().checked_sub(ago).expect("a time");
        std::fs::File::options()
            .write(true)
            .open(dir.join(".lock"))
            .and_then(|f| f.set_modified(at))
            .expect("aged");
    }

    /// A new directory is cloned from the one whose build keeps most of what its crates build on
    /// at its lock, files and their times kept and its tree's units dropped, the one used last
    /// among equals; a directory with a target dir is left as it is, unless a clone into it
    /// stopped halfway.
    #[test]
    fn a_new_directory_is_a_clone_of_the_closest_warm_one() {
        let root = scratch("seed-dir");
        let gui = ["gpui", "metal", "serde", "slopty-shell"];
        lane(&root, "L", "1", &gui, Duration::from_secs(60));
        lane(&root, "M", "2", &gui, Duration::from_secs(10));
        lane(&root, "N", "1", &["serde", "xtask"], Duration::from_secs(5));
        let fingerprints =
            |name: &str| dir(&root, name).join("target/aarch64-apple-darwin/debug/.fingerprint");
        let unit = dir(&root, "L").join("target/debug/unit");
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&unit)
            .and_then(|f| f.set_modified(old))
            .expect("aged");
        let crates = ["slopty-shell".to_owned()];
        assert_eq!(sources(&root, "L-a", lock("1").as_bytes(), &crates)[0], (3, "L".to_owned()));
        seed_dir(&root, "L-a", lock("1").as_bytes(), &crates).expect("seeded");
        let cloned = dir(&root, "L-a");
        assert_eq!(std::fs::read_to_string(cloned.join("target/debug/unit")).expect("unit"), "L");
        assert_eq!(
            std::fs::read_to_string(cloned.join("tree/Cargo.lock")).expect("a lock"),
            lock("1")
        );
        let at = std::fs::metadata(cloned.join("target/debug/unit")).and_then(|m| m.modified());
        assert_eq!(at.expect("a time"), old, "a file's time is kept");
        assert!(fingerprints("L-a").join("gpui-00112233aabbccdd").exists(), "a dependency stays");
        assert!(
            !fingerprints("L-a").join("slopty-shell-00112233aabbccdd").exists(),
            "the tree's go"
        );
        assert!(
            fingerprints("L").join("slopty-shell-00112233aabbccdd").exists(),
            "the source's stay"
        );
        assert!(!cloned.join("state.json").exists(), "the clone's check says its own");
        // At no directory's gpui: those that built what it builds on, the one used last first.
        seed_dir(&root, "M-b", lock("3").as_bytes(), &crates).expect("seeded");
        let unit = std::fs::read_to_string(dir(&root, "M-b").join("target/debug/unit"));
        assert_eq!(unit.expect("unit"), "M", "M built metal and was used after L; N did not");
        std::fs::write(cloned.join("target/debug/unit"), "mine").expect("written");
        seed_dir(&root, "L-a", lock("1").as_bytes(), &crates).expect("kept");
        let unit = std::fs::read_to_string(cloned.join("target/debug/unit")).expect("unit");
        assert_eq!(unit, "mine", "a directory with a target dir keeps it");
        // Stopped between the clone and the drop of its tree's units, it is cloned again.
        std::fs::write(cloned.join(SEEDING), "").expect("a clone stopped halfway");
        seed_dir(&root, "L-a", lock("1").as_bytes(), &crates).expect("seeded again");
        let unit = std::fs::read_to_string(cloned.join("target/debug/unit")).expect("unit");
        assert_eq!(unit, "L");
        assert!(!cloned.join(SEEDING).exists());
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// A clone waits for the best source while a check works there, rather than take one that
    /// built more of the check's own crates at another gpui.
    #[test]
    fn a_clone_waits_for_the_best_source() {
        let root = scratch("seed-wait");
        lane(&root, "L", "1", &["gpui", "metal", "serde", "slopty-shell"], Duration::from_secs(60));
        let more = ["gpui", "metal", "serde", "slopty-shell", "slopty-app", "xtask"];
        lane(&root, "M-h-lock", "2", &more, Duration::from_secs(5));
        let crates = ["slopty-shell".to_owned(), "slopty-app".to_owned()];
        let working = hold(&root, "L", Who::Check { no_wait: true }).expect("free");
        let (tx, rx) = std::sync::mpsc::channel();
        let seeding = {
            let root = root.clone();
            std::thread::spawn(move || {
                let seeded = seed_dir(&root, "L-u", lock("1").as_bytes(), &crates);
                let _sent = tx.send(seeded.is_ok());
            })
        };
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "it waits for L, at its gpui, not M-h-lock, which built more crates at another"
        );
        drop(working);
        assert!(rx.recv_timeout(Duration::from_secs(10)).expect("it goes on"));
        seeding.join().expect("joins");
        let unit = std::fs::read_to_string(dir(&root, "L-u").join("target/debug/unit"));
        assert_eq!(unit.expect("unit"), "L");
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// A check waits for another check, or is refused with `no_wait`; any other hold takes a
    /// directory no check holds or waits for; a warm gives way to a check that waits, once it is
    /// stoppable; another directory is never held up; and no holder's mark outlives its hold.
    #[test]
    fn a_directory_is_one_check_s_at_a_time_and_a_warm_gives_way() {
        let root = scratch("hold");
        let lock = dir(&root, "L").join(".lock");
        let first = hold(&root, "L", Who::Check { no_wait: true }).expect("free");
        let refused = hold(&root, "L", Who::Check { no_wait: true }).expect_err("held");
        let pid = std::process::id().to_string();
        assert!(refused.to_string().contains(&format!("pid {pid} (a check)")), "{refused}");
        assert!(hold(&root, "L", Who::Free).is_err(), "a warm takes no held directory");
        let other = hold(&root, "M", Who::Check { no_wait: true }).expect("another is free");
        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = {
            let root = root.clone();
            std::thread::spawn(move || {
                let held = hold(&root, "L", Who::Check { no_wait: false });
                let _sent = tx.send(held.is_ok());
                held
            })
        };
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "the second check waits while the first holds the directory"
        );
        assert!(wanted(&dir(&root, "L")), "and says so meanwhile");
        drop(first);
        assert!(rx.recv_timeout(Duration::from_secs(10)).expect("it goes on"));
        drop(waiting.join().expect("joins"));
        assert_eq!(std::fs::read_to_string(&lock).expect("the lock"), "", "its mark went with it");
        drop(other);
        // A check that waits keeps every other hold out.
        let wish = want(&dir(&root, "L")).expect("wanted");
        let refused = hold(&root, "L", Who::Free).expect_err("a check waits");
        assert!(refused.to_string().contains("a check waits"), "{refused}");
        drop(wish);
        // Any other hold (a warm while it syncs, a clone, an eviction) is a busy mark: a check
        // waits for it, or with `no_wait` is refused, and never stops it. A warm is stoppable only
        // once it says so, after its sync.
        let used = std::fs::metadata(&lock).and_then(|m| m.modified()).expect("a time");
        let mut warm = hold(&root, "L", Who::Free).expect("a warm takes a free directory");
        assert_eq!(std::fs::read_to_string(&lock).expect("the lock"), format!("{pid} {BUSY}"));
        assert!(hold(&root, "L", Who::Free).is_err(), "it takes no held directory");
        let refused = hold(&root, "L", Who::Check { no_wait: true }).expect_err("held");
        assert!(refused.to_string().contains(&format!("pid {pid} (a clone")), "{refused}");
        let (stop, stopped) = std::sync::mpsc::channel();
        warm.stoppable(move || {
            let _sent = stop.send(());
        })
        .expect("marked");
        assert_eq!(std::fs::read_to_string(&lock).expect("the lock"), format!("{pid} {WARM}"));
        assert!(stopped.recv_timeout(Duration::from_millis(600)).is_err(), "no check wants it");
        let (tx, rx) = std::sync::mpsc::channel();
        let checking = {
            let root = root.clone();
            std::thread::spawn(move || {
                let held = hold(&root, "L", Who::Check { no_wait: true });
                let _sent = tx.send(held.is_ok());
                held
            })
        };
        stopped.recv_timeout(Duration::from_secs(5)).expect("a check that waits stops the warm");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "until the warm lets go");
        drop(warm);
        assert!(rx.recv_timeout(Duration::from_secs(10)).expect("the check goes on"));
        let check = checking.join().expect("joins").expect("held");
        assert_eq!(std::fs::read_to_string(&lock).expect("the lock"), pid);
        drop(check);
        let kept = std::fs::metadata(&lock).and_then(|m| m.modified()).expect("a time");
        assert!(kept > used, "a check's hold is a use of the directory");
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// Idle directories go whole, the one used last with a target dir kept; a warm is due where
    /// a directory used lately holds another lock and is not a lock-moving check's.
    #[test]
    fn idle_directories_go_and_stale_ones_are_warmed() {
        let root = scratch("evict");
        lane(&root, "L", "old", &["slopty-shell"], Duration::from_hours(30));
        lane(&root, "M", "old", &["xtask"], Duration::from_hours(2));
        lane(&root, "N", "new", &["slopty-net"], Duration::from_hours(3));
        lane(&root, "O", "old", &["slopty-ui"], Duration::from_hours(9));
        lane(&root, "M-h-lock", "old", &["xtask"], Duration::from_hours(1));
        let mut moved = State::read(&dir(&root, "M-h-lock"));
        moved.moved_lock = true;
        moved.write(&dir(&root, "M-h-lock"));
        let head = lock("new");
        assert_eq!(warm_due(&root, head.as_bytes()), [("M".to_owned(), vec!["xtask".to_owned()])]);
        assert_eq!(lock_dirs(&root), [("M-h-lock".to_owned(), "abc".to_owned())]);
        assert_eq!(evict_idle(&root, Duration::from_hours(24)), 1, "L, a day idle");
        assert!(!dir(&root, "L").exists() && dir(&root, "O").exists());
        assert_eq!(
            evict_idle(&root, Duration::from_mins(150)),
            2,
            "N and O; M-h-lock, used last, stays"
        );
        assert!(dir(&root, "M").exists() && dir(&root, "M-h-lock").exists());
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// A sync stopped halfway (a warm stopped after writing a file and before its manifest)
    /// leaves a file the manifest does not name; the next sync finds its marker, verifies every
    /// file and writes again the one that is not its commit's, leaving the others' times alone.
    #[test]
    fn a_sync_stopped_halfway_is_verified_by_the_next() {
        let root = scratch("sync");
        let sh = Shell::new().expect("a shell");
        sh.change_dir(&root);
        std::fs::write(root.join("a.rs"), "// the lane's edit\n").expect("a file");
        std::fs::write(root.join("b.rs"), "// the same in both\n").expect("a file");
        cmd!(sh, "git init -q").run().expect("init");
        cmd!(sh, "git add a.rs b.rs").run().expect("add");
        cmd!(sh, "git -c user.name=t -c user.email=t@t commit -q -m c").run().expect("commit");
        let sha = cmd!(sh, "git rev-parse HEAD").read().expect("a commit");
        let lane = dir(&root, "L");
        let tree = sync_tree(&root, &lane, &sha).expect("synced");
        assert!(!lane.join("tree.syncing").exists(), "a finished sync leaves no marker");
        let old =
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(1_000_000)).expect("a time");
        std::fs::File::options()
            .write(true)
            .open(tree.join("b.rs"))
            .and_then(|f| f.set_modified(old))
            .expect("aged");
        // A warm at another commit wrote a.rs and was stopped before the manifest.
        std::fs::write(tree.join("a.rs"), "// another commit's\n").expect("written");
        std::fs::write(lane.join("tree.syncing"), "").expect("the marker it left");
        sync_tree(&root, &lane, &sha).expect("synced again");
        let a = std::fs::read_to_string(tree.join("a.rs")).expect("a.rs");
        assert_eq!(a, "// the lane's edit\n", "the check compiles its own commit's file");
        let b = std::fs::metadata(tree.join("b.rs")).and_then(|m| m.modified());
        assert_eq!(b.expect("a time"), old, "a file that is its commit's is not written again");
        // A check's cargo rewrote the lock in the tree: the next sync puts the commit's back.
        std::fs::write(root.join("Cargo.lock"), "# the commit's\n").expect("a lock");
        cmd!(sh, "git add Cargo.lock").run().expect("add");
        cmd!(sh, "git -c user.name=t -c user.email=t@t commit -q -m l").run().expect("commit");
        let sha = cmd!(sh, "git rev-parse HEAD").read().expect("a commit");
        sync_tree(&root, &lane, &sha).expect("synced");
        std::fs::write(tree.join("Cargo.lock"), "# cargo's\n").expect("rewritten");
        sync_tree(&root, &lane, &sha).expect("synced");
        let lock = std::fs::read_to_string(tree.join("Cargo.lock")).expect("the lock");
        assert_eq!(lock, "# the commit's\n");
        let _gone = std::fs::remove_dir_all(&root);
    }

    /// A state keeps the crates checked, newest first, and whether the last check moved the
    /// lock.
    #[test]
    fn a_state_keeps_the_newest_crates_first() {
        let owned = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let state = State::default().after(&owned(&["a", "b"]), "1", true).after(
            &owned(&["c", "a"]),
            "2",
            false,
        );
        assert_eq!(state.packages, ["c", "a", "b"]);
        assert!(!state.moved_lock && state.sha == "2");
    }
}
