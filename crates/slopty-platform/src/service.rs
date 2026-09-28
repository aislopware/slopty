//! The daemons the user's session keeps running, installed and removed.
//!
//! They are `LaunchAgents` on macOS and systemd user units on Linux ([`Manager`]). A worker is two
//! of them, `slopty-ptyd` (the PTY custodian, which keeps shells alive across daemon restarts) and
//! `slopty-worker`; the server is a third. Each restarts when it dies and starts at every login.
//!
//! [`install_worker`] is what `slopty worker install` and the app's "Use this Mac as a worker"
//! both run: stop the services, put the binaries where they will run from, write the definitions
//! and start them again. Waiting for the daemon to answer is the caller's, over the worker's
//! control socket ([`Layout::worker_socket`]). Every file an install writes goes under its
//! [`Session`]'s home, data directory and definitions directory, and every command goes through
//! its [`Runner`], so a test installs into a temporary directory with a runner that records what
//! it was asked, and nothing reaches the real launchd.
//!
//! The daemons run in place when their binaries ship inside an app bundle on the home's volume
//! (`Slopty.app/Contents/MacOS`, which `cargo xtask bundle` fills and signs): one signed path
//! that an update replaces, so a Screen Recording grant made once stays. Anywhere else they are
//! copied to `<data dir>/bin/` first: a dev-tree build gets overwritten by the next
//! `cargo build`, and a binary on an external volume hangs in dyld under launchd (the "removable
//! volume" consent has no one to click it). `docs/decisions/platform.md`, "This Mac as a worker".

use std::fmt::Write as _;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plist::{Dictionary, Value};

/// One daemon to keep running.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Service {
    /// The unit's name without `.service` (`slopty-worker`).
    pub name: String,
    /// One line for `systemctl --user status`.
    pub description: String,
    /// The binary.
    pub program: PathBuf,
    /// Its arguments, passed as they are (no shell, no variable expansion).
    pub args: Vec<String>,
    /// Its environment on top of the user manager's.
    pub env: Vec<(String, String)>,
    /// The services it needs started first (`slopty-ptyd` for the worker), by name.
    pub after: Vec<String>,
}

impl Service {
    /// The unit file's name: `<name>.service`.
    #[must_use]
    pub fn unit_name(&self) -> String {
        format!("{}.service", self.name)
    }

    /// The systemd user unit: restarted a second after it dies, started with the user's
    /// session (`default.target`).
    #[must_use]
    pub fn systemd_unit(&self) -> String {
        let mut unit = format!("[Unit]\nDescription={}\n", self.description.replace('\n', " "));
        for dep in &self.after {
            let _infallible = writeln!(unit, "Wants={dep}.service\nAfter={dep}.service");
        }
        let program = self.program.to_string_lossy();
        let exec: Vec<String> = std::iter::once(program.as_ref())
            .chain(self.args.iter().map(String::as_str))
            .map(|word| quote(word, true))
            .collect();
        let _infallible = write!(unit, "\n[Service]\nExecStart={}\n", exec.join(" "));
        for (name, value) in &self.env {
            let _infallible =
                writeln!(unit, "Environment={}", quote(&format!("{name}={value}"), false));
        }
        unit.push_str("Restart=always\nRestartSec=1\n\n[Install]\nWantedBy=default.target\n");
        unit
    }
}

/// A word in systemd's double-quoted syntax: `\` and `"` escaped, `%` doubled (specifiers are
/// expanded everywhere), and `$` doubled where the line is `ExecStart=` (variables are expanded
/// there only).
fn quote(word: &str, exec: bool) -> String {
    let mut out = String::with_capacity(word.len().saturating_add(2));
    out.push('"');
    for c in word.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One daemon an installation runs, as each manager names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Job {
    /// Its launchd label.
    pub label: &'static str,
    /// Its binary, which is also its systemd unit's name.
    pub program: &'static str,
    /// One line for the manager's status.
    pub description: &'static str,
}

/// The PTY custodian.
pub const PTYD: Job = Job {
    label: "dev.aislopware.slopty.ptyd",
    program: "slopty-ptyd",
    description: "Slopty PTY custodian",
};
/// The worker daemon.
pub const WORKER: Job = Job {
    label: "dev.aislopware.slopty.worker",
    program: "slopty-worker",
    description: "Slopty worker",
};
/// The server.
pub const SERVER: Job = Job {
    label: "dev.aislopware.slopty.server",
    program: "slopty-server",
    description: "Slopty server",
};

/// The binaries a worker's installation carries: its two daemons, and the CLI its sessions'
/// agent hooks run.
pub const WORKER_BINARIES: [&str; 3] = ["slopty-ptyd", "slopty-worker", "slopty"];

/// The session's service manager.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Manager {
    /// macOS: `LaunchAgents` in the user's GUI domain.
    Launchd,
    /// Linux: the systemd user manager.
    Systemd,
}

impl Manager {
    /// The manager of the platform this build serves.
    pub const NATIVE: Self = if cfg!(target_os = "linux") { Self::Systemd } else { Self::Launchd };
}

/// Runs the service manager's commands: [`System`] for real, a stand-in under test.
pub trait Runner: Send + Sync + std::fmt::Debug {
    /// `program args…`, its standard output when it succeeds.
    ///
    /// # Errors
    ///
    /// When it cannot be started, or it exits unsuccessfully (with its standard error).
    fn run(&self, program: &str, args: &[&str]) -> io::Result<String>;
}

/// The commands themselves.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

impl Runner for System {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| context(&e, format_args!("run {program}")))?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "{program} {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Whether a service is installed, and its process when it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// The manager runs it as this process.
    Running(u32),
    /// Defined, not running.
    Stopped,
    /// Not defined.
    Absent,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running(pid) => write!(f, "running (pid {pid})"),
            Self::Stopped => f.write_str("installed, not running"),
            Self::Absent => f.write_str("not installed"),
        }
    }
}

/// Where services are installed and as whom: the manager, the user's home and uid, and what
/// runs the manager's commands.
#[derive(Clone, Debug)]
pub struct Session {
    /// The manager.
    pub manager: Manager,
    /// The user's home: where the `LaunchAgents`' logs go and where they start.
    pub home: PathBuf,
    /// Where the definitions go: `~/Library/LaunchAgents`, or `~/.config/systemd/user`.
    pub definitions: PathBuf,
    /// The user whose GUI domain a `LaunchAgent` is loaded into.
    pub uid: u32,
    /// What runs `launchctl` and `systemctl`.
    pub runner: Arc<dyn Runner>,
}

impl Session {
    /// This user's session with the platform's manager, as the environment places it, and the
    /// real commands. The systemd definitions honour an absolute `$XDG_CONFIG_HOME`.
    #[must_use]
    pub fn native() -> Self {
        let home = crate::dirs::home();
        let definitions = match Manager::NATIVE {
            Manager::Launchd => home.join("Library").join("LaunchAgents"),
            Manager::Systemd => std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
                .join("systemd")
                .join("user"),
        };
        Self {
            manager: Manager::NATIVE,
            home,
            definitions,
            uid: rustix::process::getuid().as_raw(),
            runner: Arc::new(System),
        }
    }

    /// The file `job` is defined in.
    #[must_use]
    pub fn file(&self, job: Job) -> PathBuf {
        match self.manager {
            Manager::Launchd => self.definitions.join(format!("{}.plist", job.label)),
            Manager::Systemd => self.definitions.join(format!("{}.service", job.program)),
        }
    }

    /// `~/Library/Logs/Slopty/<program>.log`, where a `LaunchAgent`'s output goes.
    #[must_use]
    pub fn log_file(&self, program: &str) -> PathBuf {
        self.home.join("Library").join("Logs").join("Slopty").join(format!("{program}.log"))
    }

    /// Where `job`'s output goes, for a message.
    #[must_use]
    pub fn logs(&self, job: Job) -> String {
        match self.manager {
            Manager::Launchd => self.log_file(job.program).display().to_string(),
            Manager::Systemd => format!("journalctl --user -u {}", job.program),
        }
    }

    /// What the reader is told after an install. The systemd user manager stops a user's
    /// services at their last logout unless it lingers, and a worker must outlive its SSH
    /// session.
    #[must_use]
    pub fn install_note(&self) -> Option<String> {
        match self.manager {
            Manager::Launchd => None,
            Manager::Systemd => {
                let user = std::env::var("USER").unwrap_or_default();
                let lingers = Path::new("/var/lib/systemd/linger").join(&user).exists();
                (!lingers).then(|| {
                    format!(
                        "run `loginctl enable-linger {user}` so it runs while you are logged out"
                    )
                })
            }
        }
    }

    /// `service` as this manager's definition of `job`. `gui`: it needs the login session's
    /// window server (launchd's `Aqua` session type).
    ///
    /// # Errors
    ///
    /// When the plist cannot be written out.
    pub fn render(&self, job: Job, service: &Service, gui: bool) -> io::Result<Vec<u8>> {
        match self.manager {
            Manager::Launchd => {
                let mut bytes = Vec::new();
                plist::to_writer_xml(&mut bytes, &self.launch_agent(job, service, gui))
                    .map_err(|e| io::Error::other(format!("plist for {}: {e}", job.label)))?;
                Ok(bytes)
            }
            Manager::Systemd => Ok(service.systemd_unit().into_bytes()),
        }
    }

    /// `service` as a `LaunchAgent`: restarted when it dies and at login, its output in
    /// `~/Library/Logs/Slopty/<program>.log`. `gui` puts it in the login session
    /// (`LimitLoadToSessionType Aqua`), where `slopty-worker` reaches ScreenCaptureKit and the
    /// window server.
    ///
    /// `ProcessType Interactive` keeps it out of App Nap and background quality of service,
    /// where every answer would wait on a throttled timer; `ThrottleInterval` makes a crash loop
    /// restart every 2 s rather than launchd's 10 s default.
    fn launch_agent(&self, job: Job, service: &Service, gui: bool) -> Value {
        let path = |p: &Path| p.to_string_lossy().into_owned();
        let mut d = Dictionary::new();
        d.insert("Label".into(), Value::String(job.label.to_owned()));
        let mut argv = vec![Value::String(path(&service.program))];
        argv.extend(service.args.iter().cloned().map(Value::String));
        d.insert("ProgramArguments".into(), Value::Array(argv));
        let mut vars = Dictionary::new();
        for (k, v) in &service.env {
            vars.insert(k.clone(), Value::String(v.clone()));
        }
        d.insert("EnvironmentVariables".into(), Value::Dictionary(vars));
        d.insert("RunAtLoad".into(), Value::Boolean(true));
        d.insert("KeepAlive".into(), Value::Boolean(true));
        d.insert("ThrottleInterval".into(), Value::Integer(2.into()));
        d.insert("ProcessType".into(), Value::String("Interactive".into()));
        d.insert("WorkingDirectory".into(), Value::String(path(&self.home)));
        let log = self.log_file(job.program);
        d.insert("StandardOutPath".into(), Value::String(path(&log)));
        d.insert("StandardErrorPath".into(), Value::String(path(&log)));
        if gui {
            d.insert("LimitLoadToSessionType".into(), Value::String("Aqua".into()));
        }
        Value::Dictionary(d)
    }

    fn launchctl(&self, args: &[&str]) -> io::Result<String> {
        self.runner.run("launchctl", args)
    }

    fn systemctl(&self, args: &[&str]) -> io::Result<String> {
        let user: Vec<&str> = std::iter::once("--user").chain(args.iter().copied()).collect();
        self.runner.run("systemctl", &user)
    }

    /// `gui/<uid>/<label>`: `job` in this user's GUI domain.
    fn target(&self, job: Job) -> String {
        format!("gui/{}/{}", self.uid, job.label)
    }

    /// Load `job`'s written definition and start it.
    fn start(&self, job: Job) -> io::Result<()> {
        match self.manager {
            Manager::Launchd => {
                let domain = format!("gui/{}", self.uid);
                let path = self.file(job);
                self.launchctl(&["bootstrap", &domain, &path.to_string_lossy()]).map(drop)
            }
            Manager::Systemd => {
                let unit = format!("{}.service", job.program);
                self.systemctl(&["daemon-reload"])?;
                self.systemctl(&["enable", &unit])?;
                self.systemctl(&["restart", &unit]).map(drop)
            }
        }
    }

    /// Stop `job` if it runs, and return once the manager has let go of it; not running is not
    /// an error. `disable`: it does not come back at login either.
    async fn stop(&self, job: Job, disable: bool) {
        match self.manager {
            Manager::Launchd => self.bootout(job).await,
            Manager::Systemd => {
                let unit = format!("{}.service", job.program);
                let verb = if disable { ["disable", "--now"].as_slice() } else { &["stop"] };
                let args: Vec<&str> = verb.iter().copied().chain([unit.as_str()]).collect();
                if let Err(e) = self.systemctl(&args) {
                    tracing::debug!(unit, error = %e, "stop");
                }
            }
        }
    }

    /// Unload an agent if it is loaded, and return once launchd no longer knows it; not being
    /// loaded is not an error.
    ///
    /// `launchctl bootout` returns while the job is still being torn down, and a `bootstrap` of
    /// the same label in that window fails with "5: Input/output error": `slopty server
    /// install` failed that way on every reinstall.
    async fn bootout(&self, job: Job) {
        let target = self.target(job);
        if let Err(e) = self.launchctl(&["bootout", &target]) {
            tracing::debug!(label = job.label, error = %e, "bootout");
        }
        let started = Instant::now();
        while self.launchctl(&["print", &target]).is_ok() && started.elapsed() < BOOTOUT_TIMEOUT {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Restart `job` now, as its definition says. The worker's sessions live on in
    /// `slopty-ptyd`, so this costs its links a redial and nothing more; a daemon reads a
    /// Screen Recording grant made while it ran only once it starts again.
    ///
    /// # Errors
    ///
    /// When the manager refuses: `job` is not loaded, say.
    pub fn restart(&self, job: Job) -> io::Result<()> {
        match self.manager {
            Manager::Launchd => self.launchctl(&["kickstart", "-k", &self.target(job)]).map(drop),
            Manager::Systemd => {
                self.systemctl(&["restart", &format!("{}.service", job.program)]).map(drop)
            }
        }
    }

    /// The process the manager runs `job` as, when it runs.
    #[must_use]
    pub fn pid(&self, job: Job) -> Option<u32> {
        match self.manager {
            Manager::Launchd => {
                self.launchctl(&["print", &self.target(job)]).ok().and_then(|out| launchd_pid(&out))
            }
            Manager::Systemd => {
                let unit = format!("{}.service", job.program);
                self.systemctl(&["show", "--property=MainPID", "--value", &unit])
                    .ok()
                    .and_then(|out| out.trim().parse().ok())
                    .filter(|pid| *pid != 0)
            }
        }
    }

    /// Whether `job` is installed, and its process when it runs.
    #[must_use]
    pub fn state(&self, job: Job) -> State {
        match (self.file(job).is_file(), self.pid(job)) {
            (_, Some(pid)) => State::Running(pid),
            (true, None) => State::Stopped,
            (false, None) => State::Absent,
        }
    }

    /// Write `job`'s definition.
    fn write_definition(&self, job: Job, service: &Service, gui: bool) -> io::Result<PathBuf> {
        let path = self.file(job);
        std::fs::write(&path, self.render(job, service, gui)?)
            .map_err(|e| context(&e, format_args!("write {}", path.display())))?;
        Ok(path)
    }

    /// Make `dirs`, the definitions' directory and, for launchd, the logs'.
    fn make_dirs(&self, dirs: &[&Path]) -> io::Result<()> {
        let mut all: Vec<PathBuf> = dirs.iter().map(|d| d.to_path_buf()).collect();
        all.push(self.definitions.clone());
        if self.manager == Manager::Launchd {
            all.extend(self.log_file("slopty").parent().map(Path::to_path_buf));
        }
        for dir in all {
            std::fs::create_dir_all(&dir)
                .map_err(|e| context(&e, format_args!("mkdir {}", dir.display())))?;
        }
        Ok(())
    }

    /// Stop `job`, keep it from coming back at login, and remove its definition; whether it
    /// was installed.
    ///
    /// # Errors
    ///
    /// When the definition is there and cannot be removed.
    pub async fn remove(&self, job: Job) -> io::Result<bool> {
        self.stop(job, true).await;
        let path = self.file(job);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(context(&e, format_args!("remove {}", path.display()))),
        }
    }
}

/// How long [`Session::bootout`] waits for launchd to let go of an agent.
const BOOTOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// `e` with what was being done in front of it.
fn context(e: &io::Error, what: impl std::fmt::Display) -> io::Error {
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// The `pid = N` line of `launchctl print`.
fn launchd_pid(out: &str) -> Option<u32> {
    out.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("pid = ").and_then(|rest| rest.trim().parse().ok())
    })
}

/// The command line in a definition: a plist's `ProgramArguments`, a unit's `ExecStart=` (whose
/// words [`Service::systemd_unit`] writes each in double quotes).
#[must_use]
pub fn installed_args(manager: Manager, path: &Path) -> Option<Vec<String>> {
    match manager {
        Manager::Launchd => Value::from_file(path).ok().and_then(|v| {
            let args = v.as_dictionary()?.get("ProgramArguments")?.as_array()?.clone();
            Some(args.into_iter().filter_map(Value::into_string).collect())
        }),
        Manager::Systemd => {
            let unit = std::fs::read_to_string(path).ok()?;
            let exec = unit.lines().find_map(|line| line.strip_prefix("ExecStart="))?;
            Some(exec.split('"').skip(1).step_by(2).map(str::to_owned).collect())
        }
    }
}

/// Where an installation's files go under its data directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Layout {
    data: PathBuf,
}

impl Layout {
    /// The layout under `data_dir`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self { data: data_dir.to_path_buf() }
    }

    /// `<data>/run`: the daemons' sockets, found there by the CLI and the app without the
    /// manager's environment.
    #[must_use]
    pub fn run(&self) -> PathBuf {
        self.data.join("run")
    }

    /// `<data>/bin`: where copied daemons run from.
    #[must_use]
    pub fn bin(&self) -> PathBuf {
        self.data.join("bin")
    }

    /// The PTY custodian's socket.
    #[must_use]
    pub fn ptyd_socket(&self) -> PathBuf {
        self.run().join("ptyd.sock")
    }

    /// The worker's control socket, where `doctor` is asked.
    #[must_use]
    pub fn worker_socket(&self) -> PathBuf {
        self.run().join("worker.sock")
    }
}

/// One request line to a daemon's control socket at `socket`, and its one reply line.
///
/// The request is a line of JSON, newline included. Its side is shut once it is written, the
/// reply side kept open until the answer. The daemon answers from the request's line, so it may
/// answer and close before that side is shut; the answer is read all the same.
///
/// # Errors
///
/// When nothing listens there, or the exchange breaks off before an answer.
pub async fn ask(socket: &Path, request: &[u8]) -> io::Result<String> {
    ask_on(tokio::net::UnixStream::connect(socket).await?, request).await
}

/// [`ask`] on a connected `stream`.
async fn ask_on(stream: tokio::net::UnixStream, request: &[u8]) -> io::Result<String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    let (rd, mut wr) = stream.into_split();
    // A peer that has answered and closed fails the write (`EPIPE`) or the shutdown
    // (`ENOTCONN` on macOS), whichever its close beats; its answer is in our receive buffer.
    let sent = async {
        wr.write_all(request).await?;
        wr.shutdown().await
    }
    .await;
    let mut reply = String::new();
    let read = BufReader::new(rd).read_line(&mut reply).await;
    match sent {
        Err(e) if reply.trim().is_empty() => Err(e),
        _ => read.map(|_| reply.trim().to_owned()),
    }
}

/// How the worker runs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkerOpts {
    /// Its UDP port; the daemon's fixed one when `None`.
    pub port: Option<u16>,
    /// The one IP it listens on and reaches nothing off, for a shaped measurement.
    pub bind: Option<IpAddr>,
    /// `RUST_LOG` for both daemons.
    pub log: String,
}

impl Default for WorkerOpts {
    fn default() -> Self {
        Self { port: None, bind: None, log: "info".to_owned() }
    }
}

/// What every Slopty service shares: `program args…` out of `bin_dir` with `RUST_LOG` and
/// `SLOPTY_DATA_DIR` (plus `env`) in its environment.
fn service(
    job: Job,
    bin_dir: &Path,
    args: Vec<String>,
    log: &str,
    data_dir: &Path,
    env: Vec<(String, String)>,
) -> Service {
    let mut vars = vec![
        ("RUST_LOG".to_owned(), log.to_owned()),
        ("SLOPTY_DATA_DIR".to_owned(), data_dir.to_string_lossy().into_owned()),
    ];
    vars.extend(env);
    Service {
        name: job.program.to_owned(),
        description: job.description.to_owned(),
        program: bin_dir.join(job.program),
        args,
        env: vars,
        after: Vec::new(),
    }
}

/// The worker's two services for `opts`, in start order: the worker after ptyd.
#[must_use]
pub fn worker_services(opts: &WorkerOpts, bin_dir: &Path, data_dir: &Path) -> Vec<(Job, Service)> {
    let layout = Layout::new(data_dir);
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let sockets = || {
        vec![
            ("SLOPTY_PTYD_SOCKET".to_owned(), path(&layout.ptyd_socket())),
            ("SLOPTY_WORKER_SOCKET".to_owned(), path(&layout.worker_socket())),
        ]
    };
    let mut worker_args = vec!["--installed".to_owned()];
    if let Some(port) = opts.port {
        worker_args.push("--port".to_owned());
        worker_args.push(port.to_string());
    }
    if let Some(ip) = opts.bind {
        worker_args.push("--bind".to_owned());
        worker_args.push(ip.to_string());
    }
    let ptyd = service(PTYD, bin_dir, Vec::new(), &opts.log, data_dir, sockets());
    let mut worker = service(WORKER, bin_dir, worker_args, &opts.log, data_dir, sockets());
    worker.after.push(PTYD.program.to_owned());
    vec![(PTYD, ptyd), (WORKER, worker)]
}

/// The server's service: `slopty-server [--port N]` out of `bin_dir`.
#[must_use]
pub fn server_service(port: Option<u16>, log: &str, bin_dir: &Path, data_dir: &Path) -> Service {
    let args = port.map(|p| vec!["--port".to_owned(), p.to_string()]).unwrap_or_default();
    service(SERVER, bin_dir, args, log, data_dir, Vec::new())
}

/// The directory this process's binary is in, resolved: where an install takes the daemons
/// from unless told otherwise. Inside the app bundle it is `Slopty.app/Contents/MacOS`.
///
/// # Errors
///
/// When the running binary's path cannot be read or resolved.
pub fn sibling_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| io::Error::other("the binary has no directory"))?;
    dir.canonicalize().map_err(|e| context(&e, dir.display()))
}

/// Where the daemons from `source` run: `source` itself when it is an app bundle's
/// `Contents/MacOS` on the volume `home` is on, else `<data_dir>/bin`, where they are copied.
#[must_use]
pub fn run_dir(source: &Path, data_dir: &Path, home: &Path) -> PathBuf {
    if in_bundle(source) && same_volume(source, home) {
        source.to_path_buf()
    } else {
        Layout::new(data_dir).bin()
    }
}

/// Whether `dir` is an app bundle's `Contents/MacOS`.
fn in_bundle(dir: &Path) -> bool {
    dir.ends_with("Contents/MacOS")
        && dir
            .parent()
            .and_then(Path::parent)
            .is_some_and(|app| app.extension().is_some_and(|ext| ext == "app"))
}

/// Whether `a` and `b` are on one volume.
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    matches!((std::fs::metadata(a), std::fs::metadata(b)), (Ok(a), Ok(b)) if a.dev() == b.dev())
}

/// Copy `names` from `source` into `bin_dir`, unless they run where they are.
fn copy_binaries(names: &[&str], source: &Path, bin_dir: &Path) -> io::Result<()> {
    if bin_dir == source {
        return Ok(());
    }
    for name in names {
        let (from, to) = (source.join(name), bin_dir.join(name));
        std::fs::copy(&from, &to).map_err(|e| {
            context(&e, format_args!("copy {} to {}", from.display(), to.display()))
        })?;
    }
    Ok(())
}

/// Each of `names` is a file in `source`.
fn check_binaries(names: &[&str], source: &Path) -> io::Result<()> {
    for name in names {
        let path = source.join(name);
        if !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found", path.display()),
            ));
        }
    }
    Ok(())
}

/// What an install put where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Installed {
    /// Where the daemons run from ([`run_dir`]).
    pub bin_dir: PathBuf,
    /// Each service's definition, in start order.
    pub definitions: Vec<(Job, PathBuf)>,
}

/// Install the worker's two services in `session` from the binaries in `source`, and start them.
///
/// It stops both, put the binaries where they run from ([`run_dir`]), write the definitions
/// and start ptyd, then the worker. Idempotent. The worker reads its settings and keeps its
/// sockets under `data_dir`; whether it came up is asked of its control socket
/// ([`Layout::worker_socket`]).
///
/// # Errors
///
/// When a binary is missing from `source` ([`io::ErrorKind::NotFound`]), a file cannot be
/// written, or the manager refuses to start a service.
pub async fn install_worker(
    session: &Session,
    opts: &WorkerOpts,
    source: &Path,
    data_dir: &Path,
) -> io::Result<Installed> {
    check_binaries(&WORKER_BINARIES, source)?;
    let layout = Layout::new(data_dir);
    let bin_dir = run_dir(source, data_dir, &session.home);
    session.make_dirs(&[&layout.run(), &bin_dir])?;
    // Stop first: the copy must not land on a running binary, and a stale socket file makes
    // the daemon's bind fail (the manager would then loop on it).
    for job in [WORKER, PTYD] {
        session.stop(job, false).await;
    }
    copy_binaries(&WORKER_BINARIES, source, &bin_dir)?;
    let mut definitions = Vec::new();
    for (job, service) in worker_services(opts, &bin_dir, data_dir) {
        let path = session.write_definition(job, &service, true)?;
        session.start(job).map_err(|e| context(&e, format_args!("start {}", job.program)))?;
        definitions.push((job, path));
    }
    Ok(Installed { bin_dir, definitions })
}

/// Stop the worker's two services and remove their definitions; each with whether it was
/// installed. Its sessions die with `slopty-ptyd`.
///
/// # Errors
///
/// When a definition is there and cannot be removed.
pub async fn uninstall_worker(session: &Session) -> io::Result<Vec<(Job, bool)>> {
    let mut removed = Vec::new();
    for job in [WORKER, PTYD] {
        removed.push((job, session.remove(job).await?));
    }
    Ok(removed)
}

/// Install the server's service in `session` from `source` and start it, as
/// [`install_worker`] does the worker's; the definition's path.
///
/// # Errors
///
/// When `slopty-server` is missing from `source` ([`io::ErrorKind::NotFound`]), a file cannot
/// be written, or the manager refuses to start it.
pub async fn install_server(
    session: &Session,
    port: Option<u16>,
    log: &str,
    source: &Path,
    data_dir: &Path,
) -> io::Result<PathBuf> {
    let binaries = [SERVER.program];
    check_binaries(&binaries, source)?;
    let bin_dir = run_dir(source, data_dir, &session.home);
    session.make_dirs(&[&bin_dir])?;
    session.stop(SERVER, false).await;
    copy_binaries(&binaries, source, &bin_dir)?;
    let service = server_service(port, log, &bin_dir, data_dir);
    let path = session.write_definition(SERVER, &service, false)?;
    session.start(SERVER).map_err(|e| context(&e, "start slopty-server"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    /// The unit runs the binary with its arguments and environment verbatim, after the services
    /// it needs, restarting it and starting it at login.
    #[test]
    fn a_service_is_written_as_a_systemd_user_unit() {
        let service = Service {
            name: "slopty-worker".into(),
            description: "Slopty worker".into(),
            program: "/home/me/.local/share/slopty/bin/slopty-worker".into(),
            args: vec!["--installed".into(), "--name=50% \"mine\" $HOME".into()],
            env: vec![("SLOPTY_DATA_DIR".into(), "/home/me/a b/%h $x".into())],
            after: vec!["slopty-ptyd".into()],
        };
        assert_eq!(service.unit_name(), "slopty-worker.service", "the unit's file name");
        assert_eq!(
            service.systemd_unit(),
            "[Unit]\n\
             Description=Slopty worker\n\
             Wants=slopty-ptyd.service\n\
             After=slopty-ptyd.service\n\
             \n\
             [Service]\n\
             ExecStart=\"/home/me/.local/share/slopty/bin/slopty-worker\" \"--installed\" \
             \"--name=50%% \\\"mine\\\" $$HOME\"\n\
             Environment=\"SLOPTY_DATA_DIR=/home/me/a b/%%h $x\"\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            "the unit, quoted as systemd reads it"
        );
    }

    /// A stand-in for `launchctl` and `systemctl`: it records each command line and succeeds,
    /// except that nothing is ever loaded (`print` fails), so a bootout returns at once.
    #[derive(Debug)]
    struct Recorder(mpsc::Sender<String>);

    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
            let line = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>();
            let _sent = self.0.send(line.join(" "));
            if args.first() == Some(&"print") {
                return Err(io::Error::other("Could not find service"));
            }
            Ok(String::new())
        }
    }

    /// A launchd session whose home is `home`, uid 501, and what its runner was asked.
    fn stand_in(manager: Manager, home: &Path) -> (Session, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let definitions = match manager {
            Manager::Launchd => home.join("Library").join("LaunchAgents"),
            Manager::Systemd => home.join(".config").join("systemd").join("user"),
        };
        let session = Session {
            manager,
            home: home.to_path_buf(),
            definitions,
            uid: 501,
            runner: Arc::new(Recorder(tx)),
        };
        (session, rx)
    }

    /// `dir` made, with each of `names` in it as a stand-in binary.
    fn binaries(dir: &Path, names: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        for name in names {
            std::fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
    }

    fn program_arguments(plist: &Path) -> Vec<String> {
        installed_args(Manager::Launchd, plist).expect("the plist has its command line")
    }

    /// Installed from an app bundle on the home's volume, the agents run the bundle's own
    /// signed binaries where they are: nothing is copied, and launchd is asked to take out any
    /// old agents, then to load ptyd before the worker.
    #[tokio::test]
    async fn from_a_bundle_the_agents_run_the_bundles_binaries() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let macos = root.path().join("Slopty.app").join("Contents").join("MacOS");
        binaries(&macos, &WORKER_BINARIES);
        let data = root.path().join("data");
        let (session, calls) = stand_in(Manager::Launchd, &home);

        let installed =
            install_worker(&session, &WorkerOpts::default(), &macos, &data).await.unwrap();

        assert_eq!(installed.bin_dir, macos, "run in place");
        assert!(!Layout::new(&data).bin().exists(), "nothing copied out of the bundle");
        let agents = home.join("Library").join("LaunchAgents");
        let ptyd = agents.join("dev.aislopware.slopty.ptyd.plist");
        let worker = agents.join("dev.aislopware.slopty.worker.plist");
        assert_eq!(installed.definitions, [(PTYD, ptyd.clone()), (WORKER, worker.clone())]);
        assert_eq!(
            program_arguments(&worker),
            [macos.join("slopty-worker").display().to_string(), "--installed".to_owned()],
            "the bundle's worker"
        );
        assert_eq!(
            program_arguments(&ptyd),
            [macos.join("slopty-ptyd").display().to_string()],
            "the bundle's ptyd"
        );
        let plist = Value::from_file(&worker).unwrap();
        let d = plist.as_dictionary().unwrap();
        assert_eq!(d["LimitLoadToSessionType"].as_string(), Some("Aqua"), "in the GUI session");
        let env = d["EnvironmentVariables"].as_dictionary().unwrap();
        let socket = data.join("run").join("worker.sock").display().to_string();
        assert_eq!(env["SLOPTY_WORKER_SOCKET"].as_string(), Some(socket.as_str()), "{env:?}");
        assert!(data.join("run").is_dir(), "the sockets' directory is made");
        assert!(home.join("Library/Logs/Slopty").is_dir(), "and the logs'");
        let asked: Vec<String> = calls.try_iter().collect();
        assert_eq!(
            asked,
            [
                "launchctl bootout gui/501/dev.aislopware.slopty.worker".to_owned(),
                "launchctl print gui/501/dev.aislopware.slopty.worker".to_owned(),
                "launchctl bootout gui/501/dev.aislopware.slopty.ptyd".to_owned(),
                "launchctl print gui/501/dev.aislopware.slopty.ptyd".to_owned(),
                format!("launchctl bootstrap gui/501 {}", ptyd.display()),
                format!("launchctl bootstrap gui/501 {}", worker.display()),
            ],
            "stop both, then start ptyd before the worker"
        );
    }

    /// Anywhere else (a dev tree, the CLI beside its daemons) the binaries are copied into the
    /// data directory and the agents run the copies. Uninstalling takes both agents out and
    /// removes their definitions, and a second uninstall finds nothing to remove.
    #[tokio::test]
    async fn outside_a_bundle_the_agents_run_copies_and_uninstall_reverses_it() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = root.path().join("target").join("debug");
        binaries(&source, &WORKER_BINARIES);
        let data = root.path().join("data");
        let (session, calls) = stand_in(Manager::Launchd, &home);
        let opts = WorkerOpts { port: Some(45551), ..WorkerOpts::default() };

        let installed = install_worker(&session, &opts, &source, &data).await.unwrap();

        let bin = data.join("bin");
        assert_eq!(installed.bin_dir, bin, "run from the data directory");
        for name in WORKER_BINARIES {
            assert_eq!(std::fs::read(bin.join(name)).unwrap(), name.as_bytes(), "{name} copied");
        }
        let worker = session.file(WORKER);
        assert_eq!(
            program_arguments(&worker),
            [bin.join("slopty-worker").display().to_string(), "--installed".to_owned()]
                .into_iter()
                .chain(["--port".to_owned(), "45551".to_owned()])
                .collect::<Vec<_>>(),
            "the copy, with the port"
        );
        let _installing: Vec<String> = calls.try_iter().collect();

        let removed = uninstall_worker(&session).await.unwrap();
        assert_eq!(removed, [(WORKER, true), (PTYD, true)], "both were installed");
        assert!(!worker.exists() && !session.file(PTYD).exists(), "the definitions are gone");
        let asked: Vec<String> = calls.try_iter().collect();
        assert_eq!(
            asked,
            [
                "launchctl bootout gui/501/dev.aislopware.slopty.worker",
                "launchctl print gui/501/dev.aislopware.slopty.worker",
                "launchctl bootout gui/501/dev.aislopware.slopty.ptyd",
                "launchctl print gui/501/dev.aislopware.slopty.ptyd",
            ],
            "both taken out of launchd"
        );
        let again = uninstall_worker(&session).await.unwrap();
        assert_eq!(again, [(WORKER, false), (PTYD, false)], "nothing left to remove");
        assert_eq!(session.state(WORKER), State::Absent, "and nothing installed");
    }

    /// A missing daemon stops the install before anything is stopped or written.
    #[tokio::test]
    async fn a_missing_binary_installs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("bin");
        binaries(&source, &["slopty-worker"]);
        let (session, calls) = stand_in(Manager::Launchd, root.path());
        let err = install_worker(&session, &WorkerOpts::default(), &source, root.path())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert!(err.to_string().contains("slopty-ptyd not found"), "{err}");
        assert_eq!(calls.try_iter().count(), 0, "launchd was not asked anything");
        assert!(!session.definitions.exists(), "nothing written");
    }

    /// A bundle is recognised by its shape; a directory that is not one is copied from.
    #[test]
    fn only_an_app_bundles_macos_directory_runs_in_place() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Slopty.app/Contents/MacOS");
        let plain = root.path().join("Contents/MacOS");
        let data = root.path().join("data");
        for dir in [&bundle, &plain] {
            std::fs::create_dir_all(dir).unwrap();
        }
        assert_eq!(run_dir(&bundle, &data, root.path()), bundle, "the bundle");
        assert_eq!(run_dir(&plain, &data, root.path()), data.join("bin"), "not a bundle");
        assert_eq!(
            run_dir(&root.path().join("gone.app/Contents/MacOS"), &data, root.path()),
            data.join("bin"),
            "a bundle whose volume cannot be read is copied from"
        );
    }

    /// A restart asks the manager to kill the job and start it again.
    #[test]
    fn a_restart_kickstarts_the_agent() {
        let root = tempfile::tempdir().unwrap();
        let (session, calls) = stand_in(Manager::Launchd, root.path());
        session.restart(WORKER).unwrap();
        let asked: Vec<String> = calls.try_iter().collect();
        assert_eq!(asked, ["launchctl kickstart -k gui/501/dev.aislopware.slopty.worker"], "-k");
        let (session, calls) = stand_in(Manager::Systemd, root.path());
        session.restart(WORKER).unwrap();
        let asked: Vec<String> = calls.try_iter().collect();
        assert_eq!(asked, ["systemctl --user restart slopty-worker.service"], "the unit");
    }

    /// The worker's plist carries its flags and sockets and runs in the GUI session, kept alive
    /// out of App Nap; the installed worker is the one process that asks macOS for its grants.
    #[test]
    fn plists_carry_the_paths_and_flags() {
        let opts = WorkerOpts {
            port: Some(45551),
            bind: Some(IpAddr::from([192, 168, 1, 10])),
            ..WorkerOpts::default()
        };
        let list = worker_services(&opts, Path::new("/opt/slopty/bin"), Path::new("/data/slopty"));
        assert_eq!(list.len(), 2, "ptyd and the worker");
        let (session, _calls) = stand_in(Manager::Launchd, Path::new("/Users/me"));
        let plist = |(job, service): &(Job, Service)| session.launch_agent(*job, service, true);
        let worker = plist(&list[1]);
        let worker = worker.as_dictionary().unwrap();
        let argv: Vec<&str> = worker["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_string().unwrap())
            .collect();
        assert_eq!(
            argv,
            [
                "/opt/slopty/bin/slopty-worker",
                "--installed",
                "--port",
                "45551",
                "--bind",
                "192.168.1.10"
            ],
            "the binary and its flags"
        );
        let env = worker["EnvironmentVariables"].as_dictionary().unwrap();
        assert_eq!(env["SLOPTY_WORKER_SOCKET"].as_string(), Some("/data/slopty/run/worker.sock"));
        assert_eq!(env["SLOPTY_DATA_DIR"].as_string(), Some("/data/slopty"), "its data");
        assert_eq!(worker["KeepAlive"].as_boolean(), Some(true), "kept alive");
        assert_eq!(worker["ProcessType"].as_string(), Some("Interactive"), "out of App Nap");
        assert_eq!(worker["LimitLoadToSessionType"].as_string(), Some("Aqua"), "GUI session");
        assert_eq!(
            worker["StandardErrorPath"].as_string(),
            Some("/Users/me/Library/Logs/Slopty/slopty-worker.log"),
            "its log under the home"
        );
        let ptyd = plist(&list[0]);
        assert_eq!(ptyd.as_dictionary().unwrap()["Label"].as_string(), Some(PTYD.label));
    }

    #[test]
    fn the_server_plist_runs_slopty_server_on_its_port() {
        let service = server_service(
            Some(45561),
            "debug",
            Path::new("/opt/slopty/bin"),
            Path::new("/data/slopty"),
        );
        let (session, _calls) = stand_in(Manager::Launchd, Path::new("/Users/me"));
        let value = session.launch_agent(SERVER, &service, false);
        let d = value.as_dictionary().unwrap();
        assert_eq!(d["Label"].as_string(), Some(SERVER.label), "its label");
        let argv: Vec<String> = d["ProgramArguments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_string().unwrap().to_owned())
            .collect();
        assert_eq!(argv, ["/opt/slopty/bin/slopty-server", "--port", "45561"], "its port");
        let env = d["EnvironmentVariables"].as_dictionary().unwrap();
        assert_eq!(env["SLOPTY_DATA_DIR"].as_string(), Some("/data/slopty"), "its data");
        assert_eq!(env["RUST_LOG"].as_string(), Some("debug"), "its log level");
        assert_eq!(d["KeepAlive"].as_boolean(), Some(true), "kept alive");
        assert!(
            d["StandardErrorPath"].as_string().unwrap().ends_with("Logs/Slopty/slopty-server.log"),
            "{:?}",
            d["StandardErrorPath"]
        );
        assert!(!d.contains_key("LimitLoadToSessionType"), "the server needs no GUI session");
    }

    /// On Linux the same services are systemd user units: the worker's after ptyd, and the
    /// server's command line read back from the unit written.
    #[test]
    fn a_linux_install_writes_systemd_user_units() {
        let (bin, data) = (Path::new("/opt/slopty/bin"), Path::new("/data/slopty"));
        let list =
            worker_services(&WorkerOpts { port: Some(45551), ..WorkerOpts::default() }, bin, data);
        let (session, _calls) = stand_in(Manager::Systemd, Path::new("/home/me"));
        let (job, worker) = &list[1];
        let unit = String::from_utf8(session.render(*job, worker, true).unwrap()).unwrap();
        assert!(unit.contains("After=slopty-ptyd.service\n"), "{unit}");
        assert!(
            unit.contains(
                "ExecStart=\"/opt/slopty/bin/slopty-worker\" \"--installed\" \"--port\" \"45551\"\n"
            ),
            "{unit}"
        );
        assert!(
            unit.contains("Environment=\"SLOPTY_WORKER_SOCKET=/data/slopty/run/worker.sock\""),
            "{unit}"
        );

        let dir = tempfile::tempdir().unwrap();
        let server = session.render(SERVER, &server_service(Some(45561), "info", bin, data), false);
        let path = dir.path().join("slopty-server.service");
        std::fs::write(&path, server.unwrap()).unwrap();
        assert_eq!(
            installed_args(Manager::Systemd, &path).unwrap(),
            ["/opt/slopty/bin/slopty-server", "--port", "45561"],
            "read back"
        );
        assert!(
            session.file(SERVER).ends_with(".config/systemd/user/slopty-server.service"),
            "{:?}",
            session.file(SERVER)
        );
    }

    /// A daemon that answers from the request's first line may close before the asker has
    /// finished sending: macOS then fails the asker's write (`EPIPE`) or its shutdown
    /// (`ENOTCONN`), depending on where the close lands. The answer is still there to read. The
    /// peer here has answered and closed before the ask starts; an empty request reaches the
    /// shutdown with nothing written.
    #[tokio::test]
    async fn an_answer_from_a_peer_that_already_closed_is_read() {
        use tokio::io::AsyncWriteExt as _;
        for request in [&b""[..], b"{\"status\":null}\n"] {
            let (asker, mut daemon) = tokio::net::UnixStream::pair().unwrap();
            daemon.write_all(b"{\"ok\":true}\n").await.unwrap();
            drop(daemon);
            let reply = ask_on(asker, request).await;
            assert_eq!(reply.unwrap(), r#"{"ok":true}"#, "request {request:?}");
        }
    }

    /// A peer that closes without an answer is an error, not an empty answer, once the request
    /// could not be sent.
    #[tokio::test]
    async fn a_peer_gone_without_an_answer_is_an_error() {
        let (asker, daemon) = tokio::net::UnixStream::pair().unwrap();
        drop(daemon);
        ask_on(asker, b"{}\n").await.unwrap_err();
    }

    #[test]
    fn launchd_pid_reads_the_print_output() {
        assert_eq!(launchd_pid("\tstate = running\n\tpid = 4242\n"), Some(4242), "running");
        assert_eq!(launchd_pid("\tstate = not running\n"), None, "not running");
    }
}
