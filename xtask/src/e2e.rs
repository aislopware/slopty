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

pub mod hosted;

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
    /// The app's cases and the server's, from one build: what the gate's `e2e (mac)` job runs.
    Mac,
    /// All of the above but the simulator case (`ios`).
    All,
    /// ptyd + worker on the Mac and the iOS app in the simulator (`--sim iphone|ipad`, either or
    /// both), driven through its test socket: add the worker, open a shell, type, read the rows
    /// back, render frames against the per-device goldens (`ios-phone-*`, `ios-pad-*`).
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
    /// The simulators the `ios` case runs on, one after the other (`--sim iphone --sim ipad`),
    /// from one build: the first boots while it builds, the next once the one before tested. The
    /// iPhone when none is named. The iPad runs one test alone, whatever `-E` says
    /// ([`IPAD_LIGHT`]).
    #[arg(long, value_enum)]
    sim: Vec<crate::ios::SimKind>,
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
    /// On GitHub Actions, stop once another job of this run has ended other than green (failed,
    /// past its timeout or cancelled), or, before the build, once `gate` has moved off the run's
    /// commit (`crate::gate::order`): the run never promotes then, so a smoke job gives its Mac
    /// back (`ci.yml`'s `e2e (mac)` and `e2e (ios)`).
    #[arg(long)]
    stop_on_red: bool,
    /// On GitHub's hosted Macs, not here: `e2e.yml` dispatched on `gate` for `--sha`, followed to
    /// its end, its artifacts brought to `target/e2e/hosted/` ([`hosted`]). `mac` runs the app's
    /// and the server's suites, `ios` the simulator's on the iPhone and the iPad's one test; `-E`
    /// narrows either, and `--accept` or `--accept-all` leaves a patch of the accepted goldens
    /// there.
    #[arg(
        long,
        conflicts_with_all = ["no_build", "data_dir", "test_threads", "stop_on_red", "sim", "screen_recording"]
    )]
    hosted: bool,
    /// With `--hosted`: the commit, which must be on GitHub (`HEAD` when left out).
    #[arg(long, requires = "hosted")]
    sha: Option<String>,
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

/// What the iPad's pass of the simulator case runs, whatever the filter: the app launches there
/// and draws its layout, the sidebar beside the session, against its goldens. The person does
/// not use the iPad, so it gets this light smoke check, and the iPhone the whole case
/// (`docs/decisions/testing.md`, "The gate runs the smoke cases a push reaches").
const IPAD_LIGHT: &str = "test(=compact::a_phone_opens_on_its_chats_and_one_takes_the_screen)";

const SERVER: [Suite; 1] = [Suite::live("slopty-e2e", "server")];

pub fn run(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    if opts.hosted {
        return run_hosted(sh, opts);
    }
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
        Case::Mac => APP.iter().chain(&SERVER).collect(),
        Case::All => APP.iter().chain(WORKER).chain(&SERVER).chain(SCREEN).chain(INPUT).collect(),
        Case::Ios => IOS.iter().collect(),
    };
    let data_dir = opts
        .data_dir
        .clone()
        .unwrap_or_else(|| sh.current_dir().join("target/e2e").to_string_lossy().into_owned());
    std::fs::create_dir_all(format!("{data_dir}/run"))?;
    println!("▶ e2e {:?} (data dir {data_dir})", opts.case);
    if opts.stop_on_red {
        stop_on_red()?;
    }
    // The simulators the case runs on, the first booting while the build runs.
    let mut sims: Vec<crate::ios::SimKind> = Vec::new();
    if matches!(opts.case, Case::Ios) {
        for sim in if opts.sim.is_empty() { &[crate::ios::SimKind::Iphone][..] } else { &opts.sim }
        {
            if !sims.contains(sim) {
                sims.push(*sim);
            }
        }
    }
    let mut booting = Booting(sims.first().map(|sim| boot_in_background(sh, *sim)));

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

    // The iOS case's app, built once for every simulator it runs on.
    let app = if sims.is_empty() {
        None
    } else {
        let started = std::time::Instant::now();
        let first = sims.first().copied().unwrap_or_default();
        let built = crate::ios::build_for_simulator(sh, first, &opts.log)?;
        record.phase("simulator app", started.elapsed(), None);
        Some(built.app)
    };
    let mut failed = Vec::new();
    // The suites whose build failed, which a later simulator's pass skips.
    let mut unbuilt = Vec::new();
    let mut matched = 0_usize;
    // One pass over the suites, or one per simulator: each boots its simulator (the first's
    // boot began beside the build), installs the app, waits for it to settle, runs the suites
    // against it and shuts it down. A later simulator boots only once the last one's tests are
    // done: a boot takes a core and about 2 GB for minutes, and beside the timed tests of a
    // three-core runner with no retries it could fail them on time.
    let passes: Vec<Option<crate::ios::SimKind>> =
        if sims.is_empty() { vec![None] } else { sims.iter().copied().map(Some).collect() };
    for (pass, sim) in passes.iter().enumerate() {
        hold_if_stopping();
        let device = sim.map(|sim| {
            let family = sim.family();
            (|| -> Result<_> {
                let started = std::time::Instant::now();
                let udid = match booting.0.take() {
                    Some(boot) => {
                        let (udid, booted) = boot
                            .join()
                            .map_err(|_panic| anyhow::anyhow!("the {family} boot panicked"))??;
                        println!("  the {family} booted in {booted:.1?}, beside the build");
                        udid
                    }
                    None => crate::ios::boot_simulator(sh, sim)?,
                };
                record.phase(&format!("simulator {family} boot"), started.elapsed(), None);
                if let Some(app) = &app {
                    let started = std::time::Instant::now();
                    crate::ios::install(sh, &udid, app)?;
                    record.phase(&format!("simulator {family} install"), started.elapsed(), None);
                }
                if on_ci() {
                    let started = std::time::Instant::now();
                    match settle(sh) {
                        Some(after) => println!("  the {family} settled in {after:.0?}"),
                        None => println!(
                            "  the {family} did not settle within {SETTLE_MOST:?}: its tests run \
                             beside its daemons"
                        ),
                    }
                    record.phase(&format!("simulator {family} settle"), started.elapsed(), None);
                }
                Ok((
                    sh.push_env("SLOPTY_SIM_UDID", udid.clone()),
                    sh.push_env("SLOPTY_SIM_BUNDLE_ID", crate::ios::BUNDLE_ID),
                    udid,
                ))
            })()
        });
        let device = match device.transpose() {
            Ok(device) => device,
            Err(e) => {
                let family = sim.map_or("", crate::ios::SimKind::family);
                println!("  ✘ simulator {family}: {e:#}");
                failed.push(format!("simulator {family}"));
                continue;
            }
        };
        let on = sim.map_or_else(String::new, |sim| format!(" ({})", sim.family()));
        for suite in &suites {
            hold_if_stopping();
            let _gate = match suite.kept {
                Kept::Env(var) => Some(sh.push_env(var, "1")),
                Kept::No | Kept::Ignored => None,
            };
            let title =
                format!("{} {} {}", suite.package, suite.test, suite.only.unwrap_or_default());
            let title = format!("{}{on}", title.trim());
            let title = title.as_str();
            let binaries = recorded(suite);
            // Built once: a later simulator's pass runs the same test binary.
            if unbuilt.contains(&binaries) {
                continue;
            }
            if !opts.no_build && pass == 0 && build_suite(sh, suite, &suites, &binaries).is_err() {
                failed.push(title.to_owned());
                unbuilt.push(binaries);
                continue;
            }
            let caller = if *sim == Some(crate::ios::SimKind::Ipad) {
                Some(IPAD_LIGHT)
            } else {
                opts.filter.as_deref()
            };
            let expr = filterset(suite, caller, opts.screen_recording);
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
                Ran::Exited(Some(0)) => {
                    matched = matched.saturating_add(1);
                    "✓"
                }
                // Green, as the runner's encoder says nothing of the change, but said as not run.
                Ran::Excused => {
                    matched = matched.saturating_add(1);
                    record.excused.push(title.to_owned());
                    "⚠ excused"
                }
                // Nextest's "no tests to run": the filter picked nothing in this suite, or every
                // test it holds captures the screen and this run has no `--screen-recording`.
                Ran::Exited(Some(NO_TESTS_RUN))
                    if opts.filter.is_some() || !opts.screen_recording =>
                {
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
        if let Some((_, _, udid)) = &device {
            // A simulator left booted keeps CoreAudio busy long after: the client's audio player
            // then takes tens of seconds to open and the screen worker's test times out in the
            // next gate (gate 307, 2026-09-15).
            cmd!(sh, "xcrun simctl shutdown {udid}").ignore_status().quiet().run()?;
        }
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

/// `xtask e2e <mac|ios> --hosted`: the case on a hosted Mac, its verdict and its patches.
fn run_hosted(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    let kinds: &[hosted::Kind] = match opts.case {
        Case::Mac => &[hosted::Kind::Mac],
        Case::Ios => &[hosted::Kind::Ios],
        other => bail!(
            "--hosted runs `mac` (the app's and the server's suites) or `ios` (the simulator's), \
             each narrowed by -E; not `{other:?}`"
        ),
    };
    let wanted = format!("{}^{{commit}}", opts.sha.as_deref().unwrap_or("HEAD"));
    let sha = cmd!(sh, "git rev-parse --verify {wanted}").quiet().read()?;
    let goldens = if opts.accept_all {
        hosted::Goldens::AcceptAll
    } else if opts.accept {
        hosted::Goldens::Accept
    } else {
        hosted::Goldens::Review
    };
    let request = hosted::Request { sha: &sha, kinds, filter: opts.filter.as_deref(), goldens };
    let into = camino::Utf8PathBuf::try_from(sh.current_dir().join("target/e2e/hosted"))?;
    let verdict = hosted::run(sh, &request, &into)?;
    for (job, done) in &verdict.jobs {
        println!("  {} {job} {done}", if done == "success" { "✓" } else { "✘" });
    }
    for patch in verdict.patches() {
        println!("  accepted goldens: {patch} (`git apply --binary {patch}`)");
    }
    println!("  artifacts: {}", verdict.artifacts);
    hosted::passed(&verdict)
}

/// A simulator booting in the background ([`boot_in_background`]): one the run ends before it
/// uses is shut down once booted, as every simulator a run boots is.
struct Booting(Option<std::thread::JoinHandle<Result<(String, std::time::Duration)>>>);

impl Drop for Booting {
    fn drop(&mut self) {
        if let Some(boot) = self.0.take()
            && let Ok(Ok((udid, _))) = boot.join()
            && let Err(e) =
                std::process::Command::new("xcrun").args(["simctl", "shutdown", &udid]).status()
        {
            println!("  the simulator {udid} stays booted: {e}");
        }
    }
}

/// The simulator `kind` booted, created on first use, on a thread of its own while the caller
/// builds ([`crate::ios::boot_simulator`]); its UDID and how long the boot took, once joined.
fn boot_in_background(
    sh: &Shell,
    kind: crate::ios::SimKind,
) -> std::thread::JoinHandle<Result<(String, std::time::Duration)>> {
    let dir = sh.current_dir();
    std::thread::spawn(move || {
        let sh = Shell::new()?;
        sh.change_dir(dir);
        let started = std::time::Instant::now();
        let udid = crate::ios::boot_simulator(&sh, kind)?;
        Ok((udid, started.elapsed()))
    })
}

/// The longest a run waits for its simulator to settle ([`settle`]): in a guest on this Mac
/// (2026-10, since removed) a clone of a settled simulator took 132-138 s, and a first boot's
/// work 18-20 minutes (`docs/decisions/testing.md`, "An iOS run starts once its simulator has
/// settled").
const SETTLE_MOST: std::time::Duration = std::time::Duration::from_mins(10);

/// How long the machine took to settle once a simulator booted and took the app: until its CPU
/// is half idle or more on two 4 s samples of `top` in a row. A booted simulator's daemons
/// (`SetStoreUpdateService`, `MTLCompilerService`, `photolibraryd`) held every core of a guest
/// on this Mac (2026-10, since removed) for minutes after `bootstatus`, and tests run beside them
/// failed on time. `None` when [`SETTLE_MOST`] passed first, or `top` could not run.
fn settle(sh: &Shell) -> Option<std::time::Duration> {
    let started = std::time::Instant::now();
    let mut calm = 0_u8;
    while started.elapsed() < SETTLE_MOST {
        let top = cmd!(sh, "top -l 2 -s 4 -n 0").quiet().ignore_stderr().read().ok()?;
        if idle_share(&top).is_some_and(|idle| idle >= 50.0) {
            calm = calm.saturating_add(1);
            if calm == 2 {
                return Some(started.elapsed());
            }
        } else {
            calm = 0;
        }
    }
    None
}

/// The CPU's idle share in `top -l 2`'s second sample (its first counts since boot), from its
/// `CPU usage: 5.2% user, 10.5% sys, 84.2% idle` line.
fn idle_share(top: &str) -> Option<f64> {
    let usage = top.lines().filter(|line| line.starts_with("CPU usage:")).nth(1)?;
    usage.split(',').find_map(|part| part.trim().strip_suffix("% idle")?.trim().parse().ok())
}

/// Start the watch that ends the run once another job of its workflow run has ended red
/// ([`E2eOpts::stop_on_red`]): it looks at once, then every [`WATCH_EVERY`], through `gh` and the
/// job's token.
fn stop_on_red() -> Result<()> {
    let (Ok(repo), Ok(run), Ok(sha)) = (
        std::env::var("GITHUB_REPOSITORY"),
        std::env::var("GITHUB_RUN_ID"),
        std::env::var("GITHUB_SHA"),
    ) else {
        bail!("--stop-on-red watches a GitHub Actions run, and this is none");
    };
    let _watch = std::thread::spawn(move || {
        watch_run(&repo, &run, &sha);
    });
    Ok(())
}

/// How often [`watch_run`] looks: GitHub caches the jobs' listing for 60 s
/// (`Cache-Control: max-age=60`), so a shorter wait sees nothing newer. A run's jobs share the
/// repository's 1,000 requests an hour with binstall's lookups and `promote`'s, and a look that
/// finds nothing changed (`304`) counts against none of them.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_mins(1);

/// How long the processes under the run have to end once it stops ([`end_children`]): nextest
/// gives its tests 10 s past a `SIGTERM` before it kills them.
const ENDING: std::time::Duration = std::time::Duration::from_secs(20);

/// Set once [`watch_run`] has found the run red: no suite starts after.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Wait here for good once the run is stopping, so no suite starts while [`watch_run`] ends the
/// ones running and then the process.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; held for its end")]
fn hold_if_stopping() {
    while STOPPING.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Look at the run's jobs until another has ended other than green ([`ended_jobs`]), then end
/// the processes under this one ([`end_children`]) and the process: the run never promotes, and
/// the cases still to come would hold a Mac for nothing. The first look, before the build, asks
/// first whether the run stands aside for a newer one on `gate` ([`crate::gate::order`]). A run
/// that stands aside, or whose jobs were only cancelled, is cancelled whole first. Each look
/// after the first is conditional on the last listing's `ETag`; one that fails is tried again
/// at the next.
#[expect(clippy::exit, reason = "the watch ends the whole run, its build and tests with it")]
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling GitHub")]
fn watch_run(repo: &str, run: &str, sha: &str) -> ! {
    let jobs = format!("repos/{repo}/actions/runs/{run}/jobs?per_page=100");
    let checks = format!("repos/{repo}");
    let mut etag: Option<String> = None;
    let mut first = true;
    loop {
        let looking_first = first;
        if !first {
            std::thread::sleep(WATCH_EVERY);
        }
        first = false;
        let Ok(sh) = Shell::new() else { continue };
        let aside = looking_first
            .then(|| crate::gate::order::tip(&sh))
            .flatten()
            .filter(|tip| crate::gate::order::stands_aside(Some(tip), sha));
        let stop = if let Some(tip) = aside {
            Stop::Aside(tip)
        } else {
            let header = etag.as_ref().map(|tag| format!("If-None-Match: {tag}"));
            let header: Vec<&str> = header.iter().flat_map(|h| ["-H", h.as_str()]).collect();
            // `gh` exits 1 on a `304`; the status line says what came.
            let looked =
                cmd!(sh, "gh api -i {header...} {jobs}").quiet().ignore_status().ignore_stderr();
            let Ok(output) = looked.output() else { continue };
            let text = String::from_utf8_lossy(&output.stdout);
            let Some(answer) = Answer::read(&text) else { continue };
            if answer.status != 200 {
                continue;
            }
            etag = answer.etag.map(str::to_owned);
            let timed_out = |job| crate::gate::order::timed_out(&sh, &checks, job);
            let Some(stop) = ended_jobs(answer.body, timed_out) else { continue };
            stop
        };
        STOPPING.store(true, std::sync::atomic::Ordering::Release);
        let said = match &stop {
            Stop::Red(job) => format!("{job}: the run never promotes, so its e2e stops here"),
            Stop::Cancelled(job) => {
                format!("{job}: the run is being cancelled, so its e2e stops here")
            }
            Stop::Aside(tip) => format!(
                "`gate` moved on to {tip} before this e2e built, and that run carries this commit \
                 or a later land withdrew it: the run stands aside, cancelled whole"
            ),
        };
        println!("✋ {said}");
        if let Some(summary) = std::env::var_os("GITHUB_STEP_SUMMARY")
            && let Ok(mut file) =
                std::fs::OpenOptions::new().append(true).create(true).open(summary)
        {
            let _written =
                std::io::Write::write_all(&mut file, format!("### ✋ {said}\n\n").as_bytes());
        }
        // A run that stands aside, or whose jobs were only cancelled, ends cancelled, not red, so
        // `land --wait` follows the newer run that carries its commit. The runner then cancels
        // this job in seconds and ends its processes itself; ending them first would end the
        // step red before it.
        if matches!(stop, Stop::Cancelled(_) | Stop::Aside(_))
            && cmd!(sh, "gh run cancel {run} --repo {repo}").quiet().run().is_ok()
        {
            std::thread::sleep(CANCEL_ARRIVES);
        }
        end_children();
        std::process::exit(1);
    }
}

/// How long a job waits for the runner to cancel it once its run is cancelled: seconds, as a
/// rule; past this it ends red instead.
const CANCEL_ARRIVES: std::time::Duration = std::time::Duration::from_mins(5);

/// An HTTP answer as `gh api -i` prints it: the status line, the headers, a blank line and the
/// body.
#[derive(Debug, PartialEq, Eq)]
struct Answer<'a> {
    status: u16,
    etag: Option<&'a str>,
    body: &'a str,
}

impl<'a> Answer<'a> {
    /// The answer `text` holds, or `None` when it starts with no status line.
    fn read(text: &'a str) -> Option<Self> {
        let (head, body) =
            text.split_once("\r\n\r\n").or_else(|| text.split_once("\n\n")).unwrap_or((text, ""));
        let mut lines = head.lines();
        let status =
            lines.next()?.strip_prefix("HTTP/")?.split_whitespace().nth(1)?.parse().ok()?;
        let etag = lines.find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim().eq_ignore_ascii_case("etag").then(|| value.trim())
        });
        Some(Self { status, etag, body })
    }
}

/// Why a run's e2e stops ([`ended_jobs`]).
#[derive(Debug, PartialEq, Eq)]
enum Stop {
    /// A job failed or ran past its timeout (or ended any other way but cancelled): the run is
    /// red. The first such job, with how it ended.
    Red(String),
    /// Jobs were cancelled and none went red: the run is being cancelled whole, by a job of it
    /// that stood aside, a red one, or someone. The first, named.
    Cancelled(String),
    /// `gate` has moved on to this commit before the e2e built (`crate::gate::order`).
    Aside(String),
}

/// Whether a run's jobs listing (`body`) has a job that ended other than green or skipped: one
/// that did keeps the run from promoting, a sibling e2e job's as much as a gate lane's. One
/// still running has no conclusion. A cancelled job is red when `timed_out` says, by its id,
/// that it ran past its timeout.
fn ended_jobs(body: &str, timed_out: impl Fn(u64) -> bool) -> Option<Stop> {
    use crate::gate::order::{End, end};
    #[derive(serde::Deserialize)]
    struct Jobs {
        jobs: Vec<Job>,
    }
    #[derive(serde::Deserialize)]
    struct Job {
        id: u64,
        name: String,
        conclusion: Option<String>,
    }
    let listed: Jobs = serde_json::from_str(body).ok()?;
    let mut cancelled = None;
    for job in listed.jobs {
        let ended = job.conclusion.as_deref();
        match end(ended, || timed_out(job.id)) {
            End::Running | End::Green => {}
            End::Red if ended == Some("cancelled") => {
                return Some(Stop::Red(format!("{} ran past its timeout", job.name)));
            }
            End::Red => {
                return Some(Stop::Red(format!("{} ended {}", job.name, ended.unwrap_or("?"))));
            }
            End::Cancelled => {
                cancelled.get_or_insert_with(|| format!("{} ended cancelled", job.name));
            }
        }
    }
    cancelled.map(Stop::Cancelled)
}

/// Send `SIGTERM` to every process under this one, so nextest ends its tests and cargo its
/// compilers, and wait up to [`ENDING`] for them to go: the runner's own cleanup comes only at
/// the job's end, after its cache steps.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; waiting on ps")]
fn end_children() {
    let me = std::process::id();
    let under = || {
        let Ok(ps) = std::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid="])
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            return Vec::new();
        };
        let lister = ps.id();
        let listed = ps.wait_with_output().map(|out| out.stdout).unwrap_or_default();
        let mut pids = descendants(&String::from_utf8_lossy(&listed), me);
        pids.retain(|pid| *pid != lister);
        pids
    };
    for pid in under() {
        if let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
            let _sent = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
    }
    let started = std::time::Instant::now();
    while started.elapsed() < ENDING && !under().is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// The processes whose parent chain reaches `root` in `ps -o pid=,ppid=`'s `listing`.
fn descendants(listing: &str, root: u32) -> Vec<u32> {
    let pairs: Vec<(u32, u32)> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut under = Vec::new();
    let mut todo = vec![root];
    while let Some(parent) = todo.pop() {
        for &(pid, ppid) in &pairs {
            if ppid == parent && pid != root && !under.contains(&pid) {
                under.push(pid);
                todo.push(pid);
            }
        }
    }
    under
}

/// What a run cost and ran: each phase's time (the build, the simulator, every suite) and each
/// suite's tests as nextest's `JUnit` report counts them, summed over the run. A run of several
/// suites otherwise reads, in its last `Summary` line, as its last suite's tests alone.
#[derive(Default)]
struct Record {
    phases: Vec<(String, std::time::Duration, Option<Tests>)>,
    /// The suites that did not run, or whose failure did not count, for want of the runner's
    /// encoder ([`Ran::Excused`]).
    excused: Vec<String>,
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
        let excused = match self.excused.len() {
            0 => String::new(),
            n => format!(", {n} suite(s) excused"),
        };
        format!(
            "{} ({:.1}s): {} tests, {} failed{excused}",
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
        for suite in &self.excused {
            let _written =
                writeln!(text, "- ⚠ `{suite}` did not run: the runner's video encoder stopped");
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
    use super::{
        APP, Answer, BINS, PACKAGES, Record, Stop, Suite, Tests, descendants, ended_jobs,
        filterset, idle_share,
    };

    /// The iPad's one test is one of the simulator case's, so a rename cannot leave the iPad
    /// running none.
    #[test]
    fn the_ipad_s_light_check_names_a_test_of_the_simulator_case() {
        let name = super::IPAD_LIGHT.strip_prefix("test(=").and_then(|t| t.strip_suffix(')'));
        let (module, test) = name.and_then(|n| n.split_once("::")).expect("a module's test");
        let root = crate::tools::repo_root().expect("repo root");
        let path = root.join(format!("crates/slopty-e2e/tests/ios/{module}.rs"));
        let source = std::fs::read_to_string(&path).expect("its module");
        assert!(source.contains(&format!("async fn {test}()")), "{test} in {path}");
    }

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
        record.excused.push("slopty-e2e app codes".to_owned());
        assert!(
            record.line().ends_with("4 tests, 1 failed, 1 suite(s) excused"),
            "{}",
            record.line()
        );
    }

    /// A look at the run's jobs reads `gh api -i`'s answer whole: a `200` with its `ETag` and
    /// body, a `304` with no body, and nothing from text with no status line.
    #[test]
    fn a_look_reads_the_status_the_etag_and_the_body() {
        let fresh = "HTTP/2.0 200 OK\r\nCache-Control: private, max-age=60\r\n\
                     Etag: W/\"86c6\"\r\nX-Ratelimit-Remaining: 4941\r\n\r\n{\"jobs\":[]}";
        assert_eq!(
            Answer::read(fresh),
            Some(Answer { status: 200, etag: Some("W/\"86c6\""), body: "{\"jobs\":[]}" })
        );
        let same = "HTTP/2.0 304 Not Modified\r\nEtag: W/\"86c6\"\r\n\r\n";
        assert_eq!(Answer::read(same).map(|answer| (answer.status, answer.body)), Some((304, "")));
        let bare = "HTTP/1.1 502 Bad Gateway\nServer: x\n";
        assert_eq!(Answer::read(bare), Some(Answer { status: 502, etag: None, body: "" }));
        assert_eq!(Answer::read("gh: HTTP 304"), None);
        assert_eq!(Answer::read(""), None);
    }

    /// Any job that ended other than green or skipped stops the run's e2e, a sibling e2e job's
    /// and a lane past its timeout as much as a failed gate lane's; a running one does not. One
    /// that was only cancelled (its run is being cancelled whole) is told apart, so the run ends
    /// cancelled rather than red; one GitHub calls cancelled for running past its timeout is red.
    #[test]
    fn a_job_that_ended_neither_green_nor_skipped_stops_the_run() {
        let jobs = |conclusions: &[(&str, Option<&str>)]| {
            let jobs: Vec<String> = conclusions
                .iter()
                .enumerate()
                .map(|(id, (name, ended))| {
                    let ended = ended.map_or_else(|| "null".to_owned(), |e| format!("\"{e}\""));
                    format!(r#"{{"id":{id},"name":"{name}","status":"x","conclusion":{ended}}}"#)
                })
                .collect();
            format!(r#"{{"total_count":{},"jobs":[{}]}}"#, jobs.len(), jobs.join(","))
        };
        let green = [
            ("gate (ui)", Some("success")),
            ("release v1", Some("skipped")),
            ("e2e (ios)", None),
            ("promote", None),
        ];
        let never = |_| false;
        assert_eq!(ended_jobs(&jobs(&green), never), None);
        for ended in ["failure", "timed_out", "startup_failure"] {
            let mut run = green.to_vec();
            run.push(("e2e (mac)", Some("cancelled")));
            run.push(("gate (tools)", Some(ended)));
            assert_eq!(
                ended_jobs(&jobs(&run), never),
                Some(Stop::Red(format!("gate (tools) ended {ended}"))),
                "a red counts before a cancel"
            );
        }
        let mut cancelled = green.to_vec();
        cancelled.push(("e2e (mac)", Some("cancelled")));
        assert_eq!(
            ended_jobs(&jobs(&cancelled), never),
            Some(Stop::Cancelled("e2e (mac) ended cancelled".to_owned()))
        );
        assert_eq!(
            ended_jobs(&jobs(&cancelled), |job| job == 4),
            Some(Stop::Red("e2e (mac) ran past its timeout".to_owned())),
            "GitHub lists a job past its timeout as cancelled"
        );
        assert_eq!(ended_jobs("not json", never), None, "an answer it can't read stops nothing");
    }

    /// The processes the watch ends are those whose parent chain reaches the run's, at any depth,
    /// and no other.
    #[test]
    fn the_processes_under_the_run_are_its_whole_tree() {
        let listing = "    1     0\n  100     1\n  200   100\n  201   200\n  300   201\n  202   100\n\
                       \n  999     1\n garbage\n";
        let mut under = descendants(listing, 100);
        under.sort_unstable();
        assert_eq!(under, [200, 201, 202, 300]);
        assert_eq!(descendants(listing, 300), Vec::<u32>::new());
    }

    /// The settle reads `top`'s second sample, its first counting since boot.
    #[test]
    fn the_settle_reads_top_s_second_sample() {
        let top = "Processes: 512 total\nCPU usage: 90.0% user, 9.0% sys, 1.0% idle \n\
                   Processes: 513 total\nCPU usage: 20.5% user, 12.25% sys, 67.25% idle \n";
        assert_eq!(idle_share(top), Some(67.25));
        assert_eq!(idle_share("CPU usage: 1.0% user, 1.0% sys, 98.0% idle\n"), None);
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
