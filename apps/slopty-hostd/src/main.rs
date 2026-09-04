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

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_core::HostId;
use slopty_host::Host;
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
    /// Events every connected client should hear (session opened/closed).
    pub events: broadcast::Sender<slopty_proto::HostMsg>,
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

    let data_dir = args.data_dir.unwrap_or_else(paths::data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let store = TrustStore::open(&data_dir.join("trust.json"))?;
    let id = paths::host_id(&data_dir)?;
    let listener = HostListener::bind(store).await?;
    let host = Host::connect(args.ptyd_socket).await.context("connect to slopty-ptyd")?;
    let (events, _keep) = broadcast::channel(64);
    let daemon = Daemon { host, listener, id, name: paths::host_name(), events };

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
    tokio::spawn(ctl::serve(daemon.clone(), ctl_path));

    daemon.listener.online().await;
    tracing::info!(id = %daemon.listener.addr().id, name = %daemon.name, "online");
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
