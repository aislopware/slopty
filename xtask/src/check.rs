//! `xtask check -p <crate>…`: the gate's checks narrowed to the crates one agent owns.
//!
//! Several agents edit this checkout at once, each owning some crates. A workspace-wide check
//! would trip over another agent's half-finished crate. This runs every gate step that can be
//! scoped to packages, on those packages only, on the working tree:
//! - fmt;
//! - clippy on the host triple with every target, and on both iOS triples unless the crate is
//!   host-only;
//! - nextest and doctests;
//! - rustdoc with warnings denied;
//! - shear;
//! - typos over the crates' directories.
//!
//! Every build step names [`WORKSPACE_HACK`] beside the crates. Cargo unifies features over the
//! packages one invocation selects, so without it each crate set resolved its dependencies with
//! other features, and every set compiled and kept its own copy of them and of the crates on
//! top (`docs/decisions/tooling.md`, "target/ stays bounded").
//!
//! A clean run is what an agent reports back. The session that lands the work still gates the
//! index.

use anyhow::{Result, bail};
use xshell::{Shell, cmd};

use crate::tools::{HOST_ONLY_CRATES, TRIPLES, WORKSPACE_HACK, quiet_step, workspace_packages};

pub fn run(sh: &Shell, crates: &[String]) -> Result<()> {
    if crates.is_empty() {
        bail!("name the crates to check: `cargo xtask check -p <crate> [-p <crate>…]`");
    }
    let started = std::time::Instant::now();
    let workspace = workspace_packages()?;
    let mut owned = Vec::new();
    for name in crates {
        match workspace.iter().find(|p| &p.name == name) {
            Some(p) => owned.push(p),
            None => bail!("no workspace package is named `{name}`"),
        }
    }
    let names = || crates.iter().map(String::as_str);
    let packages = selected(names());
    let built = selected(names().chain([WORKSPACE_HACK]));
    let (p, b) = (&packages, &built);
    let host = TRIPLES[0];
    quiet_step("cargo fmt", cmd!(sh, "cargo +nightly fmt {p...} -- --check"))?;
    quiet_step(
        &format!("clippy {host}"),
        cmd!(sh, "cargo clippy {b...} --all-targets --target {host} -- -D warnings"),
    )?;
    let apple: Vec<&str> = names().filter(|c| !HOST_ONLY_CRATES.contains(c)).collect();
    if !apple.is_empty() {
        let a = selected(apple.into_iter().chain([WORKSPACE_HACK]));
        let ios: Vec<String> =
            TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
        let i = &ios;
        quiet_step("clippy ios + ios-sim", cmd!(sh, "cargo clippy {a...} {i...} -- -D warnings"))?;
    }
    quiet_step("nextest", cmd!(sh, "cargo nextest run {b...} --no-tests=pass"))?;
    let libs: Vec<&str> = owned.iter().filter(|p| p.lib).map(|p| p.name.as_str()).collect();
    if !libs.is_empty() {
        let l = selected(libs.into_iter().chain([WORKSPACE_HACK]));
        quiet_step("doctests", cmd!(sh, "cargo test {l...} --doc"))?;
    }
    {
        let _env = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
        quiet_step("rustdoc", cmd!(sh, "cargo doc {b...} --no-deps --document-private-items"))?;
    }
    quiet_step("cargo shear", cmd!(sh, "cargo shear {p...}"))?;
    let dirs: Vec<String> = owned.iter().map(|p| p.dir.to_string()).collect();
    let d = &dirs;
    quiet_step("typos", cmd!(sh, "typos {d...}"))?;
    println!("✔ checked {} ({:.1?})", crates.join(", "), started.elapsed());
    Ok(())
}

/// `-p <name>` for each name.
fn selected<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    names.flat_map(|name| ["-p".to_owned(), name.to_owned()]).collect()
}
