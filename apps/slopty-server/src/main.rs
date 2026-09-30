//! `slopty-server` — the control plane daemon.
//!
//! Workers register over QUIC on `--port` and hold their lease there; clients, the CLI and
//! agents get the worker directory and send verbs on the same port; AI agents also reach the
//! verbs over MCP (Streamable HTTP) on `--mcp-port`. Both listeners admit loopback, the tailnet
//! and the `[server] allow` ranges of `settings.toml` (a VPN Tailscale does not vouch for). The
//! worker list survives restarts in `workers.json` in the data directory.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Parser;
use slopty_net::admission::{Admission, parse_allow};
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
    /// Where `workers.json` lives (default: `server` in `$SLOPTY_DATA_DIR`, else in
    /// `~/Library/Application Support/Slopty` on macOS and `$XDG_DATA_HOME/slopty` on Linux).
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

/// Who may connect: loopback, the tailnet as this machine's Tailscale vouches for it, and the
/// `[server] allow` ranges of the `settings.toml` beside `data_dir` (the Slopty data directory
/// the server's own lives in, which the worker and the app read too).
fn admission(data_dir: &std::path::Path) -> Admission {
    let root = data_dir.parent().unwrap_or(data_dir);
    let loaded = slopty_settings::Settings::load(&slopty_settings::path_in(root));
    if let Some(e) = &loaded.error {
        tracing::warn!(error = %e, "settings.toml ignored; admitting no extra ranges");
    }
    Admission::new(parse_allow(&loaded.settings.server.allow, "[server]"))
}

#[tokio::main]
async fn main() -> Result<()> {
    slopty_crash::install(slopty_crash::Process::Server, &slopty_platform::dirs::data_dir());
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
    let admission = admission(&data_dir);
    let config = Config {
        name: args.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
            slopty_platform::computer_name().unwrap_or_else(|| "server".to_owned())
        }),
        quic: slopty_net::endpoint::any(args.port),
        mcp: slopty_net::endpoint::any(args.mcp_port),
        data_dir,
        admission,
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

#[cfg(test)]
mod tests {
    use super::admission;

    /// The ranges come from the settings beside the server's own directory, and a range that
    /// does not parse is skipped; with none, only loopback and the tailnet get in.
    #[test]
    fn the_allow_list_comes_from_the_shared_settings() {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("server");
        assert!(admission(&data_dir).ranges().is_empty(), "no range by default");
        let settings = "[server]\nallow = [\"10.8.0.0/24\", \"bogus\"]\n";
        std::fs::write(root.path().join("settings.toml"), settings).unwrap();
        let ranges: Vec<String> =
            admission(&data_dir).ranges().iter().map(ToString::to_string).collect();
        assert_eq!(ranges, ["10.8.0.0/24"]);
    }
}
