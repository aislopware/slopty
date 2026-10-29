//! `xtask warm linux`: the warm Linux runner, the actions runner in an arm64 Ubuntu VM under
//! tart, headless at `Nice 10`: tart's process and Virtualization's, which runs the VM's cores
//! and which launchd starts, so that it inherits nothing of tart's ([`run`]).
//!
//! The VM is the runner's isolation: no directory of the Mac's is shared into it and no
//! credential of the person's is in it, and its jobs run as its own `admin`. It keeps its
//! checkout, toolchains, tools and target dir on its own disk, an image on the build volume. Its
//! Linux tests run on aarch64, where GitHub's Linux job ran them on `x86_64`; clippy still lints
//! all three Linux triples.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

use super::{GUARD_CONFIG, GUARD_HOOK, LINUX_LABEL};

/// The image the VM is cloned from: Cirrus Labs' Ubuntu 24.04 for arm64, with tart's guest
/// agent, by digest.
const IMAGE: &str = "ghcr.io/cirruslabs/ubuntu@sha256:\
                     3acc11bebedcbcb813fb17dd0a4671a8c2e6baed537f3caa7922e170ed8a04ed";

/// Its cores, memory (MB) and disk (GB): eight of the Mac's ten cores at `Nice 10`, and the
/// memory a Linux test build of eight jobs takes, with swap behind it.
const CPUS: &str = "8";
const MEMORY_MB: &str = "8192";
const DISK_GB: &str = "120";

/// The guest's account, which the image makes with passwordless sudo, and its home.
const GUEST_HOME: &str = "/home/admin";

/// What the runner's jobs need beyond the image: a C toolchain for build scripts, and the
/// shells and tmux the PTY tests check.
const PACKAGES: [&str; 6] = ["build-essential", "pkg-config", "zsh", "fish", "tmux", "unzip"];

/// Longest the VM may take from `tart run` to a command running in it.
const BOOT: Duration = Duration::from_mins(3);

/// The niceness of the VM's processes: below the person's work, which runs at 0, and on every
/// core (`docs/decisions/tooling.md`, "The gate runs on warm runners here").
const NICE: i32 = 10;

/// Where tart keeps the VM and the images it was cloned from: under the warm runners' home.
pub fn tart_home(home: &Utf8Path) -> Utf8PathBuf {
    home.join("linux").join("tart")
}

/// `tart` with this home.
fn tart(home: &Utf8Path) -> Command {
    let mut command = Command::new("tart");
    command.env("TART_HOME", tart_home(home)).env("TART_NO_AUTO_PRUNE", "1");
    command
}

/// `command` run in the VM as its `admin`, through tart's guest agent; `stdin` given, it is
/// attached.
fn guest(home: &Utf8Path, command: &[&str], stdin: bool) -> Command {
    let mut tart = tart(home);
    tart.arg("exec");
    if stdin {
        tart.arg("-i");
    }
    tart.arg(LINUX_LABEL).args(command);
    tart
}

/// Run `command` in the VM, its output captured, feeding it `input`; fail with what it printed.
fn run_in(home: &Utf8Path, title: &str, command: &[&str], input: Option<&[u8]>) -> Result<String> {
    use std::io::Write as _;

    let mut child = guest(home, command, input.is_some())
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start: {title}"))?;
    if let (Some(input), Some(mut pipe)) = (input, child.stdin.take()) {
        pipe.write_all(input).with_context(|| format!("feed: {title}"))?;
    }
    let output = child.wait_with_output().with_context(|| format!("wait: {title}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        bail!(
            "{title} failed in the VM ({}):\n{stdout}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(stdout)
}

/// [`run_in`] as a step of the log.
fn step(home: &Utf8Path, title: &str, command: &[&str]) -> Result<()> {
    let started = Instant::now();
    run_in(home, title, command, None)?;
    println!("  ✓ {title} ({:.1?})", started.elapsed());
    Ok(())
}

/// `contents` into the VM's `path`, replacing it.
fn put(home: &Utf8Path, path: &str, contents: &[u8]) -> Result<()> {
    let of = format!("of={path}");
    run_in(home, &format!("write {path}"), &["dd", &of, "status=none"], Some(contents))?;
    Ok(())
}

/// The VM's state as `tart list` gives it, or `None` when there is no such VM.
fn state(home: &Utf8Path) -> Result<Option<String>> {
    #[derive(serde::Deserialize)]
    struct Vm {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "State")]
        state: String,
    }
    let output = tart(home).args(["list", "--format", "json"]).output().context("tart list")?;
    anyhow::ensure!(output.status.success(), "tart list: {}", output.status);
    let vms: Vec<Vm> = serde_json::from_slice(&output.stdout).context("tart list's JSON")?;
    Ok(vms.into_iter().find(|vm| vm.name == LINUX_LABEL).map(|vm| vm.state))
}

/// `xtask warm linux install`.
pub fn install(sh: &Shell, home: &Utf8Path, rev: &str) -> Result<()> {
    anyhow::ensure!(cfg!(target_os = "macos"), "the warm Linux runner's VM runs on a Mac");
    std::fs::create_dir_all(tart_home(home)).with_context(|| format!("create {home}"))?;
    if state(home)?.is_none() {
        println!("▶ the VM {LINUX_LABEL}, from {IMAGE}");
        let status = tart(home).args(["clone", IMAGE, LINUX_LABEL]).status()?;
        anyhow::ensure!(status.success(), "tart clone: {status}");
        let set =
            ["set", LINUX_LABEL, "--cpu", CPUS, "--memory", MEMORY_MB, "--disk-size", DISK_GB];
        let status = tart(home).args(set).status()?;
        anyhow::ensure!(status.success(), "tart set: {status}");
    }
    start(sh, home)?;
    println!("▶ provision {LINUX_LABEL}");
    provision(home)?;
    guard(sh, home, rev)?;
    runner(sh, home)?;
    Ok(())
}

/// The packages, the receive buffer a worker's socket asks for (4 MiB, which Ubuntu caps at
/// 208 KiB), completion directories zsh accepts, swap, and rustup.
fn provision(home: &Utf8Path) -> Result<()> {
    let installed = run_in(home, "dpkg-query", &["dpkg-query", "-W", "-f", "${Package}\n"], None)?;
    let missing: Vec<&str> =
        PACKAGES.iter().copied().filter(|p| !installed.lines().any(|l| l == *p)).collect();
    if !missing.is_empty() {
        step(home, "apt-get update", &["sudo", "apt-get", "update", "-qq"])?;
        let mut install = vec!["sudo", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get"];
        install.extend(["install", "-y", "-qq", "--no-install-recommends"]);
        install.extend(&missing);
        step(home, &format!("apt-get install {}", missing.join(" ")), &install)?;
    }
    let sysctl = "/etc/sysctl.d/60-slopty-warm.conf";
    put(home, "/tmp/slopty-warm.conf", b"net.core.rmem_max = 8388608\n")?;
    step(
        home,
        "the receive buffer",
        &["sudo", "install", "-m", "644", "/tmp/slopty-warm.conf", sysctl],
    )?;
    run_in(home, "sysctl", &["sudo", "sysctl", "-q", "-p", sysctl], None)?;
    // Ubuntu leaves completion directories group-writable, and zsh's compinit then stops to
    // ask about them, eating the tests' typed lines.
    let insecure = run_in(
        home,
        "compaudit",
        &["zsh", "-fc", "autoload -Uz compaudit; compaudit || true"],
        None,
    )?;
    let dirs: Vec<&str> = insecure.lines().filter(|l| l.starts_with('/')).collect();
    if !dirs.is_empty() {
        let mut chmod = vec!["sudo", "chmod", "g-w,o-w"];
        chmod.extend(&dirs);
        step(home, "completion directories zsh accepts", &chmod)?;
    }
    let swaps = run_in(home, "swapon", &["swapon", "--show=NAME", "--noheadings"], None)?;
    if !swaps.lines().any(|l| l.trim() == "/swapfile") {
        step(home, "swap", &["sudo", "fallocate", "-l", "8G", "/swapfile"])?;
        run_in(home, "chmod", &["sudo", "chmod", "600", "/swapfile"], None)?;
        run_in(home, "mkswap", &["sudo", "mkswap", "/swapfile"], None)?;
        run_in(home, "swapon", &["sudo", "swapon", "/swapfile"], None)?;
        let fstab = run_in(home, "fstab", &["cat", "/etc/fstab"], None)?;
        if !fstab.contains("/swapfile") {
            let line = "/swapfile none swap sw 0 0";
            run_in(
                home,
                "fstab",
                &["sudo", "sed", "-i", &format!("$a {line}"), "/etc/fstab"],
                None,
            )?;
        }
    }
    let rustup = format!("{GUEST_HOME}/.cargo/bin/rustup");
    if run_in(home, "rustup", &[&rustup, "--version"], None).is_err() {
        let sh = Shell::new()?;
        let downloads = home.join("linux").join("downloads");
        let init = super::rustup_init(&sh, &downloads, "aarch64-unknown-linux-gnu")?;
        let bytes = std::fs::read(&init).with_context(|| format!("read {init}"))?;
        // rustup-init goes by its file name: under another it takes itself for a proxy.
        let target = format!("{GUEST_HOME}/rustup-init");
        put(home, &target, &bytes)?;
        run_in(home, "chmod", &["chmod", "755", &target], None)?;
        let args = [
            target.as_str(),
            "-y",
            "--quiet",
            "--no-modify-path",
            "--profile",
            "minimal",
            "--default-toolchain",
            "none",
        ];
        step(home, "rustup", &args)?;
    }
    Ok(())
}

/// The guard, built in the VM from `rev`'s xtask (`git archive`, no submodule: xtask needs
/// none; not `--locked`, so a tree whose other manifests moved still builds it), with the
/// repository and its actors, and the hook that runs it.
fn guard(sh: &Shell, home: &Utf8Path, rev: &str) -> Result<()> {
    let source = format!("{GUEST_HOME}/guard-src");
    let built = format!("{GUEST_HOME}/guard-target");
    let dir = format!("{GUEST_HOME}/guard");
    let tree = cmd!(sh, "git archive --format=tar {rev}").quiet().output()?.stdout;
    run_in(home, "clean the guard's source", &["rm", "-rf", &source], None)?;
    run_in(home, "mkdir", &["mkdir", "-p", &source, &dir], None)?;
    run_in(home, "unpack the guard's source", &["tar", "-x", "-C", &source], Some(&tree))?;
    let file = cmd!(sh, "git show {rev}:rust-toolchain.toml").quiet().read()?;
    let toolchain = crate::toolchain::pinned(&file)
        .with_context(|| format!("{rev}'s rust-toolchain.toml pins no channel"))?
        .to_owned();
    let rustup = format!("{GUEST_HOME}/.cargo/bin/rustup");
    step(
        home,
        &format!("Rust {toolchain}"),
        &[&rustup, "toolchain", "install", &toolchain, "--profile", "minimal"],
    )?;
    let cargo = format!("{GUEST_HOME}/.cargo/bin/cargo");
    let pin = format!("RUSTUP_TOOLCHAIN={toolchain}");
    let target_dir = format!("--target-dir={built}");
    step(
        home,
        &format!("the guard from {rev}"),
        &[
            "env",
            "-C",
            &source,
            &pin,
            "RUSTC_WRAPPER=",
            "CARGO_INCREMENTAL=0",
            &cargo,
            "build",
            "--quiet",
            "-p",
            "xtask",
            &target_dir,
        ],
    )?;
    let binary = format!("{dir}/xtask");
    run_in(
        home,
        "install the guard",
        &["install", "-m", "755", &format!("{built}/debug/xtask"), &binary],
        None,
    )?;
    let config = super::repository(sh)?;
    let config_file = format!("{dir}/{GUARD_CONFIG}");
    put(home, &config_file, toml::to_string(&config)?.as_bytes())?;
    let hook = format!("{dir}/{GUARD_HOOK}");
    let script = super::hook_script(Utf8Path::new(&binary), Utf8Path::new(&config_file));
    put(home, &hook, script.as_bytes())?;
    run_in(home, "chmod", &["chmod", "755", &hook], None)?;
    Ok(())
}

/// The runner's `.env` in the VM: its jobs, incremental builds, the prune budget and floor of
/// its own disk, and the hook. sccache the warm jobs turn off themselves, since the runner drops
/// a variable whose value is empty.
fn environment() -> Vec<(&'static str, String)> {
    vec![
        ("LANG", "C.UTF-8".to_owned()),
        ("CARGO_BUILD_JOBS", CPUS.to_owned()),
        ("CARGO_INCREMENTAL", "1".to_owned()),
        ("SLOPTY_TARGET_BUDGET_GB", "60".to_owned()),
        ("SLOPTY_DISK_FLOOR_GB", "15".to_owned()),
        ("ACTIONS_RUNNER_HOOK_JOB_STARTED", format!("{GUEST_HOME}/guard/{GUARD_HOOK}")),
    ]
}

/// The actions runner in the VM, registered as [`LINUX_LABEL`] (the token on its standard input
/// and into `config.sh`'s environment, never on a command line) and run by systemd as `admin`.
fn runner(sh: &Shell, home: &Utf8Path) -> Result<()> {
    let dir = format!("{GUEST_HOME}/actions-runner");
    let configured = run_in(home, "the runner", &["test", "-f", &format!("{dir}/.runner")], None);
    if configured.is_err() {
        let downloads = home.join("linux").join("downloads");
        let tarball = super::runner_tarball(sh, &downloads, "linux-arm64")?;
        let bytes = std::fs::read(&tarball).with_context(|| format!("read {tarball}"))?;
        run_in(home, "mkdir", &["mkdir", "-p", &dir], None)?;
        run_in(home, "unpack the runner", &["tar", "-xz", "-C", &dir], Some(&bytes))?;
    }
    if configured.is_err() {
        let config = super::repository(sh)?;
        let token = super::registration_token(sh, &config.repository)?;
        let url = format!("https://github.com/{}", config.repository);
        let register = format!(
            "IFS= read -r token; ACTIONS_RUNNER_INPUT_TOKEN=\"$token\" exec ./config.sh \
             --unattended --replace --url {url} --name {LINUX_LABEL} --labels {LINUX_LABEL} \
             --work _work"
        );
        run_in(
            home,
            "register the runner",
            &["env", "-C", &dir, "sh", "-c", &register],
            Some(format!("{token}\n").as_bytes()),
        )?;
        step(
            home,
            "the runner's service",
            &["sudo", "env", "-C", &dir, "./svc.sh", "install", "admin"],
        )?;
    }
    // After `config.sh`, which writes both from its own environment; the runner reads them as it
    // starts, so the service starts again.
    put(home, &format!("{dir}/.env"), super::dotenv(&environment()).as_bytes())?;
    let path = format!(
        "{GUEST_HOME}/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n"
    );
    put(home, &format!("{dir}/.path"), path.as_bytes())?;
    let service = ["sudo", "env", "-C", dir.as_str(), "./svc.sh"];
    let _stopped = run_in(home, "stop the service", &[&service[..], &["stop"]].concat(), None);
    run_in(home, "start the service", &[&service[..], &["start"]].concat(), None)?;
    println!("  ✓ {LINUX_LABEL} runs its runner");
    Ok(())
}

/// `xtask warm linux run`: the VM in the foreground until it stops, as its daemon runs it. Tart
/// runs at [`NICE`], and so does the process of Virtualization's that holds the VM's disk once
/// it does: launchd starts that one, at the person's priority whatever tart's, and its threads
/// are the VM's cores. A VM whose process never shows in [`BOOT`] is stopped rather than left
/// to run at that priority.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a VM")]
pub fn run(home: &Utf8Path) -> Result<()> {
    lower(None)?;
    let mut vm =
        tart(home).args(["run", "--no-graphics", LINUX_LABEL]).stdin(Stdio::null()).spawn()?;
    let disk = tart_home(home).join("vms").join(LINUX_LABEL).join("disk.img");
    let started = Instant::now();
    loop {
        if let Some(status) = vm.try_wait()? {
            bail!("tart run {LINUX_LABEL} exited with {status}");
        }
        let holders = holders(&disk);
        if !holders.is_empty() {
            for pid in holders {
                lower(Some(pid)).with_context(|| format!("lower the VM's process {pid:?}"))?;
            }
            println!("  ✓ {LINUX_LABEL} runs at nice {NICE}");
            break;
        }
        if started.elapsed() >= BOOT {
            let _stopped = tart(home).args(["stop", LINUX_LABEL, "--timeout", "10"]).status();
            let _reaped = vm.wait();
            bail!("no process held {disk} in {BOOT:?}, so {LINUX_LABEL} was stopped");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let status = vm.wait().context("tart run")?;
    anyhow::ensure!(status.success(), "tart run {LINUX_LABEL}: {status}");
    Ok(())
}

/// The processes that have `path` open.
fn holders(path: &Utf8Path) -> Vec<rustix::process::Pid> {
    let listed = Command::new("lsof").arg("-t").arg(path).stderr(Stdio::null()).output();
    listed
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| rustix::process::Pid::from_raw(line.trim().parse().ok()?))
        .collect()
}

/// `pid` (this process for `None`) at [`NICE`], unless it already runs lower.
fn lower(pid: Option<rustix::process::Pid>) -> Result<()> {
    if rustix::process::getpriority_process(pid)? < NICE {
        rustix::process::setpriority_process(pid, NICE)?;
    }
    Ok(())
}

/// `xtask warm linux start`: [`run`] in the background, its output in `linux/vm.log`, once a
/// command runs in the VM. The VM keeps running after this process ends; one that does not come
/// up in [`BOOT`] is stopped.
#[expect(clippy::disallowed_methods, reason = "xtask is a script, not a library; polling a guest")]
pub fn start(_sh: &Shell, home: &Utf8Path) -> Result<()> {
    use std::os::unix::process::CommandExt as _;

    if state(home)?.as_deref() == Some("running") {
        return Ok(());
    }
    let log = home.join("linux").join("vm.log");
    let file = std::fs::File::create(&log).with_context(|| format!("create {log}"))?;
    let started = Instant::now();
    let mut vm = Command::new(std::env::current_exe().context("this xtask's path")?);
    vm.args(["warm", "linux", "run"])
        .env("SLOPTY_WARM_HOME", home)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .process_group(0);
    let mut vm = vm.spawn().context("tart run")?;
    loop {
        if let Some(status) = vm.try_wait()? {
            bail!("the VM exited with {status}; see {log}");
        }
        let up = guest(home, &["true"], false)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if up {
            println!("  ✓ {LINUX_LABEL} up ({:.1?})", started.elapsed());
            return Ok(());
        }
        if started.elapsed() >= BOOT {
            let _stopped = tart(home).args(["stop", LINUX_LABEL, "--timeout", "10"]).status();
            let _killed: std::io::Result<()> = vm.kill();
            let _reaped: std::io::Result<std::process::ExitStatus> = vm.wait();
            bail!("{LINUX_LABEL} did not come up in {BOOT:?} and was stopped; see {log}");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// `xtask warm linux stop`.
pub fn stop(_sh: &Shell, home: &Utf8Path) -> Result<()> {
    if state(home)?.as_deref() == Some("running") {
        let status = tart(home).args(["stop", LINUX_LABEL, "--timeout", "60"]).status()?;
        anyhow::ensure!(status.success(), "tart stop: {status}");
    }
    Ok(())
}

/// `xtask warm linux exec`.
pub fn exec(_sh: &Shell, home: &Utf8Path, command: &[String]) -> Result<()> {
    let args: Vec<&str> = command.iter().map(String::as_str).collect();
    let status = guest(home, &args, false).status().context("tart exec")?;
    anyhow::ensure!(status.success(), "{} in {LINUX_LABEL}: {status}", command.join(" "));
    Ok(())
}

/// The VM's state and its runner service's.
pub fn status(_sh: &Shell, home: &Utf8Path) {
    println!("▶ the Linux runner ({})", tart_home(home));
    let vm = state(home).ok().flatten();
    let disk = tart_home(home).join("vms").join(LINUX_LABEL).join("disk.img");
    let nice: Vec<String> = holders(&disk)
        .into_iter()
        .map(|pid| {
            rustix::process::getpriority_process(Some(pid))
                .map_or_else(|e| format!("{pid:?}: {e}"), |nice| format!("nice {nice}"))
        })
        .collect();
    println!("  VM: {} {}", vm.as_deref().unwrap_or("none"), nice.join(", "));
    if vm.as_deref() == Some("running") {
        let unit = run_in(
            home,
            "systemctl",
            &["sh", "-c", "systemctl is-active 'actions.runner.*' || true"],
            None,
        );
        println!(
            "  runner service: {}",
            unit.map_or_else(|e| format!("{e:#}"), |s| s.trim().to_owned())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::environment;

    /// The VM's runner builds with its eight cores, incrementally, keeps a floor its own disk can
    /// hold, and runs the guard first.
    #[test]
    fn the_vm_s_runner_environment() {
        let env = environment();
        let get = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str());
        assert_eq!(get("CARGO_BUILD_JOBS"), Some("8"));
        assert_eq!(get("CARGO_INCREMENTAL"), Some("1"));
        assert_eq!(get("SLOPTY_DISK_FLOOR_GB"), Some("15"));
        assert_eq!(get("ACTIONS_RUNNER_HOOK_JOB_STARTED"), Some("/home/admin/guard/started.sh"));
    }
}
