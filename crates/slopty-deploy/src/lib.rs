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
//!    side reaches a server that runs on the deploying machine, and on a Mac whether someone is
//!    logged in at it and whether `FileVault` is on ([`Console`]): Slopty runs in a login session.
//! 2. `slopty-ptyd`, `slopty-worker` and `slopty` go to [`STAGE`] under the remote home, each
//!    written beside its name and moved over it once whole.
//! 3. The uploaded `slopty worker install` runs there, which installs the services the way a local
//!    install does, saves the server to register with ([`Plan::server`]), waits for the new worker
//!    to answer as itself, and with `--update` puts the previous worker back when it does not. An
//!    update first asks it what the install would do to `slopty-ptyd` ([`Ptyd`]): kept, every shell
//!    and agent turn there goes on; restarted while it holds sessions, the deploy stops there,
//!    having changed nothing ([`DeployError::EndsSessions`]), until the person says to end them
//!    ([`Plan::end_sessions`]).
//! 4. `slopty worker doctor --json` there says what it can do; a Mac still needs a person at the
//!    desk for Screen Recording and Accessibility. `slopty worker service --json` says whether the
//!    services stop when the person logs out there ([`Deployed::stops_at_logout`]).
//! 5. With [`Plan::add_key`], the person's public key goes into `~/.ssh/authorized_keys` there, as
//!    `ssh-copy-id` puts it, so the machine stops asking for a password ([`Key`]).
//!
//! A machine that takes only a password is signed in to once, before the first step, with the
//! password the person typed ([`Plan::password`]): one connection that every step shares, its
//! question answered through `ssh`'s askpass door ([`askpass`]). The password is never kept.
//!
//! [`serve`] puts the server on a machine the same way: the same first step and upload, then
//! `slopty server install`, which waits until it answers there.
//!
//! Every step goes through a [`Runner`], so a test drives the plan with no machine. [`Local`]
//! runs the same plan on this machine with no `ssh` and no upload: it installs the binaries
//! where they are, which is how the app updates its own Mac's worker without Remote Login.
//!
//! A machine whose host key `ssh` does not know yet is offered to the person by its fingerprint
//! ([`Ssh::explain`], [`HostKey`]), since the app's `ssh` never asks.

#![forbid(unsafe_code)]

pub mod askpass;
mod platform;
mod ssh;
mod target;
mod trust;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

pub use platform::{Arch, Os, Platform, Unsupported};
pub use secrecy::{ExposeSecret, SecretString};
pub use slopty_platform::service::{InstallPlan, Ptyd, Removed};
use slopty_platform::service::{NOBODY_LOGGED_IN, Report, WORKER_BINARIES};
use slopty_proto::ctl::Health;
pub use ssh::{ALIVE, Echo, Job, Local, OnEvent, PERSIST, Pending, Ran, Runner, Signed, Ssh};
pub use target::{REMEMBERED, Remembered, Server, Target};
pub use trust::{Fingerprint, HostKey, TrustError};

/// Where the binaries land on the remote host, relative to its home (where `ssh` starts).
pub const STAGE: &str = ".slopty/deploy";

/// How many of the install's last lines a failure keeps.
pub const TAIL: usize = 8;

/// The longest a step that only asks something there may take: a script that answers at once,
/// on a link `ssh` keeps alive ([`ALIVE`]). Past it the step fails as
/// [`DeployError::Stalled`].
pub const ASK_LIMIT: Duration = Duration::from_mins(2);
/// The longest the install there may take, with the person watching: it stops and starts the
/// worker's services and waits for them.
pub const INSTALL_LIMIT: Duration = Duration::from_mins(10);
/// How long an upload may take before its bytes count ([`upload_limit`]).
pub const UPLOAD_BASE: Duration = Duration::from_mins(1);
/// The slowest an upload may go, in bytes a second, before it counts as stalled.
pub const UPLOAD_FLOOR: u64 = 64 * 1024;

/// The longest an upload of `bytes` may take: [`UPLOAD_BASE`], and its bytes at
/// [`UPLOAD_FLOOR`].
#[must_use]
pub const fn upload_limit(bytes: u64) -> Duration {
    UPLOAD_BASE.saturating_add(Duration::from_secs(bytes.div_ceil(UPLOAD_FLOOR)))
}

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
    /// The server the worker registers with, so every client of it lists the machine.
    pub server: Server,
    /// The person said to go on when the update restarts `slopty-ptyd`, ending every session
    /// it holds there. Without it such an update stops before changing anything
    /// ([`DeployError::EndsSessions`]).
    pub end_sessions: bool,
    /// The password the machine asks for instead of a key, as the person typed it: the deploy
    /// signs in with it once ([`Runner::sign_in`]) and every step shares that sign-in. It goes
    /// only to `ssh`'s askpass, and is wiped when the plan is dropped.
    pub password: Option<SecretString>,
    /// Put the person's public key into the machine's `authorized_keys` once the worker is up,
    /// so it stops asking for a password ([`Deployed::key`]).
    pub add_key: bool,
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
    /// The uploaded `slopty worker uninstall --purge` running there ([`remove`]).
    Remove,
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
    /// What an update does to `slopty-ptyd` there, once the machine said.
    Ptyd(Ptyd),
}

/// What a deploy left running there.
#[derive(Clone, PartialEq, Debug)]
pub struct Deployed {
    /// The machine.
    pub platform: Platform,
    /// The worker's own account of itself.
    pub health: Health,
    /// The server address the install saved for it to register with: [`Plan::server`] as the
    /// machine reaches it.
    pub server: String,
    /// What an update did to `slopty-ptyd` there; `None` for a fresh install.
    pub ptyd: Option<Ptyd>,
    /// What the person must do there so the worker outlives their last logout, when it does
    /// not (a Linux user that does not linger); `None` when it does, or the machine did not say.
    pub stops_at_logout: Option<String>,
    /// Whether someone is logged in at the machine and whether `FileVault` is on there.
    pub console: Console,
    /// What became of the person's key, when the plan said to add it ([`Plan::add_key`]).
    pub key: Option<Key>,
}

/// Who is at a Mac, and whether its disk is locked after a restart.
///
/// Slopty runs in the session of whoever is logged in, so a Mac nobody is logged in at runs
/// nothing of it, and a `FileVault` disk waits for its password after a restart before anyone
/// can log in. Each is `None` off a Mac, or when the machine did not say.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Console {
    /// Someone is logged in at its screen (`/dev/console` is not root's).
    pub logged_in: Option<bool>,
    /// `FileVault` is on (`fdesetup isactive`).
    pub filevault: Option<bool>,
}

/// What became of the person's public key on the machine ([`Plan::add_key`]).
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(
    clippy::enum_variant_names,
    reason = "`NoPublicKey` names what is missing, which no shorter word says"
)]
pub enum Key {
    /// It is in `authorized_keys` now.
    Added,
    /// It was there already.
    AlreadyThere,
    /// This machine has no public key to add: no key in the agent, none in `~/.ssh`.
    NoPublicKey,
    /// The machine would not take it; what it said.
    Failed(String),
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
    /// The update ends work running there, and the plan did not say to
    /// ([`Plan::end_sessions`]): it restarts `slopty-ptyd`, ending every session it holds, or
    /// agent turns run in threads the worker drives itself. Nothing was changed.
    #[error(
        "the update ends {} on {target}; pass --end-sessions to go on",
        slopty_platform::service::ended_said(*ptyd, *turns)
    )]
    EndsSessions {
        /// The machine.
        target: String,
        /// What it would do to `slopty-ptyd`.
        ptyd: Ptyd,
        /// The driven turns it would end.
        turns: usize,
    },
    /// The machine runs a newer build than the one the update would put there: a deploy never
    /// takes a machine back. Nothing was changed.
    #[error("{target} runs {running}, newer than {build}; update this machine's Slopty instead")]
    Older {
        /// The machine.
        target: String,
        /// The build its worker runs.
        running: String,
        /// The build the update would have put there.
        build: String,
    },
    /// A step there did not end within its limit: the link stalled, or the script hung. The
    /// step was stopped.
    #[error("`{script}` on {target} did not end within {} s", limit.as_secs())]
    Stalled {
        /// The script.
        script: String,
        /// The machine.
        target: String,
        /// How long it was given.
        limit: Duration,
    },
    /// The sign-in with a password failed: `ssh` could not reach the machine, or the machine
    /// refused the password.
    #[error("signing in to {target} failed{}: {stderr}", ended(*status))]
    SignIn {
        /// The machine.
        target: String,
        /// How the signing-in `ssh` ended; `None` when it did not in time.
        status: Option<ExitStatus>,
        /// What it printed, trimmed.
        stderr: String,
    },
    /// What the machine said an install would do was not a plan.
    #[error("the install's plan said {said:?}: {error}")]
    Plan {
        /// What it printed.
        said: String,
        /// Why that is not a plan.
        error: serde_json::Error,
    },
    /// The machine has no address for the server the plan names: it is this machine at
    /// loopback and the machine did not say where it was reached from, or the name holds what
    /// a shell would read as more than an address. Nothing was installed.
    #[error(
        "{target} has no address for the server at {server}; pass --server with one it reaches"
    )]
    NoServerAddress {
        /// The machine.
        target: String,
        /// The server as the plan named it.
        server: String,
    },
    /// What the uninstall there said it removed was not a removal.
    #[error("the uninstall said {said:?}: {error}")]
    NotRemoved {
        /// What it printed.
        said: String,
        /// Why that is not a removal.
        error: serde_json::Error,
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

/// How a sign-in ended, as its error says it.
fn ended(status: Option<ExitStatus>) -> String {
    status.map_or_else(|| " (no answer in time)".to_owned(), |status| format!(" ({status})"))
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
    /// The machine's host key, new to this machine's `ssh`, which the person may trust to go
    /// on ([`HostKey::offer`]).
    pub trust: Option<Box<HostKey>>,
    /// The machine asks for a password instead of a key, which the person may type to go on
    /// ([`Plan::password`]).
    pub password: Option<Box<PasswordAsk>>,
    /// The update stopped before restarting `slopty-ptyd`, which would end the sessions it holds
    /// ([`DeployError::EndsSessions`]): the person may say to go on ([`Plan::end_sessions`]).
    pub ends_sessions: Option<Ptyd>,
}

impl Failure {
    /// A failure with nothing to trust, no password to ask and no sessions to end.
    #[must_use]
    pub const fn new(title: String, hint: Option<String>, lines: Vec<String>) -> Self {
        Self { title, hint, lines, trust: None, password: None, ends_sessions: None }
    }
}

/// A machine that asks for a password: whose, and whether one given was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PasswordAsk {
    /// The user it asks for, as `ssh` signed in; empty when `ssh` did not say.
    pub user: String,
    /// The machine, as the person named it.
    pub host: String,
    /// A password was given and the machine said no.
    pub refused: bool,
}

/// The hint for a machine nobody is logged in at.
const LOG_IN: &str = "Slopty runs in the session of whoever is logged in there. On a Mac nobody \
                      sits at, turn on automatic login in Users & Groups, which needs FileVault \
                      off; with FileVault on, unlock its disk with ssh after a restart, then log \
                      in through Screen Sharing.";

/// Whether `said` is launchd finding no login session to start Slopty in: the install's own
/// word for it, or `launchctl`'s.
fn nobody_logged_in(said: &str) -> bool {
    [NOBODY_LOGGED_IN, "Domain does not support specified action", "Could not find domain for"]
        .iter()
        .any(|words| said.contains(words))
}

/// `ssh`'s own exit status when it could not connect or sign in.
const SSH_FAILED: i32 = 255;

/// What `ssh` prints when a host key it knows no longer matches, or a known address shows
/// another key (`CheckHostIP`): never a key to trust from here.
const KEY_CHANGED: [&str; 3] = [
    "REMOTE HOST IDENTIFICATION HAS CHANGED",
    "has changed and you have requested",
    "DNS SPOOFING",
];

impl DeployError {
    /// This failure as a window says it.
    #[must_use]
    pub fn failure(&self) -> Failure {
        let plain = |title: String, lines: Vec<String>| Failure::new(title, None, lines);
        match self {
            Self::NotATarget(target) => plain(format!("{target} is not a host name"), Vec::new()),
            Self::Run { program, source } => Failure::new(
                format!("Could not start {program}"),
                Some("Slopty uses the ssh on this Mac's PATH.".to_owned()),
                vec![source.to_string()],
            ),
            Self::Failed { target, stderr, .. } if nobody_logged_in(stderr) => Failure::new(
                format!("Nobody is logged in at {target}"),
                Some(LOG_IN.to_owned()),
                last_lines(stderr),
            ),
            Self::Failed { target, status, stderr, .. } => {
                ssh_failure(target, *status, stderr, || format!("A step failed on {target}"))
            }
            Self::SignIn { target, status, stderr } => signing_in(target, *status, stderr),
            Self::Stalled { target, limit, .. } => Failure::new(
                format!("{target} stopped answering"),
                Some(format!(
                    "A step there went on past {} minutes without ending, so it was stopped. \
                     Check that it is awake and on your tailnet or VPN, then try again.",
                    limit.as_secs().div_ceil(60)
                )),
                Vec::new(),
            ),
            Self::Upload { name, target, status, stderr } => {
                ssh_failure(target, *status, stderr, || {
                    format!("Could not copy {name} to {target}")
                })
            }
            Self::NoServerAddress { target, server } => Failure::new(
                format!("{target} has no address for the server"),
                Some("Connect to the server by an address other machines reach.".to_owned()),
                vec![server.clone()],
            ),
            Self::Machine { target, source } => Failure::new(
                format!("No worker runs on {target}"),
                Some("Workers run on macOS and Linux, on arm64 or x86_64.".to_owned()),
                vec![source.to_string()],
            ),
            Self::Path { path } => Failure::new(
                "Slopty is in a folder the install cannot name".to_owned(),
                Some("Move Slopty to a folder whose name has no quotes, $ or \\.".to_owned()),
                vec![path.display().to_string()],
            ),
            Self::Read { path, source } | Self::Open { path, source } => Failure::new(
                "Could not read the worker to send".to_owned(),
                None,
                vec![format!("{}: {source}", path.display())],
            ),
            Self::Mismatch { built, target, platform, .. } => Failure::new(
                format!("This build has no worker for {platform}"),
                Some(format!(
                    "{target} runs {platform}; this app carries a worker for {}.",
                    built_for(built.as_ref())
                )),
                Vec::new(),
            ),
            Self::Install { target, tail, .. } if tail.iter().any(|l| nobody_logged_in(l)) => {
                Failure::new(
                    format!("Nobody is logged in at {target}"),
                    Some(LOG_IN.to_owned()),
                    tail.clone(),
                )
            }
            Self::Install { target, tail, .. } => {
                plain(format!("The install on {target} failed"), tail.clone())
            }
            Self::EndsSessions { target, ptyd, turns } => Failure {
                ends_sessions: self.ends_sessions(),
                ..Failure::new(
                    format!(
                        "Updating ends {} on {target}",
                        slopty_platform::service::ended_said(*ptyd, *turns)
                    ),
                    Some(if ptyd.ends_sessions() {
                        "Its shell keeper changed too, so every shell and agent turn there ends. \
                         Try again to update anyway."
                            .to_owned()
                    } else {
                        "pi and ACP agents there stop mid-turn when it restarts. Try again once \
                         they rest, or now to update anyway."
                            .to_owned()
                    }),
                    Vec::new(),
                )
            },
            Self::Older { target, running, build } => Failure::new(
                format!("{target} runs a newer Slopty"),
                Some("Update Slopty on this Mac; a machine is never taken back.".to_owned()),
                vec![format!("{target} runs {running}"), format!("This Mac has {build}")],
            ),
            Self::Plan { said, .. } => {
                plain("The new worker could not say what it changes".to_owned(), last_lines(said))
            }
            Self::Doctor { said, .. } => {
                plain("The new worker did not report back".to_owned(), last_lines(said))
            }
            Self::NotRemoved { said, .. } => {
                plain("The uninstall did not say what it removed".to_owned(), last_lines(said))
            }
        }
    }

    /// The restart of `slopty-ptyd` the update stopped at, when it stopped because that ends
    /// sessions ([`Self::EndsSessions`]); the next run with [`Plan::end_sessions`] goes on.
    #[must_use]
    pub const fn ends_sessions(&self) -> Option<Ptyd> {
        match self {
            Self::EndsSessions { ptyd, .. } => Some(*ptyd),
            _ => None,
        }
    }

    /// Whether `ssh` stopped at a host key it does not know yet: one the person may check and
    /// trust ([`Ssh::explain`]). A key that changed is not one.
    #[must_use]
    pub fn unknown_host_key(&self) -> bool {
        match self {
            Self::Failed { status, stderr, .. } | Self::Upload { status, stderr, .. } => {
                status.code() == Some(SSH_FAILED)
                    && stderr.contains("Host key verification failed")
                    && !KEY_CHANGED.iter().any(|said| stderr.contains(said))
            }
            _ => false,
        }
    }
}

/// A failed sign-in with a password: a password refused asks for it again, and anything else is
/// named as `ssh`'s own failure is.
fn signing_in(target: &str, status: Option<ExitStatus>, stderr: &str) -> Failure {
    let Some(status) = status else {
        let hint = Some("Check that it is awake and on your tailnet or VPN.".to_owned());
        return Failure::new(format!("{target} did not answer"), hint, last_lines(stderr));
    };
    let mut failure =
        ssh_failure(target, status, stderr, || format!("Could not sign in to {target}"));
    if let Some(ask) = &mut failure.password {
        ask.refused = true;
        failure.title = format!("{target} did not take that password");
        failure.hint = Some("Type it again.".to_owned());
    }
    failure
}

/// The machine asks for a password: `ssh` was refused, and the ways it may sign in that it
/// named (`Permission denied (publickey,password).`) hold one a person types.
fn asks_password(target: &str, stderr: &str) -> Option<PasswordAsk> {
    let line = stderr.lines().find(|l| l.contains("Permission denied ("))?;
    let (before, methods) = line.split_once("Permission denied (")?;
    let methods = methods.split(')').next()?;
    let typed = methods.split(',').any(|m| matches!(m.trim(), "password" | "keyboard-interactive"));
    if !typed {
        return None;
    }
    let user = before
        .trim_end()
        .trim_end_matches(':')
        .rsplit_once('@')
        .map(|(user, _)| user.rsplit(' ').next().unwrap_or(user).to_owned())
        .unwrap_or_default();
    Some(PasswordAsk { user, host: target.to_owned(), refused: false })
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
        return Failure::new(other(), None, lines);
    }
    let has = |words: &str| stderr.contains(words);
    let (title, hint) = if KEY_CHANGED.iter().any(|said| has(said)) {
        (
            format!("{target}'s host key has changed"),
            Some(
                "Someone may be in between. If the machine was set up again, remove its old key \
                 with ssh-keygen -R in a terminal, then try again.",
            ),
        )
    } else if has("Host key verification failed") {
        (
            format!("{target}'s host key is not trusted yet"),
            Some("Try again to see its key here and trust it with Trust and install."),
        )
    } else if let Some(ask) = asks_password(target, stderr) {
        let whose = if ask.user.is_empty() { "a".to_owned() } else { format!("{}'s", ask.user) };
        let title = format!("{target} asks for {whose} password");
        let hint = "Type it once to install. Slopty hands it to ssh and keeps nothing, then can \
                    add your key so it stops asking.";
        return Failure {
            password: Some(Box::new(ask)),
            ..Failure::new(title, Some(hint.to_owned()), lines)
        };
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
    Failure::new(title, hint.map(str::to_owned), lines)
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
/// When the sign-in with [`Plan::password`] fails, the machine or the binaries do not fit, a
/// step there fails, or the new worker did not come up (after an update, the previous one is
/// back then).
pub async fn deploy(
    runner: &dyn Runner,
    plan: &Plan,
    on: &mut OnEvent<'_>,
) -> Result<Deployed, DeployError> {
    let signed = match &plan.password {
        Some(password) => {
            named(runner)?;
            runner.sign_in(password).await?
        }
        None => None,
    };
    let runner = signed.as_deref().unwrap_or(runner);
    let reached = reach(runner, on).await?;
    let server = if runner.is_local() {
        plan.server.address()
    } else {
        let route = if plan.server.is_here() { runner.tailnet_route().await } else { None };
        let route = route.map(|ip| ip.to_string());
        plan.server.seen_from(route.as_deref().or(reached.client.as_deref()))
    }
    .ok_or_else(|| DeployError::NoServerAddress {
        target: runner.target().to_owned(),
        server: format!("{}:{}", plan.server.host, plan.server.port),
    })?;
    let bin = send(runner, &plan.sources, &WORKER_BINARIES, reached.platform, on).await?;
    let mode = if plan.update { "--update" } else { "--fresh" };
    let register = format!(" --server {server}");
    let ptyd = if plan.update { Some(ptyd_plan(runner, &bin, plan, on).await?) } else { None };
    let end = if plan.update && plan.end_sessions { " --end-sessions" } else { "" };
    on(Event::Step(Step::Install));
    let script = format!("{bin}/slopty{register} worker install --bin-dir {bin} {mode}{end}");
    install(runner, &script, on).await?;
    on(Event::Step(Step::Check));
    let doctor = output(runner, &format!("{bin}/slopty --json worker doctor"), on).await?;
    let health: Health = serde_json::from_str(&doctor)
        .map_err(|error| DeployError::Doctor { said: doctor, error })?;
    let stops_at_logout = report(runner, &bin, on).await.and_then(|r| r.stops_at_logout);
    let key = if plan.add_key && !runner.is_local() {
        Some(match public_key().await {
            Some(key) => add_key(runner, &key, on).await,
            None => Key::NoPublicKey,
        })
    } else {
        None
    };
    let console = reached.console;
    Ok(Deployed { platform: reached.platform, health, server, ptyd, stops_at_logout, console, key })
}

/// What a removal takes to the machine: the CLI, whose purge runs there.
pub const REMOVER: [&str; 1] = ["slopty"];

/// Take the worker off `runner`'s machine, and everything else of its there; what it removed,
/// as it said.
///
/// That is `slopty worker uninstall --purge`: its services, its own files, its logs, the
/// deploy's stage and Slopty's hook entries. This build's CLI goes up to [`STAGE`] first, from the
/// first of `sources` built for the machine, so the purge is this build's and not that of whatever
/// worker is there; on this machine it runs where it is.
///
/// The person's repositories, worktrees and the agents' own sessions there are not touched:
/// the purge names what it takes ([`slopty_platform::service::WORKER_STATE`]).
///
/// # Errors
///
/// When the sign-in with `password` fails, the machine or the CLI do not fit, the purge there
/// fails, or what it said is not a removal.
pub async fn remove(
    runner: &dyn Runner,
    sources: &[PathBuf],
    password: Option<&SecretString>,
    on: &mut OnEvent<'_>,
) -> Result<Removed, DeployError> {
    let signed = match password {
        Some(password) => {
            named(runner)?;
            runner.sign_in(password).await?
        }
        None => None,
    };
    let runner = signed.as_deref().unwrap_or(runner);
    let reached = reach(runner, on).await?;
    let bin = send(runner, sources, &REMOVER, reached.platform, on).await?;
    on(Event::Step(Step::Remove));
    let said = output(runner, &format!("{bin}/slopty --json worker uninstall --purge"), on).await?;
    serde_json::from_str(&said).map_err(|error| DeployError::NotRemoved { said, error })
}

/// The person's public key, as `ssh-copy-id` picks it: the first the agent lists
/// (`ssh-add -L`), else one in `~/.ssh` ([`choose_key`]). A private key is never read.
pub async fn public_key() -> Option<String> {
    let listed = tokio::process::Command::new("ssh-add")
        .arg("-L")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    choose_key(&listed, &slopty_platform::dirs::home().join(".ssh"))
}

/// The public key to add, as `ssh-copy-id` picks it.
///
/// The first of `listed` (what `ssh-add -L` printed), else the newest `id*.pub` in `ssh_dir`,
/// else its newest `*.pub`. Only files named `.pub` are opened, so only public halves are read;
/// a line that is not a key is passed over.
#[must_use]
pub fn choose_key(listed: &str, ssh_dir: &Path) -> Option<String> {
    if let Some(key) = listed.lines().find_map(key_line) {
        return Some(key);
    }
    let mut public: Vec<(bool, std::time::SystemTime, PathBuf)> = std::fs::read_dir(ssh_dir)
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let file = !entry.file_type().ok()?.is_dir();
            let modified = entry.metadata().ok()?.modified().ok()?;
            let public = Path::new(&name).extension().is_some_and(|e| e == "pub");
            (file && public).then(|| (name.starts_with("id"), modified, entry.path()))
        })
        .collect();
    public.sort_by_key(|(id, modified, _)| std::cmp::Reverse((*id, *modified)));
    public
        .iter()
        .find_map(|(.., path)| std::fs::read_to_string(path).ok()?.lines().find_map(key_line))
}

/// `line` as a key line a script can carry: `<type> <base64> [comment]`, with a comment that holds
/// what a shell would read as more than words dropped.
fn key_line(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let (kind, blob) = (words.next()?, words.next()?);
    let typed = ["ssh-", "ecdsa-", "sk-"].iter().any(|t| kind.starts_with(t))
        && kind.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '@' | '.'));
    let base64 = blob.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    if !typed || !base64 {
        return None;
    }
    let comment = words.collect::<Vec<_>>().join(" ");
    let plain = comment
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '@' | '.' | '_' | '+' | '-' | ':'));
    Some(if comment.is_empty() || !plain {
        format!("{kind} {blob}")
    } else {
        format!("{kind} {blob} {comment}")
    })
}

/// Put `key` (a line [`choose_key`] gave) into `~/.ssh/authorized_keys` there, as `ssh-copy-id`
/// does.
///
/// Nothing is added when it is there already. The directory is made private (`0700`) and the
/// file only its owner's (`0600`) when they are new, a last line with no end is ended first, and
/// `SELinux`'s labels are put back where it labels them.
pub async fn add_key(runner: &dyn Runner, key: &str, on: &mut OnEvent<'_>) -> Key {
    let Some(line) = key_line(key) else { return Key::NoPublicKey };
    let blob = line.split(' ').take(2).collect::<Vec<_>>().join(" ");
    let script = format!(
        r#"umask 077 && k="$HOME/.ssh" && f="$k/authorized_keys" && {{ [ -d "$k" ] || mkdir -m 700 "$k"; }} && {{ [ -f "$f" ] || : > "$f"; }} && if grep -qF "{blob}" "$f"; then echo there; else {{ [ ! -s "$f" ] || [ -z "$(tail -c 1 "$f")" ] || echo >> "$f"; }} && echo "{line}" >> "$f" && {{ ! command -v restorecon > /dev/null 2>&1 || restorecon -F "$k" "$f"; }} && echo added; fi"#
    );
    match output(runner, &script, on).await {
        Ok(said) if said.ends_with("added") => Key::Added,
        Ok(said) if said.ends_with("there") => Key::AlreadyThere,
        Ok(said) => Key::Failed(said),
        Err(e) => {
            tracing::warn!(error = %e, "add the key");
            Key::Failed(match e {
                DeployError::Failed { stderr, .. } => stderr,
                other => other.to_string(),
            })
        }
    }
}

/// A runner whose target is not an option in disguise.
fn named(runner: &dyn Runner) -> Result<(), DeployError> {
    let target = runner.target();
    if target.starts_with('-') {
        return Err(DeployError::NotATarget(target.to_owned()));
    }
    Ok(())
}

/// What the install there would do to `slopty-ptyd`; an error, before anything there changed,
/// when it would take the machine back to an older build than the one running, or end
/// sessions the plan did not say to end.
async fn ptyd_plan(
    runner: &dyn Runner,
    bin: &str,
    plan: &Plan,
    on: &mut OnEvent<'_>,
) -> Result<Ptyd, DeployError> {
    let script = format!("{bin}/slopty --json worker install --bin-dir {bin} --update --plan");
    let said = output(runner, &script, on).await?;
    let install: InstallPlan =
        serde_json::from_str(&said).map_err(|error| DeployError::Plan { said, error })?;
    if install.older() {
        return Err(DeployError::Older {
            target: runner.target().to_owned(),
            running: install.running.unwrap_or_default(),
            build: install.build,
        });
    }
    let ptyd = install.ptyd;
    on(Event::Ptyd(ptyd));
    if install.ends_work() && !plan.end_sessions {
        let target = runner.target().to_owned();
        return Err(DeployError::EndsSessions { target, ptyd, turns: install.turns });
    }
    Ok(ptyd)
}

/// The services there as `slopty worker service` reports them; `None`, logged, when it does
/// not: the worker is up, and a note it could not read is no reason to fail the deploy.
async fn report(runner: &dyn Runner, bin: &str, on: &mut OnEvent<'_>) -> Option<Report> {
    let said = output(runner, &format!("{bin}/slopty --json worker service"), on)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "read the services there"))
        .ok()?;
    serde_json::from_str(&said)
        .inspect_err(|e| tracing::warn!(error = %e, said, "the services' report"))
        .ok()
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
    /// Who is at it, on a Mac.
    console: Console,
}

/// The first step: the machine's OS and CPU and, over `ssh`, the two ends of the connection.
async fn reach(runner: &dyn Runner, on: &mut OnEvent<'_>) -> Result<Reached, DeployError> {
    named(runner)?;
    let target = runner.target();
    on(Event::Step(Step::Reach));
    let local = runner.is_local();
    let said = output(runner, REACH, on).await?;
    let console = Console {
        logged_in: said.lines().find_map(|l| l.strip_prefix("console=")).map(|user| {
            let user = user.trim();
            !user.is_empty() && user != "root"
        }),
        filevault: said.lines().find_map(|l| match l.strip_prefix("filevault=")?.trim() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }),
    };
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
    Ok(Reached { platform, client: ends.next(), here: ends.next(), console })
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
/// the login is not `sshd`'s), then on a Mac who owns the console (`root` at the login window)
/// and whether `FileVault` is on, which neither needs an administrator.
const REACH: &str = r#"uname -sm && echo "$SSH_CONNECTION" && if [ "$(uname -s)" = Darwin ]; then echo "console=$(stat -f %Su /dev/console 2>/dev/null)"; echo "filevault=$(fdesetup isactive 2>/dev/null)"; fi"#;

/// `dir` as a word of a script, double-quoted: it runs in place, so it may hold spaces.
fn in_place(dir: &Path) -> Result<String, DeployError> {
    let text = dir.to_str().filter(|t| !t.contains(['"', '$', '`', '\\', '\'', '\n']));
    text.map(|t| format!("\"{t}\"")).ok_or_else(|| DeployError::Path { path: dir.to_path_buf() })
}

/// The runner's own failure to run a script.
fn run_error(runner: &dyn Runner, source: std::io::Error) -> DeployError {
    DeployError::Run { program: runner.program(), source }
}

/// Run `job` on `runner`, stopping it once it has taken `limit`: dropped, the runner's child
/// is killed.
async fn run_within(
    runner: &dyn Runner,
    job: Job<'_>,
    on: &mut OnEvent<'_>,
    limit: Duration,
) -> Result<Ran, DeployError> {
    let script = job.script.to_owned();
    match tokio::time::timeout(limit, runner.run(job, on)).await {
        Ok(ran) => ran.map_err(|e| run_error(runner, e)),
        Err(_elapsed) => {
            Err(DeployError::Stalled { script, target: runner.target().to_owned(), limit })
        }
    }
}

/// What `script` printed; its error output when it fails.
async fn output(
    runner: &dyn Runner,
    script: &str,
    on: &mut OnEvent<'_>,
) -> Result<String, DeployError> {
    let job = Job { script, input: None, watch: false };
    let ran = run_within(runner, job, on, ASK_LIMIT).await?;
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
    let ran = run_within(runner, job, &mut keep, INSTALL_LIMIT).await?;
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
    let ran = run_within(runner, job, &mut placed, upload_limit(len)).await?;
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
