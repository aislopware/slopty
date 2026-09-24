//! `cargo xtask deep` — the checks that are too slow for every commit and run on a schedule
//! (or before a release): Miri on the pure crates, a sanitizer build of the daemons, the
//! feature powerset, coverage and mutation testing.
//!
//! `cargo gate` is the bar every commit meets; these find what the gate cannot — undefined
//! behaviour a test only trips under Miri, a data race a sanitizer sees, a feature set that
//! does not build alone, a test suite that would not notice a mutated line. Each one prints
//! what it ran so the number lands in `docs/MEASUREMENTS.md` or the decision it decides.

use anyhow::{Context as _, Result};
use clap::{Subcommand, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, quiet_step};

/// Crates with no framework, FFI or GPUI in their tree: Miri can interpret their tests.
const PURE: &[&str] = &[
    "slopty-core",
    "slopty-proto",
    "slopty-grid",
    "slopty-predict",
    "slopty-theme",
    "slopty-settings",
];

/// Crates whose `unsafe` and threads are worth a sanitizer: the daemons and the codec, which
/// build on a nightly toolchain without GPUI.
const SANITIZED: &[&str] = &["slopty-pty", "slopty-net", "slopty-worker", "slopty-codec"];

/// Which sanitizer to build with.
#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Sanitizer {
    /// `AddressSanitizer`: out-of-bounds, use-after-free, leaks.
    Address,
    /// `ThreadSanitizer`: data races.
    Thread,
}

impl Sanitizer {
    const fn flag(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Thread => "thread",
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
    }
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
    let _dir = sh.push_env("CARGO_TARGET_DIR", "target/deep/miri");
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
fn sanitize(sh: &Shell, which: Sanitizer, chosen: &[String]) -> Result<()> {
    let flag = which.flag();
    let _dir = sh.push_env("CARGO_TARGET_DIR", format!("target/deep/{flag}"));
    let _flags = sh.push_env("RUSTFLAGS", format!("-Zsanitizer={flag} -C target-cpu=apple-m1"));
    let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
    let packages = packages(chosen, SANITIZED);
    let host = TRIPLES[0];
    quiet_step(
        &format!("{flag} sanitizer"),
        cmd!(sh, "cargo +nightly nextest run -Zbuild-std --target {host} {packages...}"),
    )
}

/// `--each-feature` builds every crate with each feature alone, none and all; the iOS lane
/// of the gate covers one axis of this (the `e2e`/`headless` features off), this covers all.
fn features(sh: &Shell) -> Result<()> {
    let _dir = sh.push_env("CARGO_TARGET_DIR", "target/deep/features");
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
    let _dir = sh.push_env("CARGO_TARGET_DIR", "target/deep/cov");
    let _wrapper = sh.push_env("RUSTC_WRAPPER", "");
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    let report: &[&str] = if html { &["--html", "--open"] } else { &["--summary-only"] };
    cmd!(sh, "cargo llvm-cov nextest --workspace --exclude slopty-e2e --exclude xtask {report...}")
        .run()
        .context("cargo llvm-cov")
}

/// One crate at a time: mutants rebuilds per mutation, so the whole workspace would be hours.
fn mutants(sh: &Shell, package: &str, timeout: u64) -> Result<()> {
    let timeout = timeout.to_string();
    let _env = sh.push_env("AWS_LC_SYS_CMAKE_BUILDER", "1");
    cmd!(sh, "cargo mutants --package {package} --timeout {timeout} --output target/deep/mutants")
        .run()
        .context("cargo mutants")
}
