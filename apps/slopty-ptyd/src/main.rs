//! `slopty-ptyd` — the PTY custodian.
//!
//! Holds every PTY master so `slopty-worker` can be restarted (rebuilt, upgraded, crashed)
//! without killing shells. While no worker is attached it drains output into a bounded ring so
//! the child never blocks; on attach it hands the master over `SCM_RIGHTS` with the backlog.
//! Deliberately tiny: no VT parsing, no networking, nothing that changes often.

#![forbid(unsafe_code)]

mod daemon;
mod session;

include!(concat!(env!("OUT_DIR"), "/custody.rs"));

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
    /// Bytes of output retained per session: what came in while no worker held it, or what the
    /// worker tapped since its last checkpoint. At most 4 MiB: the backlog and the checkpoint
    /// go to the next worker in one frame.
    #[arg(long, default_value_t = slopty_pty::protocol::DEFAULT_BACKLOG_BYTES,
          value_parser = backlog_bytes)]
    backlog_bytes: usize,
    /// Where the shell integration scripts are written (default: `$SLOPTY_DATA_DIR/shell`, else
    /// `shell/` next to the socket). `SLOPTY_NO_SHELL_INTEGRATION=1` leaves shells untouched.
    #[arg(long)]
    shell_dir: Option<PathBuf>,
    /// Print this build's custody fingerprint and exit: what a running ptyd hands a worker
    /// (its protocol and the shell scripts it writes). An install keeps a running ptyd, and
    /// every session it holds, when the new build prints what that one wrote beside its socket.
    #[arg(long)]
    custody: bool,
}

/// `--backlog-bytes`, refused past [`slopty_pty::protocol::MAX_BACKLOG_BYTES`].
fn backlog_bytes(text: &str) -> Result<usize, String> {
    let max = slopty_pty::protocol::MAX_BACKLOG_BYTES;
    match text.parse::<usize>() {
        Ok(bytes) if bytes <= max => Ok(bytes),
        Ok(bytes) => Err(format!("{bytes} is more than the {max} one attach carries")),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.custody {
        use std::io::Write as _;
        writeln!(std::io::stdout().lock(), "{CUSTODY}")?;
        return Ok(());
    }
    // The custodian stays clear of the platform crate; its service always sets the data dir.
    if let Some(data_dir) = std::env::var_os("SLOPTY_DATA_DIR") {
        slopty_crash::install(slopty_crash::Process::Ptyd, Path::new(&data_dir));
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let socket = args.socket.unwrap_or_else(slopty_pty::protocol::socket_path);
    let shell_dir = args.shell_dir.unwrap_or_else(|| {
        std::env::var_os("SLOPTY_DATA_DIR").map_or_else(
            || socket.parent().unwrap_or_else(|| Path::new(".")).join("shell"),
            |data| PathBuf::from(data).join("shell"),
        )
    });
    daemon::run(&socket, args.backlog_bytes, &shell_dir, CUSTODY).await
}
