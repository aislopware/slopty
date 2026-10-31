//! `xtask setup`: developer tools and vendored submodules.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::gate::LaneId;
use crate::tools::{has, has_metal, step};

/// Tools installed with `cargo binstall`, prebuilt. Each version is exact: binstall reads a bare
/// version as `=`, so setup puts the pinned release back over a newer one, and taking a new
/// release means moving its pin here.
const TOOLS: &[(&str, &str)] = &[
    // `.config/nextest.toml` requires this release too (`nextest-version`), so a machine left
    // on an older one fails loudly rather than skip the keys it does not know.
    ("cargo-nextest", "0.9.150"),
    ("cargo-deny", "0.20.2"),
    ("cargo-shear", "1.14.0"),
    ("cargo-hack", "0.6.45"),
    ("cargo-hakari", "0.9.39"),
    ("cargo-llvm-cov", "0.9.1"),
    ("cargo-mutants", "27.1.0"),
    ("cargo-insta", "1.49.0"),
    ("cargo-semver-checks", "0.51.0"),
    ("typos-cli", "1.51.1"),
    ("taplo-cli", "0.10.0"),
    ("bacon", "3.26.0"),
    ("samply", "0.13.1"),
    ("git-cliff", "2.14.2"),
    ("committed", "1.1.11"),
    // `cargo xtask linux`: cross-links the Linux worker with zig; Linux clippy compiles each
    // triple's C with it. 0.24 is the first to drop rustc's `-Wl,-O1`, which zig 0.12 and later
    // ignore with a linker warning on every optimised link (`linux::need_zig` holds the floor).
    ("cargo-zigbuild", "0.24.0"),
    // `cargo xtask fuzz` and `deep fuzz`: libFuzzer builds of `fuzz/` (on nightly).
    ("cargo-fuzz", "0.13.2"),
];

/// The release of `tool` (its crate) that setup installs.
pub fn pinned(tool: &str) -> Option<&'static str> {
    TOOLS.iter().find(|(name, _)| *name == tool).map(|(_, version)| *version)
}

/// Whether release `version` comes before `than`, comparing each dotted part as a number (the
/// digits it starts with: a pre-release's `0-rc` reads as 0).
pub fn older(version: &str, than: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('.')
            .map(|part| {
                let digits = part.find(|c: char| !c.is_ascii_digit()).unwrap_or(part.len());
                part.get(..digits).and_then(|d| d.parse().ok()).unwrap_or(0)
            })
            .collect()
    };
    parts(version) < parts(than)
}

/// taplo's own release builds, gzipped, each with its SHA-256, for the hosts that run the gate's
/// tools: binstall looks for other names than taplo's release uses, finds none, and compiled
/// `taplo-cli` from source, 84 s of every tools job (`docs/decisions/tooling.md`, "taplo comes
/// from its release").
const TAPLO_BUILDS: [(&str, &str, &str); 3] = [
    ("macos", "aarch64", "713734314c3e71894b9e77513c5349835eefbd52908445a0d73b0c7dc469347d"),
    ("linux", "x86_64", "8fe196b894ccf9072f98d4e1013a180306e17d244830b03986ee5e8eabeb6156"),
    ("linux", "aarch64", "033681d01eec8376c3fd38fa3703c79316f5e14bb013d859943b60a07bccdcc3"),
];

/// The taplo build for this host and its SHA-256, by its release's name for the platform.
fn taplo_build() -> Option<(String, &'static str)> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let (_, _, sha) = TAPLO_BUILDS.iter().find(|(o, a, _)| *o == os && *a == arch)?;
    let platform = if os == "macos" { "darwin" } else { os };
    Some((format!("{platform}-{arch}"), sha))
}

/// taplo `version` into cargo's bin directory from its release, checked against its pinned
/// SHA-256, unless that version is on `PATH` already.
fn install_taplo(sh: &Shell, version: &str) -> Result<()> {
    let wanted = format!("taplo {version}");
    let installed = cmd!(sh, "taplo --version").quiet().ignore_stderr().read();
    if installed.is_ok_and(|v| v.trim() == wanted) {
        return Ok(());
    }
    let Some((platform, sha)) = taplo_build() else {
        anyhow::bail!(
            "no taplo build is pinned for {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    };
    println!("▶ taplo {version} ({platform}) from its release");
    let home = std::env::var("CARGO_HOME").map_or_else(
        |_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cargo"),
        std::path::PathBuf::from,
    );
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let gz = bin.join(format!(".taplo-{version}.gz"));
    let url =
        format!("https://github.com/tamasfe/taplo/releases/download/{version}/taplo-{platform}.gz");
    cmd!(sh, "curl -fsSL --retry 5 -o {gz} {url}").quiet().run()?;
    let digest = sha256(sh, &gz)?;
    anyhow::ensure!(
        digest == sha,
        "taplo-{platform}.gz has SHA-256 {digest}, not the pinned {sha}"
    );
    let staged = bin.join(format!(".taplo-{version}"));
    let unpacked = cmd!(sh, "gunzip -c {gz}").quiet().output()?;
    std::fs::write(&staged, &unpacked.stdout)?;
    std::fs::set_permissions(&staged, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    let taplo = bin.join("taplo");
    std::fs::rename(&staged, &taplo)?;
    std::fs::remove_file(&gz)?;
    let now = cmd!(sh, "{taplo} --version").quiet().read()?;
    anyhow::ensure!(now.trim() == wanted, "the installed taplo says {now:?}");
    println!("  ✓ {wanted}");
    Ok(())
}

/// The SHA-256 of `file`, as hex, by whichever of the two system tools this host has.
pub fn sha256(sh: &Shell, file: &std::path::Path) -> Result<String> {
    let line = cmd!(sh, "shasum -a 256 {file}")
        .quiet()
        .ignore_stderr()
        .read()
        .or_else(|_| cmd!(sh, "sha256sum {file}").quiet().read())?;
    Ok(line.split_whitespace().next().unwrap_or_default().to_owned())
}

/// Install the tools and sync the submodules. Given `lanes` or `tools`, only what those gate
/// lanes run and those tools: a CI job runs one lane and needs neither the other lanes' tools
/// nor `xcodegen`.
pub fn run(sh: &Shell, no_tools: bool, lanes: &[LaneId], tools: &[String]) -> Result<()> {
    let everything = lanes.is_empty() && tools.is_empty();
    let wanted = |tool: &str| {
        everything
            || lanes.iter().any(|lane| lane.tools().contains(&tool))
            || tools.iter().any(|t| t == tool)
    };
    let unknown: Vec<&str> =
        tools.iter().map(String::as_str).filter(|t| TOOLS.iter().all(|(k, _)| k != t)).collect();
    anyhow::ensure!(unknown.is_empty(), "no such tool to install: {}", unknown.join(", "));
    if !no_tools {
        if !has(sh, "cargo-binstall") {
            step("install cargo-binstall", &cmd!(sh, "cargo install cargo-binstall --locked"))?;
        }
        for (tool, version) in TOOLS.iter().filter(|(tool, _)| wanted(tool)) {
            if *tool == "taplo-cli" {
                install_taplo(sh, version)?;
                continue;
            }
            let spec = format!("{tool}@{version}");
            step(
                &format!("binstall {spec}"),
                &cmd!(sh, "cargo binstall --no-confirm --locked {spec}"),
            )?;
        }
        // The canonical formatter is nightly rustfmt (unstable options in rustfmt.toml: import
        // granularity, comment wrapping). Stable rustfmt ignores them and would fight the gate.
        let formats = everything || lanes.contains(&LaneId::Tools);
        if formats
            && cmd!(sh, "rustup run nightly rustfmt --version")
                .quiet()
                .ignore_stderr()
                .read()
                .is_err()
        {
            step(
                "nightly rustfmt",
                &cmd!(sh, "rustup toolchain install nightly --profile minimal --component rustfmt"),
            )?;
        }
        if !has(sh, "xcodegen") {
            println!("  note: `brew install xcodegen` is needed for iOS builds");
        }
        if let Some(problem) = crate::tools::zig_problem(sh) {
            println!("  note: {problem}");
        }
    }
    if !no_tools && everything && !has(sh, "xcodegen") {
        // XcodeGen is a Swift tool with no cargo distribution; Homebrew is the supported route.
        step("brew install xcodegen", &cmd!(sh, "brew install xcodegen"))?;
    }
    // The lanes that build GPUI for the Mac, whose build script compiles its shaders: a check
    // runs it too, and fails without the compiler. A cached target dir hides that until a
    // cold one.
    let builds_shaders = everything
        || lanes
            .iter()
            .any(|lane| matches!(lane, LaneId::ClippyHost | LaneId::Tests | LaneId::Rustdoc));
    if !no_tools && builds_shaders && cfg!(target_os = "macos") && !has_metal(sh) {
        // Xcode 27 ships without the Metal compiler GPUI's shaders build with, a CI image's
        // included; Apple serves it as a component, fetched with no sign-in.
        step("the Metal toolchain", &cmd!(sh, "xcodebuild -downloadComponent MetalToolchain"))?;
    }
    step("git submodules", &cmd!(sh, "git submodule update --init --recursive --depth 1"))?;
    // ghostty's patches to the standard library of its zig, which come with the submodule:
    // zig 0.17.0 builds libghostty-vt for iOS with safety checks on only with them
    // (`tools::patch_zig`). CI's jobs run setup after `.github/actions/zig`, so their zig is
    // patched as a Mac's is. Without that zig there is nothing to patch.
    let ghostty = crate::tools::repo_root()?.join("vendor/ghostty");
    if let Ok(zig) = crate::tools::zig_for(sh, &ghostty) {
        for file in crate::tools::patch_zig(sh, &ghostty, &zig)? {
            println!("  ✓ ghostty's patch put into {file}");
        }
    }
    if everything {
        // What only the user can fix, for a whole setup, not a job's or a hint's few tools; a
        // probe that cannot run is not a failed setup.
        if let Err(error) = crate::doctor::run(sh) {
            println!("  doctor skipped: {error:#}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    use super::{LaneId, TAPLO_BUILDS, TOOLS};

    /// `setup --lane` installs a lane's tools from the pinned list, so each must be on it.
    #[test]
    fn every_tool_a_lane_runs_is_pinned() {
        for lane in LaneId::value_variants() {
            for tool in lane.tools() {
                assert!(TOOLS.iter().any(|(pinned, _)| pinned == tool), "{lane:?} runs {tool}");
            }
        }
    }

    /// taplo comes from its release for every host that runs the tools (this Mac, CI's Linux
    /// runners of both architectures and the warm runners), each build pinned by a SHA-256 of 64
    /// hex digits, and the version fetched is the one the list pins.
    #[test]
    fn taplo_s_release_build_is_pinned_for_every_host_of_the_tools() {
        for host in [("macos", "aarch64"), ("linux", "x86_64"), ("linux", "aarch64")] {
            let build = TAPLO_BUILDS.iter().find(|(os, arch, _)| (*os, *arch) == host);
            let (_, _, sha) = build.unwrap_or_else(|| panic!("no taplo build for {host:?}"));
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{sha}");
        }
        let (_, version) = TOOLS.iter().find(|(tool, _)| *tool == "taplo-cli").expect("pinned");
        assert_eq!(*version, "0.10.0", "a new taplo needs its builds' SHA-256s pinned anew");
        assert!(super::taplo_build().is_some(), "this host has a build");
    }

    /// setup's list is the one pin: a workflow installs a pinned tool through `xtask setup`,
    /// never at a version of its own that a bump here leaves behind (the release job held
    /// cargo-zigbuild at 0.23.4 beside the list, and so every Linux link's warning).
    #[test]
    fn no_workflow_pins_a_tool_of_its_own() {
        let workflows = crate::tools::repo_root().expect("repo root").join(".github/workflows");
        for entry in std::fs::read_dir(&workflows).expect("the workflows") {
            let path = entry.expect("a workflow").path();
            let text = std::fs::read_to_string(&path).expect("its text");
            for (tool, _) in TOOLS {
                assert!(!text.contains(&format!("{tool}@")), "{} pins {tool}", path.display());
            }
        }
    }

    /// nextest's own guard asks for the release setup installs, so moving one moves both: a
    /// guard behind the pin lets a stale nextest run the config, one ahead fails every check.
    #[test]
    fn nextest_requires_the_release_setup_installs() {
        let root = crate::tools::repo_root().expect("repo root");
        let text = std::fs::read_to_string(root.join(".config/nextest.toml")).expect("the config");
        let config: toml::Table = text.parse().expect("its TOML");
        let required = config.get("nextest-version").and_then(toml::Value::as_str);
        assert_eq!(required, super::pinned("cargo-nextest"), "`nextest-version` is the pin");
    }

    /// A release is older by its parts as numbers, not as text, and the pin itself or anything
    /// after it passes.
    #[test]
    fn a_release_is_older_by_its_numbered_parts() {
        use super::older;
        assert!(older("0.23.4", "0.24.0"));
        assert!(older("0.9.148", "0.10.0"), "9 before 10, though '9' sorts after '1'");
        assert!(older("0.24.0-rc.1", "0.24.1"));
        assert!(!older("0.24.0", "0.24.0"));
        assert!(!older("0.24.1", "0.24.0"));
        assert!(!older("1.0.0", "0.24.0"));
        assert_eq!(super::pinned("cargo-zigbuild").map(|v| older(v, "0.24.0")), Some(false));
        assert_eq!(super::pinned("no-such-tool"), None);
    }
}
