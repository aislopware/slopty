//! `cargo xtask deep` — the checks that are too slow for every commit and run on a schedule
//! (or before a release): Miri on the pure crates, a sanitizer build of the daemons, the
//! feature powerset, coverage, mutation testing, and a short fuzz of every peer-facing decoder.
//!
//! `cargo gate` is the bar every commit meets; these find what the gate cannot — undefined
//! behaviour a test only trips under Miri, a data race a sanitizer sees, a feature set that
//! does not build alone, a test suite that would not notice a mutated line. Each one prints
//! what it ran so the number lands in `docs/MEASUREMENTS.md` or the decision it decides.

use anyhow::{Context as _, Result};
use clap::{Subcommand, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, quiet_step, step};

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
];

/// Crates that install their own fatal-signal handlers and test that the process still dies of
/// the signal. A sanitizer's runtime installs its handlers before `main`, and the crate chains to
/// the handler it found, so the child would die of the runtime's report and abort instead. They
/// run apart, with the runtime's handlers off ([`OWN_SIGNALS_OPTIONS`]).
const OWN_SIGNALS: &[&str] = &["slopty-crash"];

/// Tests `ThreadSanitizer` cannot run, by package and name. Each waits on a child through
/// `tokio::process`, which learns of the exit from `SIGCHLD`. `ThreadSanitizer` holds a signal's
/// handler until the thread next calls a function it intercepts, and it does not intercept
/// `kevent`, where the runtime is parked, so the child stays `<defunct>` and the test waits
/// forever. Each was seen doing so alone on 2026-09-30 (`docs/TESTING.md`, "Sanitizers and
/// signals"). `AddressSanitizer` runs them. Only this reason puts a test here, never a timing.
const TSAN_CANNOT_RUN: &[(&str, &str)] = &[
    ("slopty-pty", "pty::tests::the_tty_is_the_controlling_terminal_of_the_child"),
    (
        "slopty-pty",
        "ssh::tests::a_host_that_cannot_take_the_entry_gets_xterm_256color_and_is_asked_again_next_time",
    ),
    ("slopty-pty", "process_state::spawns_hold_up_in_a_process_in_a_bad_state"),
    ("slopty-pty", "spawn::shells_start_while_other_threads_allocate_and_hold_locks"),
    ("slopty-worker", "actor::the_summary_carries_the_progress_and_the_restore"),
    ("slopty-worker", "actor::closing_the_session_hangs_up_the_child"),
];

/// The nextest filterset that leaves out [`TSAN_CANNOT_RUN`].
fn tsan_filter() -> String {
    let skipped: Vec<String> = TSAN_CANNOT_RUN
        .iter()
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
    /// kept regression inputs.
    Fuzz {
        /// Seconds each target runs.
        #[arg(long, default_value_t = 30)]
        time: u64,
    },
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
    /// Mutation testing of one crate (`cargo mutants`): which changed lines no test catches.
    Mutants {
        /// The crate to mutate.
        #[arg(short, long)]
        package: String,
        /// Seconds a mutant may run before it counts as a timeout.
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
}

pub fn run(sh: &Shell, cmd: &DeepCmd) -> Result<()> {
    match cmd {
        DeepCmd::Miri { package } => miri(sh, package),
        DeepCmd::Sanitize { which, package } => sanitize(sh, *which, package),
        DeepCmd::Features => features(sh),
        DeepCmd::Coverage { html } => coverage(sh, *html),
        DeepCmd::Mutants { package, timeout } => mutants(sh, package, *timeout),
        DeepCmd::Metal { filter, no_build } => metal(sh, filter.as_deref(), *no_build),
        DeepCmd::Loom => loom(sh),
        DeepCmd::Leaks { package, stacks } => leaks(sh, package, *stacks),
        DeepCmd::Fuzz { time } => crate::fuzz::run(
            sh,
            &crate::fuzz::FuzzOpts {
                target: None,
                time: *time,
                jobs: 1,
                replay: false,
                keep: None,
            },
        ),
    }
}

/// `target/deep/<name>` under the workspace root, absolute: a test that runs cargo itself (as
/// `slopty-worker`'s `ptyd_link` builds `slopty-ptyd`) runs in its crate's directory and inherits
/// `CARGO_TARGET_DIR`, which a relative path would put inside the crate.
fn deep_dir(name: &str) -> Result<camino::Utf8PathBuf> {
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
    let ready = cmd!(sh, "rustup run nightly cargo miri --version").quiet().ignore_stderr().read();
    if ready.is_err() {
        quiet_step(
            "rustup component add miri",
            cmd!(sh, "rustup component add --toolchain nightly miri"),
        )?;
    }
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("miri")?);
    let _flags = sh.push_env("MIRIFLAGS", "-Zmiri-disable-isolation");
    let _cases = sh.push_env("PROPTEST_CASES", "8");
    let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
    // insta shells out to `cargo metadata` for the workspace root unless told it; Miri cannot
    // `fork`, so it is told.
    let _root = sh.push_env("INSTA_WORKSPACE_ROOT", sh.current_dir());
    let _update = sh.push_env("INSTA_UPDATE", "no");
    let packages = packages(chosen, PURE);
    quiet_step("miri", cmd!(sh, "cargo +nightly miri test {packages...}"))
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
    if matches!(which, Sanitizer::Realtime) {
        let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("realtime")?);
        let flags = "-Zsanitizer=realtime --cfg slopty_rtsan -C target-cpu=apple-m1";
        let _flags = sh.push_env("RUSTFLAGS", flags);
        let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
        let packages = packages(chosen, &["slopty-codec"]);
        let host = TRIPLES[0];
        return quiet_step(
            "realtime sanitizer",
            cmd!(sh, "cargo +nightly nextest run --target {host} {packages...}"),
        );
    }
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir(flag)?);
    let _flags = sh.push_env("RUSTFLAGS", format!("-Zsanitizer={flag} -C target-cpu=apple-m1"));
    let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
    let host = TRIPLES[0];
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
        let filter = &filter;
        let ran = quiet_step(
            &title,
            cmd!(
                sh,
                "nice -n 10 cargo +nightly nextest run -Zbuild-std --target {host} --no-fail-fast {packages...} {filter...}"
            ),
        );
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

/// `--each-feature` builds every crate with each feature alone, none and all; the iOS lane
/// of the gate covers one axis of this (the `e2e`/`headless` features off), this covers all.
fn features(sh: &Shell) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("features")?);
    let host = TRIPLES[0];
    quiet_step(
        "cargo hack check --each-feature",
        cmd!(
            sh,
            "cargo hack check --workspace --each-feature --keep-going --target {host} --exclude xtask"
        ),
    )
}

/// The unit and headless tests, instrumented; the live e2e crate is left out (it drives a
/// built app, whose coverage is not attributable to it).
fn coverage(sh: &Shell, html: bool) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", deep_dir("cov")?);
    let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    let report: &[&str] = if html { &["--html", "--open"] } else { &["--summary-only"] };
    cmd!(sh, "cargo llvm-cov nextest --workspace --exclude slopty-e2e --exclude xtask {report...}")
        .run()
        .context("cargo llvm-cov")
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
        quiet_step(
            &format!("loom: {package}"),
            cmd!(sh, "nice -n 10 cargo nextest run -p {package} --lib -E {filter}"),
        )?;
    }
    Ok(())
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
    let reports = deep_dir("leaks")?.join("reports");
    std::fs::create_dir_all(&reports)?;
    let mut failed = Vec::new();
    for binary in &binaries {
        let name = binary.rsplit('/').next().unwrap_or(binary);
        let started = std::time::Instant::now();
        let report = reports.join(format!("{name}.txt"));
        let status = under_leaks(binary, report.as_std_path(), stacks)
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

/// Run `binary`'s tests one at a time under `leaks --atExit`, its output to `report`. One that
/// outlives [`LEAKS_BOUND`] is killed, with its process group and any process still running the
/// binary (a PTY child calls `setsid`).
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a child")]
fn under_leaks(binary: &str, report: &std::path::Path, stacks: bool) -> Result<Ran> {
    use std::os::unix::process::CommandExt as _;
    let out = std::fs::File::create(report)?;
    let mut child = std::process::Command::new("nice")
        .args(["-n", "10", "leaks", "--atExit", "--", binary, "--test-threads=1", "-q"])
        .envs(stacks.then_some(("MallocStackLogging", "1")))
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
        .find(|l| l.starts_with("Process ") && l.contains(" leaks for "))
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

/// One crate at a time: mutants rebuilds per mutation, so the whole workspace would be hours.
fn mutants(sh: &Shell, package: &str, timeout: u64) -> Result<()> {
    let timeout = timeout.to_string();
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    cmd!(sh, "cargo mutants --package {package} --timeout {timeout} --output target/deep/mutants")
        .run()
        .context("cargo mutants")
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_thread_sanitizer_leaves_out_each_test_it_cannot_run_by_its_whole_name() {
        let filter = tsan_filter();
        assert!(filter.starts_with("not ("), "{filter}");
        for (package, test) in TSAN_CANNOT_RUN {
            assert!(filter.contains(&format!("(package({package}) & test(={test}))")), "{filter}");
        }
    }
}
