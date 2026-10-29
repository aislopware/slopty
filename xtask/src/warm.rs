//! `xtask warm`: the gate's warm runners on this Mac (`docs/decisions/tooling.md`, "The gate runs
//! on warm runners here").
//!
//! Two self-hosted runners keep their checkout, toolchains, tools and target dir between runs and
//! build incrementally, so a push is an edit to a machine that already built its parent:
//! - [`MAC_LABEL`], a launchd daemon under a macOS user of its own ([`mac::USER`]), sandboxed off
//!   the person's files and services, at `Nice 10` with low-priority I/O, which runs the tools,
//!   clippy for the Apple triples, rustdoc and every Mac test (`xtask gate --warm`);
//! - [`LINUX_LABEL`], the actions runner in an arm64 Ubuntu VM under tart, which runs clippy for
//!   the Linux triples and the Linux tests. The VM runs only while a gate needs it: the Mac runner
//!   wakes it as each of its jobs starts, and it stops once idle ([`linux::run`]).
//!
//! Both refuse every job but the gate's ([`guard`]). Everything they keep is on a volume of their
//! own ([`VOLUME`]): an APFS disk image on the checkout's volume, beside the checkout and never in
//! it ([`attach`]). A launchd job may not open a file on an external disk, the person's checkouts
//! there among them, but may use a disk image attached from one.

mod guard;
mod linux;
mod mac;
mod uninstall;

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
        /// A file whose time is set to now once the job is allowed: the Mac runner's, which
        /// wakes the Linux runner's VM.
        #[arg(long)]
        wake: Option<Utf8PathBuf>,
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
    /// Everything of the warm runners taken away: their registrations, `SLOPTY_RUNNERS` when it
    /// sends the gate to them, `try`, the VM, the daemons and the runner's account (through
    /// `sudo`, when an administrator ever installed them), and the volume detached.
    Uninstall {
        /// Delete the volume's image too, with the toolchains and builds the runners kept.
        #[arg(long)]
        image: bool,
        /// Print what it would do, and do nothing but write the administrator's part
        /// (`target/warm/uninstall.sh`).
        #[arg(long)]
        dry_run: bool,
    },
}

/// `xtask warm mac`.
#[derive(Debug, clap::Subcommand)]
pub enum MacCmd {
    /// Everything that needs no administrator: the warm runners' volume, attached, and on it the
    /// runner, rustup, the guard and the sandbox; and the script an administrator runs to create
    /// the user and load the daemons (`target/warm/admin.sh`).
    Install,
    /// Register the runner with GitHub, by a registration token from `gh`, held in memory only.
    Register,
    /// Run the runner as you, in its sandbox at its priority, through an agent of your login
    /// session, until the daemon takes over.
    Try {
        /// Stop it instead.
        #[arg(long)]
        stop: bool,
    },
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
    /// Wake the VM and wait until a command runs in it, starting its supervisor as you in the
    /// background unless one runs.
    Start,
    /// The VM's supervisor in the foreground, at `Nice 10`: the VM booted while the Mac runner
    /// wants it and stopped once idle, until this process ends. The VM's daemon runs it.
    Run,
    /// Stop the VM, and the supervisor `start` started.
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
        WarmCmd::Guard { config, wake } => {
            let config = match config {
                Some(config) => config.clone(),
                None => beside_exe(GUARD_CONFIG)?,
            };
            guard::run(&config, wake.as_deref())
        }
        WarmCmd::Mac { cmd: MacCmd::Install } => {
            mac::install(sh, &attach(sh)?, &image(sh)?, &admin_path(sh)?)
        }
        WarmCmd::Mac { cmd: MacCmd::Register } => mac::register(sh, &home(), &image(sh)?),
        WarmCmd::Mac { cmd: MacCmd::Try { stop } } => mac::try_run(sh, &home(), *stop),
        WarmCmd::Linux { cmd: LinuxCmd::Install { rev } } => linux::install(sh, &attach(sh)?, rev),
        WarmCmd::Linux { cmd: LinuxCmd::Start } => linux::start(sh, &home()),
        WarmCmd::Linux { cmd: LinuxCmd::Run } => linux::run(&home()),
        WarmCmd::Linux { cmd: LinuxCmd::Stop } => linux::stop(sh, &home()),
        WarmCmd::Linux { cmd: LinuxCmd::Exec { command } } => linux::exec(sh, &home(), command),
        WarmCmd::Status => status(sh),
        WarmCmd::Uninstall { image: delete, dry_run } => {
            let script = admin_path(sh)?.with_file_name("uninstall.sh");
            uninstall::run(sh, &home(), &image(sh)?, &script, *delete, *dry_run)
        }
    }
}

/// `name` beside this binary.
fn beside_exe(name: &str) -> Result<Utf8PathBuf> {
    let exe = std::env::current_exe().context("this binary's path")?;
    let exe = Utf8PathBuf::from_path_buf(exe)
        .map_err(|p| anyhow::anyhow!("this binary's path is not UTF-8: {}", p.display()))?;
    Ok(exe.parent().context("this binary has no directory")?.join(name))
}

/// Where the warm runners' volume is attached: their home, which holds everything they keep.
pub const VOLUME: &str = "/Volumes/slopty-ci";

/// The volume's image, at the root of the checkout's volume, and what it may take of that
/// volume: both runners' toolchains and target dirs, and the Linux VM's disk. A sparse bundle
/// takes only what is written into it, and gives back what was deleted when compacted
/// (`hdiutil compact`, detached).
const IMAGE: &str = "slopty-ci.sparsebundle";
const IMAGE_SIZE: &str = "180g";

/// How the volume is mounted: ownership off, as on an external disk, so the runners' user and
/// the person both reach it; no set-user-ID program or device file on it.
const MOUNT_OPTIONS: &str = "noowners,nosuid,nodev";

/// The warm runners' home: [`VOLUME`].
fn home() -> Utf8PathBuf {
    Utf8PathBuf::from(VOLUME)
}

/// The checkout this xtask runs in.
fn checkout(sh: &Shell) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(sh.current_dir())
        .map_err(|p| anyhow::anyhow!("the checkout's path is not UTF-8: {}", p.display()))
}

/// The volume's image: at the root of the volume the checkout is on, the external disk the
/// builds use, or where `SLOPTY_WARM_IMAGE` says.
fn image(sh: &Shell) -> Result<Utf8PathBuf> {
    if let Some(named) = std::env::var("SLOPTY_WARM_IMAGE").ok().filter(|i| !i.is_empty()) {
        return Ok(Utf8PathBuf::from(named));
    }
    Ok(mount_point(&checkout(sh)?)?.join(IMAGE))
}

/// The administrator's script (`mac::admin_script`), in the checkout's `target/`: off the
/// volume, which the runner's jobs write, and out of their sandbox's reach.
fn admin_path(sh: &Shell) -> Result<Utf8PathBuf> {
    Ok(checkout(sh)?.join("target").join("warm").join("admin.sh"))
}

/// The warm runners' home, attached: their image made unless it exists, and attached at
/// [`VOLUME`] unless it is. Its daemon attaches it again at every start of the Mac
/// (`mac::admin_script`).
fn attach(sh: &Shell) -> Result<Utf8PathBuf> {
    let home = home();
    if mounted(&home) {
        return Ok(home);
    }
    let image = image(sh)?;
    if !image.exists() {
        println!("▶ the warm runners' volume, {image}, of up to {IMAGE_SIZE}");
        cmd!(
            sh,
            "diskutil image create blank --format UDSB --fs APFS --size {IMAGE_SIZE} --volumeName slopty-ci {image}"
        )
        .quiet()
        .ignore_stdout()
        .run()?;
    }
    let attach = attach_args(&image);
    cmd!(sh, "diskutil {attach...}").quiet().ignore_stdout().run()?;
    anyhow::ensure!(mounted(&home), "{image} attached, but not at {home}");
    println!("  ✓ {image} attached at {home}");
    Ok(home)
}

/// `diskutil`'s arguments that attach `image` at [`VOLUME`] with [`MOUNT_OPTIONS`], out of the
/// Finder's sidebar.
fn attach_args(image: &Utf8Path) -> Vec<String> {
    ["image", "attach", "--nobrowse", "--mountPoint", VOLUME, "--mountOptions", MOUNT_OPTIONS]
        .iter()
        .map(|&arg| arg.to_owned())
        .chain([image.to_string()])
        .collect()
}

/// Whether a volume is mounted at `path`: its device is not its parent's.
fn mounted(path: &Utf8Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let device = |p: &Utf8Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    path.parent().is_some_and(|parent| match (device(path), device(parent)) {
        (Some(here), Some(above)) => here != above,
        _ => false,
    })
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

/// The files that hold a runner's registration, in its directory: `config.sh` refuses to register
/// one that has them.
const REGISTRATION: [&str; 3] = [".runner", ".credentials", ".credentials_rsaparams"];

/// Whether `repository`'s runner `name` holds the labels every self-hosted runner takes unless
/// told not to (`self-hosted`, its system's, its architecture's), which any workflow could ask
/// for; a warm runner holds its own name alone.
fn default_labelled(sh: &Shell, repository: &str, name: &str) -> Result<bool> {
    let path = format!("repos/{repository}/actions/runners?per_page=100");
    let query = format!(".runners[] | select(.name == \"{name}\") | .labels[].name");
    let labels = cmd!(sh, "gh api {path} --jq {query}").quiet().read()?;
    Ok(labels.lines().any(|label| label == "self-hosted"))
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

/// The job-started hook's script: the guard, run on the job's environment, waking what `wake`
/// names once it allows the job. A refusal exits non-zero, which fails the job before its first
/// step.
fn hook_script(guard: &Utf8Path, config: &Utf8Path, wake: Option<&Utf8Path>) -> String {
    let wake = wake.map(|wake| format!(" --wake '{wake}'")).unwrap_or_default();
    format!("#!/bin/sh\nexec '{guard}' warm guard --config '{config}'{wake}\n")
}

/// The file the Mac runner's hook touches as each of its jobs starts (`guard --wake`), whose time
/// wakes the Linux runner's VM ([`linux::run`]): in the Mac runner's own directory, which its
/// jobs may write, so a job can do no more with it than wake the VM.
fn wake_file(home: &Utf8Path) -> Utf8PathBuf {
    home.join("mac").join("linux.wanted")
}

/// `path`'s modification time set to now, the file made if missing.
fn touch(path: &Utf8Path) -> Result<()> {
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()))
        .with_context(|| format!("touch {path}"))
}

/// How long ago `path` was last modified, or `None` when it is missing.
fn age(path: &Utf8Path) -> Option<std::time::Duration> {
    let modified = std::fs::metadata(path).and_then(|meta| meta.modified()).ok()?;
    Some(std::time::SystemTime::now().duration_since(modified).unwrap_or_default())
}

/// What a runner takes out of its environment as it starts, whatever started it: the SSH agent
/// a login session hands its jobs (`docs/decisions/tooling.md`, "The gate runs on warm runners
/// here").
const UNSET: [&str; 1] = ["SSH_AUTH_SOCK"];

/// A runner's `.env`: `KEY=value` lines the runner gives every job's environment, then a `KEY=`
/// line for each of [`UNSET`], which the runner takes out of its own.
fn dotenv(vars: &[(&str, String)]) -> String {
    let set = vars.iter().map(|(key, value)| (*key, value.as_str()));
    set.chain(UNSET.map(|key| (key, ""))).fold(String::new(), |mut out, (key, value)| {
        let _written = writeln!(out, "{key}={value}");
        out
    })
}

/// Both runners as GitHub lists them, the Mac daemon's state and the VM's, and the free space
/// where they build.
fn status(sh: &Shell) -> Result<()> {
    let home = home();
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
    if mounted(&home) {
        if let Ok(free) = crate::prune::free_space(&home) {
            println!("  free on {home}: {} GB", free / 1_000_000_000);
        }
    } else {
        println!("  {home} is not attached");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use super::{
        RUNNER_BUILDS, age, attach_args, dotenv, hook_script, mount_point, mounted, touch,
    };

    /// Every runner build is pinned by a SHA-256 of 64 hex digits, for both hosts.
    #[test]
    fn the_runner_s_builds_are_pinned() {
        for build in ["osx-arm64", "linux-arm64"] {
            let (_, sha) = RUNNER_BUILDS.iter().find(|(b, _)| *b == build).expect(build);
            assert!(sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()), "{sha}");
        }
    }

    /// The hook runs the guard on its config and nothing else, quoted, and the Mac's wakes the
    /// VM's file; the `.env` is one line a variable, and ends by taking the SSH agent out.
    #[test]
    fn the_hook_and_the_environment_are_written_whole() {
        let (guard, config) = (Utf8Path::new("/r/guard/xtask"), Utf8Path::new("/r/guard/w.toml"));
        assert_eq!(
            hook_script(guard, config, None),
            "#!/bin/sh\nexec '/r/guard/xtask' warm guard --config '/r/guard/w.toml'\n"
        );
        assert_eq!(
            hook_script(guard, config, Some(Utf8Path::new("/r/linux.wanted"))),
            "#!/bin/sh\nexec '/r/guard/xtask' warm guard --config '/r/guard/w.toml' --wake \
             '/r/linux.wanted'\n"
        );
        let env = dotenv(&[("A", "1".to_owned()), ("TMPDIR", "/r/tmp/".to_owned())]);
        assert_eq!(env, "A=1\nTMPDIR=/r/tmp/\nSSH_AUTH_SOCK=\n");
    }

    /// A touched file is made, or its time moved to now; a missing one has no age.
    #[test]
    fn a_touch_makes_the_file_new() {
        use std::time::{Duration, SystemTime};

        let dir = std::env::temp_dir().join(format!("xtask-warm-touch-{}", std::process::id()));
        let path = camino::Utf8PathBuf::from_path_buf(dir.join("wanted")).expect("UTF-8");
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        assert_eq!(age(&path), None);
        touch(&path).expect("made");
        let hour_ago = SystemTime::now().checked_sub(Duration::from_hours(1)).expect("a time");
        let aged = std::fs::File::options().append(true).open(&path);
        aged.and_then(|file| file.set_modified(hour_ago)).expect("aged");
        assert!(age(&path).is_some_and(|age| age >= Duration::from_mins(59)));
        touch(&path).expect("touched");
        assert!(age(&path).is_some_and(|age| age < Duration::from_mins(1)));
        std::fs::remove_dir_all(&dir).expect("the scratch directory removed");
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

    /// The volume is attached at its mount point with ownership, set-user-ID programs and
    /// devices off, out of the Finder; a mount point is told from a plain directory.
    #[test]
    fn the_volume_attaches_at_its_mount_point() {
        let args = attach_args(Utf8Path::new("/Volumes/B/slopty-ci.sparsebundle"));
        assert_eq!(
            args,
            [
                "image",
                "attach",
                "--nobrowse",
                "--mountPoint",
                "/Volumes/slopty-ci",
                "--mountOptions",
                "noowners,nosuid,nodev",
                "/Volumes/B/slopty-ci.sparsebundle",
            ]
        );
        let here = Utf8Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(!mounted(here), "{here} is a directory, not a mount point");
        assert!(!mounted(&here.join("no such directory")));
        let point = mount_point(here).expect("a mount point");
        assert!(point == "/" || mounted(&point), "{point} is a mount point");
    }
}
