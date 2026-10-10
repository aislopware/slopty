//! Shared helpers: repo root discovery, tool presence, target triples.

use anyhow::{Context as _, Result, ensure};
use camino::{Utf8Path, Utf8PathBuf};
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
pub const LINUX_CRATES: [&str; 22] = [
    "slopty-core",
    "slopty-crash",
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
pub const LINUX_UNTESTED: [&str; 2] = ["slopty-capture", "slopty-input"];

/// The [`LINUX_CRATES`] whose tests build for Linux and are linted there, but are not run: their
/// Mac-only tests are gated to the Mac, and the rest have not yet been run on a Linux host.
pub const LINUX_UNRUN: [&str; 2] = ["slopty-worker", "slopty-workerd"];

/// Clippy `-D warnings` for Linux on those of `crates` that build there: every one of
/// [`LINUX_CRATES`] on [`LINUX_TRIPLES`], with its tests unless it is one of
/// [`LINUX_UNTESTED`], and the [`SERVER_CRATES`] among them on [`SERVER_TRIPLE`]. Without the
/// workspace hack: its features pull in the client's GPUI, which no Linux build takes.
///
/// It runs through `cargo-zigbuild`, with zig as the C compiler for each triple. Clippy never
/// links, but build scripts compile C for the target: ring's, under the server's rustls, has no
/// path without it, and blake3's NEON for `aarch64`. Neither this Mac nor an `x86_64` Ubuntu
/// runner has a C toolchain for every one of these triples; zig is one, and every lane that
/// compiles has it already.
pub fn lint_linux(sh: &Shell, crates: &[&str]) -> Result<()> {
    ensure!(
        has(sh, "cargo-zigbuild"),
        "cargo-zigbuild lints for Linux with zig's C compiler: `cargo binstall cargo-zigbuild`"
    );
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
        quiet_step(
            &format!("clippy linux-gnu (x86_64 + aarch64), {label}"),
            cmd!(
                sh,
                "cargo-zigbuild clippy --keep-going {set...} {targets...} {all_targets...} -- -D warnings"
            ),
        )?;
    }
    let server = picked(&|c| SERVER_CRATES.contains(&c));
    if !server.is_empty() {
        quiet_step(
            &format!("clippy {SERVER_TRIPLE}"),
            cmd!(
                sh,
                "cargo-zigbuild clippy --keep-going {server...} --target {SERVER_TRIPLE} -- -D warnings"
            ),
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
}

/// Every package under `crates/`, `apps/` and `xtask`. A package's name need not match its
/// directory (`apps/slopty-server` is `slopty-serverd`), so this reads the manifests.
pub fn workspace_packages() -> Result<Vec<Package>> {
    packages_in(&repo_root()?)
}

/// [`workspace_packages`] of the tree at `root`: a gate's snapshot, or the checkout.
pub fn packages_in(root: &Utf8Path) -> Result<Vec<Package>> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        package: Named,
    }
    #[derive(serde::Deserialize)]
    struct Named {
        name: String,
    }
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
        packages.push(Package { name: parsed.package.name, dir });
    }
    Ok(packages)
}

/// The directory cargo builds into, wherever `CARGO_TARGET_DIR` or the config put it.
pub fn target_dir(sh: &Shell) -> Result<Utf8PathBuf> {
    #[derive(serde::Deserialize)]
    struct Metadata {
        target_directory: Utf8PathBuf,
    }
    let json = cmd!(sh, "cargo metadata --format-version 1 --no-deps").quiet().read()?;
    Ok(serde_json::from_str::<Metadata>(&json)?.target_directory)
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

/// The zig the vendored ghostty builds with: its `build.zig` minimum, and CI's `setup-zig`.
pub const ZIG: &str = "0.16";

/// Homebrew's way to that zig, kept from being shadowed or upgraded by a newer `zig` (an
/// unrelated `brew install` upgraded it to 0.17 once, which ghostty's `build.zig` refuses).
const ZIG_HOW: &str = "`brew unlink zig; brew install zig@0.16 && brew link --force zig@0.16 \
                       && brew pin zig@0.16`";

/// What is wrong with the zig on `PATH` for building libghostty-vt, if anything.
pub fn zig_problem(sh: &Shell) -> Option<String> {
    let Ok(version) = cmd!(sh, "zig version").quiet().ignore_stderr().read() else {
        return Some(format!("zig {ZIG} is needed to build libghostty-vt: {ZIG_HOW}"));
    };
    let version = version.trim();
    let fits =
        version.strip_prefix(ZIG).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
    (!fits).then(|| format!("zig {version} is on PATH; libghostty-vt builds with {ZIG}: {ZIG_HOW}"))
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
