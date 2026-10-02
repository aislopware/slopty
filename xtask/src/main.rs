//! `cargo xtask` — every script in this repository is a subcommand here.
//!
//! The rule is simple: if you would reach for a shell script, add a subcommand instead. Commands
//! are thin orchestration over `cargo`, `xcrun`, `xcodebuild` and friends via `xshell`.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "xtask is a CLI; stdout is its UI")]

mod bench;
mod bundle;
mod check;
mod claude;
mod claude_mod;
mod codex;
mod deep;
mod dist;
mod doctor;
mod e2e;
mod fixtures;
mod fuzz;
mod gate;
mod icon;
// HIToolbox's input sources: macOS only. Elsewhere `xtask ime` says so, so the deep checks
// that run on Linux runners build the rest.
#[cfg(target_os = "macos")]
mod ime;
mod ios;
mod land;
mod linux;
mod nightly;
mod pi;
mod prune;
mod ptys;
mod release;
mod run;
mod runner;
mod scrub;
mod setup;
mod sign;
mod soak;
mod symbolicate;
mod tailnet;
mod tools;
mod upstream;
mod vm;

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
        /// Install only the tools this gate lane runs; repeat for several (a CI job per lane).
        #[arg(long = "lane", value_enum)]
        lanes: Vec<gate::LaneId>,
    },
    /// What on this Mac slows every build and only you can fix: whether `XProtect` scans each new
    /// binary (the Developer Tools switch) and the free space the gate needs.
    Doctor,
    /// List enabled macOS input sources, or select one by id (for input-method testing).
    Ime {
        /// Input source id to select (e.g. `com.apple.inputmethod.VietnameseSimpleTelex`).
        id: Option<String>,
        /// List every installed source, not only the enabled ones.
        #[arg(long)]
        all: bool,
    },
    /// The gate's checks on the named crates only, on the working tree: what an agent that owns
    /// those crates runs before it reports.
    Check {
        /// A crate to check; repeat for several.
        #[arg(short = 'p', long = "package", required = true)]
        packages: Vec<String>,
    },
    /// The pre-commit gate on the staged tree: fmt, taplo, deny, hakari, shear, typos, committed
    /// and host clippy, then the next step. The other lanes run in CI once `land` pushes.
    Gate {
        /// Fix what can be fixed (fmt, hakari, shear, typos -w, taplo fmt) before checking.
        #[arg(long)]
        fix: bool,
        /// Every lane here, as CI runs them: the tests, clippy for iOS and Linux, and rustdoc too.
        #[arg(long, conflicts_with = "lanes")]
        full: bool,
        /// The message of the commit about to be made, for `committed` to check; the gate leaves
        /// it for `git commit -F`.
        #[arg(short, long)]
        message: Option<String>,
        /// Check the working tree in place instead of the index snapshot under
        /// `target/gate/tree` (CI, where the tree is the commit).
        #[arg(long)]
        in_place: bool,
        /// Run only the tests of the packages whose files changed since the tests lane last
        /// passed, and of every package that depends on them (all of them when a file outside
        /// the packages changed).
        #[arg(long)]
        since_pass: bool,
        /// On a hosted runner: in place, and the tests lane skips the tests that read hardware a
        /// runner's virtual Mac lacks (nextest's `ci` profile).
        #[arg(long)]
        ci: bool,
        /// Run only this lane; repeat for several. The tools lane carries the fmt check.
        #[arg(long = "lane", value_enum)]
        lanes: Vec<gate::LaneId>,
    },
    /// After a commit on main: push it to the `gate` branch, where CI runs every gate lane and
    /// fast-forwards main to it once all pass.
    Land(land::LandOpts),
    /// nextest's setup script: build every binary a test spawns before the first test starts.
    SpawnedBins,
    /// Delete the build units and incremental caches nothing has used for a while, in every
    /// target dir under `target/`, then the least recently used until `target/` is within its
    /// budget and its volume above the free-space floor (also run, skipping busy dirs, after
    /// `check` and `gate`).
    Prune {
        /// Hours a unit may go unused before it goes.
        #[arg(long, default_value_t = prune::DEFAULT_IDLE_HOURS)]
        idle_hours: u64,
        /// GB `target/` may hold (default `SLOPTY_TARGET_BUDGET_GB`, else 160).
        #[arg(long)]
        budget_gb: Option<u64>,
        /// GB its volume keeps free (default `SLOPTY_DISK_FLOOR_GB`, else 50).
        #[arg(long)]
        floor_gb: Option<u64>,
        /// Show what would go without deleting it.
        #[arg(long)]
        dry_run: bool,
        /// Skip a dir a build holds instead of waiting for it: its idle caches still go, under
        /// rustc's own session locks, and only its units stay.
        #[arg(long)]
        no_wait: bool,
    },
    /// The slow checks that run on a schedule, not per commit: Miri, sanitizers, the feature
    /// powerset, coverage, mutation testing, a fuzz smoke.
    Deep {
        #[command(subcommand)]
        cmd: deep::DeepCmd,
    },
    /// libFuzzer over every decoder that faces a peer (`fuzz/`): each target for `--time`
    /// seconds from the wire goldens and its kept corpus; `--keep` files a crash as a regression
    /// input, `--replay` replays those on a plain build.
    Fuzz(fuzz::FuzzOpts),
    /// Run the `*_cost` measurements in release and hold their retired instructions to
    /// `xtask/budgets.toml` (`--update-budgets` records a run; `--wall` keeps the wall-time
    /// trend, as the nightly run does).
    Bench(bench::BenchOpts),
    /// The daemons under a synthetic load from a temporary HOME, watched for footprint growth,
    /// descriptors and threads left behind, and leaks (`leaks`) at the end.
    Soak(soak::SoakOpts),
    /// The heavy lanes one after another (soak, bench wall time, amplified property and GPUI
    /// tests, the deep checks), a JSON summary each under `target/nightly/<date>/`; `install`
    /// runs it at 03:00 from a `LaunchAgent`.
    Nightly {
        #[command(subcommand)]
        cmd: Option<nightly::NightlyCmd>,
    },
    /// Record a CPU profile of a command with samply (pure Rust, Firefox Profiler UI):
    /// `cargo xtask profile -- cargo nextest run -p slopty-ui -E 'test(smooth)'`.
    Profile {
        /// The command to profile.
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Capture the Claude Code transcripts and hook payloads the conversation decoder is pinned
    /// by, into `crates/slopty-agent/tests/fixtures/conversation`.
    Fixtures {
        #[command(subcommand)]
        cmd: fixtures::FixturesCmd,
    },
    /// The pinned Codex build: the app-server protocol's types generated from it.
    Codex {
        #[command(subcommand)]
        cmd: codex::CodexCmd,
    },
    /// The pinned pi build, and the RPC fixtures recorded from it.
    Pi {
        #[command(subcommand)]
        cmd: pi::PiCmd,
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
    /// Launch the worker daemons or the app from the dev tree.
    Run {
        #[command(subcommand)]
        cmd: run::RunCmd,
    },
    /// macOS: build `Slopty.app` (app, worker daemons, server and CLI, and the Linux workers it
    /// installs) and sign it with a stable identity when there is one.
    Bundle(bundle::BundleOpts),
    /// Build everything a release publishes under `target/dist-out`: the app, the Mac CLI and
    /// daemons, the Linux workers and servers, the dSYMs and their checksums; signed, notarised
    /// when credentials are at hand, and checked. Publishing is CI's, on a tag.
    Dist(dist::DistOpts),
    /// macOS: codesign the dev daemons so their TCC grants survive the next `cargo build`.
    Sign(sign::SignOpts),
    /// Resolve a crash report from a shipped build: finds the dSYM with the report's build UUID
    /// (`target/dist`, `target/release`, `target/bundle`, or `--dsyms`) and gives every frame
    /// its file, line and inlined frames.
    Symbolicate(symbolicate::SymbolicateOpts),
    /// File each binary's dSYM under its UUID, as a release publishes them.
    Dsyms(symbolicate::DsymsOpts),
    /// Build `assets/icon.svg` into the Icon Composer document, compile it, and render the
    /// system's pictures of it (every size, every appearance) under a directory, for a look.
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
    /// A terminal-only Linux worker: cross-build it (`build`, the default), run it in a Docker
    /// Desktop container (`run`), run the Linux end-to-end test against it from this Mac
    /// (`e2e`), deploy it and the server into a systemd container (`deploy`), or cross-build
    /// what ships (`dist`).
    Linux(linux::LinuxOpts),
    /// Cargo's target runner for test binaries (the gate and `check` set it): runs a `deps/`
    /// binary through a hard link in `run/`, out of a directory of 100 000 entries.
    #[command(hide = true)]
    TestRunner {
        /// The binary cargo or nextest runs.
        binary: camino::Utf8PathBuf,
        /// Its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Keep the gpui-fast, gpui-kit, ghostty and libghostty-rs forks current with their upstreams
    /// and zed (`check` the drift, `sync` them, `watch` the upstreams).
    Upstream {
        #[command(subcommand)]
        cmd: upstream::UpstreamCmd,
    },
    /// macOS guests for the live tests that move the pointer, post HID events or need TCC grants:
    /// `create`, `start`/`stop`, `ssh`, `exec`, `deploy`, `live -p <crate> -- <nextest args>`,
    /// `e2e`, `list`, `prune`.
    Vm(vm::VmOpts),
    /// Run a command and count who holds the pseudo-terminals while it runs, every 200 ms: the
    /// peak and the tests and processes that made it up, and any held after its test ended.
    Ptys {
        /// The workspace whose tests count when they left the command's tree.
        #[arg(long)]
        workspace: Option<camino::Utf8PathBuf>,
        /// Where each sample is written (`ms ours visible lowest-free`).
        #[arg(long, default_value = "target/logs/ptys.log")]
        log: camino::Utf8PathBuf,
        /// The command, after `--`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// A real tailnet on loopback for the live tests: Headscale and two userspace `tailscaled`
    /// nodes with a Slopty grant between them (`up` holds it until Ctrl-C or `down`; `status`
    /// prints what a test reads).
    Tailnet {
        #[command(subcommand)]
        cmd: tailnet::TailnetCmd,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let sh = Shell::new()?;
    sh.change_dir(tools::repo_root()?);
    match cli.cmd {
        Cmd::Setup { no_tools, lanes } => setup::run(&sh, no_tools, &lanes),
        Cmd::Doctor => doctor::run(&sh),
        #[cfg(target_os = "macos")]
        Cmd::Ime { id, all } => ime::run(id.as_deref(), all),
        #[cfg(not(target_os = "macos"))]
        Cmd::Ime { .. } => anyhow::bail!("input sources are macOS's"),
        Cmd::Check { packages } => {
            let checked = check::run(&sh, &packages);
            prune::auto();
            checked
        }
        Cmd::Gate { fix, full, message, in_place, since_pass, ci, lanes } => {
            let lanes = if lanes.is_empty() && !full && !ci { gate::QUICK.to_vec() } else { lanes };
            let in_place = in_place || ci;
            let next = gate::next_step(message.is_some());
            let opts = gate::Options { fix, in_place, since_pass, message };
            let gated = gate::run_only(&sh, &opts, &gate::Only { lanes, ci });
            prune::auto();
            if gated.is_ok() && !in_place {
                print!("{next}");
            }
            gated
        }
        Cmd::Land(opts) => land::run(&sh, &opts),
        Cmd::SpawnedBins => gate::spawned_bins(&sh),
        Cmd::Prune { idle_hours, budget_gb, floor_gb, dry_run, no_wait } => {
            let mut limits = prune::Limits::from_env()?;
            if let Some(gb) = budget_gb {
                limits.budget = gb.saturating_mul(1_000_000_000);
            }
            if let Some(gb) = floor_gb {
                limits.floor = gb.saturating_mul(1_000_000_000);
            }
            prune::run(prune::Options {
                idle: std::time::Duration::from_secs(idle_hours.saturating_mul(3600)),
                limits,
                wait: !no_wait,
                dry_run,
            })
        }
        Cmd::Deep { cmd } => deep::run(&sh, &cmd),
        Cmd::Fuzz(opts) => fuzz::run(&sh, &opts),
        Cmd::Bench(opts) => bench::run(&sh, &opts),
        Cmd::Soak(opts) => soak::run(&sh, &opts),
        Cmd::Nightly { cmd } => nightly::run(&sh, cmd.as_ref()),
        Cmd::Profile { cmd } => {
            sh.create_dir("target/profile")?;
            let out = format!(
                "target/profile/{}.json.gz",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
            );
            cmd!(sh, "samply record --save-only -o {out} {cmd...}").run()?;
            println!("profile saved to {out}; view with `samply load {out}`");
            Ok(())
        }
        Cmd::E2e(opts) => e2e::run(&sh, &opts),
        Cmd::Fixtures { cmd } => fixtures::run(&cmd),
        Cmd::Codex { cmd } => codex::run(&cmd),
        Cmd::Pi { cmd } => pi::run(&cmd),
        Cmd::Fmt => gate::fmt(&sh, true),
        Cmd::Lint => gate::lint(&sh),
        Cmd::Test { args } => gate::test(&sh, &args),
        Cmd::Doc { open } => gate::doc(&sh, open),
        Cmd::Release { version, dry_run, skip_gate } => {
            release::run(&sh, &release::Options { version, dry_run, skip_gate })
        }
        Cmd::Changelog => cmd!(sh, "git cliff --unreleased --strip all").run().map_err(Into::into),
        Cmd::Run { cmd } => run::run(&sh, &cmd),
        Cmd::Bundle(opts) => bundle::run(&sh, &opts).map(|_bundle| ()),
        Cmd::Dist(opts) => dist::run(&sh, &opts),
        Cmd::Sign(opts) => sign::run(&sh, &opts),
        Cmd::Symbolicate(opts) => symbolicate::run(&sh, &opts),
        Cmd::Dsyms(opts) => symbolicate::dsyms(&sh, &opts),
        Cmd::Icon { out } => icon::run(&sh, &out),
        Cmd::Ios { cmd } => ios::run(&sh, &cmd),
        Cmd::Linux(opts) => linux::main(&sh, &opts),
        Cmd::Upstream { cmd } => upstream::run(&sh, &cmd),
        Cmd::TestRunner { binary, args } => runner::exec(&binary, &args),
        Cmd::Vm(opts) => vm::run(&sh, &opts),
        Cmd::Tailnet { cmd } => tailnet::run(&cmd),
        Cmd::Ptys { workspace, log, command } => {
            let root = tools::repo_root()?;
            ptys::watch(
                &workspace.map_or_else(|| root.clone(), |w| root.join(w)),
                &root.join(log),
                &command,
            )
        }
    }
}
