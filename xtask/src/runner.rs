//! `xtask test-runner`: cargo's target runner for the test binaries the gate and `check` run.
//!
//! A process that opens `VideoToolbox` or `CoreAudio` pays for every entry in its executable's
//! directory: the codec's tests took 25–36 s from a `deps/` of 101 000 entries and 2.7–2.9 s
//! from one of 1 100, on either volume, and a hard link of the same binary in a small directory
//! ran as fast as a copy (`docs/decisions/tooling.md`, "A test binary's directory, not the
//! volume"). Cargo to 1.99 writes every test binary into `deps/`, beside the object files of
//! every compile, so the runner runs each through a hard link in `<profile>/run/`: the same file,
//! a directory of one entry per test binary. Everything else is unchanged. The binary's
//! `current_exe()` is one level below the profile directory as before, and its debug info still
//! points at the objects in `deps/`. From Cargo 1.100 a test binary has a directory of its own
//! and runs as it is.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::CommandExt as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// The environment variable that makes this the runner of host binaries for cargo and nextest.
pub const RUNNER_VAR: &str = "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER";

/// This xtask binary as the runner, for [`RUNNER_VAR`], through a hard link beside it
/// (`xtask-runner`): another session's `cargo xtask` may rebuild `target/xtask` while the tests
/// run, and a link swapped in atomically is never missing while cargo replaces the binary.
pub fn command() -> Result<String> {
    let exe = std::env::current_exe().context("the xtask binary")?;
    let exe = Utf8PathBuf::from_path_buf(exe)
        .map_err(|p| anyhow::anyhow!("the xtask binary's path is not UTF-8: {}", p.display()))?;
    let dir = exe.parent().context("the xtask binary has no directory")?;
    let runner = replace_link(&exe, dir, "xtask-runner")?;
    Ok(format!("{runner} test-runner"))
}

/// Replace this process with `binary` run with `args`, through its link in `run/`.
pub fn exec(binary: &Utf8Path, args: &[OsString]) -> Result<()> {
    let path = link(binary)?;
    let error = std::process::Command::new(&path).args(args).exec();
    Err(error).with_context(|| format!("exec {path}"))
}

/// The hard link of a `deps/` binary in `run/` beside `deps/`. Any other binary is run where it
/// is.
fn link(binary: &Utf8Path) -> Result<Utf8PathBuf> {
    let (Some(deps), Some(name)) = (binary.parent(), binary.file_name()) else {
        return Ok(binary.to_owned());
    };
    let Some(profile) = deps.parent().filter(|_| deps.file_name() == Some("deps")) else {
        return Ok(binary.to_owned());
    };
    let run = profile.join("run");
    std::fs::create_dir_all(&run).with_context(|| format!("create {run}"))?;
    replace_link(binary, &run, name)
}

/// `dir/name` as a hard link of `source`: kept when it already is one, else made (or replaced,
/// when a rebuild wrote a new file) atomically, since nextest starts many processes of one
/// binary at once.
fn replace_link(source: &Utf8Path, dir: &Utf8Path, name: &str) -> Result<Utf8PathBuf> {
    let link = dir.join(name);
    let meta = std::fs::metadata(source).with_context(|| format!("stat {source}"))?;
    if std::fs::metadata(&link).is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino()) {
        return Ok(link);
    }
    let staged = dir.join(format!(".{name}.{}", std::process::id()));
    let _stale = std::fs::remove_file(&staged);
    std::fs::hard_link(source, &staged).with_context(|| format!("link {source} into {dir}"))?;
    std::fs::rename(&staged, &link).with_context(|| format!("rename {staged} to {link}"))?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use camino::Utf8PathBuf;

    use super::link;

    fn scratch(name: &str) -> Utf8PathBuf {
        let dir = std::env::temp_dir().join(format!("xtask-runner-{name}-{}", std::process::id()));
        let _gone = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Utf8PathBuf::from_path_buf(dir).unwrap()
    }

    /// A `deps/` binary runs through a link to the same file in `run/`, relinked when a rebuild
    /// replaced it; anything else runs where it is, and a missing binary is an error.
    #[test]
    fn a_deps_binary_runs_from_run_and_follows_a_rebuild() {
        let profile = scratch("link");
        let deps = profile.join("deps");
        std::fs::create_dir_all(&deps).unwrap();
        let binary = deps.join("codec-0123456789abcdef");
        std::fs::write(&binary, "first").unwrap();

        let linked = link(&binary).unwrap();
        assert_eq!(linked, profile.join("run").join("codec-0123456789abcdef"));
        let ino = |p: &Utf8PathBuf| std::fs::metadata(p).unwrap().ino();
        assert_eq!(ino(&linked), ino(&binary));
        assert_eq!(link(&binary).unwrap(), linked, "an existing link is reused");

        std::fs::remove_file(&binary).unwrap();
        std::fs::write(&binary, "rebuilt").unwrap();
        let relinked = link(&binary).unwrap();
        assert_eq!(ino(&relinked), ino(&binary));
        assert_eq!(std::fs::read_to_string(&relinked).unwrap(), "rebuilt");
        let entries = std::fs::read_dir(profile.join("run")).unwrap().count();
        assert_eq!(entries, 1, "no staged link is left behind");

        let elsewhere = profile.join("doctest-bin");
        std::fs::write(&elsewhere, "x").unwrap();
        assert_eq!(link(&elsewhere).unwrap(), elsewhere);
        let _missing = link(&deps.join("missing")).unwrap_err();
        std::fs::remove_dir_all(&profile).unwrap();
    }
}
