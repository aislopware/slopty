//! `xtask gate`: everything that must be green before a commit lands.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::tools::{TRIPLES, has, host_only_present, quiet_step, step};

/// Gate options.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Apply automatic fixes first.
    pub fix: bool,
    /// Fast subset only.
    pub quick: bool,
}

pub fn run(sh: &Shell, opts: Options) -> Result<()> {
    let started = std::time::Instant::now();
    crate::upstream::warn_if_stale();
    fmt(sh, opts.fix)?;
    // The tools that never touch `target/` run beside the cargo steps (which serialise on the
    // build lock anyway); each captures its output and prints it whole when it is done.
    let tools = std::thread::scope(|scope| -> Result<()> {
        let tools = (!opts.quick).then(|| {
            scope.spawn(move || -> Result<()> {
                let sh = Shell::new()?;
                deny(&sh)?;
                shear(&sh, opts.fix)?;
                typos(&sh, opts.fix)?;
                taplo(&sh, opts.fix)?;
                commits(&sh)
            })
        });
        lint(sh)?;
        test(sh, &[])?;
        if !opts.quick {
            doc(sh, false)?;
        }
        match tools {
            Some(handle) => {
                handle.join().map_err(|panic| anyhow::anyhow!("tool thread panicked: {panic:?}"))?
            }
            None => Ok(()),
        }
    });
    tools?;
    println!("✔ gate passed ({:.1?})", started.elapsed());
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

/// Clippy on every triple: the host with every target (tests, benches, examples) in one pass,
/// then the two iOS triples together in one invocation (cargo builds them side by side from
/// one resolve), library and binary targets only — tests never run on iOS, and their code is
/// the same as the host's. Host-only crates are excluded from the iOS pass.
pub fn lint(sh: &Shell) -> Result<()> {
    let host = TRIPLES[0];
    step(
        &format!("clippy {host}"),
        &cmd!(sh, "cargo clippy --workspace --all-targets --target {host} -- -D warnings"),
    )?;
    let excludes: Vec<String> = host_only_present()?
        .into_iter()
        .flat_map(|c| ["--exclude".to_owned(), c.to_owned()])
        .collect();
    let ios: Vec<String> =
        TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
    step(
        "clippy ios + ios-sim",
        &cmd!(sh, "cargo clippy --workspace {excludes...} {ios...} -- -D warnings"),
    )
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
    quiet_step("cargo deny", cmd!(sh, "cargo deny --workspace check"))
}

fn shear(sh: &Shell, fix: bool) -> Result<()> {
    let fix: &[&str] = if fix { &["--fix"] } else { &[] };
    quiet_step("cargo shear", cmd!(sh, "cargo shear {fix...}"))
}

fn typos(sh: &Shell, fix: bool) -> Result<()> {
    let write: &[&str] = if fix { &["-w"] } else { &[] };
    quiet_step("typos", cmd!(sh, "typos {write...}"))
}

/// Only the tracked TOML files: left to its own globs taplo walks `target/` and the vendored
/// trees before its `exclude` list applies, which took ~90 s.
fn taplo(sh: &Shell, apply: bool) -> Result<()> {
    let check: &[&str] = if apply { &[] } else { &["--check"] };
    let files = cmd!(sh, "git ls-files *.toml").read()?;
    let files: Vec<&str> = files.lines().collect();
    quiet_step("taplo", cmd!(sh, "taplo fmt {check...} {files...}"))
}

/// Every commit since the last tag (or the root) follows Conventional Commits.
fn commits(sh: &Shell) -> Result<()> {
    let last_tag = cmd!(sh, "git describe --tags --abbrev=0").quiet().ignore_stderr().read().ok();
    let range = last_tag.map_or_else(|| "HEAD".to_owned(), |t| format!("{}..HEAD", t.trim()));
    quiet_step("committed", cmd!(sh, "committed {range} --no-merge-commit"))
}
