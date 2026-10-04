//! `slopty worker install|uninstall` and `slopty server install|uninstall|status`: the worker
//! daemons and the server as services of the user's session, through
//! [`slopty_platform::service`], which the app's "Use this Mac" runs too.
//!
//! What is the CLI's own: where the binaries come from (`--bin-dir`, else beside this one), the
//! server a worker registers with (the global `--server`, saved before the daemon starts), and
//! the wait until the daemon answers, with a word on how clients will find it. The worker's
//! data dir, reach, port and log level are baked into its definitions at install time; its
//! sockets live under `<data dir>/run/` so the CLI can find them without the manager's
//! environment.
//!
//! An install over a worker keeps the running `slopty-ptyd`, and every shell and agent turn it
//! holds, unless the new build keeps custody another way ([`platform::Ptyd`]). Ending sessions
//! takes `--end-sessions`; `--plan` says beforehand what the install would do, which is how a
//! deploy asks the person first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use slopty_net::HostAddr;
use slopty_net::endpoint::SERVER_PORT;
use slopty_platform::service::{
    self as platform, Layout, Manager, PTYD, Ptyd, SERVER, Session, WORKER, WorkerOpts,
};
use slopty_proto::ctl::{CtlReply, CtlRequest, Health};
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
    /// Bring this machine to this build: the worker installed here is replaced, its port and
    /// address carrying over and the previous one put back if the new one does not come up;
    /// with none installed, one is (`worker deploy --update` and the app run this).
    #[arg(long, conflicts_with = "fresh")]
    update: bool,
    /// Refuse when a worker is installed here already (`worker deploy` runs this).
    #[arg(long)]
    fresh: bool,
    /// Say what the install would do to `slopty-ptyd` and the sessions it holds, as JSON with
    /// `--json`, and change nothing.
    #[arg(long)]
    plan: bool,
    /// Go on when the new build must restart `slopty-ptyd`, ending every shell and agent turn
    /// it holds. Without it such an install refuses before changing anything.
    #[arg(long)]
    end_sessions: bool,
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
/// `--server`) is saved as the server the worker registers with. With `--plan`, only say what
/// that would do, as JSON with `json`.
pub async fn install(
    opts: &InstallOpts,
    server: Option<&str>,
    data_dir: &Path,
    json: bool,
) -> Result<()> {
    let session = Session::native();
    if opts.plan {
        let source = binaries_source(opts.bin_dir.as_deref())?;
        let plan = session.ptyd_plan(&source, data_dir);
        if json {
            println!("{}", serde_json::to_string(&plan)?);
        } else {
            println!("{plan}");
        }
        return Ok(());
    }
    install_in(&session, opts, server, data_dir, START_TIMEOUT).await
}

/// Why an install that would end `ptyd`'s sessions stopped, before it changed anything.
fn ends_sessions(ptyd: Ptyd) -> anyhow::Error {
    anyhow!("the new build restarts slopty-ptyd ({ptyd}); pass --end-sessions to go on")
}

/// [`install`] in `session`, giving the daemon `within` to answer as the one just installed.
async fn install_in(
    session: &Session,
    opts: &InstallOpts,
    server: Option<&str>,
    data_dir: &Path,
    within: Duration,
) -> Result<()> {
    let source = binaries_source(opts.bin_dir.as_deref())?;
    let installed = session.file(WORKER).is_file();
    if opts.fresh && installed {
        bail!("a worker is installed here already; pass --update to replace it");
    }
    let ptyd = session.ptyd_plan(&source, data_dir);
    if ptyd.ends_sessions() && !opts.end_sessions {
        return Err(ends_sessions(ptyd));
    }
    let mut worker = opts.worker();
    let previous = if opts.update && installed {
        Some(keep_previous(session, data_dir, &mut worker)?)
    } else {
        None
    };
    if let Some(server) = server {
        save_worker_server(data_dir, server)?;
    }
    let registers_with =
        slopty_settings::Settings::load(&slopty_settings::path_in(data_dir)).settings.worker.server;
    let started = Instant::now();
    let done = platform::install_worker(session, &worker, &source, data_dir, ptyd)
        .await
        .map_err(install_error)?;
    for (job, path) in &done.definitions {
        println!("installed {}  ({})", job.program, path.display());
    }
    println!("{}", done.ptyd);
    let socket = Layout::new(data_dir).worker_socket();
    let expected = done.bin_dir.join(WORKER.program);
    let name = match come_up(&socket, within, |h| is_the_new_one(h, &expected, started)).await {
        Ok(name) => name,
        Err(e) => {
            let logs = session.logs(WORKER);
            let Some(previous) = previous else {
                bail!("daemon did not come up ({e:#}); see {logs}");
            };
            println!("the new worker did not come up ({e:#}); putting the previous one back");
            // Restoring: whatever it does to ptyd, the previous worker is what runs next.
            let ptyd = session.ptyd_plan(&previous.dir, data_dir);
            platform::install_worker(session, &previous.opts, &previous.dir, data_dir, ptyd)
                .await
                .map_err(install_error)?;
            return match come_up(&socket, within, |_| Ok(())).await {
                Ok(_) => Err(anyhow!(
                    "the new worker did not come up ({e:#}); the previous one is back. See {logs}"
                )),
                Err(again) => Err(anyhow!(
                    "the new worker did not come up ({e:#}), and the previous one did not either ({again:#}); see {logs}"
                )),
            };
        }
    };
    match &registers_with {
        Some(server) => println!(
            "\n{name} is up and registers with {server}; every client of that server lists it"
        ),
        None => println!(
            "\n{name} is up on its own (pass --server to register it); add it from a client \
             with `slopty add <this machine's tailnet name or IP>` or the app's \"Add a machine\""
        ),
    }
    if let Some(note) = session.keep_running() {
        println!("{note}");
    }
    Ok(())
}

/// The worker an update replaces: where its binaries were kept, and how it ran.
#[derive(Debug)]
struct Previous {
    dir: PathBuf,
    opts: WorkerOpts,
}

/// Copy the installed worker's binaries to `<data dir>/bin.previous` and read how it runs;
/// `worker` takes its port and address unless given its own.
fn keep_previous(session: &Session, data_dir: &Path, worker: &mut WorkerOpts) -> Result<Previous> {
    let definition = session.file(WORKER);
    let args = platform::installed_args(session.manager, &definition)
        .with_context(|| format!("read {}", definition.display()))?;
    let from = args
        .first()
        .and_then(|program| Path::new(program).parent())
        .with_context(|| format!("no program in {}", definition.display()))?;
    let opts = WorkerOpts {
        port: flag_value(&args, "--port").and_then(|p| p.parse().ok()),
        bind: flag_value(&args, "--bind").and_then(|ip| ip.parse().ok()),
        log: worker.log.clone(),
    };
    worker.port = worker.port.or(opts.port);
    worker.bind = worker.bind.or(opts.bind);
    let dir = data_dir.join("bin.previous");
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    for name in platform::WORKER_BINARIES {
        let (src, dst) = (from.join(name), dir.join(name));
        std::fs::copy(&src, &dst)
            .with_context(|| format!("keep {} as {}", src.display(), dst.display()))?;
    }
    Ok(Previous { dir, opts })
}

/// Until the worker at `socket` answers its status and a doctor that `check` accepts, for
/// `within`; its name. A doctor `check` refuses ends the wait at once.
async fn come_up(
    socket: &Path,
    within: Duration,
    check: impl Fn(&Health) -> Result<()>,
) -> Result<String> {
    let started = Instant::now();
    loop {
        let answered = async {
            let CtlReply::Status { name, .. } =
                workerctl::call_at(socket, CtlRequest::Status).await?
            else {
                bail!("a status that is no status");
            };
            let CtlReply::Doctor(health) = workerctl::call_at(socket, CtlRequest::Doctor).await?
            else {
                bail!("a doctor that is no doctor");
            };
            Ok((name, health))
        };
        match answered.await {
            Ok((name, health)) => return check(&health).map(|()| name),
            Err(e) if started.elapsed() < within => {
                tracing::debug!(error = %e, "waiting for slopty-worker");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Whether `health` is the worker just installed as `expected`: this build's version, run from
/// there, started since `installed`.
fn is_the_new_one(health: &Health, expected: &Path, installed: Instant) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    if health.version != version {
        bail!("it answers as version {}, not {version}", health.version);
    }
    let resolved = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if resolved(Path::new(&health.exe)) != resolved(expected) {
        bail!("it runs {}, not {}", health.exe, expected.display());
    }
    if health.uptime_secs > installed.elapsed().as_secs().saturating_add(1) {
        bail!("it has been up since before the install");
    }
    Ok(())
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

/// One line per service: installed or not, and its pid when it runs; then whether they stop at
/// logout. With `json`, the [`platform::Report`] a deploy reads.
pub fn status(json: bool) -> Result<()> {
    let report = Session::native().report();
    if json {
        println!("{}", serde_json::to_string(&report)?);
        return Ok(());
    }
    println!("{}  {}", PTYD.program, report.ptyd);
    println!("{}  {}", WORKER.program, report.worker);
    if let Some(note) = report.stops_at_logout {
        println!("{note}");
    }
    Ok(())
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
    /// Whether this machine serves as a Tailscale peer relay, and why and how to make it one.
    Relay,
    /// Put the server on another machine over `ssh` (a Mac, or Linux on arm64 or `x86_64`),
    /// or replace the one there, and say where clients reach it.
    Deploy(crate::deploy::ServerDeployOpts),
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
    platform::installed_args(manager, path)
        .as_deref()
        .and_then(|args| flag_value(args, "--port"))
        .and_then(|port| port.parse().ok())
        .unwrap_or(SERVER_PORT)
}

/// The value after `flag` in an argument list.
fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == flag)
        .and_then(|i| argv.get(i.saturating_add(1)))
        .map(String::as_str)
}

/// Dial the server on loopback as an agent and return its name.
async fn probe(port: u16) -> Result<String> {
    let endpoint = slopty_net::client::bind_client()?;
    let address = HostAddr::new("127.0.0.1", port);
    let role = Role::Agent { name: "slopty server".to_owned(), vouch: None };
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
        ServerCmd::Relay => {
            print!("{}", crate::relay::read().await.report());
            Ok(())
        }
        ServerCmd::Deploy(opts) => {
            let source = binaries_source(opts.bin_dir())?;
            let served = crate::deploy::serve(&opts, &source).await?;
            print!("{}", crate::deploy::served(opts.target(), &served));
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
                anyhow!("{e}; build it (cargo build -p slopty-serverd) or pass --bin-dir")
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
                if let Some(note) = session.keep_running() {
                    println!("{note}");
                }
                println!("{}", crate::relay::read().await.line());
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
    println!("{}", crate::relay::read().await.line());
}

/// Where to take binaries from: `--bin-dir`, else this binary's directory.
pub fn binaries_source(bin_dir: Option<&Path>) -> Result<PathBuf> {
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
            update: false,
            fresh: false,
            plan: false,
            end_sessions: false,
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
        assert_eq!(flag_value(&["/bin/slopty-server".to_owned()], "--port"), None);
        assert_eq!(
            flag_value(&["s".to_owned(), "--port".to_owned(), "45561".to_owned()], "--port"),
            Some("45561")
        );
        assert_eq!(installed_port(Manager::Launchd, Path::new("/nonexistent.plist")), SERVER_PORT);
    }

    /// A missing binary names the flag that points elsewhere.
    #[test]
    fn a_missing_binary_points_at_bin_dir() {
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "/x/slopty-ptyd not found");
        assert_eq!(install_error(e).to_string(), "/x/slopty-ptyd not found; pass --bin-dir");
    }

    /// Records the manager's commands and says no service is loaded, as a clean launchd would,
    /// with the user logged in: their GUI domain (`gui/<uid>`) prints.
    #[derive(Debug)]
    struct Recorder(std::sync::mpsc::Sender<String>);

    /// Whether `args` print a service (`print gui/501/<label>`) rather than a domain.
    fn prints_a_service(args: &[&str]) -> bool {
        matches!(args, ["print", target] if target.matches('/').count() > 1)
    }

    impl platform::Runner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<String> {
            let _sent = self.0.send(format!("{program} {}", args.join(" ")));
            if prints_a_service(args) {
                return Err(std::io::Error::other("Could not find service"));
            }
            Ok(String::new())
        }
    }

    /// A launchd session in a temporary home, a data dir beside it, and the worker's control
    /// socket answering status as "studio" and doctor with what `health` holds at the time.
    struct Stage {
        dir: tempfile::TempDir,
        session: Session,
        commands: std::sync::mpsc::Receiver<String>,
        health: tokio::sync::watch::Sender<Health>,
    }

    /// [`Recorder`], except that ptyd runs as pid 700 with two sessions, wrote custody `old`
    /// beside its socket, and the new `slopty-ptyd` says `new`.
    #[derive(Debug)]
    struct Busy(std::sync::mpsc::Sender<String>);

    impl platform::Runner for Busy {
        fn run(&self, program: &str, args: &[&str]) -> std::io::Result<String> {
            let _sent = self.0.send(format!("{program} {}", args.join(" ")));
            match (program, args) {
                (_, ["--custody"]) => Ok("new\n".to_owned()),
                ("ps", _) => Ok("700\n700\n1\n".to_owned()),
                ("launchctl", ["print", target]) if target.ends_with(PTYD.label) => {
                    Ok("\tpid = 700\n".to_owned())
                }
                _ if prints_a_service(args) => Err(std::io::Error::other("Could not find service")),
                _ => Ok(String::new()),
            }
        }
    }

    impl Stage {
        fn new() -> Self {
            Self::on(|tx| std::sync::Arc::new(Recorder(tx)))
        }

        fn on(
            runner: impl FnOnce(std::sync::mpsc::Sender<String>) -> std::sync::Arc<dyn platform::Runner>,
        ) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().join("home");
            let (tx, commands) = std::sync::mpsc::channel();
            let session = Session {
                manager: Manager::Launchd,
                definitions: home.join("Library").join("LaunchAgents"),
                home,
                uid: 501,
                runner: runner(tx),
            };
            let run = dir.path().join("data").join("run");
            std::fs::create_dir_all(&run).unwrap();
            let listener = tokio::net::UnixListener::bind(run.join("worker.sock")).unwrap();
            let (health, answers) = tokio::sync::watch::channel(Self::health("?", 0));
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                while let Ok((stream, _addr)) = listener.accept().await {
                    let (rd, mut wr) = stream.into_split();
                    let mut line = String::new();
                    tokio::io::BufReader::new(rd).read_line(&mut line).await.unwrap();
                    let reply = match serde_json::from_str(&line).unwrap() {
                        CtlRequest::Doctor => CtlReply::Doctor(Box::new(answers.borrow().clone())),
                        _ => CtlReply::Status {
                            id: slopty_core::WorkerId::new(),
                            name: "studio".to_owned(),
                            sessions: Vec::new(),
                        },
                    };
                    let mut out = serde_json::to_vec(&reply).unwrap();
                    out.push(b'\n');
                    wr.write_all(&out).await.unwrap();
                }
            });
            Self { dir, session, commands, health }
        }

        fn data(&self) -> PathBuf {
            self.dir.path().join("data")
        }

        fn health(exe: &str, uptime_secs: u64) -> Health {
            Health {
                worker: slopty_core::WorkerId::nil(),
                server: None,
                version: env!("CARGO_PKG_VERSION").to_owned(),
                exe: exe.to_owned(),
                caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
                listen: "[::]:45550".to_owned(),
                allow: Vec::new(),
                tailscale: slopty_proto::ctl::Tailscale::Absent,
                pasteboard: slopty_proto::ctl::PasteboardAccess::Allowed,
                clients: 0,
                sessions: 0,
                uptime_secs,
            }
        }

        /// The doctor answers as the worker installed from the data dir's `bin`.
        fn answer_as_installed(&self) {
            let exe = self.data().join("bin").join(WORKER.program);
            self.health.send_replace(Self::health(&exe.to_string_lossy(), 0));
        }

        /// Three binaries holding `tag`, in a directory of their own.
        fn binaries(&self, tag: &str) -> PathBuf {
            let dir = self.dir.path().join(tag);
            std::fs::create_dir_all(&dir).unwrap();
            for name in platform::WORKER_BINARIES {
                std::fs::write(dir.join(name), format!("{name} {tag}")).unwrap();
            }
            dir
        }

        fn opts(from: &Path, update: bool, fresh: bool) -> InstallOpts {
            InstallOpts {
                bin_dir: Some(from.to_path_buf()),
                update,
                fresh,
                ..InstallOpts::default()
            }
        }

        async fn install(&self, opts: &InstallOpts) -> Result<()> {
            let within = Duration::from_secs(2);
            install_in(&self.session, opts, None, &self.data(), within).await
        }

        fn installed(&self, name: &str) -> String {
            std::fs::read_to_string(self.data().join("bin").join(name)).unwrap()
        }

        fn bootstraps(&self) -> usize {
            self.commands.try_iter().filter(|c| c.starts_with("launchctl bootstrap")).count()
        }
    }

    /// `--update` installs where nothing is, `--fresh` then leaves the installed worker alone,
    /// and an update keeps the previous binaries and the port the worker ran on.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_keeps_the_previous_worker_and_its_port() {
        let stage = Stage::new();
        let old = stage.binaries("old");
        stage.answer_as_installed();
        let first = InstallOpts { port: Some(45551), ..Stage::opts(&old, true, false) };
        stage.install(&first).await.unwrap();
        assert_eq!(stage.bootstraps(), 2, "ptyd and the worker, installed fresh");
        assert!(!stage.data().join("bin.previous").exists(), "nothing to keep");
        let fresh = Stage::opts(&old, false, true);
        let again = stage.install(&fresh).await.unwrap_err();
        assert!(again.to_string().contains("pass --update"), "{again:#}");

        let new = stage.binaries("new");
        stage.install(&Stage::opts(&new, true, false)).await.unwrap();
        assert_eq!(stage.installed("slopty-worker"), "slopty-worker new");
        let kept = stage.data().join("bin.previous").join("slopty-worker");
        assert_eq!(std::fs::read_to_string(kept).unwrap(), "slopty-worker old");
        let args = platform::installed_args(Manager::Launchd, &stage.session.file(WORKER)).unwrap();
        assert_eq!(flag_value(&args, "--port"), Some("45551"), "the port carried over: {args:?}");
    }

    /// A new worker that answers as some other build, or one that was up before the install,
    /// is not the one installed: an update puts the previous binaries back and says so.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_that_does_not_come_up_puts_the_previous_worker_back() {
        let stage = Stage::new();
        stage.answer_as_installed();
        stage.install(&Stage::opts(&stage.binaries("old"), false, true)).await.unwrap();
        let _earlier = stage.bootstraps();

        let exe = stage.data().join("bin").join(WORKER.program).to_string_lossy().into_owned();
        let broken = Health { version: "0.0.0-broken".to_owned(), ..Stage::health(&exe, 0) };
        stage.health.send_replace(broken);
        let new = stage.binaries("new");
        let failed = stage.install(&Stage::opts(&new, true, false)).await.unwrap_err();
        let said = format!("{failed:#}");
        assert!(said.contains("answers as version 0.0.0-broken"), "{said}");
        assert!(said.contains("the previous one is back"), "{said}");
        assert_eq!(stage.installed("slopty-worker"), "slopty-worker old");
        assert_eq!(stage.installed("slopty"), "slopty old");
        assert_eq!(stage.bootstraps(), 4, "the new pair, then the previous pair");

        // A worker up since before the install is the old process, not the new one.
        stage.health.send_replace(Stage::health(&exe, 3600));
        let stale = stage.install(&Stage::opts(&new, true, false)).await.unwrap_err();
        assert!(format!("{stale:#}").contains("up since before the install"), "{stale:#}");
        assert_eq!(stage.installed("slopty-worker"), "slopty-worker old");
    }

    /// A new build that must restart ptyd, which holds two sessions, refuses before changing
    /// anything unless told to end them, and says how; told, it goes on and restarts ptyd.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_install_that_ends_sessions_needs_end_sessions() {
        let stage = Stage::on(|tx| std::sync::Arc::new(Busy(tx)));
        std::fs::write(Layout::new(&stage.data()).ptyd_custody(), "700 old\n").unwrap();
        stage.answer_as_installed();
        let new = stage.binaries("new");
        let refused = stage.install(&Stage::opts(&new, true, false)).await.unwrap_err();
        let said = format!("{refused:#}");
        assert!(said.contains("ending the 2 sessions it holds"), "{said}");
        assert!(said.contains("pass --end-sessions"), "{said}");
        let asked: Vec<String> = stage.commands.try_iter().collect();
        assert!(
            !asked.iter().any(|c| c.contains("bootout") || c.contains("bootstrap")),
            "nothing stopped or started: {asked:?}"
        );
        assert!(!stage.data().join("bin").exists(), "nothing copied");

        let told = InstallOpts { end_sessions: true, ..Stage::opts(&new, true, false) };
        stage.install(&told).await.unwrap();
        let asked: Vec<String> = stage.commands.try_iter().collect();
        assert!(
            asked.iter().any(|c| c == "launchctl bootout gui/501/dev.aislopware.slopty.ptyd"),
            "ptyd restarted: {asked:?}"
        );
    }
}
