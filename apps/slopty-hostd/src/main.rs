//! `slopty-hostd` — the host daemon.
//!
//! Owns the QUIC endpoint, admits clients by source address (loopback, the tailnet, private
//! LANs, or the `[host] allow` ranges in `settings.toml`), and bridges control and session
//! streams to [`slopty_host::Host`]. PTY masters live in `slopty-ptyd`, so this process can
//! restart without killing shells. A local control socket lets `slopty` (the CLI) inspect state
//! and relay agent hooks.

#![forbid(unsafe_code)]

mod agents;
mod conn;
mod ctl;
mod paths;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_agent::AgentTable;
use slopty_core::WorkerId;
use slopty_host::{Host, ItemStore};
use slopty_net::admission::{Admission, Cidr};
use slopty_net::host::HostListener;
use tokio::sync::broadcast;

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-hostd", version, about)]
struct Args {
    /// ptyd socket (default: `$TMPDIR/slopty/ptyd.sock`, or `$SLOPTY_PTYD_SOCKET`).
    #[arg(long)]
    ptyd_socket: Option<PathBuf>,
    /// Data directory holding `worker-id`, `items.json` and `settings.toml` (default:
    /// `$SLOPTY_DATA_DIR` or `~/Library/Application Support/Slopty`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Control socket (default: `$TMPDIR/slopty/hostd.sock`, or `$SLOPTY_HOSTD_SOCKET`).
    #[arg(long)]
    ctl_socket: Option<PathBuf>,
    /// Print the address it listens on, on stdout, once it does (a harness reads the port
    /// `--port 0` picked from it).
    #[arg(long)]
    print_addr: bool,
    /// UDP port to listen on; 0 picks a random free port (clients that stored the address
    /// then lose the host after a restart). Also `SLOPTY_PORT`.
    #[arg(long, env = "SLOPTY_PORT", default_value_t = slopty_net::endpoint::HOST_PORT)]
    port: u16,
    /// Listen on this one IP instead of every interface of both families (`::`). Also
    /// `SLOPTY_BIND`.
    #[arg(long = "bind", env = "SLOPTY_BIND")]
    bind: Option<std::net::IpAddr>,
}

/// Who may connect: the `[host] allow` ranges of `settings.toml` in `data_dir`, else the
/// defaults. A range that does not parse is logged and skipped; a list with none left falls
/// back to the defaults, which are private networks only.
fn admission(data_dir: &std::path::Path) -> Admission {
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(data_dir));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; admitting the default ranges");
    }
    let allow = loaded
        .settings
        .host
        .allow
        .iter()
        .filter_map(|range| match range.parse::<Cidr>() {
            Ok(cidr) => Some(cidr),
            Err(e) => {
                tracing::warn!(%range, error = %e, "[host] allow: skipped");
                None
            }
        })
        .collect();
    Admission::new(allow)
}

/// Shared daemon state.
#[derive(Clone, Debug)]
pub struct Daemon {
    /// Session table.
    pub host: Host,
    /// Listening endpoint and who it admits.
    pub listener: HostListener,
    /// Stable identity of this worker installation.
    pub id: WorkerId,
    /// Human name (hostname).
    pub name: String,
    /// Events every connected client should hear (session opened/closed, item deltas).
    pub events: broadcast::Sender<slopty_proto::HostMsg>,
    /// The item registry.
    pub items: ItemStore,
    /// Coding agents observed in sessions: fed by `slopty hook` over the control socket, and
    /// by [`agents::watch`] for the sessions no hook speaks for.
    pub agents: Arc<parking_lot::Mutex<AgentTable>>,
    /// The host's pasteboard, synced with remote-window clients.
    pub pasteboard: Arc<slopty_input::Pasteboard>,
    /// When the daemon came up (for `doctor`).
    pub started_at: std::time::Instant,
    /// Where it listens.
    pub listen: std::net::SocketAddr,
    /// Screen streams across every connection, for the control socket.
    pub screens: slopty_host::screen::Registry,
    /// Sleep policy: awake while a client is attached, display on while a stream is live.
    pub wake: Arc<parking_lot::Mutex<slopty_host::wake::Wake<Box<dyn slopty_host::wake::Holds>>>>,
}

/// The daemon's sleep assertions, as `NSProcessInfo` activities.
#[derive(Default)]
struct Assertions {
    system: Option<slopty_platform::Activity>,
    display: Option<slopty_platform::Activity>,
}

impl slopty_host::wake::Holds for Assertions {
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

/// How often the pasteboard's change count is read (one Mach call to the pasteboard server).
const PASTEBOARD_PERIOD: std::time::Duration = std::time::Duration::from_millis(200);

/// Broadcast every text change of the host pasteboard; `conn` forwards it only to clients
/// with a remote window open. Writes made through `Pasteboard::write` are not reported.
async fn watch_pasteboard(daemon: Daemon) -> ! {
    let mut tick = tokio::time::interval(PASTEBOARD_PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Some(text) = daemon.pasteboard.poll() {
            tracing::debug!(bytes = text.len(), "host clipboard changed");
            let event = slopty_proto::screen::ScreenEvent::Clipboard { text };
            let _sent = daemon.events.send(slopty_proto::HostMsg::Screen(event));
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
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
    let _activity = slopty_platform::Activity::latency_critical("Slopty host");

    let data_dir = args.data_dir.unwrap_or_else(paths::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let id = paths::worker_id(&data_dir)?;
    let local = args.bind.map_or_else(
        || slopty_net::endpoint::any(args.port),
        |ip| std::net::SocketAddr::new(ip, args.port),
    );
    let listener = HostListener::bind(local, admission(&data_dir))
        .with_context(|| format!("bind {local} (is another hostd running?)"))?;
    let listen = listener.local_addr()?;
    let host = Host::connect(args.ptyd_socket).await.context("connect to slopty-ptyd")?;
    let (events, _keep) = broadcast::channel(64);
    let items = ItemStore::open(&data_dir.join("items.json"))?;
    let holds: Box<dyn slopty_host::wake::Holds> = Box::new(Assertions::default());
    let wake = Arc::new(parking_lot::Mutex::new(slopty_host::wake::Wake::new(holds)));
    let screens = slopty_host::screen::Registry::default();
    screens.observe({
        let wake = Arc::clone(&wake);
        move |live| wake.lock().streams(live)
    });
    let daemon = Daemon {
        host,
        listener,
        id,
        name: paths::host_name(),
        events,
        items,
        agents: Arc::default(),
        pasteboard: Arc::new(slopty_input::Pasteboard::new()),
        started_at: std::time::Instant::now(),
        listen,
        screens,
        wake,
    };
    tokio::spawn(watch_pasteboard(daemon.clone()));
    // Agents the hooks never report: the foreground process, the title, the transcript.
    tokio::spawn(agents::watch(daemon.clone()));

    if let Some(mut exits) = daemon.host.take_exits() {
        let host = daemon.host.clone();
        tokio::spawn(async move {
            while let Some((session, status)) = exits.recv().await {
                tracing::info!(%session, status, "child exited");
                host.on_exit(session, status);
            }
        });
    }

    let ctl_path = args.ctl_socket.unwrap_or_else(paths::ctl_socket);
    // Sessions (and the `slopty hook` relay inside them) find this daemon through its socket.
    daemon.host.set_session_env(vec![(
        "SLOPTY_HOSTD_SOCKET".to_owned(),
        ctl_path.to_string_lossy().into_owned(),
    )]);
    tokio::spawn(ctl::serve(daemon.clone(), ctl_path));

    let allow: Vec<String> =
        daemon.listener.admission().ranges().iter().map(ToString::to_string).collect();
    tracing::info!(%id, name = %daemon.name, %listen, ?allow, "listening");
    // ScreenCaptureKit's first start in a process is slow; pay it now, not on the first window.
    tokio::spawn(async {
        match slopty_host::screen::warm_up().await {
            Ok(took) => tracing::debug!(ms = took.as_millis(), "capture warmed up"),
            Err(e) => tracing::debug!(error = %e, "capture warm-up failed"),
        }
    });
    // So is AppKit's first look at the cursor (seconds); the shape loop must find it warm.
    tokio::task::spawn_blocking(|| {
        let took = slopty_capture::warm_cursor();
        tracing::debug!(ms = took.as_millis(), "cursor warmed up");
    });
    if !slopty_input::can_post() {
        tracing::warn!(
            "no post-event (Accessibility) access: remote-window input will be dropped; \
             asking macOS now"
        );
        let _granted = slopty_input::request_post();
    }
    // Preflighting is not enough: until a process asks, macOS neither prompts nor lists the
    // binary under Screen Recording, so every stream fails with -3801 and there is nothing to
    // switch on. Asking costs one prompt, once, per signed identity.
    if !slopty_capture::can_capture() {
        tracing::warn!(
            "no Screen Recording access: windows and displays cannot be streamed; \
             asking macOS now"
        );
        let _granted = slopty_capture::request_capture();
    }
    if args.print_addr {
        #[expect(clippy::print_stdout, reason = "the address is what a harness waits for")]
        {
            println!("{listen}");
        }
    }

    loop {
        tokio::select! {
            client = daemon.listener.accept() => {
                let Some(client) = client else { break };
                tokio::spawn(conn::serve(daemon.clone(), client));
            }
            _signal = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down");
                break;
            }
        }
    }
    let _sent = daemon
        .events
        .send(slopty_proto::HostMsg::Rejected(slopty_proto::handshake::Rejection::Busy));
    daemon.listener.endpoint().close(0_u32.into(), b"host shutting down");
    let _drained = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        daemon.listener.endpoint().wait_idle(),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::admission;

    #[test]
    fn the_allow_list_comes_from_settings_and_a_bad_range_is_skipped() {
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let none = admission(dir.path());
        assert!(none.admits(ip("192.168.1.9")) && none.admits(ip("100.64.0.1")), "defaults");
        assert!(!none.admits(ip("8.8.8.8")));

        let settings = "[host]\nallow = [\"10.0.0.0/8\", \"bogus\"]\n";
        std::fs::write(dir.path().join("settings.toml"), settings).unwrap();
        let listed = admission(dir.path());
        assert!(listed.admits(ip("10.1.2.3")));
        assert!(!listed.admits(ip("192.168.1.9")), "the list replaces the defaults");
        assert!(listed.admits(ip("::1")), "loopback always");

        std::fs::write(dir.path().join("settings.toml"), "[host]\nallow = [\"bogus\"]\n").unwrap();
        assert_eq!(admission(dir.path()), none, "nothing usable left: the defaults, not everyone");
    }
}
