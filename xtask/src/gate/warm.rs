//! `xtask gate --warm`: every lane of this host's platform in one job, on a warm runner's tree
//! (`crate::warm`; `docs/decisions/tooling.md`, "The gate runs on warm runners here").
//!
//! A warm runner keeps its checkout, toolchains, tools and one target dir between runs and
//! builds incrementally, so a run compiles what the push changed. Its lanes share that target
//! dir, and so take turns on cargo's lock: the tests, the longest leg, build first, then run from
//! nextest's record of that build ([`reused`]), holding no lock, while the lints build behind
//! them. On a Mac the tools and fmt run beside it all, then clippy for the host and both iOS
//! triples, rustdoc, and every Mac test; on Linux, clippy for the Linux triples and xtask, and
//! the Linux tests.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::sync::mpsc;
use std::time::Instant;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::LaneId;
use crate::tools::{LINUX_TRIPLES, SERVER_TRIPLE, TRIPLES, quiet_step, repo_root};

/// The nextest profile of a warm runner's tests (`.config/nextest.toml`).
pub const PROFILE: &str = "warm";

/// The disk the warm runners' volume is an image on (the Mac runner's `.env`): the person's,
/// into which the image grows as the runners write. A run stops before it writes while that disk
/// is under the floor (`SLOPTY_DISK_FLOOR_GB`), as `prune` keeps the volume's own above it.
pub const DISK_VAR: &str = "SLOPTY_WARM_DISK";

/// The lanes a warm runner of this host runs, as `xtask setup --lane` installs their tools.
const fn lanes() -> &'static [LaneId] {
    if cfg!(target_os = "macos") {
        &[LaneId::Tools, LaneId::ClippyHost, LaneId::ClippyIos, LaneId::Rustdoc, LaneId::Tests]
    } else {
        &[LaneId::ClippyLinux, LaneId::Linux]
    }
}

/// `xtask gate --warm`: in place, on the tree the runner checked out.
pub fn run() -> Result<()> {
    let started = Instant::now();
    let root = repo_root()?;
    let _lock = super::lock(&root.join("target").join("gate"))?;
    if let Some(disk) = std::env::var(DISK_VAR).ok().filter(|disk| !disk.is_empty()) {
        room_on(Utf8Path::new(&disk), crate::prune::Limits::from_env()?.floor)?;
    }
    crate::prune::ensure_room(&root.join("target"))?;
    prepare(&shell(&root)?, &root)?;
    let results = if cfg!(target_os = "macos") { mac(&root) } else { linux(&root) };
    let junit = root.join("target").join("nextest").join(PROFILE).join("junit.xml");
    super::report(&results, started, &junit)?;
    let names: Vec<&str> = results.iter().map(|(name, _)| *name).collect();
    println!("✔ gate passed warm on {} ({:.1?})", names.join(", "), started.elapsed());
    Ok(())
}

/// A shell on the tree for one of the lanes: the zig its ghostty names first on `PATH`, as the
/// local and gate shells have it. nextest's setup script runs this xtask (`SLOPTY_XTASK`), which
/// cargo would otherwise check again.
fn shell(root: &Utf8Path) -> Result<Shell> {
    let sh = Shell::new()?;
    sh.change_dir(root);
    if let Ok(exe) = std::env::current_exe() {
        sh.set_var("SLOPTY_XTASK", exe);
    }
    crate::tools::use_zig_for(&sh, root)?;
    Ok(sh)
}

/// The pinned toolchain with what the lanes need, unless there; the lanes' tools, once for each
/// version of `xtask setup` (a stamp in cargo's home), since a warm runner keeps them; and
/// ghostty's patches in its zig on every run, as a ghostty bump moves the zig the job installs
/// while the stamp stays.
fn prepare(sh: &Shell, root: &Utf8Path) -> Result<()> {
    let toolchain = match sh.var("RUSTUP_TOOLCHAIN") {
        Ok(pinned) if !pinned.is_empty() => pinned,
        _ => {
            let file = sh.read_file("rust-toolchain.toml")?;
            crate::toolchain::pinned(&file)
                .context("rust-toolchain.toml pins no channel")?
                .to_owned()
        }
    };
    let targets: Vec<&str> = if cfg!(target_os = "macos") {
        TRIPLES.to_vec()
    } else {
        LINUX_TRIPLES.iter().copied().chain([SERVER_TRIPLE]).collect()
    };
    let have = cmd!(sh, "rustup +{toolchain} target list --installed")
        .quiet()
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    let components = cmd!(sh, "rustup +{toolchain} component list --installed")
        .quiet()
        .ignore_stderr()
        .read()
        .unwrap_or_default();
    let complete = targets.iter().all(|t| have.lines().any(|l| l == *t))
        && ["clippy", "rustfmt"].iter().all(|c| components.lines().any(|l| l.starts_with(c)));
    if !complete {
        let targets = targets.join(",");
        crate::tools::step(
            &format!("Rust {toolchain}"),
            &cmd!(
                sh,
                "rustup toolchain install {toolchain} --profile minimal --component clippy,rustfmt --target {targets}"
            ),
        )?;
    }
    let home = sh.var("CARGO_HOME").map_or_else(
        |_| Utf8PathBuf::from(sh.var("HOME").unwrap_or_default()).join(".cargo"),
        Utf8PathBuf::from,
    );
    let stamp_file = home.join("slopty-warm-setup");
    let stamp = setup_stamp(&toolchain);
    if std::fs::read_to_string(&stamp_file).ok().as_deref() != Some(stamp.as_str()) {
        crate::setup::run(sh, false, lanes(), &[])?;
        std::fs::write(&stamp_file, &stamp).with_context(|| format!("write {stamp_file}"))?;
    }
    let ghostty = root.join("vendor/ghostty");
    if let Ok(zig) = crate::tools::zig_for(sh, &ghostty) {
        for file in crate::tools::patch_zig(sh, &ghostty, &zig)? {
            println!("  ✓ ghostty's patch put into {file}");
        }
    }
    Ok(())
}

/// What `xtask setup` installs for the warm lanes, as one line: its source, the lanes and the
/// toolchain. A change to any of them installs the tools again.
fn setup_stamp(toolchain: &str) -> String {
    let mut hasher = DefaultHasher::new();
    include_str!("../setup.rs").hash(&mut hasher);
    format!("{toolchain} {:?} {:016x}\n", lanes(), hasher.finish())
}

/// A lint lane: its name and what it runs.
type Lint = (&'static str, fn(&Shell) -> Result<()>);

/// Each lane's name and result.
type Results = Vec<(&'static str, Result<()>)>;

/// Whether `disk` has `floor` bytes free, said with what to do when it has not.
fn room_on(disk: &Utf8Path, floor: u64) -> Result<()> {
    let free = crate::prune::free_space(disk)?;
    let gb = |bytes: u64| bytes / 1_000_000_000;
    anyhow::ensure!(
        free >= floor,
        "the disk under the warm runners' volume, {disk}, has {} GB free, under the floor of {} \
         GB, and their image grows into it: free some of it, or send the gate to the hosted \
         runners (`gh variable set SLOPTY_RUNNERS --body hosted`)",
        gb(free),
        gb(floor)
    );
    Ok(())
}

/// Sends once when dropped: the tests are built (or their build failed) and the lints may take
/// cargo's lock.
struct Built(Option<mpsc::Sender<()>>);

impl Drop for Built {
    fn drop(&mut self) {
        if let Some(ready) = self.0.take() {
            let _sent: Result<(), mpsc::SendError<()>> = ready.send(());
        }
    }
}

/// Wait until the tests are built, then the lints of `lints`, each its own lane's result.
fn after_build(built: &mpsc::Receiver<()>, root: &Utf8Path, lints: &[Lint]) -> Results {
    let _ready: Result<(), mpsc::RecvError> = built.recv();
    lints.iter().map(|(name, lint)| (*name, shell(root).and_then(|sh| lint(&sh)))).collect()
}

/// The Mac's lanes.
fn mac(root: &Utf8Path) -> Results {
    std::thread::scope(|scope| {
        let (ready, built) = mpsc::channel();
        let fmt = scope.spawn(|| super::fmt(&shell(root)?, false));
        let tools = scope.spawn(|| super::tools_lane(|| shell(root), &shell(root)?, None));
        let lints: [Lint; 3] = [
            ("clippy host", super::lint_host),
            ("clippy ios", super::lint_ios),
            ("rustdoc", |sh| super::rustdoc_for(sh, false, Some(TRIPLES[0]))),
        ];
        let linted = scope.spawn(move || after_build(&built, root, &lints));
        let tests = scope.spawn(move || mac_tests(root, Built(Some(ready))));
        let mut results = vec![("fmt", super::join(fmt)), ("tools", super::join(tools))];
        results.extend(linted.join().unwrap_or_else(|_panic| {
            vec![("clippy", Err(anyhow::anyhow!("the lints' thread panicked")))]
        }));
        results.push(("tests", super::join(tests)));
        results
    })
}

/// The Linux lanes.
fn linux(root: &Utf8Path) -> Results {
    std::thread::scope(|scope| {
        let (ready, built) = mpsc::channel();
        let lints: [Lint; 1] = [("clippy linux", super::lint_linux)];
        let linted = scope.spawn(move || after_build(&built, root, &lints));
        let tests = scope.spawn(move || linux_tests(root, Built(Some(ready))));
        let mut results = vec![("linux", super::join(tests))];
        results.extend(linted.join().unwrap_or_else(|_panic| {
            vec![("clippy linux", Err(anyhow::anyhow!("the lints' thread panicked")))]
        }));
        results
    })
}

/// Every Mac test of the workspace but those the Linux runner runs alone
/// (`super::LINUX_ONLY_TESTS`), built with the binaries they spawn.
fn mac_tests(root: &Utf8Path, built: Built) -> Result<()> {
    let sh = shell(root)?;
    let target: Vec<String> = super::mac_target().iter().map(|&t| t.to_owned()).collect();
    let mut build = vec!["--workspace".to_owned()];
    build.extend(target.iter().cloned());
    let mut spawned = target;
    spawned.extend(super::spawned_selection(None).unwrap_or_default());
    spawned.extend(super::spawned_bin_args());
    let alone: Vec<String> = super::LINUX_ONLY_TESTS
        .iter()
        .filter(|(_, mac)| mac.is_empty())
        .map(|(name, _)| format!("package(={name})"))
        .collect();
    let all: Vec<&str> = super::LINUX_ONLY_TESTS.iter().map(|(name, _)| *name).collect();
    let mut filter = format!("!({})", alone.join(" | "));
    if let Some(binaries) = super::mac_binaries_filter(&all) {
        filter = format!("{filter} & {binaries}");
    }
    reused(&sh, root, &build, &spawned, &filter, built)
}

/// The Linux lane's tests (`super::linux_tests`), with the Linux worker, server and the
/// binaries the tests spawn.
fn linux_tests(root: &Utf8Path, built: Built) -> Result<()> {
    let sh = shell(root)?;
    let tested = super::linux_tested();
    let packages = super::selected(tested.iter().copied());
    let mut names = tested;
    for (_, package) in super::SPAWNED_BINS {
        if !names.contains(&package) {
            names.push(package);
        }
    }
    let mut spawned = super::selected(names);
    spawned.push("--examples".to_owned());
    spawned.extend(super::spawned_bin_args());
    reused(&sh, root, &packages, &spawned, "all()", built)
}

/// Build the tests `build` selects (packages, and the target on a Mac) and the binaries
/// `spawned` selects, record the build as
/// nextest's reuse files under `target/nextest/warm`, let the lints at cargo's lock (dropping
/// `built`), and run the tests `filter` keeps from that record, with no cargo call that could
/// wait on the lints' builds. The pseudo-terminals they hold are counted as in the gate's tests
/// lane (`crate::ptys`).
fn reused(
    sh: &Shell,
    root: &Utf8Path,
    build: &[String],
    spawned: &[String],
    filter: &str,
    built: Built,
) -> Result<()> {
    quiet_step("nextest build", cmd!(sh, "cargo nextest run {build...} --no-run"))?;
    quiet_step("spawned binaries", cmd!(sh, "cargo build --profile test {spawned...}"))?;
    let store = root.join("target").join("nextest").join(PROFILE);
    std::fs::create_dir_all(&store).with_context(|| format!("create {store}"))?;
    let binaries = store.join("binaries-metadata.json");
    let metadata = store.join("cargo-metadata.json");
    let listed =
        cmd!(sh, "cargo nextest list {build...} --list-type binaries-only --message-format json")
            .quiet()
            .read()
            .context("list the test binaries")?;
    std::fs::write(&binaries, listed).with_context(|| format!("write {binaries}"))?;
    let described = cmd!(sh, "cargo metadata --format-version 1 --no-deps")
        .quiet()
        .read()
        .context("cargo metadata")?;
    std::fs::write(&metadata, described).with_context(|| format!("write {metadata}"))?;
    drop(built);
    let _fresh = sh.push_env(super::BINS_FRESH, "1");
    let runner = crate::runner::command()?;
    let log = store.join("ptys.log");
    let ptys = crate::ptys::Sampler::start(std::process::id(), root, Some(log));
    let tests = super::nextest_step(
        PROFILE,
        cmd!(
            sh,
            "cargo nextest run --binaries-metadata {binaries} --cargo-metadata {metadata} --profile {PROFILE} --no-tests=warn -E {filter}"
        )
        .env(crate::runner::RUNNER_VAR, &runner),
    );
    let ptys = ptys.finish();
    print!("{}", ptys.report());
    super::both(tests, ptys.verdict())
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::{lanes, room_on, setup_stamp};
    use crate::gate::LaneId;

    /// A run goes on while the disk under the volume keeps the floor free, and stops, saying
    /// where and how to fall back, once it does not.
    #[test]
    fn a_run_stops_while_the_disk_under_the_volume_is_short() {
        room_on(Utf8Path::new("/"), 0).expect("a floor of nothing is met");
        let short = room_on(Utf8Path::new("/"), u64::MAX).expect_err("no disk has that much");
        let said = short.to_string();
        assert!(said.contains("the disk under the warm runners' volume, /,"), "{said}");
        assert!(said.contains("--body hosted"), "{said}");
    }

    /// A Mac's warm runner runs every lane but Linux's, a Linux one only the Linux lanes, and
    /// the tools are installed again when the toolchain or `xtask setup` changes.
    #[test]
    fn a_warm_runner_runs_its_platform_s_lanes() {
        let lanes = lanes();
        if cfg!(target_os = "macos") {
            for lane in [LaneId::Tools, LaneId::ClippyHost, LaneId::ClippyIos, LaneId::Rustdoc] {
                assert!(lanes.contains(&lane), "{lane:?}");
            }
            assert!(lanes.contains(&LaneId::Tests) && !lanes.contains(&LaneId::Linux));
        } else {
            assert_eq!(lanes, [LaneId::ClippyLinux, LaneId::Linux]);
        }
        assert_ne!(setup_stamp("1.99.1"), setup_stamp("1.100.0"));
        assert_eq!(setup_stamp("1.99.1"), setup_stamp("1.99.1"));
    }
}
