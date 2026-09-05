//! `cargo xtask` — every script in this repository is a subcommand here.
//!
//! The rule is simple: if you would reach for a shell script, add a subcommand instead. Commands
//! are thin orchestration over `cargo`, `xcrun`, `xcodebuild` and friends via `xshell`.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "xtask is a CLI; stdout is its UI")]

mod bundle;
mod e2e;
mod gate;
mod icon;
mod ime;
mod ios;
mod release;
mod run;
mod setup;
mod tools;

use anyhow::Result;
use clap::{Parser, Subcommand};
use xshell::{Shell, cmd};

/// Slopty automation.
#[derive(Parser)]
#[command(name = "xtask", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install developer tools (via cargo-binstall) and initialise vendored submodules.
    Setup {
        /// Skip installing tools; only sync submodules.
        #[arg(long)]
        no_tools: bool,
    },
    /// List enabled macOS input sources, or select one by id (for input-method testing).
    Ime {
        /// Input source id to select (e.g. `com.apple.inputmethod.VietnameseSimpleTelex`).
        id: Option<String>,
        /// List every installed source, not only the enabled ones.
        #[arg(long)]
        all: bool,
    },
    /// Run the full pre-commit gate: fmt, clippy on every triple, tests, docs, deny, shear, typos.
    Gate {
        /// Fix what can be fixed (fmt, clippy --fix, typos -w, taplo fmt) before checking.
        #[arg(long)]
        fix: bool,
        /// Only run the fast checks (fmt + clippy + unit tests on the host triple).
        #[arg(long)]
        quick: bool,
    },
    /// Run the live end-to-end tests (daemons, screen capture, input) in an isolated data dir.
    E2e(e2e::E2eOpts),
    /// Format everything (rustfmt nightly if present, taplo).
    Fmt,
    /// Clippy on all targets and all triples with `-D warnings`.
    Lint,
    /// Run the test suite with nextest.
    Test {
        /// Extra arguments passed to `cargo nextest run`.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
    /// Build documentation for the workspace with warnings denied.
    Doc {
        /// Open in the browser afterwards.
        #[arg(long)]
        open: bool,
    },
    /// Cut a release: bump the workspace version from Conventional Commits, regenerate
    /// CHANGELOG.md, commit and tag.
    Release {
        /// Explicit version instead of the one computed from commits.
        #[arg(long)]
        version: Option<String>,
        /// Show the computed version and changelog without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Skip the gate (only if it already passed on this exact tree).
        #[arg(long)]
        skip_gate: bool,
    },
    /// Print the changelog for unreleased commits.
    Changelog,
    /// Launch the host daemons or the app from the dev tree.
    Run {
        #[command(subcommand)]
        cmd: run::RunCmd,
    },
    /// macOS: build `Slopty.app` (app + host daemons + CLI inside) and sign it.
    Bundle(bundle::BundleOpts),
    /// Render `assets/icon.svg` into PNG files and an `.icns` under a directory, for a look.
    Icon {
        /// Output directory.
        #[arg(default_value = "target/icon")]
        out: camino::Utf8PathBuf,
    },
    /// iOS: build the static library, package the xcframework, generate and build the Xcode
    /// project.
    Ios {
        #[command(subcommand)]
        cmd: ios::IosCmd,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let sh = Shell::new()?;
    sh.change_dir(tools::repo_root()?);
    match cli.cmd {
        Cmd::Setup { no_tools } => setup::run(&sh, no_tools),
        Cmd::Ime { id, all } => ime::run(id.as_deref(), all),
        Cmd::Gate { fix, quick } => gate::run(&sh, gate::Options { fix, quick }),
        Cmd::E2e(opts) => e2e::run(&sh, &opts),
        Cmd::Fmt => gate::fmt(&sh, true),
        Cmd::Lint => gate::lint(&sh),
        Cmd::Test { args } => gate::test(&sh, &args),
        Cmd::Doc { open } => gate::doc(&sh, open),
        Cmd::Release { version, dry_run, skip_gate } => {
            release::run(&sh, &release::Options { version, dry_run, skip_gate })
        }
        Cmd::Changelog => cmd!(sh, "git cliff --unreleased --strip all").run().map_err(Into::into),
        Cmd::Run { cmd } => run::run(&sh, &cmd),
        Cmd::Bundle(opts) => bundle::run(&sh, &opts).map(|_app| ()),
        Cmd::Icon { out } => icon::run(&sh, &out),
        Cmd::Ios { cmd } => ios::run(&sh, &cmd),
    }
}
