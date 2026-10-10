//! `xtask linux`: a terminal-only Linux worker, cross-built on this Mac and run in a container.
//!
//! - `build` (the default) cross-links `slopty-ptyd`, `slopty-worker` and the `slopty` CLI for
//!   [`TRIPLE`] under `target/linux`, with `cargo zigbuild`: zig is the C toolchain and the linker
//!   for glibc. It is already the toolchain libghostty-vt builds with, so nothing new is installed,
//!   sccache and the Mac's crate caches serve the build, and nothing compiles on the bind-mounted
//!   external volume, which is slow from inside Docker Desktop's VM.
//! - `run` starts those binaries in a fresh Debian container on Docker Desktop (never another
//!   context), as a user of its own with bash as the login shell: ptyd, then the worker, on the
//!   defaults a Linux install takes (`~/.local/share/slopty`, `/tmp/slopty-<uid>`). The worker's
//!   QUIC port is published on a free loopback port of this Mac. A container is not on the tailnet,
//!   so its settings admit the one address this Mac's packets arrive from, the bridge's gateway, as
//!   `[network] allow`. Ctrl-C stops the container, which removes itself.
//! - `e2e` does both and runs `crates/slopty-e2e/tests/linux.rs` against the worker from this Mac,
//!   then removes the container.
//! - `deploy` proves an install the way a person makes one: it starts a container whose init is
//!   systemd, with lingering on for its user, and runs this Mac's own `slopty worker deploy` and
//!   `slopty server deploy` into it, with this binary standing in for `ssh` (`docker exec` as that
//!   user). The worker and the server run as systemd user units there; the worker answers
//!   `slopty-probe ping` from this Mac, a second deploy updates it in place, and the server answers
//!   `slopty workers`.
//! - `dist` cross-builds what ships, in the `dist` profile ([`build_shipped`]): the worker's three
//!   binaries for both CPUs a Linux box runs ([`SHIPPED`]), linked against glibc [`GLIBC`] so one
//!   build runs on every distribution of the last several years, and the server for both, static on
//!   musl. `cargo xtask bundle` puts the workers inside `Slopty.app` and `cargo xtask dist`
//!   publishes all of them.
//!
//! Every container is named `slopty-linux-<pid>-<time>` and labelled [`LABEL`], capped at
//! [`CPUS`] CPUs so a build machine under load stays usable, and lives at most its `sleep` (an
//! hour for `e2e`) even when nothing is left to remove it.

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Subcommand;
use xshell::{Shell, cmd};

use crate::tools::{has, step};

/// What to do.
#[derive(Subcommand, Debug, Clone, Copy)]
pub enum LinuxCmd {
    /// Cross-build the Linux worker's binaries under `target/linux` (the default).
    Build,
    /// Build, then run ptyd and the worker in a fresh container until Ctrl-C.
    Run,
    /// Build, start the container, and run the Linux end-to-end test against it from this Mac.
    E2e,
    /// Build, then deploy the worker and the server into a container that runs systemd, with
    /// this Mac's `slopty worker deploy` and `slopty server deploy`, and check both answer.
    Deploy,
    /// Cross-build what ships for Linux, in the `dist` profile: the worker for arm64 and `x86_64`
    /// (glibc), and the server for both (static musl).
    Dist,
}

/// `cargo xtask linux` (with one argument and no subcommand, the `ssh` of a deploy into the
/// container `deploy` started).
#[derive(clap::Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct LinuxOpts {
    #[command(subcommand)]
    cmd: Option<LinuxCmd>,
    /// The script a deploy runs on the machine, when this binary is its `ssh`.
    #[arg(hide = true)]
    script: Option<String>,
}

/// The container a deploy's stand-in `ssh` reaches, and its user's uid.
const SSH_CONTAINER_VAR: &str = "SLOPTY_LINUX_SSH_CONTAINER";
const SSH_UID_VAR: &str = "SLOPTY_LINUX_SSH_UID";

/// `cargo xtask linux …`, or the stand-in `ssh` when a deploy runs this binary as one.
pub fn main(sh: &Shell, opts: &LinuxOpts) -> Result<()> {
    match (&opts.script, opts.cmd) {
        (Some(script), _) => stand_in_ssh(script),
        (None, cmd) => run(sh, cmd.unwrap_or(LinuxCmd::Build)),
    }
}

/// Stand in for `ssh`: `script` (what a login shell there would be handed) runs in the
/// container [`SSH_CONTAINER_VAR`] names, as [`USER`] in its home, with the user manager's
/// runtime directory and bus that a login there would have. Standard input passes through, as
/// an upload needs.
fn stand_in_ssh(script: &str) -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let var = |name: &str| {
        std::env::var(name)
            .with_context(|| format!("{name} unset: only `cargo xtask linux deploy` runs this"))
    };
    let (container, uid) = (var(SSH_CONTAINER_VAR)?, var(SSH_UID_VAR)?);
    let runtime = format!("XDG_RUNTIME_DIR=/run/user/{uid}");
    let bus = format!("DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus");
    let home = format!("/home/{USER}");
    let mut exec = docker();
    exec.args(["exec", "--interactive", "--user", USER, "--workdir", &home]);
    exec.args(["--env", &runtime, "--env", &bus, "--env", &format!("USER={USER}")]);
    exec.args([container.as_str(), "sh", "-c", script]);
    Err(exec.exec()).context("exec docker")
}

/// An image whose init is systemd, with a user manager and logind: [`IMAGE`] plus systemd.
const SYSTEMD_IMAGE: &str = "slopty-linux-systemd:trixie";
/// How it is made, built once and then cached by Docker.
const SYSTEMD_DOCKERFILE: &str = "FROM debian:trixie-slim\n\
    RUN apt-get update && apt-get install -y --no-install-recommends systemd systemd-sysv \
    dbus dbus-user-session libpam-systemd && rm -rf /var/lib/apt/lists/*\n\
    STOPSIGNAL SIGRTMIN+3\n\
    CMD [\"/lib/systemd/systemd\"]\n";
/// The ports the worker and the server listen on there, their defaults.
const WORKER_PORT: u16 = 45550;
const SERVER_PORT: u16 = 45560;
/// How long the container's systemd may take to start the user's manager.
const MANAGER_READY: Duration = Duration::from_secs(60);

/// Deploy the server built into `bins` into a fresh systemd container with this Mac's CLI, then
/// the worker registered with it, and check each answers from here and the server lists the
/// worker.
fn deploy_e2e(sh: &Shell, bins: &Utf8Path) -> Result<()> {
    step("cargo build -p slopty-cli", &cmd!(sh, "cargo build -p slopty-cli --bins"))?;
    let cli = Utf8PathBuf::try_from(sh.current_dir().join("target/debug/slopty"))?;
    let probe = Utf8PathBuf::try_from(sh.current_dir().join("target/debug/slopty-probe"))?;
    step(
        "the systemd image",
        &cmd!(sh, "docker --context {CONTEXT} build --quiet --tag {SYSTEMD_IMAGE} -")
            .stdin(SYSTEMD_DOCKERFILE),
    )?;
    let c = Container::systemd()?;
    println!("▶ systemd container {}", c.name);
    let started = Instant::now();
    // Each look is a `docker exec`, a tenth of a second apart on its own.
    while c.root(&["test", "-S", "/run/dbus/system_bus_socket"]).is_err() {
        ensure!(started.elapsed() < MANAGER_READY, "the container's system bus did not start");
    }
    // Degraded (a unit a container cannot run) is booted all the same.
    let booted = c.root(&["systemctl", "is-system-running", "--wait"]);
    println!("  booted: {}", booted.unwrap_or_else(|e| e.to_string()));
    c.root(&["useradd", "--create-home", "--shell", "/bin/bash", USER])?;
    let uid = c.root(&["id", "-u", USER])?;
    c.root(&["loginctl", "enable-linger", USER])?;
    let bus = format!("/run/user/{uid}/bus");
    while c.root(&["test", "-S", &bus]).is_err() {
        ensure!(started.elapsed() < MANAGER_READY, "{USER}'s systemd manager did not start");
    }
    let gateway = docker_out(&[
        "inspect",
        "--format",
        "{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}",
        &c.name,
    ])?;
    let data = format!("/home/{USER}/.local/share/slopty");
    let mkdir = c.exec(&[], &["mkdir", "-p", &data]).stdin(Stdio::null()).status()?;
    ensure!(mkdir.success(), "mkdir {data}: {mkdir}");
    // This Mac's packets reach the container from its bridge's gateway, and only from it.
    let settings = format!("[network]\nallow = [\"{gateway}\"]\n");
    write_in(&c, &format!("{data}/settings.toml"), &settings)?;

    let xtask = std::env::current_exe().context("this binary")?;
    let here = tempfile_dir(sh)?;
    let run = |program: &Utf8Path, args: &[&str]| -> Result<String> {
        let mut command = Command::new(program);
        command.arg("--data-dir").arg(&here).args(args);
        command.env(SSH_CONTAINER_VAR, &c.name).env(SSH_UID_VAR, &uid).stdin(Stdio::null());
        let out = command.output().context("run slopty")?;
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        ensure!(out.status.success(), "{program} {}: {}\n{said}", args.join(" "), out.status);
        Ok(said)
    };
    let slopty = |args: &[&str]| run(&cli, args);
    let ping = |args: &[&str]| run(&probe, &[["ping"].as_slice(), args].concat());
    let user_unit = |unit: &str| -> Result<String> {
        let runtime = format!("XDG_RUNTIME_DIR=/run/user/{uid}");
        let out = c.exec(&[&runtime], &["systemctl", "--user", "is-active", unit]).output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let ssh = xtask.to_string_lossy().into_owned();
    // The server first: a worker registers with one, and this one runs beside it.
    let serve = ["server", "deploy", "linux", "--bin-dir", bins.as_str(), "--ssh", &ssh];
    println!("{}", slopty(&serve)?);
    ensure!(user_unit("slopty-server")? == "active", "slopty-server is not an active user unit");
    let server = published(&c, SERVER_PORT)?;
    // The worker reaches the server beside it at the container's own address, which the server
    // then lists it at.
    let own = docker_out(&[
        "inspect",
        "--format",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
        &c.name,
    ])?;
    let beside = format!("{own}:{SERVER_PORT}");
    let deploy = ["--server", &beside, "worker", "deploy", "linux", "--bin-dir", bins.as_str()];
    let deploy = [deploy.as_slice(), &["--ssh", &ssh]].concat();
    let said = slopty(&deploy)?;
    println!("{said}");
    for unit in ["slopty-ptyd", "slopty-worker"] {
        ensure!(user_unit(unit)? == "active", "{unit} is not an active user unit");
    }
    let worker = published(&c, WORKER_PORT)?;
    ping(&["--worker", &worker, "--count", "3"])?;
    println!("  ✓ the worker runs as systemd user units and answers at {worker}");

    slopty(&[deploy.as_slice(), &["--update"]].concat())?;
    ping(&["--worker", &worker, "--count", "1"])?;
    println!("  ✓ a second deploy updates it in place");

    let listed = slopty(&["--server", &server, "--json", "workers"])?;
    ensure!(listed.contains(&own), "the server does not list the worker at {own}: {listed}");
    println!("  ✓ the server runs as a systemd user unit and answers at {server}");
    Ok(())
}

/// Where this Mac reaches `port`/udp of `c`.
fn published(c: &Container, port: u16) -> Result<String> {
    let published = docker_out(&["port", &c.name, &format!("{port}/udp")])?;
    Ok(published.lines().next().context("no published port")?.to_owned())
}

/// A fresh data directory for this Mac's side of a run, under `target/`.
fn tempfile_dir(sh: &Shell) -> Result<Utf8PathBuf> {
    let dir = sh.current_dir().join("target/linux/deploy-client");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    Ok(Utf8PathBuf::try_from(dir)?)
}

/// The Linux CPUs a worker ships for: each one's glibc triple, its server's musl triple, and the
/// name of its directory in the app bundle (`Contents/Resources/workers/<name>`) and a release.
pub const SHIPPED: [Shipped; 2] = [
    Shipped {
        worker: "aarch64-unknown-linux-gnu",
        server: "aarch64-unknown-linux-musl",
        name: "linux-arm64",
    },
    Shipped {
        worker: "x86_64-unknown-linux-gnu",
        server: "x86_64-unknown-linux-musl",
        name: "linux-x86_64",
    },
];

/// The oldest glibc a shipped worker links against: Debian 10, RHEL 8, Ubuntu 20.04 and later
/// run it. glibc, not musl, because a Linux desktop's libraries (`PipeWire`, VA-API) are glibc's
/// and its allocator is the faster on the terminal path; zig provides the old symbol versions.
pub const GLIBC: &str = "2.28";

/// One Linux CPU that ships.
#[derive(Clone, Copy, Debug)]
pub struct Shipped {
    /// The worker's triple (glibc).
    pub worker: &'static str,
    /// The server's triple (static musl).
    pub server: &'static str,
    /// Its directory's name in the bundle and a release.
    pub name: &'static str,
}

/// Where a shipped build put one CPU's binaries.
#[derive(Clone, Debug)]
pub struct Built {
    /// Which CPU.
    pub shipped: Shipped,
    /// The directory holding its `slopty-ptyd`, `slopty-worker` and `slopty`.
    pub workers: Utf8PathBuf,
    /// The directory holding its `slopty-server`.
    pub server: Utf8PathBuf,
}

/// The binaries a Linux worker is, as `slopty_platform::service::WORKER_BINARIES` names them.
pub const WORKER_BINARIES: [&str; 3] = ["slopty-ptyd", "slopty-worker", "slopty"];

/// Cross-build every [`SHIPPED`] CPU's worker and server in `profile`, under [`TARGET_DIR`].
///
/// # Errors
///
/// When zig or cargo-zigbuild is missing, or a build fails.
pub fn build_shipped(sh: &Shell, profile: &str) -> Result<Vec<Built>> {
    need_zig(sh)?;
    let workers: Vec<String> = SHIPPED.iter().map(|s| format!("{}.{GLIBC}", s.worker)).collect();
    let workers = workers.iter().flat_map(|t| ["--target", t.as_str()]);
    let workers: Vec<&str> = workers.collect();
    step(
        &format!("cross-build the Linux workers (glibc {GLIBC}, {profile})"),
        &cmd!(
            sh,
            "cargo zigbuild {workers...} --profile {profile} --target-dir {TARGET_DIR} -p slopty-ptyd -p slopty-workerd -p slopty-cli --bins"
        ),
    )?;
    let servers = SHIPPED.iter().flat_map(|s| ["--target", s.server]);
    let servers: Vec<&str> = servers.collect();
    step(
        &format!("cross-build the Linux servers (static musl, {profile})"),
        &cmd!(
            sh,
            "cargo zigbuild {servers...} --profile {profile} --target-dir {TARGET_DIR} -p slopty-serverd --bins"
        ),
    )?;
    let root: Utf8PathBuf = sh.current_dir().join(TARGET_DIR).try_into()?;
    // Cargo's `dev` profile builds into `debug`.
    let profile = if profile == "dev" { "debug" } else { profile };
    SHIPPED
        .iter()
        .map(|&shipped| {
            let built = Built {
                shipped,
                workers: root.join(shipped.worker).join(profile),
                server: root.join(shipped.server).join(profile),
            };
            for bin in WORKER_BINARIES {
                ensure!(built.workers.join(bin).is_file(), "missing {}", built.workers.join(bin));
            }
            let server = built.server.join("slopty-server");
            ensure!(server.is_file(), "missing {server}");
            Ok(built)
        })
        .collect()
}

/// The oldest distribution a shipped worker claims: Debian 10, glibc [`GLIBC`].
const OLDEST: &str = "debian:buster-slim";
/// Where the static server runs with no glibc at all.
const STATIC: &str = "alpine:3";

/// Run each shipped binary's `--version` on the oldest distribution it claims, for its CPU
/// (Docker Desktop runs `x86_64` under emulation): the workers on [`OLDEST`], the servers on
/// [`STATIC`]. Skipped, and said, when Docker Desktop does not answer.
fn smoke(built: &[Built]) -> Result<()> {
    if docker_out(&["info", "--format", "{{.ServerVersion}}"]).is_err() {
        println!("  ! Docker Desktop is not running: the binaries were not run on Linux");
        return Ok(());
    }
    for build in built {
        let platform = match build.shipped.name {
            "linux-arm64" => "linux/arm64",
            _ => "linux/amd64",
        };
        let run = |dir: &Utf8Path, image: &str, bin: &str| -> Result<String> {
            let mount = format!("{dir}:/opt/slopty:ro");
            let exe = format!("/opt/slopty/{bin}");
            let args = ["run", "--rm", "--platform", platform, "--volume", &mount, image, &exe];
            docker_out(&[args.as_slice(), &["--version"]].concat())
        };
        for bin in WORKER_BINARIES {
            let said = run(&build.workers, OLDEST, bin)?;
            ensure!(said.starts_with(bin), "{bin} on {OLDEST} ({platform}) said {said:?}");
        }
        let said = run(&build.server, STATIC, "slopty-server")?;
        ensure!(said.starts_with("slopty-server"), "the server on {STATIC} said {said:?}");
        println!(
            "  ✓ {} runs on glibc {GLIBC} ({OLDEST}) and the server on {STATIC}",
            build.shipped.name
        );
    }
    Ok(())
}

/// zig and cargo-zigbuild, which every Linux build here goes through.
fn need_zig(sh: &Shell) -> Result<()> {
    if let Some(problem) = crate::tools::zig_problem(sh) {
        bail!("{problem}");
    }
    ensure!(
        has(sh, "cargo-zigbuild"),
        "cargo-zigbuild is needed to cross-build for Linux: `cargo binstall cargo-zigbuild`"
    );
    Ok(())
}

/// The Docker context: Docker Desktop's VM on this Mac, never a remote one.
const CONTEXT: &str = "desktop-linux";
/// Docker Desktop's VM on Apple silicon.
const TRIPLE: &str = "aarch64-unknown-linux-gnu";
/// Where cargo builds for [`TRIPLE`], apart from the Mac's own target dirs.
const TARGET_DIR: &str = "target/linux";
/// A distribution a Linux worker runs on, slim: bash, coreutils and `useradd`, nothing else.
const IMAGE: &str = "debian:trixie-slim";
/// Where the binaries are mounted, read-only, in the container.
pub const BIN_DIR: &str = "/opt/slopty";
/// The account the daemons run as.
const USER: &str = "slopty";
/// The worker's port inside the container; the container has its own network namespace.
const PORT: u16 = 45570;
/// Every container this command starts carries it.
const LABEL: &str = "dev.slopty.linux";
/// CPUs the container may use.
const CPUS: &str = "2";
/// How long ptyd may take to bind its socket.
const PTYD_READY: Duration = Duration::from_secs(20);

pub fn run(sh: &Shell, cmd: LinuxCmd) -> Result<()> {
    if matches!(cmd, LinuxCmd::Dist) {
        let built = build_shipped(sh, "dist")?;
        for built in &built {
            println!("✔ {}: {} and {}", built.shipped.name, built.workers, built.server);
        }
        return smoke(&built);
    }
    let bins = build(sh)?;
    match cmd {
        LinuxCmd::Build | LinuxCmd::Dist => Ok(()),
        LinuxCmd::Run => {
            let stack = Stack::start(sh, &bins, "infinity")?;
            println!(
                "▶ Linux worker in {} at {}: `cargo xtask probe ping --worker {}`; Ctrl-C stops it",
                stack.container.name, stack.addr, stack.addr
            );
            // Attached, `docker` hands Ctrl-C to the container's init, which ends `sleep`; the
            // container stops and removes itself.
            let attached = docker().args(["attach", &stack.container.name]).status();
            drop(stack);
            attached.context("docker attach")?;
            Ok(())
        }
        LinuxCmd::E2e => e2e(sh, &bins),
        LinuxCmd::Deploy => deploy_e2e(sh, &bins),
    }
}

/// Cross-build the worker's binaries; the directory they are in.
fn build(sh: &Shell) -> Result<Utf8PathBuf> {
    need_zig(sh)?;
    step(
        &format!("cross-build the Linux worker ({TRIPLE})"),
        &cmd!(
            sh,
            "cargo zigbuild --target {TRIPLE} --target-dir {TARGET_DIR} -p slopty-ptyd -p slopty-workerd -p slopty-cli -p slopty-serverd --bins"
        ),
    )?;
    Ok(sh.current_dir().join(TARGET_DIR).join(TRIPLE).join("debug").try_into()?)
}

fn e2e(sh: &Shell, bins: &Utf8Path) -> Result<()> {
    let stack = Stack::start(sh, bins, "3600")?;
    println!("▶ Linux worker in {} at {}", stack.container.name, stack.addr);
    let _env = [
        sh.push_env("SLOPTY_LINUX_WORKER", &stack.addr),
        sh.push_env("SLOPTY_LINUX_CONTAINER", &stack.container.name),
        sh.push_env("SLOPTY_LINUX_USER", USER),
        sh.push_env("SLOPTY_LINUX_BIN_DIR", BIN_DIR),
        sh.push_env("DOCKER_CONTEXT", CONTEXT),
        // These tests spawn nothing on this Mac: nextest's setup script would otherwise build
        // every binary of the workspace with its tests first.
        sh.push_env(crate::gate::BINS_FRESH, "1"),
    ];
    // `--no-capture` runs one test at a time: the echo measurement has the worker to itself.
    let live = crate::e2e::LIVE;
    let tested = step(
        "slopty-e2e linux",
        &cmd!(
            sh,
            "cargo nextest run -p slopty-e2e --test linux --features {live} --run-ignored only --no-capture"
        ),
    );
    let logs = stack.logs.clone();
    drop(stack);
    println!("  daemon logs under {logs}");
    tested
}

/// `docker` on [`CONTEXT`].
fn docker() -> Command {
    let mut docker = Command::new("docker");
    docker.env("DOCKER_CONTEXT", CONTEXT);
    docker
}

/// Run `docker args…` to the end; its standard output, trimmed.
fn docker_out(args: &[&str]) -> Result<String> {
    let out = docker().args(args).stdin(Stdio::null()).output().context("run docker")?;
    ensure!(
        out.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// A container of ours, removed with everything in it when dropped.
struct Container {
    name: String,
}

impl Container {
    /// A fresh [`IMAGE`] with `bins` mounted at [`BIN_DIR`], doing nothing but `sleep lifetime`
    /// under an init that passes signals on.
    fn start(bins: &Utf8Path, lifetime: &str) -> Result<Self> {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let name = format!("slopty-linux-{}-{started}", std::process::id());
        let mount = format!("{bins}:{BIN_DIR}:ro");
        let publish = format!("127.0.0.1::{PORT}/udp");
        let label = format!("{LABEL}={}", std::process::id());
        docker_out(&[
            "run",
            "--detach",
            "--rm",
            "--init",
            "--name",
            &name,
            "--label",
            &label,
            "--cpus",
            CPUS,
            "--memory",
            "2g",
            "--publish",
            &publish,
            "--volume",
            &mount,
            IMAGE,
            "sleep",
            lifetime,
        ])?;
        Ok(Self { name })
    }

    /// A fresh [`SYSTEMD_IMAGE`] booted with systemd as its init, its worker's and server's
    /// ports published on this Mac's loopback. systemd needs the cgroup tree and its own
    /// `/run`; `--privileged` is the Docker Desktop VM's, not this Mac's.
    fn systemd() -> Result<Self> {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let name = format!("slopty-linux-{}-{started}", std::process::id());
        let label = format!("{LABEL}={}", std::process::id());
        let worker = format!("127.0.0.1::{WORKER_PORT}/udp");
        let server = format!("127.0.0.1::{SERVER_PORT}/udp");
        docker_out(&[
            "run",
            "--detach",
            "--rm",
            "--privileged",
            "--cgroupns=host",
            "--volume",
            "/sys/fs/cgroup:/sys/fs/cgroup:rw",
            "--tmpfs",
            "/run",
            "--tmpfs",
            "/run/lock",
            "--name",
            &name,
            "--label",
            &label,
            "--cpus",
            CPUS,
            "--memory",
            "2g",
            "--publish",
            &worker,
            "--publish",
            &server,
            SYSTEMD_IMAGE,
        ])?;
        Ok(Self { name })
    }

    /// `docker exec` as [`USER`] with `env` (`NAME=value`), for the caller to finish.
    fn exec(&self, env: &[&str], args: &[&str]) -> Command {
        let mut exec = docker();
        exec.args(["exec", "--user", USER]);
        for pair in env {
            exec.args(["--env", pair]);
        }
        exec.arg(&self.name).args(args);
        exec
    }

    /// Run `args` as root to the end.
    fn root(&self, args: &[&str]) -> Result<String> {
        docker_out(&[&["exec", self.name.as_str()], args].concat())
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _removed = docker()
            .args(["rm", "--force", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The container with ptyd and the worker running in it.
struct Stack {
    /// The `docker exec`s the daemons run under, stopped before the container goes.
    daemons: Vec<Child>,
    /// Where this Mac dials the worker (`127.0.0.1:<port>`).
    addr: String,
    /// The daemons' logs.
    logs: Utf8PathBuf,
    // Last, so the daemons' `docker exec`s are gone before it is removed.
    container: Container,
}

impl Stack {
    fn start(sh: &Shell, bins: &Utf8Path, lifetime: &str) -> Result<Self> {
        let container = Container::start(bins, lifetime)?;
        let logs = sh.current_dir().join("target/logs/linux").join(&container.name);
        let logs = Utf8PathBuf::try_from(logs)?;
        std::fs::create_dir_all(&logs).with_context(|| format!("mkdir {logs}"))?;
        let mut stack = Self { daemons: Vec::new(), addr: String::new(), logs, container };
        stack.addr = stack.bring_up()?;
        Ok(stack)
    }

    /// Start the daemons; where this Mac dials the worker.
    fn bring_up(&mut self) -> Result<String> {
        let c = &self.container;
        c.root(&["useradd", "--create-home", "--shell", "/bin/bash", USER])?;
        let uid = c.root(&["id", "-u", USER])?;
        let gateway = docker_out(&[
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}",
            &c.name,
        ])?;
        ensure!(!gateway.is_empty(), "the container has no network gateway");
        let data = format!("/home/{USER}/.local/share/slopty");
        let mkdir = c.exec(&[], &["mkdir", "-p", &data]).stdin(Stdio::null()).status()?;
        ensure!(mkdir.success(), "mkdir {data}: {mkdir}");
        // This Mac's packets reach the container from its bridge's gateway, and only from it.
        let settings = format!("[network]\nallow = [\"{gateway}\"]\n");
        write_in(c, &format!("{data}/settings.toml"), &settings)?;

        let log = ["RUST_LOG=info"];
        let ptyd = c.exec(&log, &[&format!("{BIN_DIR}/slopty-ptyd")]);
        self.daemons.push(spawn_logged(ptyd, &self.logs.join("ptyd.log"), Stdio::null())?);
        // The platform's socket directory with no `XDG_RUNTIME_DIR`, as a container has none.
        let socket = format!("/tmp/slopty-{uid}/ptyd.sock");
        let started = Instant::now();
        // Each look is a `docker exec`, a tenth of a second apart on its own.
        while !c.exec(&[], &["test", "-S", &socket]).stdin(Stdio::null()).status()?.success() {
            if let Some(ptyd) = self.daemons.first_mut()
                && let Some(status) = ptyd.try_wait()?
            {
                bail!("ptyd exited with {status}; see {}", self.logs.join("ptyd.log"));
            }
            ensure!(started.elapsed() < PTYD_READY, "ptyd did not bind {socket}");
        }

        let port = PORT.to_string();
        let worker =
            c.exec(&log, &[&format!("{BIN_DIR}/slopty-worker"), "--print-addr", "--port", &port]);
        let mut worker = spawn_logged(worker, &self.logs.join("worker.log"), Stdio::piped())?;
        let stdout = worker.stdout.take().context("the worker's stdout")?;
        self.daemons.push(worker);
        let mut listening = String::new();
        BufReader::new(stdout).read_line(&mut listening)?;
        if listening.trim().is_empty() {
            bail!("the worker exited before it listened; see {}", self.logs.join("worker.log"));
        }
        let published = docker_out(&["port", &c.name, &format!("{PORT}/udp")])?;
        Ok(published.lines().next().context("no published port")?.to_owned())
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for daemon in &mut self.daemons {
            let _killed = daemon.kill();
            let _reaped = daemon.wait();
        }
    }
}

/// Write `text` to `path` in the container, as [`USER`].
fn write_in(c: &Container, path: &str, text: &str) -> Result<()> {
    use std::io::Write as _;

    let mut tee = docker()
        .args(["exec", "--interactive", "--user", USER, &c.name, "tee", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("docker exec tee")?;
    tee.stdin.take().context("tee's stdin")?.write_all(text.as_bytes())?;
    let status = tee.wait()?;
    ensure!(status.success(), "write {path}: {status}");
    Ok(())
}

/// Start `command` with its standard error into `log`.
fn spawn_logged(mut command: Command, log: &Utf8Path, stdout: Stdio) -> Result<Child> {
    let file = std::fs::File::create(log).with_context(|| format!("create {log}"))?;
    command.stdin(Stdio::null()).stdout(stdout).stderr(file).spawn().context("docker exec a daemon")
}
