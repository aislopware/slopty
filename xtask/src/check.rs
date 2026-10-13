//! `xtask check -p <crate>…`: the gate's checks narrowed to the crates one agent owns.
//!
//! Several agents edit this checkout at once, each owning some crates. A workspace-wide check
//! would trip over another agent's half-finished crate. This runs every gate step that can be
//! scoped to packages, on those packages only, on the working tree:
//! - fmt;
//! - clippy on the host triple with every target, on both iOS triples unless the crate is
//!   host-only, and on Linux for the crates that build there (`tools::lint_linux`);
//! - nextest (the workspace has no doctest: `gate::runnable_doctests`);
//! - rustdoc with warnings denied;
//! - shear;
//! - typos over the crates' directories.
//!
//! Every build step names [`WORKSPACE_HACK`] beside the crates. Cargo unifies features over the
//! packages one invocation selects, so without it each crate set resolved its dependencies with
//! other features, and every set compiled and kept its own copy of them and of the crates on
//! top (`docs/decisions/tooling.md`, "target/ stays bounded").
//!
//! `--quick` keeps fmt, host clippy, nextest, shear and typos, which catch most of what goes red,
//! in about half the time, and leaves iOS and Linux clippy and rustdoc to the gate
//! (`docs/decisions/tooling.md`, "Lanes never wait on a check").
//!
//! A clean run is what an agent reports back. The session that lands the work still gates the
//! index.

use anyhow::{Result, bail};
use xshell::{Shell, cmd};

use crate::tools::{
    HOST_ONLY_CRATES, TRIPLES, WORKSPACE_HACK, lint_linux, packages_in, quiet_step,
};

/// Which part of the checks to run. A remote check runs the two on runners of their own, side by
/// side: the lints on Linux and the tests on Linux or a Mac, as the crates need
/// (`docs/decisions/tooling.md`, "A remote check lints on Linux").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Part {
    /// Every check.
    #[default]
    All,
    /// fmt, clippy, rustdoc, shear and typos: no test.
    Lint,
    /// The tests alone.
    Tests,
}

pub fn run(sh: &Shell, crates: &[String], quick: bool, part: Part) -> Result<()> {
    if crates.is_empty() {
        bail!("name the crates to check: `cargo xtask check -p <crate> [-p <crate>…]`");
    }
    let started = std::time::Instant::now();
    // The tree the shell stands in: the checkout, a runner's, or a local check's commit.
    let here = camino::Utf8PathBuf::from_path_buf(sh.current_dir())
        .map_err(|p| anyhow::anyhow!("the tree's path is not UTF-8: {}", p.display()))?;
    let workspace = packages_in(&here)?;
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
    if part == Part::Tests {
        if cfg!(target_os = "macos") {
            // As the gate's macOS lanes test on a runner: the binaries the tests spawn built
            // first.
            let names: Vec<&str> = names().collect();
            if let Some(spawned) = crate::gate::spawned_selection(Some(&names)) {
                let bins = crate::gate::spawned_bin_args();
                quiet_step(
                    "spawned binaries",
                    cmd!(sh, "cargo build --profile test {spawned...} {bins...}"),
                )?;
            }
            let _fresh = sh.push_env(crate::gate::BINS_FRESH, "1");
            // Here, beside the person's work and other lanes' builds, the profile `land` takes:
            // the gate's, less the tests that time themselves against the machine's load.
            let local = sh.var_os(crate::local::LOCAL).is_some();
            let profile = if local { "land" } else { crate::gate::NEXTEST_CI_PROFILE };
            nextest(sh, b, &["--profile", profile])?;
        } else {
            // As CI's linux lane tests them: without the workspace hack, whose features pull in
            // the client's GPUI, and with the binaries the tests spawn built first.
            let tested = crate::gate::linux_tested();
            let names: Vec<&str> = names().collect();
            if let Some(other) = names.iter().find(|c| !tested.contains(c)) {
                bail!("{other}'s tests run on a Mac, not on Linux (`gate::linux_tested`)");
            }
            crate::gate::linux_tests(sh, crate::gate::NEXTEST_CI_PROFILE, &names, false)?;
        }
        println!("✔ tested {} ({:.1?})", crates.join(", "), started.elapsed());
        return Ok(());
    }
    quiet_step("cargo fmt", cmd!(sh, "cargo +nightly fmt {p...} -- --check"))?;
    // The live `slopty-e2e` targets, which no test build takes, are linted here.
    let live: &[&str] = if names().any(|c| c == crate::e2e::LIVE_PACKAGE) {
        &["--features", crate::e2e::LIVE]
    } else {
        &[]
    };
    {
        let (cargo, _env) = crate::tools::apple_cargo(sh, false)?;
        quiet_step(
            &format!("clippy {host}"),
            cmd!(
                sh,
                "{cargo} clippy --keep-going {b...} --all-targets {live...} --target {host} -- -D warnings"
            ),
        )?;
    }
    if !quick {
        clippy_ios(sh, &names().collect::<Vec<_>>())?;
        lint_linux(sh, &names().collect::<Vec<_>>())?;
    }
    if part == Part::All {
        nextest(sh, b, &[])?;
    }
    if !quick {
        rustdoc(sh, &names().collect::<Vec<_>>())?;
    }
    quiet_step("cargo shear", cmd!(sh, "cargo shear {p...}"))?;
    // Relative, with `--force-exclude`: `typos.toml`'s excludes hold for the named directories
    // too, as they do for the gate's run over the whole tree; typos matches them against a path
    // as given, so an absolute one escapes them.
    let here = sh.current_dir();
    let dirs: Vec<String> = owned
        .iter()
        .map(|p| p.dir.as_std_path().strip_prefix(&here).unwrap_or(p.dir.as_std_path()))
        .map(|dir| dir.display().to_string())
        .collect();
    let d = &dirs;
    quiet_step("typos", cmd!(sh, "typos --force-exclude {d...}"))?;
    println!("✔ checked {} ({:.1?})", crates.join(", "), started.elapsed());
    Ok(())
}

/// nextest on `built`, the crates and [`WORKSPACE_HACK`] as `-p` arguments, with `profile`'s
/// arguments (none for the default profile).
fn nextest(sh: &Shell, built: &[String], profile: &[&str]) -> Result<()> {
    // Test binaries run out of `run/`, not `deps/` (`crate::runner`).
    let runner = crate::runner::command()?;
    quiet_step(
        "nextest",
        cmd!(sh, "cargo nextest run {built...} {profile...} --no-tests=pass")
            .env(crate::runner::RUNNER_VAR, runner),
    )
}

/// Clippy on both iOS triples for those of `crates` that build there, beside [`WORKSPACE_HACK`].
pub fn clippy_ios(sh: &Shell, crates: &[&str]) -> Result<()> {
    let apple: Vec<&str> =
        crates.iter().copied().filter(|c| !HOST_ONLY_CRATES.contains(c)).collect();
    if apple.is_empty() {
        return Ok(());
    }
    let a = selected(apple.into_iter().chain([WORKSPACE_HACK]));
    let ios: Vec<String> =
        TRIPLES[1..].iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
    let i = &ios;
    let (cargo, _env) = crate::tools::apple_cargo(sh, true)?;
    quiet_step(
        "clippy ios + ios-sim",
        cmd!(sh, "{cargo} clippy --keep-going {a...} {i...} -- -D warnings"),
    )
}

/// rustdoc on `crates` with warnings denied and the private items in, as CI's rustdoc lane.
pub fn rustdoc(sh: &Shell, crates: &[&str]) -> Result<()> {
    let b = selected(crates.iter().copied().chain([WORKSPACE_HACK]));
    let _flags = sh.push_env("RUSTDOCFLAGS", "-D warnings --cfg docsrs");
    // For the host's Apple triple, as CI's rustdoc lane documents it, which off a Mac is no
    // host at all.
    let host = TRIPLES[0];
    let (cargo, _env) = crate::tools::apple_cargo(sh, false)?;
    quiet_step(
        "rustdoc",
        cmd!(
            sh,
            "{cargo} doc --keep-going {b...} --no-deps --document-private-items --target {host}"
        ),
    )
}

/// `-p <name>` for each name.
fn selected<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    names.flat_map(|name| ["-p".to_owned(), name.to_owned()]).collect()
}
