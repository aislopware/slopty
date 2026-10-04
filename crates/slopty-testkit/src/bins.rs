//! The daemons and doubles a test spawns, from the same build as the test.
//!
//! A test that spawns `slopty-ptyd`, `slopty-worker`, `slopty-server`, `slopty` or the stand-ins
//! for `claude`, a managed launcher, `pi` and an ACP agent finds each beside its own binary. Under
//! `cargo xtask` (the gate's tests, the e2e suites) they are built before any test starts, which
//! sets [`FRESH`], so no test shells out to cargo: one that did would wait on cargo's locks for as
//! long as any other build on the machine held them, inside its own timeout. A bare `cargo nextest
//! run` builds them in its setup script (`cargo xtask spawned-bins`), which names the directory in
//! [`BUILT`]. Any other run, or a test of another profile, builds them once per test process
//! instead, fresh, since one left from an older build would test the old code.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Set by `cargo xtask` once every binary in [`NAMES`] is built into the profile directory the
/// tests run from.
pub const FRESH: &str = "SLOPTY_BINS_FRESH";

/// Set by nextest's setup script to the profile directory it built every binary in [`NAMES`]
/// into, which is fresh for the tests built there too.
pub const BUILT: &str = "SLOPTY_BINS_BUILT";

/// The binaries a test may spawn. `cargo xtask` builds the same list (`--bin`).
pub const NAMES: [&str; 8] = [
    "slopty-ptyd",
    "slopty-worker",
    "slopty-server",
    "slopty",
    "slopty-stub-claude",
    "slopty-stub-managed-claude",
    "slopty-stub-pi",
    "slopty-stub-acp",
];

/// The binary `name` from this build, beside `anchor`: a binary of the calling test's own
/// package, as `env!("CARGO_BIN_EXE_<bin>")` names it.
///
/// # Panics
///
/// When neither [`FRESH`] nor [`BUILT`] vouches for `anchor`'s directory and the build of
/// [`NAMES`] fails.
#[must_use]
pub fn bin(anchor: &str, name: &str) -> PathBuf {
    let anchor = Path::new(anchor);
    if !fresh(anchor) {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| build(anchor));
    }
    anchor.with_file_name(name)
}

fn fresh(anchor: &Path) -> bool {
    std::env::var_os(FRESH).is_some()
        || std::env::var_os(BUILT).is_some_and(|dir| anchor.parent() == Some(Path::new(&dir)))
}

fn build(anchor: &Path) {
    let release = anchor.parent().is_some_and(|dir| dir.ends_with("release"));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = std::process::Command::new(cargo);
    // The whole workspace with its tests, as the gate and the setup script build them, so features
    // resolve across every member's dev-dependencies too and each crate is the unit already built.
    build.args(["build", "--workspace", "--tests"]);
    for name in NAMES {
        build.args(["--bin", name]);
    }
    // The test's own profile, whose units its build already holds: plain `cargo build` is
    // `dev`, which optimises the workspace crates `test` leaves unoptimised.
    build.args(if release { ["--release"].as_slice() } else { ["--profile", "test"].as_slice() });
    let status = build.status();
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "build the binaries tests spawn ({}): {status:?}",
        NAMES.join(", ")
    );
}
