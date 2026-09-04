//! `xtask gate`: everything that must be green before a commit lands.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, has, host_only_present, step};

/// Gate options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Apply automatic fixes first.
    pub fix: bool,
    /// Fast subset only.
    pub quick: bool,
}

pub fn run(sh: &Shell, opts: Options) -> Result<()> {
    fmt(sh, opts.fix)?;
    lint(sh)?;
    test(sh, &[])?;
    if opts.quick {
        return Ok(());
    }
    doc(sh, false)?;
    deny(sh)?;
    shear(sh, opts.fix)?;
    typos(sh, opts.fix)?;
    taplo(sh, opts.fix)?;
    println!("✔ gate passed");
    Ok(())
}

/// Format check (or apply). Uses nightly rustfmt when available for the unstable options.
pub fn fmt(sh: &Shell, apply: bool) -> Result<()> {
    let nightly =
        cmd!(sh, "rustup run nightly rustfmt --version").quiet().ignore_stderr().read().is_ok();
    let toolchain: &[&str] = if nightly { &["+nightly"] } else { &[] };
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    step("cargo fmt", &cmd!(sh, "cargo {toolchain...} fmt --all -- {check...}"))?;
    if has(sh, "taplo") {
        taplo(sh, apply)?;
    }
    Ok(())
}

/// Clippy on every triple. Host-only crates are excluded from the iOS passes.
pub fn lint(sh: &Shell) -> Result<()> {
    for triple in TRIPLES {
        let excludes: Vec<String> = if triple == TRIPLES[0] {
            Vec::new()
        } else {
            host_only_present()?
                .into_iter()
                .flat_map(|c| ["--exclude".to_owned(), c.to_owned()])
                .collect()
        };
        step(
            &format!("clippy {triple}"),
            &cmd!(
                sh,
                "cargo clippy --workspace {excludes...} --all-targets --target {triple} -- -D warnings"
            ),
        )?;
    }
    Ok(())
}

pub fn test(sh: &Shell, extra: &[String]) -> Result<()> {
    step("nextest", &cmd!(sh, "cargo nextest run --workspace {extra...}"))?;
    step("doctests", &cmd!(sh, "cargo test --workspace --doc"))
}

pub fn doc(sh: &Shell, open: bool) -> Result<()> {
    let open: &[&str] = if open { &["--open"] } else { &[] };
    let _env = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
    step("rustdoc", &cmd!(sh, "cargo doc --workspace --no-deps --document-private-items {open...}"))
}

fn deny(sh: &Shell) -> Result<()> {
    step("cargo deny", &cmd!(sh, "cargo deny --workspace check"))
}

fn shear(sh: &Shell, fix: bool) -> Result<()> {
    let fix: &[&str] = if fix { &["--fix"] } else { &[] };
    step("cargo shear", &cmd!(sh, "cargo shear {fix...}"))
}

fn typos(sh: &Shell, fix: bool) -> Result<()> {
    let write: &[&str] = if fix { &["-w"] } else { &[] };
    step("typos", &cmd!(sh, "typos {write...}"))
}

fn taplo(sh: &Shell, apply: bool) -> Result<()> {
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    step("taplo", &cmd!(sh, "taplo fmt {check...}"))
}
