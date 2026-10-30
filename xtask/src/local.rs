//! A check's crates checked on this Mac beside the runners, at a priority below the e2e guests.
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
    // At `nice 10`, which every build and test it starts inherits: the e2e guests (8 of the 10
    // cores) and the person's own session come first, and a check takes what they leave. Beside
    // the lanes' builds at full priority a guest ran at loads of 136 to 321 on its 4 cores and a
    // worker missed its 30 s start (`docs/decisions/testing.md`, "Two e2e guests").
    rustix::process::setpriority_process(None, 10).context("lower the check's priority")?;
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
/// every lane, so cargo keeps them; the workspace's own crates are keyed by their paths within
/// the tree, also the same in every lane, so their units are dropped ([`forget_tree_units`]) and
/// rebuilt from this lane's tree. A failed clone leaves the lane cold, as before.
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

/// Drop from `target`, cloned from another lane, every unit of a package in `tree` (the
/// workspace's crates and the vendored ones), and say how many went. The other lane built them
/// from its own tree, and cargo takes a unit newer than its sources as fresh: one built there
/// after this lane's tree was written stood in for this tree's (lane F's `slopty-platform`, with
/// its unfinished edits, in lane P's iOS build, 2026-10-30). The registry's and git's units, the
/// same in every lane, stay warm.
fn forget_tree_units(target: &Utf8Path, tree: &Utf8Path) -> usize {
    let mut manifests = ["xtask", "workspace-hack", "fuzz"]
        .map(|member| tree.join(member).join("Cargo.toml"))
        .to_vec();
    for group in ["crates", "apps", "vendor"] {
        let Ok(entries) = std::fs::read_dir(tree.join(group)) else { continue };
        manifests.extend(
            entries.filter_map(Result::ok).filter_map(|entry| {
                Utf8PathBuf::from_path_buf(entry.path().join("Cargo.toml")).ok()
            }),
        );
    }
    let names: Vec<String> = manifests
        .iter()
        .filter_map(|manifest| std::fs::read_to_string(manifest).ok())
        .filter_map(|text| text.parse::<toml::Table>().ok())
        .filter_map(|table| Some(table.get("package")?.get("name")?.as_str()?.to_owned()))
        .collect();
    // `<target>/<profile>/.fingerprint` and `<target>/<triple>/<profile>/.fingerprint`.
    let profiles =
        std::fs::read_dir(target).into_iter().flatten().filter_map(Result::ok).flat_map(|entry| {
            let path = entry.path();
            let nested = std::fs::read_dir(&path).into_iter().flatten().filter_map(Result::ok);
            std::iter::once(path).chain(nested.map(|inner| inner.path())).collect::<Vec<_>>()
        });
    let ours = |name: &str| {
        name.rsplit_once('-').is_some_and(|(package, hash)| {
            names.iter().any(|n| n == package) && hash.chars().all(|c| c.is_ascii_hexdigit())
        })
    };
    profiles
        .filter_map(|profile| std::fs::read_dir(profile.join(".fingerprint")).ok())
        .flat_map(|units| units.filter_map(Result::ok))
        .filter(|unit| unit.file_name().to_str().is_some_and(ours))
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

    /// A unit of the tree's own crate goes, in every profile and triple; a dependency's, a name
    /// that only starts like one of the tree's, and a file that is not a unit stay.
    #[test]
    fn a_clone_forgets_the_trees_units_and_keeps_the_dependencies() {
        let dir = std::env::temp_dir().join(format!("xtask-seed-forget-{}", std::process::id()));
        let _fresh = std::fs::remove_dir_all(&dir);
        let root = Utf8PathBuf::from_path_buf(dir).expect("a UTF-8 temp dir");
        let tree = root.join("tree");
        for (manifest, name) in [
            ("crates/slopty-platform", "slopty-platform"),
            ("xtask", "xtask"),
            ("fuzz", "slopty-fuzz"),
        ] {
            std::fs::create_dir_all(tree.join(manifest)).expect("a crate dir");
            let text = format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
            std::fs::write(tree.join(manifest).join("Cargo.toml"), text).expect("a manifest");
        }
        let target = root.join("target");
        let host = target.join("debug/.fingerprint");
        let sim = target.join("aarch64-apple-ios-sim/debug/.fingerprint");
        let units = [
            (host.join("slopty-platform-0a1b2c3d4e5f6a7b"), false),
            (host.join("xtask-00ff00ff00ff00ff"), false),
            (host.join("slopty-fuzz-1234abcd1234abcd"), false),
            (sim.join("slopty-platform-a7b6c5d4e3f2a1b0"), false),
            (host.join("serde-0a1b2c3d4e5f6a7b"), true),
            (host.join("slopty-platform-macros-0a1b2c3d4e5f6a7b"), true),
            (sim.join("libc-a7b6c5d4e3f2a1b0"), true),
        ];
        for (unit, _) in &units {
            std::fs::create_dir_all(unit).expect("a unit dir");
        }
        std::fs::write(host.join("slopty-platform-notes"), "").expect("a stray file");
        assert_eq!(forget_tree_units(&target, &tree), 4);
        for (unit, stays) in units {
            assert_eq!(unit.exists(), stays, "{unit}");
        }
        assert!(host.join("slopty-platform-notes").exists());
        let _gone = std::fs::remove_dir_all(&root);
    }
}
