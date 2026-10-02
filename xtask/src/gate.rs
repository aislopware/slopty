//! `xtask gate`: everything that must be green before a commit lands.
//!
//! Here, before a commit, the gate runs the lanes that take seconds to a minute ([`QUICK`]: the
//! tools and host clippy) and prints the next step. The rest (tests, clippy for iOS and Linux,
//! rustdoc) runs on GitHub Actions when `cargo xtask land` pushes the commit to the `gate`
//! branch, and `main` moves to it only once every lane there passed (`.github/workflows/ci.yml`).
//! `--full` runs every lane here, as `cargo xtask release` and CI do.
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
use std::time::Instant;

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use pass::{Inputs, Plan, Scope};
use xshell::{Shell, cmd};

use crate::tools::{LINUX_CRATES, TRIPLES, has, host_only_present, quiet_step, repo_root};

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

/// The lanes a gate runs here before a commit: about a minute once the host build is warm.
pub const QUICK: [LaneId; 2] = [LaneId::Tools, LaneId::ClippyHost];

/// Where a gate given `--message` leaves it, for `git commit -F`.
const MESSAGE_FILE: &str = "target/gate/COMMIT_MSG";

/// The build lanes, each with its own target dir and a share of the cores. The host clippy
/// pass and the tests are the long ones; the rest fill the gaps.
const LANES: [(&str, u8); 5] =
    [("tools", 1), ("clippy host", 4), ("clippy ios", 4), ("tests", 6), ("rustdoc", 3)];

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
    Tests,
    Rustdoc,
}

impl LaneId {
    /// Its name in [`LANES`], the log and the pass records.
    const fn name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::ClippyHost => "clippy host",
            Self::ClippyIos => "clippy ios",
            Self::Tests => "tests",
            Self::Rustdoc => "rustdoc",
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
            Self::Tests => &["cargo-nextest"],
            Self::ClippyHost | Self::ClippyIos | Self::Rustdoc => &[],
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
}

impl Only {
    /// The lanes run here before a commit ([`QUICK`]).
    pub fn quick() -> Self {
        Self { lanes: QUICK.to_vec(), ci: false }
    }

    fn wants(&self, lane: LaneId) -> bool {
        self.lanes.is_empty() || self.lanes.contains(&lane)
    }
}

/// A shell in `tree` on lane `name`'s target dir. With `share` it runs beside other lanes and
/// gets its share of the cores; alone, cargo takes them all.
fn lane_shell(tree: &Utf8Path, gate_dir: &Utf8Path, name: &str, share: bool) -> Result<Shell> {
    let sh = Shell::new()?;
    sh.change_dir(tree);
    sh.set_var("CARGO_TARGET_DIR", gate_dir.join(name.replace(' ', "-")));
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
    // Two gates share `target/gate/tree`: a second one would rewrite the snapshot under the
    // first one's build and both logs would lie. The lock is released when the file closes.
    let gate_dir = root.join("target").join("gate");
    std::fs::create_dir_all(&gate_dir)?;
    let lock = std::fs::File::create(gate_dir.join(".lock"))?;
    if lock.try_lock().is_err() {
        bail!("another `cargo gate` is running on this checkout; wait for it");
    }
    crate::prune::ensure_room(&root.join("target"))?;
    if opts.in_place && opts.since_pass {
        bail!("--since-pass compares snapshots of the index; it cannot check the tree in place");
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
        let (tree, listing) = snapshot(&root)?;
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
                    cached(inputs, &tree, &build("clippy ios"), |_| {
                        let sh = lane("clippy ios")?;
                        // Linux runs whatever iOS found, so one gate names every failure.
                        both(lint_ios(&sh), lint_linux(&sh))
                    })
                }),
            ));
        }
        if only.wants(LaneId::Tests) {
            handles.push((
                "tests",
                scope.spawn(|| {
                    let tests = Lane {
                        name: "tests",
                        scope: Scope::Build,
                        extra: pass::tool_id("cargo-nextest"),
                        since_pass: opts.since_pass,
                    };
                    cached(inputs, &tree, &tests, |packages| {
                        test_lane(&lane("tests")?, lane("tests")?, profile, packages)
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
        let tests = std::fs::read_to_string(junit).map(|x| failed_tests(&x)).unwrap_or_default();
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
        if *lane == "tests" {
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

/// `slopty_testkit::bins::NAMES`, each after its `--bin`.
const SPAWNED_BINS: [&str; 12] = [
    "--bin",
    "slopty-ptyd",
    "--bin",
    "slopty-worker",
    "--bin",
    "slopty-server",
    "--bin",
    "slopty",
    "--bin",
    "slopty-stub-claude",
    "--bin",
    "slopty-stub-pi",
];

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
    cmd!(sh, "cargo build --workspace --tests {SPAWNED_BINS...}").run()?;
    let built = crate::tools::target_dir(sh)?.join("debug");
    std::fs::write(&env, format!("{BINS_BUILT}={built}\n"))
        .with_context(|| format!("write {}", std::path::Path::new(&env).display()))?;
    Ok(())
}

/// The gate's tests: build every test binary, then run nextest's `profile` (on `only`'s
/// packages when given) and the doctests side by side. Cargo holds the target dir's lock only
/// while it builds, and the build is done, so neither waits for the other. Meanwhile
/// [`crate::ptys`] counts the pseudo-terminals they hold: the lane fails on a leak, on more than
/// its budget at once, or on the system running out, naming the tests.
fn test_lane(sh: &Shell, doc_sh: Shell, profile: &str, only: Option<&[String]>) -> Result<()> {
    // On a runner, `--timings` leaves `cargo-timing.html` in the target dir for CI to keep: what
    // each unit of the build cost. Here it would pile up a report per gate.
    let timings: &[&str] = if profile == NEXTEST_CI_PROFILE { &["--timings"] } else { &[] };
    quiet_step("nextest build", cmd!(sh, "cargo nextest run --workspace --no-run {timings...}"))?;
    // Every binary a test spawns, so no test shells out to cargo and waits on its locks while
    // another build on the machine holds them. With the workspace's tests selected, all built
    // just now, cargo resolves features with every member's dev-dependencies as that build did
    // (`slopty-tailnet/fake`, `slopty-codec/experiments`), so each crate is the unit the tests
    // already have. Without them a dozen workspace crates compiled a second time, differently.
    quiet_step("spawned binaries", cmd!(sh, "cargo build --workspace --tests {SPAWNED_BINS...}"))?;
    let _fresh = sh.push_env(BINS_FRESH, "1");
    // Test binaries run out of `run/`, not `deps/` (`crate::runner`).
    let runner = crate::runner::command()?;
    let filter: Vec<String> = only.map_or_else(Vec::new, |packages| {
        let expr = packages.iter().map(|p| format!("package(={p})")).collect::<Vec<_>>();
        vec!["--no-tests=warn".to_owned(), "-E".to_owned(), expr.join(" | ")]
    });
    let tree = Utf8PathBuf::from_path_buf(sh.current_dir())
        .map_err(|p| anyhow::anyhow!("the tree's path is not UTF-8: {}", p.display()))?;
    // Beside the JUnit report, which CI keeps.
    let log = tree.join("target").join("nextest").join(profile).join("ptys.log");
    let ptys = crate::ptys::Sampler::start(std::process::id(), &tree, Some(log));
    let (tests, doctests) = std::thread::scope(|scope| {
        let doctests = scope
            .spawn(move || quiet_step("doctests", cmd!(doc_sh, "cargo test --workspace --doc")));
        let tests = quiet_step(
            "nextest",
            cmd!(sh, "cargo nextest run --workspace --profile {profile} {filter...}")
                .env(crate::runner::RUNNER_VAR, &runner),
        );
        (tests, join(doctests))
    });
    let ptys = ptys.finish();
    print!("{}", ptys.report());
    both(both(tests, doctests), ptys.verdict())
}

/// Sync the **index** (what `git commit` would record, not the working tree) into
/// `target/gate/tree`. Several agents edit this one checkout at once, so only the staged state
/// is a unit anyone vouched for: what passes is exactly what the next commit records. Blobs are
/// read in one `git cat-file --batch` pass; a manifest of the blob each path was last written
/// from keeps unchanged files untouched, so cargo in the snapshot rebuilds exactly what changed.
/// Paths the index no longer lists are removed; a submodule (`vendor/ghostty`) is a symlink to
/// a checkout at the commit the index pins ([`submodule_at`]). Returns the tree and the index's
/// entries it holds.
fn snapshot(root: &Utf8Path) -> Result<(Utf8PathBuf, Vec<pass::Entry>)> {
    let started = Instant::now();
    let gate = root.join("target").join("gate");
    let tree = gate.join("tree");
    let manifest_path = gate.join("tree.index");
    std::fs::create_dir_all(&tree).with_context(|| format!("create {tree}"))?;
    let sh = Shell::new()?;
    sh.change_dir(root);
    let listed = cmd!(sh, "git ls-files --stage -z").quiet().output()?;
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
    println!(
        "  snapshot of the index, {} files: {written} written, {removed} removed ({:.1?})",
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
    let targets: Vec<String> = crate::tools::LINUX_TRIPLES
        .iter()
        .flat_map(|t| ["--target".to_owned(), (*t).to_owned()])
        .collect();
    let xtask = quiet_step(
        "clippy linux-gnu (x86_64 + aarch64), xtask",
        cmd!(sh, "cargo clippy -p xtask {targets...} --all-targets -- -D warnings")
            .env("CARGO_FEATURE_NO_NEON", "1"),
    );
    both(crates, xtask)
}

/// Clippy on the host with every target (tests, benches, examples) in one pass, the live
/// `slopty-e2e` targets the tests lane leaves out among them; then on the fuzz crate, a
/// workspace of its own that no other lane builds, in its own target dir (`fuzz/target`, where
/// `cargo xtask fuzz --replay` builds it too).
pub fn lint_host(sh: &Shell) -> Result<()> {
    let host = TRIPLES[0];
    let live = crate::e2e::LIVE;
    let workspace = quiet_step(
        &format!("clippy {host}"),
        cmd!(
            sh,
            "cargo clippy --keep-going --workspace --all-targets --features {live} --target {host} -- -D warnings"
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
    use super::{
        BINS_BUILT, BINS_FRESH, SPAWNED_BINS, failed_tests, next_step, one_gpui, summary_of,
    };

    /// The gate builds what the tests look for, under the variables they read.
    #[test]
    fn the_spawned_binaries_match_the_testkit() {
        let testkit = include_str!("../../crates/slopty-testkit/src/bins.rs");
        assert!(testkit.contains(&format!("pub const FRESH: &str = \"{BINS_FRESH}\";")));
        assert!(testkit.contains(&format!("pub const BUILT: &str = \"{BINS_BUILT}\";")));
        let names: Vec<&str> = SPAWNED_BINS.iter().copied().filter(|a| *a != "--bin").collect();
        let count = format!("NAMES: [&str; {}]", names.len());
        assert!(testkit.contains(&count), "bins::NAMES is not {} long", names.len());
        for name in names {
            assert!(testkit.contains(&format!("\"{name}\"")), "{name} is not in bins::NAMES");
        }
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
