//! Shared helpers: repo root discovery, tool presence, target triples.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use xshell::{Shell, cmd};

/// Every triple we build for. Host first.
pub const TRIPLES: [&str; 3] =
    ["aarch64-apple-darwin", "aarch64-apple-ios", "aarch64-apple-ios-sim"];

/// Crates that only build on the host triple (they wrap host-only frameworks or are dev tools),
/// plus the ones waiting on the GPUI iOS backend (`slopty-ui`; see DECISIONS.md "iOS backend"):
/// they leave this list when the fork gains `gpui_ios`.
pub const HOST_ONLY_CRATES: [&str; 12] = [
    "slopty-engine",
    "slopty-pty",
    "slopty-capture",
    "slopty-input",
    "slopty-agent",
    "slopty-host",
    "slopty-hostd",
    "slopty-ptyd",
    "slopty-cli",
    "slopty",
    "slopty-ui",
    "xtask",
];

/// The host-only crates that exist in this checkout (cargo rejects `--exclude` of an unknown
/// package, and crates arrive one at a time).
pub fn host_only_present() -> Result<Vec<&'static str>> {
    let root = repo_root()?;
    Ok(HOST_ONLY_CRATES
        .into_iter()
        .filter(|name| {
            let dir =
                if *name == "xtask" { root.join("xtask") } else { root.join("crates").join(name) };
            dir.exists() || root.join("apps").join(name).exists()
        })
        .collect())
}

/// The repository root: the directory containing the workspace `Cargo.toml`.
pub fn repo_root() -> Result<Utf8PathBuf> {
    let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Utf8PathBuf::from)
        .context("xtask must live one level below the repository root")
}

/// Whether `name` is on `PATH`.
pub fn has(sh: &Shell, name: &str) -> bool {
    cmd!(sh, "which {name}").quiet().ignore_stderr().read().is_ok()
}

/// Run a command, printing it first so the log reads like a script.
pub fn step(title: &str, command: &xshell::Cmd<'_>) -> Result<()> {
    println!("▶ {title}");
    command.run().with_context(|| format!("step failed: {title}"))
}
