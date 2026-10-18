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

/// The triple xtask itself runs on, which a check that calls no Apple framework builds for: the
/// Mac's, or a Linux runner's, where the Deep workflow runs those checks so they hold none of the
/// five macOS runners (`docs/decisions/tooling.md`, "Deep's macOS jobs wait for the gate").
pub const HOST: &str = if cfg!(target_os = "macos") {
    TRIPLES[0]
} else if cfg!(target_arch = "aarch64") {
    LINUX_TRIPLES[1]
} else {
    LINUX_TRIPLES[0]
};

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
/// Slopty's tools (`slopty mcp`) on a Linux worker (`docs/decisions/platform.md`, "Linux seams").
pub const LINUX_CRATES: [&str; 29] = [
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
    // Pure crates, which CI tests on Linux alone (`gate::LINUX_ONLY_TESTS`), and the deploy
    // crate and the test kit, which it tests on both.
    "slopty-grid",
    "slopty-predict",
    "slopty-shape",
    "slopty-push",
    "slopty-theme",
    "slopty-deploy",
    "slopty-testkit",
];

/// The [`LINUX_CRATES`] whose tests are the Mac's: they drive `ScreenCaptureKit`, `CGEvent` or
/// `VideoToolbox`, or take objc2 as a dev-dependency. The Linux lane lints only their libraries
/// and binaries; every other crate's tests build for Linux too.
pub const LINUX_UNTESTED: [&str; 2] = ["slopty-capture", "slopty-input"];

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

/// The cargo that lints or documents an Apple target on this host (`ios`: the iOS triples), and
/// the environment it needs for that, held while the returned guards live.
///
/// On a Mac, cargo. Elsewhere (CI's Linux runners, where the lint jobs run so the five macOS
/// slots stay with the tests: `docs/decisions/tooling.md`, "Apple targets are linted on
/// Linux"), zig compiles the C and Objective-C build scripts compile for the target (ring,
/// blake3, `objc_exception`): through `cargo-zigbuild` for macOS, and for iOS, which it refuses,
/// through links to xtask that run as zig's compiler (`zig_cc`), named as each iOS triple's `CC`
/// and `CXX`, with an empty stand-in for
/// the SDK `cc` asks for. `DOCS_RS` is set either way: libghostty-vt-sys then skips its zig build
/// of the library, which asks `xcrun` for the SDKs and which a check never links. The only other
/// build scripts in the Apple graphs that read it are objc-sys, for non-Apple targets alone, and
/// serde's, for a check-cfg line.
pub fn apple_cargo(sh: &Shell, ios: bool) -> Result<(&'static str, Vec<xshell::PushEnv<'_>>)> {
    if cfg!(target_os = "macos") {
        return Ok(("cargo", Vec::new()));
    }
    let mut env = vec![sh.push_env("DOCS_RS", "1")];
    if !ios {
        ensure!(
            has(sh, "cargo-zigbuild"),
            "an Apple target is linted off a Mac with zig's C compiler: `cargo binstall cargo-zigbuild`"
        );
        return Ok(("cargo-zigbuild", env));
    }
    // `cc` takes `SDKROOT` for the SDK only when it names a directory; zig's compiler drops it.
    let target = repo_root()?.join("target");
    let sdk = target.join("apple-sdk-stand-in");
    std::fs::create_dir_all(&sdk).with_context(|| format!("create {sdk}"))?;
    env.push(sh.push_env("SDKROOT", &sdk));
    for (triple, zig) in
        [("aarch64_apple_ios", "aarch64-ios"), ("aarch64_apple_ios_sim", "aarch64-ios-simulator")]
    {
        let (cc, cxx) = crate::zig_cc::links(&target.join("zig-cc"), zig)?;
        env.push(sh.push_env(format!("CC_{triple}"), cc));
        env.push(sh.push_env(format!("CXX_{triple}"), cxx));
        env.push(sh.push_env(format!("AR_{triple}"), "zig ar"));
    }
    Ok(("cargo", env))
}

/// The package every `cargo xtask check` build names beside the checked crates, so each crate
/// set resolves the third-party dependencies with the workspace's features and shares their
/// builds (`workspace-hack/src/lib.rs`).
pub const WORKSPACE_HACK: &str = "workspace-hack";

/// Crates that only build on the host triple of the Apple ones: they wrap host-only frameworks
/// (`ScreenCaptureKit`, `CGEvent`, PTYs, `libproc`) or are dev tools. The client crates
/// (`slopty-ui`, `slopty-app`) build for iOS through the fork's `gpui_ios`, and with them the
/// test protocol of `slopty-e2e`, whose default harness spawns PTYs.
pub const HOST_ONLY_CRATES: [&str; 15] = [
    "slopty-e2e",
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
    // The one vendored member: the GPUI-tokio bridge the shell runs on.
    let mut dirs = vec![root.join("xtask"), root.join("vendor/gpui_tokio")];
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

/// The repository root: the directory containing the workspace `Cargo.toml`. A test reads the
/// tree it was built from, never the checkout [`REPO_ROOT`] names: under the gate, xtask's tests
/// run in the index's snapshot with the variable inherited from the gate's own xtask.
pub fn repo_root() -> Result<Utf8PathBuf> {
    let named = if cfg!(test) { None } else { std::env::var(REPO_ROOT).ok() };
    root_from(named, env!("CARGO_MANIFEST_DIR"))
}

/// The checkout an xtask built elsewhere works on: the gate's run with xtask built from its
/// snapshot of the index (`gate::by_the_index_s_xtask`) names the real checkout here, since its
/// binary's manifest is the snapshot's.
pub const REPO_ROOT: &str = "SLOPTY_REPO_ROOT";

/// The repository root: `named` when given, else the directory above xtask's manifest `manifest`.
fn root_from(named: Option<String>, manifest: &str) -> Result<Utf8PathBuf> {
    if let Some(named) = named.filter(|n| !n.is_empty()) {
        return Ok(Utf8PathBuf::from(named));
    }
    Utf8Path::new(manifest)
        .parent()
        .map(Utf8PathBuf::from)
        .context("xtask must live one level below the repository root")
}

/// Whether `name` is on `PATH`.
pub fn has(sh: &Shell, name: &str) -> bool {
    cmd!(sh, "which {name}").quiet().ignore_stderr().read().is_ok()
}

/// Whether the selected Xcode can compile Metal shaders: `metal` is a shim that fails until
/// the Metal toolchain component is downloaded.
pub fn has_metal(sh: &Shell) -> bool {
    cmd!(sh, "xcrun -sdk macosx metal --version").quiet().ignore_stderr().read().is_ok()
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

#[cfg(test)]
mod root_tests {
    use super::root_from;

    /// A run told its checkout works on it; one told nothing works on the one above its
    /// manifest, as an xtask built in place does.
    #[test]
    fn the_root_is_the_named_checkout_or_the_one_above_xtask() {
        let snapshot = "/w/slopty/target/gate/tree/xtask";
        let named = root_from(Some("/w/slopty".to_owned()), snapshot).expect("named");
        assert_eq!(named, "/w/slopty");
        let above = root_from(None, "/w/slopty/xtask").expect("above");
        assert_eq!(above, "/w/slopty");
        let blank = root_from(Some(String::new()), snapshot).expect("blank is unnamed");
        assert_eq!(blank, "/w/slopty/target/gate/tree");
    }
}
