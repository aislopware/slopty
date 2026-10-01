//! Put a worker on another machine over the system `ssh`, or update the one there: what
//! `slopty worker deploy` runs, and what the app's "Install on a machine over SSH" and a
//! different build's "Update" run.
//!
//! The system `ssh` carries everything ([`Ssh`]), so `~/.ssh/config`, the agent,
//! `ControlMaster` and Tailscale SSH apply as they do to a typed `ssh`. The steps ([`Step`]):
//!
//! 1. `uname -sm` there names its OS and CPU, and each binary to upload is read for its own
//!    ([`Platform::of_binary`]): a build for another machine is refused before anything moves. The
//!    same step reads the address `ssh` came from there (`$SSH_CONNECTION`), which is how the far
//!    side reaches a server that runs on the deploying machine.
//! 2. `slopty-ptyd`, `slopty-worker` and `slopty` go to [`STAGE`] under the remote home, each
//!    written beside its name and moved over it once whole.
//! 3. The uploaded `slopty worker install` runs there, which installs the services the way a local
//!    install does, saves the server to register with ([`Plan::server`]), waits for the new worker
//!    to answer as itself, and with `--update` puts the previous worker back when it does not.
//! 4. `slopty worker doctor --json` there says what it can do; a Mac still needs a person at the
//!    desk for Screen Recording and Accessibility.
//!
//! [`serve`] puts the server on a machine the same way: the same first step and upload, then
//! `slopty server install`, which waits until it answers there.
//!
//! Every step goes through a [`Runner`], so a test drives the plan with no machine. [`Local`]
//! runs the same plan on this machine with no `ssh` and no upload: it installs the binaries
//! where they are, which is how the app updates its own Mac's worker without Remote Login.

#![forbid(unsafe_code)]

mod platform;
mod ssh;
mod target;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

pub use platform::{Arch, Os, Platform, Unsupported};
use slopty_platform::service::WORKER_BINARIES;
use slopty_proto::ctl::Health;
pub use ssh::{Echo, Job, Local, OnEvent, Pending, Ran, Runner, Ssh};
pub use target::{REMEMBERED, Remembered, Server, Target};

/// Where the binaries land on the remote host, relative to its home (where `ssh` starts).
pub const STAGE: &str = ".slopty/deploy";

/// How many of the install's last lines a failure keeps.
pub const TAIL: usize = 8;

/// What to deploy, and how.
#[derive(Clone, Debug)]
pub struct Plan {
    /// Directories of the binaries ([`WORKER_BINARIES`]), each built for one platform: the
    /// first whose three all run on the machine goes there ([`bundled`] lists an app's).
    pub sources: Vec<PathBuf>,
    /// Bring the machine to this build: replace the worker installed there, keeping its port
    /// and address and putting it back if the new one does not come up, or install one where
    /// there is none. Without it a machine with a worker is refused.
    pub update: bool,
    /// The server the worker registers with, so every client of it lists the machine; as it
    /// was there before when `None`.
    pub server: Option<Server>,
}

/// A step of a deploy, in the order they run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// `uname` there, and the binaries' headers here.
    Reach,
    /// One binary going up to [`STAGE`].
    Upload {
        /// Its name.
        name: &'static str,
    },
    /// The uploaded `slopty worker install` running there.
    Install,
    /// The new worker's `doctor`.
    Check,
}

/// What a deploy says as it goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// A step began.
    Step(Step),
    /// What the machine runs, once `uname` said.
    Machine(Platform),
    /// Bytes of the binaries sent so far, of all of them.
    Sent {
        /// Sent so far.
        sent: u64,
        /// All the binaries together.
        total: u64,
    },
    /// A line a watched step printed there.
    Line(String),
}

/// What a deploy left running there.
#[derive(Clone, PartialEq, Debug)]
pub struct Deployed {
    /// The machine.
    pub platform: Platform,
    /// The worker's own account of itself.
    pub health: Health,
    /// The server address the install saved for it to register with: [`Plan::server`] as the
    /// machine reaches it. `None` when the plan named none, or named this machine at loopback
    /// and the machine did not say where it was reached from.
    pub server: Option<String>,
}

/// Why a deploy stopped. Each reads as the CLI has always said it; [`DeployError::failure`] is what
/// a window says.
#[derive(Debug, thiserror::Error)]
pub enum DeployError {
    /// A target that `ssh` would take for an option.
    #[error("{0:?} is not an ssh target")]
    NotATarget(String),
    /// The runner could not be started.
    #[error("run {program}")]
    Run {
        /// What runs the scripts.
        program: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A script there failed.
    #[error("`{script}` on {target} failed ({status}): {stderr}")]
    Failed {
        /// The script.
        script: String,
        /// The machine.
        target: String,
        /// How it ended.
        status: ExitStatus,
        /// Its error output, trimmed.
        stderr: String,
    },
    /// A machine no worker is built for.
    #[error("on {target}")]
    Machine {
        /// The machine.
        target: String,
        /// What it runs.
        #[source]
        source: Unsupported,
    },
    /// A binary to upload could not be read.
    #[error("read {}", path.display())]
    Read {
        /// The binary.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// A binary built for another machine.
    #[error(
        "{} is built for {}, and {target} is {platform}; pass --bin-dir with a build for it",
        path.display(),
        built_for(built.as_ref())
    )]
    Mismatch {
        /// The binary.
        path: PathBuf,
        /// What it is built for, if anything we know.
        built: Option<Platform>,
        /// The machine.
        target: String,
        /// What it runs.
        platform: Platform,
    },
    /// A binary to upload could not be opened.
    #[error("open {}", path.display())]
    Open {
        /// The binary.
        path: PathBuf,
        /// Why.
        #[source]
        source: std::io::Error,
    },
    /// An upload failed there.
    #[error("uploading {name} to {target} failed ({status}): {stderr}")]
    Upload {
        /// The binary.
        name: &'static str,
        /// The machine.
        target: String,
        /// How it ended.
        status: ExitStatus,
        /// Its error output, trimmed.
        stderr: String,
    },
    /// The install there failed; with `--update`, the previous worker is back.
    #[error("`{script}` on {target} failed ({status}){}", said(tail))]
    Install {
        /// The script.
        script: String,
        /// The machine.
        target: String,
        /// How it ended.
        status: ExitStatus,
        /// Its last lines, when they came back as lines rather than to a terminal.
        tail: Vec<String>,
    },
    /// A directory to install from in place that a script cannot name as it is.
    #[error("{} holds a character the install cannot pass to sh", path.display())]
    Path {
        /// The directory.
        path: PathBuf,
    },
    /// The doctor there answered with something other than a health report.
    #[error("the worker's doctor said {said:?}: {error}")]
    Doctor {
        /// What it printed.
        said: String,
        /// Why that is not a report.
        error: serde_json::Error,
    },
}

/// A binary's platform as the mismatch names it.
fn built_for(built: Option<&Platform>) -> String {
    built.map_or_else(|| "no machine we know".to_owned(), Platform::to_string)
}

/// Where the install's output is: on the terminal above, or its last line.
fn said(tail: &[String]) -> String {
    tail.last().map_or_else(|| "; see above".to_owned(), |line| format!(": {line}"))
}

/// A failed deploy as a window says it: a short title, what to do when there is something,
/// and the last lines the machine printed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Failure {
    /// One sentence, sentence case, no full stop.
    pub title: String,
    /// What to do about it, when that is known.
    pub hint: Option<String>,
    /// The last lines it printed, oldest first.
    pub lines: Vec<String>,
}

/// `ssh`'s own exit status when it could not connect or sign in.
const SSH_FAILED: i32 = 255;

impl DeployError {
    /// This failure as a window says it.
    #[must_use]
    pub fn failure(&self) -> Failure {
        let plain = |title: String, lines: Vec<String>| Failure { title, hint: None, lines };
        match self {
            Self::NotATarget(target) => plain(format!("{target} is not a host name"), Vec::new()),
            Self::Run { program, source } => Failure {
                title: format!("Could not start {program}"),
                hint: Some("Slopty uses the ssh on this Mac's PATH.".to_owned()),
                lines: vec![source.to_string()],
            },
            Self::Failed { target, status, stderr, .. } => {
                ssh_failure(target, *status, stderr, || format!("A step failed on {target}"))
            }
            Self::Upload { name, target, status, stderr } => {
                ssh_failure(target, *status, stderr, || {
                    format!("Could not copy {name} to {target}")
                })
            }
            Self::Machine { target, source } => Failure {
                title: format!("No worker runs on {target}"),
                hint: Some("Workers run on macOS and Linux, on arm64 or x86_64.".to_owned()),
                lines: vec![source.to_string()],
            },
            Self::Path { path } => Failure {
                title: "Slopty is in a folder the install cannot name".to_owned(),
                hint: Some("Move Slopty to a folder whose name has no quotes, $ or \\.".to_owned()),
                lines: vec![path.display().to_string()],
            },
            Self::Read { path, source } | Self::Open { path, source } => Failure {
                title: "Could not read the worker to send".to_owned(),
                hint: None,
                lines: vec![format!("{}: {source}", path.display())],
            },
            Self::Mismatch { built, target, platform, .. } => Failure {
                title: format!("This build has no worker for {platform}"),
                hint: Some(format!(
                    "{target} runs {platform}; this app carries a worker for {}.",
                    built_for(built.as_ref())
                )),
                lines: Vec::new(),
            },
            Self::Install { target, tail, .. } => {
                plain(format!("The install on {target} failed"), tail.clone())
            }
            Self::Doctor { said, .. } => {
                plain("The new worker did not report back".to_owned(), last_lines(said))
            }
        }
    }
}

/// A script's failure: `ssh`'s own (it could not reach or sign in) named by what it printed,
/// else `other`.
fn ssh_failure(
    target: &str,
    status: ExitStatus,
    stderr: &str,
    other: impl FnOnce() -> String,
) -> Failure {
    let lines = last_lines(stderr);
    if status.code() != Some(SSH_FAILED) {
        return Failure { title: other(), hint: None, lines };
    }
    let has = |words: &str| stderr.contains(words);
    let (title, hint) = if has("Host key verification failed") {
        (
            format!("{target}'s host key is not trusted yet"),
            Some("Connect once with ssh in a terminal to trust it, then try again."),
        )
    } else if has("Permission denied") {
        (
            format!("{target} did not accept your SSH key"),
            Some("Add your key to your ssh agent, or to its authorized_keys."),
        )
    } else if has("Could not resolve hostname") {
        (
            format!("No machine is named {target}"),
            Some("Check the name, and that Tailscale or your VPN is up."),
        )
    } else if has("Connection refused") {
        (
            format!("{target} does not accept SSH"),
            Some("Turn on Remote Login there, in System Settings, General, Sharing."),
        )
    } else if has("timed out") {
        (
            format!("{target} did not answer"),
            Some("Check that it is awake and on your tailnet or VPN."),
        )
    } else {
        (format!("Could not reach {target} over SSH"), None)
    };
    Failure { title, hint: hint.map(str::to_owned), lines }
}

/// The last [`TAIL`] lines of `text` with something on them, oldest first.
fn last_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let skip = lines.len().saturating_sub(TAIL);
    lines.into_iter().skip(skip).map(str::to_owned).collect()
}

/// Deploy (or with [`Plan::update`], replace) the worker on `runner`'s machine, saying each
/// step to `on` as it begins.
///
/// # Errors
///
/// When the machine or the binaries do not fit, a step there fails, or the new worker did not
/// come up (after an update, the previous one is back then).
pub async fn deploy(
    runner: &dyn Runner,
    plan: &Plan,
    on: &mut OnEvent<'_>,
) -> Result<Deployed, DeployError> {
    let reached = reach(runner, on).await?;
    let bin = send(runner, &plan.sources, &WORKER_BINARIES, reached.platform, on).await?;
    let mode = if plan.update { "--update" } else { "--fresh" };
    let client = reached.client.as_deref();
    let server = plan.server.as_ref().and_then(|server| {
        let seen = server.seen_from(client);
        if seen.is_none() {
            tracing::warn!(?server, ?client, "no address the worker there reaches the server at");
        }
        seen
    });
    let register = server.as_ref().map(|s| format!(" --server {s}")).unwrap_or_default();
    on(Event::Step(Step::Install));
    let script = format!("{bin}/slopty{register} worker install --bin-dir {bin} {mode}");
    install(runner, &script, on).await?;
    on(Event::Step(Step::Check));
    let doctor = output(runner, &format!("{bin}/slopty --json worker doctor"), on).await?;
    let health: Health = serde_json::from_str(&doctor)
        .map_err(|error| DeployError::Doctor { said: doctor, error })?;
    Ok(Deployed { platform: reached.platform, health, server })
}

/// The binaries a server's installation carries: the server, and the CLI that installs it.
pub const SERVER_BINARIES: [&str; 2] = ["slopty-server", "slopty"];

/// What a server put on a machine left running.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Served {
    /// The machine.
    pub platform: Platform,
    /// Where to dial it, best first: the address `ssh` reached it at (which this machine is known
    /// to reach), then the target as named; loopback for this machine.
    pub addresses: Vec<String>,
}

/// Put the server on `runner`'s machine from the first of `sources` built for it, or replace
/// the one there, and wait until it answers there (`slopty server install`).
///
/// # Errors
///
/// When the machine or the binaries do not fit, a step there fails, or the server did not come
/// up.
pub async fn serve(
    runner: &dyn Runner,
    sources: &[PathBuf],
    on: &mut OnEvent<'_>,
) -> Result<Served, DeployError> {
    let reached = reach(runner, on).await?;
    let bin = send(runner, sources, &SERVER_BINARIES, reached.platform, on).await?;
    on(Event::Step(Step::Install));
    install(runner, &format!("{bin}/slopty server install --bin-dir {bin}"), on).await?;
    let addresses = if runner.is_local() {
        vec!["127.0.0.1".to_owned()]
    } else {
        let mut addresses: Vec<String> = reached.here.into_iter().collect();
        if !addresses.iter().any(|a| a == runner.target()) {
            addresses.push(runner.target().to_owned());
        }
        addresses
    };
    Ok(Served { platform: reached.platform, addresses })
}

/// What the first step learnt of the machine.
#[derive(Debug)]
struct Reached {
    /// Its OS and CPU.
    platform: Platform,
    /// The address `ssh` came from, as the machine saw it.
    client: Option<String>,
    /// The machine's own address that `ssh` reached.
    here: Option<String>,
}

/// The first step: the machine's OS and CPU and, over `ssh`, the two ends of the connection.
async fn reach(runner: &dyn Runner, on: &mut OnEvent<'_>) -> Result<Reached, DeployError> {
    let target = runner.target();
    if target.starts_with('-') {
        return Err(DeployError::NotATarget(target.to_owned()));
    }
    on(Event::Step(Step::Reach));
    let local = runner.is_local();
    let said = output(runner, if local { "uname -sm" } else { REACH }, on).await?;
    let mut lines = said.lines();
    let platform = Platform::from_uname(lines.next().unwrap_or_default())
        .map_err(|source| DeployError::Machine { target: target.to_owned(), source })?;
    on(Event::Machine(platform));
    // `client-ip client-port server-ip server-port`.
    let ends: Vec<String> = lines
        .next()
        .filter(|_| !local)
        .map(|line| line.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    let mut ends = ends.into_iter().step_by(2);
    Ok(Reached { platform, client: ends.next(), here: ends.next() })
}

/// Get `names` built for `platform` to the machine from the first of `sources` that has them:
/// uploaded to [`STAGE`] there, or where they are on this machine. The directory to run them
/// from, as a word of a script.
async fn send(
    runner: &dyn Runner,
    sources: &[PathBuf],
    names: &[&'static str],
    platform: Platform,
    on: &mut OnEvent<'_>,
) -> Result<String, DeployError> {
    let target = runner.target().to_owned();
    let mut first_mismatch = None;
    let mut fitting = None;
    for dir in sources {
        match fits(dir, names, platform)? {
            Ok(total) => {
                fitting = Some((dir, total));
                break;
            }
            Err(mismatch) => {
                first_mismatch.get_or_insert(mismatch);
            }
        }
    }
    let Some((source, total)) = fitting else {
        let (path, built) = first_mismatch.unwrap_or_default();
        return Err(DeployError::Mismatch { path, built, target, platform });
    };
    if runner.is_local() {
        return in_place(source);
    }
    let mut before = 0_u64;
    for &name in names {
        on(Event::Step(Step::Upload { name }));
        before = before
            .saturating_add(upload(runner, &source.join(name), name, (before, total), on).await?);
    }
    Ok(STAGE.to_owned())
}

/// Whether the binaries `names` in `dir` all run on `platform`: their bytes together when they
/// do, else the first that does not (or is not there) and what it is built for.
fn fits(
    dir: &Path,
    names: &[&str],
    platform: Platform,
) -> Result<Result<u64, (PathBuf, Option<Platform>)>, DeployError> {
    let mut total = 0_u64;
    for name in names {
        let path = dir.join(name);
        let runs_on = match platform::platforms_of(&path) {
            Ok(runs_on) => runs_on,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(DeployError::Read { path, source }),
        };
        if !runs_on.contains(&platform) {
            return Ok(Err((path, runs_on.first().copied())));
        }
        let len =
            std::fs::metadata(&path).map_err(|source| DeployError::Read { path, source })?.len();
        total = total.saturating_add(len);
    }
    Ok(Ok(total))
}

/// Where an app bundle keeps the builds for other machines, under `Contents/Resources`: one
/// directory per platform, named as [`BUNDLED`] says, holding a worker's binaries and a
/// server's.
pub const BUNDLED_DIR: &str = "workers";

/// The other platforms' directories in [`BUNDLED_DIR`], and the cross-build's triple for each
/// (`cargo xtask linux` in a dev tree).
pub const BUNDLED: [(&str, &str); 2] =
    [("linux-arm64", "aarch64-unknown-linux-gnu"), ("linux-x86_64", "x86_64-unknown-linux-gnu")];

/// Every directory of binaries that `dir`, the one this program runs from, can deploy.
///
/// That is `dir` itself, then the other platforms' builds an app bundle carries beside it
/// (`Contents/Resources/workers/<platform>`), or, in a dev tree (`target/<profile>`), the Linux
/// cross-builds of the same profile (`target/linux/<triple>/<profile>`). Only those that hold
/// the CLI, which every install runs.
#[must_use]
pub fn bundled(dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![dir.to_path_buf()];
    let contents = dir.parent().filter(|_| dir.ends_with("Contents/MacOS"));
    let profile = dir.file_name();
    let target = dir.parent().filter(|t| t.file_name().is_some_and(|n| n == "target"));
    for (name, triple) in BUNDLED {
        let other = match (contents, target, profile) {
            (Some(contents), ..) => contents.join("Resources").join(BUNDLED_DIR).join(name),
            (None, Some(target), Some(profile)) => target.join("linux").join(triple).join(profile),
            _ => continue,
        };
        if other.join("slopty").is_file() {
            dirs.push(other);
        }
    }
    dirs
}

/// The first step there: its OS and CPU, then both ends of the `ssh` connection (empty when
/// the login is not `sshd`'s).
const REACH: &str = r#"uname -sm && echo "$SSH_CONNECTION""#;

/// `dir` as a word of a script, double-quoted: it runs in place, so it may hold spaces.
fn in_place(dir: &Path) -> Result<String, DeployError> {
    let text = dir.to_str().filter(|t| !t.contains(['"', '$', '`', '\\', '\'', '\n']));
    text.map(|t| format!("\"{t}\"")).ok_or_else(|| DeployError::Path { path: dir.to_path_buf() })
}

/// The runner's own failure to run a script.
fn run_error(runner: &dyn Runner, source: std::io::Error) -> DeployError {
    DeployError::Run { program: runner.program(), source }
}

/// What `script` printed; its error output when it fails.
async fn output(
    runner: &dyn Runner,
    script: &str,
    on: &mut OnEvent<'_>,
) -> Result<String, DeployError> {
    let job = Job { script, input: None, watch: false };
    let ran = runner.run(job, on).await.map_err(|e| run_error(runner, e))?;
    if !ran.status.success() {
        return Err(DeployError::Failed {
            script: script.to_owned(),
            target: runner.target().to_owned(),
            status: ran.status,
            stderr: ran.stderr.trim().to_owned(),
        });
    }
    Ok(ran.stdout.trim().to_owned())
}

/// `script` with the person watching; its last lines kept for a failure.
async fn install(
    runner: &dyn Runner,
    script: &str,
    on: &mut OnEvent<'_>,
) -> Result<(), DeployError> {
    let mut tail = VecDeque::with_capacity(TAIL);
    let mut keep = |event: Event| {
        if let Event::Line(line) = &event {
            if tail.len() == TAIL {
                tail.pop_front();
            }
            tail.push_back(line.clone());
        }
        on(event);
    };
    let job = Job { script, input: None, watch: true };
    let ran = runner.run(job, &mut keep).await.map_err(|e| run_error(runner, e))?;
    if !ran.status.success() {
        return Err(DeployError::Install {
            script: script.to_owned(),
            target: runner.target().to_owned(),
            status: ran.status,
            tail: tail.into(),
        });
    }
    Ok(())
}

/// Copy `from` to `<STAGE>/<name>` there, executable, replacing it only once it is whole; the
/// bytes it took. `(before, total)` places its bytes among all of them for [`Event::Sent`].
async fn upload(
    runner: &dyn Runner,
    from: &Path,
    name: &'static str,
    (before, total): (u64, u64),
    on: &mut OnEvent<'_>,
) -> Result<u64, DeployError> {
    let open = |source| DeployError::Open { path: from.to_path_buf(), source };
    let file = std::fs::File::open(from).map_err(open)?;
    let len = file.metadata().map_err(open)?.len();
    let part = format!("{STAGE}/{name}.part");
    let script = format!(
        "mkdir -p {STAGE} && cat > {part} && chmod 755 {part} && mv -f {part} {STAGE}/{name}"
    );
    let mut placed = |event: Event| match event {
        Event::Sent { sent, .. } => on(Event::Sent { sent: before.saturating_add(sent), total }),
        other => on(other),
    };
    let job = Job { script: &script, input: Some((file, len)), watch: false };
    let ran = runner.run(job, &mut placed).await.map_err(|e| run_error(runner, e))?;
    if !ran.status.success() {
        return Err(DeployError::Upload {
            name,
            target: runner.target().to_owned(),
            status: ran.status,
            stderr: ran.stderr.trim().to_owned(),
        });
    }
    Ok(len)
}

#[cfg(test)]
mod tests;
