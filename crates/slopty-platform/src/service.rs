//! The daemons the user's session keeps running, installed and removed.
//!
//! They are `LaunchAgents` on macOS and systemd user units on Linux ([`Manager`]). A worker is two
//! of them, `slopty-ptyd` (the PTY custodian, which keeps shells alive across daemon restarts) and
//! `slopty-worker`; the server is a third. Each restarts when it dies and starts at every login.
//!
//! [`install_worker`] is what `slopty worker install` and the app's "Use this Mac as a worker"
//! both run: stop the services, put the binaries where they will run from, write the definitions
//! and start them again. Waiting for the daemon to answer is the caller's, over the worker's
//! control socket ([`Layout::worker_socket`]).
//!
//! ptyd holds every shell and agent turn, so an install leaves the running one alone whenever
//! the new build keeps custody the same way ([`Session::ptyd_plan`], [`Ptyd::Kept`]): the same
//! protocol and the same shell scripts, which ptyd says beside its socket
//! ([`Layout::ptyd_custody`]). Only the worker restarts then, and takes every session back from
//! ptyd. When the custody changed and the handover did not ([`Ptyd::HandsOver`]), the running
//! ptyd runs the new build in place and hands it every session. A ptyd that must restart ends
//! the sessions it holds; the plan counts them, so the
//! caller asks the person first. Every file an install writes goes under its
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
use serde::{Deserialize, Serialize};

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
    /// Its children outlive it: the manager ends the main process alone when it stops or
    /// dies (`KillMode=process`), as ptyd's sessions must, and a ptyd that starts again takes
    /// them back from the worker that holds their masters.
    pub leaves_children: bool,
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
        if self.leaves_children {
            unit.push_str("KillMode=process\n");
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

/// Whether a service is installed, and its process when it runs. As JSON: `{"running": 700}`,
/// `"stopped"` or `"absent"`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// A worker's services as `slopty --json worker service` reports them, which a deploy reads.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Report {
    /// The PTY custodian.
    pub ptyd: State,
    /// The worker daemon.
    pub worker: State,
    /// What the person must do so the services outlive their last logout, when they do not
    /// ([`Session::stops_at_logout`]).
    pub stops_at_logout: Option<String>,
}

/// How an install says that nobody is logged in at the Mac, so there is no login session to
/// start Slopty in ([`Session::logged_in`]); the deploy names the failure by these words.
pub const NOBODY_LOGGED_IN: &str = "nobody is logged in at this Mac";

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

    /// Let the services outlive the user's last login, and say what the person must do when
    /// that could not be done here.
    ///
    /// The systemd user manager stops a user's services at their last logout unless it
    /// lingers, and a worker installed over SSH must outlive that SSH session: this turns
    /// lingering on (`loginctl enable-linger`, which logind lets a user do for themselves) when
    /// it is off. launchd keeps a `LaunchAgent` for as long as the user is logged in at the Mac.
    #[must_use]
    pub fn keep_running(&self) -> Option<String> {
        if self.manager == Manager::Launchd || self.lingers() {
            return None;
        }
        let turned_on = self.runner.run("loginctl", &["enable-linger"]);
        if turned_on.is_ok() && self.lingers() {
            return None;
        }
        let why = turned_on.err().map(|e| format!(" ({e})")).unwrap_or_default();
        Some(stops_at_logout(&why))
    }

    /// What [`Self::keep_running`] would say, read without changing anything: `None` when the
    /// services outlive the user's last logout, as they do under launchd.
    #[must_use]
    pub fn stops_at_logout(&self) -> Option<String> {
        (self.manager == Manager::Systemd && !self.lingers()).then(|| stops_at_logout(""))
    }

    /// Whether the user's login session is there to start the services in. launchd keeps a
    /// user's agents in the GUI domain their login opens, which a Mac nobody is logged in at
    /// does not have: the install says so in [`NOBODY_LOGGED_IN`]'s words rather than as
    /// `launchctl bootstrap`'s "Domain does not support specified action". systemd's user
    /// manager needs no login once it lingers ([`Self::keep_running`]).
    ///
    /// # Errors
    ///
    /// When there is no GUI domain for the user.
    pub fn logged_in(&self) -> io::Result<()> {
        if self.manager != Manager::Launchd {
            return Ok(());
        }
        let domain = format!("gui/{}", self.uid);
        self.launchctl(&["print", &domain]).map(drop).map_err(|e| {
            io::Error::other(format!(
                "{NOBODY_LOGGED_IN}: launchd has no login session of uid {} to start Slopty in \
                 ({e})",
                self.uid
            ))
        })
    }

    /// Whether logind keeps this user's manager past their last logout.
    fn lingers(&self) -> bool {
        let uid = self.uid.to_string();
        self.runner
            .run("loginctl", &["show-user", &uid, "--property=Linger", "--value"])
            .is_ok_and(|said| said.trim() == "yes")
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

    /// The worker's services as they stand, read without changing anything.
    #[must_use]
    pub fn report(&self) -> Report {
        Report {
            ptyd: self.state(PTYD),
            worker: self.state(WORKER),
            stops_at_logout: self.stops_at_logout(),
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

/// What the person runs so the services outlive their last logout; `why` says what refused it.
fn stops_at_logout(why: &str) -> String {
    format!(
        "it stops when you log out: run `sudo loginctl enable-linger $USER` there so it keeps \
         running{why}"
    )
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

    /// Where the running ptyd says its custody, `<pid> <fingerprint>`: beside its socket, as
    /// `slopty-ptyd` writes it ([`Session::ptyd_plan`]).
    #[must_use]
    pub fn ptyd_custody(&self) -> PathBuf {
        self.run().join("ptyd.custody")
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
        leaves_children: false,
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
    let mut ptyd = service(PTYD, bin_dir, Vec::new(), &opts.log, data_dir, sockets());
    ptyd.leaves_children = true;
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
///
/// Each lands beside its name and is renamed over it once whole, so a binary that still runs
/// (a ptyd kept through the install) keeps its own file: written over in place, a signed
/// Mach-O is killed at its next page-in.
fn copy_binaries(names: &[&str], source: &Path, bin_dir: &Path) -> io::Result<()> {
    if bin_dir == source {
        return Ok(());
    }
    for name in names {
        let (from, to) = (source.join(name), bin_dir.join(name));
        let part = bin_dir.join(format!("{name}.part"));
        std::fs::copy(&from, &part).and_then(|_bytes| std::fs::rename(&part, &to)).map_err(
            |e| context(&e, format_args!("copy {} to {}", from.display(), to.display())),
        )?;
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

/// What an install does to the running ptyd, and so to every shell and agent turn it holds
/// ([`Session::ptyd_plan`]).
///
/// As JSON it is `{"ptyd": "kept"}`, `{"ptyd": "starts"}` or
/// `{"ptyd": "restarts", "sessions": 3}`, which `slopty --json worker install --plan` prints
/// for a deploy to read.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "ptyd", rename_all = "snake_case")]
pub enum Ptyd {
    /// None runs: it starts, with nothing to end.
    Starts,
    /// The one running stays, and every session with it: the new build keeps custody the same
    /// way. Its binary is replaced, to run from its next start.
    Kept,
    /// The one running runs the new build in place and hands it every session
    /// (`slopty-ptyd --succeed`): the new build keeps custody another way, and hands sessions on
    /// as the running one does (their succession is the same).
    HandsOver,
    /// The one running is replaced, ending every session it holds.
    Restarts {
        /// How many it holds (its child processes), when they could be counted.
        sessions: Option<u32>,
    },
}

impl Ptyd {
    /// Whether carrying it out ends sessions, or may: a restart of a ptyd that holds any, or
    /// whose sessions could not be counted. A person is asked before that.
    #[must_use]
    pub const fn ends_sessions(self) -> bool {
        matches!(self, Self::Restarts { sessions } if !matches!(sessions, Some(0)))
    }
}

/// What `slopty --json worker install --plan` prints for a deploy to read first.
///
/// That is what the install does to the running ptyd, the build it installs, the build of the
/// worker running there, so a deploy never takes a machine back to an older build, and the
/// turns that end with that worker.
///
/// As JSON, the ptyd's plan with the rest beside it:
/// `{"ptyd": "kept", "build": "0.4.0+wire…", "running": "0.4.0+wire…", "turns": 1}`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct InstallPlan {
    /// What it does to the running ptyd.
    #[serde(flatten)]
    pub ptyd: Ptyd,
    /// The build it installs ([`slopty_proto::wire::this_build`] of the CLI that planned it).
    pub build: String,
    /// The build of the worker running there, as its doctor says; none when none answers.
    pub running: Option<String>,
    /// The turns running in threads that worker drives itself (pi, an ACP agent), which end
    /// when it stops ([`slopty_proto::ctl::Health::turns`]).
    pub turns: usize,
}

impl InstallPlan {
    /// Whether carrying it out ends anything running: a ptyd's sessions, or a driven turn. A
    /// person is asked before that.
    #[must_use]
    pub const fn ends_work(&self) -> bool {
        self.ptyd.ends_sessions() || self.turns > 0
    }

    /// What carrying it out ends, as a sentence names it: "2 sessions and 1 agent turn".
    #[must_use]
    pub fn ended(&self) -> String {
        ended_said(self.ptyd, self.turns)
    }

    /// Whether carrying it out takes the machine back to an older build than the one running.
    #[must_use]
    pub fn older(&self) -> bool {
        self.running.as_deref().is_some_and(|running| {
            slopty_proto::wire::newer(&self.build, running)
                == Some(slopty_proto::wire::Newer::There)
        })
    }
}

/// What an install that does `ptyd` with `turns` running ends, as a sentence names it: "3
/// sessions", "1 agent turn", "every session and 2 agent turns"; empty when it ends nothing.
#[must_use]
pub fn ended_said(ptyd: Ptyd, turns: usize) -> String {
    let sessions = match ptyd {
        Ptyd::Restarts { sessions: Some(0) } | Ptyd::Starts | Ptyd::Kept | Ptyd::HandsOver => None,
        Ptyd::Restarts { sessions: Some(1) } => Some("1 session".to_owned()),
        Ptyd::Restarts { sessions: Some(n) } => Some(format!("{n} sessions")),
        Ptyd::Restarts { sessions: None } => Some("every session".to_owned()),
    };
    let turns = match turns {
        0 => None,
        1 => Some("1 agent turn".to_owned()),
        n => Some(format!("{n} agent turns")),
    };
    match (sessions, turns) {
        (Some(sessions), Some(turns)) => format!("{sessions} and {turns}"),
        (Some(one), None) | (None, Some(one)) => one,
        (None, None) => String::new(),
    }
}

/// The plan as a sentence for the CLI.
impl std::fmt::Display for Ptyd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starts => f.write_str("slopty-ptyd starts"),
            Self::Kept => f.write_str("slopty-ptyd keeps running, and every session with it"),
            Self::HandsOver => f.write_str("slopty-ptyd hands every session to the new build"),
            Self::Restarts { sessions: Some(0) } => {
                f.write_str("slopty-ptyd restarts; it holds no sessions")
            }
            Self::Restarts { sessions: Some(1) } => {
                f.write_str("slopty-ptyd restarts, ending the 1 session it holds")
            }
            Self::Restarts { sessions: Some(n) } => {
                write!(f, "slopty-ptyd restarts, ending the {n} sessions it holds")
            }
            Self::Restarts { sessions: None } => {
                f.write_str("slopty-ptyd restarts, ending every session it holds")
            }
        }
    }
}

impl Session {
    /// What installing the binaries in `source` over the worker whose data is in `data_dir`
    /// does to its ptyd.
    ///
    /// None running: [`Ptyd::Starts`]. Kept when the new `slopty-ptyd --custody` says the
    /// custody the running one wrote ([`Layout::ptyd_custody`]), trusted only while its pid is
    /// the one the manager runs, and while it holds sessions: one that holds none is restarted
    /// all the same, so the new build's own runs from now. Handed over when the custody differs
    /// and the succession is the same ([`Ptyd::HandsOver`]). Anything else restarts it, counting
    /// its child processes, which are its sessions: an older ptyd says no succession, so it
    /// restarts once.
    #[must_use]
    pub fn ptyd_plan(&self, source: &Path, data_dir: &Path) -> Ptyd {
        let Some(pid) = self.pid(PTYD) else { return Ptyd::Starts };
        let running = std::fs::read_to_string(Layout::new(data_dir).ptyd_custody())
            .ok()
            .and_then(|said| custody_of(&said, pid));
        let program = source.join(PTYD.program);
        let new = self
            .runner
            .run(&program.to_string_lossy(), &["--custody"])
            .inspect_err(|e| tracing::debug!(error = %e, "the new ptyd's custody"))
            .ok()
            .and_then(|said| Custody::parse(&said));
        let sessions = self.children(pid);
        if let (Some(running), Some(new)) = (&running, &new)
            && sessions != Some(0)
        {
            if running.custody == new.custody {
                return Ptyd::Kept;
            }
            if running.succession == new.succession {
                return Ptyd::HandsOver;
            }
        }
        tracing::info!(?running, ?new, ?sessions, "slopty-ptyd restarts");
        Ptyd::Restarts { sessions }
    }

    /// Have the ptyd running for `layout` run the build in `source` in place, handing it every
    /// session ([`Ptyd::HandsOver`]): `slopty-ptyd --succeed` of that build, which returns once
    /// the running one says it runs it.
    ///
    /// The build runs from where it is installed, `bin_dir`: copied there beside the binary it
    /// replaces (`slopty-ptyd.next`) and renamed over it once ptyd runs it, never from `source`,
    /// which may go once the install is done. A `--succeed` that says it failed is weighed
    /// against the custody file: when the running ptyd says, under its own pid, the new build's
    /// custody or that it is handing over, the handover went through all the same. When it did
    /// not, the copy goes and nothing else has changed.
    ///
    /// Once it went through, nothing that fails after is a reason to call it off: the `exec`
    /// happened. `Ok(true)` when the new build is in place where it runs; `Ok(false)` when the
    /// rename failed, so the install copies it there as it copies the rest.
    fn hand_over(&self, source: &Path, bin_dir: &Path, layout: &Layout) -> io::Result<bool> {
        let staged = source.join(PTYD.program);
        let installed = bin_dir.join(PTYD.program);
        let pid = self.pid(PTYD);
        let new = self
            .runner
            .run(&staged.to_string_lossy(), &["--custody"])
            .ok()
            .and_then(|said| Custody::parse(&said));
        let program = if bin_dir == source {
            installed.clone()
        } else {
            let next = bin_dir.join(format!("{}.next", PTYD.program));
            std::fs::copy(&staged, &next).map_err(|e| {
                context(&e, format_args!("copy {} to {}", staged.display(), next.display()))
            })?;
            next
        };
        let socket = layout.ptyd_socket();
        let ran = self
            .runner
            .run(&program.to_string_lossy(), &["--succeed", "--socket", &socket.to_string_lossy()]);
        let went = ran.is_ok() || {
            let now = std::fs::read_to_string(layout.ptyd_custody()).ok();
            let now = now.zip(pid).and_then(|(said, pid)| custody_of(&said, pid));
            now.is_some_and(|now| now.custody == HANDING || Some(&now) == new.as_ref())
        };
        if !went {
            if program != installed {
                let _gone = std::fs::remove_file(&program);
            }
            let e = ran.err().unwrap_or_else(|| io::Error::other("the handover did not happen"));
            return Err(context(
                &e,
                "slopty-ptyd did not hand its sessions to the new build; nothing changed",
            ));
        }
        if let Err(e) = &ran {
            tracing::warn!(error = %e, "slopty-ptyd --succeed failed, but ptyd runs the new build");
        }
        if program != installed
            && let Err(e) = std::fs::rename(&program, &installed)
        {
            tracing::warn!(from = %program.display(), to = %installed.display(), error = %e, "the new ptyd runs but is not in place; copied there instead");
            return Ok(false);
        }
        Ok(true)
    }

    /// How many processes `pid` is the parent of, as `ps` lists them; `None` when it cannot.
    fn children(&self, pid: u32) -> Option<u32> {
        let listed = self
            .runner
            .run("ps", &["-A", "-o", "ppid="])
            .inspect_err(|e| tracing::warn!(error = %e, "count ptyd's sessions"))
            .ok()?;
        let count = listed.lines().filter(|line| line.trim().parse() == Ok(pid)).count();
        u32::try_from(count).ok()
    }

    /// A service left running through an install: its new definition is known to the manager
    /// for its next start, and it still starts at login.
    fn keep(&self, job: Job) -> io::Result<()> {
        match self.manager {
            // launchd reads the file again at the next login; loading it now would refuse a
            // label already loaded.
            Manager::Launchd => Ok(()),
            Manager::Systemd => {
                self.systemctl(&["daemon-reload"])?;
                self.systemctl(&["enable", &format!("{}.service", job.program)]).map(drop)
            }
        }
    }
}

/// What a ptyd's custody file says in place of a custody while it runs the next build in place
/// (`slopty-ptyd`'s `daemon::HANDING`).
const HANDING: &str = "handing";

/// How a ptyd keeps sessions, as `slopty-ptyd --custody` prints it: `<custody> <succession>`.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Custody {
    /// What it hands the worker: its protocol and its shell scripts.
    custody: String,
    /// How it hands its sessions to the build it runs next.
    succession: String,
}

impl Custody {
    /// The two words of `said`, when it has both.
    fn parse(said: &str) -> Option<Self> {
        let mut words = said.split_whitespace();
        let custody = words.next()?.to_owned();
        let succession = words.next()?.to_owned();
        Some(Self { custody, succession })
    }
}

/// What a custody file's `<pid> <custody> <succession>` says, when `pid` wrote it.
fn custody_of(said: &str, pid: u32) -> Option<Custody> {
    let (by, rest) = said.trim().split_once(' ')?;
    by.parse().ok().filter(|by: &u32| *by == pid).and_then(|_| Custody::parse(rest))
}

/// What an install put where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Installed {
    /// Where the daemons run from ([`run_dir`]).
    pub bin_dir: PathBuf,
    /// Each service's definition, in start order.
    pub definitions: Vec<(Job, PathBuf)>,
    /// What it did to ptyd.
    pub ptyd: Ptyd,
}

/// Install the worker's two services in `session` from the binaries in `source`, and start them,
/// carrying out `ptyd` ([`Session::ptyd_plan`], which the caller asked first).
///
/// It stops the worker (and ptyd, unless [`Ptyd::Kept`] or [`Ptyd::HandsOver`]), puts the
/// binaries where they run from ([`run_dir`]), writes the definitions and starts ptyd, then the
/// worker. A kept ptyd is not stopped or started: its binary and definition are replaced for
/// its next start, and the new worker takes its sessions back. One handed over runs the new
/// build once the worker is stopped, before anything else changes; when it does not, the old
/// worker starts again and the install fails. Idempotent. The worker reads its settings and keeps
/// its sockets under `data_dir`; whether it came up is asked of its control socket
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
    ptyd: Ptyd,
) -> io::Result<Installed> {
    check_binaries(&WORKER_BINARIES, source)?;
    session.logged_in()?;
    let layout = Layout::new(data_dir);
    let bin_dir = run_dir(source, data_dir, &session.home);
    session.make_dirs(&[&layout.run(), &bin_dir])?;
    let kept = |job: Job| job == PTYD && matches!(ptyd, Ptyd::Kept | Ptyd::HandsOver);
    // Stop first: a stale socket file makes the daemon's bind fail (the manager would then
    // loop on it).
    session.stop(WORKER, false).await;
    // With the worker gone, before anything else is touched: no worker holds a session across
    // the handover, which would leave it reading a master the new build drains too, and one
    // that does not happen leaves everything as it was, the old worker started again.
    let mut in_place = false;
    if ptyd == Ptyd::HandsOver {
        match session.hand_over(source, &bin_dir, &layout) {
            Ok(renamed) => in_place = renamed,
            Err(e) => {
                if let Err(again) = session.start(WORKER) {
                    tracing::warn!(error = %again, "the old worker did not start again");
                }
                return Err(e);
            }
        }
    }
    if !kept(PTYD) {
        session.stop(PTYD, false).await;
    }
    let copied: Vec<&str> =
        WORKER_BINARIES.iter().copied().filter(|name| !in_place || *name != PTYD.program).collect();
    copy_binaries(&copied, source, &bin_dir)?;
    let mut definitions = Vec::new();
    for (job, service) in worker_services(opts, &bin_dir, data_dir) {
        let path = session.write_definition(job, &service, true)?;
        let started = if kept(job) { session.keep(job) } else { session.start(job) };
        started.map_err(|e| context(&e, format_args!("start {}", job.program)))?;
        definitions.push((job, path));
    }
    Ok(Installed { bin_dir, definitions, ptyd })
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

/// What the worker keeps under its data directory, each its own: what [`purge_worker`] removes.
///
/// The data directory is shared with the app and a server on the same
/// machine (their layout, caches, settings), so the purge names the worker's paths rather than
/// taking the directory. `apps/slopty-worker/tests/purge.rs` runs the real daemons on an empty
/// one and fails when they leave anything this list does not name.
pub const WORKER_STATE: [&str; 21] = [
    "worker-id",
    "input-source",
    "caps-lock",
    "items.json",
    "session.key",
    "sessions",
    "threads",
    "snapshots",
    "presence",
    "claude-mod",
    "pi-gate",
    "claude-managed",
    "ssh-terminfo",
    "shell",
    "bin/slopty-ptyd",
    "bin/slopty-worker",
    "bin/slopty",
    "run/worker.sock",
    "run/worker.mod.sock",
    "run/ptyd.sock",
    "run/ptyd.custody",
];

/// The directories a worker shares with the server's install and the app (the copied binaries,
/// the sockets, the crash reports): a purge removes them only once nothing else is in them.
const WORKER_EMPTIED: [&str; 3] = ["bin", "run", "crashes"];

/// The programs whose crash reports a purge removes: the worker's two daemons.
const WORKER_PROGRAMS: [&str; 2] = ["slopty-worker", "slopty-ptyd"];

/// What `slopty --json worker uninstall --purge` removed from a machine, which a remove over
/// `ssh` reads.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Removed {
    /// The services taken out, by program.
    pub services: Vec<String>,
    /// The files and directories removed, as paths there.
    pub paths: Vec<String>,
    /// Whether Slopty's hook entries were taken out of the agent's settings.
    pub hooks: bool,
}

/// Remove what the worker keeps on this machine, its services gone already
/// ([`uninstall_worker`]); the paths removed, in order.
///
/// That is its own paths under `data_dir` ([`WORKER_STATE`]), its daemons' crash reports, its
/// `LaunchAgents`' logs, and the deploy's stage under the home. A directory the purge emptied
/// goes too; one that holds anything else stays.
///
/// Nothing outside those paths is touched: not the data directory's other files (the app's,
/// a server's, `settings.toml`), and nothing under the home but the logs and the stage.
///
/// # Errors
///
/// When a path is there and cannot be removed.
pub fn purge_worker(session: &Session, data_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for path in WORKER_STATE.iter().map(|rel| data_dir.join(rel)) {
        if remove_path(&path)? {
            removed.push(path);
        }
    }
    let crashes = data_dir.join("crashes");
    for entry in std::fs::read_dir(&crashes).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if crash_of_worker(&name.to_string_lossy()) && remove_path(&entry.path())? {
            removed.push(entry.path());
        }
    }
    for dir in WORKER_EMPTIED {
        let dir = data_dir.join(dir);
        if remove_if_empty(&dir)? {
            removed.push(dir);
        }
    }
    if session.manager == Manager::Launchd {
        for program in WORKER_PROGRAMS {
            let log = session.log_file(program);
            if remove_path(&log)? {
                removed.push(log);
            }
        }
        let logs = session.log_file("slopty");
        if let Some(dir) = logs.parent()
            && remove_if_empty(dir)?
        {
            removed.push(dir.to_path_buf());
        }
    }
    let stage = session.home.join(".slopty");
    if remove_path(&stage.join("deploy"))? {
        removed.push(stage.join("deploy"));
    }
    if remove_if_empty(&stage)? {
        removed.push(stage);
    }
    Ok(removed)
}

/// Whether a crash report's file name (`<ms>-<pid>-<program>.<ext>…`) is one of the worker's
/// daemons'.
fn crash_of_worker(name: &str) -> bool {
    let mut parts = name.splitn(3, '-');
    let (Some(_ms), Some(_pid), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    WORKER_PROGRAMS
        .iter()
        .any(|program| rest.strip_prefix(program).is_some_and(|after| after.starts_with('.')))
}

/// Remove `path`, a file, a socket or a directory with all in it; whether it was there.
fn remove_path(path: &Path) -> io::Result<bool> {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return Ok(false) };
    let gone =
        if meta.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    match gone {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(context(&e, format_args!("remove {}", path.display()))),
    }
}

/// Remove `dir` when it is there and empty; whether it went.
///
/// # Errors
///
/// When it is empty and cannot be removed.
pub fn remove_if_empty(dir: &Path) -> io::Result<bool> {
    let empty = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_none());
    if !empty {
        return Ok(false);
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(context(&e, format_args!("remove {}", dir.display()))),
    }
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
    session.logged_in()?;
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
            leaves_children: false,
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
    /// except that nothing is ever loaded (`print` of a service fails), so a bootout returns at
    /// once. The user is logged in: their GUI domain prints.
    #[derive(Debug)]
    struct Recorder(mpsc::Sender<String>);

    /// Whether `args` print a service (`print gui/501/<label>`), not a domain.
    fn prints_a_service(args: &[&str]) -> bool {
        args.first() == Some(&"print") && args.get(1).is_some_and(|t| t.matches('/').count() > 1)
    }

    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
            let line = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>();
            let _sent = self.0.send(line.join(" "));
            if prints_a_service(args) {
                return Err(io::Error::other("Could not find service"));
            }
            Ok(String::new())
        }
    }

    /// What `loginctl` says, as a stand-in runner: lingering until asked, and whether
    /// `enable-linger` takes.
    #[derive(Debug)]
    struct Logind {
        asked: parking_lot::Mutex<Vec<String>>,
        lingers: std::sync::atomic::AtomicBool,
        may_enable: bool,
    }

    impl Runner for Logind {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
            use std::sync::atomic::Ordering::SeqCst;
            self.asked.lock().push(format!("{program} {}", args.join(" ")));
            match args.first() {
                Some(&"show-user") => {
                    Ok(if self.lingers.load(SeqCst) { "yes\n" } else { "no\n" }.to_owned())
                }
                Some(&"enable-linger") if self.may_enable => {
                    self.lingers.store(true, SeqCst);
                    Ok(String::new())
                }
                _ => Err(io::Error::other("Access denied")),
            }
        }
    }

    /// A systemd user that does not linger is made to, so a worker installed over SSH outlives
    /// that login; when logind refuses, the person is told what to run. launchd needs nothing.
    #[test]
    fn a_systemd_install_lingers_so_it_outlives_the_login() {
        let logind = |lingers: bool, may_enable: bool| {
            let runner = Arc::new(Logind {
                asked: parking_lot::Mutex::default(),
                lingers: lingers.into(),
                may_enable,
            });
            let (mut session, _calls) = stand_in(Manager::Systemd, Path::new("/home/me"));
            session.runner = Arc::<Logind>::clone(&runner);
            (session, runner)
        };
        let (session, runner) = logind(false, true);
        assert_eq!(session.keep_running(), None);
        assert_eq!(
            *runner.asked.lock(),
            [
                "loginctl show-user 501 --property=Linger --value",
                "loginctl enable-linger",
                "loginctl show-user 501 --property=Linger --value",
            ]
        );
        let (session, runner) = logind(true, false);
        assert_eq!(session.keep_running(), None);
        assert_eq!(runner.asked.lock().len(), 1, "lingering already: nothing to turn on");
        let (session, _runner) = logind(false, false);
        let note = session.keep_running().unwrap();
        assert!(note.contains("sudo loginctl enable-linger") && note.contains("Access denied"));
        let (session, _calls) = stand_in(Manager::Launchd, Path::new("/Users/me"));
        assert_eq!(session.keep_running(), None);
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

        let plan = session.ptyd_plan(&macos, &data);
        assert_eq!(plan, Ptyd::Starts, "no ptyd runs");
        let installed =
            install_worker(&session, &WorkerOpts::default(), &macos, &data, plan).await.unwrap();

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
                "launchctl print gui/501/dev.aislopware.slopty.ptyd".to_owned(),
                "launchctl print gui/501".to_owned(),
                "launchctl bootout gui/501/dev.aislopware.slopty.worker".to_owned(),
                "launchctl print gui/501/dev.aislopware.slopty.worker".to_owned(),
                "launchctl bootout gui/501/dev.aislopware.slopty.ptyd".to_owned(),
                "launchctl print gui/501/dev.aislopware.slopty.ptyd".to_owned(),
                format!("launchctl bootstrap gui/501 {}", ptyd.display()),
                format!("launchctl bootstrap gui/501 {}", worker.display()),
            ],
            "asked whether ptyd runs and the user is logged in, stop both, then start ptyd \
             before the worker"
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

        let installed =
            install_worker(&session, &opts, &source, &data, Ptyd::Starts).await.unwrap();

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

    /// A purge takes the worker's own paths, its daemons' crash reports and logs and the
    /// deploy's stage, and leaves what the data directory holds for anything else: the app's
    /// layout and caches, a server's binary, its socket and its log, the settings. A directory
    /// it emptied goes; a second purge finds nothing.
    #[test]
    fn a_purge_takes_the_workers_own_files_and_leaves_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let data = root.path().join("data");
        let (session, calls) = stand_in(Manager::Launchd, &home);
        let write = |path: PathBuf| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        };
        for rel in WORKER_STATE {
            let path = data.join(rel);
            // A file or a directory of things, as the worker keeps each: either goes whole.
            if rel.contains('.') || rel.starts_with("bin/") {
                write(path);
            } else {
                write(path.join("held"));
            }
        }
        write(data.join("crashes/1700-42-slopty-worker.native"));
        write(data.join("crashes/1700-43-slopty-ptyd.json"));
        write(home.join("Library/Logs/Slopty/slopty-worker.log"));
        write(home.join("Library/Logs/Slopty/slopty-ptyd.log"));
        write(home.join(".slopty/deploy/slopty"));
        let kept = [
            data.join("settings.toml"),
            data.join("layout.json"),
            data.join("thread-cache/7/state"),
            data.join("bin/slopty-server"),
            data.join("run/server.sock"),
            data.join("crashes/1700-44-slopty-app.native"),
            home.join("Library/Logs/Slopty/slopty-server.log"),
            home.join("slopty/clones/github.com/o/atlas/README"),
            home.join(".claude/settings.json"),
        ];
        for path in &kept {
            write(path.clone());
        }

        let removed = purge_worker(&session, &data).unwrap();
        for rel in WORKER_STATE {
            assert!(!data.join(rel).exists(), "{rel} removed");
        }
        for path in &kept {
            assert_eq!(std::fs::read(path).unwrap(), b"x", "{} kept as it was", path.display());
        }
        assert!(removed.contains(&data.join("crashes/1700-42-slopty-worker.native")));
        assert!(!data.join("crashes/1700-43-slopty-ptyd.json").exists(), "ptyd's report");
        assert!(!home.join("Library/Logs/Slopty/slopty-worker.log").exists(), "its logs");
        assert!(!home.join(".slopty").exists(), "the stage, emptied, goes");
        assert!(data.join("run").is_dir() && data.join("bin").is_dir(), "shared, so kept");
        assert!(calls.try_iter().next().is_none(), "no service manager asked");

        for path in &kept[3..7] {
            std::fs::remove_file(path).unwrap();
        }
        let again = purge_worker(&session, &data).unwrap();
        let emptied = [
            data.join("bin"),
            data.join("run"),
            data.join("crashes"),
            home.join("Library/Logs/Slopty"),
        ];
        assert_eq!(again, emptied, "only the directories nothing else holds now");
    }

    #[test]
    fn a_crash_report_is_the_workers_by_its_program() {
        assert!(crash_of_worker("1700-42-slopty-worker.native"));
        assert!(crash_of_worker("1700-42-slopty-ptyd.json.partial.9.0"));
        for not in ["1700-42-slopty.native", "1700-42-slopty-app.json", "slopty-worker.native"] {
            assert!(!crash_of_worker(not), "{not}");
        }
    }

    /// A missing daemon stops the install before anything is stopped or written.
    #[tokio::test]
    async fn a_missing_binary_installs_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("bin");
        binaries(&source, &["slopty-worker"]);
        let (session, calls) = stand_in(Manager::Launchd, root.path());
        let err =
            install_worker(&session, &WorkerOpts::default(), &source, root.path(), Ptyd::Starts)
                .await
                .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        assert!(err.to_string().contains("slopty-ptyd not found"), "{err}");
        assert_eq!(calls.try_iter().count(), 0, "launchd was not asked anything");
        assert!(!session.definitions.exists(), "nothing written");
    }

    /// A stand-in launchd with no login session for the user: their GUI domain does not
    /// print, as on a Mac nobody is logged in at.
    #[derive(Debug)]
    struct NobodyHome(mpsc::Sender<String>);

    impl Runner for NobodyHome {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
            let _sent = self.0.send(format!("{program} {}", args.join(" ")));
            if args.first() == Some(&"print") {
                return Err(io::Error::other("Could not find domain for user gui: 501"));
            }
            Ok(String::new())
        }
    }

    /// With nobody logged in at the Mac, an install of the worker or the server says so in
    /// [`NOBODY_LOGGED_IN`]'s words before it stops, copies or writes anything; systemd asks
    /// for no login.
    #[tokio::test]
    async fn an_install_with_nobody_logged_in_says_so_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("bin");
        binaries(&source, &WORKER_BINARIES);
        binaries(&source, &[SERVER.program]);
        let data = root.path().join("data");
        let (tx, calls) = mpsc::channel();
        let session = Session {
            runner: Arc::new(NobodyHome(tx)),
            ..stand_in(Manager::Launchd, root.path()).0
        };
        let opts = WorkerOpts::default();
        let err = install_worker(&session, &opts, &source, &data, Ptyd::Starts).await.unwrap_err();
        assert!(err.to_string().starts_with(NOBODY_LOGGED_IN), "{err}");
        assert!(err.to_string().contains("Could not find domain"), "launchd's words kept: {err}");
        let err = install_server(&session, None, "info", &source, &data).await.unwrap_err();
        assert!(err.to_string().starts_with(NOBODY_LOGGED_IN), "{err}");
        let asked: Vec<String> = calls.try_iter().collect();
        assert_eq!(asked, ["launchctl print gui/501", "launchctl print gui/501"], "nothing else");
        assert!(!session.definitions.exists() && !data.exists(), "nothing written");

        let (linux, _calls) = stand_in(Manager::Systemd, root.path());
        let linux = Session { runner: Arc::new(NobodyHome(mpsc::channel().0)), ..linux };
        linux.logged_in().unwrap();
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

    /// On Linux the same services are systemd user units: the worker's after ptyd, ptyd's
    /// sessions outliving it (systemd ends its main process alone), and the server's command
    /// line read back from the unit written.
    #[test]
    fn a_linux_install_writes_systemd_user_units() {
        let (bin, data) = (Path::new("/opt/slopty/bin"), Path::new("/data/slopty"));
        let list =
            worker_services(&WorkerOpts { port: Some(45551), ..WorkerOpts::default() }, bin, data);
        let (session, _calls) = stand_in(Manager::Systemd, Path::new("/home/me"));
        let (job, ptyd) = &list[0];
        let unit = String::from_utf8(session.render(*job, ptyd, true).unwrap()).unwrap();
        assert!(unit.contains("\nKillMode=process\n"), "a ptyd's end ends no shell: {unit}");
        let (job, worker) = &list[1];
        let unit = String::from_utf8(session.render(*job, worker, true).unwrap()).unwrap();
        assert!(!unit.contains("KillMode"), "the worker's children go with it: {unit}");
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

    /// A machine where ptyd runs as pid 700 with `children` sessions: `launchctl print` and
    /// `systemctl show` say so for ptyd only, the new `slopty-ptyd --custody` says `custody`,
    /// and `ps` lists ptyd's children, or fails.
    #[derive(Debug)]
    struct Running {
        asked: mpsc::Sender<String>,
        custody: &'static str,
        children: Option<usize>,
        /// What `--succeed` does.
        succeeds: Succeeds,
    }

    /// What a stand-in `slopty-ptyd --succeed` does.
    #[derive(Debug)]
    enum Succeeds {
        /// It says the handover went through.
        Yes,
        /// It fails.
        No,
        /// It fails, though ptyd runs the new build: it wrote this custody file first.
        NoButRan(PathBuf, String),
    }

    impl Runner for Running {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<String> {
            let line = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>();
            let _sent = self.asked.send(line.join(" "));
            let ptyd = args.iter().any(|a| a.contains(PTYD.label) || a.contains(PTYD.program));
            match (program, args.first().copied()) {
                (_, Some("--custody")) => Ok(format!("{}\n", self.custody)),
                (_, Some("--succeed")) => match &self.succeeds {
                    Succeeds::Yes => Ok(String::new()),
                    Succeeds::No => Err(io::Error::other("did not come back within 30 s")),
                    Succeeds::NoButRan(file, said) => {
                        std::fs::write(file, said)?;
                        Err(io::Error::other("did not come back within 30 s"))
                    }
                },
                ("ps", _) => {
                    let n = self.children.ok_or_else(|| io::Error::other("ps: not found"))?;
                    Ok(std::iter::repeat_n("  700\n", n).chain(["    1\n", "  701\n"]).collect())
                }
                ("launchctl", Some("print")) if ptyd => Ok("\tpid = 700\n".to_owned()),
                ("launchctl", Some("print")) if prints_a_service(args) => {
                    Err(io::Error::other("Could not find service"))
                }
                ("systemctl", _) if args.contains(&"MainPID") || args.contains(&"--value") => {
                    Ok(if ptyd { "700\n" } else { "0\n" }.to_owned())
                }
                _ => Ok(String::new()),
            }
        }
    }

    /// A session on `manager` in `root`'s home with ptyd running as [`Running`] says, the
    /// custody file `said` written beside its socket under `root/data`, and what it was asked.
    fn running(
        manager: Manager,
        root: &Path,
        said: Option<&str>,
        custody: &'static str,
        children: Option<usize>,
    ) -> (Session, mpsc::Receiver<String>) {
        let (mut session, _calls) = stand_in(manager, &root.join("home"));
        std::fs::create_dir_all(&session.home).unwrap();
        let (tx, asked) = mpsc::channel();
        session.runner =
            Arc::new(Running { asked: tx, custody, children, succeeds: Succeeds::Yes });
        let run = Layout::new(&root.join("data")).run();
        std::fs::create_dir_all(&run).unwrap();
        if let Some(said) = said {
            std::fs::write(Layout::new(&root.join("data")).ptyd_custody(), said).unwrap();
        }
        (session, asked)
    }

    /// The new build keeps custody as the running ptyd does, and it holds sessions: the plan
    /// keeps it, and the install restarts only the worker. ptyd is neither taken out nor loaded
    /// again, and its binary is replaced by a new file renamed over it, never written in place,
    /// so the running one keeps its own.
    #[tokio::test]
    async fn an_install_keeps_a_ptyd_whose_custody_is_unchanged() {
        use std::os::unix::fs::MetadataExt as _;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let (session, asked) = running(
            Manager::Launchd,
            root.path(),
            Some("700 0123abcd 5555\n"),
            "0123abcd 5555",
            Some(3),
        );
        let source = root.path().join("new");
        binaries(&source, &WORKER_BINARIES);
        let bin = Layout::new(&data).bin();
        binaries(&bin, &WORKER_BINARIES);
        let before = std::fs::metadata(bin.join(PTYD.program)).unwrap().ino();

        let plan = session.ptyd_plan(&source, &data);
        assert_eq!(plan, Ptyd::Kept);
        assert!(!plan.ends_sessions());
        let installed =
            install_worker(&session, &WorkerOpts::default(), &source, &data, plan).await.unwrap();

        assert_eq!(installed.ptyd, Ptyd::Kept);
        let after = std::fs::metadata(bin.join(PTYD.program)).unwrap().ino();
        assert_ne!(before, after, "a new file, renamed over the running one's");
        assert!(!bin.join("slopty-ptyd.part").exists(), "nothing half-copied is left");
        let asked: Vec<String> = asked.try_iter().collect();
        let touched_ptyd = asked
            .iter()
            .filter(|c| c.starts_with("launchctl") && !c.starts_with("launchctl print"))
            .filter(|c| c.contains("ptyd"))
            .collect::<Vec<_>>();
        assert_eq!(touched_ptyd, Vec::<&String>::new(), "ptyd left running: {asked:?}");
        assert!(
            asked.iter().any(|c| c == "launchctl bootout gui/501/dev.aislopware.slopty.worker"),
            "{asked:?}"
        );
        assert!(
            asked.iter().any(|c| c.starts_with("launchctl bootstrap") && c.contains("worker")),
            "the worker restarts: {asked:?}"
        );
    }

    /// A new build that keeps custody another way and hands sessions on another way restarts
    /// ptyd, counting the sessions that ends; so does a custody file left by another process (a
    /// ptyd that died and came back), an older ptyd that wrote no succession, and a new ptyd
    /// that says nothing. One that hands sessions on the same way is handed over. A ptyd
    /// holding nothing restarts even when it could be kept, so the new build's own runs from
    /// now.
    #[test]
    fn a_changed_or_unknown_custody_restarts_ptyd_and_counts_its_sessions() {
        let root = tempfile::tempdir().unwrap();
        let (data, source) = (root.path().join("data"), root.path().join("new"));
        let plan = |said: Option<&str>, custody, children| {
            let (session, _asked) = running(Manager::Launchd, root.path(), said, custody, children);
            if said.is_none() {
                let _gone = std::fs::remove_file(Layout::new(&data).ptyd_custody());
            }
            session.ptyd_plan(&source, &data)
        };
        let two = Ptyd::Restarts { sessions: Some(2) };
        assert_eq!(plan(Some("700 aaaa 11"), "bbbb 22", Some(2)), two, "another custody");
        assert_eq!(plan(Some("699 aaaa 11"), "aaaa 11", Some(2)), two, "written by another pid");
        assert_eq!(plan(None, "aaaa 11", Some(2)), two, "an older ptyd says none");
        assert_eq!(plan(Some("700 aaaa"), "aaaa 11", Some(2)), two, "nor any succession");
        assert_eq!(plan(Some("700 aaaa 11"), "", Some(2)), two, "the new one says none");
        assert!(two.ends_sessions());
        let handed = plan(Some("700 aaaa 11"), "bbbb 11", Some(2));
        assert_eq!(handed, Ptyd::HandsOver, "another custody, handed over the same way");
        assert!(!handed.ends_sessions(), "and nothing ends");
        assert_eq!(handed.to_string(), "slopty-ptyd hands every session to the new build");
        let idle = plan(Some("700 aaaa 11"), "aaaa 11", Some(0));
        assert_eq!(idle, Ptyd::Restarts { sessions: Some(0) }, "nothing to keep");
        assert!(!idle.ends_sessions(), "and nothing ends");
        assert_eq!(plan(Some("700 aaaa 11"), "bbbb 11", Some(0)), idle, "nor to hand over");
        let uncounted = plan(Some("700 aaaa 11"), "bbbb 22", None);
        assert_eq!(uncounted, Ptyd::Restarts { sessions: None }, "ps failed");
        assert!(uncounted.ends_sessions(), "sessions that could not be counted may end");

        let (session, _asked) = stand_in(Manager::Launchd, root.path());
        assert_eq!(session.ptyd_plan(&source, &data), Ptyd::Starts, "no ptyd runs");
    }

    /// Restarting ptyd stops and starts it as before.
    #[tokio::test]
    async fn an_install_that_restarts_ptyd_takes_it_out_and_loads_it_again() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let (session, asked) =
            running(Manager::Launchd, root.path(), Some("700 aaaa 11"), "bbbb 22", Some(1));
        let source = root.path().join("new");
        binaries(&source, &WORKER_BINARIES);
        let plan = session.ptyd_plan(&source, &data);
        assert_eq!(plan, Ptyd::Restarts { sessions: Some(1) });
        assert_eq!(plan.to_string(), "slopty-ptyd restarts, ending the 1 session it holds");
        let _planning: Vec<String> = asked.try_iter().collect();
        install_worker(&session, &WorkerOpts::default(), &source, &data, plan).await.unwrap();
        let asked: Vec<String> = asked.try_iter().collect();
        assert!(asked.iter().any(|c| c == "launchctl bootout gui/501/dev.aislopware.slopty.ptyd"));
        assert!(asked.iter().any(|c| c.starts_with("launchctl bootstrap") && c.contains("ptyd")));
    }

    /// A ptyd handed over is asked to run the new build once the worker is stopped, before
    /// anything else changes (`slopty-ptyd --succeed` of that build, copied beside the installed
    /// one, on the installation's socket), and is never taken out or loaded again; the worker
    /// restarts.
    #[tokio::test]
    async fn an_install_hands_ptyd_over_before_anything_else() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let (session, asked) =
            running(Manager::Launchd, root.path(), Some("700 aaaa 11"), "bbbb 11", Some(2));
        let source = root.path().join("new");
        binaries(&source, &WORKER_BINARIES);
        let plan = session.ptyd_plan(&source, &data);
        assert_eq!(plan, Ptyd::HandsOver);
        let _planning: Vec<String> = asked.try_iter().collect();
        install_worker(&session, &WorkerOpts::default(), &source, &data, plan).await.unwrap();
        let asked: Vec<String> = asked.try_iter().collect();
        let socket = Layout::new(&data).ptyd_socket();
        let bin = Layout::new(&data).bin();
        let succeed = format!(
            "{} --succeed --socket {}",
            bin.join("slopty-ptyd.next").display(),
            socket.display()
        );
        let at = asked.iter().position(|c| *c == succeed).unwrap_or_else(|| panic!("{asked:?}"));
        let before: Vec<&String> = asked[..at]
            .iter()
            .filter(|c| !c.starts_with("launchctl print") && !c.ends_with(" --custody"))
            .collect();
        assert_eq!(
            before,
            [&format!("launchctl bootout gui/501/{}", WORKER.label)],
            "only the worker stops before: {asked:?}"
        );
        assert_eq!(
            std::fs::read(bin.join(PTYD.program)).unwrap(),
            PTYD.program.as_bytes(),
            "the new build is installed where it runs"
        );
        assert!(!bin.join("slopty-ptyd.next").exists(), "renamed over the installed one");
        let touched_ptyd = asked
            .iter()
            .filter(|c| c.starts_with("launchctl") && !c.starts_with("launchctl print"))
            .filter(|c| c.contains("ptyd"))
            .collect::<Vec<_>>();
        assert_eq!(touched_ptyd, Vec::<&String>::new(), "ptyd left running: {asked:?}");
        assert!(
            asked.iter().any(|c| c.starts_with("launchctl bootstrap") && c.contains("worker")),
            "the worker restarts: {asked:?}"
        );
    }

    /// A handover that does not happen changes nothing: the install fails, the copy beside the
    /// installed binary goes, ptyd is left alone, no definition is written, and the old worker
    /// starts again. One whose `--succeed` failed although ptyd runs the new build, or is
    /// running it (its custody file says the new custody, or that it is handing over, under its
    /// pid), goes on as one that went through.
    #[tokio::test]
    async fn a_handover_that_does_not_happen_changes_nothing() {
        for wrote in [None, Some("700 bbbb 11\n"), Some("700 handing 11\n")] {
            let went = wrote.is_some();
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("data");
            let (mut session, asked) =
                running(Manager::Launchd, root.path(), Some("700 aaaa 11"), "bbbb 11", Some(2));
            let said = Layout::new(&data).ptyd_custody();
            let (tx, asked_too) = mpsc::channel();
            let succeeds = wrote
                .map_or(Succeeds::No, |wrote| Succeeds::NoButRan(said.clone(), wrote.to_owned()));
            session.runner =
                Arc::new(Running { asked: tx, custody: "bbbb 11", children: Some(2), succeeds });
            drop(asked);
            let source = root.path().join("new");
            binaries(&source, &WORKER_BINARIES);
            let bin = Layout::new(&data).bin();
            binaries(&bin, &[PTYD.program]);
            std::fs::write(bin.join(PTYD.program), b"old").unwrap();
            let done =
                install_worker(&session, &WorkerOpts::default(), &source, &data, Ptyd::HandsOver)
                    .await;
            let asked: Vec<String> = asked_too.try_iter().collect();
            let changed: Vec<&String> = asked
                .iter()
                .filter(|c| c.starts_with("launchctl") && !c.starts_with("launchctl print"))
                .collect();
            assert!(!bin.join("slopty-ptyd.next").exists(), "no copy left: {went}");
            if went {
                done.unwrap();
                assert_eq!(
                    std::fs::read(bin.join(PTYD.program)).unwrap(),
                    PTYD.program.as_bytes(),
                    "installed"
                );
                continue;
            }
            let failed = done.unwrap_err();
            assert!(failed.to_string().contains("nothing changed"), "{failed}");
            assert_eq!(std::fs::read(bin.join(PTYD.program)).unwrap(), b"old", "left as it was");
            assert_eq!(
                changed
                    .iter()
                    .map(|c| c.split(' ').take(2).collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>(),
                ["launchctl bootout", "launchctl bootstrap"],
                "the worker stopped and started again, nothing else: {asked:?}"
            );
            assert!(changed.iter().all(|c| c.contains("worker")), "{asked:?}");
            assert!(!session.file(WORKER).exists(), "no definition written");
        }
    }

    /// Under systemd a kept ptyd's unit is reloaded and stays enabled, and is never restarted;
    /// the worker's is.
    #[tokio::test]
    async fn a_systemd_install_keeps_ptyd_without_restarting_it() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let (session, asked) =
            running(Manager::Systemd, root.path(), Some("700 cafe 11"), "cafe 11", Some(4));
        let source = root.path().join("new");
        binaries(&source, &WORKER_BINARIES);
        let plan = session.ptyd_plan(&source, &data);
        assert_eq!(plan, Ptyd::Kept);
        let _planning: Vec<String> = asked.try_iter().collect();
        install_worker(&session, &WorkerOpts::default(), &source, &data, plan).await.unwrap();
        let asked: Vec<String> = asked.try_iter().collect();
        assert_eq!(
            asked,
            [
                "systemctl --user stop slopty-worker.service",
                "systemctl --user daemon-reload",
                "systemctl --user enable slopty-ptyd.service",
                "systemctl --user daemon-reload",
                "systemctl --user enable slopty-worker.service",
                "systemctl --user restart slopty-worker.service",
            ],
            "ptyd reloaded and enabled, never stopped or restarted"
        );
    }

    /// The plan reads back from the JSON the CLI prints for a deploy.
    #[test]
    fn the_plan_is_json_a_deploy_reads() {
        for (plan, json) in [
            (Ptyd::Kept, r#"{"ptyd":"kept"}"#),
            (Ptyd::Starts, r#"{"ptyd":"starts"}"#),
            (Ptyd::Restarts { sessions: Some(3) }, r#"{"ptyd":"restarts","sessions":3}"#),
            (Ptyd::Restarts { sessions: None }, r#"{"ptyd":"restarts","sessions":null}"#),
        ] {
            assert_eq!(serde_json::to_string(&plan).unwrap(), json);
            assert_eq!(serde_json::from_str::<Ptyd>(json).unwrap(), plan);
        }
    }

    /// The read-only linger check says what the install would, and turns nothing on.
    #[test]
    fn stops_at_logout_only_reads() {
        let (mut session, _calls) = stand_in(Manager::Systemd, Path::new("/home/me"));
        let runner = Arc::new(Logind {
            asked: parking_lot::Mutex::default(),
            lingers: false.into(),
            may_enable: true,
        });
        session.runner = Arc::<Logind>::clone(&runner);
        let note = session.stops_at_logout().unwrap();
        assert!(note.contains("sudo loginctl enable-linger"), "{note}");
        assert_eq!(*runner.asked.lock(), ["loginctl show-user 501 --property=Linger --value"]);
        let (session, _calls) = stand_in(Manager::Launchd, Path::new("/Users/me"));
        assert_eq!(session.stops_at_logout(), None);
    }

    #[test]
    fn launchd_pid_reads_the_print_output() {
        assert_eq!(launchd_pid("\tstate = running\n\tpid = 4242\n"), Some(4242), "running");
        assert_eq!(launchd_pid("\tstate = not running\n"), None, "not running");
    }
}
