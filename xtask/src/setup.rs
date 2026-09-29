//! `xtask setup`: developer tools and vendored submodules.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::gate::LaneId;
use crate::tools::{has, step};

/// Tools installed with `cargo binstall`. Versions are floors; binstall fetches prebuilt binaries.
const TOOLS: &[(&str, &str)] = &[
    ("cargo-nextest", "0.9.146"),
    ("cargo-deny", "0.20.2"),
    ("cargo-shear", "1.14.0"),
    ("cargo-hack", "0.6.45"),
    ("cargo-hakari", "0.9.39"),
    ("cargo-llvm-cov", "0.9.1"),
    ("cargo-mutants", "27.1.0"),
    ("cargo-insta", "1.48.0"),
    ("cargo-semver-checks", "0.50.0"),
    ("typos-cli", "1.50.3"),
    ("taplo-cli", "0.10.0"),
    ("bacon", "3.25.0"),
    ("samply", "0.13.1"),
    ("git-cliff", "2.14.2"),
    ("committed", "1.1.11"),
    // `cargo xtask linux`: cross-links the Linux worker with zig.
    ("cargo-zigbuild", "0.23.4"),
    // `cargo xtask fuzz` and `deep fuzz`: libFuzzer builds of `fuzz/` (on nightly).
    ("cargo-fuzz", "0.13.2"),
];

/// Install the tools and sync the submodules. Given `lanes`, only what those gate lanes run:
/// a CI job runs one lane and needs neither the other lanes' tools nor `xcodegen`.
pub fn run(sh: &Shell, no_tools: bool, lanes: &[LaneId]) -> Result<()> {
    let everything = lanes.is_empty();
    let wanted = |tool: &str| everything || lanes.iter().any(|lane| lane.tools().contains(&tool));
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
        if !has(sh, "zig") {
            println!("  note: `brew install zig` (0.16) is needed to build libghostty-vt");
        }
    }
    if !no_tools && everything && !has(sh, "xcodegen") {
        // XcodeGen is a Swift tool with no cargo distribution; Homebrew is the supported route.
        step("brew install xcodegen", &cmd!(sh, "brew install xcodegen"))?;
    }
    step("git submodules", &cmd!(sh, "git submodule update --init --recursive --depth 1"))?;
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
