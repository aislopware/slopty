//! The remote host: a [`Runner`] runs each step's script there. [`Ssh`] is the system `ssh`,
//! one connection per step; a test drives a runner of its own.

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::Command;

use crate::Event;

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
}

/// How much of a file goes up per write: small enough that the bar moves often.
const CHUNK: usize = 256 * 1024;

impl Ssh {
    /// `ssh <target>` with no options, a watched script's output on the terminal.
    #[must_use]
    pub const fn new(program: PathBuf, target: String) -> Self {
        Self { program, options: Vec::new(), target, echo: Echo::Terminal }
    }

    /// For a window with nobody at a terminal: `ssh` never asks (a password, a new host key)
    /// and gives up on a host that does not answer, and a watched script's output comes back
    /// as lines. `user` and `port` are `-l` and `-p`.
    #[must_use]
    pub fn unattended(target: String, user: Option<&str>, port: Option<u16>) -> Self {
        let mut options = Vec::new();
        if let Some(user) = user {
            options.extend(["-l".to_owned(), user.to_owned()]);
        }
        if let Some(port) = port {
            options.extend(["-p".to_owned(), port.to_string()]);
        }
        options.extend(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"].map(str::to_owned));
        Self { program: PathBuf::from("ssh"), options, target, echo: Echo::Lines }
    }

    /// `script` run by `sh` there, whatever the login shell (the scripts hold no `'`).
    fn command(&self, script: &str) -> Command {
        let mut ssh = Command::new(&self.program);
        ssh.args(&self.options).arg(&self.target).arg(format!("sh -c '{script}'"));
        ssh.kill_on_drop(true);
        ssh
    }

    async fn go(&self, job: Job<'_>, on: &mut OnEvent<'_>) -> io::Result<Ran> {
        let mut command = self.command(job.script);
        let shown = job.watch && self.echo == Echo::Terminal;
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
}

impl Runner for Ssh {
    fn target(&self) -> &str {
        &self.target
    }

    fn program(&self) -> String {
        self.program.display().to_string()
    }

    fn run<'a>(&'a self, job: Job<'a>, on: &'a mut OnEvent<'_>) -> Pending<'a, io::Result<Ran>> {
        Box::pin(self.go(job, on))
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
