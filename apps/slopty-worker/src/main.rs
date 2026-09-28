//! `slopty-worker` — the worker daemon.
//!
//! Owns the QUIC endpoint, admits clients by source address (loopback, the tailnet, private
//! LANs, or the `[worker] allow` ranges in `settings.toml`), and bridges control and session
//! streams to [`slopty_worker::Worker`]. PTY masters live in `slopty-ptyd`, so this process can
//! restart without killing shells. A local control socket lets `slopty` (the CLI) inspect state
//! and relay agent hooks.

#![forbid(unsafe_code)]

mod agents;
mod clip;
mod conn;
mod ctl;
mod files;
pub mod follow;
mod modsock;
mod paths;
mod ports;
mod screens;
mod server;
pub mod tailnet;
mod tunnel;
mod xfer;

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

/// How long a daemon going down waits for its streams' input threads to let go of what their
/// clients hold down on this desktop.
#[cfg(target_os = "macos")]
const RELEASE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-worker", version, about)]
struct Args {
    /// ptyd socket (default: `$SLOPTY_PTYD_SOCKET`, else `ptyd.sock` in the platform's socket
    /// directory, `$TMPDIR/slopty` on macOS).
    #[arg(long)]
    ptyd_socket: Option<PathBuf>,
    /// Data directory holding `worker-id`, `items.json` and `settings.toml` (default:
    /// `$SLOPTY_DATA_DIR`, else `~/Library/Application Support/Slopty` on macOS and
    /// `$XDG_DATA_HOME/slopty` on Linux).
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
    /// `SLOPTY_SERVER`, else `[worker] server` in `settings.toml`. Without one the worker runs
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
/// `[worker] allow` ranges of `settings.toml` in `data_dir` by address. A range that does not
/// parse is logged and skipped.
fn admission(data_dir: &std::path::Path) -> Admission {
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(data_dir));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; admitting no extra ranges");
    }
    Admission::new(parse_allow(&loaded.settings.worker.allow, "[worker]"))
}

/// The clipboard the worker syncs: `NSPasteboard` on macOS.
#[cfg(target_os = "macos")]
pub type Board = slopty_input::MacBoard;

/// The general pasteboard, or the one called `name` (tests).
#[cfg(target_os = "macos")]
fn board(name: Option<&str>) -> Board {
    name.map_or_else(Board::general, Board::named)
}

/// No clipboard is synced on Linux yet (`docs/decisions/platform.md`, "Linux seams").
#[cfg(not(target_os = "macos"))]
pub type Board = slopty_input::pasteboard::Unsupported;

/// The board that refuses every paste, whatever it is named.
#[cfg(not(target_os = "macos"))]
const fn board(_name: Option<&str>) -> Board {
    slopty_input::pasteboard::Unsupported
}

/// Shared daemon state.
#[derive(Clone, Debug)]
pub struct Daemon {
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
    /// The item registry.
    pub items: ItemStore,
    /// Coding agents observed in sessions: fed by `slopty hook` over the control socket, and
    /// by [`agents::watch`] for the sessions no hook speaks for.
    pub agents: Arc<parking_lot::Mutex<AgentTable>>,
    /// Clipboard sync over the worker's pasteboard.
    pub clip: Arc<slopty_worker::clip::Clipboard<Board>>,
    /// Uploads in flight.
    pub transfers: Arc<slopty_worker::xfer::Transfers>,
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
    /// Who follows which agent's conversation, and the permission prompts held for them.
    pub follows: Arc<parking_lot::Mutex<follow::Follows>>,
    /// Slopty's Claude Code mod as written under the data dir, and the socket it posts to;
    /// `None` when it could not be written, and agents run without it.
    pub claude_mod: Option<slopty_agent::claude_mod::Installed>,
    /// The displays made for clients, on the main thread; `None` where none can be made, and
    /// every `OpenDisplay` then streams a physical display.
    pub displays: Option<slopty_worker::screen::sized::Displays<slopty_worker::screen::sized::Cg>>,
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
        let released = {
            let mut follows = self.follows.lock();
            follows.board.forget(session);
            follows.holds.forget(session)
        };
        follow::release(self, released);
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
pub struct Assertions {
    system: Option<slopty_platform::Activity>,
    display: Option<slopty_platform::Activity>,
}

impl slopty_worker::wake::Holds for Assertions {
    fn system(&mut self, hold: bool) {
        self.system =
            hold.then(|| slopty_platform::Activity::system_awake("Slopty client attached"));
        tracing::info!(hold, "system sleep hold");
    }

    fn display(&mut self, hold: bool) {
        self.display =
            hold.then(|| slopty_platform::Activity::display_awake("Slopty window streaming"));
        tracing::info!(hold, "display sleep hold");
    }
}

/// Register with the configured server, if there is one, on a task of its own that dials and
/// keeps dialing (`server::run`), registering with the daemon's [`Daemon::caps`].
fn join_server(daemon: &Daemon, flag: Option<&str>, data_dir: &std::path::Path) {
    let settings = slopty_settings::Settings::load(&slopty_settings::path_in(data_dir)).settings;
    let addr = match server::configured(flag, &settings) {
        Ok(Some(addr)) => addr,
        Ok(None) => {
            tracing::info!("no server configured; running on our own");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "server address ignored; running on our own");
            return;
        }
    };
    let endpoint = match slopty_net::client::bind_client() {
        Ok(endpoint) => endpoint,
        Err(e) => {
            tracing::warn!(error = %e, "no endpoint to dial the server from; running on our own");
            return;
        }
    };
    let launch = slopty_worker::orchestrate::Launch {
        relay: slopty_agent::hooks::relay_beside_this_binary(),
        claude_mod: daemon.claude_mod.clone(),
    };
    let orchestrator = slopty_worker::orchestrate::Orchestrator::new(
        daemon.id,
        daemon.worker.clone(),
        daemon.items.clone(),
        daemon.events.clone(),
        launch,
        Arc::new(follow::Orchestrated(daemon.clone())),
    );
    let caps = daemon.caps.clone();
    tokio::spawn(server::run(daemon.clone(), orchestrator, endpoint, addr, caps));
}

/// Keep `caps` and `load` current (the agents' versions follow once their `--version`
/// answers) and tell every client each change.
fn watch_caps(
    caps: tokio::sync::watch::Sender<slopty_proto::server::WorkerCaps>,
    load: tokio::sync::watch::Sender<f32>,
    events: broadcast::Sender<slopty_proto::WorkerMsg>,
) {
    let mut changed = caps.subscribe();
    let mut moved = load.subscribe();
    let load_events = events.clone();
    tokio::spawn(async move {
        let agents = slopty_worker::caps::installed_agents().await;
        caps.send_modify(|c| c.agents.clone_from(&agents));
        slopty_worker::caps::watch(caps, load, agents).await;
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
type Displays = Option<slopty_worker::screen::sized::Displays<slopty_worker::screen::sized::Cg>>;

fn main() -> Result<()> {
    // The connections' loops and noq's drivers carry every keystroke and echo: they run at the
    // class of work a person waits on, as the session threads do.
    slopty_platform::user_interactive_thread();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()
        .context("start the runtime")?;
    serve(runtime)
}

/// macOS: the main thread serves the run loop the displays made for clients live on
/// (`CGVirtualDisplay` refuses every other thread), and the daemon runs on a thread beside it,
/// ending the process when it ends.
#[cfg(target_os = "macos")]
fn serve(runtime: tokio::runtime::Runtime) -> Result<()> {
    let displays = slopty_worker::screen::sized::on_main_queue();
    std::thread::Builder::new()
        .name("slopty-worker".to_owned())
        .spawn(move || {
            slopty_platform::user_interactive_thread();
            let ended = runtime.block_on(run(displays));
            drop(runtime);
            if let Err(e) = &ended {
                tracing::error!(error = ?e, "worker stopped");
            }
            #[expect(
                clippy::exit,
                reason = "the main thread never returns; this ends the process"
            )]
            std::process::exit(i32::from(ended.is_err()));
        })
        .context("start the daemon thread")?;
    slopty_worker::screen::sized::park_main()
}

/// Elsewhere no display is made, and the daemon runs on the main thread.
#[cfg(not(target_os = "macos"))]
fn serve(runtime: tokio::runtime::Runtime) -> Result<()> {
    let ended = runtime.block_on(run(None));
    drop(runtime);
    ended
}

async fn run(displays: Displays) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    // Sharp timers for the whole daemon: screen capture, encode and QUIC heartbeats all run on
    // timers macOS would otherwise coalesce for a background process.
    let _activity = slopty_platform::Activity::latency_critical("Slopty worker");

    let data_dir = args.data_dir.unwrap_or_else(slopty_platform::dirs::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let id = paths::worker_id(&data_dir)?;
    let local = args.bind.map_or_else(
        || slopty_net::endpoint::any(args.port),
        |ip| std::net::SocketAddr::new(ip, args.port),
    );
    let listener = WorkerListener::bind(local, admission(&data_dir))
        .with_context(|| format!("bind {local} (is another worker running?)"))?;
    let listen = listener.local_addr()?;
    let agents: Arc<parking_lot::Mutex<AgentTable>> = Arc::default();
    let (worker, reports) =
        Worker::connect(args.ptyd_socket, Arc::new(server::DaemonAgents(Arc::clone(&agents))))
            .await
            .context("connect to slopty-ptyd")?;
    let slopty_worker::manager::Reports { mut exits, port_hints, mut moves } = reports;
    let (events, _keep) = broadcast::channel(EVENT_BUFFER);
    let items = ItemStore::open(&data_dir.join("items.json"))?;
    let wake =
        Arc::new(parking_lot::Mutex::new(slopty_worker::wake::Wake::new(Assertions::default())));
    let screens = slopty_worker::screen::Registry::default();
    screens.observe({
        let wake = Arc::clone(&wake);
        move |live| wake.lock().streams(live)
    });
    let paths = tailnet::Paths::spawn(listener.admission().clone());
    let (caps_tx, caps) = tokio::sync::watch::channel(slopty_worker::caps::probe(&[], None));
    let (load_tx, load) = tokio::sync::watch::channel(slopty_worker::caps::load());
    watch_caps(caps_tx, load_tx, events.clone());
    let ctl_path = args.ctl_socket.unwrap_or_else(paths::ctl_socket);
    let mod_path = modsock::beside(&ctl_path);
    let claude_mod = match slopty_agent::claude_mod::install(&data_dir) {
        Ok(dir) => Some(slopty_agent::claude_mod::Installed { dir, socket: mod_path.clone() }),
        Err(e) => {
            tracing::warn!(error = %e, "the Claude Code mod could not be written; agents run without it");
            None
        }
    };
    let daemon = Daemon {
        worker,
        listener,
        id,
        name: paths::worker_name(),
        events,
        items,
        agents,
        clip: Arc::new(slopty_worker::clip::Clipboard::new(
            board(args.pasteboard.as_deref()),
            slopty_proto::transfer::Peer::Worker(id),
        )),
        transfers: Arc::new(slopty_worker::xfer::Transfers::new(
            args.drop_dir.clone().unwrap_or_else(slopty_worker::xfer::Transfers::default_drop_root),
        )),
        ports: Arc::default(),
        started_at: std::time::Instant::now(),
        listen,
        screens,
        wake,
        paths,
        caps,
        load,
        home: slopty_platform::dirs::home().to_str().map(str::to_owned).unwrap_or_default(),
        follows: Arc::default(),
        claude_mod,
        displays,
    };
    let transfers = Arc::clone(&daemon.transfers);
    tokio::task::spawn_blocking(move || {
        let removed = transfers.sweep(slopty_worker::xfer::STALE_PARTIAL);
        if removed > 0 {
            tracing::info!(removed, "swept the partial files of abandoned uploads");
        }
    });
    tokio::spawn(clip::watch(daemon.clone()));
    tokio::spawn(ports::watch(daemon.clone(), port_hints));
    // Agents the hooks never report: the foreground process, the title, the transcript.
    tokio::spawn(agents::watch(daemon.clone()));

    // A terminal whose program exits stays, its last screen and its status kept, until a client
    // closes it (`docs/decisions/terminal.md`, "An exited shell stays until it is closed"). The
    // exit reaches the viewers on the session stream and everyone else as the session's changed
    // summary. Nobody watching it for `EXITED_UNWATCHED` closes it here.
    //
    // The exits end when ptyd's connection does. A worker without ptyd can spawn nothing and
    // hands its sessions to nobody, so it goes down (`ptyd_gone`) and launchd starts one that
    // connects again.
    let (ptyd_gone_tx, mut ptyd_gone) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn({
        let daemon = daemon.clone();
        async move {
            while let Some((session, status)) = exits.recv().await {
                tracing::info!(%session, status, "child exited");
                daemon.worker.on_exit(session, status);
                if let Some(summary) = daemon.worker.summary(session).await {
                    let _sent =
                        daemon.events.send(slopty_proto::WorkerMsg::SessionChanged(summary));
                }
            }
            let _sent = ptyd_gone_tx.send(());
        }
    });
    tokio::spawn(close_stale_exits(daemon.clone()));
    // A session that moved (`cd`, a checkout) has a stale summary everywhere it was sent: the
    // server's listing and the clients that do not watch that session.
    tokio::spawn({
        let daemon = daemon.clone();
        async move {
            while let Some(session) = moves.recv().await {
                if let Some(summary) = daemon.worker.summary(session).await {
                    let _sent =
                        daemon.events.send(slopty_proto::WorkerMsg::SessionChanged(summary));
                }
            }
        }
    });

    // Sessions (and the `slopty hook` relay inside them) find this daemon through its socket,
    // and a `claude` typed in one finds the mod (the shell integration's `claude` function).
    let mut session_env =
        vec![("SLOPTY_WORKER_SOCKET".to_owned(), ctl_path.to_string_lossy().into_owned())];
    if let Some(installed) = &daemon.claude_mod {
        session_env.extend(installed.session_env());
        tokio::spawn(modsock::serve(daemon.clone(), mod_path));
    }
    daemon.worker.set_session_env(session_env);
    tokio::spawn(ctl::serve(daemon.clone(), ctl_path));

    join_server(&daemon, args.server.as_deref(), &data_dir);

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
                let Some(client) = client else { break Ok(()) };
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
            _gone = &mut ptyd_gone => {
                tracing::error!("lost slopty-ptyd: shutting down for a worker that connects again");
                break Err(anyhow::anyhow!("slopty-ptyd hung up"));
            }
        }
    };
    daemon.listener.endpoint().close(0_u32.into(), b"worker shutting down");
    let _drained = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        daemon.listener.endpoint().wait_idle(),
    )
    .await;
    // Last, so nothing a client sent before the close is posted after it.
    let_go().await;
    ended
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
    use super::admission;

    /// The ranges come from the settings, a range that does not parse is skipped, and with
    /// none listed no LAN is let in by address: only loopback and the tailnet.
    #[test]
    fn the_allow_list_comes_from_settings_and_a_bad_range_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        assert!(admission(dir.path()).ranges().is_empty(), "no LAN by default");
        let settings = "[worker]\nallow = [\"10.0.0.0/8\", \"bogus\"]\n";
        std::fs::write(dir.path().join("settings.toml"), settings).unwrap();
        let listed = admission(dir.path());
        let ranges: Vec<String> = listed.ranges().iter().map(ToString::to_string).collect();
        assert_eq!(ranges, ["10.0.0.0/8"]);
    }
}
