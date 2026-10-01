//! `cargo xtask vm`: macOS guests for the live tests that move the real pointer, post HID events,
//! lock the screen, reach the login window or need a TCC grant. None of that may happen on this
//! Mac while someone works on it (over Parsec) or on the other Mac, so it happens in a guest.
//!
//! The guests run under [tart] (`brew install openai/tools/tart`), from the Cirrus Labs base images
//! pinned by digest ([`Macos::image`]): macOS set up with an `admin` account that logs in at boot,
//! SSH on, no sleep, screen saver or screen lock, SIP off, and the tart guest agent in the
//! logged-in session with Accessibility, Screen Recording and `PostEvent` granted.
//! `docs/decisions/testing.md` ▸ **Live tests that drive the desktop run in a macOS guest under
//! tart** says why this and not lume or Virtualization.framework from Rust.
//!
//! - Everything lives under `SLOPTY_VM_HOME` when it is set, else under `/Volumes/Lacie/vms`
//!   ([`home`]), and never on the startup disk: tart's images and guests (`TART_HOME`), the SSH key
//!   the guests trust, the MAC locks, the logs.
//! - `create` pulls the image. tart checks every layer against its digest, and a pull that stops
//!   resumes: it writes into a directory named after the image, and the next pull skips each layer
//!   whose bytes there already hash to the layer's digest. The pulled manifest is then hashed here
//!   against the pin. Then it makes the base guest from the image (`slopty-26`, `slopty-27`):
//!   [`CPUS`] and [`MEMORY_MB`], the key, the worker's own TCC grants, fish, no Spotlight, no
//!   software update. Nothing boots the base again; every other guest is a copy-on-write clone.
//! - `start`/`stop`/`ssh`/`exec`/`deploy` work on a long-lived clone (`slopty-26-dev`). A guest
//!   runs headless (`--no-graphics`) on tart's shared NAT network, where this Mac reaches it at its
//!   own address. (The host-only network, `--net-host`, runs Softnet as root, which needs a sudo
//!   rule this lane does not install.)
//! - `deploy` builds this tree's `slopty-ptyd`, `slopty-worker` and `slopty` and runs the real
//!   `slopty worker deploy` (`slopty-deploy`) against the guest. The CLI takes an `ssh` program but
//!   no ssh options, so this binary stands in as that program: `slopty worker deploy vm --ssh
//!   <xtask>` makes the CLI run `xtask vm <script>`, which runs the system `ssh` with the guest's
//!   address, key and options ([`VmOpts::script`]).
//! - `live -p <crate> [--test <target>] -- <nextest filters>` runs tests in a fresh clone against
//!   the guest's own worker. A nextest archive of the crates' tests goes into the guest at this
//!   checkout's own path (so the paths compiled into the tests hold) and runs there in the
//!   logged-in session through `tart exec`, one test at a time (there is one pointer), with
//!   `SLOPTY_INPUT_E2E`, `SLOPTY_SCREEN_E2E`, `SLOPTY_DND_E2E` and `SLOPTY_VM` set. Under
//!   `SLOPTY_VM` a test that would skip itself fails instead (`slopty_testkit::live::skip`):
//!   nothing a live test needs is missing in a guest. nextest's reports and `target/e2e` come back
//!   under `target/vm/<guest>`.
//! - `e2e` is the lane's own proof: a host test (`slopty-e2e --test vm`) reaches the guest's worker
//!   over the VM network, and a guest test posts a real HID pointer move and reads it back.
//!
//! A run's guest is `<base>-run-<pid>-<time>`, or `<base>-kept-<pid>-<time>` with `--keep`. It is
//! removed when the run ends, and on SIGINT, SIGTERM or SIGHUP ([`watch_signals`]). A run killed
//! outright leaves its guest behind; the next `live`, `e2e` or `prune` removes each run guest whose
//! process is gone, and never one whose process lives. Run guests take one of two fixed MACs
//! ([`RUN_MACS`]), held by a lock for the run's life, so the host's DHCP server leases two
//! addresses for them however many runs there are. macOS lets two macOS guests run at once (its
//! licence, enforced by Virtualization.framework).
//!
//! [tart]: https://github.com/openai/tart

use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Subcommand, ValueEnum};
use xshell::{Shell, cmd};

use crate::tools::{has, repo_root, step, workspace_packages};

/// Where the guests live unless `SLOPTY_VM_HOME` says otherwise: the external disk.
const DEFAULT_HOME: &str = "/Volumes/Lacie/vms";
/// Cores a guest may use.
const CPUS: &str = "4";
/// Memory a guest may use, in MB.
const MEMORY_MB: &str = "8192";
/// The account the images log in as, with passwordless sudo.
const USER: &str = "admin";
/// The worker's QUIC port (`slopty_net::endpoint::WORKER_PORT`).
const WORKER_PORT: u16 = 45550;
/// Where `slopty worker install` puts the worker in the guest: the path its grants are for.
const WORKER_EXE: &str = "/Users/admin/Library/Application Support/Slopty/bin/slopty-worker";
/// The worker's settings in the guest, as a shell word.
const GUEST_SETTINGS_DIR: &str = "\"$HOME/Library/Application Support/Slopty\"";
/// Longest a guest may take from `tart run` to a command running in its logged-in session.
const BOOT: Duration = Duration::from_secs(300);
/// Longest one look at whether the guest agent answers may take.
const PROBE: Duration = Duration::from_secs(10);
/// The environment variable the stand-in `ssh` reads the guest's address from.
const SSH_HOST_VAR: &str = "SLOPTY_VM_SSH_HOST";
/// What the guest runs the tests with, beside the archive.
const GUEST_STAGE: &str = "/Users/admin/slopty-live";
/// The MACs run guests take, one each: two, because macOS runs two macOS guests at most. Locally
/// administered, so they collide with no real interface.
const RUN_MACS: [&str; 2] = ["7e:51:0b:7e:00:01", "7e:51:0b:7e:00:02"];
/// The options every `ssh` to a guest takes after its key.
const SSH_OPTIONS: [&str; 9] = [
    "IdentitiesOnly=yes",
    "BatchMode=yes",
    "StrictHostKeyChecking=no",
    "UserKnownHostsFile=/dev/null",
    "LogLevel=ERROR",
    "ConnectTimeout=10",
    "ServerAliveInterval=10",
    "ControlMaster=auto",
    "ControlPersist=30",
];

/// `cargo xtask vm` (with no subcommand and one argument, the `ssh` of `slopty worker deploy`).
#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct VmOpts {
    #[command(subcommand)]
    cmd: Option<VmCmd>,
    /// The script `slopty worker deploy` runs on the guest when this binary is its `ssh`.
    #[arg(hide = true)]
    script: Option<String>,
}

/// What to do with the guests.
#[derive(Subcommand, Debug)]
pub enum VmCmd {
    /// Pull the pinned image and make the base guest every other guest is cloned from.
    Create {
        #[arg(long, value_enum, default_value_t)]
        macos: Macos,
        /// Make the base again even when it exists.
        #[arg(long)]
        fresh: bool,
    },
    /// Boot the guest headless (cloned from the base when it does not exist yet).
    Start(Which),
    /// Shut the guest down.
    Stop(Which),
    /// A shell on the guest, or `-- <command>` run there, over SSH.
    Ssh {
        #[command(flatten)]
        which: Which,
        /// The command to run instead of a shell.
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// `-- <command>` run in the guest's logged-in session (`tart exec`), where events post.
    Exec {
        #[command(flatten)]
        which: Which,
        /// The command.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Build this tree's worker and put it on the guest with `slopty worker deploy` (cloned from
    /// the base when it does not exist yet).
    Deploy(Which),
    /// Run live tests in a fresh guest, against its own worker, and bring the results back.
    Live(LiveOpts),
    /// The lane's own proof: the host reaches the guest's worker, and a real HID event posted in
    /// the guest arrives.
    E2e {
        #[arg(long, value_enum, default_value_t)]
        macos: Macos,
        /// Leave the guest running afterwards (`cargo xtask vm prune --kept` removes it).
        #[arg(long)]
        keep: bool,
    },
    /// The guests and images, with their state and size.
    List,
    /// Remove the run guests whose process is gone; `--kept` also the kept ones, `--all` every
    /// guest, image and the key (refused while a run's process lives).
    Prune {
        #[arg(long)]
        kept: bool,
        #[arg(long)]
        all: bool,
    },
}

/// Which guest.
#[derive(Args, Debug, Clone)]
pub struct Which {
    #[arg(long, value_enum, default_value_t)]
    macos: Macos,
    /// A guest other than the long-lived `slopty-<macos>-dev`.
    #[arg(long)]
    name: Option<String>,
}

impl Which {
    fn name(&self) -> String {
        self.name.clone().unwrap_or_else(|| format!("{}-dev", self.macos.base()))
    }
}

/// `cargo xtask vm live`.
#[derive(Args, Debug)]
pub struct LiveOpts {
    #[arg(long, value_enum, default_value_t)]
    macos: Macos,
    /// A crate whose tests go to the guest; repeat for several.
    #[arg(short = 'p', long = "package", required = true)]
    packages: Vec<String>,
    /// Only this integration test target of the crates goes; repeat for several.
    #[arg(long = "test")]
    tests: Vec<String>,
    /// Leave the guest running afterwards (`cargo xtask vm prune --kept` removes it).
    #[arg(long)]
    keep: bool,
    /// What `cargo nextest run` takes in the guest from an archive: test name filters, `-E
    /// <filterset>`, but no build selection (`--test` is this command's own).
    #[arg(last = true)]
    args: Vec<String>,
}

/// A macOS release a guest runs.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Macos {
    /// macOS 26 (Tahoe), the floor's major release.
    #[default]
    #[value(name = "26")]
    V26,
    /// macOS 27.
    #[value(name = "27")]
    V27,
}

impl Macos {
    /// The Cirrus Labs base image, by digest: a new one is taken on purpose, never by a tag moving.
    const fn image(self) -> &'static str {
        match self {
            // macOS 26.6.2 (25G83), uploaded 2026-09-05.
            Self::V26 => {
                "ghcr.io/cirruslabs/macos-tahoe-base@sha256:1b093499716409d29e8b5336844528e1cae375db97d2ad8e5aeff78cf0da201e"
            }
            // macOS 27.0, uploaded 2026-09-21.
            Self::V27 => {
                "ghcr.io/cirruslabs/macos-golden-gate-base@sha256:9a2f20d179d6418a128dcb76c593588ea85a263dbfa286e757db63f3d949af7c"
            }
        }
    }

    /// The base guest's name.
    const fn base(self) -> &'static str {
        match self {
            Self::V26 => "slopty-26",
            Self::V27 => "slopty-27",
        }
    }

    /// The major version `sw_vers` says.
    const fn major(self) -> &'static str {
        match self {
            Self::V26 => "26",
            Self::V27 => "27",
        }
    }
}

pub fn run(sh: &Shell, opts: &VmOpts) -> Result<()> {
    if let Some(script) = &opts.script {
        return stand_in_ssh(script);
    }
    let Some(cmd) = &opts.cmd else { bail!("a subcommand: `cargo xtask vm --help`") };
    ensure!(has(sh, "tart"), "tart runs the guests: `brew install openai/tools/tart`");
    let home = home()?;
    match cmd {
        VmCmd::Create { macos, fresh } => create(&home, *macos, *fresh),
        VmCmd::Start(which) => {
            let name = which.name();
            ensure_clone(&home, which.macos, &name)?;
            let booted = boot(&home, &name)?;
            println!("▶ {name} is up at {} (booted in {booted:.1?})", ip(&home, &name)?);
            Ok(())
        }
        VmCmd::Stop(which) => stop(&home, &which.name()),
        VmCmd::Ssh { which, command } => {
            let ip = ip(&home, &which.name())?;
            let status = ssh(&home, &ip).args(command).status().context("ssh")?;
            ensure!(status.success(), "ssh: {status}");
            Ok(())
        }
        VmCmd::Exec { which, command } => {
            let status = exec(&home, &which.name(), command).status().context("tart exec")?;
            ensure!(status.success(), "{}: {status}", command.join(" "));
            Ok(())
        }
        VmCmd::Deploy(which) => {
            let name = which.name();
            let bins = build_worker(sh)?;
            ensure_clone(&home, which.macos, &name)?;
            deploy(sh, &home, &name, &bins).map(|_took| ())
        }
        VmCmd::Live(opts) => live(sh, &home, opts),
        VmCmd::E2e { macos, keep } => e2e(sh, &home, *macos, *keep),
        VmCmd::List => list(&home),
        VmCmd::Prune { kept, all } => prune(&home, *kept, *all),
    }
}

/// Where the guests live, made if absent and canonical: never on the startup disk, whose free
/// space cannot take a guest.
fn home() -> Result<Utf8PathBuf> {
    let root = std::env::var("SLOPTY_VM_HOME").unwrap_or_else(|_| DEFAULT_HOME.to_owned());
    let root = Utf8PathBuf::from(root);
    // Before anything is made: a path under an unmounted volume would otherwise be made on the
    // startup disk, where it would also hide the volume's mount point.
    let existing = root.ancestors().find(|a| a.exists()).context("no part of the home exists")?;
    off_the_startup_disk(existing.as_std_path())?;
    for dir in ["tart", "ssh", "logs", "locks"] {
        std::fs::create_dir_all(root.join(dir)).with_context(|| format!("mkdir {root}/{dir}"))?;
    }
    let canonical = root.canonicalize_utf8().with_context(|| format!("canonicalize {root}"))?;
    off_the_startup_disk(canonical.as_std_path())?;
    Ok(canonical)
}

/// Refuse `path` when it is on the disk macOS started from: any volume of the startup disk's APFS
/// container (the system, its data, a volume made beside them), whatever its device number.
fn off_the_startup_disk(path: &Path) -> Result<()> {
    let startup = whole_disk_of(Path::new("/"))?.context("the startup volume names no disk")?;
    if whole_disk_of(path)?.as_deref() == Some(startup.as_str()) {
        bail!(
            "{} is on the startup disk ({startup}); set SLOPTY_VM_HOME to a directory on another \
             disk, or mount the external one",
            path.display()
        );
    }
    Ok(())
}

/// The whole disk the volume holding `path` was mounted from (`disk3` for `/dev/disk3s5`), or
/// `None` for a volume from no local disk.
#[cfg(target_os = "macos")]
fn whole_disk_of(path: &Path) -> Result<Option<String>> {
    let stat = rustix::fs::statfs(path).with_context(|| format!("statfs {}", path.display()))?;
    let from: Vec<u8> =
        stat.f_mntfromname.iter().map(|c| c.cast_unsigned()).take_while(|&b| b != 0).collect();
    Ok(whole_disk(&String::from_utf8_lossy(&from)))
}

/// No tart guest runs off this host: there is no disk to name.
#[cfg(not(target_os = "macos"))]
#[expect(clippy::unnecessary_wraps, reason = "the macOS twin's signature")]
const fn whole_disk_of(_path: &Path) -> Result<Option<String>> {
    Ok(None)
}

/// `disk<N>` from a device path `/dev/disk<N>[s<M>…]`; an APFS volume's `N` is its container's.
#[cfg(any(target_os = "macos", test))]
fn whole_disk(device: &str) -> Option<String> {
    let number: String =
        device.strip_prefix("/dev/disk")?.chars().take_while(char::is_ascii_digit).collect();
    (!number.is_empty()).then(|| format!("disk{number}"))
}

/// `tart` with its home under `home`, never pruning on its own.
fn tart(home: &Utf8Path) -> Command {
    let mut tart = Command::new("tart");
    tart.env("TART_HOME", home.join("tart")).env("TART_NO_AUTO_PRUNE", "1");
    tart
}

/// Run `tart args…` to the end; its standard output, trimmed.
fn tart_out(home: &Utf8Path, args: &[&str]) -> Result<String> {
    let out = tart(home).args(args).stdin(Stdio::null()).output().context("run tart")?;
    ensure!(
        out.status.success(),
        "tart {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Run `tart args…` with its output on this terminal.
fn tart_shown(home: &Utf8Path, args: &[&str]) -> Result<()> {
    let status = tart(home).args(args).stdin(Stdio::null()).status().context("run tart")?;
    ensure!(status.success(), "tart {}: {status}", args.join(" "));
    Ok(())
}

/// One guest or image as `tart list` sees it.
#[derive(serde::Deserialize, Debug)]
#[serde(rename_all = "PascalCase")]
struct Listed {
    name: String,
    source: String,
    state: String,
    /// GB its files take on disk.
    size: u64,
}

fn listed(home: &Utf8Path) -> Result<Vec<Listed>> {
    Ok(serde_json::from_str(&tart_out(home, &["list", "--format", "json"])?)?)
}

fn exists(home: &Utf8Path, name: &str) -> Result<bool> {
    Ok(listed(home)?.iter().any(|vm| vm.source == "local" && vm.name == name))
}

fn running(home: &Utf8Path, name: &str) -> Result<bool> {
    Ok(listed(home)?.iter().any(|vm| vm.name == name && vm.state == "running"))
}

/// The key the guests trust, made on first use; the public half.
fn key(home: &Utf8Path) -> Result<String> {
    let private = home.join("ssh/id_ed25519");
    if !private.exists() {
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "slopty-vm", "-f", private.as_str()])
            .stdin(Stdio::null())
            .status()
            .context("ssh-keygen")?;
        ensure!(status.success(), "ssh-keygen: {status}");
    }
    Ok(std::fs::read_to_string(home.join("ssh/id_ed25519.pub"))?.trim().to_owned())
}

/// The options before the host for every `ssh` to a guest, as `ssh` and `rsync -e` take them. A
/// guest's host key is a clone's and its address is leased again to the next guest, so neither is
/// remembered.
fn ssh_args(home: &Utf8Path) -> Vec<String> {
    let mut args = vec!["-i".to_owned(), home.join("ssh/id_ed25519").into_string()];
    for option in SSH_OPTIONS {
        args.extend(["-o".to_owned(), option.to_owned()]);
    }
    args.extend(["-o".to_owned(), format!("ControlPath={home}/ssh/cm-%C")]);
    args.extend(["-l".to_owned(), USER.to_owned()]);
    args
}

/// `ssh` to the guest at `ip` as [`USER`], with the guests' key and nothing asked.
fn ssh(home: &Utf8Path, ip: &str) -> Command {
    let mut ssh = Command::new("ssh");
    ssh.args(ssh_args(home)).arg(ip);
    ssh
}

/// `script` run by `sh` on the guest at `ip`; its standard output, trimmed.
fn ssh_out(home: &Utf8Path, ip: &str, script: &str) -> Result<String> {
    let out = ssh(home, ip).arg(script).stdin(Stdio::null()).output().context("ssh")?;
    ensure!(
        out.status.success(),
        "`{script}` on the guest: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Stand in for `ssh` under `slopty worker deploy vm --ssh <this binary>`: the CLI runs this
/// binary as `xtask vm <script>`, and this runs the real one on the guest the parent named.
fn stand_in_ssh(script: &str) -> Result<()> {
    let ip = std::env::var(SSH_HOST_VAR)
        .with_context(|| format!("{SSH_HOST_VAR} unset: only `cargo xtask vm deploy` runs this"))?;
    let home = home()?;
    Err(ssh(&home, &ip).arg(script).exec()).context("exec ssh")
}

/// `command` in `name`'s logged-in session, through the guest agent: a process there posts
/// events to the session's window server and holds the agent's grants.
fn exec(home: &Utf8Path, name: &str, command: &[String]) -> Command {
    let mut exec = tart(home);
    exec.args(["exec", name]).args(command);
    exec
}

/// Run `script` with `sh` in `name`'s logged-in session; its standard output, trimmed.
fn exec_out(home: &Utf8Path, name: &str, script: &str) -> Result<String> {
    let command = ["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()];
    let out = exec(home, name, &command).stdin(Stdio::null()).output().context("tart exec")?;
    ensure!(
        out.status.success(),
        "`{script}` in {name}: {}{}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// The guest's address on the VM network.
fn ip(home: &Utf8Path, name: &str) -> Result<String> {
    tart_out(home, &["ip", name, "--wait", "60"])
}

/// Wait up to `limit` for `child` to end; whether it ended well. One still running at the limit is
/// killed and counts as failed.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a child")]
fn finish_within(mut child: Child, limit: Duration) -> Result<bool> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.success());
        }
        if started.elapsed() >= limit {
            let _killed: std::io::Result<()> = child.kill();
            let _reaped: std::io::Result<std::process::ExitStatus> = child.wait();
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Boot `name` headless and wait until a command runs in its logged-in session; how long that
/// took. The guest keeps running after this process ends. One that does not come up in [`BOOT`]
/// is stopped.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a guest")]
fn boot(home: &Utf8Path, name: &str) -> Result<Duration> {
    if running(home, name)? {
        return Ok(Duration::ZERO);
    }
    let log = home.join("logs").join(format!("{name}.log"));
    let file = File::create(&log).with_context(|| format!("create {log}"))?;
    let started = Instant::now();
    let mut vm = tart(home)
        .args(["run", name, "--no-graphics"])
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .process_group(0)
        .spawn()
        .context("tart run")?;
    loop {
        if let Some(status) = vm.try_wait()? {
            bail!("tart run {name} exited with {status}; see {log}");
        }
        let probe = exec(home, name, &["/usr/bin/true".to_owned()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if finish_within(probe, PROBE)? {
            return Ok(started.elapsed());
        }
        if started.elapsed() >= BOOT {
            let stopped = tart_out(home, &["stop", name, "--timeout", "10"]);
            let _killed: std::io::Result<()> = vm.kill();
            let _reaped: std::io::Result<std::process::ExitStatus> = vm.wait();
            bail!("{name} did not come up in {BOOT:?} and was stopped ({stopped:?}); see {log}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn stop(home: &Utf8Path, name: &str) -> Result<()> {
    if running(home, name)? {
        tart_out(home, &["stop", name, "--timeout", "30"])?;
    }
    Ok(())
}

/// Stop `name` if it runs, then delete it.
fn remove(home: &Utf8Path, name: &str) -> Result<()> {
    if running(home, name)? {
        tart_out(home, &["stop", name, "--timeout", "10"])?;
    }
    tart_out(home, &["delete", name]).map(|_out| ())
}

/// `name` as a clone of the base, when it does not exist yet.
fn ensure_clone(home: &Utf8Path, macos: Macos, name: &str) -> Result<()> {
    if !exists(home, name)? {
        ensure_base(home, macos)?;
        tart_out(home, &["clone", macos.base(), name])?;
    }
    Ok(())
}

fn ensure_base(home: &Utf8Path, macos: Macos) -> Result<()> {
    ensure!(
        exists(home, macos.base())?,
        "no {} guest yet: `cargo xtask vm create --macos {}`",
        macos.base(),
        macos.major()
    );
    Ok(())
}

/// Pull `macos`'s image and make its base guest. The base is made under another name and takes
/// its own only once provisioned, so a failed `create` never leaves a base to clone from.
fn create(home: &Utf8Path, macos: Macos, fresh: bool) -> Result<()> {
    let base = macos.base();
    if exists(home, base)? && !fresh {
        println!("✓ {base} exists (`--fresh` makes it again)");
        return Ok(());
    }
    let pulled = Instant::now();
    tart_shown(home, &["pull", macos.image()])?;
    verify_pin(home, macos.image())?;
    println!("  pulled {} in {:.1?}, manifest verified", macos.image(), pulled.elapsed());
    let making = format!("{base}-new");
    if exists(home, &making)? {
        remove(home, &making)?;
    }
    tart_out(home, &["clone", macos.image(), &making])?;
    tart_out(home, &["set", &making, "--cpu", CPUS, "--memory", MEMORY_MB])?;
    let booted = boot(home, &making)?;
    println!("  {making} booted in {booted:.1?}");
    let provisioned = provision(home, macos, &making);
    stop(home, &making)?;
    provisioned?;
    if exists(home, base)? {
        remove(home, base)?;
    }
    tart_out(home, &["rename", &making, base])?;
    println!("✓ {base}: macOS {}, {CPUS} cores, {MEMORY_MB} MB", macos.major());
    list(home)
}

/// Hash the manifest tart pulled for `image` and hold it to the digest the image is pinned by.
fn verify_pin(home: &Utf8Path, image: &str) -> Result<()> {
    let (repository, digest) = image.split_once("@sha256:").context("an image pinned by digest")?;
    let manifest = home
        .join("tart/cache/OCIs")
        .join(repository)
        .join(format!("sha256:{digest}/manifest.json"));
    let out = Command::new("shasum")
        .args(["-a", "256", "-b", manifest.as_str()])
        .output()
        .context("shasum")?;
    ensure!(out.status.success(), "shasum {manifest}: {}", String::from_utf8_lossy(&out.stderr));
    let sum = String::from_utf8(out.stdout)?;
    let sum = sum.split_whitespace().next().unwrap_or_default();
    ensure!(sum == digest, "{manifest} hashes to {sum}, not the pinned {digest}");
    Ok(())
}

/// Make the fresh base what every guest starts from: the key trusted, the worker's grants in
/// both TCC databases, the shells the terminal tests drive, nothing indexing or updating in the
/// background.
fn provision(home: &Utf8Path, macos: Macos, base: &str) -> Result<()> {
    let version = exec_out(home, base, "sw_vers -productVersion")?;
    ensure!(
        version.split('.').next() == Some(macos.major()),
        "{base} runs macOS {version}, not {}",
        macos.major()
    );
    let sip = exec_out(home, base, "csrutil status")?;
    ensure!(sip.contains("disabled"), "{base} has SIP on ({sip}): its TCC databases are shut");
    let console = exec_out(home, base, "stat -f %Su /dev/console")?;
    ensure!(console == USER, "{base} is not logged in as {USER} ({console})");

    let key = key(home)?;
    exec_out(
        home,
        base,
        &format!(
            "mkdir -p ~/.ssh && chmod 700 ~/.ssh && \
             (grep -qxF '{key}' ~/.ssh/authorized_keys 2>/dev/null || echo '{key}' >> ~/.ssh/authorized_keys) && \
             chmod 600 ~/.ssh/authorized_keys"
        ),
    )?;
    exec_out(home, base, "sudo mdutil -a -i off >/dev/null && sudo softwareupdate --schedule off")?;
    // slopty-pty's shell integration drives fish beside zsh and bash; a guest lacks nothing.
    exec_out(home, base, "/opt/homebrew/bin/brew install --quiet fish >/dev/null")?;

    // The worker runs from launchd, so it is its own responsible process and needs grants of its
    // own; commands through `tart exec` hold the agent's. A row with no code requirement is
    // matched by path, which a rebuilt worker keeps.
    let rows = ["kTCCServiceAccessibility", "kTCCServiceScreenCapture", "kTCCServicePostEvent"]
        .map(|service| format!("('{service}', 1, '{WORKER_EXE}', 2, 0, 1, NULL, 'UNUSED')"))
        .join(", ");
    let insert = format!(
        "INSERT OR REPLACE INTO access (service, client_type, client, auth_value, auth_reason, \
         auth_version, indirect_object_identifier_type, indirect_object_identifier) VALUES {rows};"
    );
    // macOS 27 keeps the user's database in a container tccd holds open.
    let user_db = if macos == Macos::V26 {
        "$HOME/Library/Application Support/com.apple.TCC/TCC.db".to_owned()
    } else {
        exec_out(
            home,
            base,
            "sudo lsof -a -u $(id -u) -c tccd -Fn | sed -n 's|^n\\(/private/var/containers/Data/ProtectedSystem/.*/com.apple.TCC/TCC.db\\)$|\\1|p' | head -n 1",
        )?
    };
    ensure!(!user_db.is_empty(), "no user TCC database open in {base}");
    for db in ["/Library/Application Support/com.apple.TCC/TCC.db", user_db.as_str()] {
        exec_out(home, base, &format!("sudo sqlite3 \"{db}\" \"{insert}\""))?;
    }
    println!(
        "  {base}: key trusted, fish installed, worker granted Accessibility, Screen Recording \
         and PostEvent"
    );
    Ok(())
}

/// The directory cargo builds into, wherever `CARGO_TARGET_DIR` or the config put it.
fn target_dir(sh: &Shell) -> Result<Utf8PathBuf> {
    #[derive(serde::Deserialize)]
    struct Metadata {
        target_directory: Utf8PathBuf,
    }
    let json = cmd!(sh, "cargo metadata --format-version 1 --no-deps").quiet().read()?;
    Ok(serde_json::from_str::<Metadata>(&json)?.target_directory)
}

/// Build the worker's binaries for this Mac (and so for the guest); the directory they are in.
fn build_worker(sh: &Shell) -> Result<Utf8PathBuf> {
    step(
        "build slopty-ptyd, slopty-worker and slopty",
        &cmd!(sh, "cargo build -p slopty-ptyd -p slopty-workerd -p slopty-cli --bins"),
    )?;
    Ok(target_dir(sh)?.join("debug"))
}

/// Put the worker built into `bins` on `name` with `slopty worker deploy`, admitting this Mac's
/// address on the VM network; how long it took. `name` is booted if it is not running.
fn deploy(sh: &Shell, home: &Utf8Path, name: &str, bins: &Utf8Path) -> Result<Duration> {
    boot(home, name)?;
    let started = Instant::now();
    let ip = ip(home, name)?;
    // This Mac as the guest sees it: where the worker's packets will come from.
    let host = ssh_out(home, &ip, "echo ${SSH_CONNECTION%% *}")?;
    ensure!(!host.is_empty(), "no SSH_CONNECTION on the guest");
    admit(home, &ip, &host)?;
    let installed = ssh(home, &ip)
        .arg(format!("test -x '{WORKER_EXE}'"))
        .stdin(Stdio::null())
        .status()?
        .success();
    let xtask = std::env::current_exe().context("this binary")?;
    let mut deploy = Command::new(bins.join("slopty"));
    deploy
        .args(["worker", "deploy", "vm", "--no-server", "--bin-dir"])
        .arg(bins)
        .arg("--ssh")
        .arg(&xtask)
        .env(SSH_HOST_VAR, &ip)
        .env("SLOPTY_VM_HOME", home.as_str())
        .current_dir(sh.current_dir())
        .stdin(Stdio::null());
    if installed {
        deploy.arg("--update");
    }
    println!("▶ slopty worker deploy → {name} ({ip})");
    let status = deploy.status().context("slopty worker deploy")?;
    ensure!(status.success(), "slopty worker deploy: {status}");
    let took = started.elapsed();
    println!("  ✓ worker at {ip}:{WORKER_PORT} ({took:.1?})");
    Ok(took)
}

/// Add `host` to `[worker] allow` in the guest's settings, keeping every other key there. A file
/// that already admits it is left alone.
fn admit(home: &Utf8Path, ip: &str, host: &str) -> Result<()> {
    let file = format!("{GUEST_SETTINGS_DIR}/settings.toml");
    let text = ssh_out(home, ip, &format!("cat {file} 2>/dev/null || true"))?;
    let Some(merged) = admitting(&text, host)? else { return Ok(()) };
    let mut write = ssh(home, ip)
        .arg(format!("mkdir -p {GUEST_SETTINGS_DIR} && cat > {file}"))
        .stdin(Stdio::piped())
        .spawn()
        .context("ssh")?;
    write.stdin.take().context("ssh's stdin")?.write_all(merged.as_bytes())?;
    let status = write.wait()?;
    ensure!(status.success(), "write the guest's settings.toml: {status}");
    if !text.is_empty() {
        println!(
            "  added {host} to [worker] allow in the guest's settings.toml (comments not kept)"
        );
    }
    Ok(())
}

/// `text` (a settings file) with `host` in `[worker] allow`, or `None` when it is there already.
fn admitting(text: &str, host: &str) -> Result<Option<String>> {
    let mut settings: toml::Table = text.parse().context("the guest's settings.toml")?;
    let worker = settings.entry("worker").or_insert_with(|| toml::Table::new().into());
    let worker = worker.as_table_mut().context("[worker] is not a table")?;
    let allow = worker.entry("allow").or_insert_with(|| toml::Value::Array(Vec::new()));
    let allow = allow.as_array_mut().context("[worker] allow is not a list")?;
    if allow.iter().any(|a| a.as_str() == Some(host)) {
        return Ok(None);
    }
    allow.push(host.into());
    Ok(Some(toml::to_string(&settings)?))
}

/// The signal that asked this process to stop, 0 until one did.
static SIGNALLED: AtomicI32 = AtomicI32::new(0);
/// This process's run guest, for [`watch_signals`] to remove; set once, when it is cloned.
static RUN_GUEST: OnceLock<(Utf8PathBuf, String)> = OnceLock::new();
/// Who is removing the run guest: nobody yet, someone is, it is gone.
static REMOVAL: AtomicU8 = AtomicU8::new(REMOVAL_IDLE);
const REMOVAL_IDLE: u8 = 0;
const REMOVAL_BUSY: u8 = 1;
const REMOVAL_DONE: u8 = 2;

extern "C" fn on_signal(signal: libc::c_int) {
    SIGNALLED.store(signal, Ordering::SeqCst);
}

/// On SIGINT, SIGTERM or SIGHUP, remove this run's guest and exit as the signal would have. The
/// handler only records the signal; a thread does the rest, since a VM's `tart stop` is no work
/// for a signal handler.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a flag")]
fn watch_signals() -> Result<()> {
    let handler: extern "C" fn(libc::c_int) = on_signal;
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        #[expect(
            clippy::fn_to_numeric_cast_any,
            reason = "signal(3) takes the handler's address as a sighandler_t"
        )]
        let address = handler as libc::sighandler_t;
        // SAFETY: `signal(3)` installs a handler; `on_signal` only stores to a lock-free atomic,
        // which POSIX allows a signal handler to do (async-signal-safe).
        let previous = unsafe { libc::signal(signal, address) };
        ensure!(previous != libc::SIG_ERR, "install a handler for signal {signal}");
    }
    std::thread::spawn(|| {
        loop {
            let signal = SIGNALLED.load(Ordering::SeqCst);
            if signal != 0 {
                eprintln!("✘ signal {signal}: removing this run's guest");
                remove_run_guest();
                exit_for(signal);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    Ok(())
}

#[expect(
    clippy::exit,
    reason = "the process ends as the signal that stopped it would have ended it"
)]
fn exit_for(signal: libc::c_int) -> ! {
    std::process::exit(128_i32.saturating_add(signal))
}

/// Remove this run's guest, once, whether the run ended or a signal ended it; a second caller
/// waits for the first.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; waiting a turn")]
fn remove_run_guest() {
    let Some((home, name)) = RUN_GUEST.get() else { return };
    if REMOVAL
        .compare_exchange(REMOVAL_IDLE, REMOVAL_BUSY, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        while REMOVAL.load(Ordering::SeqCst) != REMOVAL_DONE {
            std::thread::sleep(Duration::from_millis(100));
        }
        return;
    }
    if let Err(e) = remove(home, name) {
        eprintln!(
            "✘ could not remove {name}: {e:#}; the next `cargo xtask vm prune` removes it once \
             this process is gone"
        );
    }
    REMOVAL.store(REMOVAL_DONE, Ordering::SeqCst);
}

/// The pid in a run guest's name, `<base>-run-<pid>-<time>`; `None` for any other guest.
fn run_pid(name: &str) -> Option<u32> {
    let (_base, rest) = name.split_once("-run-")?;
    rest.split('-').next()?.parse().ok()
}

/// Whether process `pid` exists (a zero signal reaches it, or it belongs to someone else).
fn alive(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else {
        return false;
    };
    !matches!(rustix::process::test_kill_process(pid), Err(rustix::io::Errno::SRCH))
}

/// The run guests among `names` whose process `alive` says is gone.
fn orphans<'a>(
    names: impl IntoIterator<Item = &'a str>,
    alive: impl Fn(u32) -> bool,
) -> Vec<&'a str> {
    names.into_iter().filter(|name| run_pid(name).is_some_and(|pid| !alive(pid))).collect()
}

/// Remove every run guest a process that is gone left behind.
fn reap_orphans(home: &Utf8Path) -> Result<()> {
    let guests = listed(home)?;
    let local = guests.iter().filter(|vm| vm.source == "local").map(|vm| vm.name.as_str());
    for name in orphans(local, alive) {
        remove(home, name)?;
        println!("  removed {name}: the run that made it is gone");
    }
    Ok(())
}

/// A lock on one of [`RUN_MACS`], held until dropped (or the process ends).
struct MacLock {
    _file: File,
    mac: &'static str,
}

/// Take a free one of [`RUN_MACS`].
fn claim_mac(home: &Utf8Path) -> Result<MacLock> {
    for (slot, mac) in RUN_MACS.iter().enumerate() {
        let path = home.join(format!("locks/mac-{slot}.lock"));
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {path}"))?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(MacLock { _file: file, mac }),
            Err(rustix::io::Errno::WOULDBLOCK) => {}
            Err(e) => return Err(e).with_context(|| format!("lock {path}")),
        }
    }
    bail!("both run MACs are taken by runs in progress (macOS runs two guests at most)")
}

/// Give guest `name` the MAC `mac`, before it boots.
fn set_mac(home: &Utf8Path, name: &str, mac: &str) -> Result<()> {
    let path = home.join("tart/vms").join(name).join("config.json");
    let mut config: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let object = config.as_object_mut().context("a guest's config is an object")?;
    object.insert("macAddress".to_owned(), mac.into());
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    Ok(())
}

/// A guest cloned for one run: removed when dropped (or on a signal) unless it is kept.
struct RunGuest {
    name: String,
    keep: bool,
    /// Held for the guest's life.
    mac: MacLock,
}

impl RunGuest {
    /// This process's run guest name for `macos`.
    fn name(macos: Macos, keep: bool) -> String {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let kind = if keep { "kept" } else { "run" };
        format!("{}-{kind}-{}-{stamp}", macos.base(), std::process::id())
    }

    /// `name`, a fresh clone of `macos`'s base with a run MAC; how long the clone took.
    fn clone(home: &Utf8Path, macos: Macos, name: String, keep: bool) -> Result<(Self, Duration)> {
        ensure_base(home, macos)?;
        let mac = claim_mac(home)?;
        let started = Instant::now();
        tart_out(home, &["clone", macos.base(), &name])?;
        let took = started.elapsed();
        if !keep && RUN_GUEST.set((home.to_owned(), name.clone())).is_err() {
            bail!("one run guest per process");
        }
        let guest = Self { name, keep, mac };
        set_mac(home, &guest.name, guest.mac.mac)?;
        Ok((guest, took))
    }
}

impl Drop for RunGuest {
    fn drop(&mut self) {
        if self.keep {
            println!("  kept {} (`cargo xtask vm prune --kept` removes it)", self.name);
        } else {
            remove_run_guest();
        }
    }
}

/// What one run is: its guest, and where its archive, binaries and results are.
struct Plan {
    macos: Macos,
    name: String,
    keep: bool,
    /// The worker's binaries, built.
    bins: Utf8PathBuf,
    /// `target/vm/<name>` on this Mac.
    dir: Utf8PathBuf,
}

impl Plan {
    /// Name the run's guest and make its directory, then build the worker.
    fn new(sh: &Shell, macos: Macos, keep: bool) -> Result<Self> {
        let name = RunGuest::name(macos, keep);
        let dir = run_dir(sh, &name)?;
        let bins = build_worker(sh)?;
        Ok(Self { macos, name, keep, bins, dir })
    }
}

/// A clone booted with this tree's worker on it.
struct Session {
    guest: RunGuest,
    ip: String,
    dir: Utf8PathBuf,
}

/// Clone the plan's guest from the base, boot it and deploy the plan's worker to it; each step
/// timed into `timings.txt` in the plan's directory.
fn session(sh: &Shell, home: &Utf8Path, plan: Plan) -> Result<Session> {
    let Plan { macos, name, keep, bins, dir } = plan;
    let used_before = used(home)?;
    let (guest, cloned) = RunGuest::clone(home, macos, name, keep)?;
    println!("▶ {} cloned in {cloned:.1?}", guest.name);
    let booted = boot(home, &guest.name)?;
    println!("  booted in {booted:.1?}");
    let deployed = deploy(sh, home, &guest.name, &bins)?;
    let ip = ip(home, &guest.name)?;
    // The whole volume's growth: an upper bound on the guest's writes, since other builds share it.
    let grown = used(home)?.saturating_sub(used_before);
    let timings = format!(
        "clone_s={:.2} boot_s={:.1} deploy_s={:.1} volume_grew_mb={}\n",
        cloned.as_secs_f64(),
        booted.as_secs_f64(),
        deployed.as_secs_f64(),
        grown / 1_000_000
    );
    print!("  {timings}");
    std::fs::write(dir.join("timings.txt"), timings)?;
    Ok(Session { guest, ip, dir })
}

/// Bytes in use on the volume `home` is on.
fn used(home: &Utf8Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(home.as_std_path())?;
    Ok(stat.f_blocks.saturating_sub(stat.f_bfree).saturating_mul(stat.f_frsize))
}

/// Where one run's archive and results go on this Mac.
fn run_dir(sh: &Shell, name: &str) -> Result<Utf8PathBuf> {
    let dir = Utf8PathBuf::try_from(sh.current_dir().join("target/vm").join(name))?;
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {dir}"))?;
    Ok(dir)
}

fn live(sh: &Shell, home: &Utf8Path, opts: &LiveOpts) -> Result<()> {
    watch_signals()?;
    reap_orphans(home)?;
    let plan = Plan::new(sh, opts.macos, opts.keep)?;
    let archive = archive(&plan.dir, &opts.packages, &opts.tests)?;
    let session = session(sh, home, plan)?;
    run_archive(home, &session, &archive, &opts.packages, &opts.args)
}

fn e2e(sh: &Shell, home: &Utf8Path, macos: Macos, keep: bool) -> Result<()> {
    watch_signals()?;
    reap_orphans(home)?;
    let plan = Plan::new(sh, macos, keep)?;
    let live = crate::e2e::LIVE;
    step(
        "build slopty-e2e vm",
        &cmd!(
            sh,
            "cargo nextest run -p slopty-e2e --test vm --features {live} --run-ignored only --no-run"
        ),
    )?;
    let packages = ["slopty-input".to_owned()];
    let archive = archive(&plan.dir, &packages, &["inject".to_owned()])?;

    let session = session(sh, home, plan)?;
    let worker = format!("{}:{WORKER_PORT}", session.ip);
    let _env =
        [sh.push_env("SLOPTY_VM_WORKER", &worker), sh.push_env("SLOPTY_VM_MACOS", macos.major())];
    step(
        "slopty-e2e vm: this Mac reaches the guest's worker",
        &cmd!(
            sh,
            "cargo nextest run -p slopty-e2e --test vm --features {live} --run-ignored only --no-capture"
        ),
    )?;
    let filter = [
        "-E".to_owned(),
        "binary(inject) & test(moves_the_real_pointer_on_a_display_stream)".to_owned(),
    ];
    run_archive(home, &session, &archive, &packages, &filter)
}

/// A nextest archive of `packages`' tests (only the `tests` targets, when named) in `dir`.
fn archive(dir: &Utf8Path, packages: &[String], tests: &[String]) -> Result<Utf8PathBuf> {
    let archive = dir.join("tests.tar.zst");
    let mut archiving = Command::new("cargo");
    archiving.args(["nextest", "archive", "--archive-file", archive.as_str()]);
    for package in packages {
        archiving.args(["-p", package]);
    }
    for test in tests {
        archiving.args(["--test", test]);
    }
    if packages.iter().any(|p| p == crate::e2e::LIVE_PACKAGE) {
        archiving.args(["--features", crate::e2e::LIVE]);
    }
    let started = Instant::now();
    println!("▶ archive the tests of {}", packages.join(", "));
    let root = repo_root()?;
    let status = archiving.current_dir(&root).status().context("cargo nextest archive")?;
    ensure!(status.success(), "cargo nextest archive: {status}");
    println!("  ✓ archived in {:.1?}", started.elapsed());
    debug_info(&debug_dir(&archive), &root, &test_binaries(&root, packages, tests)?)?;
    Ok(archive)
}

/// Where [`debug_info`] puts the dSYMs of `archive`'s binaries: beside it, laid out as under
/// the checkout.
fn debug_dir(archive: &Utf8Path) -> Utf8PathBuf {
    archive.with_file_name("debug")
}

/// What `cargo nextest list --message-format json` says, of what [`test_binaries`] reads.
#[derive(serde::Deserialize)]
struct TestList {
    #[serde(rename = "rust-binaries")]
    binaries: std::collections::BTreeMap<String, ListedBinary>,
}

/// One test binary of a [`TestList`].
#[derive(serde::Deserialize)]
struct ListedBinary {
    #[serde(rename = "binary-path")]
    path: Utf8PathBuf,
}

/// The test binaries `cargo nextest archive` takes for `packages` (and `tests`).
fn test_binaries(
    root: &Utf8Path,
    packages: &[String],
    tests: &[String],
) -> Result<Vec<Utf8PathBuf>> {
    let mut listing = Command::new("cargo");
    listing.args(["nextest", "list", "--list-type", "binaries-only", "--message-format", "json"]);
    for package in packages {
        listing.args(["-p", package]);
    }
    for test in tests {
        listing.args(["--test", test]);
    }
    if packages.iter().any(|p| p == crate::e2e::LIVE_PACKAGE) {
        listing.args(["--features", crate::e2e::LIVE]);
    }
    let out =
        listing.current_dir(root).stderr(Stdio::inherit()).output().context("nextest list")?;
    ensure!(out.status.success(), "cargo nextest list: {}", out.status);
    let listed: TestList = serde_json::from_slice(&out.stdout).context("nextest's list")?;
    Ok(listed.binaries.into_values().map(|binary| binary.path).collect())
}

/// A dSYM of each of `binaries` under `dir`, at its path below `root`. The debug info of a test
/// build stays in this checkout's object files (`split-debuginfo = "unpacked"`), which the
/// archive does not carry: in the guest a frame would have its name and no file or line.
/// `backtrace` finds a `*.dSYM` beside the executable by its UUID.
fn debug_info(dir: &Utf8Path, root: &Utf8Path, binaries: &[Utf8PathBuf]) -> Result<()> {
    let started = Instant::now();
    println!("▶ dsymutil {} test binaries", binaries.len());
    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("remove {dir}"))?;
    }
    let children = binaries
        .iter()
        .map(|binary| {
            let below =
                binary.strip_prefix(root).with_context(|| format!("{binary} is outside {root}"))?;
            let dsym = dir.join(format!("{below}.dSYM"));
            std::fs::create_dir_all(dsym.parent().context("a dSYM with no directory")?)?;
            let child = Command::new("dsymutil")
                .args([binary.as_str(), "-o", dsym.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .context("dsymutil")?;
            Ok((binary, child))
        })
        .collect::<Result<Vec<_>>>()?;
    for (binary, mut child) in children {
        let status = child.wait().context("dsymutil")?;
        ensure!(status.success(), "dsymutil {binary}: {status}");
    }
    println!("  ✓ dsymutil in {:.1?}", started.elapsed());
    Ok(())
}

/// Run `archive` in the session's guest with `args`, and bring nextest's reports and `target/e2e`
/// back under the session's directory.
fn run_archive(
    home: &Utf8Path,
    session: &Session,
    archive: &Utf8Path,
    packages: &[String],
    args: &[String],
) -> Result<()> {
    let root = repo_root()?;
    let ip = &session.ip;
    // At this checkout's own path, so every path a test was compiled with holds in the guest.
    ssh_out(
        home,
        ip,
        &format!(
            "sudo mkdir -p '{root}' '{GUEST_STAGE}' && sudo chown {USER} '{root}' '{GUEST_STAGE}'"
        ),
    )?;
    let nextest = which("cargo-nextest")?;
    copy_to(home, ip, &[archive.as_str(), nextest.as_str()], GUEST_STAGE)?;
    // The crates' own directories (a test runs in its crate's, and some read files there) and
    // nextest's config.
    let dirs: Vec<Utf8PathBuf> = workspace_packages()?
        .into_iter()
        .filter(|p| packages.contains(&p.name))
        .map(|p| p.dir)
        .collect();
    ensure!(dirs.len() == packages.len(), "a crate in {packages:?} is not in the workspace");
    let mut sources = vec![root.join(".config"), root.join("Cargo.toml")];
    sources.extend(dirs);
    sync_up(home, ip, &root, &sources)?;
    // Each binary's dSYM beside where the archive puts it.
    let status = Command::new("rsync")
        .args(["-a", "-e", &ssh_for_copy(home)?])
        .arg(format!("{}/", debug_dir(archive)))
        .arg(format!("{ip}:{root}/"))
        .stdin(Stdio::null())
        .status()
        .context("rsync")?;
    ensure!(status.success(), "rsync the dSYMs to the guest: {status}");

    let archive_name = archive.file_name().context("the archive's name")?;
    // insta finds the workspace with `cargo metadata`, and the guest has no cargo.
    let insta_root = format!("INSTA_WORKSPACE_ROOT={root}");
    let mut command: Vec<String> = [
        "/usr/bin/env",
        &insta_root,
        "SLOPTY_INPUT_E2E=1",
        "SLOPTY_SCREEN_E2E=1",
        "SLOPTY_DND_E2E=1",
        "SLOPTY_VM=1",
        &format!("{GUEST_STAGE}/cargo-nextest"),
        "nextest",
        "run",
        "--archive-file",
        &format!("{GUEST_STAGE}/{archive_name}"),
        "--extract-to",
        root.as_str(),
        "--extract-overwrite",
        "--workspace-remap",
        root.as_str(),
        "--run-ignored",
        "all",
        "--test-threads",
        "1",
        "--success-output",
        "immediate",
        "--failure-output",
        "immediate",
    ]
    .map(str::to_owned)
    .to_vec();
    command.extend(args.iter().cloned());
    println!("▶ in {}: cargo nextest run {}", session.guest.name, args.join(" "));
    let log = session.dir.join("nextest.log");
    let status = run_teed(exec(home, &session.guest.name, &command), &log)?;

    let back = session.dir.join("target");
    let pulled = sync_down(home, ip, &root, &back);
    println!("  results and artifacts under {back}, the run's output in {log}");
    ensure!(status.success(), "the tests failed in the guest ({status})");
    pulled
}

/// Run `command` with its output, stdout and stderr interleaved by line, on this terminal and in
/// `log`; how it ended. Bytes that are not UTF-8 are shown replaced, never lost with the rest.
fn run_teed(mut command: Command, log: &Utf8Path) -> Result<std::process::ExitStatus> {
    fn forward(from: impl std::io::Read, to: &std::sync::mpsc::Sender<String>) {
        let mut from = BufReader::new(from);
        let mut line = Vec::new();
        loop {
            line.clear();
            match from.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line).trim_end_matches('\n').to_owned();
                    if to.send(text).is_err() {
                        break;
                    }
                }
            }
        }
    }

    let mut file = File::create(log).with_context(|| format!("create {log}"))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn")?;
    let (tx, lines) = std::sync::mpsc::channel();
    let stderr = child.stderr.take().context("stderr")?;
    let err_tx = tx.clone();
    let err = std::thread::spawn(move || forward(stderr, &err_tx));
    let stdout = child.stdout.take().context("stdout")?;
    let out = std::thread::spawn(move || forward(stdout, &tx));
    for line in lines {
        println!("{line}");
        writeln!(file, "{line}")?;
    }
    let joined = (out.join(), err.join());
    ensure!(joined.0.is_ok() && joined.1.is_ok(), "a reader thread panicked");
    Ok(child.wait()?)
}

/// `name`'s full path on `PATH`.
fn which(name: &str) -> Result<Utf8PathBuf> {
    let out = Command::new("which").arg(name).output().context("which")?;
    ensure!(out.status.success(), "{name} is not on PATH");
    Ok(Utf8PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// The `-e` that makes `rsync` go the way [`ssh`] does (no argument holds a space).
fn ssh_for_copy(home: &Utf8Path) -> Result<String> {
    let args = ssh_args(home);
    ensure!(!args.iter().any(|a| a.contains(' ')), "{home} holds a space, which rsync -e splits");
    Ok(format!("ssh {}", args.join(" ")))
}

/// Copy `files` into `dir` on the guest.
fn copy_to(home: &Utf8Path, ip: &str, files: &[&str], dir: &str) -> Result<()> {
    let status = Command::new("rsync")
        .args(["-a", "--inplace", "-e", &ssh_for_copy(home)?])
        .args(files)
        .arg(format!("{ip}:{dir}/"))
        .stdin(Stdio::null())
        .status()
        .context("rsync")?;
    ensure!(status.success(), "rsync to the guest: {status}");
    Ok(())
}

/// `sources` (under `root`) to the same paths on the guest, without their build output.
fn sync_up(home: &Utf8Path, ip: &str, root: &Utf8Path, sources: &[Utf8PathBuf]) -> Result<()> {
    let relative: Vec<&Utf8Path> =
        sources.iter().filter_map(|s| s.strip_prefix(root).ok()).collect();
    let status = Command::new("rsync")
        .args(["-a", "--relative", "--delete", "--exclude", "target/", "-e", &ssh_for_copy(home)?])
        .args(relative.iter().map(|r| format!("./{r}")))
        .arg(format!("{ip}:{root}/"))
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .context("rsync")?;
    ensure!(status.success(), "rsync the sources to the guest: {status}");
    Ok(())
}

/// The guest's `target/nextest` (the reports) and `target/e2e` into `back`.
fn sync_down(home: &Utf8Path, ip: &str, root: &Utf8Path, back: &Utf8Path) -> Result<()> {
    std::fs::create_dir_all(back)?;
    for dir in ["nextest", "e2e"] {
        // macOS's openrsync has no `--ignore-missing-args`.
        let there = ssh(home, ip)
            .arg(format!("test -d '{root}/target/{dir}'"))
            .stdin(Stdio::null())
            .status()?
            .success();
        if !there {
            continue;
        }
        let status = Command::new("rsync")
            .args(["-a", "-e", &ssh_for_copy(home)?])
            .arg(format!("{ip}:{root}/target/{dir}"))
            .arg(format!("{back}/"))
            .stdin(Stdio::null())
            .status()
            .context("rsync")?;
        ensure!(status.success(), "rsync target/{dir} back: {status}");
    }
    Ok(())
}

fn list(home: &Utf8Path) -> Result<()> {
    for vm in listed(home)? {
        println!("{:<44} {:<8} {:>4} GB  {}", vm.name, vm.state, vm.size, vm.source);
    }
    let stat = rustix::fs::statvfs(home.as_std_path())?;
    println!("{home}: {} GB free", stat.f_bavail.saturating_mul(stat.f_frsize) / 1_000_000_000);
    Ok(())
}

/// Remove the run guests whose process is gone; with `kept`, the kept guests too; with `all`,
/// every guest, every image and the key, unless a run's process still lives.
fn prune(home: &Utf8Path, kept: bool, all: bool) -> Result<()> {
    reap_orphans(home)?;
    let guests = listed(home)?;
    if all {
        let live: Vec<&str> = guests
            .iter()
            .map(|vm| vm.name.as_str())
            .filter(|name| run_pid(name).is_some_and(alive))
            .collect();
        ensure!(live.is_empty(), "runs are in progress in {live:?}; `--all` waits for them to end");
    }
    for vm in guests.iter().filter(|vm| vm.source == "local") {
        if all || (kept && vm.name.contains("-kept-")) {
            remove(home, &vm.name)?;
            println!("  removed {}", vm.name);
        }
    }
    if all {
        std::fs::remove_dir_all(home.join("tart")).context("remove the images")?;
        std::fs::remove_dir_all(home.join("ssh")).context("remove the key")?;
        println!("  removed every image under {home}/tart, and the key");
    }
    list(home)
}

#[cfg(test)]
mod tests {
    use clap::FromArgMatches as _;

    use super::*;

    fn parse(args: &[&str]) -> VmOpts {
        let command = VmOpts::augment_args(clap::Command::new("vm"));
        let matches = command.try_get_matches_from(args).expect("parses");
        VmOpts::from_arg_matches(&matches).expect("into VmOpts")
    }

    /// `slopty worker deploy vm --ssh <xtask>` runs `xtask vm <script>`: the script is taken as
    /// the stand-in's, however it reads, and no subcommand runs.
    #[test]
    fn the_deploy_cli_s_script_is_the_stand_in_ssh_s() {
        let opts = parse(&["vm", "sh -c 'uname -sm'"]);
        assert!(opts.cmd.is_none(), "{opts:?}");
        assert_eq!(opts.script.as_deref(), Some("sh -c 'uname -sm'"));
        let opts = parse(&["vm", "list"]);
        assert!(matches!(opts.cmd, Some(VmCmd::List)), "a subcommand is one: {opts:?}");
    }

    /// `live` keeps its crates and targets for the archive and hands the rest to nextest.
    #[test]
    fn live_splits_the_archive_s_selection_from_nextest_s_filters() {
        let opts = parse(&[
            "vm",
            "live",
            "--macos",
            "27",
            "-p",
            "slopty-input",
            "--test",
            "inject",
            "--",
            "-E",
            "test(moves)",
        ]);
        let Some(VmCmd::Live(live)) = opts.cmd else { panic!("live: {opts:?}") };
        assert_eq!(live.macos, Macos::V27);
        assert_eq!(live.packages, ["slopty-input"]);
        assert_eq!(live.tests, ["inject"]);
        assert_eq!(live.args, ["-E", "test(moves)"]);
        assert!(!live.keep, "a run's guest goes by default");
    }

    /// Every volume of the startup disk's container is refused, the data volume included though
    /// its device number differs from the system's.
    #[test]
    fn a_home_on_the_startup_disk_is_refused() {
        let user_home = std::env::var("HOME").expect("HOME");
        for path in [user_home.as_str(), "/", "/private/tmp"] {
            let refused = off_the_startup_disk(Path::new(path)).expect_err(path);
            assert!(refused.to_string().contains("startup disk"), "{path}: {refused}");
        }
    }

    /// A device's whole disk is its container's number, however deep its slices go.
    #[test]
    fn a_device_s_whole_disk_is_its_container_s() {
        assert_eq!(whole_disk("/dev/disk3s1s1").as_deref(), Some("disk3"));
        assert_eq!(whole_disk("/dev/disk3s5").as_deref(), Some("disk3"));
        assert_eq!(whole_disk("/dev/disk12s1").as_deref(), Some("disk12"));
        assert_eq!(whole_disk("/dev/disk7").as_deref(), Some("disk7"));
        assert_eq!(whole_disk("map auto_home"), None);
        assert_eq!(whole_disk("//me@nas/share"), None);
        assert_eq!(whole_disk("/dev/diskette"), None);
    }

    /// An image is taken by digest, so a tag moving upstream never changes a guest here.
    #[test]
    fn every_image_is_pinned_by_digest() {
        for macos in [Macos::V26, Macos::V27] {
            let (_repo, digest) = macos.image().split_once("@sha256:").expect("a digest");
            assert_eq!(digest.len(), 64, "{}", macos.image());
            assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{}", macos.image());
            assert!(macos.base().ends_with(macos.major()), "{}", macos.base());
        }
    }

    /// Only a run guest whose process is gone is an orphan: never a kept guest, a base, a dev
    /// guest, or a run whose process lives.
    #[test]
    fn only_a_run_guest_whose_process_is_gone_is_reaped() {
        let names = [
            "slopty-26",
            "slopty-26-dev",
            "slopty-26-new",
            "slopty-26-run-100-1790000000",
            "slopty-27-run-200-1790000001",
            "slopty-26-kept-100-1790000002",
            "slopty-26-run-x-1",
        ];
        let reaped = orphans(names, |pid| pid == 200);
        assert_eq!(reaped, ["slopty-26-run-100-1790000000"]);
        assert_eq!(run_pid("slopty-26-run-4079-1790753744"), Some(4079));
        assert_eq!(run_pid("slopty-26-kept-4079-1790753744"), None);
    }

    /// A zero signal tells a live process from one that has exited and been reaped.
    #[test]
    fn a_process_that_exited_is_not_alive() {
        assert!(alive(std::process::id()), "this process");
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn true");
        let pid = child.id();
        child.wait().expect("reap true");
        assert!(!alive(pid), "{pid} exited and was reaped");
        assert!(!alive(0) && !alive(u32::MAX), "no such pids");
    }

    /// The guest's settings keep every key; this Mac is added to `[worker] allow` once.
    #[test]
    fn admitting_the_host_keeps_the_rest_of_the_settings() {
        let fresh = admitting("", "192.168.64.1").expect("parses").expect("written");
        assert_eq!(fresh.trim(), "[worker]\nallow = [\"192.168.64.1\"]");
        let edited = "theme = \"dark\"\n[worker]\nallow = [\"100.64.0.0/10\"]\nport = 7\n";
        let merged = admitting(edited, "192.168.64.1").expect("parses").expect("written");
        let merged: toml::Table = merged.parse().expect("toml");
        assert_eq!(merged["theme"].as_str(), Some("dark"));
        assert_eq!(merged["worker"]["port"].as_integer(), Some(7));
        let allow = merged["worker"]["allow"].as_array().expect("a list");
        assert_eq!(allow.len(), 2, "{allow:?}");
        let again = toml::to_string(&merged).expect("toml");
        assert!(admitting(&again, "192.168.64.1").expect("parses").is_none(), "already there");
    }

    /// Two runs never hold one MAC, and a lock goes with its holder.
    #[test]
    fn two_runs_take_two_macs_and_a_third_waits() {
        let dir = std::env::temp_dir().join(format!("slopty-vm-macs-{}", std::process::id()));
        let home = Utf8PathBuf::try_from(dir).expect("utf-8");
        std::fs::create_dir_all(home.join("locks")).expect("mkdir");
        let first = claim_mac(&home).expect("a first MAC");
        let second = claim_mac(&home).expect("a second MAC");
        assert_ne!(first.mac, second.mac);
        assert!(claim_mac(&home).is_err(), "a third run has no MAC");
        let freed = first.mac;
        drop(first);
        assert_eq!(claim_mac(&home).expect("the freed MAC").mac, freed);
        drop(second);
        std::fs::remove_dir_all(&home).expect("clean up");
    }
}
