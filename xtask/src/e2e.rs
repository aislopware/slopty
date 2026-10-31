//! `xtask e2e`: the live tests, run on purpose and in isolation.
//!
//! Every test that touches real hardware or real permissions (posting events, capturing the
//! screen, running the daemons) is live, `#[ignore = "live: …"]`, so `cargo gate` never touches
//! the desktop and reports each one as skipped, not passed. This command is the one sanctioned
//! way to run them: it picks the case, runs its ignored tests, gives the daemons their own
//! `SLOPTY_DATA_DIR` under `target/e2e/` so nothing installed is touched, runs nextest with
//! output visible, and prints what ran. No ad-hoc key presses, screenshots or window
//! probing outside these tests: what they need is asserted inside them.
//!
//! Every suite runs from a recorded build: its test binary is built and listed once
//! (`cargo nextest list --list-type binaries-only`), and nextest runs that list with
//! `--binaries-metadata`, so `--no-build` reruns the last build without calling cargo at all.
//! Other sessions build in this checkout's `target/` too; a rerun that goes through cargo waits on
//! their build lock and rebuilds what they edited.

use std::fmt::Write as _;

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::{WORKSPACE_HACK, step};

/// The feature that builds `slopty-e2e`'s live test targets, which every build that runs or
/// lints them names (`crates/slopty-e2e/Cargo.toml`).
pub const LIVE: &str = "slopty-e2e/live";

/// The package whose test targets need [`LIVE`].
pub const LIVE_PACKAGE: &str = "slopty-e2e";

/// The packages every e2e build selects: the daemons, the CLI, the app, the e2e crate (its live
/// targets and helpers), the test kit (the stand-ins), and [`WORKSPACE_HACK`], as `xtask check`
/// names it. The binaries and the test binary a run takes are built by two cargo calls, and cargo
/// resolves features over the packages each selects: with `-p slopty-e2e` alone the test binary
/// resolved its own, and 57 units (`slopty-platform` without `e2e`, `tokio`, `objc2`, `syn`, …)
/// were built a second time, 57–65 s of every run whose core crates changed and 37–126 s on a
/// runner (`docs/decisions/testing.md`, "Goldens render in the test process", item 6).
const PACKAGES: [&str; 8] = [
    "slopty-ptyd",
    "slopty-workerd",
    "slopty-serverd",
    "slopty-cli",
    "slopty",
    LIVE_PACKAGE,
    "slopty-testkit",
    WORKSPACE_HACK,
];

/// The features every e2e build turns on, in both of its cargo calls ([`PACKAGES`]): the app's
/// renderer access for `Render`, which also makes the `slopty-app-e2e` binary the tests start;
/// the worker's seam that stalls its control streams; and the live test targets.
const FEATURES: [&str; 3] = ["slopty/e2e", "slopty-workerd/e2e", LIVE];

/// The binaries an e2e suite spawns, each named: every binary of [`PACKAGES`] but the plain
/// `slopty-app` (174 MB, which the harness never starts: it starts `slopty-app-e2e`) and the
/// developer's probes `slopty-probe` and `slopty-glass`, which no test runs. `--bins` linked and
/// copied all thirteen. A binary a test spawns that is missing here is missing from the run, so
/// `tests::every_binary_of_the_e2e_packages_is_built_or_left_out_by_name` holds the list to the
/// packages' own.
pub const BINS: [&str; 10] = [
    "slopty-app-e2e",
    "slopty-ptyd",
    "slopty-worker",
    "slopty-server",
    "slopty",
    PROBE,
    "slopty-idle-window",
    "slopty-stub-claude",
    "slopty-stub-managed-claude",
    "slopty-stub-window",
];

/// `-p` for each package of [`PACKAGES`] and `extra`, [`FEATURES`], and the Mac's triple: what
/// every cargo call of an e2e build selects, the build of the binaries and each test binary's
/// list or archive alike, so they resolve one feature set and share every unit. The triple, as
/// every other cargo call on a Mac names it (`gate::mac_target`): the simulator's build is cross
/// too, so their build scripts and proc macros are one set, where without it the Mac's were built
/// again with the Mac's `target-cpu`.
pub fn selection(extra: &[&str]) -> Vec<String> {
    let mut packages: Vec<&str> = PACKAGES.to_vec();
    for package in extra {
        if !packages.contains(package) {
            packages.push(package);
        }
    }
    let mut args: Vec<String> =
        packages.iter().flat_map(|p| ["-p".to_owned(), (*p).to_owned()]).collect();
    args.extend(FEATURES.iter().flat_map(|f| ["--features".to_owned(), (*f).to_owned()]));
    args.extend(crate::gate::mac_target().iter().map(|a| (*a).to_owned()));
    args
}

/// Where an e2e build in `target` (a target dir) leaves its binaries: under the Mac's triple
/// ([`selection`]).
pub fn bin_dir(target: &camino::Utf8Path) -> camino::Utf8PathBuf {
    let triple = crate::gate::mac_target().get(1).copied();
    triple.map_or_else(|| target.join("debug"), |t| target.join(t).join("debug"))
}

/// Which live tests to run.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    /// ptyd + worker + the real app, driven through its test socket: add the worker, open a
    /// shell, type, read the rows back, render frames with the app's own renderer and compare
    /// them with the goldens. No permissions needed; `--screen-recording` adds the cases that
    /// capture a window.
    App,
    /// ptyd + worker + a client over loopback QUIC: open a shell, read its output. No
    /// permissions needed.
    Worker,
    /// Window geometry, a display stream and the stream through the worker. Needs Screen Recording
    /// for the test binaries (System Settings ▸ Privacy ▸ Screen Recording).
    Screen,
    /// One pointer move on the main display and back. Needs Accessibility for the test binary.
    Input,
    /// The server with a real worker: `slopty-server` and ptyd + worker registered with it, on
    /// ports of their own, driven through the `slopty` CLI with `--json` and MCP over HTTP:
    /// the directory, a shell typed to and read, files both ways, ports, close and exit, the
    /// lease lost on a killed worker and taken back. No permissions needed.
    Server,
    /// All of the above but the simulator case (`ios`).
    All,
    /// ptyd + worker on the Mac and the iOS app in the simulator (`--sim iphone|ipad`), driven
    /// through its test socket: add the worker, open a shell, type, read the rows back, render
    /// frames against the per-device goldens (`ios-phone-*`, `ios-pad-*`).
    Ios,
}

/// Options.
#[derive(Args, Debug)]
pub struct E2eOpts {
    /// Which tests.
    #[arg(value_enum, default_value_t = Case::App)]
    case: Case,
    /// Write every render golden under `crates/slopty-e2e/golden/` that its frame differs from
    /// by a pixel or a word, within the tolerance too.
    #[arg(long)]
    accept: bool,
    /// Rewrite every render golden under `crates/slopty-e2e/golden/`.
    #[arg(long)]
    accept_all: bool,
    /// Fail on no render golden and write none: every changed frame leaves its render and diff
    /// under the artifacts, so one run shows every golden for a design review.
    #[arg(long, conflicts_with_all = ["accept", "accept_all"])]
    review: bool,
    /// Data directory for the daemons the tests spawn (default `target/e2e`).
    #[arg(long)]
    data_dir: Option<String>,
    /// `RUST_LOG` for the daemons and tests.
    #[arg(long, default_value = "info")]
    log: String,
    /// Which simulator the `ios` case uses.
    #[arg(long, value_enum, default_value_t)]
    sim: crate::ios::SimKind,
    /// A nextest filterset narrowing the case, such as
    /// `test(a_folder_tile_browses_the_worker)`. `--accept` and `--review` then touch only the
    /// goldens the chosen tests render. A suite it matches nothing in is skipped; matching
    /// nothing in every suite fails.
    #[arg(long, short = 'E')]
    filter: Option<String>,
    /// Build nothing: rerun the binaries and the test binaries the last run built, as they are.
    /// For a rerun right after a build; an edit since then is not in it.
    #[arg(long)]
    no_build: bool,
    /// Also run the tests that capture the screen (each target's `screen_recording` module), which
    /// need the Screen Recording grant.
    #[arg(long)]
    screen_recording: bool,
    /// Run this many tests at once, their output captured and shown for the failures, instead of
    /// one at a time with every line shown as it comes (`--no-capture`).
    #[arg(long, value_name = "N")]
    test_threads: Option<usize>,
}

/// How a suite's tests stay out of `cargo gate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kept {
    /// They do not: the suite runs under `cargo gate` too.
    No,
    /// `#[ignore = "live: …"]`, run here with `--run-ignored only`.
    Ignored,
    /// A variable the tests read, returning early without it; set to `1` here. Only the live
    /// tests of `slopty-capture` and `slopty-input` still keep out this way.
    Env(&'static str),
}

/// One nextest invocation.
struct Suite {
    /// How its tests stay out of `cargo gate`.
    kept: Kept,
    /// Package.
    package: &'static str,
    /// Integration test target.
    test: &'static str,
    /// A nextest filterset every run of the suite applies, beside the caller's.
    only: Option<&'static str>,
    /// A nextest filterset for a run the caller gives none: what the case means when the target
    /// holds more live tests, which `--filter` can still reach.
    default: Option<&'static str>,
    /// Its tests code video through VideoToolbox: they run after the rest, behind the encoder
    /// probe and a deadline ([`coding_step`]).
    codes: bool,
    /// Its tests code no video, so its processes open no `VideoToolbox` session: the warm-up at
    /// launch is refused with the rest ([`NO_VIDEOTOOLBOX`]).
    no_videotoolbox: bool,
}

impl Suite {
    /// A suite of live tests: the whole target, in parallel.
    const fn live(package: &'static str, test: &'static str) -> Self {
        Self {
            kept: Kept::Ignored,
            package,
            test,
            only: None,
            default: None,
            codes: false,
            no_videotoolbox: false,
        }
    }

    /// A suite kept out of `cargo gate` by `var`.
    const fn env(var: &'static str, package: &'static str, test: &'static str) -> Self {
        Self { kept: Kept::Env(var), ..Self::live(package, test) }
    }

    /// Only the tests `filterset` picks.
    const fn only(self, filterset: &'static str) -> Self {
        Self { only: Some(filterset), ..self }
    }

    /// Its tests code video ([`Suite::codes`]).
    const fn codes(self) -> Self {
        Self { codes: true, ..self }
    }

    /// Its tests code no video ([`Suite::no_videotoolbox`]).
    const fn no_videotoolbox(self) -> Self {
        Self { no_videotoolbox: true, ..self }
    }
}

/// The module holding a target's tests that need the Screen Recording grant; they run only
/// with `--screen-recording`.
const SCREEN_RECORDING: &str = "test(~screen_recording::)";
/// The app target's tests that code video: each target's `stream` module (a remote window of
/// the worker's drawn screen streamed into the right pane, no grant needed) and its
/// `screen_recording` module (real capture, run with `--screen-recording`). A new one left out
/// fails in the first group, where every `VideoToolbox` session is refused
/// ([`NO_VIDEOTOOLBOX`]), naming the call.
macro_rules! coding {
    () => {
        "(test(~screen_recording::) | test(~stream::))"
    };
}

/// The app's tests: first every one that codes no video, its processes opening no session,
/// so a runner whose encoder stops cannot hold them; then the ones that code, last, behind the
/// encoder probe ([`coding_step`]).
const APP: &[Suite] = &[
    Suite::live("slopty-e2e", "app").only(concat!("not ", coding!())).no_videotoolbox(),
    Suite::live("slopty-e2e", "app").only(coding!()).codes(),
];

const WORKER: &[Suite] = &[Suite {
    kept: Kept::No,
    ..Suite::live("slopty-workerd", "e2e").only("test(~shell_round_trip)")
}];

/// The real-capture cases. The worker's suites code video, so they run behind the encoder probe
/// and its deadline: on run 38245137677 a hosted runner's encoder stopped after the rate
/// control test, and `display_stream_produces_datagrams` then sat in `BuildStuck`, its process
/// stuck in its exit inside the driver for 25 minutes.
const SCREEN: &[Suite] = &[
    Suite::env("SLOPTY_SCREEN_E2E", "slopty-capture", "geometry"),
    Suite::env("SLOPTY_SCREEN_E2E", "slopty-capture", "latency"),
    Suite::live("slopty-worker", "screen").codes(),
    // The worker's other live tests are measurements, run by name with `--filter`.
    Suite { default: Some("test(~screen_stream)"), ..Suite::live("slopty-workerd", "e2e") }.codes(),
];

const INPUT: &[Suite] = &[Suite::env("SLOPTY_INPUT_E2E", "slopty-input", "inject")];

const IOS: [Suite; 1] = [Suite::live("slopty-e2e", "ios")];

const SERVER: [Suite; 1] = [Suite::live("slopty-e2e", "server")];

pub fn run(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    // The simulator cases' tools, before a minute of compiling.
    if matches!(opts.case, Case::Ios) {
        crate::ios::preflight(sh, true)?;
    }
    let suites: Vec<&Suite> = match opts.case {
        Case::App => APP.iter().collect(),
        Case::Worker => WORKER.iter().collect(),
        Case::Screen => SCREEN.iter().collect(),
        Case::Input => INPUT.iter().collect(),
        Case::Server => SERVER.iter().collect(),
        Case::All => APP.iter().chain(WORKER).chain(&SERVER).chain(SCREEN).chain(INPUT).collect(),
        Case::Ios => IOS.iter().collect(),
    };
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| sh.current_dir().join("target/e2e").to_string_lossy().into_owned());
    std::fs::create_dir_all(format!("{data_dir}/run"))?;
    println!("▶ e2e {:?} (data dir {data_dir})", opts.case);

    let artifacts = format!("{data_dir}/artifacts");
    std::fs::create_dir_all(&artifacts)?;
    let target = crate::tools::target_dir(sh)?;
    let bin_dir = bin_dir(&target);
    // Where each suite's nextest run leaves its JUnit report, which the run's record counts.
    let profile = std::env::var("NEXTEST_PROFILE").unwrap_or_else(|_| "default".to_owned());
    let junit = target.join("nextest").join(profile).join("junit.xml");
    let mut record = Record::default();
    // Daemons the tests spawn get their own sockets and state; nothing installed is touched.
    let _env = [
        sh.push_env("SLOPTY_DATA_DIR", &data_dir),
        sh.push_env("SLOPTY_PTYD_SOCKET", format!("{data_dir}/run/ptyd.sock")),
        sh.push_env("SLOPTY_WORKER_SOCKET", format!("{data_dir}/run/worker.sock")),
        sh.push_env("SLOPTY_E2E_BIN_DIR", &bin_dir),
        sh.push_env("SLOPTY_E2E_ARTIFACTS", &artifacts),
        sh.push_env("RUST_LOG", &opts.log),
    ];
    let _accept = if opts.accept_all {
        Some(sh.push_env("SLOPTY_E2E_ACCEPT", "all"))
    } else if opts.accept {
        Some(sh.push_env("SLOPTY_E2E_ACCEPT", "changed"))
    } else if opts.review {
        Some(sh.push_env("SLOPTY_E2E_ACCEPT", "review"))
    } else {
        None
    };

    let reuse = sh.current_dir().join("target/e2e/reuse");
    let cargo_metadata = reuse.join("cargo-metadata.json");
    let recorded = |suite: &Suite| reuse.join(format!("{}-{}.json", suite.package, suite.test));
    if opts.no_build {
        if let Some(missing) =
            suites.iter().find(|s| !recorded(s).exists() || !cargo_metadata.exists())
        {
            bail!(
                "no build of {} {} to reuse: run once without --no-build",
                missing.package,
                missing.test
            );
        }
        println!("▶ reusing the last build (--no-build)");
    } else {
        // Every binary a suite may spawn up front, so a test never shells out to cargo, and the
        // suites' test binaries with them: one feature set ([`build`]).
        let started = std::time::Instant::now();
        let (packages, tests) = suites_built(&suites);
        build(sh, &packages, &tests, &[])?;
        std::fs::create_dir_all(&reuse)?;
        let metadata = cmd!(sh, "cargo metadata --format-version 1").quiet().read()?;
        std::fs::write(&cargo_metadata, metadata)?;
        record.phase("build", started.elapsed(), None);
    }

    // Every binary a suite spawns is built: `slopty_testkit::bins` looks no further.
    let _fresh = sh.push_env(crate::gate::BINS_FRESH, "1");

    // The iOS case also needs the app in a booted simulator; the test launches it there.
    let simulator = matches!(opts.case, Case::Ios)
        .then(|| -> Result<_> {
            let started = std::time::Instant::now();
            let ios_opts = crate::ios::IosOpts::for_e2e(opts.sim, &opts.log);
            let udid = crate::ios::install_on_simulator(sh, &ios_opts)?;
            record.phase("simulator", started.elapsed(), None);
            Ok((
                sh.push_env("SLOPTY_SIM_UDID", udid.clone()),
                sh.push_env("SLOPTY_SIM_BUNDLE_ID", crate::ios::BUNDLE_ID),
                udid,
            ))
        })
        .transpose()?;

    let mut failed = Vec::new();
    let mut matched = 0_usize;
    for suite in &suites {
        let _gate = match suite.kept {
            Kept::Env(var) => Some(sh.push_env(var, "1")),
            Kept::No | Kept::Ignored => None,
        };
        let title = format!("{} {} {}", suite.package, suite.test, suite.only.unwrap_or_default());
        let title = title.trim();
        let binaries = recorded(suite);
        if !opts.no_build && build_suite(sh, suite, &suites, &binaries).is_err() {
            failed.push(title.to_owned());
            continue;
        }
        let expr = filterset(suite, opts.filter.as_deref(), opts.screen_recording);
        let ignored: &[&str] =
            if suite.kept == Kept::Ignored { &["--run-ignored", "only"] } else { &[] };
        let output: Vec<String> = match opts.test_threads {
            None => vec!["--no-capture".to_owned()],
            // Passing tests' output too: their MEASURE lines are what a run reports.
            Some(n) => vec![
                "--test-threads".to_owned(),
                n.to_string(),
                "--failure-output".to_owned(),
                "final".to_owned(),
                "--success-output".to_owned(),
                "final".to_owned(),
            ],
        };
        let mut command = cmd!(
            sh,
            "cargo nextest run --binaries-metadata {binaries} --cargo-metadata {cargo_metadata} {output...} --no-fail-fast {ignored...} -E {expr}"
        );
        if suite.no_videotoolbox {
            command = command.env(NO_VIDEOTOOLBOX, "1");
        }
        println!("▶ {title}");
        // A suite that runs nothing writes no report: the last suite's must not count for it.
        let _gone = std::fs::remove_file(&junit);
        let started = std::time::Instant::now();
        let watchdog = on_ci().then(|| {
            crate::watchdog::Watchdog::start(title, std::process::id(), SUITE_HANG_AFTER, None)
        });
        let ran = if suite.codes && cfg!(target_os = "macos") {
            coding_step(&bin_dir.join(PROBE), command)
        } else {
            Ran::Exited(
                std::process::Command::from(command)
                    .status()
                    .with_context(|| format!("start nextest for {title}"))?
                    .code(),
            )
        };
        if let Some(watchdog) = watchdog {
            // Given no group to end, it ends nothing.
            let _ended = watchdog.finish();
        }
        let outcome = match ran {
            Ran::Exited(Some(0)) | Ran::Excused => {
                matched = matched.saturating_add(1);
                "✓"
            }
            // Nextest's "no tests to run": the filter picked nothing in this suite, or every test
            // it holds captures the screen and this run has no `--screen-recording`.
            Ran::Exited(Some(NO_TESTS_RUN)) if opts.filter.is_some() || !opts.screen_recording => {
                "–"
            }
            Ran::Exited(_) => {
                matched = matched.saturating_add(1);
                failed.push(title.to_owned());
                if on_ci() {
                    print!("{}", machine_state());
                }
                "✘"
            }
        };
        println!("  {outcome} {title} ({:.1?})", started.elapsed());
        let report = std::fs::read_to_string(&junit).ok();
        record.phase(title, started.elapsed(), report.as_deref().and_then(Tests::of));
    }
    if let Some((_, _, udid)) = &simulator {
        // A simulator left booted keeps CoreAudio busy long after: the client's audio player
        // then takes tens of seconds to open and the screen worker's test times out in the
        // next gate (gate 307, 2026-09-15).
        cmd!(sh, "xcrun simctl shutdown {udid}").ignore_status().quiet().run()?;
    }
    if let Some(filter) = &opts.filter
        && matched == 0
        && failed.is_empty()
    {
        bail!("e2e {:?}: `{filter}` matched no test", opts.case);
    }
    // The record is a report, not a verdict: one it cannot write fails nothing.
    if let Err(e) = record.report(&format!("{:?}", opts.case)) {
        println!("  the run's record went unwritten: {e:#}");
    }
    if failed.is_empty() {
        println!(
            "✔ e2e {:?}: {} suite(s) passed; renders under {artifacts}",
            opts.case,
            suites.len()
        );
        Ok(())
    } else {
        bail!("e2e {:?}: failed {}", opts.case, failed.join(", "))
    }
}

/// What a run cost and ran: each phase's time (the build, the simulator, every suite) and each
/// suite's tests as nextest's `JUnit` report counts them, summed over the run. A run of several
/// suites otherwise reads, in its last `Summary` line, as its last suite's tests alone.
#[derive(Default)]
struct Record {
    phases: Vec<(String, std::time::Duration, Option<Tests>)>,
}

/// One suite's tests, from its `JUnit` report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Tests {
    run: u32,
    /// Each failed test, as `binary test`.
    failed: Vec<String>,
}

impl Tests {
    /// The tests nextest's `JUnit` report `xml` holds, but for its setup scripts, which it
    /// reports as tests too (`@setup-script:…`), and those that failed; none when `xml` is no
    /// such report.
    fn of(xml: &str) -> Option<Self> {
        let (_, cases) = xml.split_once("<testsuites ")?;
        let run = cases
            .split("<testcase ")
            .skip(1)
            .filter(|case| !crate::gate::attribute(case, "classname").starts_with('@'))
            .count();
        Some(Self {
            run: u32::try_from(run).unwrap_or(u32::MAX),
            failed: crate::gate::failed_tests(xml),
        })
    }

    fn add(&mut self, other: &Self) {
        self.run = self.run.saturating_add(other.run);
        self.failed.extend(other.failed.iter().cloned());
    }
}

impl Record {
    fn phase(&mut self, title: &str, took: std::time::Duration, tests: Option<Tests>) {
        self.phases.push((title.to_owned(), took, tests));
    }

    /// Every phase in a line, and the whole run's time and tests.
    fn line(&self) -> String {
        let mut whole = Tests::default();
        let mut took = std::time::Duration::ZERO;
        let mut phases = Vec::new();
        for (title, time, tests) in &self.phases {
            took = took.saturating_add(*time);
            let counted = tests.as_ref().map_or_else(String::new, |tests| {
                whole.add(tests);
                format!(", {} tests", tests.run)
            });
            phases.push(format!("{title} {:.1}s{counted}", time.as_secs_f64()));
        }
        format!(
            "{} ({:.1}s): {} tests, {} failed",
            phases.join("; "),
            took.as_secs_f64(),
            whole.run,
            whole.failed.len()
        )
    }

    /// Print the record and, on GitHub Actions, add it to the job's summary with every failed
    /// test by name.
    fn report(&self, case: &str) -> Result<()> {
        let line = self.line();
        println!("  run: {line}");
        let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY") else { return Ok(()) };
        let mut text = format!("### e2e {case}\n\n{line}\n\n");
        for (_, _, tests) in &self.phases {
            for test in tests.iter().flat_map(|tests| &tests.failed) {
                let _written = writeln!(text, "- ✘ `{test}`");
            }
        }
        text.push('\n');
        let mut file = std::fs::OpenOptions::new().append(true).create(true).open(summary)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        Ok(())
    }
}

/// Build every binary the suites spawn ([`BINS`]: the daemons, the CLI, the app with its `e2e`
/// feature, the e2e crate's helpers and the test kit's stand-ins) and the test targets `tests`
/// of `packages` (beside [`PACKAGES`]), in one cargo call over [`selection`]. The test targets
/// are in it for more than their binaries: a call that builds a test resolves the selected
/// packages' dev-dependencies too, so this one and the list or archive after it resolve alike,
/// and the second compiles nothing. `args` go to cargo, such as a `--target-dir`.
pub fn build(sh: &Shell, packages: &[&str], tests: &[&str], args: &[&str]) -> Result<()> {
    let selection = selection(packages);
    let bins: Vec<&str> = BINS.iter().flat_map(|bin| ["--bin", bin]).collect();
    let tests: Vec<&str> = tests.iter().flat_map(|test| ["--test", test]).collect();
    step(
        "build daemons and app",
        &cmd!(sh, "cargo build {selection...} {bins...} {tests...} {args...}"),
    )
}

/// The filtersets the app case runs with `filter`, in turn, as [`run`] composes them: first the
/// tests that code no video, then (`true`) those that do, behind the encoder probe; neither
/// captures the screen (that needs the Screen Recording grant).
pub fn app_filtersets(filter: Option<&str>) -> Vec<(String, bool)> {
    APP.iter().map(|suite| (filterset(suite, filter, false), suite.codes)).collect()
}

/// The filterset the simulator case runs with `filter`, as [`run`] composes it.
pub fn ios_filterset(filter: Option<&str>) -> String {
    let [suite] = &IOS;
    filterset(suite, filter, false)
}

/// The filterset the server case runs with `filter`, as [`run`] composes it.
pub fn server_filterset(filter: Option<&str>) -> String {
    let [suite] = &SERVER;
    filterset(suite, filter, false)
}

/// The environment the server case's suite runs in: the app's, but for the `VideoToolbox`
/// refusal, which its suite does not take.
pub fn server_env(data_dir: &str, bin_dir: &str, accept: Option<&str>) -> Vec<(String, String)> {
    app_env(data_dir, bin_dir, accept).into_iter().filter(|(k, _)| k != NO_VIDEOTOOLBOX).collect()
}

/// The environment the simulator case's suite runs in: the app's, but for the `VideoToolbox`
/// refusal, which its suite does not take, and with the booted simulator `udid` the app runs in.
pub fn ios_env(
    data_dir: &str,
    bin_dir: &str,
    accept: Option<&str>,
    udid: &str,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = app_env(data_dir, bin_dir, accept)
        .into_iter()
        .filter(|(k, _)| k != NO_VIDEOTOOLBOX)
        .collect();
    env.push(("SLOPTY_SIM_UDID".to_owned(), udid.to_owned()));
    env.push(("SLOPTY_SIM_BUNDLE_ID".to_owned(), crate::ios::BUNDLE_ID.to_owned()));
    env
}

/// The environment the app's suites run in, with the data dir `data_dir` and the binaries in
/// `bin_dir`: what [`run`] sets for them.
pub fn app_env(data_dir: &str, bin_dir: &str, accept: Option<&str>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = [
        ("SLOPTY_DATA_DIR", data_dir.to_owned()),
        ("SLOPTY_PTYD_SOCKET", format!("{data_dir}/run/ptyd.sock")),
        ("SLOPTY_WORKER_SOCKET", format!("{data_dir}/run/worker.sock")),
        ("SLOPTY_E2E_BIN_DIR", bin_dir.to_owned()),
        ("SLOPTY_E2E_ARTIFACTS", format!("{data_dir}/artifacts")),
        ("RUST_LOG", "info".to_owned()),
        (crate::gate::BINS_FRESH, "1".to_owned()),
        (NO_VIDEOTOOLBOX, "1".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    if let Some(accept) = accept {
        env.push(("SLOPTY_E2E_ACCEPT".to_owned(), accept.to_owned()));
    }
    // `SLOPTY_E2E_A11Y=always` keeps the app's accessibility tree on for the whole run, as an
    // assistive app does, and `GPUI_VIEW_RETENTION=0` builds every view in every frame: the two
    // sort a view acting on its last build from a fault of its own (`docs/TESTING.md`, "Retained
    // frames").
    for key in ["SLOPTY_E2E_A11Y", "GPUI_VIEW_RETENTION"] {
        if let Ok(mode) = std::env::var(key) {
            env.push((key.to_owned(), mode));
        }
    }
    env
}

/// Nextest's exit code when the filters leave no test to run.
pub const NO_TESTS_RUN: i32 = 4;

/// The variable that has a process open no `VideoToolbox` session, refusing each it would
/// (`slopty_codec::NO_VIDEOTOOLBOX`).
pub const NO_VIDEOTOOLBOX: &str = "SLOPTY_NO_VIDEOTOOLBOX";

/// How long a suite runs on a runner before [`crate::watchdog`] says what it waits on: the
/// app's whole suite takes about a quarter of an hour on a green runner.
const SUITE_HANG_AFTER: std::time::Duration = std::time::Duration::from_mins(25);

/// How long the tests that code video may take together: four of them, under 20 s each on a
/// green runner, against nextest's two minutes for any one.
const CODING_DEADLINE: std::time::Duration = std::time::Duration::from_mins(6);

/// How long the encoder probe may take: one keyframe codes in milliseconds, and a cold session
/// in under a second, so only an encoder that does not answer runs this long.
const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_mins(1);

/// The probe binary (`crates/slopty-e2e/src/bin/slopty-encoder-probe.rs`).
pub const PROBE: &str = "slopty-encoder-probe";

/// How a suite's run went.
enum Ran {
    /// Nextest exited with this code (`None`: killed).
    Exited(Option<i32>),
    /// It failed, or was not run, for want of a runner's encoder: a warning, not a failure.
    Excused,
}

/// Whether this runs on CI.
fn on_ci() -> bool {
    std::env::var_os("CI").is_some()
}

/// Whether this Mac's encoder codes one keyframe in time ([`PROBE`]), run from a copy on the
/// boot volume: a session started from a binary on a volume mounted without ownership waits tens
/// of seconds on its signature check (`slopty_e2e::harness::bin_dir`).
fn encoder_answers(built: &camino::Utf8Path) -> bool {
    let copy = std::env::temp_dir().join(format!("{PROBE}-{}", std::process::id()));
    if std::fs::copy(built, &copy).is_err() {
        println!("  the encoder probe is not built at {built}");
        return false;
    }
    let answered = bounded(std::process::Command::new(&copy), PROBE_DEADLINE);
    let _gone = std::fs::remove_file(&copy);
    answered == Ended::Exited(Some(0))
}

/// Run the app's tests that code video, after the rest, and judge a failure by whether the
/// runner's encoder still answers, as the gate's VideoToolbox step does
/// (`crate::gate::videotoolbox_step`).
///
/// A hosted runner's virtual Mac forwards its encoder to the host, and at times that encoder
/// stops for good with a few dozen clients open (`docs/decisions/video.md`): every test that
/// codes then hangs, and its killed processes stall in their exit inside the driver. Run
/// 37955284234 lost its last 21 tests so, after the display stream. So the encoder is asked first,
/// and an encoder that does not answer skips these tests with a warning; the run is killed whole
/// past [`CODING_DEADLINE`]; and after a failure, an encoder that stopped answering (or said so in
/// the system log) makes it a warning, since it says nothing of the change. A failure while the
/// encoder still codes is a failure.
fn coding_step(probe: &camino::Utf8Path, command: xshell::Cmd<'_>) -> Ran {
    if !encoder_answers(probe) {
        println!(
            "::warning title=VideoToolbox::this runner's video encoder does not code one frame, \
             so the app's tests that stream were not run"
        );
        println!("  ⚠ the runner's encoder does not answer: the tests that code video are skipped");
        return Ran::Excused;
    }
    let ran = bounded(std::process::Command::from(command), CODING_DEADLINE);
    if let Ended::Exited(Some(code @ (0 | NO_TESTS_RUN))) = ran {
        return Ran::Exited(Some(code));
    }
    if ran == Ended::Late {
        println!("  the tests that code video passed {CODING_DEADLINE:?}: killed them");
    }
    if crate::watchdog::encoder_stopped() || !encoder_answers(probe) {
        println!(
            "::warning title=VideoToolbox::this runner's video encoder stopped during the app's \
             tests that stream, so they say nothing of the change"
        );
        println!("  ⚠ the runner's encoder stopped");
        return Ran::Excused;
    }
    println!("  the encoder still codes, so the failure is the tests'");
    Ran::Exited(None)
}

/// How a [`bounded`] run ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ended {
    /// With this exit code; `None` when it did not start or a signal ended it.
    Exited(Option<i32>),
    /// Past its deadline, killed with its whole group.
    Late,
}

/// Run `command` in a process group of its own, killing the whole group past `deadline`, as the
/// gate runs its VideoToolbox step (`crate::gate`'s `within`).
fn bounded(mut command: std::process::Command, deadline: std::time::Duration) -> Ended {
    use std::os::unix::process::CommandExt as _;

    command.process_group(0);
    let Ok(mut child) = command.spawn() else { return Ended::Exited(None) };
    let group = child.id();
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = done.send(child.wait());
    });
    if let Ok(status) = waited.recv_timeout(deadline) {
        return Ended::Exited(status.ok().and_then(|s| s.code()));
    }
    let _killed = std::process::Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status();
    Ended::Late
}

/// The machine as a failed suite left it, for the log: its disks, its memory, and every process
/// of the run's still alive with its state, a stuck one among them.
fn machine_state() -> String {
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program).args(args).output().map_or_else(
            |e| format!("{program}: {e}\n"),
            |out| String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let processes = run("/bin/ps", &["-axo", "pid,ppid,stat,wchan,etime,rss,command"])
        .lines()
        .enumerate()
        .filter(|(n, line)| *n == 0 || line.contains("slopty") || line.contains("VTEncoder"))
        .fold(String::new(), |mut kept, (_, line)| {
            kept.push_str(line);
            kept.push('\n');
            kept
        });
    format!(
        "── the machine after the failure\n{}{}{processes}",
        run("/bin/df", &["-h", "/", "/private/tmp"]),
        run("/usr/bin/memory_pressure", &["-Q"]),
    )
}

/// The packages beyond [`PACKAGES`] and the test targets an e2e build of `suites` selects.
fn suites_built<'a>(suites: &[&'a Suite]) -> (Vec<&'a str>, Vec<&'a str>) {
    let (mut packages, mut tests) = (Vec::new(), Vec::new());
    for suite in suites {
        if !packages.contains(&suite.package) {
            packages.push(suite.package);
        }
        if !tests.contains(&suite.test) {
            tests.push(suite.test);
        }
    }
    (packages, tests)
}

/// List `suite`'s test binary, built with the run's other `suites`, and record where it is, for
/// nextest to run without cargo. Over the build's own [`selection`], so it compiles nothing.
fn build_suite(
    sh: &Shell,
    suite: &Suite,
    suites: &[&Suite],
    binaries: &std::path::Path,
) -> Result<()> {
    let test = suite.test;
    println!("▶ build {} {test}", suite.package);
    let started = std::time::Instant::now();
    let (packages, _tests) = suites_built(suites);
    let selection = selection(&packages);
    // In the dev profile the binaries above were built in, so the test binary reuses every unit
    // they compiled: in the test profile it compiled the workspace and its dependencies again,
    // 30–100 s of every e2e run on a runner (`docs/decisions/tooling.md`, "An e2e run builds
    // once").
    let listed = cmd!(
        sh,
        "cargo nextest list --cargo-profile dev {selection...} --test {test} --list-type binaries-only --message-format json"
    )
    .read();
    let package = suite.package;
    let ok = listed.is_ok();
    println!("  {} build {package} {test} ({:.1?})", if ok { "✓" } else { "✘" }, started.elapsed());
    std::fs::write(binaries, listed?)?;
    Ok(())
}

/// The nextest filterset for a suite: its own binary (the build's selection lists every
/// package's target of the suite's name: `slopty-cli` has a `server` test too), its own filter,
/// the caller's (else the suite's default), and, without `screen_recording`, none of the tests
/// that capture.
fn filterset(suite: &Suite, caller: Option<&str>, screen_recording: bool) -> String {
    let binary = format!("binary_id({}::{})", suite.package, suite.test);
    let capture = (suite.kept == Kept::Ignored && !screen_recording)
        .then(|| format!("not {SCREEN_RECORDING}"));
    [
        Some(binary),
        suite.only.map(str::to_owned),
        caller.or(suite.default).map(str::to_owned),
        capture,
    ]
    .into_iter()
    .flatten()
    .map(|part| format!("({part})"))
    .collect::<Vec<_>>()
    .join(" & ")
}

#[cfg(test)]
mod tests {
    use super::{APP, BINS, PACKAGES, Record, Suite, Tests, filterset};

    /// The binaries of [`PACKAGES`] an e2e build leaves out, and why (`BINS`).
    const LEFT_OUT: [(&str, &str); 3] = [
        ("slopty-app", "the harness starts `slopty-app-e2e`"),
        ("slopty-probe", "a developer's measurement, which no test runs"),
        ("slopty-glass", "a developer's measurement, which no test runs"),
    ];

    /// A run's record counts every suite's tests, not its last suite's alone, leaves out
    /// nextest's setup script, names each failed test, and takes a missing or foreign report
    /// for no count rather than for none run.
    #[test]
    fn a_runs_record_counts_every_suites_tests() {
        let report = |cases: &str| {
            format!(
                "<?xml version=\"1.0\"?>\n<testsuites name=\"nextest-run\" tests=\"9\">\n\
                 <testsuite name=\"@setup-script:spawned-bins\"><testcase name=\"spawned-bins\" \
                 classname=\"@setup-script:spawned-bins\"></testcase></testsuite>\n\
                 <testsuite name=\"slopty-e2e::app\">{cases}</testsuite></testsuites>"
            )
        };
        let first = report(
            r#"<testcase name="a" classname="slopty-e2e::app" time="1.0"/>
               <testcase name="b" classname="slopty-e2e::app" time="1.0"><failure type="x"/>
               </testcase>
               <testcase name="c" classname="slopty-e2e::app" time="1.0">
               <flakyFailure/></testcase>"#,
        );
        let second = report(r#"<testcase name="d" classname="slopty-e2e::app" time="1.0"/>"#);
        let first = Tests::of(&first).unwrap();
        assert_eq!(first.run, 3, "the setup script is no test");
        assert_eq!(first.failed, ["slopty-e2e::app b"], "a flaky pass is no failure");
        assert_eq!(Tests::of("<html></html>"), None, "a foreign file counts nothing");

        let mut record = Record::default();
        record.phase("build", std::time::Duration::from_secs(14), None);
        record.phase("app", std::time::Duration::from_millis(211_200), Some(first));
        record.phase("app codes", std::time::Duration::from_millis(10_500), Tests::of(&second));
        record.phase("none ran", std::time::Duration::from_millis(100), None);
        assert_eq!(
            record.line(),
            "build 14.0s; app 211.2s, 3 tests; app codes 10.5s, 1 tests; none ran 0.1s \
             (235.8s): 4 tests, 1 failed"
        );
    }

    /// The guest runs both of the app's groups, the one that codes last and on its own, each
    /// in the app's own binary.
    #[test]
    fn the_guest_runs_the_tests_that_code_after_the_rest() {
        let groups = super::app_filtersets(Some("test(a)"));
        let [(rest, false), (coding, true)] = groups.as_slice() else { panic!("{groups:?}") };
        assert!(
            rest.starts_with("(binary_id(slopty-e2e::app)) & (not ")
                && rest.ends_with("& (test(a)) & (not test(~screen_recording::))"),
            "{rest}"
        );
        assert!(coding.contains("test(~stream::)") && !coding.contains("(not (test"), "{coding}");
    }

    /// The app's tests run in two groups that split the target between them on the same list of
    /// tests that code video: first the rest, opening no decoder at launch, then those, behind
    /// the probe.
    #[test]
    fn the_app_runs_its_tests_that_code_video_last_and_apart() {
        let [rest, coding] = APP else { panic!("two groups") };
        assert!(rest.no_videotoolbox && !rest.codes);
        assert!(coding.codes && !coding.no_videotoolbox);
        let (rest, coding) = (rest.only.unwrap_or_default(), coding.only.unwrap_or_default());
        assert_eq!(rest.strip_prefix("not "), Some(coding), "the same list, once negated");
        assert!(coding.contains("test(~screen_recording::)"), "{coding}");
        assert!(coding.contains("test(~stream::)"), "a drawn screen's stream codes too: {coding}");
    }

    #[test]
    fn a_suites_filters_the_callers_and_the_capture_rule_all_apply() {
        let pair = Suite::live("slopty-e2e", "pair").only("test(~on_the_mac)");
        assert_eq!(
            filterset(&pair, Some("test(pans) | test(zooms)"), false),
            "(binary_id(slopty-e2e::pair)) & (test(~on_the_mac)) & (test(pans) | test(zooms)) & \
             (not test(~screen_recording::))"
        );
        assert_eq!(
            filterset(&pair, None, true),
            "(binary_id(slopty-e2e::pair)) & (test(~on_the_mac))"
        );

        let worker = Suite { default: Some("test(~screen_stream)"), ..Suite::live("w", "e2e") };
        assert_eq!(
            filterset(&worker, None, true),
            "(binary_id(w::e2e)) & (test(~screen_stream))",
            "the default stands in for no caller filter"
        );
        assert_eq!(
            filterset(&worker, Some("test(x)"), true),
            "(binary_id(w::e2e)) & (test(x))",
            "and gives way to one"
        );

        let gated = Suite::env("SLOPTY_INPUT_E2E", "slopty-input", "inject");
        assert_eq!(
            filterset(&gated, None, false),
            "(binary_id(slopty-input::inject))",
            "a suite kept by a variable has no capture module"
        );
    }

    /// Every cargo call of an e2e build selects the same packages and features, the workspace
    /// hack among them, so the binaries' build and the test binary's archive resolve alike: a
    /// suite's own package joins the selection, never replaces it.
    #[test]
    fn the_build_and_the_archive_select_one_feature_set() {
        let plain = super::selection(&[]);
        assert!(plain.windows(2).any(|w| w == ["-p", "workspace-hack"]), "{plain:?}");
        for feature in super::FEATURES {
            assert!(plain.windows(2).any(|w| w == ["--features", feature]), "{plain:?}");
        }
        let with = super::selection(&["slopty-e2e", "slopty-capture"]);
        let packages = 2 * PACKAGES.len();
        assert_eq!(with[..packages], plain[..packages], "the same packages first");
        assert_eq!(with[packages..packages + 2], ["-p", "slopty-capture"], "and the suite's");
        assert_eq!(with[packages + 2..], plain[packages..], "the same features and triple");
    }

    /// The e2e build names its binaries rather than taking every one: each binary of its
    /// packages is either built ([`BINS`]) or left out by name ([`LEFT_OUT`]), so a new binary a
    /// test may spawn cannot go missing unseen, and the plain app stays out.
    #[test]
    fn every_binary_of_the_e2e_packages_is_built_or_left_out_by_name() {
        let root = crate::tools::repo_root().expect("root");
        let packages = crate::tools::packages_in(&root).expect("packages");
        let mut found = Vec::new();
        for name in PACKAGES.iter().filter(|p| **p != crate::tools::WORKSPACE_HACK) {
            let package = packages.iter().find(|p| p.name == *name).expect("a package");
            found.extend(bins_of(&package.dir, name));
        }
        found.sort();
        let mut named: Vec<String> = BINS
            .iter()
            .chain(LEFT_OUT.iter().map(|(bin, _)| bin))
            .map(|b| (*b).to_owned())
            .collect();
        named.sort();
        assert_eq!(found, named, "the e2e packages' binaries, against BINS and LEFT_OUT");
        assert!(!BINS.contains(&"slopty-app") && BINS.contains(&crate::e2e::PROBE));
    }

    /// The binaries cargo makes of the package `name` in `dir`: its `[[bin]]` targets and, unless
    /// `autobins = false`, `src/main.rs` and `src/bin/` that none of them claims.
    fn bins_of(dir: &camino::Utf8Path, name: &str) -> Vec<String> {
        let text = std::fs::read_to_string(dir.join("Cargo.toml")).expect("manifest");
        let manifest: toml::Table = text.parse().expect("toml");
        let mut bins = Vec::new();
        let mut claimed = Vec::new();
        for bin in manifest.get("bin").and_then(toml::Value::as_array).into_iter().flatten() {
            let bin_name = bin.get("name").and_then(toml::Value::as_str).expect("a bin's name");
            let path = bin
                .get("path")
                .and_then(toml::Value::as_str)
                .map_or_else(|| format!("src/bin/{bin_name}.rs"), str::to_owned);
            bins.push(bin_name.to_owned());
            claimed.push(path);
        }
        let auto = manifest
            .get("package")
            .and_then(|p| p.get("autobins"))
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if !auto {
            return bins;
        }
        if dir.join("src/main.rs").exists() && !claimed.iter().any(|c| c == "src/main.rs") {
            bins.push(name.to_owned());
        }
        for entry in std::fs::read_dir(dir.join("src/bin")).into_iter().flatten().flatten() {
            let path = entry.path();
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
            let relative = if path.is_dir() {
                format!("src/bin/{stem}/main.rs")
            } else {
                format!("src/bin/{stem}.rs")
            };
            if !claimed.contains(&relative) && !bins.contains(&stem) {
                bins.push(stem);
            }
        }
        bins
    }
}
