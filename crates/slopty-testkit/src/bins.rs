//! The daemons and doubles a test spawns, from the same build as the test.
//!
//! A test that spawns `slopty-ptyd`, `slopty-worker`, `slopty-server`, `slopty` or the stand-ins
//! for `claude`, a managed launcher, `pi`, an ACP agent and an app's window finds each beside its
//! own binary. Under
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
pub const NAMES: [&str; 9] = [
    "slopty-ptyd",
    "slopty-worker",
    "slopty-server",
    "slopty",
    "slopty-stub-claude",
    "slopty-stub-managed-claude",
    "slopty-stub-pi",
    "slopty-stub-acp",
    "slopty-stub-window",
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
    // Into the target directory the test runs from. A tool may have named it to cargo with
    // `--target-dir` while this process's environment names another: `cargo llvm-cov` runs the
    // tests from `llvm-cov-target`, and a build that followed `CARGO_TARGET_DIR` left every
    // binary somewhere else, so an ACP test found no agent (Deep run 38001943825).
    if let Some((dir, triple)) = anchor.parent().and_then(target_of) {
        build.arg("--target-dir").arg(dir);
        if let Some(triple) = triple {
            build.arg("--target").arg(triple);
        }
    }
    let status = build.status();
    assert!(
        status.as_ref().is_ok_and(std::process::ExitStatus::success),
        "build the binaries tests spawn ({}): {status:?}",
        NAMES.join(", ")
    );
}

/// The target directory a profile directory `profile` is in, and the target triple between them
/// when there is one (`<target>/<triple>/debug`). Cargo marks a target directory's root with
/// `CACHEDIR.TAG`.
fn target_of(profile: &Path) -> Option<(&Path, Option<&std::ffi::OsStr>)> {
    let above = profile.parent()?;
    if above.join("CACHEDIR.TAG").is_file() {
        return Some((above, None));
    }
    let root = above.parent()?;
    root.join("CACHEDIR.TAG").is_file().then_some((root, above.file_name()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::target_of;

    /// A profile directory's target directory is the one marked as cargo marks it, with the
    /// triple between them when the build named one, and none when nothing is marked.
    #[test]
    fn a_profile_directory_names_its_target_directory() {
        let dir = std::env::temp_dir().join(format!("slopty-testkit-bins-{}", std::process::id()));
        let root = dir.join("llvm-cov-target");
        let plain = root.join("debug");
        let crossed = root.join("aarch64-apple-darwin").join("debug");
        for made in [&plain, &crossed] {
            std::fs::create_dir_all(made).unwrap();
        }
        assert_eq!(target_of(&plain), None, "no CACHEDIR.TAG yet");
        std::fs::write(root.join("CACHEDIR.TAG"), "").unwrap();
        assert_eq!(target_of(&plain), Some((root.as_path(), None)));
        let triple = Some(std::ffi::OsStr::new("aarch64-apple-darwin"));
        assert_eq!(target_of(&crossed), Some((root.as_path(), triple)));
        assert_eq!(target_of(Path::new("/")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
