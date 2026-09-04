//! `slopty` — the command-line face of Slopty.
//!
//! * `slopty host …` talks to the local `slopty-hostd` over its control socket.
//! * `slopty pair <ticket>` pairs this machine with a host.
//! * `slopty sessions|open|attach` are a real client over iroh: a raw-mode terminal that renders
//!   frames locally. It is the reference client for latency measurements and works before (and
//!   without) the GPUI apps.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a CLI; stdout is its UI")]

mod attach;
mod client;
mod hostctl;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty", version, about)]
struct Cli {
    /// Data directory (default: `$SLOPTY_DATA_DIR` or `~/Library/Application Support/Slopty`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Control the local host daemon.
    Host {
        #[command(subcommand)]
        cmd: hostctl::HostCmd,
    },
    /// Pair with a host using the ticket it printed.
    Pair {
        /// The ticket (`sloptypair…`).
        ticket: String,
    },
    /// Paired hosts.
    Hosts,
    /// Forget a paired host.
    Forget {
        /// Host name or endpoint id prefix.
        host: String,
    },
    /// List sessions on a host.
    Sessions {
        /// Host name or endpoint id prefix (the only paired host when omitted).
        #[arg(long)]
        host: Option<String>,
    },
    /// Open a session and attach to it.
    Open {
        /// Host name or endpoint id prefix.
        #[arg(long)]
        host: Option<String>,
        /// Working directory on the host.
        #[arg(long)]
        cwd: Option<String>,
        /// Program and arguments (the login shell when empty).
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Attach to an existing session.
    Attach {
        /// Host name or endpoint id prefix.
        #[arg(long)]
        host: Option<String>,
        /// Session id prefix.
        session: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(client::data_dir);
    match cli.cmd {
        Cmd::Host { cmd } => hostctl::run(cmd).await,
        Cmd::Pair { ticket } => client::pair(&data_dir, &ticket).await,
        Cmd::Hosts => client::hosts(&data_dir),
        Cmd::Forget { host } => client::forget(&data_dir, &host),
        Cmd::Sessions { host } => client::sessions(&data_dir, host.as_deref()).await,
        Cmd::Open { host, cwd, command } => {
            attach::open(&data_dir, host.as_deref(), cwd, command).await
        }
        Cmd::Attach { host, session } => attach::attach(&data_dir, host.as_deref(), &session).await,
    }
}
