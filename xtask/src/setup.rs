//! `xtask setup`: developer tools and vendored submodules.

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::tools::{has, step};

/// Tools installed with `cargo binstall`. Versions are floors; binstall fetches prebuilt binaries.
const TOOLS: &[(&str, &str)] = &[
    ("cargo-nextest", "0.9.144"),
    ("cargo-deny", "0.20.2"),
    ("cargo-shear", "1.13.4"),
    ("cargo-hack", "0.6.45"),
    ("cargo-llvm-cov", "0.9.1"),
    ("cargo-mutants", "27.1.0"),
    ("cargo-insta", "1.48.0"),
    ("cargo-semver-checks", "0.50.0"),
    ("typos-cli", "1.50.1"),
    ("taplo-cli", "0.10.0"),
    ("bacon", "3.25.0"),
    ("samply", "0.13.1"),
    ("prek", "0.5.2"),
    ("git-cliff", "2.14.1"),
    ("committed", "1.1.11"),
];

pub fn run(sh: &Shell, no_tools: bool) -> Result<()> {
    if !no_tools {
        if !has(sh, "cargo-binstall") {
            step("install cargo-binstall", &cmd!(sh, "cargo install cargo-binstall --locked"))?;
        }
        for (tool, version) in TOOLS {
            let spec = format!("{tool}@{version}");
            step(
                &format!("binstall {spec}"),
                &cmd!(sh, "cargo binstall --no-confirm --locked {spec}"),
            )?;
        }
        // The canonical formatter is nightly rustfmt (unstable options in rustfmt.toml: import
        // granularity, comment wrapping). Stable rustfmt ignores them and would fight the gate.
        if cmd!(sh, "rustup run nightly rustfmt --version").quiet().ignore_stderr().read().is_err()
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
    if !no_tools && !has(sh, "xcodegen") {
        // XcodeGen is a Swift tool with no cargo distribution; Homebrew is the supported route.
        step("brew install xcodegen", &cmd!(sh, "brew install xcodegen"))?;
    }
    step("git submodules", &cmd!(sh, "git submodule update --init --recursive --depth 1"))?;
    if has(sh, "prek") {
        step(
            "install git hooks",
            &cmd!(sh, "prek install --hook-type pre-commit --hook-type commit-msg"),
        )?;
    }
    Ok(())
}
