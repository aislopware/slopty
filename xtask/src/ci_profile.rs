//! `.cargo/ci-profile.toml`: every proc-macro dependency compiled unoptimised, on CI only.
//!
//! The workspace's `[profile.dev.package."*"]` gives every dependency opt-level 3, and cargo's
//! precedence puts that above `build-override`, so the proc macros compiled optimised too. sccache
//! never caches a proc macro (it links), so each CI lane paid 190–316 CPU-seconds for them on
//! every run, and the units that expand them waited (`.research/dev-speed-2026-10-10.md` item 4).
//! Only the macro crates themselves go to opt-level 0: `syn`, `quote` and `proc-macro2`, which
//! do the parsing, are libraries sccache caches, and stay optimised, so expansion stays fast.
//! What a macro expands to does not depend on how it was compiled.
//!
//! Cargo reads package overrides from config files but not from the environment, so each CI job
//! copies the file to `$CARGO_HOME/config.toml`. Here the macros compile once per target dir,
//! and the file is not read.
//!
//! The file also carries CI's network settings. Cargo's own git (libgit2) failed a gate run on
//! "SSL error: unknown error; class=Ssl (16)" cloning a git dependency (run 38144514113), and
//! cargo retries no libgit2 error of that class. The `git` command's failures are retried, and
//! five times rather than three. They live here, not in `CARGO_NET_*` variables, because sccache
//! hashes every `CARGO_*` variable into each unit's key: a new one would recompile every lane.

use std::collections::BTreeSet;

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use xshell::{Shell, cmd};

/// Where the profile lives, from the repository root.
pub const PATH: &str = ".cargo/ci-profile.toml";

/// The names of the proc-macro packages the workspace depends on, members aside.
pub fn proc_macros(sh: &Shell) -> Result<BTreeSet<String>> {
    #[derive(serde::Deserialize)]
    struct Metadata {
        packages: Vec<Package>,
    }
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
        source: Option<String>,
        targets: Vec<Target>,
    }
    #[derive(serde::Deserialize)]
    struct Target {
        kind: Vec<String>,
    }
    let json = cmd!(sh, "cargo metadata --format-version 1 --locked").quiet().read()?;
    let metadata: Metadata = serde_json::from_str(&json).context("cargo metadata")?;
    Ok(metadata
        .packages
        .into_iter()
        .filter(|p| p.source.is_some())
        .filter(|p| p.targets.iter().any(|t| t.kind.iter().any(|k| k == "proc-macro")))
        .map(|p| p.name)
        .collect())
}

/// The profile's text for `names`.
pub fn render(names: &BTreeSet<String>) -> String {
    let mut text = String::from(
        "# Written by `cargo xtask ci-profile` (xtask/src/ci_profile.rs): every proc-macro \
         dependency\n# unoptimised on CI, where sccache cannot cache one. CI copies this to \
         `$CARGO_HOME/config.toml`.\n\n# Git dependencies fetched by the git command, whose \
         failures cargo retries.\n[net]\ngit-fetch-with-cli = true\nretry = 5\n",
    );
    for name in names {
        text.push_str("\n[profile.dev.package.");
        text.push_str(name);
        text.push_str("]\nopt-level = 0\n");
    }
    text
}

/// The tools lane's check: the profile names every proc macro `Cargo.lock` brings in, and
/// nothing else. A dependency bump that adds one would compile it optimised on CI again until
/// the file is written anew. A check of the text here, not a test: `cargo metadata` over the
/// whole graph took 9–48 s inside a Mac test shard, and seconds on Linux beside the other tool
/// checks.
pub fn check(sh: &Shell) -> Result<()> {
    let names = proc_macros(sh)?;
    let kept = std::fs::read_to_string(sh.current_dir().join(PATH))
        .with_context(|| format!("read {PATH}"))?;
    match stale(&names, &kept) {
        Some(why) => anyhow::bail!("{PATH} is stale ({why}): run `cargo xtask ci-profile`"),
        None => Ok(()),
    }
}

/// Why `kept` is not the profile for `names`; `None` when it is.
fn stale(names: &BTreeSet<String>, kept: &str) -> Option<String> {
    if !names.contains("serde_derive") || names.contains("syn") {
        return Some("cargo metadata named the wrong crates as proc macros".to_owned());
    }
    let missing: Vec<&str> = names
        .iter()
        .filter(|n| !kept.contains(&format!("[profile.dev.package.{n}]")))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Some(format!("it lacks {}", missing.join(", ")));
    }
    (kept != render(names)).then(|| "it names crates that are no proc macro".to_owned())
}

/// `cargo xtask ci-profile`: write the profile for the dependencies `Cargo.lock` holds now.
pub fn write(sh: &Shell) -> Result<()> {
    let path = Utf8PathBuf::from(PATH);
    let text = render(&proc_macros(sh)?);
    std::fs::write(sh.current_dir().join(&path), text).with_context(|| format!("write {path}"))?;
    println!("wrote {path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{render, stale};

    fn names(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_profile_naming_every_proc_macro_and_no_other_is_fresh() {
        let macros = names(&["serde_derive", "thiserror-impl"]);
        assert_eq!(stale(&macros, &render(&macros)), None);
        // A bump that brings a new macro, and one that drops one.
        let grown = names(&["serde_derive", "thiserror-impl", "strum_macros"]);
        assert!(stale(&grown, &render(&macros)).is_some_and(|w| w.contains("strum_macros")));
        let shrunk = names(&["serde_derive"]);
        assert!(stale(&shrunk, &render(&macros)).is_some_and(|w| w.contains("no proc macro")));
        // A metadata read that went wrong is never taken for a fresh profile.
        assert!(stale(&names(&["syn", "serde_derive"]), "").is_some());
        assert!(stale(&BTreeSet::new(), &render(&BTreeSet::new())).is_some());
    }
}
