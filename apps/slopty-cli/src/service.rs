//! `slopty host install|uninstall`: the host daemons as `LaunchAgents`.
//!
//! Two agents in `~/Library/LaunchAgents`: `slopty-ptyd` (the PTY custodian, keeps shells alive
//! across daemon restarts) and `slopty-hostd`, both `KeepAlive` so launchd restarts either one
//! that dies and both come back at login. Sockets live under `<data dir>/run/` so the CLI can
//! find them without launchd's environment; the data dir, reach, port and log level are baked
//! into the plists at install time. The binaries are copied to `<data dir>/bin/` first: a
//! dev-tree build gets overwritten by the next `cargo build`, and a binary on an external
//! volume hangs in dyld under launchd (the "removable volume" consent has no one to click it).
//! `install` is idempotent: it stops the agents, recopies, rewrites the plists, bootstraps
//! them again, then waits for the daemon and prints a pairing ticket.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use clap::Args;
use plist::{Dictionary, Value};
use slopty_host::ctl::{CtlReply, CtlRequest};

use crate::hostctl;

/// launchd label of the PTY custodian.
pub const PTYD_LABEL: &str = "dev.aislopware.slopty.ptyd";
/// launchd label of the host daemon.
pub const HOSTD_LABEL: &str = "dev.aislopware.slopty.hostd";
/// How long `install` waits for the daemon's control socket before giving up on the ticket.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// `slopty host install` options.
#[derive(Args, Debug, Clone, Default)]
pub struct InstallOpts {
    /// Where to copy `slopty-ptyd`, `slopty-hostd` and `slopty` from (default: this binary's
    /// directory).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// Data directory (default: `$SLOPTY_DATA_DIR` or `~/Library/Application Support/Slopty`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// No relay, no wide-area lookup (LAN or private mesh only).
    #[arg(long)]
    direct_only: bool,
    /// UDP port for the host (default: the daemon's fixed port).
    #[arg(long)]
    port: Option<u16>,
    /// Listen on this one IP and reach nothing off it. A shaped measurement installs the host
    /// this way so the run stays on its relay; it is unreachable from anywhere else meanwhile.
    #[arg(long)]
    bind: Option<std::net::IpAddr>,
    /// `RUST_LOG` for both daemons.
    #[arg(long, default_value = "info")]
    log: String,
}

/// Where the agents' sockets and logs go for a data dir.
pub struct Layout {
    /// `<data>/run`.
    pub run: PathBuf,
    /// `~/Library/Logs/Slopty`.
    pub logs: PathBuf,
    /// `~/Library/LaunchAgents`.
    pub agents: PathBuf,
}

impl Layout {
    fn new(data_dir: &Path) -> Self {
        let home = home();
        Self {
            run: data_dir.join("run"),
            logs: home.join("Library").join("Logs").join("Slopty"),
            agents: home.join("Library").join("LaunchAgents"),
        }
    }

    /// `<data>/bin`: where the daemons run from.
    fn bin(&self) -> PathBuf {
        self.run.parent().map_or_else(|| PathBuf::from("bin"), |data| data.join("bin"))
    }

    fn ptyd_socket(&self) -> PathBuf {
        self.run.join("ptyd.sock")
    }

    fn hostd_socket(&self) -> PathBuf {
        self.run.join("hostd.sock")
    }

    fn plist(&self, label: &str) -> PathBuf {
        self.agents.join(format!("{label}.plist"))
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

/// The two agents' property lists for `opts`, in start order.
///
/// `ProcessType Interactive` keeps the daemons out of App Nap and lets `slopty-hostd` reach
/// ScreenCaptureKit and the window server (a `Background` agent gets neither); `ThrottleInterval`
/// makes a crash loop restart every 2 s rather than launchd's 10 s default.
pub fn plists(opts: &InstallOpts, bin_dir: &Path, data_dir: &Path) -> Vec<(String, Value)> {
    let layout = Layout::new(data_dir);
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let common = |label: &str, program: &str, args: &[String], env: Vec<(&str, String)>| {
        let mut d = Dictionary::new();
        d.insert("Label".into(), Value::String(label.to_owned()));
        let mut argv = vec![Value::String(path(&bin_dir.join(program)))];
        argv.extend(args.iter().cloned().map(Value::String));
        d.insert("ProgramArguments".into(), Value::Array(argv));
        let mut vars = Dictionary::new();
        vars.insert("RUST_LOG".into(), Value::String(opts.log.clone()));
        vars.insert("SLOPTY_DATA_DIR".into(), Value::String(path(data_dir)));
        vars.insert("SLOPTY_PTYD_SOCKET".into(), Value::String(path(&layout.ptyd_socket())));
        vars.insert("SLOPTY_HOSTD_SOCKET".into(), Value::String(path(&layout.hostd_socket())));
        for (k, v) in env {
            vars.insert(k.into(), Value::String(v));
        }
        d.insert("EnvironmentVariables".into(), Value::Dictionary(vars));
        d.insert("RunAtLoad".into(), Value::Boolean(true));
        d.insert("KeepAlive".into(), Value::Boolean(true));
        d.insert("ThrottleInterval".into(), Value::Integer(2.into()));
        d.insert("ProcessType".into(), Value::String("Interactive".into()));
        d.insert("LimitLoadToSessionType".into(), Value::String("Aqua".into()));
        d.insert("WorkingDirectory".into(), Value::String(path(&home())));
        let log = layout.logs.join(format!("{program}.log"));
        d.insert("StandardOutPath".into(), Value::String(path(&log)));
        d.insert("StandardErrorPath".into(), Value::String(path(&log)));
        Value::Dictionary(d)
    };
    let mut hostd_args = Vec::new();
    if opts.direct_only {
        hostd_args.push("--direct-only".to_owned());
    }
    if let Some(port) = opts.port {
        hostd_args.push("--port".to_owned());
        hostd_args.push(port.to_string());
    }
    if let Some(ip) = opts.bind {
        hostd_args.push("--bind".to_owned());
        hostd_args.push(ip.to_string());
    }
    vec![
        (PTYD_LABEL.to_owned(), common(PTYD_LABEL, "slopty-ptyd", &[], Vec::new())),
        (HOSTD_LABEL.to_owned(), common(HOSTD_LABEL, "slopty-hostd", &hostd_args, Vec::new())),
    ]
}

/// The binaries an installation carries.
const BINARIES: [&str; 3] = ["slopty-ptyd", "slopty-hostd", "slopty"];

/// Copy the binaries, write the plists, (re)bootstrap both agents, wait for the daemon and
/// print a ticket.
pub async fn install(opts: &InstallOpts) -> Result<()> {
    let source = match &opts.bin_dir {
        Some(dir) => dir.clone(),
        None => std::env::current_exe()
            .context("current exe")?
            .parent()
            .context("exe dir")?
            .to_path_buf(),
    };
    let source = source.canonicalize().with_context(|| format!("{}", source.display()))?;
    for bin in BINARIES {
        let p = source.join(bin);
        if !p.is_file() {
            bail!("{} not found; pass --bin-dir", p.display());
        }
    }
    let data_dir = opts.data_dir.clone().unwrap_or_else(crate::client::data_dir);
    let layout = Layout::new(&data_dir);
    let bin_dir = layout.bin();
    for dir in [&layout.run, &layout.logs, &layout.agents, &bin_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    }
    let uid = rustix::process::getuid().as_raw();
    // Stop first: the copy must not land on a running binary, and a stale socket file makes
    // the daemon's bind fail (launchd would then loop on it).
    for label in [HOSTD_LABEL, PTYD_LABEL] {
        bootout(uid, label);
    }
    if bin_dir != source {
        for bin in BINARIES {
            let (from, to) = (source.join(bin), bin_dir.join(bin));
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
        }
    }
    for (label, value) in plists(opts, &bin_dir, &data_dir) {
        let path = layout.plist(&label);
        plist::to_file_xml(&path, &value).with_context(|| format!("write {}", path.display()))?;
        launchctl(&["bootstrap", &format!("gui/{uid}"), &path.to_string_lossy()])
            .with_context(|| format!("bootstrap {label}"))?;
        println!("installed {label}  ({})", path.display());
    }
    let socket = layout.hostd_socket();
    let started = Instant::now();
    loop {
        match hostctl::call_at(&socket, CtlRequest::Ticket).await {
            Ok(CtlReply::Ticket { ticket }) => {
                println!("\npair a client with:\n{ticket}");
                return Ok(());
            }
            Ok(other) => bail!("unexpected reply {other:?}"),
            Err(e) if started.elapsed() < START_TIMEOUT => {
                tracing::debug!(error = %e, "waiting for slopty-hostd");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => {
                bail!("daemon did not come up ({e:#}); see {}", layout.logs.display())
            }
        }
    }
}

/// Stop both agents and remove their plists. Sessions die with `slopty-ptyd`.
pub fn uninstall(data_dir: Option<&Path>) -> Result<()> {
    let data_dir = data_dir.map_or_else(crate::client::data_dir, Path::to_path_buf);
    let layout = Layout::new(&data_dir);
    let uid = rustix::process::getuid().as_raw();
    for label in [HOSTD_LABEL, PTYD_LABEL] {
        bootout(uid, label);
        let path = layout.plist(label);
        match std::fs::remove_file(&path) {
            Ok(()) => println!("removed {label}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("{label} not installed"),
            Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
        }
    }
    Ok(())
}

/// One line per agent: installed or not, and launchd's pid when it runs.
pub fn status(data_dir: Option<&Path>) {
    let data_dir = data_dir.map_or_else(crate::client::data_dir, Path::to_path_buf);
    let layout = Layout::new(&data_dir);
    let uid = rustix::process::getuid().as_raw();
    for label in [PTYD_LABEL, HOSTD_LABEL] {
        let installed = layout.plist(label).is_file();
        let pid = launchctl(&["print", &format!("gui/{uid}/{label}")])
            .ok()
            .and_then(|out| launchd_pid(&out));
        let state = match (installed, pid) {
            (_, Some(pid)) => format!("running (pid {pid})"),
            (true, None) => "installed, not running".to_owned(),
            (false, None) => "not installed".to_owned(),
        };
        println!("{label}  {state}");
    }
}

/// The `pid = N` line of `launchctl print`.
fn launchd_pid(out: &str) -> Option<u32> {
    out.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("pid = ").and_then(|rest| rest.trim().parse().ok())
    })
}

/// Unload an agent if it is loaded; not being loaded is not an error.
fn bootout(uid: u32, label: &str) {
    if let Err(e) = launchctl(&["bootout", &format!("gui/{uid}/{label}")]) {
        tracing::debug!(label, error = %e, "bootout");
    }
}

fn launchctl(args: &[&str]) -> Result<String> {
    let out = Command::new("launchctl").args(args).output().context("run launchctl")?;
    if !out.status.success() {
        bail!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plists_carry_the_paths_and_flags() {
        let opts = InstallOpts {
            direct_only: true,
            port: Some(45551),
            bind: Some(std::net::IpAddr::from([192, 168, 1, 10])),
            ..InstallOpts::default()
        };
        let list = plists(&opts, Path::new("/opt/slopty/bin"), Path::new("/data/slopty"));
        assert_eq!(list.len(), 2);
        let hostd = list[1].1.as_dictionary().unwrap();
        let argv: Vec<&str> = hostd["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_string().unwrap())
            .collect();
        assert_eq!(
            argv,
            [
                "/opt/slopty/bin/slopty-hostd",
                "--direct-only",
                "--port",
                "45551",
                "--bind",
                "192.168.1.10"
            ]
        );
        let env = hostd["EnvironmentVariables"].as_dictionary().unwrap();
        assert_eq!(env["SLOPTY_HOSTD_SOCKET"].as_string(), Some("/data/slopty/run/hostd.sock"));
        assert_eq!(env["SLOPTY_DATA_DIR"].as_string(), Some("/data/slopty"));
        assert_eq!(hostd["KeepAlive"].as_boolean(), Some(true));
        assert_eq!(hostd["ProcessType"].as_string(), Some("Interactive"));
        let ptyd = list[0].1.as_dictionary().unwrap();
        assert_eq!(ptyd["Label"].as_string(), Some(PTYD_LABEL));
    }

    #[test]
    fn launchd_pid_reads_the_print_output() {
        assert_eq!(launchd_pid("\tstate = running\n\tpid = 4242\n"), Some(4242));
        assert_eq!(launchd_pid("\tstate = not running\n"), None);
    }
}
