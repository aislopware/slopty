//! `slopty host install|uninstall` and `slopty server install|uninstall|status`: the host
//! daemons and the server as `LaunchAgents`.
//!
//! The server is one more agent, `slopty-server`, installed the same way: copied to
//! `<data dir>/bin/`, `KeepAlive`, its port baked into the plist, and `install` waits until it
//! answers a hello on loopback.
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
use clap::{Args, Subcommand};
use plist::{Dictionary, Value};
use slopty_host::ctl::{CtlReply, CtlRequest};
use slopty_net::HostAddr;
use slopty_net::endpoint::SERVER_PORT;
use slopty_proto::server::Role;

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
/// `LimitLoadToSessionType Aqua` puts them in the login session, where `slopty-hostd` reaches
/// ScreenCaptureKit and the window server.
pub fn plists(opts: &InstallOpts, bin_dir: &Path, data_dir: &Path) -> Vec<(String, Value)> {
    let layout = Layout::new(data_dir);
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let common = |label: &str, program: &str, args: &[String], env: Vec<(&str, String)>| {
        let mut vars = Dictionary::new();
        vars.insert("SLOPTY_PTYD_SOCKET".into(), Value::String(path(&layout.ptyd_socket())));
        vars.insert("SLOPTY_HOSTD_SOCKET".into(), Value::String(path(&layout.hostd_socket())));
        for (k, v) in env {
            vars.insert(k.into(), Value::String(v));
        }
        let program = bin_dir.join(program);
        let mut d = launch_agent(label, &program, args, &opts.log, data_dir, vars);
        d.insert("LimitLoadToSessionType".into(), Value::String("Aqua".into()));
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

/// What every Slopty `LaunchAgent` shares: `program args…` with `RUST_LOG` and
/// `SLOPTY_DATA_DIR` (plus `vars`) in its environment, restarted when it dies and at login, its
/// output in `~/Library/Logs/Slopty/<program>.log`.
///
/// `ProcessType Interactive` keeps it out of App Nap and background quality of service, where
/// every answer would wait on a throttled timer; `ThrottleInterval` makes a crash loop restart
/// every 2 s rather than launchd's 10 s default.
fn launch_agent(
    label: &str,
    program: &Path,
    args: &[String],
    log: &str,
    data_dir: &Path,
    mut vars: Dictionary,
) -> Dictionary {
    let layout = Layout::new(data_dir);
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut d = Dictionary::new();
    d.insert("Label".into(), Value::String(label.to_owned()));
    let mut argv = vec![Value::String(path(program))];
    argv.extend(args.iter().cloned().map(Value::String));
    d.insert("ProgramArguments".into(), Value::Array(argv));
    vars.insert("RUST_LOG".into(), Value::String(log.to_owned()));
    vars.insert("SLOPTY_DATA_DIR".into(), Value::String(path(data_dir)));
    d.insert("EnvironmentVariables".into(), Value::Dictionary(vars));
    d.insert("RunAtLoad".into(), Value::Boolean(true));
    d.insert("KeepAlive".into(), Value::Boolean(true));
    d.insert("ThrottleInterval".into(), Value::Integer(2.into()));
    d.insert("ProcessType".into(), Value::String("Interactive".into()));
    d.insert("WorkingDirectory".into(), Value::String(path(&home())));
    let name = program.file_name().map_or_else(|| "slopty".into(), |n| n.to_string_lossy());
    let log = layout.logs.join(format!("{name}.log"));
    d.insert("StandardOutPath".into(), Value::String(path(&log)));
    d.insert("StandardErrorPath".into(), Value::String(path(&log)));
    d
}

/// The binaries an installation carries.
const BINARIES: [&str; 3] = ["slopty-ptyd", "slopty-hostd", "slopty"];

/// Copy the binaries, write the plists, (re)bootstrap both agents, wait for the daemon and
/// print a ticket.
pub async fn install(opts: &InstallOpts) -> Result<()> {
    let source = binaries_source(opts.bin_dir.as_deref())?;
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
        match hostctl::call_at(&socket, CtlRequest::Status).await {
            Ok(CtlReply::Status { name, .. }) => {
                println!(
                    "\n{name} is up; add it from a client with `slopty add <this Mac's tailnet \
                     name or IP>` or the app's \"Add host…\""
                );
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

/// launchd label of the server.
pub const SERVER_LABEL: &str = "dev.aislopware.slopty.server";

/// `slopty server …`.
#[derive(Subcommand, Debug)]
pub enum ServerCmd {
    /// Run `slopty-server` as a `LaunchAgent` (starts now and at every login).
    Install(ServerInstallOpts),
    /// Stop the `LaunchAgent` and remove it.
    Uninstall,
    /// Whether the `LaunchAgent` is installed and running, and whether the server answers.
    Status,
}

/// `slopty server install` options.
#[derive(Args, Debug, Clone)]
pub struct ServerInstallOpts {
    /// Where to copy `slopty-server` from (default: this binary's directory).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// UDP port for the server (default: its fixed port).
    #[arg(long)]
    port: Option<u16>,
    /// `RUST_LOG` for the server.
    #[arg(long, default_value = "info")]
    log: String,
}

/// The server's property list: `slopty-server [--port N]` out of `bin_dir`.
fn server_plist(opts: &ServerInstallOpts, bin_dir: &Path, data_dir: &Path) -> Value {
    let args = opts.port.map(|p| vec!["--port".to_owned(), p.to_string()]).unwrap_or_default();
    let program = bin_dir.join("slopty-server");
    Value::Dictionary(launch_agent(
        SERVER_LABEL,
        &program,
        &args,
        &opts.log,
        data_dir,
        Dictionary::new(),
    ))
}

/// The port an installed server's property list names, else the default.
fn installed_port(plist: &Path) -> u16 {
    let argv = Value::from_file(plist).ok().and_then(|v| {
        let args = v.as_dictionary()?.get("ProgramArguments")?.as_array()?.clone();
        Some(args.into_iter().filter_map(Value::into_string).collect::<Vec<_>>())
    });
    argv.as_deref().and_then(port_arg).unwrap_or(SERVER_PORT)
}

/// The value of `--port` in an argument list.
fn port_arg(argv: &[String]) -> Option<u16> {
    argv.iter()
        .position(|a| a == "--port")
        .and_then(|i| argv.get(i.saturating_add(1)))?
        .parse()
        .ok()
}

/// Dial the server on loopback as an agent and return its name.
async fn probe(port: u16) -> Result<String> {
    let endpoint = slopty_net::client::bind_client()?;
    let address = HostAddr::new("127.0.0.1", port);
    let role = Role::Agent { name: "slopty server".to_owned() };
    let name = match slopty_net::server::connect(&endpoint, &address, role).await {
        Ok(link) => {
            link.close();
            Ok(link.name)
        }
        Err(e) => Err(e.into()),
    };
    crate::client::close_endpoint(&endpoint).await;
    name
}

/// `slopty server …`.
pub async fn server(cmd: ServerCmd, data_dir: &Path) -> Result<()> {
    match cmd {
        ServerCmd::Install(opts) => install_server(&opts, data_dir).await,
        ServerCmd::Uninstall => uninstall_server(data_dir),
        ServerCmd::Status => {
            server_status(data_dir).await;
            Ok(())
        }
    }
}

/// Copy `slopty-server`, write its plist, (re)bootstrap it and wait until it answers.
async fn install_server(opts: &ServerInstallOpts, data_dir: &Path) -> Result<()> {
    let source = binaries_source(opts.bin_dir.as_deref())?;
    let from = source.join("slopty-server");
    if !from.is_file() {
        bail!(
            "{} not found; build it (cargo build -p slopty-server) or pass --bin-dir",
            from.display()
        );
    }
    let layout = Layout::new(data_dir);
    let bin_dir = layout.bin();
    for dir in [&layout.logs, &layout.agents, &bin_dir] {
        std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    }
    let uid = rustix::process::getuid().as_raw();
    bootout(uid, SERVER_LABEL);
    if bin_dir != source {
        let to = bin_dir.join("slopty-server");
        std::fs::copy(&from, &to)
            .with_context(|| format!("copy {} to {}", from.display(), to.display()))?;
    }
    let path = layout.plist(SERVER_LABEL);
    plist::to_file_xml(&path, &server_plist(opts, &bin_dir, data_dir))
        .with_context(|| format!("write {}", path.display()))?;
    launchctl(&["bootstrap", &format!("gui/{uid}"), &path.to_string_lossy()])
        .with_context(|| format!("bootstrap {SERVER_LABEL}"))?;
    println!("installed {SERVER_LABEL}  ({})", path.display());
    let port = opts.port.unwrap_or(SERVER_PORT);
    let started = Instant::now();
    loop {
        match probe(port).await {
            Ok(name) => {
                println!(
                    "\n{name} is up on UDP {port}; point clients at it with `slopty --server \
                     <this Mac's tailnet name or IP>` or `server = \"…\"` under [client] in \
                     settings.toml"
                );
                return Ok(());
            }
            Err(e) if started.elapsed() < START_TIMEOUT => {
                tracing::debug!(error = %e, "waiting for slopty-server");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => bail!("the server did not come up ({e:#}); see {}", layout.logs.display()),
        }
    }
}

/// Stop the server's agent and remove its plist.
fn uninstall_server(data_dir: &Path) -> Result<()> {
    let path = Layout::new(data_dir).plist(SERVER_LABEL);
    bootout(rustix::process::getuid().as_raw(), SERVER_LABEL);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("removed {SERVER_LABEL}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("{SERVER_LABEL} not installed");
        }
        Err(e) => return Err(e).with_context(|| format!("remove {}", path.display())),
    }
    Ok(())
}

/// Installed or not, launchd's pid, and whether the server answers on loopback.
async fn server_status(data_dir: &Path) {
    let path = Layout::new(data_dir).plist(SERVER_LABEL);
    let uid = rustix::process::getuid().as_raw();
    let pid = launchctl(&["print", &format!("gui/{uid}/{SERVER_LABEL}")])
        .ok()
        .and_then(|out| launchd_pid(&out));
    let state = match (path.is_file(), pid) {
        (_, Some(pid)) => format!("running (pid {pid})"),
        (true, None) => "installed, not running".to_owned(),
        (false, None) => "not installed".to_owned(),
    };
    println!("{SERVER_LABEL}  {state}");
    let port = installed_port(&path);
    match probe(port).await {
        Ok(name) => println!("{name} answers on UDP {port}"),
        Err(e) => println!("nothing answers on UDP {port}: {e:#}"),
    }
}

/// Where to copy binaries from: `--bin-dir`, else this binary's directory.
fn binaries_source(bin_dir: Option<&Path>) -> Result<PathBuf> {
    let source = match bin_dir {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_exe()
            .context("current exe")?
            .parent()
            .context("exe dir")?
            .to_path_buf(),
    };
    source.canonicalize().with_context(|| format!("{}", source.display()))
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
    fn the_server_plist_runs_slopty_server_on_its_port() {
        let opts = ServerInstallOpts { bin_dir: None, port: Some(45561), log: "debug".to_owned() };
        let value = server_plist(&opts, Path::new("/opt/slopty/bin"), Path::new("/data/slopty"));
        let d = value.as_dictionary().unwrap();
        assert_eq!(d["Label"].as_string(), Some(SERVER_LABEL));
        let argv: Vec<String> = d["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_string().unwrap().to_owned())
            .collect();
        assert_eq!(argv, ["/opt/slopty/bin/slopty-server", "--port", "45561"]);
        assert_eq!(port_arg(&argv), Some(45561));
        let env = d["EnvironmentVariables"].as_dictionary().unwrap();
        assert_eq!(env["SLOPTY_DATA_DIR"].as_string(), Some("/data/slopty"));
        assert_eq!(env["RUST_LOG"].as_string(), Some("debug"));
        assert_eq!(d["KeepAlive"].as_boolean(), Some(true));
        assert!(
            d["StandardErrorPath"].as_string().unwrap().ends_with("Logs/Slopty/slopty-server.log"),
            "{:?}",
            d["StandardErrorPath"]
        );
        assert!(!d.contains_key("LimitLoadToSessionType"), "the server needs no GUI session");
    }

    #[test]
    fn an_installed_server_without_a_port_flag_uses_the_default() {
        assert_eq!(port_arg(&["/bin/slopty-server".to_owned()]), None);
        assert_eq!(installed_port(Path::new("/nonexistent.plist")), SERVER_PORT);
    }

    #[test]
    fn launchd_pid_reads_the_print_output() {
        assert_eq!(launchd_pid("\tstate = running\n\tpid = 4242\n"), Some(4242));
        assert_eq!(launchd_pid("\tstate = not running\n"), None);
    }
}
