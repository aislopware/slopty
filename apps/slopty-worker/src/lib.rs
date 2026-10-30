//! `slopty-worker` — the worker daemon.
//!
//! Owns the QUIC endpoint, admits clients by source address (loopback, the tailnet, private
//! LANs, or the `[network] allow` ranges in `settings.toml`), and bridges control and session
//! streams to [`slopty_worker::Worker`]. PTY masters live in `slopty-ptyd`, so this process can
//! restart without killing shells. A local control socket lets `slopty` (the CLI) inspect state
//! and act on its behalf.

#![forbid(unsafe_code)]

mod agents;
mod clip;
mod conn;
mod ctl;
mod dnd;
mod files;
mod handoff;
mod paths;
mod ports;
mod screens;
mod server;
mod stall;
mod tailnet;
mod threads;
mod tunnel;
mod xfer;

include!(concat!(env!("OUT_DIR"), "/custody.rs"));

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_agent::AgentTable;
use slopty_core::{ClientId, SessionId, WorkerId};
use slopty_net::admission::{Admission, parse_allow};
use slopty_net::worker::WorkerListener;
use slopty_proto::terminal::CloseReason;
use slopty_worker::{ItemStore, Worker, WorkerError};
use tokio::sync::broadcast;

/// Events the daemon's broadcast holds for a connection that has not taken them yet. A client
/// that falls further behind is sent the state again (`conn`), and the server link registers
/// again; both cost more than the few hundred kilobytes a deeper queue does.
const EVENT_BUFFER: usize = 1024;

/// Moved sessions taken from the queue at once, each told once however often it moved.
const MOVES_AT_ONCE: usize = 256;

/// How long a daemon going down waits for its streams' input threads to let go of what their
/// clients hold down on this desktop.
#[cfg(target_os = "macos")]
const RELEASE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a daemon going down waits for the worker's own input source to be selected back.
const SOURCE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-worker", version, about)]
struct Args {
    /// ptyd socket (default: `$SLOPTY_PTYD_SOCKET`, else `ptyd.sock` in the platform's socket
    /// directory, `$TMPDIR/slopty` on macOS).
    #[arg(long)]
    ptyd_socket: Option<PathBuf>,
    /// Data directory holding `worker-id`, `items.json`, `settings.toml` and the kept sessions
    /// (`sessions/`) (default: `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty`
    /// on macOS and `$XDG_DATA_HOME/slopty` on Linux).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Control socket (default: `$TMPDIR/slopty/worker.sock`, or `$SLOPTY_WORKER_SOCKET`).
    #[arg(long)]
    ctl_socket: Option<PathBuf>,
    /// Print the address it listens on, on stdout, once it does (a harness reads the port
    /// `--port 0` picked from it).
    #[arg(long)]
    print_addr: bool,
    /// UDP port to listen on; 0 picks a random free port (clients that stored the address
    /// then lose the worker after a restart). Also `SLOPTY_PORT`.
    #[arg(long, env = "SLOPTY_PORT", default_value_t = slopty_net::endpoint::WORKER_PORT)]
    port: u16,
    /// Listen on this one IP instead of every interface of both families (`::`). Also
    /// `SLOPTY_BIND`.
    #[arg(long = "bind", env = "SLOPTY_BIND")]
    bind: Option<std::net::IpAddr>,
    /// The server to register with as a worker, `host[:port]` (port 45560 when absent); also
    /// `SLOPTY_SERVER`, else `[network] server` in `settings.toml`. Without one the worker runs
    /// on its own.
    #[arg(long, env = "SLOPTY_SERVER")]
    server: Option<String>,
    /// Run as the installed daemon: ask macOS for Screen Recording and Accessibility when
    /// either is missing (until a process asks, macOS neither prompts nor lists it, so a grant
    /// cannot be given), and warm capture up at start. A worker a test starts does neither:
    /// the prompts, the private-window consent that enumerating windows raises, and the audio
    /// a warm-up capture opens all land on whoever uses this Mac.
    #[arg(long)]
    installed: bool,
    /// Sync this named pasteboard instead of the general one; also `SLOPTY_PASTEBOARD`. For
    /// tests, which must never touch the user's clipboard.
    #[arg(long, env = "SLOPTY_PASTEBOARD", hide = true)]
    pasteboard: Option<String>,
    /// Where dropped files land when their name is taken, and staged files always (default
    /// `~/.slopty/drop`); also `SLOPTY_DROP_DIR`.
    #[arg(long, env = "SLOPTY_DROP_DIR")]
    drop_dir: Option<PathBuf>,
}

/// Who may connect: loopback, the tailnet as this machine's Tailscale vouches for it, and the
/// `[network] allow` ranges of `settings.toml` in `data_dir` by address. A range that does not
/// parse is logged and skipped.
fn admission(data_dir: &std::path::Path) -> Admission {
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(data_dir));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; admitting no extra ranges");
    }
    Admission::new(parse_allow(&loaded.settings.network.allow, "[network]"))
}

/// The clipboard the worker syncs: `NSPasteboard` on macOS.
#[cfg(target_os = "macos")]
pub(crate) type Board = slopty_input::MacBoard;

/// The general pasteboard, or the one called `name` (tests).
#[cfg(target_os = "macos")]
fn board(name: Option<&str>) -> Board {
    name.map_or_else(Board::general, Board::named)
}

/// The clipboard the worker syncs off macOS: one it holds itself, which the session's `xclip`,
/// `xsel`, `wl-copy` and `wl-paste` reach (`docs/decisions/platform.md`, "A Linux worker holds
/// its clipboard").
#[cfg(not(target_os = "macos"))]
pub(crate) type Board = slopty_input::pasteboard::Held;

/// A fresh held clipboard: there is only ever the one, whatever it is named.
#[cfg(not(target_os = "macos"))]
fn board(_name: Option<&str>) -> Board {
    Board::default()
}

/// Shared daemon state.
#[derive(Clone, Debug)]
pub(crate) struct Daemon {
    /// Session table.
    pub worker: Worker,
    /// Listening endpoint and who it admits.
    pub listener: WorkerListener,
    /// Stable identity of this worker installation.
    pub id: WorkerId,
    /// Human name (hostname).
    pub name: String,
    /// Events every connected client should hear (session opened/closed, item deltas).
    pub events: broadcast::Sender<slopty_proto::WorkerMsg>,
    /// Each link as its client says hello, with the name it says: an older link of the same
    /// client and name lets go of the input it holds down ([`conn`]).
    pub hellos: broadcast::Sender<(ClientId, String, slopty_worker::clip::Link)>,
    /// The item registry.
    pub items: ItemStore,
    /// The coding agents people run by hand in terminals, as [`agents::watch`] sees them.
    pub agents: Arc<parking_lot::Mutex<AgentTable>>,
    /// Clipboard sync over the worker's pasteboard.
    pub clip: Arc<slopty_worker::clip::Clipboard<Board>>,
    /// The primary selection the session's `xclip` and `xsel` keep, never synced.
    pub primary: Arc<slopty_input::pasteboard::Held>,
    /// Uploads in flight.
    pub transfers: Arc<slopty_worker::xfer::Transfers>,
    /// The drag from a client crossing the worker, and the helper that carries it.
    pub dnd: Arc<dnd::Dnd>,
    /// When each session's listening ports are scanned, and what they were.
    pub ports: Arc<parking_lot::Mutex<slopty_worker::ports::Trigger>>,
    /// When the daemon came up (for `doctor`).
    pub started_at: std::time::Instant,
    /// Where it listens.
    pub listen: std::net::SocketAddr,
    /// Screen streams across every connection, for the control socket.
    pub screens: slopty_worker::screen::Registry,
    /// Sleep policy: awake while a client is attached, display on while a stream is live.
    pub wake: Arc<parking_lot::Mutex<slopty_worker::wake::Wake<Assertions>>>,
    /// How each client's packets travel, from this machine's Tailscale.
    pub paths: tailnet::Paths,
    /// What this worker can do, kept current for the whole daemon's life
    /// ([`slopty_worker::caps::watch`]): every client's greeting carries it, a change goes out
    /// as `WorkerMsg::Caps`, and the server link registers with it.
    pub caps: tokio::sync::watch::Receiver<slopty_proto::server::WorkerCaps>,
    /// The one-minute load average, kept current beside [`Self::caps`]: in every client's
    /// greeting, and a move goes out as `WorkerMsg::Load` and `ToServer::Load`.
    pub load: tokio::sync::watch::Receiver<f32>,
    /// The daemon's home directory, which a client writes as `~`.
    pub home: String,
    /// Its `settings.toml`, which a client opens to edit this machine's settings.
    pub settings: String,
    /// The displays made for clients, on the main thread; `None` where none can be made, and
    /// every `OpenDisplay` then streams a physical display.
    pub displays: Displays,
    /// The curtain over the Mac's own screens and input while a client holds it, on the main
    /// thread; `None` off a Mac.
    pub curtain: Curtain,
    /// The keyboard input sources the streams' clients asked for, one claim each, and the
    /// worker's own kept beside the data to come back even after a crash.
    pub sources: slopty_input::sources::Sources,
    /// Connected clients, what each focuses and typed into, and the pages and edits a shell
    /// handed them.
    pub handoffs: Arc<parking_lot::Mutex<slopty_worker::handoff::Handoffs>>,
    /// Where a session's presence file is made (`true`) or removed, in order
    /// ([`handoff::presence`]).
    pub presence: tokio::sync::mpsc::UnboundedSender<(SessionId, bool)>,
    /// The key every session's token is made under, kept in the data directory: the token
    /// proves which session a program speaks from, to this daemon and to the server.
    pub session_key: slopty_agent::vouch::SessionKey,
    /// The agents' threads, kept under the data dir, and served to clients; `None` when they
    /// could not be opened there.
    pub threads: Option<threads::Threads>,
    /// The server registered with and how the link to it stands, for the doctor; `None` while
    /// none is set.
    pub server_link: Arc<tokio::sync::watch::Sender<Option<slopty_proto::ctl::ServerHealth>>>,
    /// The clones under way, the server's and the person's, which share their turns.
    pub cloner: slopty_worker::repo::cloning::Cloner,
    /// What has the daemon exit for its service manager to start it again, at a client's word
    /// ([`slopty_proto::orchestration::Verb::RestartWorker`]); `None` for a worker no service
    /// manager keeps alive (one run by hand or by a test), which would not come back.
    pub restart: Option<Arc<tokio::sync::Notify>>,
}

impl Daemon {
    /// End `session` (kill its program if it still runs) and tell every client and the server
    /// why, on behalf of `by` (whose item delta is not echoed back to it).
    ///
    /// Only the call that takes the session out of the table announces it, so a session two
    /// clients close at once, or one the unwatched sweep closes as a client does, is announced
    /// once. `NoSuchSession` when it was already gone.
    pub async fn end_session(
        &self,
        session: SessionId,
        reason: CloseReason,
        by: ClientId,
    ) -> Result<(), WorkerError> {
        let closed = self.worker.close(session).await;
        if matches!(closed, Err(WorkerError::NoSuchSession)) {
            return closed;
        }
        // Past the table the session is gone whatever ptyd answered; a failed ptyd close is
        // the caller's to report, but the clients must still hear of it.
        self.agents.lock().forget(session);
        let watched = {
            let mut handoffs = self.handoffs.lock();
            let watched = handoffs.watched(session);
            handoffs.forget(session);
            watched
        };
        if watched {
            let _sent = self.presence.send((session, false));
        }
        let _sent = self.events.send(slopty_proto::WorkerMsg::SessionClosed { session, reason });
        for delta in self.items.remove_session(session, by) {
            let _sent = self.events.send(slopty_proto::WorkerMsg::Items(delta));
        }
        closed
    }
}

/// How often the exited sessions are checked for a viewer. The bound is a day, so ten minutes
/// late is nothing.
const EXIT_SWEEP: std::time::Duration = std::time::Duration::from_mins(10);

/// Close the exited sessions nobody has watched for [`slopty_worker::manager::EXITED_UNWATCHED`],
/// until the daemon stops.
async fn close_stale_exits(daemon: Daemon) -> ! {
    let mut sweep = tokio::time::interval(EXIT_SWEEP);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        sweep.tick().await;
        let now = std::time::Instant::now();
        let after = slopty_worker::manager::EXITED_UNWATCHED;
        for session in daemon.worker.stale_exits(now, after).await {
            tracing::info!(%session, "closing an exited session nobody watched for a day");
            match daemon.end_session(session, CloseReason::Exited, ClientId::nil()).await {
                Ok(()) | Err(WorkerError::NoSuchSession) => {}
                Err(e) => tracing::warn!(%session, error = %e, "closing an exited session"),
            }
        }
    }
}

/// The daemon's sleep assertions, as `NSProcessInfo` activities.
#[derive(Debug, Default)]
pub(crate) struct Assertions {
    system: Option<slopty_platform::Activity>,
    display: Option<slopty_platform::Activity>,
}

impl slopty_worker::wake::Holds for Assertions {
    fn system(&mut self, hold: bool) {
        self.system =
            hold.then(|| slopty_platform::Activity::system_awake("Slopty client or agent at work"));
        tracing::info!(hold, "system sleep hold");
    }

    fn display(&mut self, hold: bool) {
        self.display =
            hold.then(|| slopty_platform::Activity::display_awake("Slopty window streaming"));
        tracing::info!(hold, "display sleep hold");
    }
}

/// The server to register with: `--server`, `SLOPTY_SERVER` or the settings file; `None` to
/// run on our own.
fn server_address(flag: Option<&str>, data_dir: &std::path::Path) -> Option<slopty_net::HostAddr> {
    let settings = slopty_settings::Settings::load(&slopty_settings::path_in(data_dir)).settings;
    match server::configured(flag, &settings) {
        Ok(Some(addr)) => Some(addr),
        Ok(None) => {
            tracing::info!("no server configured; running on our own");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "server address ignored; running on our own");
            None
        }
    }
}

/// The registration with a server while it runs: dropping it ends its tasks.
struct Joined(Vec<tokio::task::AbortHandle>);

impl Drop for Joined {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Register with the server at `addr` on a task of its own that dials and keeps dialing
/// (`server::run`), registering with the daemon's [`Daemon::caps`], and keep this worker's
/// facts for it on another ([`slopty_worker::facts::watch`]), gathered once the server asks.
/// `None` when no endpoint could be bound.
fn join_server(
    daemon: &Daemon,
    addr: slopty_net::HostAddr,
    data_dir: &std::path::Path,
) -> Option<Joined> {
    let endpoint = match slopty_net::client::bind_client() {
        Ok(endpoint) => endpoint,
        Err(e) => {
            tracing::warn!(error = %e, "no endpoint to dial the server from; running on our own");
            let why = format!("no endpoint to dial the server from: {e}");
            server::stands(daemon, &addr, slopty_proto::ctl::LinkState::Refused { why });
            return None;
        }
    };
    server::stands(daemon, &addr, slopty_proto::ctl::LinkState::Dialling);
    let orchestrator = slopty_worker::orchestrate::Orchestrator::new(
        daemon.id,
        daemon.worker.clone(),
        daemon.items.clone(),
        daemon.events.clone(),
        daemon.cloner.clone(),
    );
    if let Some(threads) = &daemon.threads {
        orchestrator.set_seated_threads(Arc::new(threads.clone()));
        orchestrator.set_thread_reads(Arc::new(threads::Reads::new(threads)));
    }
    orchestrator.set_settings_file(slopty_settings::path_in(data_dir));
    if let Some(restart) = &daemon.restart {
        orchestrator.set_restart(Arc::clone(restart));
    }
    let kept = slopty_worker::facts::Kept::default();
    orchestrator.set_facts(kept.clone());
    let facts = kept.subscribe();
    let facts_task = tokio::spawn(slopty_worker::facts::watch(kept));
    let watched = server::Watched { caps: daemon.caps.clone(), facts };
    let run = tokio::spawn(server::run(daemon.clone(), orchestrator, endpoint, addr, watched));
    Some(Joined(vec![facts_task.abort_handle(), run.abort_handle()]))
}

/// What the sessions started from now on find in their environment: this daemon's control
/// socket, where Slopty's CLI inside them reaches it; and the server it registers with, so
/// `slopty mcp` and the CLI in any of them, an agent's tools included, reach the fleet with no
/// flag.
fn session_env(
    ctl_path: &std::path::Path,
    server: Option<&slopty_net::HostAddr>,
) -> Vec<(String, String)> {
    let mut env =
        vec![("SLOPTY_WORKER_SOCKET".to_owned(), ctl_path.to_string_lossy().into_owned())];
    if let Some(addr) = server {
        env.push((slopty_proto::ctl::SERVER_ENV.to_owned(), addr.to_string()));
    }
    env
}

/// What a change of `settings.toml` from `before` to `now` asks of the running worker.
#[derive(Debug, Default, PartialEq)]
struct Changed {
    /// `[network] allow` changed: the ranges to let in from the next peer on.
    allow: Option<Vec<slopty_net::admission::Cidr>>,
    /// The server to register with changed. `flag` (`--server`, `SLOPTY_SERVER`) holds over
    /// the file, so with one this never is.
    reregister: bool,
    /// The server registered with from now, when `reregister`; `None` runs on its own.
    server: Option<slopty_net::HostAddr>,
    /// `keep_awake` changed: what keeps the machine awake from now.
    keeping: Option<slopty_worker::wake::Policy>,
}

/// [`Changed`] for a file that went from `before` to `now`.
fn changed(
    before: &slopty_settings::Settings,
    now: &slopty_settings::Settings,
    flag: Option<&str>,
) -> Changed {
    let allow = (before.network.allow != now.network.allow)
        .then(|| parse_allow(&now.network.allow, "[network]"));
    let registers = |settings| server::configured(flag, settings).ok().flatten();
    let (was, server) = (registers(before), registers(now));
    let reregister = was != server;
    let (before, now) = (&before.worker, &now.worker);
    Changed {
        allow,
        reregister,
        server,
        keeping: (before.keep_awake != now.keep_awake).then(|| policy(now.keep_awake)),
    }
}

/// The sleep policy `[worker] keep_awake` names.
const fn policy(keep: slopty_settings::KeepAwake) -> slopty_worker::wake::Policy {
    match keep {
        slopty_settings::KeepAwake::Working => slopty_worker::wake::Policy::Working,
        slopty_settings::KeepAwake::Attached => slopty_worker::wake::Policy::Attached,
        slopty_settings::KeepAwake::Never => slopty_worker::wake::Policy::Never,
    }
}

/// Follow `settings.toml` under `data_dir` for as long as the daemon runs, applying each change
/// of `[network]` and `[worker]` as it is read ([`Changed`]): the allowed ranges from the next
/// peer on, a new server registered with at once (`joined`, the registration running now,
/// ended first), and the sleep policy at once. A file that does not parse changes nothing.
async fn follow_settings(
    daemon: Daemon,
    data_dir: PathBuf,
    flag: Option<String>,
    ctl_path: PathBuf,
    mut joined: Option<Joined>,
) -> ! {
    let path = slopty_settings::path_in(&data_dir);
    let mut seen = slopty_settings::follow::Seen::of(&path);
    let mut applied = slopty_settings::Settings::load(&path).settings;
    loop {
        tokio::time::sleep(slopty_settings::follow::POLL).await;
        if !seen.changed(&path) {
            continue;
        }
        let loaded = slopty_settings::Settings::load(&path);
        if let Some(e) = &loaded.error {
            tracing::warn!(error = %e, "settings.toml ignored; what was applied stays");
            continue;
        }
        let now = loaded.settings;
        let Changed { allow, reregister, server, keeping } =
            changed(&applied, &now, flag.as_deref());
        if let Some(allow) = allow {
            let ranges: Vec<String> = allow.iter().map(ToString::to_string).collect();
            tracing::info!(?ranges, "[network] allow changed: applied");
            daemon.listener.admission().set_ranges(allow);
        }
        if reregister {
            drop(joined.take());
            let env = session_env(&ctl_path, server.as_ref());
            daemon.worker.set_session_env(env);
            if let Some(addr) = server {
                tracing::info!(server = %addr, "[network] server changed: registering");
                joined = join_server(&daemon, addr, &data_dir);
            } else {
                tracing::info!("[network] server cleared: running on our own");
                daemon.server_link.send_replace(None);
            }
        }
        if let Some(policy) = keeping {
            tracing::info!(?policy, "[worker] keep_awake changed: applied");
            daemon.wake.lock().set_policy(policy);
        }
        applied = now;
    }
}

/// Keep `caps` and `load` current (the agents' versions follow once their `--version`
/// answers, and an agent installed or removed while the worker runs as its directory changes)
/// and tell every client each change.
fn watch_caps(
    caps: tokio::sync::watch::Sender<slopty_proto::server::WorkerCaps>,
    load: tokio::sync::watch::Sender<f32>,
    events: broadcast::Sender<slopty_proto::WorkerMsg>,
) {
    let mut changed = caps.subscribe();
    let mut moved = load.subscribe();
    let load_events = events.clone();
    tokio::spawn(async move {
        let dirs = slopty_worker::facts::agent_dirs().await;
        let found = slopty_worker::caps::installed_agents(&dirs).await;
        let (installed, agents) = tokio::sync::watch::channel(found);
        tokio::spawn(slopty_worker::caps::watch(caps, load, agents));
        slopty_worker::caps::follow_agents(dirs, installed).await;
    });
    tokio::spawn(async move {
        while changed.changed().await.is_ok() {
            let now = changed.borrow_and_update().clone();
            let _sent = events.send(slopty_proto::WorkerMsg::Caps(now));
        }
    });
    tokio::spawn(async move {
        while moved.changed().await.is_ok() {
            let now = *moved.borrow_and_update();
            let _sent = load_events.send(slopty_proto::WorkerMsg::Load(now));
        }
    });
}

/// The displays made for clients, as the worker holds them.
type Displays =
    Option<slopty_worker::screen::sized::Displays<slopty_worker::screen::holder::Remote>>;

/// The curtain over the Mac, as the worker holds it: `None` off a Mac.
type Curtain =
    Option<slopty_worker::screen::curtain::Curtain<slopty_worker::screen::curtain::Native>>;

/// The exit code of a worker whose daemon thread panicked, as Rust's own for a panic.
#[cfg(target_os = "macos")]
const PANICKED: i32 = 101;

/// The exit code of a worker that ends to be started again, at a client's word
/// (`Daemon::restart`): `EX_TEMPFAIL`.
///
/// The service managers start the worker again after any exit but a clean one
/// (`Restart=on-failure`, `KeepAlive { SuccessfulExit = false }`, `slopty_platform::service`),
/// since launchd tells exit codes apart no finer than that. A clean exit is one with nothing to do:
/// a signal stopped it, or another worker holds its data directory (`Held`), which a restart
/// every second would only find held again.
const START_AGAIN: u8 = 75;

/// The worker ends to be started again ([`START_AGAIN`]).
#[derive(Debug)]
struct StartAgain;

impl std::fmt::Display for StartAgain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ended at a client's word, to be started again")
    }
}

impl std::error::Error for StartAgain {}

/// Another worker holds the data directory: this one ends before it does anything there.
#[derive(Debug)]
struct Held(String);

impl std::fmt::Display for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Held {}

/// Hold `data_dir` for this worker alone, for as long as the answer lives: an exclusive `flock`
/// on the directory itself, which the kernel lets go however the process ends, a crash or a
/// `kill -9` included, so a lock never outlives its worker. Taken before anything is read or
/// written there, so a second worker on the same directory (one started by hand beside the
/// service's) touches nothing of the first's: not the curtain's marker, the Caps Lock, the input
/// source, the threads, nor the port.
///
/// # Errors
///
/// [`Held`] when another process holds it; an I/O error when the directory cannot be opened.
fn hold_data_dir(data_dir: &std::path::Path) -> Result<std::fs::File> {
    let dir = std::fs::File::open(data_dir)
        .with_context(|| format!("open data dir {}", data_dir.display()))?;
    match dir.try_lock() {
        Ok(()) => Ok(dir),
        Err(std::fs::TryLockError::WouldBlock) => Err(Held(format!(
            "another worker holds {}; this one ends without touching it",
            data_dir.display()
        ))
        .into()),
        Err(std::fs::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("lock data dir {}", data_dir.display()))
        }
    }
}

/// One connection's hold on the curtain.
type CurtainLink = slopty_worker::screen::curtain::Link<slopty_worker::screen::curtain::Native>;

/// Have the displays made for clients held by the display holder beside the control socket
/// at `ctl_path`, its list under `data_dir`, and take those it holds from the worker before
/// ([`slopty_worker::screen::holder`]). Once more a little later, for those a holder that had to
/// go is still making again.
fn hold_displays(
    displays: &slopty_worker::screen::sized::Displays<slopty_worker::screen::holder::Remote>,
    ctl_path: &std::path::Path,
    data_dir: &std::path::Path,
) {
    use slopty_worker::screen::holder;
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(e) => {
            tracing::warn!(error = %e, "no display holder: every display streams a physical one");
            return;
        }
    };
    let dial = holder::spawning(
        program,
        ctl_path.with_file_name("displays.sock"),
        data_dir.join("displays.json"),
    );
    displays.with_factory(move |remote| remote.set_dial(dial));
    displays.adopt();
    let again = displays.clone();
    tokio::spawn(async move {
        tokio::time::sleep(holder::ADOPT_AGAIN_AFTER).await;
        again.adopt();
    });
}

/// The display holder's whole life, when this binary was started as one
/// (`slopty-worker --display-holder <socket> <list>`, [`slopty_worker::screen::holder`]): its
/// displays on the main thread, workers served from a thread beside it, until it is alone for
/// [`slopty_worker::screen::holder::ALONE_FOR`] or asked to quit.
#[cfg(target_os = "macos")]
fn holder_main() -> Option<std::process::ExitCode> {
    use std::sync::Arc;

    use slopty_worker::screen::holder::{self, Ended, Holder};
    use slopty_worker::screen::sized::{self, Main};
    let mut args = std::env::args_os().skip(1);
    if args.next()? != holder::HOLDER_ARG {
        return None;
    }
    let (Some(socket), Some(store)) = (args.next(), args.next()) else {
        return Some(std::process::ExitCode::FAILURE);
    };
    let (socket, store) = (PathBuf::from(socket), PathBuf::from(store));
    // Capped at its file's size bound under launchd (`slopty_crash::log`).
    slopty_crash::log::init("info");
    let Some(queue) = sized::MainQueue::new(Holder::new(sized::Cg, store.clone())) else {
        return Some(std::process::ExitCode::FAILURE);
    };
    let main: Arc<dyn Main<Holder<sized::Cg>>> = Arc::new(queue);
    let serving =
        std::thread::Builder::new().name("slopty-display-holder".to_owned()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            let ended = runtime.map(|runtime| {
                runtime.block_on(async {
                    let first = holder::first_link()
                        .inspect_err(|e| tracing::info!(error = %e, "no worker on stdin"))
                        .ok();
                    let listener = holder::listen(&socket)
                        .inspect_err(|e| tracing::warn!(error = %e, "the holder's socket"))
                        .ok();
                    let restoring = Arc::clone(&main);
                    tokio::spawn(async move { holder::restore(restoring, &store).await });
                    let ended =
                        holder::serve(Arc::clone(&main), first, listener, holder::ALONE_FOR).await;
                    if ended == Ended::Alone {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        main.run(Box::new(move |held: &mut Holder<sized::Cg>| {
                            held.release_all();
                            let _done = tx.send(());
                        }));
                        let _released = rx.await;
                    }
                    tracing::info!(?ended, "display holder ending");
                });
            });
            #[expect(
                clippy::exit,
                reason = "the main thread never returns; this ends the process"
            )]
            std::process::exit(i32::from(ended.is_err()));
        });
    if serving.is_err() {
        return Some(std::process::ExitCode::FAILURE);
    }
    sized::park_main()
}

/// What this binary is started with to be the daemon's warden ([`slopty_worker::warden`]).
const WARDEN_ARG: &str = "--warden";

/// Start this binary as the daemon's warden, in a process group of its own, so that every
/// group the daemon runs commands in ends with the daemon however it ends
/// ([`slopty_worker::warden`]). A thread waits on it, reaping it should it end first.
fn start_warden() {
    use std::os::unix::process::CommandExt as _;
    let started = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg(WARDEN_ARG)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
    });
    let mut warden = match started {
        Ok(warden) => warden,
        Err(e) => {
            tracing::warn!(error = %e, "no warden: what the worker starts may outlive it");
            return;
        }
    };
    if let Some(pipe) = warden.stdin.take() {
        slopty_worker::warden::ward(pipe);
    }
    let waited = std::thread::Builder::new().name("slopty-warden".to_owned()).spawn(move || {
        let _ended = warden.wait();
    });
    if let Err(e) = waited {
        tracing::warn!(error = %e, "nothing waits on the warden");
    }
}

/// The daemon: `src/main.rs` is this and nothing else.
pub fn main() -> Result<std::process::ExitCode> {
    if std::env::args_os().nth(1).is_some_and(|arg| arg == WARDEN_ARG) {
        slopty_worker::warden::serve(std::io::stdin().lock());
        return Ok(std::process::ExitCode::SUCCESS);
    }
    #[cfg(target_os = "macos")]
    if let Some(code) = dnd::helper_main() {
        return Ok(code);
    }
    slopty_crash::install(slopty_crash::Process::Worker, &slopty_platform::dirs::data_dir());
    #[cfg(target_os = "macos")]
    if let Some(code) = holder_main() {
        return Ok(code);
    }
    start_warden();
    // The connections' loops and noq's drivers carry every keystroke and echo: they run at the
    // class of work a person waits on, as the session threads do.
    slopty_platform::user_interactive_thread();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()
        .context("start the runtime")?;
    match serve(runtime) {
        Ok(()) => Ok(std::process::ExitCode::SUCCESS),
        // Nothing to do here: ended for good, not started again.
        Err(e) if e.is::<Held>() => {
            tracing::warn!("{e}");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Err(e) if e.is::<StartAgain>() => Ok(std::process::ExitCode::from(START_AGAIN)),
        Err(e) => Err(e),
    }
}

/// macOS: the main thread serves the run loop the displays made for clients live on
/// (`CGVirtualDisplay` refuses every other thread) and the input-source switches are heard on,
/// and the daemon runs on a thread beside it, ending the process when it ends.
#[cfg(target_os = "macos")]
fn serve(runtime: tokio::runtime::Runtime) -> Result<()> {
    let displays = slopty_worker::screen::sized::on_main_queue(
        slopty_worker::screen::holder::Remote::unlinked(),
    );
    let curtain = slopty_worker::screen::curtain::on_main_queue();
    let exit_lock = curtain.clone();
    let sources = slopty_input::sources::Sources::system();
    // Lives as long as the main thread's run loop, which never returns.
    let _heard = hear_switches(&sources);
    std::thread::Builder::new()
        .name("slopty-worker".to_owned())
        .spawn(move || {
            slopty_platform::user_interactive_thread();
            // A panic here must end the process as well: the main thread never returns, and a
            // process left with it alone keeps the curtain's shield and input hold, which only
            // the process ending takes down.
            let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(run(displays, curtain, sources))
            }));
            let code = match ended {
                Ok(Ok(())) => 0,
                // It did nothing, and holds nothing to lock the Mac for: ended for good.
                Ok(Err(e)) if e.is::<Held>() => {
                    tracing::warn!("{e}");
                    0
                }
                // The Mac was locked on the way out, as a stop locks it.
                Ok(Err(e)) if e.is::<StartAgain>() => i32::from(START_AGAIN),
                Ok(Err(e)) => {
                    tracing::error!(error = ?e, "worker stopped");
                    lock_now(exit_lock.as_ref());
                    1
                }
                Err(_panic) => {
                    tracing::error!("the daemon thread panicked; the worker ends");
                    lock_now(exit_lock.as_ref());
                    PANICKED
                }
            };
            #[expect(
                clippy::exit,
                reason = "the main thread never returns; this ends the process"
            )]
            std::process::exit(code);
        })
        .context("start the daemon thread")?;
    slopty_worker::screen::sized::park_main()
}

/// Every keyboard input-source switch on this Mac, told to `sources` as `HIToolbox` announces
/// it, until the answer is dropped: a stream whose client's source another client took hears
/// it, and an answer waits for its own switch. Made on the main thread, whose run loop
/// delivers the notification.
#[cfg(target_os = "macos")]
fn hear_switches(
    sources: &slopty_input::sources::Sources,
) -> Option<slopty_platform::input_source::Watch> {
    use slopty_platform::input_source;
    let heard = sources.clone();
    let watch = input_source::Watch::new(Box::new(move || heard.heard(input_source::current())));
    if watch.is_some() {
        sources.hearing();
        sources.heard(input_source::current());
    } else {
        tracing::warn!("input-source switches unheard; a claim is answered as it is made");
    }
    watch
}

/// Elsewhere no display is made, and the daemon runs on the main thread.
#[cfg(not(target_os = "macos"))]
fn serve(runtime: tokio::runtime::Runtime) -> Result<()> {
    let ended = runtime.block_on(run(None, None, slopty_input::sources::Sources::system()));
    drop(runtime);
    ended
}

async fn run(
    displays: Displays,
    curtain: Curtain,
    sources: slopty_input::sources::Sources,
) -> Result<()> {
    // Capped at its file's size bound under launchd (`slopty_crash::log`).
    slopty_crash::log::init("info");
    let args = Args::parse();
    // Following files holds a descriptor per watched path on macOS, past launchd's 256.
    match slopty_worker::fswatch::raise_descriptor_limit() {
        Ok(limit) => tracing::debug!(limit, "descriptor limit"),
        Err(e) => tracing::warn!(error = %e, "descriptor limit not raised; watches may run short"),
    }
    // Sharp timers for the whole daemon: screen capture, encode and QUIC heartbeats all run on
    // timers macOS would otherwise coalesce for a background process.
    let _activity = slopty_platform::Activity::latency_critical("Slopty worker");

    let data_dir = args.data_dir.unwrap_or_else(slopty_platform::dirs::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    // Before anything else in it: one worker per data directory.
    let _held = hold_data_dir(&data_dir)?;
    let id = paths::worker_id(&data_dir)?;
    let own = slopty_settings::Settings::load(&slopty_settings::path_in(&data_dir)).settings.worker;
    // A run that ended with a client's source still selected puts the worker's own back first.
    sources.keep_at(data_dir.join("input-source"));
    // A worker killed with the curtain up left the desk open: the Mac locks before anything
    // else.
    if let Some(curtain) = &curtain {
        curtain.keep_marker_at(data_dir.join(slopty_worker::screen::curtain::MARKER)).await;
    }
    if let Some(displays) = &displays {
        displays.set_linger(slopty_worker::screen::sized::LINGER);
    }
    // And the Caps Lock such a run left set, unless it was changed since.
    #[cfg(target_os = "macos")]
    slopty_input::keep_caps(data_dir.join("caps-lock"), &mut slopty_input::System);
    let local = args.bind.map_or_else(
        || slopty_net::endpoint::any(args.port),
        |ip| std::net::SocketAddr::new(ip, args.port),
    );
    let listener = WorkerListener::bind(local, admission(&data_dir))
        .with_context(|| format!("bind {local} (is another worker running?)"))?;
    let listen = listener.local_addr()?;
    let agents: Arc<parking_lot::Mutex<AgentTable>> = Arc::default();
    let (worker, reports) = Worker::connect(
        args.ptyd_socket,
        Arc::new(server::DaemonAgents(Arc::clone(&agents))),
        &data_dir.join("sessions"),
        Some(CUSTODY.to_owned()),
    )
    .await
    .context("connect to slopty-ptyd")?;
    let slopty_worker::manager::Reports { mut exits, port_hints, mut moves } = reports;
    let (events, _keep) = broadcast::channel(EVENT_BUFFER);
    let items = ItemStore::open(&data_dir.join("items.json"))?;
    let wake = Arc::new(parking_lot::Mutex::new(
        slopty_worker::wake::Wake::new(Assertions::default()).keeping(policy(own.keep_awake)),
    ));
    let screens = slopty_worker::screen::Registry::default();
    screens.observe({
        let wake = Arc::clone(&wake);
        move |live| wake.lock().streams(live)
    });
    let paths = tailnet::Paths::spawn(listener.admission().clone());
    let (caps_tx, caps) = tokio::sync::watch::channel(slopty_worker::caps::probe(
        &[],
        &slopty_worker::caps::Seldom::default(),
    ));
    let (load_tx, load) = tokio::sync::watch::channel(slopty_worker::caps::load());
    let ctl_path = args.ctl_socket.unwrap_or_else(paths::ctl_socket);
    if let Some(displays) = &displays {
        hold_displays(displays, &ctl_path, &data_dir);
    }
    let presence_dir = data_dir.join("presence");
    let (presence, presence_changes) = tokio::sync::mpsc::unbounded_channel();
    let session_key = slopty_agent::vouch::SessionKey::load_or_make(&data_dir)
        .with_context(|| format!("the session key in {}", data_dir.display()))?;
    worker.set_session_key(session_key);
    let transfers = Arc::new(slopty_worker::xfer::Transfers::new(
        args.drop_dir.clone().unwrap_or_else(slopty_worker::xfer::Transfers::default_drop_root),
    ));
    let threads = threads::open(
        &data_dir.join("threads"),
        worker.clone(),
        events.clone(),
        items.clone(),
        paths::worker_name(),
    );
    watch_caps(caps_tx, load_tx, events.clone());
    let daemon = Daemon {
        worker,
        listener,
        id,
        name: paths::worker_name(),
        events,
        hellos: broadcast::Sender::new(EVENT_BUFFER),
        items,
        agents,
        clip: Arc::new(slopty_worker::clip::Clipboard::new(
            board(args.pasteboard.as_deref()),
            slopty_proto::transfer::Peer::Worker(id),
        )),
        primary: Arc::default(),
        transfers: Arc::clone(&transfers),
        dnd: Arc::new(dnd::Dnd::new(transfers)),
        ports: Arc::default(),
        started_at: std::time::Instant::now(),
        listen,
        screens,
        wake,
        paths,
        caps,
        load,
        home: slopty_platform::dirs::home().to_str().map(str::to_owned).unwrap_or_default(),
        settings: slopty_settings::path_in(&data_dir).to_string_lossy().into_owned(),
        handoffs: Arc::default(),
        presence,
        session_key,
        server_link: Arc::new(tokio::sync::watch::Sender::new(None)),
        cloner: slopty_worker::repo::cloning::Cloner::default(),
        restart: args.installed.then(Arc::default),
        displays,
        curtain,
        sources,
        threads,
    };
    let transfers = Arc::clone(&daemon.transfers);
    tokio::task::spawn_blocking(move || {
        let removed = transfers.sweep(slopty_worker::xfer::STALE_PARTIAL);
        if removed > 0 {
            tracing::info!(removed, "swept the partial files of abandoned uploads");
        }
    });
    let for_sessions: Arc<dyn slopty_worker::clip::ForSessions> =
        Arc::<slopty_worker::clip::Clipboard<Board>>::clone(&daemon.clip);
    daemon.worker.share_clipboard(&for_sessions);
    tokio::spawn(clip::watch(daemon.clone()));
    tokio::spawn(ports::watch(daemon.clone(), port_hints));
    // Agents people run by hand in terminals: the foreground process and the title.
    tokio::spawn(agents::watch(daemon.clone()));
    threads::start(&daemon);

    // A terminal whose program exits stays, its last screen and its status kept, until a client
    // closes it (`docs/decisions/terminal.md`, "An exited shell stays until it is closed"). The
    // exit reaches the viewers on the session stream and everyone else as the session's changed
    // summary. Nobody watching it for `EXITED_UNWATCHED` closes it here.
    //
    // A lost ptyd is dialled again and handed every session back, so the exits run as long as
    // the worker does.
    tokio::spawn({
        let daemon = daemon.clone();
        async move {
            while let Some((session, exit)) = exits.recv().await {
                tracing::info!(%session, status = ?exit.status, "child exited");
                daemon.worker.on_exit(session, exit);
                // Whatever agent ran there has ended, killed or not: its thread hears it now.
                let _ended = daemon.agents.lock().exited(session);
                if let Some(summary) = daemon.worker.summary(session).await {
                    let _sent =
                        daemon.events.send(slopty_proto::WorkerMsg::SessionChanged(summary));
                }
            }
        }
    });
    tokio::spawn(close_stale_exits(daemon.clone()));
    // A session that moved (`cd`, a checkout, its progress) has a stale summary everywhere it was
    // sent: the server's listing and the clients that do not watch that session. The moves queued
    // while one batch is read are taken together, each session once: its summary is read when
    // it goes, so it has every move before it.
    tokio::spawn({
        let daemon = daemon.clone();
        async move {
            let mut moved = Vec::new();
            while moves.recv_many(&mut moved, MOVES_AT_ONCE).await > 0 {
                moved.sort_unstable();
                moved.dedup();
                for &session in &moved {
                    if let Some(summary) = daemon.worker.summary(session).await {
                        let _sent =
                            daemon.events.send(slopty_proto::WorkerMsg::SessionChanged(summary));
                    }
                }
                moved.clear();
            }
        }
    });

    let server = server_address(args.server.as_deref(), &data_dir);
    let env = session_env(&ctl_path, server.as_ref());
    daemon.worker.set_session_env(env);
    daemon.worker.set_presence_dir(presence_dir.clone());
    tokio::spawn(handoff::presence(presence_dir.clone(), presence_changes));
    let ctl_listener = ctl::bind(&ctl_path).await.context("control socket")?;
    tokio::spawn(ctl::serve(daemon.clone(), ctl_listener));
    // Sessions whose shells were lost to a reboot or to ptyd ending come back under their old
    // ids, before any client asks for them, so every item keeps its tile.
    for session in daemon.worker.restore().await {
        if let Some(delta) = daemon.items.ensure_terminal(session, ClientId::nil()) {
            let _sent = daemon.events.send(slopty_proto::WorkerMsg::Items(delta));
        }
    }

    let joined = server.and_then(|addr| join_server(&daemon, addr, &data_dir));
    tokio::spawn(follow_settings(
        daemon.clone(),
        data_dir.clone(),
        args.server.clone(),
        ctl_path,
        joined,
    ));

    let allow: Vec<String> =
        daemon.listener.admission().ranges().iter().map(ToString::to_string).collect();
    tracing::info!(%id, name = %daemon.name, %listen, ?allow, "listening");
    desktop(args.installed);
    if args.print_addr {
        #[expect(clippy::print_stdout, reason = "the address is what a harness waits for")]
        {
            println!("{listen}");
        }
    }

    // launchd stops a job with SIGTERM (`launchctl kickstart -k`, `bootout`, logout); a
    // terminal with SIGINT. Both go down the same way.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    let ended = loop {
        tokio::select! {
            client = daemon.listener.accept() => {
                let Some(client) = client else {
                    break Err(anyhow::anyhow!("the worker's endpoint closed"));
                };
                tokio::spawn(conn::serve(daemon.clone(), client));
            }
            _signal = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT: shutting down");
                break Ok(());
            }
            _signal = terminate.recv() => {
                tracing::info!("SIGTERM: shutting down");
                break Ok(());
            }
            () = restarted(daemon.restart.as_deref()) => {
                tracing::info!("restarting at a client's word: shutting down to be started again");
                break Err(StartAgain.into());
            }
        }
    };
    // The shield and the input hold end with the process: while they are up, the Mac locks
    // first.
    if let Some(curtain) = &daemon.curtain {
        lock_before_exit(curtain).await;
    }
    // Nobody is in front of anything once the worker is gone: Claude Code only checks that a
    // presence file exists, so one left behind would hold its phone pushes for good.
    if let Err(e) = std::fs::remove_dir_all(&presence_dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %presence_dir.display(), error = %e, "presence files left behind");
    }
    // A reboot stops the worker before it stops ptyd: this is the last chance to keep the
    // screens as they are now.
    daemon.worker.keep_now().await;
    daemon.listener.endpoint().close(0_u32.into(), b"worker shutting down");
    let _drained = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        daemon.listener.endpoint().wait_idle(),
    )
    .await;
    // Last, so nothing a client sent before the close is posted after it.
    let_go().await;
    // The worker's own input source back, and what it turned on for clients off.
    if tokio::time::timeout(SOURCE_WAIT, daemon.sources.release_all()).await.is_err() {
        tracing::warn!("the input source was not put back in time");
    }
    ended
}

/// Lock the Mac while the curtain is up, before the worker ends
/// ([`slopty_worker::screen::curtain::Curtain::lock_for_exit`]).
async fn lock_before_exit(
    curtain: &slopty_worker::screen::curtain::Curtain<slopty_worker::screen::curtain::Native>,
) {
    use slopty_worker::screen::curtain::EXIT_WAIT;
    let answer = curtain.lock_for_exit();
    let locked = tokio::task::spawn_blocking(move || answer.recv_timeout(EXIT_WAIT)).await;
    if !matches!(locked, Ok(Ok(true))) {
        tracing::warn!("the Mac not locked before the worker ends; its next start locks it");
    }
}

/// [`lock_before_exit`] for a daemon thread whose runtime is gone: waited on here, the main
/// thread drawing the curtain still running.
#[cfg(target_os = "macos")]
fn lock_now(
    curtain: Option<
        &slopty_worker::screen::curtain::Curtain<slopty_worker::screen::curtain::Native>,
    >,
) {
    use slopty_worker::screen::curtain::EXIT_WAIT;
    let Some(curtain) = curtain else { return };
    if curtain.lock_for_exit().recv_timeout(EXIT_WAIT) != Ok(true) {
        tracing::warn!("the Mac not locked before the worker ends; its next start locks it");
    }
}

/// Once a client asked this daemon to start again; never for one no service manager keeps
/// alive.
async fn restarted(restart: Option<&tokio::sync::Notify>) {
    match restart {
        Some(restart) => restart.notified().await,
        None => std::future::pending().await,
    }
}

/// Get the desktop half ready: capture and input warmed up, and their permissions checked (and,
/// installed, asked for).
#[cfg(target_os = "macos")]
fn desktop(installed: bool) {
    // ScreenCaptureKit's first start in a process is slow; pay it now, not on the first window.
    if installed {
        tokio::spawn(async {
            match slopty_worker::screen::warm_up().await {
                Ok(took) => tracing::debug!(ms = took.as_millis(), "capture warmed up"),
                Err(e) => tracing::debug!(error = %e, "capture warm-up failed"),
            }
        });
    }
    // So is AppKit's first look at the cursor (seconds); the shape loop must find it warm.
    tokio::task::spawn_blocking(|| {
        let took = slopty_capture::warm_cursor();
        tracing::debug!(ms = took.as_millis(), "cursor warmed up");
    });
    if !slopty_input::can_post() {
        tracing::warn!("no post-event (Accessibility) access: remote-window input will be dropped");
        if installed {
            let _granted = slopty_input::request_post();
        }
    }
    // Preflighting is not enough: until a process asks, macOS neither prompts nor lists the
    // binary under Screen Recording, so every stream fails with -3801 and there is nothing to
    // switch on. Asking costs one prompt, once, per signed identity.
    if !slopty_capture::can_capture() {
        tracing::warn!("no Screen Recording access: windows and displays cannot be streamed");
        if installed {
            let _granted = slopty_capture::request_capture();
        }
    }
}

/// A Linux worker streams no window or display and injects no input: it advertises neither in
/// its [`slopty_proto::server::WorkerCaps`] (`docs/decisions/platform.md`, "Linux seams").
#[cfg(not(target_os = "macos"))]
fn desktop(_installed: bool) {
    tracing::info!("desktop streaming is unsupported here: terminals, files and agents only");
}

/// Release whatever keys and buttons the streams' input threads hold down on this desktop.
#[cfg(target_os = "macos")]
async fn let_go() {
    let released =
        tokio::task::spawn_blocking(|| slopty_input::let_go_everywhere(RELEASE_WAIT)).await;
    if !matches!(released, Ok(true)) {
        tracing::warn!("an input thread did not let go in time; a key or button may stay down");
    }
}

/// No input is injected here, so nothing is held down.
#[cfg(not(target_os = "macos"))]
#[expect(clippy::unused_async, reason = "the macOS twin waits on the input threads")]
async fn let_go() {}

#[cfg(test)]
mod tests {
    use super::{Changed, admission, changed};

    /// A change of the file asks the running worker for what changed and nothing else: new
    /// ranges, a new server or none and the sleep policy; a server named by `--server` holds
    /// over the file's.
    #[test]
    fn a_change_of_the_file_is_applied_as_it_is_read() {
        let before = slopty_settings::Settings::default();
        assert_eq!(changed(&before, &before, None), Changed::default(), "nothing changed");
        let mut now = before.clone();
        now.network.allow = vec!["10.0.0.0/8".to_owned(), "bogus".to_owned()];
        now.network.server = Some(slopty_net::HostAddr::new("hub", 45_560));
        now.worker.keep_awake = slopty_settings::KeepAwake::Never;
        let asked = changed(&before, &now, None);
        let ranges: Vec<String> = asked.allow.iter().flatten().map(ToString::to_string).collect();
        assert_eq!(ranges, ["10.0.0.0/8"], "a range that does not parse is skipped");
        assert!(asked.reregister);
        assert_eq!(asked.server, Some(slopty_net::HostAddr::new("hub", 45_560)));
        assert_eq!(asked.keeping, Some(slopty_worker::wake::Policy::Never));
        let cleared = changed(&now, &before, None);
        assert_eq!((cleared.reregister, cleared.server), (true, None), "cleared: on its own");
        assert!(!changed(&before, &now, Some("other")).reregister, "the flag holds");
    }

    /// The ranges come from the settings, a range that does not parse is skipped, and with
    /// none listed no LAN is let in by address: only loopback and the tailnet.
    #[test]
    fn the_allow_list_comes_from_settings_and_a_bad_range_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        assert!(admission(dir.path()).ranges().is_empty(), "no LAN by default");
        let settings = "[network]\nallow = [\"10.0.0.0/8\", \"bogus\"]\n";
        std::fs::write(dir.path().join("settings.toml"), settings).unwrap();
        let listed = admission(dir.path());
        let ranges: Vec<String> = listed.ranges().iter().map(ToString::to_string).collect();
        assert_eq!(ranges, ["10.0.0.0/8"]);
    }
}
