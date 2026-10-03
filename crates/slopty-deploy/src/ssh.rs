//! The machine a deploy runs on: a [`Runner`] runs each step's script there. [`Ssh`] is the
//! system `ssh`, one connection per step, or after a password one connection all the steps share
//! ([`Signed`]); [`Local`] is this machine itself; a test drives a runner of its own.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use crate::trust::Scratch;
use crate::{DeployError, Event, Target, askpass};

/// Work a runner does, awaited by the deploy.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where a deploy's events go as they happen: a step begun, bytes sent, a line printed.
pub type OnEvent<'a> = dyn FnMut(Event) + Send + 'a;

/// One script to run there.
#[derive(Debug)]
pub struct Job<'a> {
    /// What `sh -c` runs there (it holds no `'`).
    pub script: &'a str,
    /// A file fed to its standard input, with its length; nothing is fed without one.
    pub input: Option<(std::fs::File, u64)>,
    /// The person watches it: its output is shown as it comes, not kept.
    pub watch: bool,
}

/// How a script ended.
#[derive(Debug)]
pub struct Ran {
    /// Its exit status (ssh's own 255 when ssh could not reach or sign in).
    pub status: ExitStatus,
    /// What it printed, when it was not watched.
    pub stdout: String,
    /// Its error output, when it was not watched.
    pub stderr: String,
}

/// Runs scripts on the machine being deployed to.
pub trait Runner: Send + Sync + std::fmt::Debug {
    /// The machine, as the person named it.
    fn target(&self) -> &str;

    /// What runs the scripts, for a message when it cannot be started.
    fn program(&self) -> String;

    /// Run `job` there. While it feeds a file, [`Event::Sent`] reports the bytes of that file
    /// sent so far; a watched job's lines go out as [`Event::Line`] unless they go straight to
    /// a terminal.
    ///
    /// # Errors
    ///
    /// When it cannot be started or waited for.
    fn run<'a>(&'a self, job: Job<'a>, on: &'a mut OnEvent<'_>) -> Pending<'a, io::Result<Ran>>;

    /// The scripts run on this machine: the binaries install from where they are, with no
    /// upload, and no `ssh` stands between, so there is no address it came from.
    fn is_local(&self) -> bool {
        false
    }

    /// Sign in once with `password`, which the machine asks for instead of a key: the runner
    /// the later steps take, so they ride that one sign-in. `None` when this runner has nothing
    /// to sign in to (this machine, a test's), and the steps go on through it.
    ///
    /// # Errors
    ///
    /// When the sign-in could not be made or the machine refused it ([`DeployError::SignIn`]).
    fn sign_in<'a>(
        &'a self,
        _password: &'a SecretString,
    ) -> Pending<'a, Result<Option<Box<dyn Runner>>, DeployError>> {
        Box::pin(std::future::ready(Ok(None)))
    }
}

/// Where a watched script's output goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Echo {
    /// Straight to this process's terminal, as a typed `ssh` shows it (the CLI).
    Terminal,
    /// As [`Event::Line`]s, one per line (the app).
    Lines,
}

/// The system `ssh`: `~/.ssh/config`, the agent, `ControlMaster` and Tailscale SSH apply as
/// they do to a typed one.
#[derive(Clone, Debug)]
pub struct Ssh {
    /// The `ssh` to run.
    pub program: PathBuf,
    /// Options before the target (`-l`, `-p`, `-o …`).
    pub options: Vec<String>,
    /// The machine, as `ssh` takes it.
    pub target: String,
    /// Where a watched script's output goes.
    pub echo: Echo,
    /// The askpass helper a sign-in with a password hands to `ssh`: the `slopty` CLI, beside
    /// this program as it is in the app and beside the CLI's own daemons.
    pub askpass: Option<PathBuf>,
}

/// How much of a file goes up per write: small enough that the bar moves often.
const CHUNK: usize = 256 * 1024;

impl Ssh {
    /// `ssh <target>` with no options, a watched script's output on the terminal.
    #[must_use]
    pub fn new(program: PathBuf, target: String) -> Self {
        Self { program, options: Vec::new(), target, echo: Echo::Terminal, askpass: askpass_here() }
    }

    /// For a window with nobody at a terminal: `ssh` never asks (a password, a new host key)
    /// and gives up on a host that does not answer, and a watched script's output comes back
    /// as lines. The target's user and port are `-l` and `-p`.
    #[must_use]
    pub fn unattended(target: &Target) -> Self {
        let mut options = Vec::new();
        if let Some(user) = &target.user {
            options.extend(["-l".to_owned(), user.clone()]);
        }
        if let Some(port) = target.port {
            options.extend(["-p".to_owned(), port.to_string()]);
        }
        options.extend(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"].map(str::to_owned));
        let program = PathBuf::from("ssh");
        let target = target.host.clone();
        Self { program, options, target, echo: Echo::Lines, askpass: askpass_here() }
    }

    /// `script` run by `sh` there, whatever the login shell (the scripts hold no `'`).
    fn command(&self, script: &str) -> Command {
        let mut ssh = Command::new(&self.program);
        ssh.args(&self.options).arg(&self.target).arg(format!("sh -c '{script}'"));
        ssh.kill_on_drop(true);
        ssh
    }
}

/// This machine: each script runs under `sh` in `home`, as one would after `ssh` here, its
/// watched output coming back as lines.
#[derive(Clone, Debug)]
pub struct Local {
    /// Where the scripts start.
    pub home: PathBuf,
}

impl Local {
    /// This user's home.
    #[must_use]
    pub fn here() -> Self {
        Self { home: slopty_platform::dirs::home() }
    }
}

impl Runner for Local {
    fn target(&self) -> &'static str {
        "this machine"
    }

    fn program(&self) -> String {
        "sh".to_owned()
    }

    fn run<'a>(&'a self, job: Job<'a>, on: &'a mut OnEvent<'_>) -> Pending<'a, io::Result<Ran>> {
        let mut sh = Command::new("sh");
        sh.arg("-c").arg(job.script).current_dir(&self.home).kill_on_drop(true);
        Box::pin(go(sh, Echo::Lines, job, on))
    }

    fn is_local(&self) -> bool {
        true
    }
}

/// Run `command` for `job`: its input fed, its output kept, or shown as `echo` says.
async fn go(
    mut command: Command,
    echo: Echo,
    job: Job<'_>,
    on: &mut OnEvent<'_>,
) -> io::Result<Ran> {
    let shown = job.watch && echo == Echo::Terminal;
    let (out, err) = if shown {
        (Stdio::inherit(), Stdio::inherit())
    } else if job.input.is_some() {
        (Stdio::null(), Stdio::piped())
    } else {
        (Stdio::piped(), Stdio::piped())
    };
    let stdin = if job.input.is_some() { Stdio::piped() } else { Stdio::null() };
    let mut child = command.stdin(stdin).stdout(out).stderr(err).spawn()?;
    if let Some((file, total)) = job.input {
        let stdin = child.stdin.take();
        let stderr = child.stderr.take();
        let (fed, stderr) = tokio::join!(feed(file, total, stdin, on), read_all(stderr));
        let status = child.wait().await?;
        if status.success() {
            fed?;
        }
        return Ok(Ran { status, stdout: String::new(), stderr: stderr? });
    }
    if job.watch && !shown {
        lines(child.stdout.take(), child.stderr.take(), on).await?;
        let status = child.wait().await?;
        return Ok(Ran { status, stdout: String::new(), stderr: String::new() });
    }
    let output = child.wait_with_output().await?;
    Ok(Ran {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

impl Runner for Ssh {
    fn target(&self) -> &str {
        &self.target
    }

    fn program(&self) -> String {
        self.program.display().to_string()
    }

    fn run<'a>(&'a self, job: Job<'a>, on: &'a mut OnEvent<'_>) -> Pending<'a, io::Result<Ran>> {
        Box::pin(go(self.command(job.script), self.echo, job, on))
    }

    fn sign_in<'a>(
        &'a self,
        password: &'a SecretString,
    ) -> Pending<'a, Result<Option<Box<dyn Runner>>, DeployError>> {
        Box::pin(async move {
            let signed: Box<dyn Runner> = Box::new(self.signed_in(password).await?);
            Ok(Some(signed))
        })
    }
}

/// The `slopty` beside this program, the askpass helper.
fn askpass_here() -> Option<PathBuf> {
    slopty_platform::service::sibling_dir().ok().map(|dir| dir.join("slopty"))
}

/// How long a sign-in may take, its password answered at once.
const SIGN_IN: Duration = Duration::from_secs(60);

/// How long the shared connection outlives its last step, should the deploy end without
/// closing it.
pub const PERSIST: &str = "ControlPersist=60";

impl Ssh {
    /// Sign in once with `password`: a master connection (`ControlMaster`) in the background,
    /// its password questions answered through the askpass helper ([`askpass`]), which every
    /// step of [`Signed`] shares with `BatchMode` still on, so nothing asks again.
    ///
    /// # Errors
    ///
    /// When `ssh` or the socket cannot be started, or the machine refused the password.
    pub async fn signed_in(&self, password: &SecretString) -> Result<Signed, DeployError> {
        let program = self.program();
        let failed = |source: io::Error| DeployError::Run { program: program.clone(), source };
        let helper = self.askpass.clone().ok_or_else(|| {
            failed(io::Error::new(io::ErrorKind::NotFound, "no slopty beside this program"))
        })?;
        let dir = Scratch::new("slopty-ssh").map_err(|(_, e)| failed(e))?;
        let control = dir.path().join("c");
        let sock = dir.path().join("a");
        let answering = askpass::serve(&sock, password.clone()).map_err(failed)?;
        let said = dir.path().join("e");
        let errors = std::fs::File::create(&said).map_err(failed)?;
        let mut master = self.master(&control, &helper, &sock);
        master.stdin(Stdio::null()).stdout(Stdio::null()).stderr(errors);
        let status = match tokio::time::timeout(SIGN_IN, master.status()).await {
            Ok(status) => Some(status.map_err(failed)?),
            Err(_elapsed) => None,
        };
        drop(answering);
        let stderr = std::fs::read_to_string(&said).unwrap_or_default().trim().to_owned();
        if !status.is_some_and(|s| s.success()) {
            return Err(DeployError::SignIn { target: self.target.clone(), status, stderr });
        }
        let mut ssh = self.clone();
        let shared = format!("ControlPath={}", control.display());
        let shared = ["-o", &shared, "-o", "ControlMaster=no"].map(str::to_owned);
        ssh.options.splice(0..0, shared);
        Ok(Signed { ssh, master: Master { exit: Some(self.exit(&control)), dir: Some(dir) } })
    }

    /// The `ssh` that signs in: in the background once it has (`-f -N`), its socket at `control`,
    /// every question it asks put to `helper`, which asks `sock`. The options go before the
    /// runner's own, since `ssh` takes the first value given.
    pub(crate) fn master(&self, control: &Path, helper: &Path, sock: &Path) -> Command {
        let mut ssh = Command::new(&self.program);
        let path = format!("ControlPath={}", control.display());
        for option in
            ["ControlMaster=yes", &path, PERSIST, "BatchMode=no", "NumberOfPasswordPrompts=1"]
        {
            ssh.arg("-o").arg(option);
        }
        ssh.args(["-f", "-N"]).args(&self.options).arg(&self.target);
        ssh.env("SSH_ASKPASS_REQUIRE", "force").env("SSH_ASKPASS", helper).env(askpass::SOCK, sock);
        ssh.kill_on_drop(true);
        ssh
    }

    /// `ssh -O exit`, which ends the master at `control`.
    fn exit(&self, control: &Path) -> std::process::Command {
        let mut ssh = std::process::Command::new(&self.program);
        ssh.arg("-o").arg(format!("ControlPath={}", control.display()));
        ssh.args(["-O", "exit"]).args(&self.options).arg(&self.target);
        ssh.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        ssh
    }
}

/// The system `ssh` once signed in with a password: every step runs over the one connection
/// the sign-in made, which ends when this is dropped.
#[derive(Debug)]
pub struct Signed {
    ssh: Ssh,
    master: Master,
}

impl Signed {
    /// End the shared connection now, and return once `ssh` has ended it and its directory is
    /// gone. Dropped instead, it ends the same way off this task.
    pub async fn end(mut self) {
        if let Some(exit) = self.master.exit.take() {
            let ended = Command::from(exit).kill_on_drop(true).status().await;
            if let Err(e) = ended {
                tracing::warn!(error = %e, "end the shared ssh connection");
            }
        }
    }
}

impl Runner for Signed {
    fn target(&self) -> &str {
        self.ssh.target()
    }

    fn program(&self) -> String {
        self.ssh.program()
    }

    fn run<'a>(&'a self, job: Job<'a>, on: &'a mut OnEvent<'_>) -> Pending<'a, io::Result<Ran>> {
        self.ssh.run(job, on)
    }
}

/// The master connection: ended (`ssh -O exit`), then its directory removed, when dropped.
#[derive(Debug)]
struct Master {
    exit: Option<std::process::Command>,
    dir: Option<Scratch>,
}

impl Drop for Master {
    fn drop(&mut self) {
        let dir = self.dir.take();
        let Some(mut exit) = self.exit.take() else { return };
        match exit.spawn() {
            // Waited for off this thread, so a drop in async code never blocks; the directory
            // goes once the master has let go of its socket.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ended = child.wait();
                    drop(dir);
                });
            }
            Err(e) => tracing::warn!(error = %e, "end the shared ssh connection"),
        }
    }
}

/// Copy `file` into `stdin` a chunk at a time, saying how much has gone, then close it. A
/// write the far end refused stops the copy: its exit status says why.
async fn feed(
    file: std::fs::File,
    total: u64,
    stdin: Option<tokio::process::ChildStdin>,
    on: &mut OnEvent<'_>,
) -> io::Result<()> {
    let Some(mut stdin) = stdin else { return Ok(()) };
    let mut file = tokio::fs::File::from_std(file);
    let mut chunk = vec![0; CHUNK];
    let mut sent = 0_u64;
    loop {
        let n = file.read(&mut chunk).await?;
        let Some(bytes) = chunk.get(..n).filter(|b| !b.is_empty()) else { break };
        stdin.write_all(bytes).await?;
        sent = sent.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        on(Event::Sent { sent: sent.min(total), total });
    }
    stdin.shutdown().await
}

/// Everything `pipe` holds, as text.
async fn read_all(pipe: Option<tokio::process::ChildStderr>) -> io::Result<String> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_end(&mut bytes).await?;
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Each line of `stdout` and `stderr` as it comes, until both close.
async fn lines(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    on: &mut OnEvent<'_>,
) -> io::Result<()> {
    let mut out = stdout.map(|s| BufReader::new(s).lines());
    let mut err = stderr.map(|s| BufReader::new(s).lines());
    loop {
        let line = tokio::select! {
            line = async { out.as_mut()?.next_line().await.transpose() }, if out.is_some() => {
                line.or_else(|| { out = None; None })
            }
            line = async { err.as_mut()?.next_line().await.transpose() }, if err.is_some() => {
                line.or_else(|| { err = None; None })
            }
            else => return Ok(()),
        };
        if let Some(line) = line {
            on(Event::Line(line?));
        }
    }
}
