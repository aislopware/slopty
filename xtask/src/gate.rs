//! `xtask gate`: everything that must be green before a commit lands.
//!
//! Here, before a commit, the gate runs only the lane that compiles nothing ([`QUICK`]: the
//! tools, seconds) and prints the next step, so this Mac's cores stay on the work. Every lane
//! that compiles (host, iOS and Linux clippy, the tests, rustdoc) runs on GitHub Actions when
//! `cargo xtask land` pushes the commit to the `gate` branch, and `main` moves to it only once
//! every lane there passed (`.github/workflows/ci.yml`).
//! `--full` runs every lane here, as `cargo xtask release` and CI do. The `linux` lane, the
//! Linux worker's build and tests, runs only on a Linux host: CI's Linux runner.
//!
//! The gate checks a **snapshot of the index** (`target/gate/tree`, synced from the staged
//! blobs before every run), not the working tree: several agents edit this one checkout at
//! once, so the tree stays free to edit while the gate runs and what passed is exactly what
//! the next `git commit` records. Stage what you mean to land, then gate it. The cargo steps run as
//! parallel **lanes**, each on its own target dir under `target/gate/` (cargo serialises
//! concurrent builds that share one), so the wall time is the longest lane, not the sum.
//!
//! The tool checks (fmt, deny, hakari, shear, typos, taplo, committed) take seconds, so they run
//! first, side by side, and a failure among them stops the gate before a compile starts.
//!
//! A lane whose inputs are exactly those of its last pass is not run again ([`pass`]): the
//! index entries it reads, the toolchain, the environment and the tools. `--since-pass` goes
//! further for the tests: when only files inside packages changed since that pass, nextest runs
//! the tests of those packages and of every package that depends on them. Either way the lanes
//! run on the snapshot of the index, and a lane that is not run passed on the same inputs.

mod pass;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use pass::{Inputs, Plan, Scope};
use xshell::{Shell, cmd};

use crate::tools::{
    LINUX_CRATES, LINUX_UNTESTED, TRIPLES, WORKSPACE_HACK, has, host_only_present, quiet_step,
    repo_root, workspace_packages,
};

/// Gate options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Apply automatic fixes first.
    pub fix: bool,
    /// Check the working tree in place instead of a snapshot (CI, or a tree nobody edits).
    pub in_place: bool,
    /// Run only the tests of the packages changed since the tests lane last passed, and of
    /// their dependents.
    pub since_pass: bool,
    /// The message of the commit about to be made, which `committed` checks beside the history.
    pub message: Option<String>,
}

/// The lanes a gate runs here before a commit: the tools, which read text and compile nothing
/// (`docs/decisions/tooling.md`, "Nothing heavy runs here before a land").
pub const QUICK: [LaneId; 1] = [LaneId::Tools];

/// Where a gate given `--message` leaves it, for `git commit -F`.
const MESSAGE_FILE: &str = "target/gate/COMMIT_MSG";

/// The build lanes, each with its own target dir and a share of the cores. The host clippy
/// pass and the tests are the long ones; the rest fill the gaps.
const LANES: [(&str, u8); 6] = [
    ("tools", 1),
    ("clippy host", 4),
    ("clippy ios", 4),
    ("tests", 6),
    ("rustdoc", 3),
    ("linux", 6),
];

/// The nextest profile of the gate's tests lane (`.config/nextest.toml`).
const NEXTEST_PROFILE: &str = "gate";

/// The nextest profile of the tests lane on a hosted runner: `gate`'s, less the tests that read
/// hardware a runner's virtual Mac does not have.
const NEXTEST_CI_PROFILE: &str = "ci";

/// A lane as `--lane` names it. The tools lane carries the fmt check with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum LaneId {
    Tools,
    ClippyHost,
    ClippyIos,
    /// Clippy for Linux on the crates that build there. On CI a free Linux runner takes it;
    /// here it shares the iOS lane's target dir.
    ClippyLinux,
    Tests,
    Rustdoc,
    /// The Linux worker built natively and its crates' tests run, on a Linux host only.
    Linux,
}

impl LaneId {
    /// Its name in [`LANES`], the log and the pass records.
    const fn name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::ClippyHost => "clippy host",
            Self::ClippyIos => "clippy ios",
            Self::ClippyLinux => "clippy linux",
            Self::Tests => "tests",
            Self::Rustdoc => "rustdoc",
            Self::Linux => "linux",
        }
    }

    /// The crates of the tools it runs (`cargo xtask setup --lane`).
    #[must_use]
    pub const fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Tools => &[
                "cargo-deny",
                "cargo-hakari",
                "cargo-shear",
                "typos-cli",
                "taplo-cli",
                "committed",
            ],
            Self::Tests | Self::Linux => &["cargo-nextest"],
            // Linux clippy compiles each triple's C with zig (`tools::lint_linux`).
            Self::ClippyLinux => &["cargo-zigbuild"],
            Self::ClippyHost | Self::ClippyIos | Self::Rustdoc => &[],
        }
    }
}

/// A share of the tests lane, as `--shard` names it. The lane's build bounds every CI run and is
/// CPU-bound on a three-core runner, so CI runs it as one job per shard, each building and testing
/// its own packages (`docs/decisions/tooling.md`, "The tests lane runs in three shards").
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Shard {
    /// The client: the UI, the apps over it, and what only they use; and xtask.
    Ui,
    /// The worker daemon and what it stands on: sessions, capture, codecs, input, files.
    Worker,
    /// The rest: the server, the CLI, the wire, the client core, the terminal engine.
    Rest,
}

impl Shard {
    /// Its name in the lane's name and CI's matrix.
    const fn name(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Worker => "worker",
            Self::Rest => "rest",
        }
    }

    /// Its packages. Every workspace member is in exactly one shard
    /// (`tests::every_member_is_in_exactly_one_shard`), except [`WORKSPACE_HACK`], which each
    /// builds beside its own so the third-party crates resolve as in every other build. The split
    /// evens out the build's CPU per package (the tests lane's `cargo-timing.html`), since the
    /// tests themselves take a fraction of it.
    const fn packages(self) -> &'static [&'static str] {
        match self {
            Self::Ui => &[
                "slopty-ui",
                "slopty-app",
                "slopty",
                "slopty-e2e",
                "slopty-settings",
                "slopty-tools",
                "slopty-theme",
                "slopty-ios",
                // Here, not in the rest: its icon test waits on actool for minutes, and the rest
                // was the longer shard (.research/dev-speed-2026-10-05.md).
                "xtask",
            ],
            Self::Worker => &[
                "slopty-workerd",
                "slopty-worker",
                "slopty-ptyd",
                "slopty-pty",
                "slopty-capture",
                "slopty-codec",
                "slopty-media",
                "slopty-dnd",
                "slopty-input",
                "slopty-vdisplay",
                "slopty-files",
            ],
            Self::Rest => &[
                "slopty-server",
                "slopty-serverd",
                "slopty-push",
                "slopty-cli",
                "slopty-proto",
                "slopty-agent",
                "slopty-client",
                "slopty-net",
                "slopty-engine",
                "slopty-grid",
                "slopty-predict",
                "slopty-shape",
                "slopty-tailnet",
                "slopty-deploy",
                "slopty-crash",
                "slopty-core",
                "slopty-platform",
                "slopty-testkit",
            ],
        }
    }
}

/// Which lanes a gate runs, and where.
#[derive(Clone, Debug, Default)]
pub struct Only {
    /// The lanes to run; every lane when empty. CI runs each on a runner of its own.
    pub lanes: Vec<LaneId>,
    /// On a hosted runner: the tests lane uses [`NEXTEST_CI_PROFILE`].
    pub ci: bool,
    /// The tests lane on this shard's packages only.
    pub shard: Option<Shard>,
}

impl Only {
    /// The lanes run here before a commit ([`QUICK`]).
    pub fn quick() -> Self {
        Self { lanes: QUICK.to_vec(), ci: false, shard: None }
    }

    fn wants(&self, lane: LaneId) -> bool {
        // Every lane, when none is named, is every lane this host can run.
        let here = lane != LaneId::Linux || cfg!(target_os = "linux");
        (self.lanes.is_empty() && here) || self.lanes.contains(&lane)
    }
}

/// A shell in `tree` on lane `name`'s target dir. With `share` it runs beside other lanes and
/// gets its share of the cores; alone, cargo takes them all.
fn lane_shell(tree: &Utf8Path, gate_dir: &Utf8Path, name: &str, share: bool) -> Result<Shell> {
    let sh = Shell::new()?;
    sh.change_dir(tree);
    sh.set_var("CARGO_TARGET_DIR", gate_dir.join(name.replace(' ', "-")));
    // `.cargo/config.toml` names it relative to the tree, which here is the snapshot, whose
    // sync deletes what the index does not hold; the checkout's is the one every build shares.
    if let Some(target) = gate_dir.parent() {
        sh.set_var("LIBGHOSTTY_VT_SYS_PREBUILT_DIR", target.join(crate::prune::PREBUILT));
    }
    if share {
        let jobs = LANES.iter().find(|(n, _)| *n == name).map_or(4, |(_, j)| *j);
        sh.set_var("CARGO_BUILD_JOBS", jobs.to_string());
    }
    Ok(sh)
}

/// The gate on the lanes `only` names.
pub fn run_only(sh: &Shell, opts: &Options, only: &Only) -> Result<()> {
    let started = Instant::now();
    crate::upstream::warn_if_stale();
    let root = repo_root()?;
    let gate_dir = root.join("target").join("gate");
    let _lock = lock(&gate_dir)?;
    crate::prune::ensure_room(&root.join("target"))?;
    if opts.in_place && opts.since_pass {
        bail!("--since-pass compares snapshots of the index; it cannot check the tree in place");
    }
    if only.lanes.contains(&LaneId::Linux) && !cfg!(target_os = "linux") {
        bail!(
            "the linux lane runs on a Linux host (CI's); here `cargo xtask linux e2e` runs the \
             Linux worker in Docker"
        );
    }
    if only.shard.is_some() {
        ensure!(
            only.lanes == [LaneId::Tests] && !opts.since_pass,
            "--shard splits the tests lane: give it with `--lane tests` alone, without --since-pass"
        );
    }
    if opts.fix {
        // Fixers write to the working tree; the snapshot reads the index, so stage their edits.
        fmt(sh, true)?;
        hakari(sh, true)?;
        shear(sh, true)?;
        typos(sh, true)?;
    }
    let message = root.join(MESSAGE_FILE);
    match &opts.message {
        Some(text) => std::fs::write(&message, text)?,
        None => match std::fs::remove_file(&message) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        },
    }
    let message = opts.message.is_some().then_some(message.as_path());
    // In place, the tree is whatever is on disk, which no record describes: every lane runs.
    let (tree, inputs) = if opts.in_place {
        (root.clone(), None)
    } else {
        let (tree, listing) = snapshot(&root, Source::Index)?;
        let inputs = Inputs::gather(&gate_dir, &tree, listing)?;
        (tree, Some(inputs))
    };
    let inputs = inputs.as_ref();
    let share = only.lanes.len() != 1;
    let lane = |name: &str| lane_shell(&tree, &gate_dir, name, share);
    let checkout = || -> Result<Shell> {
        let sh = Shell::new()?;
        sh.change_dir(&root);
        Ok(sh)
    };

    // Seconds of tool checks, then minutes of compiles: a tool failure stops the gate first.
    let first: Vec<(&str, Result<()>)> = std::thread::scope(|scope| {
        let fmt_check = only.wants(LaneId::Tools).then(|| {
            scope.spawn(|| -> Result<()> {
                let sh = Shell::new()?;
                sh.change_dir(&tree);
                fmt(&sh, false)
            })
        });
        let tools = only.wants(LaneId::Tools).then(|| {
            scope.spawn(|| {
                let tools = Lane {
                    name: "tools",
                    scope: Scope::Tree,
                    extra: tools_extra(&checkout()?, opts.message.as_deref())?,
                    since_pass: false,
                };
                cached(inputs, &tree, &tools, |_| {
                    tools_lane(|| lane("tools"), &checkout()?, message)
                })
            })
        });
        let mut results = Vec::new();
        if let Some(fmt_check) = fmt_check {
            results.push(("fmt", join(fmt_check)));
        }
        if let Some(tools) = tools {
            results.push(("tools", join(tools)));
        }
        results
    });
    let profile = if only.ci { NEXTEST_CI_PROFILE } else { NEXTEST_PROFILE };
    // nextest keeps its reports under the workspace, not the target dir.
    let junit = tree.join("target").join("nextest").join(profile).join("junit.xml");
    report(&first, started, &junit)?;

    let build = |name| Lane { name, scope: Scope::Build, extra: String::new(), since_pass: false };
    // A shard builds other packages than the whole lane: a target dir and a pass record of its own.
    let tests_name: &str =
        &only.shard.map_or_else(|| "tests".to_owned(), |s| format!("tests {}", s.name()));
    let second: Vec<(&str, Result<()>)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        if only.wants(LaneId::ClippyHost) {
            handles.push((
                "clippy host",
                scope.spawn(|| {
                    cached(inputs, &tree, &build("clippy host"), |_| {
                        lint_host(&lane("clippy host")?)
                    })
                }),
            ));
        }
        if only.wants(LaneId::ClippyIos) {
            handles.push((
                "clippy ios",
                scope.spawn(|| {
                    cached(inputs, &tree, &build("clippy ios"), |_| lint_ios(&lane("clippy ios")?))
                }),
            ));
        }
        if only.wants(LaneId::ClippyLinux) {
            handles.push((
                "clippy linux",
                scope.spawn(|| {
                    // The iOS lane's target dir: cargo takes turns on it, and a full gate here
                    // keeps one tree of metadata for both instead of two.
                    cached(inputs, &tree, &build("clippy linux"), |_| {
                        lint_linux(&lane("clippy ios")?)
                    })
                }),
            ));
        }
        if only.wants(LaneId::Tests) {
            handles.push((
                tests_name,
                scope.spawn(|| {
                    let tests = Lane {
                        name: tests_name,
                        scope: Scope::Build,
                        extra: pass::tool_id("cargo-nextest"),
                        since_pass: opts.since_pass,
                    };
                    cached(inputs, &tree, &tests, |packages| {
                        let (sh, doc_sh) = (lane(tests_name)?, lane(tests_name)?);
                        test_lane(&sh, doc_sh, profile, packages, only.shard)
                    })
                }),
            ));
        }
        if only.wants(LaneId::Rustdoc) {
            handles.push((
                "rustdoc",
                scope.spawn(|| {
                    cached(inputs, &tree, &build("rustdoc"), |_| doc(&lane("rustdoc")?, false))
                }),
            ));
        }
        if only.wants(LaneId::Linux) {
            handles.push((
                "linux",
                scope.spawn(|| {
                    let linux = Lane {
                        name: "linux",
                        scope: Scope::Build,
                        extra: pass::tool_id("cargo-nextest"),
                        since_pass: false,
                    };
                    cached(inputs, &tree, &linux, |_| {
                        let (sh, doc_sh) = (lane("linux")?, lane("linux")?);
                        linux_lane(&sh, doc_sh, profile)
                    })
                }),
            ));
        }
        handles.into_iter().map(|(name, handle)| (name, join(handle))).collect()
    });
    report(&second, started, &junit)?;
    if only.lanes.is_empty() {
        println!("✔ gate passed ({:.1?})", started.elapsed());
    } else {
        let lanes: Vec<&str> = only.lanes.iter().map(|l| l.name()).collect();
        println!("✔ gate passed on {} ({:.1?})", lanes.join(", "), started.elapsed());
    }
    Ok(())
}

/// The gate's lock on `gate_dir`, held until the file it returns closes. Gates (and `land`'s
/// tests) share `target/gate/tree`: a second one would rewrite the snapshot under the first one's
/// build and both logs would lie.
fn lock(gate_dir: &Utf8Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(gate_dir)?;
    let lock = std::fs::File::create(gate_dir.join(".lock"))?;
    if lock.try_lock().is_err() {
        bail!("another `cargo gate` (or `land`'s tests) is running on this checkout; wait for it");
    }
    Ok(lock)
}

/// Where the tests lane keeps the main run's test report beside the VideoToolbox run's, which
/// writes `junit.xml` after it.
const MAIN_JUNIT: &str = "junit-main.xml";

/// The nextest profile of the tests `land` runs (`.config/nextest.toml`).
const NEXTEST_LAND_PROFILE: &str = "land";

/// Before `cargo xtask land` pushes: the checks a red CI run most often names, on the packages
/// its commits change since `base` and every package that depends on them, on HEAD's tree
/// (`target/gate/tree`), side by side under `nice`:
/// - their tests, in the tests lane's target dir;
/// - rustdoc with warnings denied, in the rustdoc lane's;
/// - clippy on both iOS triples and on Linux, in the iOS clippy lane's.
///
/// A red CI run costs the better part of an hour, and most were a test or a lint a few minutes
/// here would have failed (`docs/decisions/tooling.md`, "land checks the changed packages
/// first"). It is a net, not the gate: the tests that time themselves against the machine's load
/// are CI's alone (nextest's `land` profile), and a change outside every package (the manifests'
/// root, the lockfile, cargo's config) leaves all of it to CI, since it reaches every package.
/// Skipped when it last passed on the same inputs.
pub fn land_checks(base: &str) -> Result<()> {
    let started = Instant::now();
    let root = repo_root()?;
    let sh = Shell::new()?;
    sh.change_dir(&root);
    let diff = cmd!(sh, "git diff --name-only -z {base} HEAD").quiet().output()?;
    let changed: std::collections::BTreeSet<String> = diff
        .stdout
        .split(|b| *b == 0)
        .filter_map(|p| std::str::from_utf8(p).ok())
        .filter(|p| !p.is_empty() && !pass::inert(p))
        .map(str::to_owned)
        .collect();
    if changed.is_empty() {
        println!("  checks before the push: nothing a build reads changed since {base}");
        return Ok(());
    }
    let gate_dir = root.join("target").join("gate");
    let _lock = lock(&gate_dir)?;
    crate::prune::ensure_room(&root.join("target"))?;
    let (tree, listing) = snapshot(&root, Source::Head)?;
    let Some(packages) = pass::affected(&pass::workspace(&tree)?, &changed) else {
        let outside: Vec<&str> = changed.iter().take(3).map(String::as_str).collect();
        println!(
            "  checks before the push: left to CI, since a change reaches every package ({}…)",
            outside.join(", ")
        );
        return Ok(());
    };
    let inputs = Inputs::gather(&gate_dir, &tree, listing)?;
    let lane = Lane {
        name: "land",
        scope: Scope::Build,
        extra: pass::tool_id("cargo-nextest"),
        since_pass: false,
    };
    // Every cargo below inherits it: this Mac keeps working while the net runs.
    let pid = std::process::id().to_string();
    cmd!(sh, "renice -n 10 -p {pid}").quiet().ignore_stdout().run()?;
    cached(Some(&inputs), &tree, &lane, |_| {
        println!("▶ checks before the push: {}", packages.join(" "));
        let names: Vec<&str> = packages.iter().map(String::as_str).collect();
        let shell = |name| lane_shell(&tree, &gate_dir, name, false);
        std::thread::scope(|scope| {
            let tests = scope.spawn(|| affected_lane(&shell("tests")?, &packages));
            let docs = scope.spawn(|| crate::check::rustdoc(&shell("rustdoc")?, &names));
            let cross = scope.spawn(|| {
                let sh = shell("clippy ios")?;
                let linux = crate::tools::lint_linux(&sh, &names);
                let xtask = if names.contains(&"xtask") { lint_linux_xtask(&sh) } else { Ok(()) };
                both(both(crate::check::clippy_ios(&sh, &names), linux), xtask)
            });
            both(both(join(tests), join(docs)), join(cross))
        })
    })?;
    println!("✔ checks before the push ({:.1?})", started.elapsed());
    Ok(())
}

/// [`land_checks`]'s build and run of `packages`' tests.
fn affected_lane(sh: &Shell, packages: &[String]) -> Result<()> {
    let names: Vec<&str> = packages.iter().map(String::as_str).collect();
    let p = &selected(names.iter().copied().chain([WORKSPACE_HACK]));
    quiet_step("nextest build", cmd!(sh, "cargo nextest run {p...} --no-run"))?;
    if let Some(spawned) = spawned_selection(Some(&names)) {
        let bins = spawned_bin_args();
        quiet_step(
            "spawned binaries",
            cmd!(sh, "cargo build --profile test {spawned...} {bins...}"),
        )?;
    }
    let _fresh = sh.push_env(BINS_FRESH, "1");
    let runner = crate::runner::command()?;
    quiet_step(
        "nextest",
        cmd!(sh, "cargo nextest run {p...} --profile {NEXTEST_LAND_PROFILE}")
            .env(crate::runner::RUNNER_VAR, &runner),
    )
}

/// What to run once a gate on the index passed: commit what it checked (with the message it was
/// given, if any), then land it.
pub fn next_step(message: bool) -> String {
    let commit =
        if message { format!("git commit -F {MESSAGE_FILE}") } else { "git commit".to_owned() };
    format!(
        "next: {commit}\n      \
         (restage nothing: the index is what passed)\n\
         then: cargo xtask land\n      \
         (CI runs every lane on the `gate` branch, and main moves there once all pass)\n"
    )
}

fn join(handle: std::thread::ScopedJoinHandle<'_, Result<()>>) -> Result<()> {
    handle.join().unwrap_or_else(|panic| Err(anyhow::anyhow!("lane panicked: {panic:?}")))
}

/// Print every failed lane and fail if there was one. On GitHub Actions the run's summary names
/// each failed lane, its failed step and, for the tests, every test that failed (from `junit`).
fn report(results: &[(&str, Result<()>)], started: Instant, junit: &Utf8Path) -> Result<()> {
    let failed: Vec<(&str, String)> = results
        .iter()
        .filter_map(|(name, result)| {
            let e = format!("{:#}", result.as_ref().err()?);
            eprintln!("✘ {name}: {e}");
            Some((*name, e))
        })
        .collect();
    if failed.is_empty() {
        return Ok(());
    }
    if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") {
        let tests: Vec<String> = [junit.to_owned(), junit.with_file_name(MAIN_JUNIT)]
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .flat_map(|x| failed_tests(&x))
            .collect();
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(summary)?;
        std::io::Write::write_all(&mut file, summary_of(&failed, &tests).as_bytes())?;
    }
    let names: Vec<&str> = failed.iter().map(|(name, _)| *name).collect();
    bail!("gate failed: {} ({:.1?})", names.join(", "), started.elapsed());
}

/// The run summary's account of `failed` lanes, with the failed `tests` under the tests lane.
fn summary_of(failed: &[(&str, String)], tests: &[String]) -> String {
    let mut text = String::new();
    for (lane, error) in failed {
        let _written = writeln!(text, "### ✘ gate lane failed: {lane}\n\n{error}\n");
        if lane.starts_with("tests") {
            for test in tests {
                let _written = writeln!(text, "- `{test}`");
            }
            if !tests.is_empty() {
                text.push('\n');
            }
        }
    }
    text
}

/// The tests a nextest `JUnit` report says failed, as `binary test`. A test that failed and then
/// passed on a retry is flaky (`<flakyFailure>`), not failed.
fn failed_tests(junit: &str) -> Vec<String> {
    junit
        .split("<testcase ")
        .skip(1)
        .filter_map(|case| {
            let (head, body) = case.split_once('>')?;
            let body = if head.ends_with('/') {
                ""
            } else {
                body.split("</testcase>").next().unwrap_or_default()
            };
            let failed = body.contains("<failure") || body.contains("<error");
            failed.then(|| format!("{} {}", attribute(head, "classname"), attribute(head, "name")))
        })
        .collect()
}

/// The unescaped value of `key` in an element's attributes.
fn attribute(head: &str, key: &str) -> String {
    let value = format!(" {head}")
        .split_once(&format!(" {key}=\""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(value, _)| value.to_owned())
        .unwrap_or_default();
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A lane as the pass records see it.
struct Lane<'a> {
    name: &'a str,
    /// The index entries it reads.
    scope: Scope,
    /// What it reads besides them and the inputs common to every lane.
    extra: String,
    /// It may run on the packages changed since its last pass only.
    since_pass: bool,
}

/// Run `lane` through `check` unless its inputs are those of its last pass, and record them
/// when it passes. `check` gets the packages to narrow to under `--since-pass`, or `None` for
/// all of them.
fn cached(
    inputs: Option<&Inputs>,
    tree: &Utf8Path,
    lane: &Lane<'_>,
    check: impl FnOnce(Option<&[String]>) -> Result<()>,
) -> Result<()> {
    let Some(inputs) = inputs else { return check(None) };
    let name = lane.name;
    let key = inputs.key(name, lane.scope, &lane.extra);
    let plan = inputs.plan(name, &key, lane.since_pass, tree)?;
    let only = match &plan {
        Plan::Skip => {
            println!("  ✓ {name}: inputs unchanged since it last passed");
            return Ok(());
        }
        Plan::Only(packages) => {
            println!("  {name}: changed since its last pass: {}", packages.join(" "));
            Some(packages.as_slice())
        }
        Plan::Run => None,
    };
    check(only)?;
    inputs.record(name, &key)
}

/// What the tools lane reads besides the tree: the tools themselves, the history and the
/// pending `message` `committed` lints, and the day, so `cargo deny` sees new advisories at
/// least daily.
fn tools_extra(checkout: &Shell, message: Option<&str>) -> Result<String> {
    let mut extra: String = ["cargo-deny", "cargo-hakari", "cargo-shear", "typos", "committed"]
        .into_iter()
        .map(pass::tool_id)
        .collect();
    let head = cmd!(checkout, "git rev-parse HEAD").quiet().read()?;
    let tag = cmd!(checkout, "git describe --tags --abbrev=0").quiet().ignore_stderr().read();
    let day = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let _written = writeln!(extra, "head {head}\ntag {}\nday {day}", tag.unwrap_or_default());
    if let Some(message) = message {
        let _written = writeln!(extra, "message {message}");
    }
    Ok(extra)
}

/// deny, hakari, shear and typos on the snapshot (each in a shell from `on_tree`) and
/// `committed` on the checkout's history and the pending `message`, side by side; every failure
/// is reported.
fn tools_lane(
    on_tree: impl Fn() -> Result<Shell> + Sync,
    checkout: &Shell,
    message: Option<&Utf8Path>,
) -> Result<()> {
    let on_tree = &on_tree;
    let results: Vec<Result<()>> = std::thread::scope(|scope| {
        let handles = [
            scope.spawn(move || locked(&on_tree()?)),
            scope.spawn(move || deny(&on_tree()?)),
            scope.spawn(move || hakari(&on_tree()?, false)),
            scope.spawn(move || shear(&on_tree()?, false)),
            scope.spawn(move || typos(&on_tree()?, false)),
        ];
        // `committed` reads the history, which only the checkout has.
        let mut results = vec![commits(checkout, message)];
        results.extend(handles.into_iter().map(join));
        results
    });
    let errors: Vec<String> =
        results.into_iter().filter_map(Result::err).map(|e| format!("{e:#}")).collect();
    if errors.is_empty() { Ok(()) } else { bail!("{}", errors.join("; ")) }
}

/// `slopty_testkit::bins::FRESH`: set once every binary a test spawns is built.
pub const BINS_FRESH: &str = "SLOPTY_BINS_FRESH";

/// `slopty_testkit::bins::NAMES`, each with the package that builds it.
const SPAWNED_BINS: [(&str, &str); 8] = [
    ("slopty-ptyd", "slopty-ptyd"),
    ("slopty-worker", "slopty-workerd"),
    ("slopty-server", "slopty-serverd"),
    ("slopty", "slopty-cli"),
    ("slopty-stub-claude", "slopty-testkit"),
    ("slopty-stub-managed-claude", "slopty-testkit"),
    ("slopty-stub-pi", "slopty-testkit"),
    ("slopty-stub-acp", "slopty-testkit"),
];

/// `--bin <name>` for each of [`SPAWNED_BINS`].
fn spawned_bin_args() -> Vec<String> {
    SPAWNED_BINS.iter().flat_map(|(bin, _)| ["--bin".to_owned(), (*bin).to_owned()]).collect()
}

/// `-p <name>` for each name.
fn selected<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    names.into_iter().flat_map(|name| ["-p".to_owned(), name.to_owned()]).collect()
}

/// `slopty_testkit::bins::BUILT`: where nextest's setup script built them.
pub const BINS_BUILT: &str = "SLOPTY_BINS_BUILT";

/// `cargo xtask spawned-bins`, nextest's setup script (`.config/nextest.toml`): build every binary
/// a test spawns before the first test starts, as the test lane does, so a bare
/// `cargo nextest run` never builds them inside one test's timeout. The tests learn the directory
/// through `$NEXTEST_ENV`. Under `cargo xtask` they are built already, and this returns at once.
pub fn spawned_bins(sh: &Shell) -> Result<()> {
    if std::env::var_os(BINS_FRESH).is_some() {
        return Ok(());
    }
    let env =
        std::env::var_os("NEXTEST_ENV").context("nextest runs this and names $NEXTEST_ENV")?;
    let bins = spawned_bin_args();
    cmd!(sh, "cargo build --profile test --workspace --tests {bins...}").run()?;
    let built = crate::tools::target_dir(sh)?.join("debug");
    std::fs::write(&env, format!("{BINS_BUILT}={built}\n"))
        .with_context(|| format!("write {}", std::path::Path::new(&env).display()))?;
    Ok(())
}

/// The gate's tests: build every test binary (of the workspace, or of `shard`'s packages), then
/// run nextest's `profile` (on `only`'s packages when given) and the doctests side by side. Cargo
/// holds the target dir's lock only while it builds, and the build is done, so neither waits for
/// the other. Meanwhile [`crate::ptys`] counts the pseudo-terminals they hold: the lane fails on
/// a leak, on more than its budget at once, or on the system running out, naming the tests.
fn test_lane(
    sh: &Shell,
    doc_sh: Shell,
    profile: &str,
    only: Option<&[String]>,
    shard: Option<Shard>,
) -> Result<()> {
    let packages = shard.map_or_else(
        || vec!["--workspace".to_owned()],
        |s| selected(s.packages().iter().copied().chain([WORKSPACE_HACK])),
    );
    let p = &packages;
    // On a runner, `--timings` leaves `cargo-timing.html` in the target dir for CI to keep: what
    // each unit of the build cost. Here it would pile up a report per gate.
    let timings: &[&str] = if profile == NEXTEST_CI_PROFILE { &["--timings"] } else { &[] };
    quiet_step("nextest build", cmd!(sh, "cargo nextest run {p...} --no-run {timings...}"))?;
    // Every binary a test spawns, so no test shells out to cargo and waits on its locks while
    // another build on the machine holds them. With the tests selected that were built just now,
    // cargo resolves features with their dev-dependencies as that build did
    // (`slopty-tailnet/fake`, `slopty-codec/experiments`), so each crate is the unit the tests
    // already have. Without them a dozen workspace crates compiled a second time, differently.
    // In the tests' own profile too: plain `cargo build` is `dev`, which optimises the workspace
    // crates that `test` leaves at opt-level 0, and would compile all of them again.
    if let Some(spawned) = spawned_selection(shard.map(Shard::packages)) {
        let bins = spawned_bin_args();
        quiet_step(
            "spawned binaries",
            cmd!(sh, "cargo build --profile test {spawned...} {bins...}"),
        )?;
    }
    let _fresh = sh.push_env(BINS_FRESH, "1");
    // Test binaries run out of `run/`, not `deps/` (`crate::runner`).
    let runner = crate::runner::command()?;
    let only = only.map(|packages| {
        packages.iter().map(|p| format!("package(={p})")).collect::<Vec<_>>().join(" | ")
    });
    let apart = profile == NEXTEST_CI_PROFILE && cfg!(target_os = "macos");
    let rest = match (&only, apart) {
        (Some(only), true) => Some(format!("({only}) & !{VIDEOTOOLBOX}")),
        (Some(only), false) => Some(only.clone()),
        (None, true) => Some(format!("!{VIDEOTOOLBOX}")),
        (None, false) => None,
    };
    let filter: Vec<String> = rest
        .map_or_else(Vec::new, |rest| vec!["--no-tests=warn".to_owned(), "-E".to_owned(), rest]);
    let tree = Utf8PathBuf::from_path_buf(sh.current_dir())
        .map_err(|p| anyhow::anyhow!("the tree's path is not UTF-8: {}", p.display()))?;
    // Beside the JUnit report, which CI keeps.
    let log = tree.join("target").join("nextest").join(profile).join("ptys.log");
    let ptys = crate::ptys::Sampler::start(std::process::id(), &tree, Some(log));
    let libs = shard.map_or_else(|| Ok(packages.clone()), shard_libs)?;
    let (tests, doctests) = std::thread::scope(|scope| {
        let doctests =
            scope.spawn(move || quiet_step("doctests", cmd!(doc_sh, "cargo test {libs...} --doc")));
        let tests = nextest_step(
            profile,
            cmd!(sh, "cargo nextest run {p...} --profile {profile} {filter...}")
                .env(crate::runner::RUNNER_VAR, &runner),
        );
        (tests, join(doctests))
    });
    let videotoolbox = if apart {
        // nextest writes its report to one path a profile, so the second run's would replace the
        // first's, and the summary and CI's timings would lose every test but the encoder's.
        let report = tree.join("target").join("nextest").join(profile).join("junit.xml");
        match std::fs::rename(&report, report.with_file_name(MAIN_JUNIT)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let expr = only
            .map_or_else(|| VIDEOTOOLBOX.to_owned(), |only| format!("({only}) & {VIDEOTOOLBOX}"));
        videotoolbox_step(
            cmd!(sh, "cargo nextest run {p...} --profile {profile} --no-tests=pass -E {expr}")
                .env(crate::runner::RUNNER_VAR, &runner),
        )
    } else {
        Ok(())
    };
    let ptys = ptys.finish();
    print!("{}", ptys.report());
    both(both(both(tests, doctests), videotoolbox), ptys.verdict())
}

/// The nextest test group of the tests that code through VideoToolbox (`.config/nextest.toml`).
const VIDEOTOOLBOX: &str = "group(videotoolbox)";

/// How long the VideoToolbox tests may take on a runner. Green, the slowest shard's took under
/// two minutes.
const VIDEOTOOLBOX_DEADLINE: Duration = Duration::from_mins(10);

/// Run the VideoToolbox tests on their own, after the rest, and judge a failure by whether the
/// runner's encoder had stopped.
///
/// A hosted runner's virtual Mac shares its host's media engine. At times its encoder stops
/// ("No real codec", `docs/decisions/video.md`) with only a few dozen clients open (run
/// 37185543085: 46). Every test that codes then times out, and its killed process stays stuck in
/// exit inside the driver, where no signal ends it, so nextest waited on it until the job was
/// cancelled. Here a run past [`VIDEOTOOLBOX_DEADLINE`] is killed. A failure on a runner whose
/// encoder said it stopped says nothing of the change, so it is a warning. Any other failure
/// fails the lane. A change to the coding path runs these tests on a Mac's real encoder before it
/// lands (`docs/TESTING.md`, "VideoToolbox").
fn videotoolbox_step(command: xshell::Cmd<'_>) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    println!("▶ videotoolbox");
    let started = Instant::now();
    let mut command = std::process::Command::from(command);
    command.process_group(0);
    let mut child = command.spawn().context("videotoolbox: nextest failed to start")?;
    let group = child.id();
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(child.wait());
    });
    let passed = if let Ok(status) = waited.recv_timeout(VIDEOTOOLBOX_DEADLINE) {
        status.context("videotoolbox: waiting on nextest")?.success()
    } else {
        println!("  the run passed {VIDEOTOOLBOX_DEADLINE:?}: killing it");
        let _killed = std::process::Command::new("/bin/kill")
            .args(["-KILL", "--", &format!("-{group}")])
            .status();
        false
    };
    let took = started.elapsed();
    if passed {
        println!("  ✓ videotoolbox ({took:.1?})");
        return Ok(());
    }
    if crate::watchdog::encoder_stopped() {
        println!(
            "::warning title=VideoToolbox::this runner's video encoder stopped (\"No real \
             codec\"), so its VideoToolbox tests say nothing of the change"
        );
        println!("  ⚠ videotoolbox ({took:.1?}): the runner's encoder stopped");
        return Ok(());
    }
    println!("  ✘ videotoolbox ({took:.1?})");
    anyhow::bail!("step failed: videotoolbox")
}

/// How long the CI nextest run goes before [`crate::watchdog`] reports what it waits on. The
/// slowest shard's run takes under 10 minutes, and its job ends at 45.
const NEXTEST_HANG_AFTER: Duration = Duration::from_mins(15);

/// The tests lane's nextest run. On a runner it prints as it goes, so the profile's SLOW and
/// TERMINATING lines are in the log while it runs, and the watchdog names what a run that
/// outlives [`NEXTEST_HANG_AFTER`] waits on: a hung run once ended with the job's cancel and
/// not a line about the test (run 37173900555). Here its output comes as one block, as the
/// doctests' beside it does.
fn nextest_step(profile: &str, command: xshell::Cmd<'_>) -> Result<()> {
    if profile != NEXTEST_CI_PROFILE {
        return quiet_step("nextest", command);
    }
    let watchdog =
        crate::watchdog::Watchdog::start("nextest", std::process::id(), NEXTEST_HANG_AFTER);
    let result = crate::tools::step("nextest", &command);
    watchdog.finish();
    result
}

/// The [`LINUX_CRATES`] whose tests build and run on Linux: all but [`LINUX_UNTESTED`].
fn linux_tested() -> Vec<&'static str> {
    LINUX_CRATES.iter().copied().filter(|c| !LINUX_UNTESTED.contains(c)).collect()
}

/// The Linux lane, on a Linux host: the worker, its ptyd, the CLI and the server built as a
/// Linux box runs them (with the binaries the tests spawn), then the tests of every crate of
/// theirs whose tests build there, and their doctests. Without the workspace hack, whose
/// features pull in the client's GPUI, which no Linux build takes.
fn linux_lane(sh: &Shell, doc_sh: Shell, profile: &str) -> Result<()> {
    let tested = linux_tested();
    let p = &selected(tested.iter().copied());
    let timings: &[&str] = if profile == NEXTEST_CI_PROFILE { &["--timings"] } else { &[] };
    quiet_step("nextest build", cmd!(sh, "cargo nextest run {p...} --no-run {timings...}"))?;
    let mut spawned = tested.clone();
    for (_, package) in SPAWNED_BINS {
        if !spawned.contains(&package) {
            spawned.push(package);
        }
    }
    let spawned = selected(spawned);
    let bins = spawned_bin_args();
    // The binaries a Linux worker and server install, which the tests also spawn.
    quiet_step(
        "the Linux worker, server and spawned binaries",
        cmd!(sh, "cargo build --profile test {spawned...} --examples {bins...}"),
    )?;
    let _fresh = sh.push_env(BINS_FRESH, "1");
    let libs: Vec<String> = workspace_packages()?
        .into_iter()
        .filter(|package| package.lib && tested.contains(&package.name.as_str()))
        .map(|package| package.name)
        .collect();
    let libs = selected(libs.iter().map(String::as_str));
    let (tests, doctests) = std::thread::scope(|scope| {
        let doctests =
            scope.spawn(move || quiet_step("doctests", cmd!(doc_sh, "cargo test {libs...} --doc")));
        let tests = nextest_step(profile, cmd!(sh, "cargo nextest run {p...} --profile {profile}"));
        (tests, join(doctests))
    });
    both(tests, doctests)
}

/// The packages whose tests spawn [`SPAWNED_BINS`] (`slopty_testkit::bins::bin`), as
/// `tests::the_packages_whose_tests_spawn_binaries_are_listed` reads them from the sources.
const SPAWNING: [&str; 5] =
    ["slopty-workerd", "slopty-worker", "slopty-files", "slopty-cli", "slopty-client"];

/// What the build of [`SPAWNED_BINS`] selects besides `--bin`: the workspace with its tests, or
/// some packages (a shard's, or those `land` tests) and those that build the binaries, or
/// nothing when none of those packages' tests spawns one. Those leave the others' tests
/// unbuilt: `--tests` would build the binaries' packages' tests too, so `--examples` stands in
/// for it. No member has an example, and selecting them is what makes cargo resolve features
/// with the selected packages' dev-dependencies, as their test build did.
fn spawned_selection(packages: Option<&[&str]>) -> Option<Vec<String>> {
    let Some(packages) = packages else {
        return Some(vec!["--workspace".to_owned(), "--tests".to_owned()]);
    };
    if !packages.iter().any(|p| SPAWNING.contains(p)) {
        return None;
    }
    let mut names: Vec<&str> = packages.to_vec();
    names.push(WORKSPACE_HACK);
    for (_, package) in SPAWNED_BINS {
        if !names.contains(&package) {
            names.push(package);
        }
    }
    let mut args = selected(names);
    args.push("--examples".to_owned());
    Some(args)
}

/// `-p <name>` for `shard`'s packages that have a library, and so doctests, and for
/// [`WORKSPACE_HACK`].
fn shard_libs(shard: Shard) -> Result<Vec<String>> {
    let libs: Vec<String> = workspace_packages()?
        .into_iter()
        .filter(|p| p.lib && shard.packages().contains(&p.name.as_str()))
        .map(|p| p.name)
        .collect();
    Ok(selected(libs.iter().map(String::as_str).chain([WORKSPACE_HACK])))
}

/// What [`snapshot`] copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The index: what the next commit records, which the gate checks.
    Index,
    /// HEAD's tree: the commits `land` pushes.
    Head,
}

/// `source`'s entries as `git ls-files --stage -z` lists the index's, `<mode> <sha>
/// <stage>\t<path>` each: a commit's are all at stage 0.
fn list(sh: &Shell, source: Source) -> Result<std::process::Output> {
    Ok(match source {
        Source::Index => cmd!(sh, "git ls-files --stage -z").quiet().output()?,
        Source::Head => {
            let format = "--format=%(objectmode) %(objectname) 0%x09%(path)";
            cmd!(sh, "git ls-tree -r -z --full-tree {format} HEAD").quiet().output()?
        }
    })
}

/// Sync the **index** (what `git commit` would record, not the working tree), or HEAD's tree,
/// into `target/gate/tree`. Several agents edit this one checkout at once, so only the staged
/// state is a unit anyone vouched for: what passes is exactly what the next commit records. Blobs
/// are read in one `git cat-file --batch` pass; a manifest of the blob each path was last written
/// from keeps unchanged files untouched, so cargo in the snapshot rebuilds exactly what changed.
/// Paths the source no longer lists are removed; a submodule (`vendor/ghostty`) is a symlink to
/// a checkout at the commit the source pins ([`submodule_at`]). Returns the tree and the
/// source's entries it holds.
fn snapshot(root: &Utf8Path, source: Source) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
    let started = Instant::now();
    let gate = root.join("target").join("gate");
    let tree = gate.join("tree");
    let manifest_path = gate.join("tree.index");
    std::fs::create_dir_all(&tree).with_context(|| format!("create {tree}"))?;
    let sh = Shell::new()?;
    sh.change_dir(root);
    let listed = list(&sh, source)?;
    let before: HashMap<String, String> = std::fs::read_to_string(&manifest_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' ').map(|(sha, rel)| (rel.to_owned(), sha.to_owned())))
        .collect();
    let mut after: HashMap<String, String> = HashMap::new();
    let mut wanted: HashSet<String> = HashSet::new();
    let mut fetch: Vec<(String, String, bool)> = Vec::new();
    let mut listing = Vec::new();
    for entry in listed.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let entry = std::str::from_utf8(entry).context("an index path is not UTF-8")?;
        let Some((meta, rel)) = entry.split_once('\t') else { continue };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(sha), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("unexpected `git ls-files --stage` line: {entry}");
        };
        if stage != "0" {
            bail!("{rel} is unmerged in the index");
        }
        wanted.insert(rel.to_owned());
        listing.push(pass::Entry { path: rel.to_owned(), id: format!("{mode} {sha}") });
        let dst = tree.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if mode == "160000" {
            let module = submodule_at(root, &gate, rel, sha)?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(module.as_std_path()) {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(&module, &dst).with_context(|| format!("link {dst}"))?;
            }
            continue;
        }
        let key = format!("{mode}:{sha}");
        let present = std::fs::symlink_metadata(&dst).is_ok();
        if !present || before.get(rel) != Some(&key) {
            fetch.push((rel.to_owned(), sha.to_owned(), mode == "120000"));
        }
        after.insert(rel.to_owned(), key);
    }
    let written = write_blobs(root, &tree, &fetch, &after)?;
    let removed = prune(&tree, &tree, &wanted)?;
    let manifest = after.iter().fold(String::new(), |mut s, (rel, key)| {
        s.push_str(key);
        s.push(' ');
        s.push_str(rel);
        s.push('\n');
        s
    });
    std::fs::write(&manifest_path, manifest).with_context(|| format!("write {manifest_path}"))?;
    let what = if source == Source::Index { "the index" } else { "HEAD" };
    println!(
        "  snapshot of {what}, {} files: {written} written, {removed} removed ({:.1?})",
        wanted.len(),
        started.elapsed()
    );
    Ok((tree, listing))
}

/// A checkout of submodule `rel` at the commit the index pins, under `target/gate/modules/`:
/// the working submodule may be mid-bump, and the snapshot must build what the index records.
/// It is a worktree of the submodule's own repository, so it shares its objects and moving it
/// to another commit rewrites only the files that differ.
fn submodule_at(root: &Utf8Path, gate: &Utf8Path, rel: &str, sha: &str) -> Result<Utf8PathBuf> {
    let module = gate.join("modules").join(rel.replace('/', "__"));
    let sh = Shell::new()?;
    if module.join(".git").exists() {
        sh.change_dir(&module);
        let head = cmd!(sh, "git rev-parse HEAD").quiet().read()?;
        if head.trim() != sha {
            cmd!(sh, "git checkout --quiet --detach {sha}").quiet().run()?;
        }
    } else {
        std::fs::create_dir_all(gate.join("modules"))?;
        sh.change_dir(root.join(rel));
        cmd!(sh, "git worktree add --quiet --detach --force {module} {sha}").quiet().run()?;
    }
    Ok(module)
}

/// Write the blobs `fetch` names into `tree`, reading them all through one
/// `git cat-file --batch`. A file whose bytes already match keeps its mtime; a rewritten one is
/// stamped now, because a lane may have built the old bytes after the blob's own time.
fn write_blobs(
    root: &Utf8Path,
    tree: &Utf8Path,
    fetch: &[(String, String, bool)],
    modes: &HashMap<String, String>,
) -> Result<usize> {
    use std::io::{BufRead as _, Read as _, Write as _};
    if fetch.is_empty() {
        return Ok(0);
    }
    let mut child = std::process::Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .context("spawn git cat-file")?;
    let mut stdin = child.stdin.take().context("git cat-file stdin")?;
    let shas = fetch.iter().fold(String::new(), |mut s, (_, sha, _)| {
        s.push_str(sha);
        s.push('\n');
        s
    });
    let feeder = std::thread::spawn(move || stdin.write_all(shas.as_bytes()));
    let mut out = std::io::BufReader::new(child.stdout.take().context("git cat-file stdout")?);
    let mut written = 0_usize;
    for (rel, sha, link) in fetch {
        let mut header = String::new();
        out.read_line(&mut header)?;
        let size: usize = header
            .split(' ')
            .nth(2)
            .and_then(|s| s.trim().parse().ok())
            .with_context(|| format!("git cat-file header for {sha}: {header:?}"))?;
        let mut blob = vec![0_u8; size];
        out.read_exact(&mut blob)?;
        let mut newline = [0_u8; 1];
        out.read_exact(&mut newline)?;
        let dst = tree.join(rel);
        if *link {
            let target = std::str::from_utf8(&blob).context("a symlink target is not UTF-8")?;
            if std::fs::read_link(&dst).ok().as_deref() != Some(Utf8Path::new(target).as_std_path())
            {
                let _removed = std::fs::remove_file(&dst);
                std::os::unix::fs::symlink(target, &dst)?;
                written = written.saturating_add(1);
            }
            continue;
        }
        if std::fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink() || m.is_dir()) {
            let _removed = std::fs::remove_file(&dst);
        }
        if std::fs::read(&dst).is_ok_and(|old| old == blob) {
            continue;
        }
        std::fs::write(&dst, &blob).with_context(|| format!("write {rel}"))?;
        let executable = modes.get(rel).is_some_and(|k| k.starts_with("100755"));
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&dst, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        written = written.saturating_add(1);
    }
    feeder.join().map_err(|panic| anyhow::anyhow!("git cat-file feeder panicked: {panic:?}"))??;
    let status = child.wait()?;
    if !status.success() {
        bail!("git cat-file failed: {status}");
    }
    Ok(written)
}

/// Remove every file under `dir` that the tree no longer lists, and the directories that
/// empties. Returns how many files went.
fn prune(tree: &Utf8Path, dir: &Utf8Path, wanted: &HashSet<String>) -> Result<usize> {
    let mut removed = 0_usize;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {dir}"))? {
        let entry = entry?;
        let path = Utf8PathBuf::try_from(entry.path())
            .map_err(|e| anyhow::anyhow!("a snapshot path is not UTF-8: {e}"))?;
        let rel = path.strip_prefix(tree)?.as_str().to_owned();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            removed = removed.saturating_add(prune(tree, &path, wanted)?);
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
            }
        } else if !wanted.contains(&rel) {
            std::fs::remove_file(&path).with_context(|| format!("remove {path}"))?;
            removed = removed.saturating_add(1);
        }
    }
    Ok(removed)
}

/// Format check (or apply). Uses nightly rustfmt when available for the unstable options.
pub fn fmt(sh: &Shell, apply: bool) -> Result<()> {
    let nightly =
        cmd!(sh, "rustup run nightly rustfmt --version").quiet().ignore_stderr().read().is_ok();
    let toolchain: &[&str] = if nightly { &["+nightly"] } else { &[] };
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    quiet_step("cargo fmt", cmd!(sh, "cargo {toolchain...} fmt --all -- {check...}"))?;
    // The fuzz crate is a workspace of its own (`fuzz/`); without `--all`, which would reach
    // into its path dependencies, this formats that one package.
    if sh.path_exists("fuzz/Cargo.toml") {
        quiet_step(
            "cargo fmt (fuzz)",
            cmd!(sh, "cargo {toolchain...} fmt --manifest-path fuzz/Cargo.toml -- {check...}"),
        )?;
    }
    if has(sh, "taplo") {
        taplo(sh, apply)?;
    }
    Ok(())
}

/// Clippy on every triple: [`lint_host`], [`lint_ios`], then [`lint_linux`].
pub fn lint(sh: &Shell) -> Result<()> {
    lint_host(sh)?;
    lint_ios(sh)?;
    lint_linux(sh)
}

/// Clippy for Linux on every one of [`LINUX_CRATES`] (`tools::lint_linux`), and on xtask,
/// which the deep checks run on Linux runners.
pub fn lint_linux(sh: &Shell) -> Result<()> {
    let crates = crate::tools::lint_linux(sh, &LINUX_CRATES);
    both(crates, lint_linux_xtask(sh))
}

/// Clippy for Linux on xtask, which the deep checks run on Linux runners: through
/// `cargo-zigbuild`, as the crates are (`tools::lint_linux`).
fn lint_linux_xtask(sh: &Shell) -> Result<()> {
    let targets: Vec<String> = crate::tools::LINUX_TRIPLES
        .iter()
        .flat_map(|t| ["--target".to_owned(), (*t).to_owned()])
        .collect();
    quiet_step(
        "clippy linux-gnu (x86_64 + aarch64), xtask",
        cmd!(sh, "cargo-zigbuild clippy -p xtask {targets...} --all-targets -- -D warnings"),
    )
}

/// Clippy on the host with every target (tests, benches, examples) in one pass, the live
/// `slopty-e2e` targets the tests lane leaves out among them, `--locked` as CI fetches, so a
/// lock written from a manifest left out of the index fails here and not on the `gate` branch;
/// then on the fuzz crate, a
/// workspace of its own that no other lane builds, in its own target dir (`fuzz/target`, where
/// `cargo xtask fuzz --replay` builds it too).
pub fn lint_host(sh: &Shell) -> Result<()> {
    let host = TRIPLES[0];
    let live = crate::e2e::LIVE;
    let workspace = quiet_step(
        &format!("clippy {host}"),
        cmd!(
            sh,
            "cargo clippy --locked --keep-going --workspace --all-targets --features {live} --target {host} -- -D warnings"
        ),
    );
    let fuzz = if sh.path_exists("fuzz/Cargo.toml") {
        quiet_step(
            &format!("clippy {host} (fuzz)"),
            cmd!(
                sh,
                "cargo clippy --manifest-path fuzz/Cargo.toml --locked --all-targets --target {host} -- -D warnings"
            ),
        )
    } else {
        Ok(())
    };
    both(workspace, fuzz)
}

/// Both results, with both errors when both failed.
fn both(a: Result<()>, b: Result<()>) -> Result<()> {
    match (a, b) {
        (Err(a), Err(b)) => Err(anyhow::anyhow!("{a:#}; {b:#}")),
        (a, b) => a.and(b),
    }
}

/// Clippy on the two iOS triples together in one invocation (cargo builds them side by side
/// from one resolve), library and binary targets only — tests never run on iOS, and their code
/// is the same as the host's. Host-only crates are excluded.
pub fn lint_ios(sh: &Shell) -> Result<()> {
    let excludes: Vec<String> = host_only_present()?
        .into_iter()
        .flat_map(|c| ["--exclude".to_owned(), c.to_owned()])
        .collect();
    let ios: Vec<String> =
        TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
    quiet_step(
        "clippy ios + ios-sim",
        cmd!(sh, "cargo clippy --keep-going --workspace {excludes...} {ios...} -- -D warnings"),
    )
}

pub fn test(sh: &Shell, extra: &[String]) -> Result<()> {
    quiet_step("nextest", cmd!(sh, "cargo nextest run --workspace {extra...}"))?;
    quiet_step("doctests", cmd!(sh, "cargo test --workspace --doc"))
}

pub fn doc(sh: &Shell, open: bool) -> Result<()> {
    let open: &[&str] = if open { &["--open"] } else { &[] };
    let _env = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
    quiet_step(
        "rustdoc",
        cmd!(sh, "cargo doc --keep-going --workspace --no-deps --document-private-items {open...}"),
    )
}

/// Both lockfiles hold what the manifests ask for, as CI's `cargo fetch --locked` wants: a lock
/// written from a manifest left out of the index fails here, in seconds, rather than every
/// lane on the `gate` branch.
fn locked(sh: &Shell) -> Result<()> {
    quiet_step("cargo fetch --locked", cmd!(sh, "cargo fetch --locked"))?;
    if sh.path_exists("fuzz/Cargo.toml") {
        quiet_step(
            "cargo fetch --locked (fuzz)",
            cmd!(sh, "cargo fetch --locked --manifest-path fuzz/Cargo.toml"),
        )?;
    }
    Ok(())
}

fn deny(sh: &Shell) -> Result<()> {
    quiet_step("cargo deny", cmd!(sh, "cargo deny --workspace check"))?;
    one_gpui(&sh.read_file(sh.current_dir().join("Cargo.lock"))?)
}

/// The graph holds one GPUI: gpui-fast's. gpui-kit asks for `gpui-pre =0.3.x`, which the root
/// `[patch.crates-io]` points at the fork's `compat/` crates; when gpui-kit moves to a snapshot
/// the patch no longer matches, cargo only warns and builds crates.io's `gpui-pre` beside the
/// fork. `gpui-pre-reqwest` is a plain reqwest, built for wasm alone.
fn one_gpui(lock: &str) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Lock {
        package: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        version: String,
        source: Option<String>,
    }
    let lock: Lock = toml::from_str(lock).context("parse Cargo.lock")?;
    let mut wrong: Vec<String> = lock
        .package
        .iter()
        .filter(|p| {
            let registry = p.source.as_deref().is_some_and(|s| s.starts_with("registry+"));
            let pre = p.name.starts_with("gpui-pre") && p.name != "gpui-pre-reqwest";
            registry && (pre || p.name == "gpui")
        })
        .map(|p| format!("{} {} from crates.io", p.name, p.version))
        .collect();
    let gpuis = lock.package.iter().filter(|p| p.name == "gpui").count();
    if gpuis != 1 {
        wrong.push(format!("{gpuis} packages named gpui"));
    }
    ensure!(
        wrong.is_empty(),
        "a second GPUI in Cargo.lock ({}): bump the gpui-fast fork's `compat/` crates to the \
         gpui-pre version gpui-kit pins, and the root `[patch.crates-io]` with them",
        wrong.join(", ")
    );
    Ok(())
}

/// `workspace-hack` carries the workspace's current feature union (`cargo xtask check` builds
/// against it): regenerated under `--fix`, a stale one fails the gate.
fn hakari(sh: &Shell, fix: bool) -> Result<()> {
    let diff: &[&str] = if fix { &[] } else { &["--diff"] };
    quiet_step("cargo hakari", cmd!(sh, "cargo hakari generate {diff...}"))
}

fn shear(sh: &Shell, fix: bool) -> Result<()> {
    let fix: &[&str] = if fix { &["--fix"] } else { &[] };
    quiet_step("cargo shear", cmd!(sh, "cargo shear {fix...}"))
}

fn typos(sh: &Shell, fix: bool) -> Result<()> {
    let write: &[&str] = if fix { &["-w"] } else { &[] };
    quiet_step("typos", cmd!(sh, "typos {write...}"))
}

/// Only the tracked TOML files: left to its own globs taplo walks `target/` and the vendored
/// trees before its `exclude` list applies, which took ~90 s.
fn taplo(sh: &Shell, apply: bool) -> Result<()> {
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    let root = repo_root()?;
    let files = cmd!(sh, "git -C {root} ls-files *.toml").read()?;
    let files: Vec<&str> = files.lines().collect();
    quiet_step("taplo", cmd!(sh, "taplo fmt {check...} {files...}"))
}

/// Every commit since the last tag (or the root) follows Conventional Commits, and so does the
/// `message` of the one about to be made.
///
/// The tag is peeled to its commit: `cargo xtask release` writes **annotated** tags, and
/// `committed` panics (`Option::unwrap()` on `None`, `committed/src/git.rs:10`) on a range whose
/// start is a tag object rather than a commit. `v0.1.0..HEAD` crashed the step for every branch
/// from the first release on; `v0.1.0^{commit}..HEAD` is the same range and does not.
fn commits(sh: &Shell, message: Option<&Utf8Path>) -> Result<()> {
    let last_tag = cmd!(sh, "git describe --tags --abbrev=0").quiet().ignore_stderr().read().ok();
    let range =
        last_tag.map_or_else(|| "HEAD".to_owned(), |t| format!("{}^{{commit}}..HEAD", t.trim()));
    let history = quiet_step("committed", cmd!(sh, "committed {range} --no-merge-commit"));
    let pending = message.map_or(Ok(()), |file| {
        quiet_step("committed (message)", cmd!(sh, "committed --commit-file {file}"))
    });
    both(history, pending)
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    use super::{
        BINS_BUILT, BINS_FRESH, SPAWNED_BINS, Shard, Source, failed_tests, list, next_step,
        one_gpui, spawned_selection, summary_of,
    };
    use crate::tools::{WORKSPACE_HACK, repo_root, workspace_packages};

    /// The gate builds what the tests look for, under the variables they read.
    #[test]
    fn the_spawned_binaries_match_the_testkit() {
        let testkit = include_str!("../../crates/slopty-testkit/src/bins.rs");
        assert!(testkit.contains(&format!("pub const FRESH: &str = \"{BINS_FRESH}\";")));
        assert!(testkit.contains(&format!("pub const BUILT: &str = \"{BINS_BUILT}\";")));
        let names: Vec<&str> = SPAWNED_BINS.iter().map(|(bin, _)| *bin).collect();
        let count = format!("NAMES: [&str; {}]", names.len());
        assert!(testkit.contains(&count), "bins::NAMES is not {} long", names.len());
        for name in names {
            assert!(testkit.contains(&format!("\"{name}\"")), "{name} is not in bins::NAMES");
        }
    }

    /// A member in no shard would have its tests run by no CI job; one in two, by both.
    #[test]
    fn every_member_is_in_exactly_one_shard() {
        let members = workspace_packages().expect("the workspace's packages");
        for member in &members {
            let shards: Vec<&str> = Shard::value_variants()
                .iter()
                .filter(|s| s.packages().contains(&member.name.as_str()))
                .map(|s| s.name())
                .collect();
            assert_eq!(shards.len(), 1, "{} is in the shards {shards:?}", member.name);
        }
        for shard in Shard::value_variants() {
            for package in shard.packages() {
                assert!(
                    members.iter().any(|m| m.name == *package),
                    "shard {} names {package}, which is no member",
                    shard.name()
                );
                assert_ne!(*package, WORKSPACE_HACK, "every shard builds it already");
            }
        }
    }

    /// CI runs one job per shard: a shard missing from its matrix would run no tests at all.
    #[test]
    fn ci_runs_every_shard() {
        let path = repo_root().expect("repo root").join(".github/workflows/ci.yml");
        let workflow = std::fs::read_to_string(&path).expect("ci.yml");
        let listed = workflow.matches("shard: ").count();
        assert_eq!(listed, Shard::value_variants().len(), "one matrix entry a shard");
        for shard in Shard::value_variants() {
            let entry = format!("lane: tests, shard: {}, ", shard.name());
            assert!(workflow.contains(&entry), "ci.yml's matrix has no `{entry}`");
        }
    }

    /// CI runs the Linux lane on a Linux runner, apart from the gate's matrix until it is
    /// required; its tested crates all build for Linux and none is one the lane leaves untested.
    /// Every job's zig is the pinned one, every sccache setup names its version, and the deep
    /// checks take only some of the macOS runners.
    #[test]
    fn ci_runs_the_linux_lane_on_linux() {
        let path = repo_root().expect("repo root").join(".github/workflows/ci.yml");
        let workflow = std::fs::read_to_string(&path).expect("ci.yml");
        assert!(workflow.contains("cargo xtask gate --ci --lane linux"), "ci.yml runs it");
        assert!(workflow.contains("cargo xtask setup --lane linux"), "with its tools");
        assert!(workflow.contains("needs: [gate, linux]"), "and main waits for it");
        assert!(
            workflow.contains("lane: clippy-linux\n            os: ubuntu-24.04"),
            "clippy for Linux takes a Linux runner, not one of the five Macs"
        );
        let zig = format!("version: {}.", crate::tools::ZIG);
        assert!(workflow.contains(&zig), "CI's zig is the one setup asks for");
        let deep = std::fs::read_to_string(path.with_file_name("deep.yml")).expect("deep.yml");
        for jobs in [&workflow, &deep] {
            assert!(!jobs.contains("brew install zig"), "Homebrew's zig is whatever it is today");
            let sccache = jobs.matches("sccache-action@").count();
            let named =
                jobs.matches("sccache-action@v0.0.11\n        with:\n          version: v").count();
            assert_eq!(named, sccache, "every sccache setup names its version, or asks the API");
        }
        assert!(deep.contains("max-parallel: 1"), "the deep checks leave the gate macOS runners");
        let tested = super::linux_tested();
        assert!(tested.contains(&"slopty-ptyd") && tested.contains(&"slopty-cli"), "{tested:?}");
        assert!(tested.iter().all(|c| !crate::tools::LINUX_UNTESTED.contains(c)));
    }

    /// Nothing that compiles runs here before a land: the quick gate is the tools lane, and
    /// CI carries host clippy and the app's live tests, which ran on this Mac before; the
    /// latter in a workflow of their own, which holds no gate run back.
    #[test]
    fn the_quick_gate_compiles_nothing_and_ci_runs_the_app_e2e() {
        assert_eq!(super::QUICK, [super::LaneId::Tools], "the tools read text");
        let path = repo_root().expect("repo root").join(".github/workflows/ci.yml");
        let workflow = std::fs::read_to_string(&path).expect("ci.yml");
        assert!(workflow.contains("{ lane: clippy-host, os: macos-26 }"), "host clippy on CI");
        let e2e = std::fs::read_to_string(path.with_file_name("e2e.yml")).expect("e2e.yml");
        assert!(e2e.contains("run: cargo xtask e2e app --review"), "the app's e2e on CI");
        assert!(e2e.contains("name: e2e-app\n          path: target/e2e/artifacts"));
        assert!(!workflow.contains("e2e app"), "in a workflow of its own, off the gate's queue");
    }

    /// A shard builds the spawned binaries beside its own packages without the others' tests,
    /// and not at all when none of its tests spawns one.
    #[test]
    fn a_shard_builds_the_spawned_binaries_without_their_packages_tests() {
        let args = spawned_selection(Some(Shard::Rest.packages())).unwrap_or_default().join(" ");
        for (_, package) in SPAWNED_BINS {
            assert!(args.contains(&format!("-p {package}")), "{args}");
        }
        assert!(args.contains("-p slopty-cli") && args.contains("-p workspace-hack"), "{args}");
        assert!(args.ends_with("--examples") && !args.contains("--tests"), "{args}");
        assert_eq!(args.matches("-p slopty-testkit").count(), 1, "named once: {args}");
        assert_eq!(
            spawned_selection(Some(Shard::Ui.packages())),
            None,
            "the UI's tests spawn none"
        );
        assert_eq!(
            spawned_selection(None).unwrap_or_default(),
            ["--workspace", "--tests"],
            "the whole lane builds them as before"
        );
    }

    /// A package whose tests spawn a binary and is missing from [`SPAWNING`] would run them where
    /// nothing built it.
    #[test]
    fn the_packages_whose_tests_spawn_binaries_are_listed() {
        let packages = workspace_packages().expect("the workspace's packages");
        let mut spawning = std::collections::BTreeSet::new();
        for package in &packages {
            let mut sources = Vec::new();
            for dir in ["src", "tests"] {
                rust_sources(&package.dir.join(dir).into_std_path_buf(), &mut sources);
            }
            let spawns = sources.iter().any(|file| {
                std::fs::read_to_string(file).is_ok_and(|text| text.contains("bins::bin("))
            });
            // The one defines it, and this one names it.
            if spawns && !["slopty-testkit", "xtask"].contains(&package.name.as_str()) {
                spawning.insert(package.name.as_str());
            }
        }
        let listed: std::collections::BTreeSet<&str> = super::SPAWNING.into_iter().collect();
        assert_eq!(spawning, listed, "the packages whose tests call `slopty_testkit::bins::bin`");
    }

    fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// `land` snapshots HEAD as the gate snapshots the index: the same entries, in the same form,
    /// for every file the index holds as HEAD has it.
    #[test]
    fn head_is_listed_as_the_index_is() {
        let sh = xshell::Shell::new().expect("a shell");
        // `land` runs this inside its HEAD snapshot (`target/gate/tree`), which is no checkout of
        // its own: git answers there for the repository around it, and from that ignored folder
        // `ls-files` would list nothing. The listings are compared at the repository's top.
        sh.change_dir(repo_root().expect("repo root"));
        let top =
            xshell::cmd!(sh, "git rev-parse --show-toplevel").quiet().read().expect("a checkout");
        sh.change_dir(top);
        let entries = |source| {
            let listed = list(&sh, source).expect("git lists it");
            assert!(listed.status.success(), "{source:?}: {listed:?}");
            listed
                .stdout
                .split(|b| *b == 0)
                .filter(|e| !e.is_empty())
                .map(|e| String::from_utf8(e.to_vec()).expect("UTF-8"))
                .collect::<std::collections::BTreeSet<String>>()
        };
        let (head, index) = (entries(Source::Head), entries(Source::Index));
        for entry in &head {
            let (meta, _path) = entry.split_once('\t').expect("a tab before the path");
            let fields: Vec<&str> = meta.split(' ').collect();
            assert!(
                matches!(fields.as_slice(), [mode, sha, "0"] if mode.len() == 6 && sha.len() == 40),
                "{entry}"
            );
        }
        let shared = head.intersection(&index).count();
        let alike = shared.saturating_mul(10) > head.len().saturating_mul(9);
        assert!(alike, "{shared} of {} entries alike", head.len());
    }

    const FORK: &str = "git+https://github.com/aislopware/gpui-fast.git#58fb4674";

    fn lock(packages: &[(&str, &str)]) -> String {
        packages
            .iter()
            .map(|(name, source)| {
                format!(
                    "[[package]]\nname = \"{name}\"\nversion = \"0.3.7\"\nsource = \"{source}\"\n"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_fork_and_its_compat_crates_pass() -> anyhow::Result<()> {
        let crates_io = "registry+https://github.com/rust-lang/crates.io-index";
        let ok = lock(&[
            ("gpui", FORK),
            ("gpui-pre", FORK),
            ("gpui-pre-platform", FORK),
            ("gpui-pre-reqwest", crates_io),
        ]);
        one_gpui(&ok)
    }

    #[test]
    fn crates_io_gpui_pre_beside_the_fork_fails() {
        let crates_io = "registry+https://github.com/rust-lang/crates.io-index";
        let two = lock(&[("gpui", FORK), ("gpui-pre", crates_io)]);
        let error = one_gpui(&two).map_err(|e| e.to_string()).err().unwrap_or_default();
        assert!(error.contains("gpui-pre 0.3.7 from crates.io"), "{error}");
    }

    #[test]
    fn a_second_gpui_package_fails() {
        let two = lock(&[("gpui", FORK), ("gpui", "git+https://github.com/zed-industries/zed")]);
        assert!(one_gpui(&two).is_err());
    }

    /// A nextest report as the `ci` profile writes it: a pass, a failure, a flaky pass, a
    /// timeout and a name with markup in it.
    const JUNIT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="slopty" tests="5" failures="2" errors="1">
<testsuite name="slopty-grid" tests="2" failures="1">
<testcase name="tests::passes" classname="slopty-grid" time="0.01"/>
<testcase name="tests::breaks" classname="slopty-grid" time="0.02"><failure type="test failure">assertion failed</failure><system-err>panicked</system-err></testcase>
</testsuite>
<testsuite name="slopty-worker::session_actor" tests="3">
<testcase name="actor::flakes" classname="slopty-worker::session_actor" time="1.0"><flakyFailure type="test failure">once</flakyFailure></testcase>
<testcase name="actor::hangs" classname="slopty-worker::session_actor" time="60.0"><failure type="test timeout"/></testcase>
<testcase name="actor::reads_&lt;T&gt;" classname="slopty-worker::session_actor" time="0.1"><error type="crash">SIGSEGV</error></testcase>
</testsuite>
</testsuites>"#;

    #[test]
    fn the_failed_tests_are_named_and_a_flaky_pass_is_not() {
        assert_eq!(
            failed_tests(JUNIT),
            [
                "slopty-grid tests::breaks",
                "slopty-worker::session_actor actor::hangs",
                "slopty-worker::session_actor actor::reads_<T>",
            ],
            "the failures, timeouts and crashes, unescaped"
        );
        assert!(failed_tests("").is_empty(), "no report, no tests");
    }

    #[test]
    fn the_summary_names_each_failed_lane_and_the_failed_tests_under_theirs() {
        let tests = failed_tests(JUNIT);
        let failed = [
            ("clippy host", "step failed: clippy aarch64-apple-darwin".to_owned()),
            ("tests", "step failed: nextest (exit status: 100)".to_owned()),
        ];
        let summary = summary_of(&failed, &tests);
        let clippy = summary.find("gate lane failed: clippy host").unwrap_or(usize::MAX);
        let lane = summary.find("gate lane failed: tests").unwrap_or(usize::MAX);
        let test = summary.find("- `slopty-grid tests::breaks`").unwrap_or(usize::MAX);
        assert!(clippy < lane && lane < test, "{summary}");
        assert!(summary.contains("clippy aarch64-apple-darwin"), "the step that failed: {summary}");
        assert_eq!(summary.matches("\n- `").count(), 3, "one line a test: {summary}");
    }

    #[test]
    fn the_next_step_commits_what_was_gated_then_lands_it() {
        let with = next_step(true);
        assert!(with.contains("git commit -F target/gate/COMMIT_MSG"), "{with}");
        let without = next_step(false);
        assert!(without.starts_with("next: git commit\n"), "{without}");
        for text in [with, without] {
            assert!(text.contains("then: cargo xtask land"), "{text}");
        }
    }
}
