//! "Use this Mac as a worker": the worker's services installed from this app, its `doctor` read
//! as a checklist until it may stream and take input, then this Mac added over loopback.
//!
//! The install is `slopty worker install`'s own ([`slopty_platform::service::install_worker`]),
//! from the binaries beside the app: in place inside `Slopty.app`, copied out of a dev tree.
//! The worker registers with the server this app reads, so the server's other clients see
//! this Mac too ([`slopty_settings::join_clients_server`]).
//! The checklist is the worker's own `doctor` ([`slopty_proto::ctl::Health`]), so it says what
//! `slopty worker doctor` says. Everything that touches the machine goes through a [`Host`]:
//! [`native`] on the Mac, and a stand-in under test, so no test installs an agent, raises a
//! permission prompt or opens System Settings (`docs/decisions/platform.md`, "This Mac as a
//! worker").

use std::pin::Pin;
use std::rc::Rc;

pub use slopty_platform::privacy::Pane;
use slopty_proto::tailnet::BackendState;

use crate::net;

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
            /// Install the worker on this Mac and walk its permissions until it is added.
            UseThisMac,
        ]
    );
}

/// The entry's words, and the checklist's heading.
pub const TITLE: &str = "Use this Mac as a worker";
/// What a missing grant's line says to do: its button opens the list to do it in.
pub const TURN_ON: &str = "Turn on slopty-worker in System Settings.";
/// What the entry's row says under its words: what pressing it does.
pub const ROW_META: &str = "Runs the worker here and adds this Mac";
/// The checklist's line under its heading.
pub const BLURB: &str = "Slopty runs its worker here, then adds this Mac to your workspace.";
/// The address this Mac is added at: loopback, which every worker admits.
pub const LOOPBACK: &str = "127.0.0.1";
/// How many times the worker's control socket is asked before it counts as not answering.
pub const ATTEMPTS: u32 = 40;
/// How long apart: [`ATTEMPTS`] of these is the CLI's ten seconds.
pub const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// Work the flow waits on: it runs wherever the host puts it (the networking runtime on the
/// Mac) and is awaited on the foreground.
pub type Pending<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// What the flow does to this machine.
pub trait Host: std::fmt::Debug {
    /// Install and start the worker's services.
    fn install(&self) -> Pending<Result<(), String>>;
    /// The worker's `doctor`; `None` while nothing answers on its control socket.
    fn doctor(&self) -> Pending<Option<Doctor>>;
    /// Start the worker again, so it reads a Screen Recording grant made while it ran.
    fn restart(&self) -> Pending<()>;
    /// Add the worker at `address` to this app's workers.
    fn add(&self, address: &str) -> Pending<Result<net::Added, String>>;
    /// Open `pane` in System Settings.
    fn open(&self, pane: Pane);
}

/// What the checklist reads from the worker's `doctor`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Doctor {
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
    /// Where the worker stands.
    pub worker: Worker,
    /// The worker's `doctor` is being asked.
    pub reading: bool,
    /// This Mac is being added.
    pub adding: bool,
    /// Why adding it failed.
    pub error: Option<String>,
}

impl Flow {
    /// A run that has just started its install.
    pub const fn installing(run: u64) -> Self {
        Self { run, worker: Worker::Installing, reading: false, adding: false, error: None }
    }

    /// Whether the app coming back to the front should ask the worker again: it answered
    /// short of ready, or never answered.
    pub const fn rereads(&self) -> bool {
        !self.reading
            && !self.adding
            && matches!(&self.worker, Worker::Up(_) | Worker::Silent)
            && !self.worker.ready()
    }

    /// Whether the worker should be started again before it is asked: it lacked Screen
    /// Recording, which a running process does not see granted.
    pub const fn restarts(&self) -> bool {
        matches!(&self.worker, Worker::Up(d) if !d.screen_recording)
    }
}

/// One line of the checklist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    /// The worker's daemon answers.
    Running,
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
            Self::Running => "Worker running",
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
            Self::Tailnet => "Reachable on your tailnet",
        }
    }

    /// Its name in ids.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::ScreenRecording => "screen",
            Self::Accessibility => "accessibility",
            Self::Tailnet => "tailnet",
        }
    }

    /// The id of its fix's button.
    pub const fn fix_id(self) -> &'static str {
        match self {
            Self::Running => "this-mac-fix-running",
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
}

impl Fix {
    /// The button's words.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Open(_) => "Open settings",
            Self::Retry => "Try again",
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

/// The checklist for `worker`, in order; `logs` is where its output goes.
pub fn checklist(worker: &Worker, logs: &str) -> [Line; 4] {
    let line =
        |check, mark, detail: &str, fix| Line { check, mark, detail: detail.to_owned(), fix };
    let running = match worker {
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
    };
    let doctor = match worker {
        Worker::Up(d) => Some(d),
        Worker::Installing | Worker::Starting | Worker::Silent | Worker::Failed(_) => None,
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
    [running, screen, accessibility, tailnet]
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
/// [`slopty_e2e::THIS_MAC_ENV`]: it installs nothing, restarts nothing, opens no pane, adds
/// nothing, and answers every read with that report, so the checklist can be drawn in any
/// state with the machine untouched.
#[cfg(all(target_os = "macos", feature = "e2e"))]
fn stand_in() -> Option<Rc<dyn Host>> {
    let report = std::env::var(slopty_e2e::THIS_MAC_ENV).ok()?;
    let health: slopty_proto::ctl::Health = serde_json::from_str(&report)
        .inspect_err(|e| tracing::warn!(error = %e, "this Mac's stand-in report"))
        .ok()?;
    Some(Rc::new(StandIn { doctor: Doctor::from(health) }))
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
}

#[cfg(all(target_os = "macos", feature = "e2e"))]
impl Host for StandIn {
    fn install(&self) -> Pending<Result<(), String>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn doctor(&self) -> Pending<Option<Doctor>> {
        Box::pin(std::future::ready(Some(self.doctor.clone())))
    }

    fn restart(&self) -> Pending<()> {
        Box::pin(std::future::ready(()))
    }

    fn add(&self, _address: &str) -> Pending<Result<net::Added, String>> {
        Box::pin(std::future::ready(Err("the self-test adds nothing".to_owned())))
    }

    fn open(&self, _pane: Pane) {}
}

/// [`native`] where no worker runs: nothing.
#[cfg(not(target_os = "macos"))]
pub const fn native(_runtime: &tokio::runtime::Handle) -> Option<Rc<dyn Host>> {
    None
}

#[cfg(target_os = "macos")]
mod mac {
    //! The flow on this Mac: `launchctl`, the worker's control socket and System Settings.

    use std::path::PathBuf;

    use slopty_platform::service::{self, Layout, Session, WORKER, WorkerOpts};
    use slopty_proto::ctl::{CtlReply, CtlRequest};

    use super::{Doctor, Host, Pane, Pending};
    use crate::net;

    /// This Mac, its networking runtime and the data directory the worker keeps.
    #[derive(Debug)]
    pub struct Native {
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
        fn install(&self) -> Pending<Result<(), String>> {
            let data = self.data.clone();
            self.run(
                async move {
                    slopty_settings::join_clients_server(&data)?;
                    let source = service::sibling_dir().map_err(|e| e.to_string())?;
                    let opts = WorkerOpts::default();
                    service::install_worker(&Session::native(), &opts, &source, &data)
                        .await
                        .map(drop)
                        .map_err(|e| e.to_string())
                },
                Err("the install stopped".to_owned()),
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

        fn add(&self, address: &str) -> Pending<Result<net::Added, String>> {
            let address = address.to_owned();
            self.run(
                async move { net::add_worker(&address).await.map_err(|e| format!("{e:#}")) },
                Err("adding it stopped".to_owned()),
            )
        }

        fn open(&self, pane: Pane) {
            slopty_platform::privacy::open(pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doctor(screen_recording: bool, accessibility: bool, tailnet: Tailnet) -> Doctor {
        Doctor { version: "0.3.0".to_owned(), screen_recording, accessibility, tailnet }
    }

    fn marks(lines: &[Line; 4]) -> [Mark; 4] {
        lines.each_ref().map(|l| l.mark)
    }

    fn fixes(lines: &[Line; 4]) -> [Option<Fix>; 4] {
        lines.each_ref().map(|l| l.fix)
    }

    /// Before the worker answers only its own line moves; a failed install or a silent worker
    /// is red with a way to try again, and says why.
    #[test]
    fn the_running_line_follows_the_install() {
        use Mark::{Busy, Missing, Unknown};
        let logs = "/Users/me/Library/Logs/Slopty/slopty-worker.log";
        for worker in [Worker::Installing, Worker::Starting] {
            let lines = checklist(&worker, logs);
            assert_eq!(marks(&lines), [Busy, Unknown, Unknown, Unknown], "{worker:?}");
            assert_eq!(fixes(&lines), [None; 4], "nothing to press yet");
        }
        let silent = checklist(&Worker::Silent, logs);
        assert_eq!(marks(&silent), [Missing, Unknown, Unknown, Unknown]);
        assert_eq!(silent[0].fix, Some(Fix::Retry));
        assert!(silent[0].detail.contains(logs), "names its log: {}", silent[0].detail);
        let failed = checklist(&Worker::Failed("launchctl bootstrap failed".to_owned()), logs);
        assert_eq!((failed[0].mark, failed[0].fix), (Missing, Some(Fix::Retry)));
        assert_eq!(failed[0].detail, "launchctl bootstrap failed", "the reason, as it came");
        let titles = checklist(&Worker::Silent, logs).map(|l| l.check.title());
        assert_eq!(
            titles,
            ["Worker running", "Screen Recording", "Accessibility", "Reachable on your tailnet"]
        );
    }

    /// Each permission the worker lacks is red with the button that opens its own pane, and
    /// names the list to turn it on in; a granted one says what it is for. A tailnet that does
    /// not reach it is a warning, not a stop.
    #[test]
    fn the_doctor_maps_to_lines_and_buttons() {
        use Mark::{Advisory, Missing, Ok};
        use slopty_ui::icons::Status::{Done, Failed, Idle, NeedsYou};
        let down = Tailnet::Down(BackendState::Stopped);
        let lines = checklist(&Worker::Up(doctor(false, false, down)), "");
        assert_eq!(marks(&lines), [Ok, Missing, Missing, Advisory]);
        assert_eq!(
            fixes(&lines),
            [
                None,
                Some(Fix::Open(Pane::ScreenRecording)),
                Some(Fix::Open(Pane::Accessibility)),
                None
            ]
        );
        assert_eq!(lines[1].detail, TURN_ON, "one line; the button opens the list");
        assert_eq!(lines[2].detail, TURN_ON);
        let statuses = lines.each_ref().map(Line::status);
        assert_eq!(
            statuses,
            [Done, NeedsYou, NeedsYou, Idle],
            "a grant to give waits on the human; a tailnet out of the way is quiet"
        );
        let failed = checklist(&Worker::Failed("bootstrap failed".to_owned()), "");
        assert_eq!(failed[0].status(), Failed, "red is for what went wrong");
        assert_eq!(lines[3].detail, "Tailscale is stopped, so only this Mac reaches it.");
        assert_eq!(lines[0].detail, "slopty-worker 0.3.0", "the daemon that answered");

        let studio = Tailnet::Reachable("studio.tail1234.ts.net".to_owned());
        let lines = checklist(&Worker::Up(doctor(true, false, studio)), "");
        assert_eq!(marks(&lines), [Ok, Ok, Missing, Ok]);
        assert_eq!(fixes(&lines), [None, None, Some(Fix::Open(Pane::Accessibility)), None]);
        assert_eq!(lines[3].detail, "Your devices reach it as studio.tail1234.ts.net.");
        let lines = checklist(&Worker::Up(doctor(true, true, Tailnet::Absent)), "");
        assert_eq!(marks(&lines), [Ok, Ok, Ok, Advisory], "no Tailscale is no stop either");
    }

    /// Ready is both grants, whatever the tailnet says; the app coming back rereads a worker
    /// short of that, restarting it first when Screen Recording was the one missing.
    #[test]
    fn ready_is_both_grants_and_a_return_rereads_until_then() {
        let up = |s, a| Worker::Up(doctor(s, a, Tailnet::Absent));
        assert!(up(true, true).ready(), "both granted");
        assert!(!up(true, false).ready() && !up(false, true).ready(), "one missing");
        assert!(!Worker::Silent.ready() && !Worker::Starting.ready(), "not answering");

        let flow = |worker| Flow { worker, ..Flow::installing(1) };
        assert!(flow(up(false, true)).rereads() && flow(up(false, true)).restarts());
        assert!(flow(up(true, false)).rereads() && !flow(up(true, false)).restarts());
        assert!(flow(Worker::Silent).rereads(), "a silent worker is asked again");
        assert!(!flow(up(true, true)).rereads(), "ready: nothing to ask");
        assert!(!flow(Worker::Installing).rereads(), "an install under way is left alone");
        let reading = Flow { reading: true, ..flow(up(true, false)) };
        assert!(!reading.rereads(), "one read at a time");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_health_report_reads_as_the_checklists_doctor() {
        use slopty_proto::ctl::Tailscale;
        let health = slopty_proto::ctl::Health {
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
            clients: 0,
            sessions: 0,
            uptime_secs: 1,
        };
        let studio = Tailnet::Reachable("studio.tail1234.ts.net".to_owned());
        assert_eq!(Doctor::from(health.clone()), doctor(true, false, studio));
        let bare = Tailscale::Up { node: String::new(), ip: Some([100, 64, 0, 3].into()) };
        let health = slopty_proto::ctl::Health { tailscale: bare, ..health };
        let by_address = Tailnet::Reachable("100.64.0.3".to_owned());
        assert_eq!(Doctor::from(health.clone()).tailnet, by_address, "no name: the address");
        let signed_out = Tailscale::Down { backend: BackendState::NeedsLogin };
        let health = slopty_proto::ctl::Health { tailscale: signed_out, ..health };
        assert_eq!(Doctor::from(health.clone()).tailnet, Tailnet::Down(BackendState::NeedsLogin));
        let lines = checklist(&Worker::Up(Doctor::from(health.clone())), "");
        assert_eq!(lines[3].detail, "Tailscale is signed out, so only this Mac reaches it.");
        let silent = Tailscale::Unreachable { error: "timed out".to_owned() };
        let health = slopty_proto::ctl::Health { tailscale: silent, ..health };
        assert_eq!(Doctor::from(health.clone()).tailnet, Tailnet::Unreachable);
        let lines = checklist(&Worker::Up(Doctor::from(health.clone())), "");
        assert_eq!(lines[3].detail, "Tailscale is not answering, so only this Mac reaches it.");
        assert_eq!(marks(&lines)[3], Mark::Advisory, "a warning, not a stop");
        let health = slopty_proto::ctl::Health { tailscale: Tailscale::Absent, ..health };
        assert_eq!(Doctor::from(health).tailnet, Tailnet::Absent);
    }
}
