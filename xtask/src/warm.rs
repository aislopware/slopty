//! `xtask warm`: the gate's warm runners on this Mac (`docs/decisions/tooling.md`, "The gate runs
//! on warm runners here").
//!
//! Two self-hosted runners keep their checkout, toolchains, tools and target dir between runs and
//! build incrementally, so a push is an edit to a machine that already built its parent:
//! - [`MAC_LABEL`], a launchd daemon under a macOS user of its own ([`mac::USER`]), sandboxed off
//!   the person's files and services, at `Nice 10` with low-priority I/O, which runs the tools,
//!   clippy for the Apple triples, rustdoc and every Mac test (`xtask gate --warm`);
//! - [`LINUX_LABEL`], the actions runner in an arm64 Ubuntu VM under tart, which runs clippy for
//!   the Linux triples and the Linux tests.
//!
//! Both refuse every job but the gate's ([`guard`]). Everything they keep is under one directory
//! at the root of the build volume, beside the checkout and never in it ([`home`]).

mod guard;
mod linux;
mod mac;

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

/// The label, the name and the `runs-on` of the warm Mac runner.
pub const MAC_LABEL: &str = "slopty-warm-mac";

/// The label, the name and the `runs-on` of the warm Linux runner, and its VM's name.
pub const LINUX_LABEL: &str = "slopty-warm-linux";

/// The actions runner a warm runner is installed from; it updates itself after, as GitHub asks.
const RUNNER_VERSION: &str = "2.338.2";

/// Each build of [`RUNNER_VERSION`] a warm runner takes, with the SHA-256 its release names.
const RUNNER_BUILDS: [(&str, &str); 2] = [
    ("osx-arm64", "25a065af409ffa317ab4d5a3b614dd15fc9c40f2172bc1b35a1b1258e919a388"),
    ("linux-arm64", "670adb2f268fe21dcbb58b3db0e0aa9d5b3bddfe2c651481336e561ff86de945"),
];

/// The file the job-started hook reads (`guard::Config`), beside the guard's binary.
const GUARD_CONFIG: &str = "warm.toml";

/// The job-started hook's script, which the runner insists be a `.sh`: one line that runs the
/// guard.
const GUARD_HOOK: &str = "started.sh";

/// The warm runners.
#[derive(Debug, clap::Subcommand)]
pub enum WarmCmd {
    /// The job-started hook (`ACTIONS_RUNNER_HOOK_JOB_STARTED`): fail any job but a push to
    /// `gate` or a `v*` tag, or a `check.yml` dispatch, by one of the repository's own actors.
    Guard {
        /// The repository and its actors (`warm.toml` beside this binary by default).
        #[arg(long)]
        config: Option<Utf8PathBuf>,
    },
    /// The Mac runner, under its own user (`slopty-ci`).
    Mac {
        #[command(subcommand)]
        cmd: MacCmd,
    },
    /// The Linux runner, in a tart VM.
    Linux {
        #[command(subcommand)]
        cmd: LinuxCmd,
    },
    /// Both runners as GitHub lists them, and this Mac's side of each.
    Status,
}

/// `xtask warm mac`.
#[derive(Debug, clap::Subcommand)]
pub enum MacCmd {
    /// Everything that needs no administrator: the runner, rustup, the guard, the sandbox and
    /// launchd's job, under the runner's home; then what an administrator runs to create the
    /// user and start the daemon, printed.
    Install,
    /// Register the runner with GitHub, by a registration token from `gh`, held in memory only.
    Register,
}

/// `xtask warm linux`.
#[derive(Debug, clap::Subcommand)]
pub enum LinuxCmd {
    /// Create the VM unless it exists, start it, provision it, build its guard from `rev` and
    /// register its runner; each step is skipped once done.
    Install {
        /// The commit whose xtask is the guard (one with this subcommand).
        #[arg(long, default_value = "HEAD")]
        rev: String,
    },
    /// Start the VM in the background, at `Nice 10`, unless it runs.
    Start,
    /// Run the VM in the foreground, at `Nice 10`, until it stops: the VM's daemon.
    Run,
    /// Stop the VM.
    Stop,
    /// Run a command in the VM, as its `admin`.
    Exec {
        /// The command and its arguments.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
}

/// `xtask warm`.
pub fn run(sh: &Shell, cmd: &WarmCmd) -> Result<()> {
    match cmd {
        WarmCmd::Guard { config } => {
            let config = match config {
                Some(config) => config.clone(),
                None => beside_exe(GUARD_CONFIG)?,
            };
            guard::run(&config)
        }
        WarmCmd::Mac { cmd: MacCmd::Install } => mac::install(sh, &home(sh)?),
        WarmCmd::Mac { cmd: MacCmd::Register } => mac::register(sh, &home(sh)?),
        WarmCmd::Linux { cmd: LinuxCmd::Install { rev } } => linux::install(sh, &home(sh)?, rev),
        WarmCmd::Linux { cmd: LinuxCmd::Start } => linux::start(sh, &home(sh)?),
        WarmCmd::Linux { cmd: LinuxCmd::Run } => linux::run(&home(sh)?),
        WarmCmd::Linux { cmd: LinuxCmd::Stop } => linux::stop(sh, &home(sh)?),
        WarmCmd::Linux { cmd: LinuxCmd::Exec { command } } => linux::exec(sh, &home(sh)?, command),
        WarmCmd::Status => status(sh),
    }
}

/// `name` beside this binary.
fn beside_exe(name: &str) -> Result<Utf8PathBuf> {
    let exe = std::env::current_exe().context("this binary's path")?;
    let exe = Utf8PathBuf::from_path_buf(exe)
        .map_err(|p| anyhow::anyhow!("this binary's path is not UTF-8: {}", p.display()))?;
    Ok(exe.parent().context("this binary has no directory")?.join(name))
}

/// The warm runners' directory: `slopty-ci` at the root of the volume this checkout is on, off
/// the checkout, so nothing the person's tree holds is the runners' and nothing of theirs lands
/// in it; `SLOPTY_WARM_HOME` names another.
pub fn home(sh: &Shell) -> Result<Utf8PathBuf> {
    if let Some(named) = std::env::var("SLOPTY_WARM_HOME").ok().filter(|h| !h.is_empty()) {
        return Ok(Utf8PathBuf::from(named));
    }
    let root = Utf8PathBuf::from_path_buf(sh.current_dir())
        .map_err(|p| anyhow::anyhow!("the checkout's path is not UTF-8: {}", p.display()))?;
    Ok(mount_point(&root)?.join("slopty-ci"))
}

/// The root of the volume `path` is on: its last ancestor on the same device.
fn mount_point(path: &Utf8Path) -> Result<Utf8PathBuf> {
    use std::os::unix::fs::MetadataExt as _;

    let device = std::fs::metadata(path).with_context(|| format!("stat {path}"))?.dev();
    let mut point = path;
    while let Some(parent) = point.parent() {
        if std::fs::metadata(parent).map_or(true, |m| m.dev() != device) {
            break;
        }
        point = parent;
    }
    Ok(point.to_owned())
}

/// The repository's `owner/name`, and its collaborators with push access: those whose pushes and
/// dispatches the guard lets run.
fn repository(sh: &Shell) -> Result<guard::Config> {
    let repository =
        cmd!(sh, "gh repo view --json nameWithOwner --jq .nameWithOwner").quiet().read()?;
    let path = format!("repos/{repository}/collaborators?permission=push&per_page=100");
    let actors = cmd!(sh, "gh api --paginate {path} --jq .[].login").quiet().read()?;
    let mut actors: Vec<String> = actors.lines().map(str::to_owned).collect();
    actors.sort();
    anyhow::ensure!(!actors.is_empty(), "{repository} lists no collaborator with push access");
    Ok(guard::Config { repository, actors })
}

/// A registration token for `repository`'s runners, from `gh` as the person is signed in to
/// it. It lives in this process alone: never an argument, a file or a line of output.
fn registration_token(sh: &Shell, repository: &str) -> Result<String> {
    let path = format!("repos/{repository}/actions/runners/registration-token");
    let token = cmd!(sh, "gh api -X POST {path} --jq .token").quiet().read()?;
    anyhow::ensure!(!token.trim().is_empty(), "GitHub gave no registration token");
    Ok(token.trim().to_owned())
}

/// The actions runner's `build` in `downloads`, fetched unless there and checked against its
/// pinned SHA-256.
fn runner_tarball(sh: &Shell, downloads: &Utf8Path, build: &str) -> Result<Utf8PathBuf> {
    let (_, sha) = RUNNER_BUILDS
        .iter()
        .find(|(b, _)| *b == build)
        .with_context(|| format!("no actions runner build {build} is pinned"))?;
    std::fs::create_dir_all(downloads).with_context(|| format!("create {downloads}"))?;
    let name = format!("actions-runner-{build}-{RUNNER_VERSION}.tar.gz");
    let tarball = downloads.join(&name);
    if !tarball.is_file() || crate::setup::sha256(sh, tarball.as_std_path())? != *sha {
        let url =
            format!("https://github.com/actions/runner/releases/download/v{RUNNER_VERSION}/{name}");
        cmd!(sh, "curl -fsSL --retry 5 -o {tarball} {url}").quiet().run()?;
    }
    let digest = crate::setup::sha256(sh, tarball.as_std_path())?;
    anyhow::ensure!(digest == *sha, "{name} has SHA-256 {digest}, not the pinned {sha}");
    Ok(tarball)
}

/// rustup's installer for `triple` in `downloads`, checked against the SHA-256 rustup publishes
/// beside it.
fn rustup_init(sh: &Shell, downloads: &Utf8Path, triple: &str) -> Result<Utf8PathBuf> {
    std::fs::create_dir_all(downloads).with_context(|| format!("create {downloads}"))?;
    let init = downloads.join(format!("rustup-init-{triple}"));
    let url = format!("https://static.rust-lang.org/rustup/dist/{triple}/rustup-init");
    let sha_url = format!("{url}.sha256");
    let published = cmd!(sh, "curl -fsSL --retry 5 {sha_url}").quiet().read()?;
    let sha = published.split_whitespace().next().unwrap_or_default().to_owned();
    if !init.is_file() || crate::setup::sha256(sh, init.as_std_path())? != sha {
        cmd!(sh, "curl -fsSL --retry 5 -o {init} {url}").quiet().run()?;
    }
    let digest = crate::setup::sha256(sh, init.as_std_path())?;
    anyhow::ensure!(digest == sha, "rustup-init for {triple} has SHA-256 {digest}, not {sha}");
    std::fs::set_permissions(&init, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    Ok(init)
}

/// The job-started hook's script: the guard, run on the job's environment. A refusal exits
/// non-zero, which fails the job before its first step.
fn hook_script(guard: &Utf8Path, config: &Utf8Path) -> String {
    format!("#!/bin/sh\nexec '{guard}' warm guard --config '{config}'\n")
}

/// A runner's `.env`: `KEY=value` lines the runner gives every job's environment.
fn dotenv(vars: &[(&str, String)]) -> String {
    vars.iter().fold(String::new(), |mut out, (key, value)| {
        let _written = writeln!(out, "{key}={value}");
        out
    })
}

/// Both runners as GitHub lists them, the Mac daemon's state and the VM's, and the free space
/// where they build.
fn status(sh: &Shell) -> Result<()> {
    let home = home(sh)?;
    let config = repository(sh)?;
    let path = format!("repos/{}/actions/runners", config.repository);
    let listed = cmd!(sh, "gh api {path} --jq '.runners[] | [.name, .status, .busy] | @tsv'")
        .quiet()
        .read()?;
    println!("▶ GitHub's runners of {}", config.repository);
    for label in [MAC_LABEL, LINUX_LABEL] {
        let line = listed.lines().find(|l| l.split('\t').next() == Some(label));
        match line {
            Some(line) => {
                let fields: Vec<&str> = line.split('\t').collect();
                let state = fields.get(1).copied().unwrap_or_default();
                let busy = fields.get(2).is_some_and(|b| *b == "true");
                println!("  {label}: {state}{}", if busy { ", running a job" } else { "" });
            }
            None => println!("  {label}: not registered"),
        }
    }
    mac::status(sh, &home);
    linux::status(sh, &home);
    if let Ok(free) = crate::prune::free_space(&home) {
        println!("  free on {home}'s volume: {} GB", free / 1_000_000_000);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::{RUNNER_BUILDS, dotenv, hook_script, mount_point};

    /// Every runner build is pinned by a SHA-256 of 64 hex digits, for both hosts.
    #[test]
    fn the_runner_s_builds_are_pinned() {
        for build in ["osx-arm64", "linux-arm64"] {
            let (_, sha) = RUNNER_BUILDS.iter().find(|(b, _)| *b == build).expect(build);
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{sha}");
        }
    }

    /// The hook runs the guard on its config and nothing else, quoted; the `.env` is one line a
    /// variable.
    #[test]
    fn the_hook_and_the_environment_are_written_whole() {
        let hook = hook_script(Utf8Path::new("/r/guard/xtask"), Utf8Path::new("/r/guard/w.toml"));
        assert_eq!(
            hook,
            "#!/bin/sh\nexec '/r/guard/xtask' warm guard --config '/r/guard/w.toml'\n"
        );
        let env = dotenv(&[("A", "1".to_owned()), ("TMPDIR", "/r/tmp/".to_owned())]);
        assert_eq!(env, "A=1\nTMPDIR=/r/tmp/\n");
    }

    /// A path's volume is its last ancestor on the same device: `/` for this test's own tree on
    /// the boot volume, or the volume's mount point.
    #[test]
    fn a_path_s_volume_is_its_mount_point() {
        let here = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
        let point = mount_point(here).expect("a mount point");
        assert!(here.starts_with(&point), "{point} holds {here}");
        let device = |p: &Utf8Path| {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(p).expect("stat").dev()
        };
        assert!(
            point.parent().is_none_or(|p| device(p) != device(&point)),
            "{point} is the volume's root"
        );
    }
}
