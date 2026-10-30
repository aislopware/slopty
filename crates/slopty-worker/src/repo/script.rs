//! The shell a repository's setup runs in (`docs/decisions/projects.md`): the person's login
//! shell, interactive, as their own terminal runs a command, so their `PATH`, toolchains and
//! aliases apply.

/// The person's login shell, as the worker's environment names it; `/bin/sh` when it does not.
#[must_use]
pub fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}
