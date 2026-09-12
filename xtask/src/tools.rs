//! Shared helpers: repo root discovery, tool presence, target triples.

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use xshell::{Shell, cmd};

/// Every triple we build for. Host first.
pub const TRIPLES: [&str; 3] =
    ["aarch64-apple-darwin", "aarch64-apple-ios", "aarch64-apple-ios-sim"];

/// Crates that only build on the host triple: they wrap host-only frameworks (`ScreenCaptureKit`,
/// `CGEvent`, PTYs) or are dev tools. The client crates (`slopty-ui`, `slopty-app`) build for iOS
/// through the fork's `gpui_ios`.
pub const HOST_ONLY_CRATES: [&str; 11] = [
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

/// Run a command, printing it first so the log reads like a script, and its wall time after.
pub fn step(title: &str, command: &xshell::Cmd<'_>) -> Result<()> {
    println!("▶ {title}");
    let started = std::time::Instant::now();
    let result = command.run().with_context(|| format!("step failed: {title}"));
    println!("  {} {title} ({:.1?})", if result.is_ok() { "✓" } else { "✘" }, started.elapsed());
    result
}

/// Run a command with its output captured, for steps that run beside others: the log stays
/// readable because everything the tool printed comes out under one header when it is done.
pub fn quiet_step(title: &str, command: xshell::Cmd<'_>) -> Result<()> {
    use std::fmt::Write as _;

    let started = std::time::Instant::now();
    let output = command
        .quiet()
        .ignore_status()
        .output()
        .with_context(|| format!("step failed to start: {title}"))?;
    let ok = output.status.success();
    let mut text = format!("▶ {title}\n");
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let mark = if ok { "✓" } else { "✘" };
    let _written = writeln!(text, "  {mark} {title} ({:.1?})", started.elapsed());
    print!("{text}");
    anyhow::ensure!(ok, "step failed: {title} ({})", output.status);
    Ok(())
}
