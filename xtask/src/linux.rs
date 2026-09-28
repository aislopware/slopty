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
//!   `[worker] allow`. Ctrl-C stops the container, which removes itself.
//! - `e2e` does both and runs `crates/slopty-e2e/tests/linux.rs` against the worker from this Mac,
//!   then removes the container.
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
    let bins = build(sh)?;
    match cmd {
        LinuxCmd::Build => Ok(()),
        LinuxCmd::Run => {
            let stack = Stack::start(sh, &bins, "infinity")?;
            println!(
                "▶ Linux worker in {} at {}: `slopty ping --worker {}`; Ctrl-C stops it",
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
    }
}

/// Cross-build the worker's binaries; the directory they are in.
fn build(sh: &Shell) -> Result<Utf8PathBuf> {
    for (tool, how) in [
        ("zig", "`brew install zig` (0.16)"),
        ("cargo-zigbuild", "`cargo binstall cargo-zigbuild`"),
    ] {
        ensure!(has(sh, tool), "{tool} is needed to cross-build for Linux: {how}");
    }
    step(
        &format!("cross-build the Linux worker ({TRIPLE})"),
        &cmd!(
            sh,
            "cargo zigbuild --target {TRIPLE} --target-dir {TARGET_DIR} -p slopty-ptyd -p slopty-workerd -p slopty-cli --bins"
        ),
    )?;
    Ok(sh.current_dir().join(TARGET_DIR).join(TRIPLE).join("debug").try_into()?)
}

fn e2e(sh: &Shell, bins: &Utf8Path) -> Result<()> {
    let stack = Stack::start(sh, bins, "3600")?;
    println!("▶ Linux worker in {} at {}", stack.container.name, stack.addr);
    let _env = [
        sh.push_env("SLOPTY_LINUX_E2E", "1"),
        sh.push_env("SLOPTY_LINUX_WORKER", &stack.addr),
        sh.push_env("SLOPTY_LINUX_CONTAINER", &stack.container.name),
        sh.push_env("SLOPTY_LINUX_USER", USER),
        sh.push_env("SLOPTY_LINUX_BIN_DIR", BIN_DIR),
        sh.push_env("DOCKER_CONTEXT", CONTEXT),
    ];
    // `--no-capture` runs one test at a time: the echo measurement has the worker to itself.
    let tested = step(
        "slopty-e2e linux",
        &cmd!(sh, "cargo nextest run -p slopty-e2e --test linux --no-capture"),
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
        let settings = format!("[worker]\nallow = [\"{gateway}\"]\n");
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
