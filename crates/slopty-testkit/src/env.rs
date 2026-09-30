//! The environment a spawned daemon or agent starts from: this machine's, never the person's.
//!
//! A test runs inside the developer's shell, which may hold their Claude Code settings and
//! credentials (`CLAUDE_*`, `ANTHROPIC_*`), the Slopty terminal it runs in (`SLOPTY_SESSION`,
//! `SLOPTY_SESSION_TOKEN`), a `PATH` that finds the real `claude`, and a home full of dotfiles.
//! Whatever a daemon inherits, every shell and agent it starts inherits too. So a test starts
//! each process it spawns from [`scrub`]: the environment cleared, then only what makes the
//! machine work kept ([`KEPT`]), `PATH` set to the system's own ([`PATH`]), and `HOME` a
//! directory of the test's own. What a test needs beyond that it sets itself, after.

use std::ffi::OsStr;
use std::path::Path;

/// The system's own directories, in which a test finds `sh`, `cat` and the rest, and no
/// program the person installed: no real `claude` among them.
pub const PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// What is kept of the test's own environment: the locale, the terminal type, who the user is,
/// the temporary directory, and what the Rust toolchain reads (logging, backtraces, coverage).
pub const KEPT: [&str; 14] = [
    "TMPDIR",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "__CF_USER_TEXT_ENCODING",
    "RUST_LOG",
    "RUST_BACKTRACE",
    "RUST_LIB_BACKTRACE",
    "LLVM_PROFILE_FILE",
];

/// Whether the variable `name` is kept.
#[must_use]
pub fn kept(name: &OsStr) -> bool {
    KEPT.iter().any(|k| OsStr::new(k) == name)
}

/// Start `command` from a clean environment: only [`KEPT`] of this process's, [`PATH`], and
/// `HOME` at `home`, which is made if it is not there.
///
/// # Panics
///
/// When `home` cannot be made.
pub fn scrub<'a>(
    command: &'a mut std::process::Command,
    home: &Path,
) -> &'a mut std::process::Command {
    let made = std::fs::create_dir_all(home);
    assert!(made.is_ok(), "a test's home at {}: {made:?}", home.display());
    command.env_clear();
    command.envs(std::env::vars_os().filter(|(name, _)| kept(name)));
    command.env("PATH", PATH).env("HOME", home)
}

/// [`PATH`] with `first` searched ahead of it: where a test puts its stand-in programs.
#[must_use]
pub fn path_with(first: &Path) -> std::ffi::OsString {
    let mut path = first.as_os_str().to_owned();
    path.push(":");
    path.push(PATH);
    path
}

/// A digest of the value `value`, to tell whether a child inherited a variable without
/// writing its value anywhere: the same in every process of one build.
#[must_use]
pub fn digest(value: &OsStr) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Every variable of this process, by name, as [`digest`]s: what a spawned child records.
#[must_use]
pub fn digests() -> std::collections::BTreeMap<String, u64> {
    std::env::vars_os()
        .map(|(name, value)| (name.to_string_lossy().into_owned(), digest(&value)))
        .collect()
}

/// What every Slopty terminal sets for itself (`slopty_pty`), whatever its daemon had: the
/// same value in a test run inside a Slopty terminal is no leak.
pub const TERMINAL: [&str; 3] = ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];

/// The variables of this test process that `child` (a spawned process's [`digests`]) holds
/// with the same value, besides those [`scrub`] passes on and a terminal sets
/// ([`TERMINAL`]): what leaked into it.
#[must_use]
pub fn leaked(child: &std::collections::BTreeMap<String, u64>) -> Vec<String> {
    digests()
        .into_iter()
        .filter(|(name, value)| {
            let passed = kept(OsStr::new(name))
                || ["PATH", "HOME"].contains(&name.as_str())
                || TERMINAL.contains(&name.as_str());
            !passed && child.get(name) == Some(value)
        })
        .map(|(name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spawned process sees what [`scrub`] keeps and nothing else of the test's: here the
    /// test's own environment, whatever it holds, against what `env` prints in the child.
    #[test]
    fn a_scrubbed_process_sees_only_the_kept_variables_and_its_own_home() {
        let home = std::env::temp_dir().join(format!("slopty-testkit-home-{}", std::process::id()));
        let mut env = std::process::Command::new("/usr/bin/env");
        let out = scrub(&mut env, &home).env("SLOPTY_WORKER_NAME", "set after").output();
        let out = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let _gone = std::fs::remove_dir_all(&home);
        let names: Vec<&str> =
            out.lines().filter_map(|l| l.split_once('=')).map(|(n, _)| n).collect();
        let allowed = |name: &str| {
            kept(OsStr::new(name)) || ["PATH", "HOME", "SLOPTY_WORKER_NAME"].contains(&name)
        };
        assert!(names.iter().all(|n| allowed(n)), "{names:?}");
        assert!(out.lines().any(|l| l == format!("PATH={PATH}")), "{out}");
        assert!(out.lines().any(|l| l == format!("HOME={}", home.display())), "{out}");
        assert!(out.lines().any(|l| l == "SLOPTY_WORKER_NAME=set after"), "{out}");
    }
}
