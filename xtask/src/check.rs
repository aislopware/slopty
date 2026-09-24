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
//! A clean run is what an agent reports back. The session that lands the work still gates the
//! index.

use anyhow::{Result, bail};
use xshell::{Shell, cmd};

use crate::tools::{HOST_ONLY_CRATES, TRIPLES, quiet_step, workspace_packages};

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
    let packages: Vec<String> = crates.iter().flat_map(|c| ["-p".to_owned(), c.clone()]).collect();
    let p = &packages;
    let host = TRIPLES[0];
    quiet_step("cargo fmt", cmd!(sh, "cargo +nightly fmt {p...} -- --check"))?;
    quiet_step(
        &format!("clippy {host}"),
        cmd!(sh, "cargo clippy {p...} --all-targets --target {host} -- -D warnings"),
    )?;
    let apple: Vec<String> = crates
        .iter()
        .filter(|c| !HOST_ONLY_CRATES.contains(&c.as_str()))
        .flat_map(|c| ["-p".to_owned(), c.clone()])
        .collect();
    if !apple.is_empty() {
        let ios: Vec<String> =
            TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
        let (a, i) = (&apple, &ios);
        quiet_step("clippy ios + ios-sim", cmd!(sh, "cargo clippy {a...} {i...} -- -D warnings"))?;
    }
    quiet_step("nextest", cmd!(sh, "cargo nextest run {p...} --no-tests=pass"))?;
    let libs: Vec<String> =
        owned.iter().filter(|p| p.lib).flat_map(|p| ["-p".to_owned(), p.name.clone()]).collect();
    if !libs.is_empty() {
        let l = &libs;
        quiet_step("doctests", cmd!(sh, "cargo test {l...} --doc"))?;
    }
    {
        let _env = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
        quiet_step("rustdoc", cmd!(sh, "cargo doc {p...} --no-deps --document-private-items"))?;
    }
    quiet_step("cargo shear", cmd!(sh, "cargo shear {p...}"))?;
    let dirs: Vec<String> = owned.iter().map(|p| p.dir.to_string()).collect();
    let d = &dirs;
    quiet_step("typos", cmd!(sh, "typos {d...}"))?;
    println!("✔ checked {} ({:.1?})", crates.join(", "), started.elapsed());
    Ok(())
}
