//! `cargo xtask deep` — the checks that are too slow for every commit and run on a schedule
//! (or before a release): Miri on the pure crates, a sanitizer build of the daemons, the
//! feature powerset, coverage, mutation testing, a short fuzz of every peer-facing decoder, and
//! the vendored QUIC stack's own suite.
//!
//! `cargo gate` is the bar every commit meets; these find what the gate cannot — undefined
//! behaviour a test only trips under Miri, a data race a sanitizer sees, a feature set that
//! does not build alone, a test suite that would not notice a mutated line. Each one prints
//! what it ran so the number lands in `docs/MEASUREMENTS.md` or the decision it decides, and
//! each has a canary (`deep canary`, [`canary`]): a planted defect it must catch, which the
//! Deep workflow runs beside it, so a check that could not fail turns its job red.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use clap::{Subcommand, ValueEnum};
use xshell::{PushEnv, Shell, cmd};

use crate::tools::{TRIPLES, quiet_step, step};

mod canary;
mod mutants;

/// Crates with no framework, FFI or GPUI in their tree: Miri can interpret their tests.
const PURE: &[&str] = &[
    "slopty-core",
    "slopty-proto",
    "slopty-grid",
    "slopty-predict",
    "slopty-theme",
    "slopty-settings",
];

/// Crates whose `unsafe` and threads are worth a sanitizer: the daemons, the codec, and every
/// crate that holds a share of the workspace's `unsafe` (the Apple framework calls of
/// `slopty-platform`, `slopty-capture`, `slopty-vdisplay` and `slopty-input`, the fatal-signal
/// handler of `slopty-crash`, the interface walk of `slopty-tailnet`). All of them build on a
/// nightly toolchain without GPUI.
const SANITIZED: &[&str] = &[
    "slopty-pty",
    // The PTY daemon itself, whose tests start its binary (`roundtrip`, `crash`).
    "slopty-ptyd",
    "slopty-net",
    "slopty-worker",
    "slopty-codec",
    "slopty-platform",
    "slopty-capture",
    "slopty-vdisplay",
    "slopty-crash",
    "slopty-tailnet",
    "slopty-input",
    // The drag-and-drop pasteboard calls, and the test kit's own `unsafe`: the counting global
    // allocator every allocation test runs under, and the process and IOReport reads.
    "slopty-dnd",
    "slopty-testkit",
];

/// Crates that install their own fatal-signal handlers and test that the process still dies of
/// the signal. A sanitizer's runtime installs its handlers before `main`, and the crate chains to
/// the handler it found, so the child would die of the runtime's report and abort instead. They
/// run apart, with the runtime's handlers off ([`OWN_SIGNALS_OPTIONS`]).
const OWN_SIGNALS: &[&str] = &["slopty-crash"];

/// Tests that count the instructions an operation takes against a budget, which
/// `ThreadSanitizer` cannot run: it adds its own instructions at every memory access and atomic,
/// so the count is the instrumentation's. `AddressSanitizer` leaves a loop of additions as it
/// is and runs them.
///
/// The tests that wait on a child once stood beside these: `ThreadSanitizer` held the child's
/// `SIGCHLD` for a minute (`fixtures/sigchld.rs`), and each such test waited that long or timed
/// out. The `SIGCHLD` pump linked into every sanitized binary ([`sigchld_pump`]) hands it over at
/// once, so they run.
const TSAN_COUNTS_INSTRUCTIONS: &[(&str, &str)] =
    &[("slopty-testkit", "bench::tests::a_series_reports_per_operation_and_as_json")];

/// Tests whose work runs on the test's one thread, so `ThreadSanitizer` has no second thread to
/// race it with, and which its instrumentation holds past their limit. The codec's clock-drift
/// test plays eight minutes of audio through the ring, and the codec's tests get a minute on a
/// runner (a stopped encoder hangs them): it timed out at 60 s (Deep run 40989163091). The
/// ring's threads are `ThreadSanitizer`'s in the player's own tests and loom's in its models.
/// The PTY's process-state test forks 300 children in a row, each the other half of a spawn in a
/// process the runtime does not follow, and a child costs it about 0.45 s on a Mac: 137 s alone,
/// past a runner's 180 s beside the other tests (Deep run 41018723691). Its one step with a
/// second thread hands that thread a spawn and shares only atomics with it. The PTY's spawns
/// from many threads at once stay instrumented. Only these two reasons put a test in a list here.
const TSAN_ONE_THREAD: &[(&str, &str)] = &[
    ("slopty-codec", "audio::latency_tests::clock_drift_is_absorbed_by_slices"),
    ("slopty-pty", "process_state::spawns_hold_up_in_a_process_in_a_bad_state"),
];

/// The nextest filterset that leaves out [`TSAN_COUNTS_INSTRUCTIONS`] and [`TSAN_ONE_THREAD`].
fn tsan_filter() -> String {
    let skipped: Vec<String> = TSAN_COUNTS_INSTRUCTIONS
        .iter()
        .chain(TSAN_ONE_THREAD)
        .map(|(package, test)| format!("(package({package}) & test(={test}))"))
        .collect();
    format!("not ({})", skipped.join(" | "))
}

/// The sanitizer runtime options (`ASAN_OPTIONS`, `TSAN_OPTIONS`) for [`OWN_SIGNALS`]: the
/// runtime leaves every fatal signal to the crate.
const OWN_SIGNALS_OPTIONS: &str =
    "handle_segv=0:handle_sigbus=0:handle_abort=0:handle_sigill=0:handle_sigfpe=0";

/// Which sanitizer to build with.
#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Sanitizer {
    /// `AddressSanitizer`: out-of-bounds, use-after-free, leaks.
    Address,
    /// `ThreadSanitizer`: data races.
    Thread,
    /// `RealtimeSanitizer`: an allocation, lock or blocking call in a function marked real-time,
    /// which is the audio render callback (`crates/slopty-codec/src/audio.rs`, `render`).
    Realtime,
}

impl Sanitizer {
    const fn flag(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Thread => "thread",
            Self::Realtime => "realtime",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum DeepCmd {
    /// Miri over the pure crates' tests (undefined behaviour, uninitialised reads, aliasing).
    Miri {
        /// Only these crates (default: the pure set).
        #[arg(short, long)]
        package: Vec<String>,
    },
    /// A sanitizer build of the daemons' tests on nightly with `-Zbuild-std`.
    Sanitize {
        /// Which sanitizer.
        #[arg(value_enum, default_value_t = Sanitizer::Thread)]
        which: Sanitizer,
        /// Only these crates (default: the daemon set).
        #[arg(short, long)]
        package: Vec<String>,
    },
    /// Every feature of every crate builds on its own and all together (`cargo hack`).
    Features,
    /// Line coverage per crate from the unit and headless tests (`cargo llvm-cov`).
    Coverage {
        /// Write the HTML report under `target/llvm-cov/html` and open it.
        #[arg(long)]
        html: bool,
    },
    /// A short run of every fuzz target (`cargo xtask fuzz` with `--time`), after replaying the
    /// kept regression inputs and the committed corpora.
    Fuzz {
        /// Seconds each target runs, under each engine.
        #[arg(long, default_value_t = 30)]
        time: u64,
        /// Fuzzing processes per target.
        #[arg(long, default_value_t = 1)]
        jobs: u32,
        /// AFL++ too, through ziggy, after libFuzzer (Linux).
        #[arg(long)]
        ziggy: bool,
    },
    /// The vendored QUIC stack's own suites (`vendor/noq-proto` and `vendor/noq-udp`, outside
    /// the workspace): the protocol's tests over two endpoints in memory and its
    /// random-interaction proptest, and the socket's sends and receives over loopback.
    Noq,
    /// The loom models of the lock-free structures (`--cfg slopty_loom`): the audio ring between
    /// the decoder's thread and the device's render callback, over every interleaving.
    Loom,
    /// The test binaries of the daemons' and the wire's crates under `leaks --atExit`: memory their
    /// paths leave unreachable, which a long-lived daemon would never get back.
    Leaks {
        /// Only these crates (default: the daemons' and the wire's).
        #[arg(short, long)]
        package: Vec<String>,
        /// Give the tests `MallocStackLogging`, so a report shows where a leak was allocated. A
        /// shell a test starts inherits it and prints its notice into the terminal the test reads,
        /// so tests that compare terminal output fail with it on.
        #[arg(long)]
        stacks: bool,
    },
    /// The app self-test (`cargo xtask e2e app`) with Metal's API and shader validation on: a
    /// misuse of the Metal API or an out-of-bounds access in a shader kills the app, and the test
    /// that drew it fails. GPUI's headless tests draw on no GPU, so this is the lane that can.
    Metal {
        /// A nextest filterset narrowing the self-test (`cargo xtask e2e app --filter`).
        #[arg(long)]
        filter: Option<String>,
        /// Reuse the self-test's last build (`cargo xtask e2e app --no-build`).
        #[arg(long)]
        no_build: bool,
    },
    /// Mutation testing (`cargo mutants`): which changed lines no test catches, in one crate
    /// (`-p`) or in the code a diff touches (`--in-diff`, `--since`), by tier.
    Mutants(mutants::Opts),
    /// Run one check against its planted defect, which it must catch: exits 0 when it caught
    /// it, for the planted reason, and fails when the defect got through.
    Canary {
        /// The check.
        #[arg(value_enum)]
        check: canary::Check,
        /// The fuzz canary under AFL++ too (`fuzz --ziggy`; Linux).
        #[arg(long)]
        ziggy: bool,
    },
    /// Fetch every package `Cargo.lock` names, for every platform, and drop from the registry's
    /// cache each `.crate` it no longer names: what the features job saves as main's cargo cache.
    CargoCache,
    /// The `mutants` nextest profile's setup script: build, in a mutant's tree, the binaries its
    /// tests spawn.
    #[command(hide = true)]
    MutantBins,
}

pub fn run(sh: &Shell, cmd: &DeepCmd) -> Result<()> {
    match cmd {
        DeepCmd::Miri { package } => miri(sh, package),
        DeepCmd::Sanitize { which, package } => sanitize(sh, *which, package),
        DeepCmd::Features => features(sh),
        DeepCmd::Coverage { html } => coverage(sh, *html),
        DeepCmd::Mutants(opts) => mutants::run(sh, opts),
        DeepCmd::Canary { check, ziggy } => canary::run(sh, *check, *ziggy),
        DeepCmd::CargoCache => cargo_cache(sh),
        DeepCmd::MutantBins => mutants::spawned(sh),
        DeepCmd::Metal { filter, no_build } => metal(sh, filter.as_deref(), *no_build),
        DeepCmd::Loom => loom(sh),
        DeepCmd::Noq => noq(sh),
        DeepCmd::Leaks { package, stacks } => leaks(sh, package, *stacks),
        DeepCmd::Fuzz { time, jobs, ziggy } => crate::fuzz::run(
            sh,
            &crate::fuzz::FuzzOpts {
                target: None,
                time: *time,
                jobs: *jobs,
                replay: false,
                cmin: false,
                ziggy: *ziggy,
                keep: None,
            },
        ),
    }
}

/// `target/deep/<name>` under the workspace root, absolute: a test that runs cargo itself (as
/// `slopty-worker`'s `ptyd_link` builds `slopty-ptyd`) runs in its crate's directory and inherits
/// `CARGO_TARGET_DIR`, which a relative path would put inside the crate.
fn deep_dir(name: &str) -> Result<Utf8PathBuf> {
    Ok(crate::tools::repo_root()?.join("target/deep").join(name))
}

fn packages(chosen: &[String], default: &[&str]) -> Vec<String> {
    let names: Vec<&str> = if chosen.is_empty() {
        default.to_vec()
    } else {
        chosen.iter().map(String::as_str).collect()
    };
    names.iter().flat_map(|c| ["-p".to_owned(), (*c).to_owned()]).collect()
}

/// Miri needs its own target dir (its artefacts are not the host's) and to see the file system
/// for insta's snapshots; proptest is cut to a few cases because the interpreter is ~100×
/// slower than native.
fn miri(sh: &Shell, chosen: &[String]) -> Result<()> {
    miri_ready(sh)?;
    let _env = miri_env(sh)?;
    let packages = packages(chosen, PURE);
    // Under nextest each test is its own interpreter, side by side on the cores, and the `miri`
    // profile ends one that runs past its bound instead of the night.
    let filter = miri_filter();
    step(
        "miri",
        &cmd!(
            sh,
            "nice -n 10 cargo +nightly miri nextest run --profile miri --no-fail-fast {packages...} -E {filter}"
        ),
    )
}

/// Miri on the nightly toolchain, added when it is missing.
fn miri_ready(sh: &Shell) -> Result<()> {
    let ready = cmd!(sh, "rustup run nightly cargo miri --version").quiet().ignore_stderr().read();
    if ready.is_err() {
        quiet_step(
            "rustup component add miri",
            cmd!(sh, "rustup component add --toolchain nightly miri"),
        )?;
    }
    Ok(())
}

/// What every Miri run shares, its canary's too.
fn miri_env(sh: &Shell) -> Result<Vec<PushEnv<'_>>> {
    Ok(vec![
        sh.push_env("CARGO_TARGET_DIR", deep_dir("miri")?),
        // Floats exact, as the hardware computes them: Miri otherwise adds a random error to
        // every operation the language leaves inexact, and the theme's colour tests, which
        // compare conversions to `f32::EPSILON`, failed on it (Deep trial run 38148072272).
        sh.push_env("MIRIFLAGS", "-Zmiri-disable-isolation -Zmiri-deterministic-floats"),
        sh.push_env("PROPTEST_CASES", "8"),
        sh.push_env("RUSTC_WRAPPER", ""),
        // insta shells out to `cargo metadata` for the workspace root unless told it; Miri
        // cannot `fork`, so it is told.
        sh.push_env("INSTA_WORKSPACE_ROOT", sh.current_dir()),
        sh.push_env("INSTA_UPDATE", "no"),
        // No pure crate's test spawns a daemon, and none could under Miri, but they take the
        // test kit, so nextest's setup script would build every one the workspace has: on a
        // Linux runner, the client's too, which no Linux build takes (run 38001943825, and
        // every night from 2026-10-04 on).
        sh.push_env(crate::gate::BINS_FRESH, "1"),
    ])
}

/// What Miri leaves out: the allocation-budget targets, which count a real allocator over
/// thousands of operations (an hour each under the interpreter, 2026-10-01) and hold no
/// `unsafe` of their own; the code they drive runs under Miri in the other targets.
const MIRI_FILTER: &str = "not binary(allocs)";

/// Tests Miri cannot run, by package and name, with why. Each failed or timed out on the first
/// night Miri ran a test at all (Deep trial run 38148072272); none of them was a finding.
const MIRI_CANNOT_RUN: &[(&str, &str, &str)] = &[
    // Settings are written whole and the directory synced after the rename; Miri syncs files only.
    ("slopty-settings", "tests::init_writes_once", "fsync on a directory"),
    (
        "slopty-settings",
        "tests::a_mac_joins_the_server_it_is_given_unless_it_has_one",
        "fsync on a directory",
    ),
    // Decoding at the frame limits, and a thousand random edits: past 20 minutes under the
    // interpreter. The same code runs under Miri on small inputs in the other tests.
    ("slopty-proto", "bounds::a_largest_frame_of_blank_lines_stops_at_the_budget", "too slow"),
    ("slopty-proto", "bounds::lines_wider_than_any_terminal_do_not_decode", "too slow"),
    ("slopty-proto", "units::a_frame_at_the_limit_passes_and_one_past_it_is_refused", "too slow"),
    ("slopty-predict", "editing::random_editing_never_shows_a_wrong_line", "too slow"),
];

/// The nextest filterset Miri runs: [`MIRI_FILTER`] without [`MIRI_CANNOT_RUN`].
fn miri_filter() -> String {
    let skipped: Vec<String> = MIRI_CANNOT_RUN
        .iter()
        .map(|(package, test, _)| format!("(package({package}) & test(={test}))"))
        .collect();
    format!("{MIRI_FILTER} & not ({})", skipped.join(" | "))
}

/// `-Zbuild-std` so the standard library is instrumented too (a race through `std` is still
/// a race); the host triple is passed explicitly because build-std needs it.
///
/// `RealtimeSanitizer` needs neither: its runtime intercepts `malloc`, locks and system calls
/// whatever called them, and it checks only functions marked `#[sanitize(realtime =
/// "nonblocking")]`, so it builds the codec alone with `--cfg slopty_rtsan`, which marks the
/// render callback and adds the test that proves an allocation there aborts.
fn sanitize(sh: &Shell, which: Sanitizer, chosen: &[String]) -> Result<()> {
    let flag = which.flag();
    // nextest's setup script runs `$SLOPTY_XTASK spawned-bins`, else `cargo xtask`, which under
    // these `RUSTFLAGS` rebuilt xtask with the sanitizer on its proc macros, which rustc refuses:
    // the setup failed and no sanitized test ran (Deep run 38228066972). This binary is built.
    let xtask = std::env::current_exe().context("the xtask binary")?;
    let _xtask = sh.push_env("SLOPTY_XTASK", xtask);
    let _env = sanitized_env(sh, which)?;
    let host = TRIPLES[0];
    let profile = runner_profile();
    if matches!(which, Sanitizer::Realtime) {
        // No codec test spawns a daemon; nextest's setup script would build them without the
        // target, so with the sanitizer on the proc macros, which rustc refuses.
        let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
        let packages = packages(chosen, &["slopty-codec"]);
        return step(
            "realtime sanitizer",
            &cmd!(sh, "cargo +nightly nextest run --target {host} {profile...} {packages...}"),
        );
    }
    let options_var = match which {
        Sanitizer::Thread => "TSAN_OPTIONS",
        Sanitizer::Address | Sanitizer::Realtime => "ASAN_OPTIONS",
    };
    let filter: Vec<String> = match which {
        Sanitizer::Thread => vec!["-E".to_owned(), tsan_filter()],
        Sanitizer::Address | Sanitizer::Realtime => Vec::new(),
    };
    let mut failed = Vec::new();
    for (group, options) in sanitized_groups(chosen) {
        let title = format!("{flag} sanitizer: {}", group.join(" "));
        let packages = packages(&group, &[]);
        let _options = options.map(|o| sh.push_env(options_var, o));
        let (filter, profile) = (&filter, &profile);
        // The daemons the tests spawn, built here with the sanitizer and the target as the tests
        // are. Left to nextest's setup script, they were built without the target, so with the
        // sanitizer on the proc macros, which rustc refuses: no sanitized test ran from
        // 2026-10-04 to 2026-10-11 (run 38001943825).
        let names: Vec<&str> = group.iter().map(String::as_str).collect();
        let built = crate::gate::spawned_bins_for(&names).map_or(Ok(()), |spawned| {
            step(
                &format!("{title}: the binaries tests spawn"),
                &cmd!(
                    sh,
                    "nice -n 10 cargo +nightly build -Zbuild-std --target {host} --profile test {spawned...}"
                ),
            )
        });
        let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
        let ran = built.and_then(|()| {
            step(
                &title,
                &cmd!(
                    sh,
                    "nice -n 10 cargo +nightly nextest run -Zbuild-std --target {host} {profile...} --no-fail-fast {packages...} {filter...}"
                ),
            )
        });
        if let Err(e) = ran {
            failed.push(format!("{e:#}"));
        }
    }
    anyhow::ensure!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}

/// The crates to sanitize (`chosen`, else [`SANITIZED`]) in the runs they need: the ones that
/// keep the sanitizer's own signal handlers, then [`OWN_SIGNALS`] with its options.
fn sanitized_groups(chosen: &[String]) -> Vec<(Vec<String>, Option<&'static str>)> {
    let names: Vec<String> = if chosen.is_empty() {
        SANITIZED.iter().map(|c| (*c).to_owned()).collect()
    } else {
        chosen.to_vec()
    };
    let (own, rest): (Vec<String>, Vec<String>) =
        names.into_iter().partition(|c| OWN_SIGNALS.contains(&c.as_str()));
    [(rest, None), (own, Some(OWN_SIGNALS_OPTIONS))]
        .into_iter()
        .filter(|(group, _)| !group.is_empty())
        .collect()
}

/// What every sanitized build shares, the canaries' too: the sanitizer in `RUSTFLAGS` (with the
/// [`sigchld_pump`] linked in under `ThreadSanitizer`), no compile cache, and a target dir of its
/// own, which `RUSTFLAGS` must name with no space in it.
fn sanitized_env(sh: &Shell, which: Sanitizer) -> Result<Vec<PushEnv<'_>>> {
    let dir = deep_dir(which.flag())?;
    let mut flags = match which {
        Sanitizer::Realtime => "-Zsanitizer=realtime --cfg slopty_rtsan".to_owned(),
        Sanitizer::Address | Sanitizer::Thread => format!("-Zsanitizer={}", which.flag()),
    };
    flags.push_str(" -C target-cpu=apple-m1");
    if matches!(which, Sanitizer::Thread) {
        let pump = sigchld_pump(sh)?;
        anyhow::ensure!(
            !pump.as_str().contains(char::is_whitespace),
            "RUSTFLAGS cannot hold {pump}"
        );
        let _pump = write!(flags, " -C link-arg={pump}");
    }
    Ok(vec![
        sh.push_env("CARGO_TARGET_DIR", dir),
        sh.push_env("RUSTFLAGS", flags),
        sh.push_env("RUSTC_WRAPPER", ""),
    ])
}

/// Build `fixtures/sigchld.rs` into the object every `ThreadSanitizer` binary links: before
/// `main` it blocks `SIGCHLD` on every thread but one, which waits for it in `sigsuspend`, so the
/// runtime runs the handler at once rather than on the main thread's next intercepted call, a
/// minute later (`docs/decisions/testing.md`, "Red means something", its `SIGCHLD` pump).
/// `no_std` and on its own, so it needs no build of the standard library.
fn sigchld_pump(sh: &Shell) -> Result<Utf8PathBuf> {
    let source = crate::tools::repo_root()?.join("xtask/src/deep/fixtures/sigchld.rs");
    let out = deep_dir("thread")?.join("sigchld.o");
    std::fs::create_dir_all(deep_dir("thread")?)?;
    let host = TRIPLES[0];
    quiet_step(
        "the SIGCHLD pump",
        cmd!(
            sh,
            "rustc +nightly --edition 2024 --crate-type lib --crate-name slopty_sigchld --emit obj -C panic=abort -C opt-level=2 --target {host} -o {out} {source}"
        ),
    )?;
    Ok(out)
}

/// nextest's profile on a runner, `ci`: the hardware a hosted Mac lacks left out as the gate
/// leaves it out, its timeouts and its `JUnit` report. Without it, the sanitizers and coverage ran
/// the counter and `IOReport` tests, which read nothing on a virtual Mac, and failed on them
/// every night (Deep run 40862351365). Here, the default: this Mac has the hardware.
fn runner_profile() -> Vec<&'static str> {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        vec!["--profile", crate::gate::NEXTEST_CI_PROFILE]
    } else {
        Vec::new()
    }
}

/// `--each-feature` builds every crate with each feature alone, none and all; the iOS lane
/// of the gate covers one axis of this (the `e2e`/`headless` features off), this covers all.
/// Off a Mac, as CI runs it, cargo-hack's cargo is the one that checks an Apple target there
/// (`tools::apple_cargo`), which it takes from `CARGO_HACK_CARGO_SRC`.
fn features(sh: &Shell) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("features")?);
    let host = TRIPLES[0];
    let (cargo, _env) = crate::tools::apple_cargo(sh, false)?;
    let _cargo = (cargo != "cargo").then(|| sh.push_env("CARGO_HACK_CARGO_SRC", cargo));
    step(
        "cargo hack check --each-feature",
        &cmd!(
            sh,
            "cargo hack check --workspace --each-feature --keep-going --target {host} --exclude xtask"
        ),
    )
}

/// The unit and headless tests, instrumented; the live e2e crate is left out (it drives a
/// built app, whose coverage is not attributable to it). Every build under one environment,
/// `cargo llvm-cov show-env`'s: the binaries tests spawn first, as the sanitizers build them,
/// then the tests with [`crate::gate::BINS_FRESH`], then the report. Under `cargo llvm-cov
/// nextest` the spawned binaries were built where no test looked, and a test that spawns one
/// built them again inside its own bound: 2 min 44 s of the repository watch's 180 s (Deep run
/// 40862351365). A failed test fails the run, after the report.
fn coverage(sh: &Shell, html: bool) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("cov")?);
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    let _cov = llvm_cov_env(sh)?;
    let profile = runner_profile();
    let scope = ["--workspace", "--exclude", "slopty-e2e", "--exclude", "xtask"];
    let bins = crate::gate::spawned_bin_args();
    step(
        "coverage: the binaries tests spawn",
        &cmd!(sh, "nice -n 10 cargo build --profile test {scope...} --tests {bins...}"),
    )?;
    let tests = {
        let _bins = sh.push_env(crate::gate::BINS_FRESH, "1");
        step(
            "coverage: the tests",
            &cmd!(sh, "nice -n 10 cargo nextest run {scope...} {profile...} --no-fail-fast"),
        )
    };
    let lcov = deep_dir("cov")?.join("lcov.info");
    step("coverage: lcov", &cmd!(sh, "cargo llvm-cov report --lcov --output-path {lcov}"))?;
    let report: &[&str] = if html { &["--html", "--open"] } else { &["--summary-only"] };
    step("coverage: the report", &cmd!(sh, "cargo llvm-cov report {report...}"))?;
    tests
}

/// `cargo llvm-cov show-env`'s environment, set on `sh` for every cargo after it, and the
/// profiles of the last run cleaned away. Without the compile cache: its wrapper would sit under
/// cargo-llvm-cov's, which instruments the workspace's crates alone.
fn llvm_cov_env(sh: &Shell) -> Result<Vec<PushEnv<'_>>> {
    let mut env = vec![sh.push_env("RUSTC_WRAPPER", "")];
    let shown =
        cmd!(sh, "cargo llvm-cov show-env --sh").read().context("cargo llvm-cov show-env")?;
    for (key, value) in shell_exports(&shown) {
        env.push(sh.push_env(key, value));
    }
    quiet_step("cargo llvm-cov clean", cmd!(sh, "cargo llvm-cov clean --workspace"))?;
    Ok(env)
}

/// The `export KEY='VALUE'` lines (or `KEY=VALUE`) `show-env --sh` prints, unquoted.
fn shell_exports(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let ok = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            let value = value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .map_or_else(|| value.to_owned(), |v| v.replace("'\\''", "'"));
            ok.then(|| (key.to_owned(), value))
        })
        .collect()
}

/// The loom models, by crate and nextest filterset. A model lives beside the structure it checks,
/// behind `cfg(slopty_loom)`, which swaps the structure's atomics for loom's; the crate's other
/// tests are not built then. The cfg is our own rather than loom's `cfg(loom)`, which tokio and
/// others read to swap their own internals.
const LOOM_MODELS: &[(&str, &str)] = &[("slopty-codec", "test(/^audio::ring_model::/)")];

/// Every model, exhaustively: no preemption bound (`LOOM_MAX_PREEMPTIONS` unset), since the
/// models are small enough to explore in full (`docs/decisions/tooling.md`, "The audio ring is
/// model-checked with loom").
fn loom(sh: &Shell) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("loom")?);
    let _flags = sh.push_env("RUSTFLAGS", "--cfg slopty_loom -C target-cpu=apple-m1");
    for (package, filter) in LOOM_MODELS {
        step(
            &format!("loom: {package}"),
            &cmd!(sh, "nice -n 10 cargo nextest run -p {package} --lib -E {filter}"),
        )?;
    }
    Ok(())
}

/// The vendored QUIC stack's own tests, which nothing else runs: `vendor/noq-proto`'s and
/// `vendor/noq-udp`'s, each outside the workspace. Their patches (`SLOPTY.md` beside each) are
/// otherwise held only by what Slopty's own tests reach of them, and noq-udp's on the platform
/// it runs on (Apple's batched send path on a Mac). Each lock is committed beside its crate, so
/// a run builds what the last did; the proptest draws new cases every run.
fn noq(sh: &Shell) -> Result<()> {
    let target = deep_dir("noq")?;
    ["noq-proto", "noq-udp"].into_iter().try_for_each(|name| {
        let manifest = format!("vendor/{name}/Cargo.toml");
        step(
            &format!("{name}'s suite"),
            &cmd!(
                sh,
                "nice -n 10 cargo test --manifest-path {manifest} --locked --target-dir {target}"
            ),
        )
    })
}

/// Crates whose tests run under `leaks --atExit`: the daemons' and the wire's, where memory left
/// unreachable is memory a long-lived process never gets back. The UI's tests leak on purpose
/// (`Box::leak` for a `'static` selector), so they are left out. So is `slopty-pty`: a process
/// `leaks` launches holds its PTY children's job control otherwise than a shell does, and its
/// terminal tests hang or fail under it (`the_tty_is_the_controlling_terminal_of_the_child`,
/// children stopped before `exec`; 2026-09-30). ptyd's heap is read live by `cargo xtask soak`'s
/// `leaks <pid>` instead.
const LEAK_CHECKED: &[&str] = &[
    "slopty-net",
    // The PTY daemon, whose tests start its binary; built here, `slopty-worker`'s `ptyd_link`
    // finds it too.
    "slopty-ptyd",
    "slopty-media",
    "slopty-engine",
    "slopty-grid",
    "slopty-server",
    "slopty-worker",
];

/// Build the test binaries of [`LEAK_CHECKED`] as `cargo test` does and run each, one test at a
/// time, under `leaks --atExit` (`man leaks`), which reads the heap once the tests are done. A
/// binary fails on a leak (exit 1), on a failed test, or on `leaks` failing. Each report goes to
/// `target/deep/leaks/reports/`. macOS's own tool rather than `LeakSanitizer`, which Apple's
/// sanitizer runtime does not support on this platform; and no instrumented build.
fn leaks(sh: &Shell, chosen: &[String], stacks: bool) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("leaks")?);
    let packages = packages(chosen, LEAK_CHECKED);
    let built = cmd!(sh, "nice -n 10 cargo test --no-run --message-format=json {packages...}")
        .ignore_stderr()
        .read()
        .context("building the test binaries")?;
    let binaries = test_binaries(&built);
    anyhow::ensure!(!binaries.is_empty(), "cargo built no test binary");
    // The daemons and stubs the tests spawn, built beside them first, so no test builds them
    // inside itself under `leaks`: those that did ran into the 600 s bound (run 38001943825).
    let names: Vec<&str> = if chosen.is_empty() {
        LEAK_CHECKED.to_vec()
    } else {
        chosen.iter().map(String::as_str).collect()
    };
    if let Some(spawned) = crate::gate::spawned_bins_for(&names) {
        quiet_step(
            "leaks: the binaries tests spawn",
            cmd!(sh, "nice -n 10 cargo build --profile test {spawned...}"),
        )?;
    }
    let reports = deep_dir("leaks")?.join("reports");
    std::fs::create_dir_all(&reports)?;
    let mut failed = Vec::new();
    for binary in &binaries {
        let name = binary.rsplit('/').next().unwrap_or(binary);
        let started = std::time::Instant::now();
        let report = reports.join(format!("{name}.txt"));
        let status = under_leaks(binary, report.as_std_path(), stacks, &[])
            .with_context(|| format!("leaks on {name}"))?;
        let text = std::fs::read_to_string(&report).unwrap_or_default();
        let verdict = match status {
            Ran::Exited(code) => leaks_verdict(&text, code),
            Ran::Killed => Err(format!("still running after {LEAKS_BOUND:?}, killed")),
        };
        let took = started.elapsed();
        match verdict {
            Ok(clean) => println!("  {name}: {clean} ({took:.1?})"),
            Err(why) => {
                println!("  {name}: {why} ({took:.1?})");
                failed.push(format!("{name}: {why} (target/deep/leaks/reports/{name}.txt)"));
            }
        }
    }
    anyhow::ensure!(failed.is_empty(), "leaks:\n  {}", failed.join("\n  "));
    Ok(())
}

/// How long one test binary may run under `leaks`: past it, it is killed and fails, since a test
/// that hangs there is a finding too and must not hold up the binaries after it.
const LEAKS_BOUND: std::time::Duration = std::time::Duration::from_secs(600);

/// How a run under `leaks` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ran {
    /// It exited with this code (`None` for a signal).
    Exited(Option<i32>),
    /// It outlived [`LEAKS_BOUND`] and was killed.
    Killed,
}

/// Run `binary`'s tests one at a time under `leaks --atExit`, its output to `report`, with `env`
/// added to its environment. One that
/// outlives [`LEAKS_BOUND`] is killed, with its process group and any process still running the
/// binary (a PTY child calls `setsid`).
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a child")]
fn under_leaks(
    binary: &str,
    report: &std::path::Path,
    stacks: bool,
    env: &[(&str, &str)],
) -> Result<Ran> {
    use std::os::unix::process::CommandExt as _;
    let out = std::fs::File::create(report)?;
    let mut child = std::process::Command::new("nice")
        .args(["-n", "10", "leaks", "--atExit", "--", binary, "--test-threads=1", "-q"])
        .envs(stacks.then_some(("MallocStackLogging", "1")))
        .envs(env.iter().copied())
        // Built by `leaks` before any binary runs.
        .env(crate::gate::BINS_FRESH, "1")
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0)
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Ran::Exited(status.code()));
        }
        if started.elapsed() > LEAKS_BOUND {
            let group = format!("-{}", child.id());
            let _killed = std::process::Command::new("kill").args(["-KILL", &group]).status();
            let _stray = std::process::Command::new("pkill").args(["-KILL", "-f", binary]).status();
            let _reaped = child.wait();
            return Ok(Ran::Killed);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Test targets that cannot run under `leaks --atExit`, which turns on `MallocStackLogging` in the
/// process it launches. Every child inherits it, and a shell a test drives on a PTY then prints
/// libmalloc's notice (`sh(…) MallocStackLogging: could not tag …`) into the terminal the test
/// reads, so its output checks fail (2026-09-30). Only this reason puts a target here.
const LEAKS_CANNOT_RUN: &[&str] = &["roundtrip", "session_actor"];

/// The test executables in `cargo test --no-run --message-format=json`'s output, less
/// [`LEAKS_CANNOT_RUN`].
fn test_binaries(built: &str) -> Vec<String> {
    built
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|m| m.pointer("/profile/test").and_then(serde_json::Value::as_bool) == Some(true))
        .filter(|m| {
            let target = m.pointer("/target/name").and_then(serde_json::Value::as_str);
            !target.is_some_and(|t| LEAKS_CANNOT_RUN.contains(&t))
        })
        .filter_map(|m| m.get("executable")?.as_str().map(str::to_owned))
        .collect()
}

/// What `leaks --atExit` said of a test binary: the tests passed and nothing leaked (its verdict
/// line), or why not.
fn leaks_verdict(text: &str, status: Option<i32>) -> Result<String, String> {
    let verdict = text
        .lines()
        .find(|l| {
            l.starts_with("Process ") && (l.contains(" leaks for ") || l.contains(" leak for "))
        })
        .map(str::trim);
    let passed = text.lines().any(|l| l.starts_with("test result: ok."));
    match (status, verdict, passed) {
        (Some(0), Some(v), true) => Ok(v.to_owned()),
        (_, _, false) => Err("a test failed, or none ran".to_owned()),
        (Some(1), Some(v), true) => Err(v.to_owned()),
        (code, v, true) => Err(format!("leaks failed ({code:?}): {}", v.unwrap_or("no verdict"))),
    }
}

/// Metal's validation switches (`man MetalValidation`), which Metal reads when the device is made,
/// so they go in the environment the harness hands the app. API validation asserts on an error
/// (`MTL_DEBUG_LAYER_ERROR_MODE` defaults to `assert`) and logs a warning; shader validation
/// reports a fault on stderr and aborts the app.
const METAL_VALIDATION: [(&str, &str); 5] = [
    ("MTL_DEBUG_LAYER", "1"),
    ("MTL_DEBUG_LAYER_WARNING_MODE", "nslog"),
    ("MTL_SHADER_VALIDATION", "1"),
    ("MTL_SHADER_VALIDATION_ABORT_ON_FAULT", "1"),
    ("MTL_SHADER_VALIDATION_REPORT_TO_STDERR", "1"),
];

/// The app self-test with [`METAL_VALIDATION`] in its environment, run by this xtask binary so
/// its build and filters are exactly `cargo xtask e2e app`'s.
fn metal(sh: &Shell, filter: Option<&str>, no_build: bool) -> Result<()> {
    let exe = std::env::current_exe().context("this xtask's path")?;
    let _env: Vec<_> = METAL_VALIDATION.iter().map(|(k, v)| sh.push_env(k, v)).collect();
    let filter: Vec<&str> = filter.map(|f| vec!["--filter", f]).unwrap_or_default();
    let no_build: &[&str] = if no_build { &["--no-build"] } else { &[] };
    step(
        "the app self-test under Metal validation",
        &cmd!(sh, "{exe} e2e app {filter...} {no_build...}"),
    )
}

/// Fetch every package `Cargo.lock` names, for every platform, then drop from each registry's
/// cache the `.crate` files it no longer names, so the cache main saves is the lock's alone.
/// In Rust, not shell: the step it replaces piped `ls` into `grep -v`, which exits 1 when it
/// selects nothing, so under pipefail a cache with nothing stale failed the features job every
/// night after its check had passed (Deep run 40862351365).
fn cargo_cache(sh: &Shell) -> Result<()> {
    quiet_step("cargo fetch --locked", cmd!(sh, "cargo fetch --locked"))?;
    let lock = std::fs::read_to_string(crate::tools::repo_root()?.join("Cargo.lock"))?;
    let locked = locked_crates(&lock)?;
    let home = std::env::var_os("CARGO_HOME").map_or_else(
        || std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cargo")),
        |h| Some(h.into()),
    );
    let cache = home.context("neither CARGO_HOME nor HOME is set")?.join("registry/cache");
    let (mut kept, mut dropped, mut bytes) = (0_usize, 0_usize, 0_u64);
    for registry in std::fs::read_dir(&cache).into_iter().flatten().filter_map(Result::ok) {
        for entry in std::fs::read_dir(registry.path())?.filter_map(Result::ok) {
            let name = entry.file_name();
            let crate_file = std::path::Path::new(&name).extension().is_some_and(|e| e == "crate");
            let Some(name) = name.to_str().filter(|_| crate_file) else { continue };
            if locked.contains(name) {
                kept = kept.saturating_add(1);
            } else {
                bytes = bytes.saturating_add(entry.metadata().map_or(0, |m| m.len()));
                std::fs::remove_file(entry.path())
                    .with_context(|| format!("removing {}", entry.path().display()))?;
                dropped = dropped.saturating_add(1);
            }
        }
    }
    println!(
        "cargo cache: {} packages locked, {kept} kept, {dropped} dropped ({} MB)",
        locked.len(),
        bytes / 1_000_000
    );
    Ok(())
}

/// Each package of a `Cargo.lock` as its registry cache names its file, `<name>-<version>.crate`.
fn locked_crates(lock: &str) -> Result<std::collections::BTreeSet<String>> {
    let lock: toml::Table = lock.parse().context("Cargo.lock")?;
    let packages = lock.get("package").and_then(toml::Value::as_array).context("no [[package]]")?;
    Ok(packages
        .iter()
        .filter_map(|p| {
            let (name, version) = (p.get("name")?.as_str()?, p.get("version")?.as_str()?);
            Some(format!("{name}-{version}.crate"))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test that passes only on a retry fails every run but for the tests named as timing
    /// themselves against the machine's load, each retried by its exact name, never a pattern,
    /// and no profile retries all its tests.
    #[test]
    fn a_flaky_pass_fails_but_for_the_named_retries() {
        let root = crate::tools::repo_root().unwrap();
        let text = std::fs::read_to_string(root.join(".config/nextest.toml")).unwrap();
        let config: toml::Table = text.parse().unwrap();
        let profiles = config["profile"].as_table().unwrap();
        let default = profiles["default"].as_table().unwrap();
        assert_eq!(default["flaky-result"].as_str(), Some("fail"));
        let mut named = 0_usize;
        for (name, profile) in profiles {
            let profile = profile.as_table().unwrap();
            assert!(
                profile.get("flaky-result").is_none_or(|r| r.as_str() == Some("fail")),
                "profile {name} lets a flaky pass stand"
            );
            assert!(
                profile.get("retries").is_none_or(|r| r.as_integer() == Some(0)),
                "profile {name} retries every test"
            );
            let overrides = profile.get("overrides").and_then(toml::Value::as_array);
            for over in overrides.into_iter().flatten() {
                let over = over.as_table().unwrap();
                if over.get("flaky-result").and_then(toml::Value::as_str) != Some("pass") {
                    assert!(
                        over.get("retries").is_none(),
                        "{name}: a retry whose pass does not stand passes nothing"
                    );
                    continue;
                }
                let filter = over["filter"].as_str().unwrap();
                assert!(over.contains_key("retries"), "{name}: {filter} passes with no retry");
                let tests = filter.matches("test(").count();
                assert!(tests > 0, "{name}: {filter} names no test");
                assert_eq!(
                    filter.matches("test(=").count(),
                    tests,
                    "{name}: {filter} retries tests by a pattern"
                );
                named = named.saturating_add(tests);
            }
        }
        assert!(named > 0, "no named retries: drop this test's allowance with them");
    }

    #[test]
    fn the_cargo_cache_keeps_each_locked_package_by_its_file_name() {
        let lock = r#"
version = 4

[[package]]
name = "anyhow"
version = "1.0.100"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "slopty-core"
version = "0.1.0"
"#;
        let locked = locked_crates(lock).unwrap();
        assert!(locked.contains("anyhow-1.0.100.crate"), "{locked:?}");
        assert!(!locked.contains("anyhow-1.0.99.crate"));
        assert!(locked_crates("version = 4\n").is_err(), "a lock with no package is refused");
    }

    #[test]
    fn llvm_cov_s_environment_is_read_from_its_shell_exports() {
        let shown = "export RUSTFLAGS='-C instrument-coverage --cfg=coverage'\n\
                     export LLVM_PROFILE_FILE='/t/x-%p-%10m.profraw'\n\
                     CARGO_LLVM_COV=1\n\
                     info: not an export\n";
        assert_eq!(
            shell_exports(shown),
            [
                ("RUSTFLAGS".to_owned(), "-C instrument-coverage --cfg=coverage".to_owned()),
                ("LLVM_PROFILE_FILE".to_owned(), "/t/x-%p-%10m.profraw".to_owned()),
                ("CARGO_LLVM_COV".to_owned(), "1".to_owned()),
            ]
        );
        assert_eq!(shell_exports("export A='it'\\''s'"), [("A".to_owned(), "it's".to_owned())]);
    }

    #[test]
    fn a_deep_target_dir_is_absolute_under_the_workspace_target() {
        let dir = deep_dir("address").unwrap();
        assert!(dir.is_absolute(), "{dir}");
        assert_eq!(dir, crate::tools::repo_root().unwrap().join("target/deep/address"));
    }

    #[test]
    fn crates_with_their_own_signal_handlers_run_apart_with_the_runtimes_off() {
        let groups = sanitized_groups(&[]);
        let [(rest, None), (own, Some(options))] = groups.as_slice() else {
            panic!("two runs: {groups:?}");
        };
        assert_eq!(own, &["slopty-crash"]);
        assert!(!rest.iter().any(|c| c == "slopty-crash"), "{rest:?}");
        assert_eq!(rest.len() + own.len(), SANITIZED.len());
        assert!(options.contains("handle_segv=0") && options.contains("handle_abort=0"));
        let chosen = sanitized_groups(&["slopty-net".to_owned()]);
        assert_eq!(chosen, [(vec!["slopty-net".to_owned()], None)], "one crate, one run");
    }

    #[test]
    fn a_binary_passes_leaks_only_with_its_tests_passed_and_nothing_leaked() {
        let clean =
            "test result: ok. 49 passed; 0 failed\nProcess 1: 0 leaks for 0 total leaked bytes.\n";
        assert_eq!(
            leaks_verdict(clean, Some(0)),
            Ok("Process 1: 0 leaks for 0 total leaked bytes.".into())
        );
        let leaked = "test result: ok. 3 passed\nProcess 1: 2 leaks for 64 total leaked bytes.\n";
        assert_eq!(
            leaks_verdict(leaked, Some(1)),
            Err("Process 1: 2 leaks for 64 total leaked bytes.".into())
        );
        let failed = "test result: FAILED. 1 passed; 1 failed\nProcess 1: 0 leaks for 0 total leaked bytes.\n";
        assert!(leaks_verdict(failed, Some(0)).is_err(), "a failed test is not clean");
        assert!(leaks_verdict("test result: ok. 1 passed\n", Some(2)).is_err(), "no verdict");
        let one = "test result: ok. 1 passed\nProcess 1: 1 leak for 65536 total leaked bytes.\n";
        assert_eq!(
            leaks_verdict(one, Some(1)),
            Err("Process 1: 1 leak for 65536 total leaked bytes.".into()),
            "one leak is named by its own verdict, not as a missing one"
        );
    }

    #[test]
    fn only_the_test_executables_leaks_can_run_are_run() {
        let built = [
            r#"{"reason":"compiler-artifact","target":{"name":"net"},"profile":{"test":true},"executable":"/t/deps/net-1"}"#,
            r#"{"reason":"compiler-artifact","target":{"name":"session_actor"},"profile":{"test":true},"executable":"/t/deps/session_actor-2"}"#,
            r#"{"reason":"compiler-artifact","profile":{"test":false},"executable":"/t/slopty-ptyd"}"#,
            r#"{"reason":"compiler-artifact","profile":{"test":false},"executable":null}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        assert_eq!(test_binaries(&built), ["/t/deps/net-1"]);
    }

    #[test]
    fn miri_leaves_out_the_allocation_targets_and_each_test_it_cannot_run() {
        let filter = miri_filter();
        assert!(filter.starts_with("not binary(allocs) & not ("), "{filter}");
        for (package, test, _) in MIRI_CANNOT_RUN {
            assert!(filter.contains(&format!("(package({package}) & test(={test}))")), "{filter}");
        }
    }

    #[test]
    fn the_thread_sanitizer_leaves_out_only_its_two_lists_by_their_whole_names() {
        let filter = tsan_filter();
        assert!(filter.starts_with("not ("), "{filter}");
        let listed: Vec<_> = TSAN_COUNTS_INSTRUCTIONS.iter().chain(TSAN_ONE_THREAD).collect();
        for (package, test) in &listed {
            assert!(filter.contains(&format!("(package({package}) & test(={test}))")), "{filter}");
        }
        assert_eq!(filter.matches("package(").count(), listed.len());
    }
}
