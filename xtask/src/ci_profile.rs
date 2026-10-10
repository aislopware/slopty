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
         `$CARGO_HOME/config.toml`.\n",
    );
    for name in names {
        text.push_str("\n[profile.dev.package.");
        text.push_str(name);
        text.push_str("]\nopt-level = 0\n");
    }
    text
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
    use super::{PATH, proc_macros, render};
    use crate::tools::repo_root;

    /// The profile names every proc macro `Cargo.lock` brings in, and nothing else: a dependency
    /// bump that adds one compiles it optimised on CI again until the file is written anew.
    #[test]
    fn the_ci_profile_names_every_proc_macro() {
        let root = repo_root().unwrap();
        let sh = xshell::Shell::new().unwrap();
        sh.change_dir(&root);
        let names = proc_macros(&sh).unwrap();
        assert!(names.contains("serde_derive"), "{names:?}");
        assert!(!names.contains("syn"), "a library, cached and kept optimised");
        let kept = std::fs::read_to_string(root.join(PATH)).unwrap();
        assert!(kept == render(&names), "{PATH} is stale: run `cargo xtask ci-profile`");
    }
}
