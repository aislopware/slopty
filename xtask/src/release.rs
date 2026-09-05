//! `xtask release`: version bump + changelog + tag, all derived from Conventional Commits.
//!
//! Flow: clean tree → `git cliff --bumped-version` (or an explicit version) → write
//! `[workspace.package].version` → `cargo update --workspace` (lockfile) → regenerate
//! `CHANGELOG.md` → commit `chore(release): vX.Y.Z` → annotated tag `vX.Y.Z`. Pushing the tag is
//! left to the operator (CI builds the artifacts from it).

use anyhow::{Context as _, Result, bail};
use xshell::{Shell, cmd};

use crate::tools::step;

/// Release options.
#[derive(Clone, Debug)]
pub struct Options {
    /// Explicit version (`1.2.3`); computed from commits when `None`.
    pub version: Option<String>,
    /// Print what would happen and stop before writing anything.
    pub dry_run: bool,
    /// Skip `cargo gate` (it must have passed on this exact tree).
    pub skip_gate: bool,
}

pub fn run(sh: &Shell, opts: &Options) -> Result<()> {
    let dirty = cmd!(sh, "git status --porcelain").read()?;
    if !dirty.trim().is_empty() && !opts.dry_run {
        bail!("working tree is dirty; commit or stash first");
    }
    let version = match &opts.version {
        Some(v) => v.trim_start_matches('v').to_owned(),
        None => cmd!(sh, "git cliff --bumped-version")
            .read()
            .context("git-cliff (cargo binstall git-cliff)")?
            .trim()
            .trim_start_matches('v')
            .to_owned(),
    };
    let tag = format!("v{version}");
    let current = current_version(sh)?;
    println!("release: {current} → {version} (tag {tag})");
    if opts.dry_run {
        let preview = cmd!(sh, "git cliff --unreleased --tag {tag} --strip all").read()?;
        println!("{preview}");
        return Ok(());
    }
    let tag_exists = cmd!(sh, "git rev-parse -q --verify refs/tags/{tag}")
        .quiet()
        .ignore_stderr()
        .read()
        .is_ok();
    if tag_exists {
        bail!("{tag} already exists; no releasable commits since");
    }
    if !opts.skip_gate {
        crate::gate::run(sh, crate::gate::Options { fix: false, quick: false })?;
    }
    set_version(sh, &version)?;
    step("cargo update --workspace", &cmd!(sh, "cargo update --workspace --offline"))?;
    step("changelog", &cmd!(sh, "git cliff --tag {tag} -o CHANGELOG.md"))?;
    step("commit", &cmd!(sh, "git add Cargo.toml Cargo.lock CHANGELOG.md"))?;
    let message = format!("chore(release): {tag}");
    step("commit", &cmd!(sh, "git commit -q -m {message}"))?;
    step("tag", &cmd!(sh, "git tag -a {tag} -m {message}"))?;
    println!("✔ {tag} committed and tagged; `git push --follow-tags` when ready");
    Ok(())
}

/// `[workspace.package].version` in the root manifest.
pub fn current_version(sh: &Shell) -> Result<String> {
    let manifest = sh.read_file("Cargo.toml")?;
    workspace_version_line(&manifest)
        .and_then(|line| line.split('"').nth(1))
        .map(str::to_owned)
        .context("no [workspace.package] version in Cargo.toml")
}

/// The `version = "..."` line inside `[workspace.package]`.
fn workspace_version_line(manifest: &str) -> Option<&str> {
    let mut in_section = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
            continue;
        }
        if in_section && trimmed.starts_with("version") {
            return Some(line);
        }
    }
    None
}

fn set_version(sh: &Shell, version: &str) -> Result<()> {
    let manifest = sh.read_file("Cargo.toml")?;
    let line = workspace_version_line(&manifest).context("workspace version line")?.to_owned();
    let updated = manifest.replacen(&line, &format!("version = \"{version}\""), 1);
    sh.write_file("Cargo.toml", updated)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_workspace_version_not_package_versions() {
        let manifest = "[package]\nversion = \"9.9.9\"\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n";
        assert_eq!(workspace_version_line(manifest), Some("version = \"0.1.0\""));
    }
}
