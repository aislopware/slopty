//! `slopty-server` — the control plane daemon.
//!
//! Workers register over QUIC on `--port` and hold their lease there; clients, the CLI and
//! agents get the worker directory and send verbs on the same port; AI agents also reach the
//! verbs over MCP (Streamable HTTP) on `--mcp-port`. Both listeners admit loopback, the tailnet
//! and private LANs only. The worker list survives restarts in `workers.json` in the data
//! directory.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_net::admission::Admission;
use slopty_server::{Config, Server};

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-server", version, about)]
struct Args {
    /// UDP port for worker, client and agent links; 0 picks a free one. Also
    /// `SLOPTY_SERVER_PORT`.
    #[arg(long, env = "SLOPTY_SERVER_PORT", default_value_t = slopty_net::endpoint::SERVER_PORT)]
    port: u16,
    /// TCP port for the MCP endpoint (`http://<host>:<port>/mcp`); 0 picks a free one. Also
    /// `SLOPTY_MCP_PORT`.
    #[arg(long, env = "SLOPTY_MCP_PORT", default_value_t = slopty_net::endpoint::MCP_PORT)]
    mcp_port: u16,
    /// Where `workers.json` lives (default: `$SLOPTY_DATA_DIR/server`, else
    /// `~/Library/Application Support/Slopty/server`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// The name clients show for this server (default: `$SLOPTY_SERVER_NAME`, else the
    /// machine's computer name).
    #[arg(long, env = "SLOPTY_SERVER_NAME")]
    name: Option<String>,
    /// Once both listeners are bound, print where on stdout as one JSON line,
    /// `{"quic":"[::]:45560","mcp":"[::]:45561"}` (a harness reads the ports `0` picked).
    #[arg(long)]
    print_addr: bool,
}

/// The machine's computer name, else `server`.
fn computer_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "server".to_owned())
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
    let data_dir = args.data_dir.unwrap_or_else(slopty_server::store::default_data_dir);
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let config = Config {
        name: args.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(computer_name),
        quic: slopty_net::endpoint::any(args.port),
        mcp: slopty_net::endpoint::any(args.mcp_port),
        data_dir,
        admission: Admission::default(),
    };
    let server = Server::start(config).await.context("start (is another server running?)")?;
    if args.print_addr {
        let bound = serde_json::json!({
            "quic": server.quic_addr().to_string(),
            "mcp": server.mcp_addr().to_string(),
        });
        #[expect(clippy::print_stdout, reason = "the addresses are what a harness waits for")]
        {
            println!("{bound}");
        }
    }

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listen for SIGTERM")?;
    tokio::select! {
        _signal = terminate.recv() => {}
        _signal = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
    server.shutdown().await;
    Ok(())
}
