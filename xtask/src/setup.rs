//! `xtask setup`: developer tools and vendored submodules.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::gate::LaneId;
use crate::tools::{has, has_metal, step};

/// Tools installed with `cargo binstall`. Versions are floors; binstall fetches prebuilt binaries.
const TOOLS: &[(&str, &str)] = &[
    ("cargo-nextest", "0.9.148"),
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
    // triple's C with it.
    ("cargo-zigbuild", "0.23.4"),
    // `cargo xtask fuzz` and `deep fuzz`: libFuzzer builds of `fuzz/` (on nightly).
    ("cargo-fuzz", "0.13.2"),
];

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
    if lanes.is_empty() {
        // What only the user can fix; a probe that cannot run is not a failed setup.
        if let Err(error) = crate::doctor::run(sh) {
            println!("  doctor skipped: {error:#}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::ValueEnum as _;

    use super::{LaneId, TOOLS};

    /// `setup --lane` installs a lane's tools from the pinned list, so each must be on it.
    #[test]
    fn every_tool_a_lane_runs_is_pinned() {
        for lane in LaneId::value_variants() {
            for tool in lane.tools() {
                assert!(TOOLS.iter().any(|(pinned, _)| pinned == tool), "{lane:?} runs {tool}");
            }
        }
    }
}
