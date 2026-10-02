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

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::step;

/// The feature that builds `slopty-e2e`'s live test targets, which every build that runs or
/// lints them names (`crates/slopty-e2e/Cargo.toml`).
pub const LIVE: &str = "slopty-e2e/live";

/// The package whose test targets need [`LIVE`].
pub const LIVE_PACKAGE: &str = "slopty-e2e";

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
    /// Frame-time budget: 20 streaming shells panned and zoomed, a display stream beside
    /// shells (only with `--screen-recording`), typing with and without the local echo, and the
    /// conversation face under a streaming answer; prints the percentiles and fails when
    /// panning is over budget. No permissions needed for the shell scenarios.
    Smooth,
    /// Renders of every surface with a busy day's data, for a person's review: no golden is
    /// compared; the pictures land in `target/e2e/artifacts/showcase/`.
    Showcase,
    /// The same frame-time scenarios with the app in the simulator (`--sim iphone|ipad`);
    /// indicative only, the simulator has no GPU-backed display link.
    SmoothIos,
    /// Two clients on one worker: two app processes on this Mac, each on its own socket, both
    /// added to the same daemons; a terminal opened on one appears on the other, typing on both is
    /// serialised, attention badges both, a client dying leaves the other streaming, closing
    /// and notes propagate. No permissions needed (the display scenario also needs
    /// `--screen-recording`).
    Pair,
    /// The same with the second client in the simulator (`--sim iphone|ipad`): the Mac and
    /// the phone on one worker.
    PairIos,
    /// One client, two workers: ptyd + worker + the app, and a second ptyd + worker under a root
    /// of its own with a private HOME, reached through a relay shaped like the tailnet path to
    /// another Mac. Proves cross-worker attention against a real second daemon: the app adds two
    /// workers, a shell on the second round-trips over the shaped link, a permission hook played
    /// to it through `slopty hook` badges the cross-worker pill and routes a banner back to it, and
    /// killing it mid-stream shows it down while the first keeps streaming, then connected again
    /// on restart. No permissions needed.
    Workers,
    /// The server with a real worker: `slopty-server` and ptyd + worker registered with it, on
    /// ports of their own, driven through the `slopty` CLI with `--json` and MCP over HTTP:
    /// the directory, a shell typed to and read, files both ways, ports, close and exit, the
    /// lease lost on a killed worker and taken back. No permissions needed.
    Server,
    /// The app as it is normally used: at its first run it is pointed at a `slopty-server` with
    /// two workers registered, one on loopback and one behind a relay shaped like the tailnet.
    /// Both come from the directory with a shell each, a hook on the far one badges the pill,
    /// and the far one killed and restarted is measured back, by its own link and by the
    /// server listing it online. No permissions needed.
    ThroughServer,
    /// All of the above but the simulator cases (`smooth-ios`, `pair-ios`).
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
    /// Write missing and failing render goldens under `crates/slopty-e2e/golden/`.
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
    /// One test at a time: the frame-time scenarios measure a quiet machine.
    serial: bool,
}

impl Suite {
    /// A suite of live tests: the whole target, in parallel.
    const fn live(package: &'static str, test: &'static str) -> Self {
        Self { kept: Kept::Ignored, package, test, only: None, default: None, serial: false }
    }

    /// A suite kept out of `cargo gate` by `var`.
    const fn env(var: &'static str, package: &'static str, test: &'static str) -> Self {
        Self { kept: Kept::Env(var), ..Self::live(package, test) }
    }

    /// Only the tests `filterset` picks.
    const fn only(self, filterset: &'static str) -> Self {
        Self { only: Some(filterset), ..self }
    }

    /// One test at a time.
    const fn serial(self) -> Self {
        Self { serial: true, ..self }
    }
}

/// The module holding a target's tests that need the Screen Recording grant; they run only
/// with `--screen-recording`.
const SCREEN_RECORDING: &str = "test(~screen_recording::)";
/// The module holding the app target's frame-time measurements, which run alone under `smooth`.
const FRAME_TIME: &str = "test(~frame_time::)";
/// The module holding the app target's showcase renders, which run only as their own case.
const SHOWCASE: &str = "test(~showcase::)";

const APP: &[Suite] =
    &[Suite::live("slopty-e2e", "app").only("not test(~frame_time::) and not test(~showcase::)")];

const SHOWCASE_APP: &[Suite] = &[Suite::live("slopty-e2e", "app").only(SHOWCASE)];

const WORKER: &[Suite] = &[Suite {
    kept: Kept::No,
    ..Suite::live("slopty-workerd", "e2e").only("test(~shell_round_trip)")
}];

const SCREEN: &[Suite] = &[
    Suite::env("SLOPTY_SCREEN_E2E", "slopty-capture", "geometry"),
    Suite::live("slopty-worker", "screen"),
    Suite::env("SLOPTY_SCREEN_E2E", "slopty-capture", "latency"),
    // The worker's other live tests are measurements, run by name with `--filter`.
    Suite { default: Some("test(~screen_stream)"), ..Suite::live("slopty-workerd", "e2e") },
];

const INPUT: &[Suite] = &[Suite::env("SLOPTY_INPUT_E2E", "slopty-input", "inject")];

const IOS: &[Suite] = &[
    Suite::live("slopty-e2e", "ios"),
    // The UIKit-boundary scenarios share the one simulator: one app at a time.
    Suite::live("slopty-e2e", "ios_uikit").serial(),
];

const SMOOTH: &[Suite] = &[
    Suite::live("slopty-e2e", "smooth").only("test(~on_the_mac)").serial(),
    Suite::live("slopty-e2e", "app").only(FRAME_TIME).serial(),
];

const PAIR: &[Suite] = &[Suite::live("slopty-e2e", "pair").only("test(~on_the_mac)").serial()];

const PAIR_IOS: &[Suite] =
    &[Suite::live("slopty-e2e", "pair").only("test(~with_the_simulator)").serial()];

const WORKERS: &[Suite] = &[Suite::live("slopty-e2e", "workers").serial()];

const SERVER: &[Suite] = &[Suite::live("slopty-e2e", "server")];

const THROUGH_SERVER: &[Suite] = &[Suite::live("slopty-e2e", "through_server").serial()];

const SMOOTH_IOS: &[Suite] =
    &[Suite::live("slopty-e2e", "smooth").only("test(~on_the_simulator)").serial()];

pub fn run(sh: &Shell, opts: &E2eOpts) -> Result<()> {
    let suites: Vec<&Suite> = match opts.case {
        Case::App => APP.iter().collect(),
        Case::Worker => WORKER.iter().collect(),
        Case::Screen => SCREEN.iter().collect(),
        Case::Input => INPUT.iter().collect(),
        Case::Smooth => SMOOTH.iter().collect(),
        Case::Showcase => SHOWCASE_APP.iter().collect(),
        Case::SmoothIos => SMOOTH_IOS.iter().collect(),
        Case::Pair => PAIR.iter().collect(),
        Case::PairIos => PAIR_IOS.iter().collect(),
        Case::Workers => WORKERS.iter().collect(),
        Case::Server => SERVER.iter().collect(),
        Case::ThroughServer => THROUGH_SERVER.iter().collect(),
        Case::All => APP
            .iter()
            .chain(WORKER)
            .chain(SERVER)
            .chain(SCREEN)
            .chain(INPUT)
            .chain(SMOOTH)
            .chain(PAIR)
            .chain(WORKERS)
            .chain(THROUGH_SERVER)
            .collect(),
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
    let bin_dir = sh.current_dir().join("target/debug");
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
        // Build every binary a suite may spawn up front, so a test never shells out to cargo.
        // `slopty-e2e` is in there for its own helper binaries (the idle window), and
        // `slopty-testkit` for the stand-in `claude` the agent and projects suites start.
        // The app carries the `e2e` feature (renderer access for `Render`), which also makes
        // the `slopty-app-e2e` bin the tests start. `--bins`, not `--bin slopty-app-e2e`: a
        // `--bin` filter applies to every selected package and would leave the daemons stale.
        step(
            "build daemons and app",
            &cmd!(
                sh,
                "cargo build -p slopty-ptyd -p slopty-workerd -p slopty-serverd -p slopty-cli -p slopty -p slopty-e2e -p slopty-testkit --bins --features slopty/e2e --features {LIVE}"
            ),
        )?;
        std::fs::create_dir_all(&reuse)?;
        let metadata = cmd!(sh, "cargo metadata --format-version 1").quiet().read()?;
        std::fs::write(&cargo_metadata, metadata)?;
    }

    // Every binary a suite spawns is built: `slopty_testkit::bins` looks no further.
    let _fresh = sh.push_env(crate::gate::BINS_FRESH, "1");

    // The iOS case also needs the app in a booted simulator; the test launches it there.
    let simulator = matches!(opts.case, Case::Ios | Case::SmoothIos | Case::PairIos)
        .then(|| -> Result<_> {
            let ios_opts = crate::ios::IosOpts::for_e2e(opts.sim, &opts.log);
            let udid = crate::ios::install_on_simulator(sh, &ios_opts)?;
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
        if !opts.no_build && build_suite(sh, suite, &binaries).is_err() {
            failed.push(title.to_owned());
            continue;
        }
        let expr = filterset(suite, opts.filter.as_deref(), opts.screen_recording);
        let expr: &[String] = expr.as_ref().map_or(&[], |e| std::slice::from_ref(e));
        let filter_flag: &[&str] = if expr.is_empty() { &[] } else { &["-E"] };
        let threads: &[&str] = if suite.serial { &["--test-threads", "1"] } else { &[] };
        let ignored: &[&str] =
            if suite.kept == Kept::Ignored { &["--run-ignored", "only"] } else { &[] };
        let command = cmd!(
            sh,
            "cargo nextest run --binaries-metadata {binaries} --cargo-metadata {cargo_metadata} --no-capture --no-fail-fast {ignored...} {threads...} {filter_flag...} {expr...}"
        );
        println!("▶ {title}");
        let started = std::time::Instant::now();
        let status = std::process::Command::from(command)
            .status()
            .with_context(|| format!("start nextest for {title}"))?;
        let outcome = match status.code() {
            Some(0) => {
                matched = matched.saturating_add(1);
                "✓"
            }
            // Nextest's "no tests to run": the filter picked nothing in this suite.
            Some(NO_TESTS_RUN) if opts.filter.is_some() => "–",
            _ => {
                matched = matched.saturating_add(1);
                failed.push(title.to_owned());
                "✘"
            }
        };
        println!("  {outcome} {title} ({:.1?})", started.elapsed());
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

/// Nextest's exit code when the filters leave no test to run.
const NO_TESTS_RUN: i32 = 4;

/// Build `suite`'s test binary and record where it is, for nextest to run without cargo.
fn build_suite(sh: &Shell, suite: &Suite, binaries: &std::path::Path) -> Result<()> {
    let (package, test) = (suite.package, suite.test);
    println!("▶ build {package} {test}");
    let started = std::time::Instant::now();
    let live: &[&str] = if package == LIVE_PACKAGE { &["--features", LIVE] } else { &[] };
    let listed = cmd!(
        sh,
        "cargo nextest list -p {package} --test {test} {live...} --list-type binaries-only --message-format json"
    )
    .read();
    let ok = listed.is_ok();
    println!("  {} build {package} {test} ({:.1?})", if ok { "✓" } else { "✘" }, started.elapsed());
    std::fs::write(binaries, listed?)?;
    Ok(())
}

/// The nextest filterset for a suite: its own, the caller's (else the suite's default), and,
/// without `screen_recording`, none of the tests that capture.
fn filterset(suite: &Suite, caller: Option<&str>, screen_recording: bool) -> Option<String> {
    let capture = (suite.kept == Kept::Ignored && !screen_recording)
        .then(|| format!("not {SCREEN_RECORDING}"));
    let parts: Vec<String> =
        [suite.only.map(str::to_owned), caller.or(suite.default).map(str::to_owned), capture]
            .into_iter()
            .flatten()
            .map(|part| format!("({part})"))
            .collect();
    (!parts.is_empty()).then(|| parts.join(" & "))
}

#[cfg(test)]
mod tests {
    use super::{Suite, filterset};

    #[test]
    fn a_suites_filters_the_callers_and_the_capture_rule_all_apply() {
        let pair = Suite::live("slopty-e2e", "pair").only("test(~on_the_mac)");
        assert_eq!(
            filterset(&pair, Some("test(pans) | test(zooms)"), false).as_deref(),
            Some(
                "(test(~on_the_mac)) & (test(pans) | test(zooms)) & (not test(~screen_recording::))"
            )
        );
        assert_eq!(filterset(&pair, None, true).as_deref(), Some("(test(~on_the_mac))"));

        let worker = Suite { default: Some("test(~screen_stream)"), ..Suite::live("w", "e2e") };
        assert_eq!(
            filterset(&worker, None, true).as_deref(),
            Some("(test(~screen_stream))"),
            "the default stands in for no caller filter"
        );
        assert_eq!(
            filterset(&worker, Some("test(x)"), true).as_deref(),
            Some("(test(x))"),
            "and gives way to one"
        );

        let gated = Suite::env("SLOPTY_INPUT_E2E", "slopty-input", "inject");
        assert_eq!(
            filterset(&gated, None, false),
            None,
            "a suite kept by a variable has no capture module"
        );
    }
}
