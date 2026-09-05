//! `slopty-ptyd` — the PTY custodian.
//!
//! Holds every PTY master so `slopty-hostd` can be restarted (rebuilt, upgraded, crashed)
//! without killing shells. While no host is attached it drains output into a bounded ring so
//! the child never blocks; on attach it hands the master over `SCM_RIGHTS` with the backlog.
//! Deliberately tiny: no VT parsing, no networking, nothing that changes often.

mod daemon;
mod session;

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

/// Command line.
#[derive(Parser, Debug)]
#[command(name = "slopty-ptyd", version, about)]
struct Args {
    /// Socket path (default: `$TMPDIR/slopty/ptyd.sock`, or `$SLOPTY_PTYD_SOCKET`).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Bytes of output retained per detached session.
    #[arg(long, default_value_t = slopty_pty::protocol::DEFAULT_BACKLOG_BYTES)]
    backlog_bytes: usize,
    /// Where the shell integration scripts are written (default: `$SLOPTY_DATA_DIR/shell`, else
    /// `shell/` next to the socket). `SLOPTY_NO_SHELL_INTEGRATION=1` leaves shells untouched.
    #[arg(long)]
    shell_dir: Option<PathBuf>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let socket = args.socket.unwrap_or_else(slopty_pty::protocol::socket_path);
    let shell_dir = args.shell_dir.unwrap_or_else(|| {
        std::env::var_os("SLOPTY_DATA_DIR").map_or_else(
            || socket.parent().unwrap_or_else(|| Path::new(".")).join("shell"),
            |data| PathBuf::from(data).join("shell"),
        )
    });
    daemon::run(&socket, args.backlog_bytes, &shell_dir).await
}
