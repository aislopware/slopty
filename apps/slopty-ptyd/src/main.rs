//! `slopty-ptyd` — the PTY custodian.
//!
//! Holds every PTY master so `slopty-worker` can be restarted (rebuilt, upgraded, crashed)
//! without killing shells. While no worker is attached it drains output into a bounded ring so
//! the child never blocks; on attach it hands the master over `SCM_RIGHTS` with the backlog.
//! An update of ptyd itself is handed every session: the running one runs the new build in
//! place (`slopty-ptyd --succeed`). Deliberately tiny: no VT parsing, no networking, nothing
//! that changes often.

// One `unsafe` call, allowed where it stands: owning the descriptors the image before this one
// kept open for it across a handover (`daemon::inherit`).
#![deny(unsafe_code)]

mod daemon;
mod session;
mod succeed;

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
    /// Ask the slopty-ptyd running on the socket to become this build, handing it every
    /// session, and wait until it has. An install runs this when the running one's custody
    /// differs and its succession (`--custody`, the second word) is this build's.
    #[arg(long, conflicts_with = "custody")]
    succeed: bool,
    /// Print how many sessions the slopty-ptyd running on the socket holds, and exit: what an
    /// install says ending them costs when it restarts that ptyd. Its child processes would
    /// miss the sessions handed to it after a crash, whose shells are no children of it.
    #[arg(long, conflicts_with_all = ["custody", "succeed"])]
    sessions: bool,
    /// Take every session the ptyd before this build handed over in the state file open on
    /// this descriptor, which says how that ptyd ran too. Only a ptyd running this build in
    /// place passes it ([`slopty_pty::protocol::inherit_args`]).
    #[arg(long = INHERIT, hide = true, value_name = "FD")]
    inherit: Option<i32>,
}

/// [`slopty_pty::protocol::INHERIT_FLAG`] as clap names a flag.
const INHERIT: &str = match slopty_pty::protocol::INHERIT_FLAG.split_at_checked(2) {
    Some((_dashes, name)) => name,
    None => slopty_pty::protocol::INHERIT_FLAG,
};

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
        writeln!(std::io::stdout().lock(), "{CUSTODY} {SUCCESSION}")?;
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
    if args.sessions {
        use std::io::Write as _;
        let (mut ptyd, _exits) = slopty_pty::PtydClient::connect(&socket).await?;
        let held = ptyd.list().await?.len();
        writeln!(std::io::stdout().lock(), "{held}")?;
        return Ok(());
    }
    if args.succeed {
        return succeed::run(&socket).await;
    }
    // A descriptor per session's master and per connection, past launchd's 256.
    match raise_descriptor_limit() {
        Ok(limit) => tracing::debug!(limit, "descriptor limit"),
        Err(e) => tracing::warn!(error = %e, "descriptor limit not raised; sessions may run short"),
    }
    let shell_dir = args.shell_dir.unwrap_or_else(|| {
        std::env::var_os("SLOPTY_DATA_DIR").map_or_else(
            || socket.parent().unwrap_or_else(|| Path::new(".")).join("shell"),
            |data| PathBuf::from(data).join("shell"),
        )
    });
    daemon::run(daemon::Config {
        socket,
        backlog_bytes: args.backlog_bytes,
        shell_dir,
        custody: CUSTODY,
        succession: SUCCESSION,
        inherit: args.inherit,
    })
    .await
}

/// Raise the soft limit on open descriptors to the hard one (on macOS at most `OPEN_MAX`,
/// 10240, past which `setrlimit` refuses); the limit it is now.
fn raise_descriptor_limit() -> std::io::Result<u64> {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limit = getrlimit(Resource::Nofile);
    let hard = limit.maximum.unwrap_or(u64::MAX);
    #[cfg(target_vendor = "apple")]
    let want = hard.min(10_240);
    #[cfg(not(target_vendor = "apple"))]
    let want = hard;
    match limit.current {
        None => Ok(u64::MAX),
        Some(now) if now >= want => Ok(now),
        Some(_) => {
            setrlimit(Resource::Nofile, Rlimit { current: Some(want), maximum: limit.maximum })?;
            Ok(want)
        }
    }
}
