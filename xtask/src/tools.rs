//! Shared helpers: repo root discovery, tool presence, target triples.

use anyhow::{Context as _, Result};
use camino::Utf8PathBuf;
use xshell::{Shell, cmd};

/// Every triple we build for. Host first.
pub const TRIPLES: [&str; 3] =
    ["aarch64-apple-darwin", "aarch64-apple-ios", "aarch64-apple-ios-sim"];

/// The Linux triples [`LINUX_CRATES`] are linted for: glibc, where a Linux desktop's libraries
/// live, on both architectures a Linux box runs. `aarch64` is not `x86_64` with another name: its
/// `c_char` is unsigned, and a PTY or `/proc` reader crosses that.
pub const LINUX_TRIPLES: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

/// The triple [`SERVER_CRATES`] are also linted for. Static musl is how a Linux server ships.
pub const SERVER_TRIPLE: &str = "x86_64-unknown-linux-musl";

/// The server and what it stands on (`docs/decisions/topology.md`, "The server builds for
/// Linux").
pub const SERVER_CRATES: [&str; 8] = [
    "slopty-core",
    "slopty-proto",
    "slopty-settings",
    "slopty-tailnet",
    "slopty-net",
    "slopty-tools",
    "slopty-server",
    "slopty-serverd",
];

/// The crates that build for Linux and must go on doing so: [`SERVER_CRATES`], the worker with
/// every crate under it, and the CLI with the client core, for `slopty worker install` and
/// `slopty hook` on a Linux worker (`docs/decisions/platform.md`, "Linux seams").
pub const LINUX_CRATES: [&str; 21] = [
    "slopty-core",
    "slopty-proto",
    "slopty-settings",
    "slopty-tailnet",
    "slopty-net",
    "slopty-tools",
    "slopty-server",
    "slopty-serverd",
    "slopty-platform",
    "slopty-pty",
    "slopty-ptyd",
    "slopty-agent",
    "slopty-engine",
    "slopty-capture",
    "slopty-input",
    "slopty-codec",
    "slopty-media",
    "slopty-worker",
    "slopty-workerd",
    "slopty-client",
    "slopty-cli",
];

/// The [`LINUX_CRATES`] whose tests are the Mac's: they drive `ScreenCaptureKit`, `CGEvent` or
/// `VideoToolbox`, or take objc2 as a dev-dependency. The Linux lane lints only their libraries
/// and binaries; every other crate's tests build for Linux too.
pub const LINUX_UNTESTED: [&str; 4] =
    ["slopty-capture", "slopty-input", "slopty-worker", "slopty-workerd"];

/// Clippy `-D warnings` for Linux on those of `crates` that build there: every one of
/// [`LINUX_CRATES`] on [`LINUX_TRIPLES`], with its tests unless it is one of
/// [`LINUX_UNTESTED`], and the [`SERVER_CRATES`] among them on [`SERVER_TRIPLE`]. Without the
/// workspace hack: its features pull in the client's GPUI, which no Linux build takes.
pub fn lint_linux(sh: &Shell, crates: &[&str]) -> Result<()> {
    let picked = |keep: &dyn Fn(&str) -> bool| -> Vec<String> {
        crates
            .iter()
            .copied()
            .filter(|c| keep(c))
            .flat_map(|c| ["-p".to_owned(), c.to_owned()])
            .collect()
    };
    let targets: Vec<String> =
        LINUX_TRIPLES.iter().flat_map(|t| ["--target".to_owned(), (*t).to_owned()]).collect();
    let tested = picked(&|c| LINUX_CRATES.contains(&c) && !LINUX_UNTESTED.contains(&c));
    let untested = picked(&|c| LINUX_UNTESTED.contains(&c));
    for (label, set, all_targets) in [("with tests", tested, true), ("libraries", untested, false)]
    {
        if set.is_empty() {
            continue;
        }
        let all_targets = all_targets.then_some("--all-targets");
        let targets = &targets;
        // blake3 compiles its NEON C for aarch64, and this Mac has no C toolchain for Linux.
        // Clippy never links, so its pure-Rust path (the `no_neon` feature, which its build
        // script reads from this variable) lints the same Rust.
        quiet_step(
            &format!("clippy linux-gnu (x86_64 + aarch64), {label}"),
            cmd!(sh, "cargo clippy {set...} {targets...} {all_targets...} -- -D warnings")
                .env("CARGO_FEATURE_NO_NEON", "1"),
        )?;
    }
    let server = picked(&|c| SERVER_CRATES.contains(&c));
    if !server.is_empty() {
        quiet_step(
            &format!("clippy {SERVER_TRIPLE}"),
            cmd!(sh, "cargo clippy {server...} --target {SERVER_TRIPLE} -- -D warnings"),
        )?;
    }
    Ok(())
}

/// The package every `cargo xtask check` build names beside the checked crates, so each crate
/// set resolves the third-party dependencies with the workspace's features and shares their
/// builds (`workspace-hack/src/lib.rs`).
pub const WORKSPACE_HACK: &str = "workspace-hack";

/// Crates that only build on the host triple of the Apple ones: they wrap host-only frameworks
/// (`ScreenCaptureKit`, `CGEvent`, PTYs) or are dev tools. The client crates (`slopty-ui`,
/// `slopty-app`) build for iOS through the fork's `gpui_ios`.
pub const HOST_ONLY_CRATES: [&str; 14] = [
    "slopty-shape",
    "slopty-engine",
    "slopty-pty",
    "slopty-capture",
    "slopty-input",
    "slopty-agent",
    "slopty-worker",
    "slopty-workerd",
    "slopty-ptyd",
    "slopty-cli",
    "slopty-server",
    "slopty-serverd",
    "slopty",
    "xtask",
];

/// The host-only crates that exist in this checkout (cargo rejects `--exclude` of an unknown
/// package, and crates arrive one at a time).
pub fn host_only_present() -> Result<Vec<&'static str>> {
    let packages = workspace_packages()?;
    Ok(HOST_ONLY_CRATES
        .into_iter()
        .filter(|name| packages.iter().any(|p| p.name == *name))
        .collect())
}

/// A workspace member, as its manifest declares it.
pub struct Package {
    pub name: String,
    pub dir: Utf8PathBuf,
    /// It has a library target, so it has doctests.
    pub lib: bool,
}

/// Every package under `crates/`, `apps/` and `xtask`. A package's name need not match its
/// directory (`apps/slopty-server` is `slopty-serverd`), so this reads the manifests.
pub fn workspace_packages() -> Result<Vec<Package>> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        package: Named,
        lib: Option<toml::Table>,
    }
    #[derive(serde::Deserialize)]
    struct Named {
        name: String,
    }
    let root = repo_root()?;
    let mut dirs = vec![root.join("xtask")];
    for group in ["crates", "apps"] {
        for entry in root.join(group).read_dir_utf8().with_context(|| format!("listing {group}"))? {
            dirs.push(entry?.into_path());
        }
    }
    let mut packages = Vec::new();
    for dir in dirs {
        let manifest = dir.join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else { continue };
        let parsed: Manifest =
            toml::from_str(&text).with_context(|| format!("parsing {manifest}"))?;
        let lib = parsed.lib.is_some() || dir.join("src").join("lib.rs").exists();
        packages.push(Package { name: parsed.package.name, dir, lib });
    }
    Ok(packages)
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
