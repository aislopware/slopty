//! `slopty` — the command-line face of Slopty.
//!
//! * `slopty workers|terminals|open|send|wait|…` drive workers through the server, one verb each
//!   (`slopty_proto::orchestration`), as text or `--json`.
//! * `slopty mcp` is the same verbs as an MCP server on stdio, for an AI agent.
//! * `slopty server …` runs `slopty-server` as a `LaunchAgent`.
//! * `slopty worker …` talks to the local `slopty-worker` over its control socket.
//! * `slopty hook` is the Claude Code hook relay (`slopty hook install` registers it).
//! * `slopty add <host[:port]>` remembers a worker (today's `slopty-worker`) by its address.
//! * `slopty sessions|attach` are a real client over QUIC straight to a worker: a raw-mode terminal
//!   that renders frames locally. It is the reference client for latency measurements and works
//!   before (and without) the GPUI apps.

#![allow(clippy::print_stdout, clippy::print_stderr, reason = "a CLI; stdout is its UI")]
#![forbid(unsafe_code)]

mod attach;
mod bench;
mod client;
mod hook;
mod link;
mod mcp;
mod service;
mod verbs;
mod workerctl;

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
    /// The server, `host[:port]` (default: `$SLOPTY_SERVER`, else `server` under `[client]` in
    /// settings.toml). `worker install` saves it as the server this Mac registers with.
    #[arg(long, global = true)]
    server: Option<String>,
    /// Print the answer as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    #[command(flatten)]
    Verb(verbs::VerbCmd),
    /// Serve the verbs to an AI agent over MCP on stdio.
    #[command(after_help = mcp::REGISTER_HELP)]
    Mcp,
    /// Run the server as a `LaunchAgent`.
    Server {
        #[command(subcommand)]
        cmd: service::ServerCmd,
    },
    /// Control the local worker daemon.
    Worker {
        #[command(subcommand)]
        cmd: workerctl::WorkerCmd,
    },
    /// Claude Code hook relay: forwards the hook on stdin to the worker daemon (exits 0 always).
    Hook {
        #[command(subcommand)]
        cmd: Option<hook::HookCmd>,
    },
    /// The app's `settings.toml`.
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// Add a worker by its address: a Tailscale `MagicDNS` name, a LAN name or an IP, with an
    /// optional `:port`.
    Add {
        /// `host[:port]`.
        address: String,
    },
    /// Forget a worker.
    Forget {
        /// Worker name, address or id prefix.
        worker: String,
    },
    /// List sessions on a worker.
    Sessions {
        /// Worker name, address or id prefix, or any `host[:port]` (the only worker when
        /// omitted).
        #[arg(long)]
        worker: Option<String>,
    },
    /// Measure application round-trip time to a worker (control-stream ping).
    Ping {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Number of probes.
        #[arg(long, default_value_t = 20)]
        count: u32,
    },
    /// Measure a screen stream (fps, latency on loopback, loss/FEC/NACK counters).
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Attach to a session straight on its worker, or open one and attach when no session is
    /// given. Detach with `^]`.
    Attach {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Session id prefix.
        session: Option<String>,
        /// Working directory for a new session.
        #[arg(long, conflicts_with = "session")]
        cwd: Option<String>,
        /// Program and arguments for a new session, after `--` (the login shell when omitted).
        #[arg(last = true, conflicts_with = "session")]
        command: Vec<String>,
    },
}

/// Benchmarks.
#[derive(Subcommand, Debug)]
enum SettingsCmd {
    /// Print where the app reads its settings from.
    Path,
    /// Write a commented default file there unless one exists.
    Init,
}

#[derive(Subcommand, Debug)]
enum BenchCmd {
    /// Keystroke round trip: a byte to a `cat` session on the worker, timed to the first frame
    /// back.
    Echo {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Samples.
        #[arg(long, default_value_t = 30)]
        count: u32,
    },
    /// Stream a window or display and report what arrived.
    Screen {
        /// Worker name, address or id prefix, or any `host[:port]`.
        #[arg(long)]
        worker: Option<String>,
        /// Print the worker's windows and displays instead of streaming.
        #[arg(long)]
        list: bool,
        /// Window id (see `--list`).
        #[arg(long)]
        window: Option<u32>,
        /// Display id (see `--list`); the main display when neither is given.
        #[arg(long)]
        display: Option<u32>,
        /// Run length in seconds.
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        /// Capture scale, 1.0 = native.
        #[arg(long, default_value_t = 1.0)]
        scale: f32,
        /// Frame rate cap.
        #[arg(long, default_value_t = 60)]
        fps: u16,
        /// Bitrate in Mbit/s.
        #[arg(long, default_value_t = 30)]
        mbit: u32,
        /// Fail the run when the receiver counted more stalls than this. The self-check for a
        /// quiet loopback stream, where the answer is zero.
        #[arg(long)]
        max_stalls: Option<u64>,
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
        Cmd::Worker { cmd } => workerctl::run(cmd, cli.server.as_deref()).await,
        Cmd::Hook { cmd: None } => {
            hook::relay().await;
            Ok(())
        }
        Cmd::Hook { cmd: Some(cmd) } => hook::run(cmd).await,
        Cmd::Settings { cmd } => {
            let path = slopty_settings::path_in(&data_dir);
            match cmd {
                SettingsCmd::Path => println!("{}", path.display()),
                SettingsCmd::Init => {
                    if slopty_settings::Settings::init(&path)? {
                        println!("wrote {}", path.display());
                    } else {
                        println!("{} already exists; left as is", path.display());
                    }
                }
            }
            Ok(())
        }
        Cmd::Verb(cmd) => verbs::run(cmd, cli.server.as_deref(), &data_dir, cli.json).await,
        Cmd::Mcp => mcp::run(cli.server.as_deref(), &data_dir).await,
        Cmd::Server { cmd } => service::server(cmd, &data_dir).await,
        Cmd::Add { address } => client::add(&data_dir, &address).await,
        Cmd::Forget { worker } => client::forget(&data_dir, &worker),
        Cmd::Sessions { worker } => client::sessions(&data_dir, worker.as_deref()).await,
        Cmd::Attach { worker, session: Some(session), .. } => {
            attach::attach(&data_dir, worker.as_deref(), &session).await
        }
        Cmd::Attach { worker, session: None, cwd, command } => {
            attach::open(&data_dir, worker.as_deref(), cwd, command).await
        }
        Cmd::Ping { worker, count } => client::ping(&data_dir, worker.as_deref(), count).await,
        Cmd::Bench { cmd: BenchCmd::Echo { worker, count } } => {
            bench::echo(&data_dir, worker.as_deref(), count).await
        }
        Cmd::Bench { cmd: BenchCmd::Screen { worker, list, .. } } if list => {
            bench::list(&data_dir, worker.as_deref()).await
        }
        Cmd::Bench {
            cmd:
                BenchCmd::Screen {
                    worker, window, display, seconds, scale, fps, mbit, max_stalls, ..
                },
        } => {
            let spec =
                bench::ScreenBench { window, display, seconds, scale, fps, mbit, max_stalls };
            bench::screen(&data_dir, worker.as_deref(), spec).await
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    #[test]
    fn the_command_line_is_well_formed() {
        super::Cli::command().debug_assert();
    }
}
