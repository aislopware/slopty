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
/// The app target's tests that code video: a remote window streamed into the right pane, which
/// needs the Screen Recording grant too. A new one left out fails in the first group, where every
/// `VideoToolbox` session is refused ([`NO_VIDEOTOOLBOX`]), naming the call.
macro_rules! coding {
    () => {
        "(test(~screen_recording::))"
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

const IOS: &[Suite] = &[Suite::live("slopty-e2e", "ios")];

const SERVER: &[Suite] = &[Suite::live("slopty-e2e", "server")];

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
        Case::All => APP.iter().chain(WORKER).chain(SERVER).chain(SCREEN).chain(INPUT).collect(),
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
    let simulator = matches!(opts.case, Case::Ios)
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
        let ignored: &[&str] =
            if suite.kept == Kept::Ignored { &["--run-ignored", "only"] } else { &[] };
        let mut command = cmd!(
            sh,
            "cargo nextest run --binaries-metadata {binaries} --cargo-metadata {cargo_metadata} --no-capture --no-fail-fast {ignored...} {filter_flag...} {expr...}"
        );
        if suite.no_videotoolbox {
            command = command.env(NO_VIDEOTOOLBOX, "1");
        }
        println!("▶ {title}");
        let started = std::time::Instant::now();
        let watchdog = on_ci().then(|| {
            crate::watchdog::Watchdog::start(title, std::process::id(), SUITE_HANG_AFTER, None)
        });
        let ran = if suite.codes && cfg!(target_os = "macos") {
            coding_step(sh, command)
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
const PROBE: &str = "slopty-encoder-probe";

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
fn encoder_answers(sh: &Shell) -> bool {
    let built = sh.current_dir().join("target/debug").join(PROBE);
    let copy = std::env::temp_dir().join(format!("{PROBE}-{}", std::process::id()));
    if std::fs::copy(&built, &copy).is_err() {
        println!("  the encoder probe is not built at {}", built.display());
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
fn coding_step(sh: &Shell, command: xshell::Cmd<'_>) -> Ran {
    if !encoder_answers(sh) {
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
    if crate::watchdog::encoder_stopped() || !encoder_answers(sh) {
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
    use super::{APP, Suite, filterset};

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
    }

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
