//! `slopty worker install|uninstall` and `slopty server install|uninstall|status`: the worker
//! daemons and the server as services of the user's session, through
//! [`slopty_platform::service`], which the app's "Use this Mac as a worker" runs too.
//!
//! What is the CLI's own: where the binaries come from (`--bin-dir`, else beside this one), the
//! server a worker registers with (the global `--server`, saved before the daemon starts), and
//! the wait until the daemon answers, with a word on how clients will find it. The worker's
//! data dir, reach, port and log level are baked into its definitions at install time; its
//! sockets live under `<data dir>/run/` so the CLI can find them without the manager's
//! environment.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use slopty_net::HostAddr;
use slopty_net::endpoint::SERVER_PORT;
use slopty_platform::service::{
    self as platform, Layout, Manager, PTYD, SERVER, Session, WORKER, WorkerOpts,
};
use slopty_proto::ctl::{CtlReply, CtlRequest};
use slopty_proto::server::Role;

use crate::workerctl;

/// How long `install` waits for the daemon's control socket before it gives up.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// `slopty worker install` options.
#[derive(Args, Debug, Clone, Default)]
pub struct InstallOpts {
    /// Where to copy `slopty-ptyd`, `slopty-worker` and `slopty` from (default: this binary's
    /// directory).
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// UDP port for the worker (default: the daemon's fixed port).
    #[arg(long)]
    port: Option<u16>,
    /// Listen on this one IP and reach nothing off it. A shaped measurement installs the worker
    /// this way so the run stays on its relay; it is unreachable from anywhere else meanwhile.
    #[arg(long)]
    bind: Option<std::net::IpAddr>,
    /// `RUST_LOG` for both daemons.
    #[arg(long, default_value = "info")]
    log: String,
}

impl InstallOpts {
    /// How the daemons run.
    fn worker(&self) -> WorkerOpts {
        WorkerOpts { port: self.port, bind: self.bind, log: self.log.clone() }
    }
}

/// Save `server` as `[worker] server` in the settings file under `data_dir`, keeping the rest of
/// the file. The daemon reads it when it starts, so this runs before the bootstrap.
fn save_worker_server(data_dir: &Path, server: &str) -> Result<HostAddr> {
    let addr = HostAddr::parse_with_port(server, SERVER_PORT)
        .with_context(|| format!("server address {server:?}"))?;
    slopty_settings::save_server(data_dir, slopty_settings::ServerOf::Worker, Some(&addr))
        .map_err(|e| anyhow!(e))?;
    Ok(addr)
}

/// An install's error, pointing at `--bin-dir` when a binary was not where it looked.
fn install_error(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound { anyhow!("{e}; pass --bin-dir") } else { e.into() }
}

/// Install and start both services, then wait for the daemon. `server` (the global
/// `--server`) is saved as the server the worker registers with.
pub async fn install(opts: &InstallOpts, server: Option<&str>, data_dir: &Path) -> Result<()> {
    let source = binaries_source(opts.bin_dir.as_deref())?;
    if let Some(server) = server {
        save_worker_server(data_dir, server)?;
    }
    let registers_with =
        slopty_settings::Settings::load(&slopty_settings::path_in(data_dir)).settings.worker.server;
    let session = Session::native();
    let installed = platform::install_worker(&session, &opts.worker(), &source, data_dir)
        .await
        .map_err(install_error)?;
    for (job, path) in &installed.definitions {
        println!("installed {}  ({})", job.program, path.display());
    }
    let socket = Layout::new(data_dir).worker_socket();
    let started = Instant::now();
    loop {
        match workerctl::call_at(&socket, CtlRequest::Status).await {
            Ok(CtlReply::Status { name, .. }) => {
                match &registers_with {
                    Some(server) => println!(
                        "\n{name} is up and registers with {server}; every client of that server \
                         lists it"
                    ),
                    None => println!(
                        "\n{name} is up on its own (pass --server to register it); add it from a \
                         client with `slopty add <this machine's tailnet name or IP>` or the \
                         app's \"Add a worker\""
                    ),
                }
                if let Some(note) = session.install_note() {
                    println!("{note}");
                }
                return Ok(());
            }
            Ok(other) => bail!("unexpected reply {other:?}"),
            Err(e) if started.elapsed() < START_TIMEOUT => {
                tracing::debug!(error = %e, "waiting for slopty-worker");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => bail!("daemon did not come up ({e:#}); see {}", session.logs(WORKER)),
        }
    }
}

/// Stop both services and remove their definitions. Sessions die with `slopty-ptyd`.
pub async fn uninstall() -> Result<()> {
    for (job, was) in platform::uninstall_worker(&Session::native()).await? {
        say_removed(job, was);
    }
    Ok(())
}

fn say_removed(job: platform::Job, was: bool) {
    if was {
        println!("removed {}", job.program);
    } else {
        println!("{} not installed", job.program);
    }
}

/// One line per service: installed or not, and its pid when it runs.
pub fn status() {
    let session = Session::native();
    for job in [PTYD, WORKER] {
        println!("{}  {}", job.program, session.state(job));
    }
}

/// `slopty server …`.
#[derive(Subcommand, Debug)]
pub enum ServerCmd {
    /// Run `slopty-server` as a service of this session (starts now and at every login).
    Install(ServerInstallOpts),
    /// Stop the service and remove it.
    Uninstall,
    /// Whether the service is installed and running, and whether the server answers.
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

/// The port the installed server's definition names, else the default.
fn installed_port(manager: Manager, path: &Path) -> u16 {
    platform::installed_args(manager, path).as_deref().and_then(port_arg).unwrap_or(SERVER_PORT)
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
        ServerCmd::Uninstall => {
            say_removed(SERVER, Session::native().remove(SERVER).await?);
            Ok(())
        }
        ServerCmd::Status => {
            server_status().await;
            Ok(())
        }
    }
}

/// Install and start `slopty-server`, then wait until it answers.
async fn install_server(opts: &ServerInstallOpts, data_dir: &Path) -> Result<()> {
    let source = binaries_source(opts.bin_dir.as_deref())?;
    let session = Session::native();
    let path = platform::install_server(&session, opts.port, &opts.log, &source, data_dir)
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!("{e}; build it (cargo build -p slopty-server) or pass --bin-dir")
            } else {
                e.into()
            }
        })?;
    println!("installed slopty-server  ({})", path.display());
    let port = opts.port.unwrap_or(SERVER_PORT);
    let started = Instant::now();
    loop {
        match probe(port).await {
            Ok(name) => {
                println!(
                    "\n{name} is up on UDP {port}; point clients at it with `slopty --server \
                     <this machine's tailnet name or IP>` or `server = \"…\"` under [client] in \
                     settings.toml"
                );
                if let Some(note) = session.install_note() {
                    println!("{note}");
                }
                return Ok(());
            }
            Err(e) if started.elapsed() < START_TIMEOUT => {
                tracing::debug!(error = %e, "waiting for slopty-server");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => bail!("the server did not come up ({e:#}); see {}", session.logs(SERVER)),
        }
    }
}

/// Installed or not, the manager's pid, and whether the server answers on loopback.
async fn server_status() {
    let session = Session::native();
    println!("slopty-server  {}", session.state(SERVER));
    let port = installed_port(session.manager, &session.file(SERVER));
    match probe(port).await {
        Ok(name) => println!("{name} answers on UDP {port}"),
        Err(e) => println!("nothing answers on UDP {port}: {e:#}"),
    }
}

/// Where to take binaries from: `--bin-dir`, else this binary's directory.
fn binaries_source(bin_dir: Option<&Path>) -> Result<PathBuf> {
    match bin_dir {
        Some(dir) => dir.canonicalize().with_context(|| format!("{}", dir.display())),
        None => platform::sibling_dir().context("this binary's directory"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_saves_the_workers_server_and_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = save_worker_server(dir.path(), "studio").unwrap();
        assert_eq!((fresh.host(), fresh.port()), ("studio", SERVER_PORT));
        let path = slopty_settings::path_in(dir.path());
        let loaded = slopty_settings::Settings::load(&path).settings;
        assert_eq!(loaded.worker.server, Some(fresh), "a missing file starts from the defaults");
        assert_eq!(loaded.client.server, None, "the client's server is not the worker's");

        std::fs::write(&path, "[font]\nmono_size = 15.0 # mine\n").unwrap();
        save_worker_server(dir.path(), "100.64.0.9:7000").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("[font]\nmono_size = 15.0 # mine\n"), "{text}");
        assert!(text.contains("[worker]\nserver = \"100.64.0.9:7000\""), "{text}");

        save_worker_server(dir.path(), "a b").unwrap_err();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "a bad address writes nothing");
    }

    /// The CLI's flags reach the library's worker as they were given.
    #[test]
    fn the_install_flags_are_the_workers_options() {
        let opts = InstallOpts {
            bin_dir: None,
            port: Some(45551),
            bind: Some(std::net::IpAddr::from([192, 168, 1, 10])),
            log: "debug".to_owned(),
        };
        assert_eq!(
            opts.worker(),
            WorkerOpts {
                port: Some(45551),
                bind: Some(std::net::IpAddr::from([192, 168, 1, 10])),
                log: "debug".to_owned(),
            }
        );
    }

    #[test]
    fn an_installed_server_without_a_port_flag_uses_the_default() {
        assert_eq!(port_arg(&["/bin/slopty-server".to_owned()]), None);
        assert_eq!(
            port_arg(&["s".to_owned(), "--port".to_owned(), "45561".to_owned()]),
            Some(45561)
        );
        assert_eq!(installed_port(Manager::Launchd, Path::new("/nonexistent.plist")), SERVER_PORT);
    }

    /// A missing binary names the flag that points elsewhere.
    #[test]
    fn a_missing_binary_points_at_bin_dir() {
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "/x/slopty-ptyd not found");
        assert_eq!(install_error(e).to_string(), "/x/slopty-ptyd not found; pass --bin-dir");
    }
}
