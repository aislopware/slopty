//! "Use this Mac": a server joined or started here, then this Mac's worker installed.
//!
//! The worker's services are installed from this app, its `doctor` read as a checklist until
//! it may stream and take input, and the flow is done once the server's directory lists this
//! Mac.
//!
//! Which server ([`Serve`]) is the app's to decide before anything is installed: the one set,
//! else the one Ready server the tailnet answered with, else one started here. Several, or no
//! look possible, and the person is asked. The installs are the CLI's own
//! ([`slopty_platform::service::install_server`] and
//! [`slopty_platform::service::install_worker`]), from the binaries beside the app: in place
//! inside `Slopty.app`, copied out of a dev tree. The worker registers with that server
//! ([`slopty_settings::join_server`]), so every client of it lists this Mac. The checklist is the
//! worker's own `doctor` ([`slopty_proto::ctl::Health`]), so it says what `slopty worker doctor`
//! says. Everything that touches the machine goes through a [`Host`]: [`native`] on the Mac, and a
//! stand-in under test, so no test installs an agent, raises a permission prompt or opens System
//! Settings (`docs/decisions/platform.md`, "This Mac as a worker").

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;

use slopty_core::WorkerId;
use slopty_net::HostAddr;
pub use slopty_platform::privacy::Pane;
use slopty_proto::ctl::LinkState;
use slopty_proto::tailnet::BackendState;

pub mod actions {
    //! The palette's way to this Mac's checklist after the first run.
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        workers,
        [
            /// Install the worker on this Mac and walk its permissions until the server lists
            /// it.
            UseThisMac,
        ]
    );
}

/// The entry's words, and the checklist's heading.
pub const TITLE: &str = "Use this Mac";
/// What a missing grant's line says to do: its button opens the list to do it in, and shows
/// the worker in Finder, since a background worker's request may leave it out of that list.
pub const TURN_ON: &str = "Turn on slopty-worker there, or drag it in from Finder.";
/// What the entry's row says under its words: what pressing it does.
pub const ROW_META: &str = "Shares its shells and windows through your server";
/// The checklist's line under its heading.
pub const BLURB: &str = "Slopty shares this Mac's shells and windows through your server.";
/// Why Slopty moves before it installs: where it runs now is gone after a restart.
pub const MISPLACED: &str =
    "Slopty runs from outside Applications, so this Mac would stop sharing after a restart.";
/// This Mac as an SSH target and as the host of a server started here: loopback.
pub const LOOPBACK: &str = "127.0.0.1";
/// How many times, [`RETRY`] apart (20 s in all), a new worker's listing is looked for in the
/// server's directory: counted, not timed, so a test's clock drives it.
pub const LISTED_TRIES: u32 = 80;
/// What the flow says when the directory never listed this Mac.
pub const NOT_LISTED: &str = "This Mac's worker has not registered with the server.";

/// Which server this Mac's worker registers with, decided before anything is installed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Serve {
    /// Start one here, which this app and the worker reach on loopback.
    Here,
    /// The server at this address.
    Join(HostAddr),
}

impl Serve {
    /// Where the worker and this app reach the server: loopback for one started here.
    #[must_use]
    pub fn address(&self) -> HostAddr {
        match self {
            Self::Here => HostAddr::new(LOOPBACK, slopty_net::endpoint::SERVER_PORT),
            Self::Join(address) => address.clone(),
        }
    }
}

/// How the flow stands on the server, for the checklist's Server line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Server {
    /// The tailnet is being looked on for one.
    Looking,
    /// Decided.
    Chosen(Serve),
    /// Starting it here failed, for this reason.
    Failed(String),
}
/// How many times the worker's control socket is asked before it counts as not answering.
pub const ATTEMPTS: u32 = 40;
/// How long apart: [`ATTEMPTS`] of these is the CLI's ten seconds.
pub const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// Work the flow waits on: it runs wherever the host puts it (the networking runtime on the
/// Mac) and is awaited on the foreground.
pub type Pending<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Why an install of this Mac's worker stopped.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Stopped {
    /// It would restart `slopty-ptyd`, ending the sessions it holds, as this sentence says;
    /// nothing was changed. Installing again with `end_sessions` goes on.
    EndsSessions(String),
    /// Slopty runs from somewhere a restart or an eject takes away ([`misplaced`]); nothing
    /// was installed.
    Misplaced,
    /// It failed, for this reason.
    Failed(String),
    /// Starting the server here failed, for this reason; the worker was not touched.
    Server(String),
}

/// What the flow does to this machine.
pub trait Host: std::fmt::Debug {
    /// Start the server here when `serve` says so, have the worker register with the server it
    /// names, then install and start the worker's services; with `end_sessions`, even where
    /// that restarts `slopty-ptyd` and ends its sessions. The server the worker registers
    /// with.
    fn install(&self, serve: &Serve, end_sessions: bool) -> Pending<Result<HostAddr, Stopped>>;
    /// The worker's `doctor`; `None` while nothing answers on its control socket.
    fn doctor(&self) -> Pending<Option<Doctor>>;
    /// Start the worker again, so it reads a Screen Recording grant made while it ran.
    fn restart(&self) -> Pending<()>;
    /// Open `pane` in System Settings, with the worker shown in Finder to drag into its list.
    fn open(&self, pane: Pane);
    /// Copy Slopty into Applications ([`applications`]), the copy there before it to the
    /// Trash, and say where it now is, to be opened from there.
    fn move_to_applications(&self) -> Pending<Result<PathBuf, String>>;
    /// Point this Mac's worker services at this copy of Slopty when they run another that is
    /// gone (moved, or a translocated copy a restart took away): `None` when they need not.
    fn repoint(&self) -> Pending<Option<Result<(), String>>>;
}

/// Whether Slopty, its binaries in `macos_dir`, runs from somewhere its services would lose
/// it: an app bundle outside `/Applications` and `~/Applications`.
///
/// A download runs from a read-only copy App Translocation makes and a restart removes; a disk
/// image ejects. A build run from a source tree is no bundle, and its daemons are copied out of
/// it.
#[must_use]
pub fn misplaced(macos_dir: &Path, home: &Path) -> bool {
    let Some(app) = bundle(macos_dir) else { return false };
    !(app.starts_with("/Applications") || app.starts_with(home.join("Applications")))
}

/// The `.app` whose `Contents/MacOS` is `dir`.
fn bundle(dir: &Path) -> Option<&Path> {
    if !dir.ends_with("Contents/MacOS") {
        return None;
    }
    let app = dir.parent()?.parent()?;
    app.extension().is_some_and(|ext| ext == "app").then_some(app)
}

/// Where Slopty moves to: `/Applications` when this user may write there, else
/// `~/Applications`, as Finder offers.
#[must_use]
pub fn applications(home: &Path, system_writable: bool) -> PathBuf {
    if system_writable { PathBuf::from("/Applications") } else { home.join("Applications") }
}

/// Whether services that run `program` should be pointed at this copy, its binaries in
/// `macos_dir`: they run another copy's binary, and that one is gone.
#[must_use]
pub fn repoints(program: &Path, macos_dir: &Path) -> bool {
    bundle(macos_dir).is_some()
        && program.parent().is_some_and(|dir| bundle(dir).is_some() && dir != macos_dir)
        && !program.exists()
}

/// What the checklist reads from the worker's `doctor`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Doctor {
    /// The worker, as the server's directory lists it.
    pub worker: WorkerId,
    /// How its link to its server stands; `None` while it has no server set.
    pub server: Option<LinkState>,
    /// The daemon's version.
    pub version: String,
    /// It may record the screen.
    pub screen_recording: bool,
    /// It may post input events.
    pub accessibility: bool,
    /// Whether the tailnet reaches it.
    pub tailnet: Tailnet,
}

/// This Mac on the tailnet, as the worker reads its Tailscale.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tailnet {
    /// Tailscale runs here and names this node so.
    Reachable(String),
    /// Tailscale answered, and it is not up.
    Down(BackendState),
    /// Tailscale is there and did not answer.
    Unreachable,
    /// No Tailscale the worker can read.
    Absent,
}

#[cfg(target_os = "macos")]
impl From<slopty_proto::ctl::Health> for Doctor {
    fn from(health: slopty_proto::ctl::Health) -> Self {
        use slopty_proto::ctl::Tailscale;
        let tailnet = match health.tailscale {
            Tailscale::Up { node, ip } => Tailnet::Reachable(match ip {
                Some(ip) if node.is_empty() => ip.to_string(),
                Some(_) | None => node,
            }),
            Tailscale::Down { backend } => Tailnet::Down(backend),
            Tailscale::Unreachable { error } => {
                tracing::debug!(%error, "the worker's Tailscale did not answer");
                Tailnet::Unreachable
            }
            Tailscale::Absent => Tailnet::Absent,
        };
        Self {
            worker: health.worker,
            server: health.server.map(|s| s.link),
            version: health.version,
            screen_recording: health.caps.can_capture,
            accessibility: health.caps.can_inject,
            tailnet,
        }
    }
}

/// Where the worker stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Worker {
    /// Its services are being installed.
    Installing,
    /// Installed; its control socket has not answered yet.
    Starting,
    /// It answered with this.
    Up(Doctor),
    /// It never answered.
    Silent,
    /// The install failed, for this reason.
    Failed(String),
    /// The install stopped before it changed anything: it would end sessions, as this says.
    EndsSessions(String),
    /// Slopty runs from somewhere its worker would lose it: it moves to Applications first.
    Misplaced,
}

impl Worker {
    /// It may stream and take input: this Mac can be added.
    pub const fn ready(&self) -> bool {
        matches!(self, Self::Up(d) if d.screen_recording && d.accessibility)
    }
}

/// The flow while its checklist is shown.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Flow {
    /// Which run of the flow this is; an answer for an older one is dropped.
    pub run: u64,
    /// Where the server stands.
    pub server: Server,
    /// Where the worker stands.
    pub worker: Worker,
    /// The worker's `doctor` is being asked.
    pub reading: bool,
    /// Ready: the server's directory is awaited to list this Mac.
    pub listing: bool,
    /// Why the flow did not end once ready: the directory never listed this Mac.
    pub error: Option<String>,
}

impl Flow {
    /// A run whose server is still being decided.
    pub const fn looking(run: u64) -> Self {
        Self {
            run,
            server: Server::Looking,
            worker: Worker::Installing,
            reading: false,
            listing: false,
            error: None,
        }
    }

    /// A run that has just started its install against `serve`.
    pub fn installing(run: u64, serve: Serve) -> Self {
        Self { server: Server::Chosen(serve), ..Self::looking(run) }
    }

    /// Whether the app coming back to the front should ask the worker again: it answered
    /// short of ready, or never answered.
    pub const fn rereads(&self) -> bool {
        !self.reading
            && !self.listing
            && matches!(&self.worker, Worker::Up(_) | Worker::Silent)
            && !self.worker.ready()
    }

    /// Whether the worker should be started again before it is asked: it lacked Screen
    /// Recording, which a running process does not see granted.
    pub const fn restarts(&self) -> bool {
        matches!(&self.worker, Worker::Up(d) if !d.screen_recording)
    }

    /// An install or a read is under way: pressing the entry again waits for it.
    pub const fn busy(&self) -> bool {
        self.listing
            || matches!(self.server, Server::Looking)
            || matches!(self.worker, Worker::Installing)
    }
}

/// One line of the checklist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    /// The worker's daemon answers.
    Running,
    /// The worker registers with a server: this Mac's own, or the one it joined.
    Server,
    /// It may record the screen.
    ScreenRecording,
    /// It may post input.
    Accessibility,
    /// The tailnet reaches it.
    Tailnet,
}

impl Check {
    /// The line's name.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Server => "Server",
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
            Self::Tailnet => "Reachable on your tailnet",
        }
    }

    /// Its name in ids.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Server => "server",
            Self::ScreenRecording => "screen",
            Self::Accessibility => "accessibility",
            Self::Tailnet => "tailnet",
        }
    }

    /// The id of its fix's button.
    pub const fn fix_id(self) -> &'static str {
        match self {
            Self::Running => "this-mac-fix-running",
            Self::Server => "this-mac-fix-server",
            Self::ScreenRecording => "this-mac-fix-screen",
            Self::Accessibility => "this-mac-fix-accessibility",
            Self::Tailnet => "this-mac-fix-tailnet",
        }
    }
}

/// How a line stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// Done.
    Ok,
    /// Missing, and in the way.
    Missing,
    /// Missing, and not in the way: this Mac is added without it.
    Advisory,
    /// Under way.
    Busy,
    /// Not known yet.
    Unknown,
}

/// What a missing line's button does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fix {
    /// Open this pane of System Settings.
    Open(Pane),
    /// Install again.
    Retry,
    /// Install again, ending the sessions the last try said it would.
    EndSessions,
    /// Move Slopty to Applications and open it from there.
    MoveToApplications,
}

impl Fix {
    /// The button's words.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open(_) => "Open settings",
            Self::Retry => "Try again",
            Self::EndSessions => "Update anyway",
            Self::MoveToApplications => "Move to Applications",
        }
    }
}

/// One line of the checklist, as drawn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    /// Which.
    pub check: Check,
    /// How it stands.
    pub mark: Mark,
    /// The line under its name: what it is for, or what to do.
    pub detail: String,
    /// The button a missing line carries.
    pub fix: Option<Fix>,
}

impl Line {
    /// The mark it wears. A grant not yet given is the human's to do, in the waiting tone with
    /// the button beside it, not a failure: red is for an install that failed or a worker that
    /// never answered. A line that is not in the way wears the quiet ring an unknown one does,
    /// its words saying why.
    pub const fn status(&self) -> slopty_ui::icons::Status {
        use slopty_ui::icons::Status;
        match (self.mark, self.fix) {
            (Mark::Ok, _) => Status::Done,
            (Mark::Missing, Some(Fix::Open(_))) => Status::NeedsYou,
            (Mark::Missing, _) => Status::Failed,
            (Mark::Busy, _) => Status::Running,
            (Mark::Advisory | Mark::Unknown, _) => Status::Idle,
        }
    }
}

/// The checklist for `flow`, in order; `logs` is where the worker's output goes and
/// `server_logs` the server's.
pub fn checklist(flow: &Flow, logs: &str, server_logs: &str) -> [Line; 5] {
    let worker = &flow.worker;
    let line =
        |check, mark, detail: &str, fix| Line { check, mark, detail: detail.to_owned(), fix };
    let running = match worker {
        _ if matches!(flow.server, Server::Looking | Server::Failed(_)) => {
            line(Check::Running, Mark::Unknown, "Waits for the server.", None)
        }
        Worker::Installing => line(Check::Running, Mark::Busy, "Installing\u{2026}", None),
        Worker::Starting => line(Check::Running, Mark::Busy, "Starting\u{2026}", None),
        Worker::Up(d) => {
            line(Check::Running, Mark::Ok, &format!("slopty-worker {}", d.version), None)
        }
        Worker::Silent => line(
            Check::Running,
            Mark::Missing,
            &format!("Not answering. Its log is {logs}"),
            Some(Fix::Retry),
        ),
        Worker::Failed(why) => line(Check::Running, Mark::Missing, why, Some(Fix::Retry)),
        Worker::EndsSessions(plan) => line(
            Check::Running,
            Mark::Missing,
            &format!("Its shell keeper changed too: {plan}. Every shell and agent turn ends."),
            Some(Fix::EndSessions),
        ),
        Worker::Misplaced => {
            line(Check::Running, Mark::Missing, MISPLACED, Some(Fix::MoveToApplications))
        }
    };
    let doctor = match worker {
        Worker::Up(d) => Some(d),
        Worker::Installing
        | Worker::Starting
        | Worker::Silent
        | Worker::Failed(_)
        | Worker::EndsSessions(_)
        | Worker::Misplaced => None,
    };
    // A missing grant's line is one line: the button opens the very list to turn it on in, so
    // naming the list ("Screen & System Audio Recording") only wrapped the line in two.
    let grant = |check, granted: Option<bool>, purpose: &str, pane: Pane| match granted {
        Some(true) => line(check, Mark::Ok, purpose, None),
        Some(false) => line(check, Mark::Missing, TURN_ON, Some(Fix::Open(pane))),
        None => line(check, Mark::Unknown, purpose, None),
    };
    let screen = grant(
        Check::ScreenRecording,
        doctor.map(|d| d.screen_recording),
        "Streams its windows and desktop.",
        Pane::ScreenRecording,
    );
    let accessibility = grant(
        Check::Accessibility,
        doctor.map(|d| d.accessibility),
        "Takes your clicks and keys in its windows.",
        Pane::Accessibility,
    );
    let tailnet = match doctor.map(|d| &d.tailnet) {
        Some(Tailnet::Reachable(node)) => {
            line(Check::Tailnet, Mark::Ok, &format!("Your devices reach it as {node}."), None)
        }
        Some(Tailnet::Down(state)) => line(
            Check::Tailnet,
            Mark::Advisory,
            &format!("Tailscale is {}, so only this Mac reaches it.", tailscale_state(*state)),
            None,
        ),
        Some(Tailnet::Unreachable) => line(
            Check::Tailnet,
            Mark::Advisory,
            "Tailscale is not answering, so only this Mac reaches it.",
            None,
        ),
        Some(Tailnet::Absent) => line(
            Check::Tailnet,
            Mark::Advisory,
            "No Tailscale here. Your other devices reach it over your VPN.",
            None,
        ),
        None => line(Check::Tailnet, Mark::Unknown, "Your other devices reach it there.", None),
    };
    let server = server_line(&flow.server, doctor, server_logs);
    [running, server, screen, accessibility, tailnet]
}

/// The Server line: where the server comes from, then how the worker's link to it stands.
fn server_line(server: &Server, doctor: Option<&Doctor>, server_logs: &str) -> Line {
    let line = |mark, detail: String, fix| Line { check: Check::Server, mark, detail, fix };
    let serve = match server {
        Server::Looking => {
            return line(Mark::Busy, "Looking for one on your tailnet\u{2026}".to_owned(), None);
        }
        Server::Failed(why) => {
            let detail = format!("{why}. Its log is {server_logs}");
            return line(Mark::Missing, detail, Some(Fix::Retry));
        }
        Server::Chosen(serve) => serve,
    };
    let (here, host) = match serve {
        Serve::Here => (true, LOOPBACK.to_owned()),
        Serve::Join(address) => (false, address.host().to_owned()),
    };
    let Some(doctor) = doctor else {
        let doing = if here {
            "Starts on this Mac.".to_owned()
        } else {
            format!("Joins the server on {host}.")
        };
        return line(Mark::Busy, doing, None);
    };
    match &doctor.server {
        Some(LinkState::Linked) if here => line(
            Mark::Ok,
            "Runs on this Mac. Your other devices connect to it here.".to_owned(),
            None,
        ),
        Some(LinkState::Linked) => line(Mark::Ok, format!("Registered with {host}."), None),
        Some(LinkState::Dialling) => {
            line(Mark::Busy, format!("Registering with {host}\u{2026}"), None)
        }
        Some(LinkState::Redialling { why }) => {
            line(Mark::Busy, format!("Not reached yet: {why}"), None)
        }
        Some(LinkState::Refused { why }) => line(Mark::Missing, why.clone(), Some(Fix::Retry)),
        None => {
            line(Mark::Missing, "Its worker registers with no server.".to_owned(), Some(Fix::Retry))
        }
    }
}

/// A Tailscale that is not up, in words: its own for the common states, else not up.
const fn tailscale_state(state: BackendState) -> &'static str {
    match state {
        BackendState::Stopped => "stopped",
        BackendState::NeedsLogin => "signed out",
        BackendState::NeedsMachineAuth => "waiting for approval",
        BackendState::Starting => "starting",
        BackendState::NoState
        | BackendState::InUseOtherUser
        | BackendState::Running
        | BackendState::Other => "not up",
    }
}

/// Whether this build offers the flow: a Mac, which can run the worker.
pub const OFFERED: bool = cfg!(target_os = "macos");

/// The host this app runs the flow through: the machine itself on a Mac.
///
/// Under the self-test, which must never install an agent, it is the e2e build's stand-in where
/// the harness named one, and otherwise nothing.
#[cfg(target_os = "macos")]
pub fn native(runtime: &tokio::runtime::Handle) -> Option<Rc<dyn Host>> {
    if crate::self_test() {
        return stand_in();
    }
    let data = slopty_platform::dirs::data_dir();
    Some(Rc::new(mac::Native { runtime: runtime.clone(), data }))
}

/// The self-test's host when the harness put a `doctor` report in
/// [`slopty_e2e::THIS_MAC_ENV`]: it installs nothing, restarts nothing and opens no pane, and
/// answers every read with that report, so the checklist can be drawn in any state with the
/// machine untouched. A server the report names stands in for one started here.
#[cfg(all(target_os = "macos", feature = "e2e"))]
fn stand_in() -> Option<Rc<dyn Host>> {
    let report = std::env::var(slopty_e2e::THIS_MAC_ENV).ok()?;
    let health: slopty_proto::ctl::Health = serde_json::from_str(&report)
        .inspect_err(|e| tracing::warn!(error = %e, "this Mac's stand-in report"))
        .ok()?;
    let here = health.server.as_ref().and_then(|s| s.address.parse().ok());
    Some(Rc::new(StandIn { doctor: Doctor::from(health), here }))
}

/// The self-test's host outside the e2e build: none, so there is no entry to press.
#[cfg(all(target_os = "macos", not(feature = "e2e")))]
const fn stand_in() -> Option<Rc<dyn Host>> {
    None
}

/// [`stand_in`]'s host: a fixed report and nothing done.
#[cfg(all(target_os = "macos", feature = "e2e"))]
#[derive(Debug)]
struct StandIn {
    doctor: Doctor,
    /// The server the report names, which a [`Serve::Here`] reaches.
    here: Option<HostAddr>,
}

#[cfg(all(target_os = "macos", feature = "e2e"))]
impl Host for StandIn {
    fn install(&self, serve: &Serve, _end_sessions: bool) -> Pending<Result<HostAddr, Stopped>> {
        let reached = match serve {
            Serve::Here => self
                .here
                .clone()
                .ok_or_else(|| Stopped::Server("the self-test starts no server".to_owned())),
            Serve::Join(address) => Ok(address.clone()),
        };
        Box::pin(std::future::ready(reached))
    }

    fn doctor(&self) -> Pending<Option<Doctor>> {
        Box::pin(std::future::ready(Some(self.doctor.clone())))
    }

    fn restart(&self) -> Pending<()> {
        Box::pin(std::future::ready(()))
    }

    fn open(&self, _pane: Pane) {}

    fn move_to_applications(&self) -> Pending<Result<PathBuf, String>> {
        Box::pin(std::future::ready(Err("the self-test moves nothing".to_owned())))
    }

    fn repoint(&self) -> Pending<Option<Result<(), String>>> {
        Box::pin(std::future::ready(None))
    }
}

/// [`native`] where no worker runs: nothing.
#[cfg(not(target_os = "macos"))]
pub const fn native(_runtime: &tokio::runtime::Handle) -> Option<Rc<dyn Host>> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    //! The flow on this Mac: `launchctl`, the worker's control socket and System Settings.

    use std::path::{Path, PathBuf};

    use slopty_net::HostAddr;
    use slopty_platform::service::{self, Layout, SERVER, Session, WORKER, WorkerOpts};
    use slopty_proto::ctl::{CtlReply, CtlRequest};

    use super::{Doctor, Host, Pane, Pending, Serve, Stopped};
    use crate::net;

    /// How long a server started here has to answer on loopback, as the CLI waits.
    const SERVER_START: std::time::Duration = std::time::Duration::from_secs(10);

    /// Install and start `slopty-server` from `source`, keeping its state in `data`, and wait
    /// until it answers at `address`.
    async fn start_server(source: &Path, data: &Path, address: &HostAddr) -> Result<(), String> {
        let session = Session::native();
        service::install_server(&session, None, "info", source, data)
            .await
            .map_err(|e| format!("Could not start the server: {e}"))?;
        let started = std::time::Instant::now();
        loop {
            match net::link_server(address).await {
                Ok(link) => {
                    link.close();
                    return Ok(());
                }
                Err(e) if started.elapsed() < SERVER_START => {
                    tracing::debug!(error = %e, "waiting for the server started here");
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(e) => {
                    return Err(format!(
                        "The server did not answer ({e:#}); its log is {}",
                        session.logs(SERVER)
                    ));
                }
            }
        }
    }

    /// This Mac, its networking runtime and the data directory the worker keeps.
    #[derive(Debug)]
    pub(super) struct Native {
        /// Where installs, reads and dials run.
        pub runtime: tokio::runtime::Handle,
        /// The worker's data directory: its settings, sockets and copied binaries.
        pub data: PathBuf,
    }

    impl Native {
        /// `task` on the runtime, awaited from anywhere.
        fn run<T: Send + 'static>(
            &self,
            task: impl Future<Output = T> + Send + 'static,
            died: T,
        ) -> Pending<T> {
            let handle = self.runtime.spawn(task);
            Box::pin(async move { handle.await.unwrap_or(died) })
        }
    }

    impl Host for Native {
        fn install(&self, serve: &Serve, end_sessions: bool) -> Pending<Result<HostAddr, Stopped>> {
            let data = self.data.clone();
            let serve = serve.clone();
            self.run(
                async move {
                    let source =
                        service::sibling_dir().map_err(|e| Stopped::Failed(e.to_string()))?;
                    if super::misplaced(&source, &slopty_platform::dirs::home()) {
                        return Err(Stopped::Misplaced);
                    }
                    let session = Session::native();
                    let ptyd = session.ptyd_plan(&source, &data);
                    if ptyd.ends_sessions() && !end_sessions {
                        return Err(Stopped::EndsSessions(ptyd.to_string()));
                    }
                    let address = serve.address();
                    if serve == Serve::Here {
                        start_server(&source, &data, &address).await.map_err(Stopped::Server)?;
                    }
                    let joined =
                        slopty_settings::join_server(&data, &address).map_err(Stopped::Failed)?;
                    let opts = WorkerOpts::default();
                    service::install_worker(&session, &opts, &source, &data, ptyd)
                        .await
                        .map_err(|e| Stopped::Failed(e.to_string()))?;
                    Ok(joined)
                },
                Err(Stopped::Failed("the install stopped".to_owned())),
            )
        }

        fn doctor(&self) -> Pending<Option<Doctor>> {
            let socket = Layout::new(&self.data).worker_socket();
            self.run(
                async move {
                    let mut line = serde_json::to_vec(&CtlRequest::Doctor).ok()?;
                    line.push(b'\n');
                    let reply = service::ask(&socket, &line).await.ok()?;
                    match serde_json::from_str(&reply).ok()? {
                        CtlReply::Doctor(health) => Some(Doctor::from(*health)),
                        other => {
                            tracing::warn!(?other, "doctor: not a health report");
                            None
                        }
                    }
                },
                None,
            )
        }

        fn restart(&self) -> Pending<()> {
            self.run(
                async {
                    if let Err(e) = Session::native().restart(WORKER) {
                        tracing::warn!(error = %e, "restart the worker");
                    }
                },
                (),
            )
        }

        fn open(&self, pane: Pane) {
            slopty_platform::privacy::open(pane);
            // A `LaunchAgent`'s request often raises no prompt, so the worker is not in the
            // list until it is dragged in or added with +.
            let session = Session::native();
            let installed = service::installed_args(session.manager, &session.file(WORKER));
            if let Some(exe) = installed.as_ref().and_then(|args| args.first()) {
                slopty_platform::web::reveal(Path::new(exe));
            }
        }

        fn move_to_applications(&self) -> Pending<Result<PathBuf, String>> {
            self.run(
                async {
                    let source = service::sibling_dir().map_err(|e| e.to_string())?;
                    tokio::task::spawn_blocking(move || move_bundle(&source))
                        .await
                        .map_err(|e| e.to_string())?
                },
                Err("the move stopped".to_owned()),
            )
        }

        fn repoint(&self) -> Pending<Option<Result<(), String>>> {
            let data = self.data.clone();
            self.run(
                async move {
                    let source = service::sibling_dir().ok()?;
                    super::bundle(&source)?;
                    if super::misplaced(&source, &slopty_platform::dirs::home()) {
                        return None;
                    }
                    let session = Session::native();
                    let gone = |job| {
                        let installed = service::installed_args(session.manager, &session.file(job))?;
                        let program = PathBuf::from(installed.first()?);
                        super::repoints(&program, &source).then_some((program, installed))
                    };
                    let server = gone(SERVER);
                    let worker = gone(WORKER);
                    if server.is_none() && worker.is_none() {
                        return None;
                    }
                    if let Some((program, args)) = server {
                        tracing::info!(was = %program.display(), now = %source.display(), "repoint the server");
                        let port = args
                            .iter()
                            .position(|a| a == "--port")
                            .and_then(|i| args.get(i.saturating_add(1)))
                            .and_then(|p| p.parse().ok());
                        if let Err(e) = service::install_server(&session, port, "info", &source, &data).await {
                            return Some(Err(format!("The server was not moved to this copy of Slopty: {e}")));
                        }
                    }
                    let Some((program, _args)) = worker else { return Some(Ok(())) };
                    tracing::info!(was = %program.display(), now = %source.display(), "repoint the worker");
                    // Its ptyd ran the binary gone too: kept if it still runs, else started.
                    let ptyd = session.ptyd_plan(&source, &data);
                    if ptyd.ends_sessions() {
                        return Some(Err(format!("Not moved to this copy of Slopty: {ptyd}")));
                    }
                    let done = service::install_worker(&session, &WorkerOpts::default(), &source, &data, ptyd).await;
                    Some(done.map(drop).map_err(|e| e.to_string()))
                },
                None,
            )
        }
    }

    /// Copy the bundle `source` is the `Contents/MacOS` of into Applications with `ditto`,
    /// a copy there before it to the Trash first, and give the copy's path.
    ///
    /// The copy loses the quarantine flag the download carried: this copy already passed
    /// Gatekeeper to run, and a flagged copy would be translocated again.
    fn move_bundle(source: &Path) -> Result<PathBuf, String> {
        let app = super::bundle(source).ok_or("Slopty runs from no app bundle")?;
        let name = app.file_name().ok_or("the bundle has no name")?;
        let home = slopty_platform::dirs::home();
        let dir = super::applications(&home, writable(Path::new("/Applications")));
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let to = dir.join(name);
        if to.exists() {
            let trash = home.join(".Trash");
            let stem = Path::new(name).file_stem().unwrap_or(name).to_string_lossy().into_owned();
            let aside = (0_u32..1_000)
                .map(|n| match n {
                    0 => trash.join(name),
                    n => trash.join(format!("{stem} {n}.app")),
                })
                .find(|at| !at.exists())
                .ok_or("the Trash is full of Sloptys")?;
            std::fs::rename(&to, &aside)
                .map_err(|e| format!("{} to the Trash: {e}", to.display()))?;
        }
        let out = std::process::Command::new("/usr/bin/ditto")
            .arg(app)
            .arg(&to)
            .output()
            .map_err(|e| format!("ditto: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        let unflagged = std::process::Command::new("/usr/bin/xattr")
            .args(["-d", "-r", "com.apple.quarantine"])
            .arg(&to)
            .output();
        if let Err(e) = unflagged {
            tracing::warn!(error = %e, "xattr on the moved copy");
        }
        Ok(to)
    }

    /// Whether this user may make a file in `dir`: an admin may in `/Applications`.
    fn writable(dir: &Path) -> bool {
        let probe = dir.join(format!(".slopty-{}", std::process::id()));
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe);
        made.is_ok_and(|_file| std::fs::remove_file(&probe).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doctor(screen_recording: bool, accessibility: bool, tailnet: Tailnet) -> Doctor {
        Doctor {
            worker: WorkerId::nil(),
            server: Some(LinkState::Linked),
            version: "0.3.0".to_owned(),
            screen_recording,
            accessibility,
            tailnet,
        }
    }

    /// The checklist of a run starting a server here, its worker at `worker`.
    fn checklist_of(worker: Worker, logs: &str) -> [Line; 5] {
        checklist(&Flow { worker, ..Flow::installing(1, Serve::Here) }, logs, "server.log")
    }

    fn marks(lines: &[Line; 5]) -> [Mark; 5] {
        lines.each_ref().map(|l| l.mark)
    }

    fn fixes(lines: &[Line; 5]) -> [Option<Fix>; 5] {
        lines.each_ref().map(|l| l.fix)
    }

    /// Before the worker answers only its own line and the server's move; a failed install or a
    /// silent worker is red with a way to try again, and says why.
    #[test]
    fn the_running_line_follows_the_install() {
        use Mark::{Busy, Missing, Unknown};
        let logs = "/Users/me/Library/Logs/Slopty/slopty-worker.log";
        for worker in [Worker::Installing, Worker::Starting] {
            let lines = checklist_of(worker.clone(), logs);
            assert_eq!(marks(&lines), [Busy, Busy, Unknown, Unknown, Unknown], "{worker:?}");
            assert_eq!(fixes(&lines), [None; 5], "nothing to press yet");
        }
        let silent = checklist_of(Worker::Silent, logs);
        assert_eq!(marks(&silent), [Missing, Busy, Unknown, Unknown, Unknown]);
        assert_eq!(silent[0].fix, Some(Fix::Retry));
        assert!(silent[0].detail.contains(logs), "names its log: {}", silent[0].detail);
        let failed = checklist_of(Worker::Failed("launchctl bootstrap failed".to_owned()), logs);
        assert_eq!((failed[0].mark, failed[0].fix), (Missing, Some(Fix::Retry)));
        assert_eq!(failed[0].detail, "launchctl bootstrap failed", "the reason, as it came");
        let titles = checklist_of(Worker::Silent, logs).map(|l| l.check.title());
        assert_eq!(
            titles,
            ["Running", "Server", "Screen Recording", "Accessibility", "Reachable on your tailnet"]
        );
    }

    /// The Server line says where the server comes from while it is looked for and installed,
    /// then how the worker's link to it stands; a server that could not start is red with its
    /// log and a way to try again, and the worker waits for it.
    #[test]
    fn the_server_line_follows_the_server_then_the_workers_link() {
        use Mark::{Busy, Missing, Ok, Unknown};
        let looking = checklist(&Flow::looking(1), "", "server.log");
        assert_eq!((looking[0].mark, looking[1].mark), (Unknown, Busy));
        assert_eq!(looking[1].detail, "Looking for one on your tailnet\u{2026}");

        let studio = HostAddr::new("studio.tail1234.ts.net", 45560);
        let joining = Flow { worker: Worker::Starting, ..Flow::installing(1, Serve::Join(studio)) };
        let lines = checklist(&joining, "", "");
        assert_eq!(
            (lines[1].mark, lines[1].detail.as_str()),
            (Busy, "Joins the server on studio.tail1234.ts.net.")
        );
        let joined = Flow { worker: Worker::Up(doctor(true, true, Tailnet::Absent)), ..joining };
        let lines = checklist(&joined, "", "");
        assert_eq!(
            (lines[1].mark, lines[1].detail.as_str()),
            (Ok, "Registered with studio.tail1234.ts.net.")
        );

        let here = checklist_of(Worker::Up(doctor(true, true, Tailnet::Absent)), "");
        assert_eq!(here[1].mark, Ok);
        assert!(here[1].detail.starts_with("Runs on this Mac"), "{}", here[1].detail);
        let refused = Doctor {
            server: Some(LinkState::Refused { why: "a worker with this id is linked".to_owned() }),
            ..doctor(true, true, Tailnet::Absent)
        };
        let lines = checklist_of(Worker::Up(refused), "");
        assert_eq!((lines[1].mark, lines[1].fix), (Missing, Some(Fix::Retry)));
        let unset = Doctor { server: None, ..doctor(true, true, Tailnet::Absent) };
        assert_eq!(checklist_of(Worker::Up(unset), "")[1].mark, Missing, "registered nowhere");

        let failed = Flow {
            server: Server::Failed("Could not start the server".to_owned()),
            ..Flow::looking(1)
        };
        let lines = checklist(&failed, "", "/Users/me/Library/Logs/Slopty/slopty-server.log");
        assert_eq!((lines[1].mark, lines[1].fix), (Missing, Some(Fix::Retry)));
        assert!(
            lines[1].detail.ends_with("slopty-server.log"),
            "names its log: {}",
            lines[1].detail
        );
        assert_eq!(lines[0].mark, Unknown, "the worker waits for it");
    }

    /// Each permission the worker lacks is red with the button that opens its own pane, and
    /// names the list to turn it on in; a granted one says what it is for. A tailnet that does
    /// not reach it is a warning, not a stop.
    #[test]
    fn the_doctor_maps_to_lines_and_buttons() {
        use Mark::{Advisory, Missing, Ok};
        use slopty_ui::icons::Status::{Done, Failed, Idle, NeedsYou};
        let down = Tailnet::Down(BackendState::Stopped);
        let lines = checklist_of(Worker::Up(doctor(false, false, down)), "");
        assert_eq!(marks(&lines), [Ok, Ok, Missing, Missing, Advisory]);
        assert_eq!(
            fixes(&lines),
            [
                None,
                None,
                Some(Fix::Open(Pane::ScreenRecording)),
                Some(Fix::Open(Pane::Accessibility)),
                None
            ]
        );
        assert_eq!(lines[2].detail, TURN_ON, "one line; the button opens the list");
        assert_eq!(lines[3].detail, TURN_ON);
        let statuses = lines.each_ref().map(Line::status);
        assert_eq!(
            statuses,
            [Done, Done, NeedsYou, NeedsYou, Idle],
            "a grant to give waits on the human; a tailnet out of the way is quiet"
        );
        let failed = checklist_of(Worker::Failed("bootstrap failed".to_owned()), "");
        assert_eq!(failed[0].status(), Failed, "red is for what went wrong");
        assert_eq!(lines[4].detail, "Tailscale is stopped, so only this Mac reaches it.");
        assert_eq!(lines[0].detail, "slopty-worker 0.3.0", "the daemon that answered");

        let studio = Tailnet::Reachable("studio.tail1234.ts.net".to_owned());
        let lines = checklist_of(Worker::Up(doctor(true, false, studio)), "");
        assert_eq!(marks(&lines), [Ok, Ok, Ok, Missing, Ok]);
        assert_eq!(fixes(&lines), [None, None, None, Some(Fix::Open(Pane::Accessibility)), None]);
        assert_eq!(lines[4].detail, "Your devices reach it as studio.tail1234.ts.net.");
        let lines = checklist_of(Worker::Up(doctor(true, true, Tailnet::Absent)), "");
        assert_eq!(marks(&lines), [Ok, Ok, Ok, Ok, Advisory], "no Tailscale is no stop either");
    }

    /// Ready is both grants, whatever the tailnet says; the app coming back rereads a worker
    /// short of that, restarting it first when Screen Recording was the one missing.
    #[test]
    fn ready_is_both_grants_and_a_return_rereads_until_then() {
        let up = |s, a| Worker::Up(doctor(s, a, Tailnet::Absent));
        assert!(up(true, true).ready(), "both granted");
        assert!(!up(true, false).ready() && !up(false, true).ready(), "one missing");
        assert!(!Worker::Silent.ready() && !Worker::Starting.ready(), "not answering");

        let flow = |worker| Flow { worker, ..Flow::installing(1, Serve::Here) };
        assert!(flow(up(false, true)).rereads() && flow(up(false, true)).restarts());
        assert!(flow(up(true, false)).rereads() && !flow(up(true, false)).restarts());
        assert!(flow(Worker::Silent).rereads(), "a silent worker is asked again");
        assert!(!flow(up(true, true)).rereads(), "ready: nothing to ask");
        assert!(!flow(Worker::Installing).rereads(), "an install under way is left alone");
        let reading = Flow { reading: true, ..flow(up(true, false)) };
        assert!(!reading.rereads(), "one read at a time");
        let listing = Flow { listing: true, ..flow(up(true, true)) };
        assert!(listing.busy() && !listing.rereads(), "waiting for the server to list it");
        assert!(Flow::looking(1).busy(), "looking for a server");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_health_report_reads_as_the_checklists_doctor() {
        use slopty_proto::ctl::Tailscale;
        let health = slopty_proto::ctl::Health {
            worker: WorkerId::nil(),
            server: None,
            version: "0.3.0".to_owned(),
            exe: "/Applications/Slopty.app/Contents/MacOS/slopty-worker".to_owned(),
            caps: slopty_proto::server::WorkerCaps {
                can_capture: true,
                can_inject: false,
                ..slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs)
            },
            listen: "[::]:45550".to_owned(),
            allow: Vec::new(),
            tailscale: Tailscale::Up {
                node: "studio.tail1234.ts.net".to_owned(),
                ip: Some([100, 64, 0, 3].into()),
            },
            pasteboard: slopty_proto::ctl::PasteboardAccess::Allowed,
            clients: 0,
            sessions: 0,
            uptime_secs: 1,
        };
        let studio = Tailnet::Reachable("studio.tail1234.ts.net".to_owned());
        let unregistered = Doctor { server: None, ..doctor(true, false, studio) };
        assert_eq!(Doctor::from(health.clone()), unregistered);
        let bare = Tailscale::Up { node: String::new(), ip: Some([100, 64, 0, 3].into()) };
        let health = slopty_proto::ctl::Health { tailscale: bare, ..health };
        let by_address = Tailnet::Reachable("100.64.0.3".to_owned());
        assert_eq!(Doctor::from(health.clone()).tailnet, by_address, "no name: the address");
        let signed_out = Tailscale::Down { backend: BackendState::NeedsLogin };
        let health = slopty_proto::ctl::Health { tailscale: signed_out, ..health };
        assert_eq!(Doctor::from(health.clone()).tailnet, Tailnet::Down(BackendState::NeedsLogin));
        let lines = checklist_of(Worker::Up(Doctor::from(health.clone())), "");
        assert_eq!(lines[4].detail, "Tailscale is signed out, so only this Mac reaches it.");
        let silent = Tailscale::Unreachable { error: "timed out".to_owned() };
        let health = slopty_proto::ctl::Health { tailscale: silent, ..health };
        assert_eq!(Doctor::from(health.clone()).tailnet, Tailnet::Unreachable);
        let lines = checklist_of(Worker::Up(Doctor::from(health.clone())), "");
        assert_eq!(lines[4].detail, "Tailscale is not answering, so only this Mac reaches it.");
        assert_eq!(marks(&lines)[4], Mark::Advisory, "a warning, not a stop");
        let health = slopty_proto::ctl::Health { tailscale: Tailscale::Absent, ..health };
        assert_eq!(Doctor::from(health).tailnet, Tailnet::Absent);
    }

    /// Slopty in an Applications folder runs its worker from there; a download's translocated
    /// copy, a disk image or Downloads would lose it at a restart, and a build from a source
    /// tree is no bundle (its daemons are copied out). Services that run a copy that is gone
    /// are pointed at this one; ones that run this copy, or one that still exists, are not.
    #[test]
    fn slopty_runs_its_worker_only_from_applications() {
        let home = Path::new("/Users/me");
        let macos = |app: &str| PathBuf::from(app).join("Contents/MacOS");
        assert!(!misplaced(&macos("/Applications/Slopty.app"), home));
        assert!(!misplaced(&macos("/Applications/Tools/Slopty.app"), home));
        assert!(!misplaced(&macos("/Users/me/Applications/Slopty.app"), home));
        let translocated = "/private/var/folders/x1/T/AppTranslocation/0A1B/d/Slopty.app";
        assert!(misplaced(&macos(translocated), home));
        assert!(misplaced(&macos("/Users/me/Downloads/Slopty.app"), home));
        assert!(misplaced(&macos("/Volumes/Slopty/Slopty.app"), home));
        assert!(!misplaced(Path::new("/Users/me/src/slopty/target/release"), home), "a dev build");
        assert!(!misplaced(Path::new("/Users/me/Contents/MacOS"), home), "no .app over it");

        assert_eq!(applications(home, true), Path::new("/Applications"));
        assert_eq!(applications(home, false), Path::new("/Users/me/Applications"));

        let here = macos("/Applications/Slopty.app");
        let gone = macos(translocated).join("slopty-worker");
        assert!(repoints(&gone, &here), "a copy a restart took away");
        assert!(!repoints(&here.join("slopty-worker"), &here), "this copy's own");
        let dev = Path::new("/Users/me/src/slopty/target/release");
        assert!(!repoints(&gone, dev), "a dev build points nothing at itself");
        let dir = tempfile::tempdir().unwrap();
        let there = dir.path().join("Other.app/Contents/MacOS");
        std::fs::create_dir_all(&there).unwrap();
        std::fs::write(there.join("slopty-worker"), b"").unwrap();
        assert!(!repoints(&there.join("slopty-worker"), &here), "another copy that still runs");
        assert!(!repoints(Path::new("/Users/me/.slopty/bin/slopty-worker"), &here), "copied out");
    }
}
