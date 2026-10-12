//! A check's crates checked on this Mac, at full power, beside the runners.
//!
//! `cargo xtask remote -p <crate>…` checks the commit it made of the tree (`HEAD` and the lane's
//! own paths). Its crates' fmt, host clippy, shear, typos and tests run here, where an
//! incremental build returns in a minute or two and no Mac queue stands in front of it; what
//! needs another platform (iOS and Linux clippy, rustdoc, Linux's tests, the e2e cases and
//! probes) still runs on a runner, side by side (`docs/decisions/tooling.md`, "Checks run where
//! they return soonest").
//!
//! Each lane checks in a directory of its own, `target/local/<lane>/`: the commit's tree, synced
//! as the gate syncs its snapshot so that unchanged files keep their times and cargo rebuilds only
//! what changed, and a target dir beside it. Lanes so check side by side and stay warm, and the
//! other lanes' unfinished edits in the checkout never reach a check.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
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
}

/// The directory lane `name`'s local checks use: its lane, as `remote` groups checks.
pub fn dir(root: &Utf8Path, lane: &str) -> Utf8PathBuf {
    root.join("target").join("local").join(lane)
}

/// Check `crates` at commit `sha` in lane `lane`'s directory: fmt, host clippy, shear and typos,
/// then the tests as a Mac runner runs them, the spawned binaries built first. Prints each step
/// as `xtask check` does.
pub fn check(root: &Utf8Path, lane: &str, sha: &str, crates: &[String]) -> Result<Outcome> {
    let started = Instant::now();
    let dir = dir(root, lane);
    let tree = crate::gate::snapshot_commit(root, &dir, sha)?;
    let sh = Shell::new()?;
    sh.change_dir(&tree);
    sh.set_var("CARGO_TARGET_DIR", dir.join("target"));
    // `.cargo/config.toml` names it relative to the tree: the checkout's is the one every build
    // shares.
    sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", root.join("target").join(crate::prune::PREBUILT));
    sh.set_var(LOCAL, "1");
    let watch = Watch::start();
    let lint = crate::check::run(&sh, crates, true, crate::check::Part::Lint);
    let tests = lint
        .as_ref()
        .ok()
        .map(|()| crate::check::run(&sh, crates, true, crate::check::Part::Tests));
    let (thermal, load) = watch.finish();
    if let Err(e) = &lint {
        println!("✘ local lint: {e:#}");
    }
    if let Some(Err(e)) = &tests {
        println!("✘ local tests: {e:#}");
    }
    let passed = lint.is_ok() && tests.is_some_and(|t| t.is_ok());
    Ok(Outcome { passed, took: started.elapsed(), thermal, load })
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
