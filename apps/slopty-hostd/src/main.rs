//! `slopty-hostd` — the host daemon.
//!
//! Owns the iroh endpoint, authenticates clients against the trust store, and bridges control
//! and session streams to [`slopty_host::Host`]. PTY masters live in `slopty-ptyd`, so this
//! process can restart without killing shells. A local control socket lets `slopty` (the CLI)
//! mint pairing tickets and inspect state.

mod conn;
mod ctl;
mod paths;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_agent::AgentTable;
use slopty_core::HostId;
use slopty_host::{CanvasStore, Host};
use slopty_net::Reach;
use slopty_net::host::HostListener;
use slopty_net::pairing::TrustStore;
use tokio::sync::broadcast;

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-hostd", version, about)]
struct Args {
    /// ptyd socket (default: `$TMPDIR/slopty/ptyd.sock`, or `$SLOPTY_PTYD_SOCKET`).
    #[arg(long)]
    ptyd_socket: Option<PathBuf>,
    /// Data directory holding `trust.json` (default: `$SLOPTY_DATA_DIR` or
    /// `~/Library/Application Support/Slopty`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Control socket (default: `$TMPDIR/slopty/hostd.sock`, or `$SLOPTY_HOSTD_SOCKET`).
    #[arg(long)]
    ctl_socket: Option<PathBuf>,
    /// Print a pairing ticket on stdout once the endpoint is online.
    #[arg(long)]
    print_ticket: bool,
    /// No relay, no wide-area lookup: clients must reach this host directly (LAN or a private
    /// mesh such as `NetBird`). Also `SLOPTY_DIRECT_ONLY=1`.
    #[arg(long, env = Reach::ENV)]
    direct_only: bool,
    /// UDP port to listen on; 0 picks a random free port (clients that cannot hear mDNS
    /// then lose the host after a restart). Also `SLOPTY_PORT`.
    #[arg(long, env = "SLOPTY_PORT", default_value_t = slopty_net::endpoint::HOST_PORT)]
    port: u16,
}

/// Shared daemon state.
#[derive(Clone, Debug)]
pub struct Daemon {
    /// Session table.
    pub host: Host,
    /// Listening endpoint + trust store.
    pub listener: HostListener,
    /// Stable identity of this host installation.
    pub id: HostId,
    /// Human name (hostname).
    pub name: String,
    /// Events every connected client should hear (session opened/closed, canvas deltas).
    pub events: broadcast::Sender<slopty_proto::HostMsg>,
    /// The canvas document.
    pub canvas: CanvasStore,
    /// Coding agents observed in sessions (fed by `slopty hook` over the control socket).
    pub agents: Arc<parking_lot::Mutex<AgentTable>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| {
                // iroh's path events carry the abandon reason; always keep them.
                tracing_subscriber::EnvFilter::new("info,iroh::_events::path=debug")
            },
        ))
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    // Sharp timers for the whole daemon: screen capture, encode and QUIC heartbeats all run on
    // timers macOS would otherwise coalesce for a background process.
    let _activity = slopty_platform::Activity::latency_critical("Slopty host");

    let data_dir = args.data_dir.unwrap_or_else(paths::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let store = TrustStore::open(&data_dir.join("trust.json"))?;
    let id = paths::host_id(&data_dir)?;
    let reach = if args.direct_only { Reach::DirectOnly } else { Reach::Anywhere };
    let listener = HostListener::bind_on(store, reach, args.port)
        .await
        .with_context(|| format!("bind UDP port {} (is another hostd running?)", args.port))?;
    let host = Host::connect(args.ptyd_socket).await.context("connect to slopty-ptyd")?;
    let (events, _keep) = broadcast::channel(64);
    let canvas = CanvasStore::open(&data_dir.join("canvas.json"))?;
    let daemon = Daemon {
        host,
        listener,
        id,
        name: paths::host_name(),
        events,
        canvas,
        agents: Arc::default(),
    };

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

    daemon.listener.online().await;
    tracing::info!(
        id = %daemon.listener.addr().id,
        name = %daemon.name,
        reach = ?daemon.listener.reach(),
        "online"
    );
    if !slopty_input::can_post() {
        tracing::warn!(
            "no post-event (Accessibility) access: remote-window input will be dropped; \
             asking macOS now"
        );
        let _granted = slopty_input::request_post();
    }
    if args.print_ticket {
        #[expect(clippy::print_stdout, reason = "the ticket is the program's output")]
        {
            println!("{}", daemon.listener.pair_ticket().await);
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
    daemon.listener.endpoint().close().await;
    Ok(())
}
