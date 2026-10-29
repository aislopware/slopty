//! A check's crates checked on this Mac, at full power, beside the runners.
//!
//! `cargo xtask remote -p <crate>…` checks the commit it made of the tree (`HEAD` and the lane's
//! own paths). Its crates' fmt, host clippy, rustdoc, shear, typos and tests run here, where an
//! incremental build returns in a minute or two and no Mac queue stands in front of it; what
//! needs another platform (iOS and Linux clippy, Linux's tests, the e2e cases and probes)
//! still runs on a runner, side by side (`docs/decisions/tooling.md`, "Checks run where
//! they return soonest").
//!
//! Each lane checks in a directory of its own, `target/local/<lane>/`: the commit's tree, synced
//! as the gate syncs its snapshot so that unchanged files keep their times and cargo rebuilds only
//! what changed, and a target dir beside it. Lanes so check side by side and stay warm, and the
//! other lanes' unfinished edits in the checkout never reach a check.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
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

/// The directory lane `name`'s local checks use: its lane, as `remote` groups checks.
pub fn dir(root: &Utf8Path, lane: &str) -> Utf8PathBuf {
    root.join("target").join("local").join(lane)
}

/// Check `crates` at commit `sha` in lane `lane`'s directory: fmt, host clippy, rustdoc, shear
/// and typos, then the tests as a Mac runner runs them, the spawned binaries built first. Prints
/// each step as `xtask check` does.
pub fn check(root: &Utf8Path, lane: &str, sha: &str, crates: &[String]) -> Result<Outcome> {
    let started = Instant::now();
    // The volume keeps its floor through the build, and after it for the next.
    crate::prune::before_build();
    let dir = dir(root, lane);
    let tree = crate::gate::snapshot_commit(root, &dir, sha)?;
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    target_by_config(&dir)?;
    seed(&dir, "target");
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
    let lint = crate::check::run(&sh, crates, true, crate::check::Part::Lint);
    let tests = lint
        .as_ref()
        .ok()
        .map(|()| crate::check::run(&sh, crates, true, crate::check::Part::Tests));
    let (thermal, load) = watch.finish();
    crate::prune::auto();
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
    Ok(Outcome { passed, took: started.elapsed(), thermal, load, failed })
}

/// Give lane `dir` a warm `sub` target dir where it has none: a copy-on-write clone (APFS
/// `clonefile`, near instant and no space until either side writes) of the one another lane
/// used last. A new lane's cold dir rebuilt every dependency: sccache cannot keep a proc-macro
/// or a binary, so 729 units compiled again and a three-crate check took 8.5 min, 37 s of it
/// testing (2026-10-19). The dependencies' units are keyed by the registry's paths, the same in
/// every lane, so cargo keeps them and rebuilds only the workspace's own crates. A failed clone
/// leaves the lane cold, as before.
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
        Ok(status) if status.success() => println!(
            "  {sub} seeded from {} by a clone ({:.1}s)",
            from.display(),
            started.elapsed().as_secs_f64()
        ),
        outcome => {
            println!("  {sub} not seeded from {} ({outcome:?}): built cold", from.display());
            let _gone = std::fs::remove_dir_all(target.as_std_path());
        }
    }
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
