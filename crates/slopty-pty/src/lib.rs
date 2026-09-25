//! PTY plumbing shared by the worker daemon and the PTY custodian (`slopty-ptyd`).
//!
//! * [`pty`] — open a pseudo-terminal, spawn a child on it, resize it, do async I/O on the master.
//! * [`ring`] — the bounded byte ring ptyd drains output into while no worker is attached.
//! * [`protocol`] — messages between the worker and ptyd, framed with [`slopty_proto::codec`].
//! * [`fdpass`] — frames plus `SCM_RIGHTS` fds over the Unix socket.
//! * [`client`] — the worker side of the ptyd socket.
//! * [`shell_integration`] — bundled zsh scripts that emit OSC 133 prompt marks.
//! * [`terminfo`] — ghostty's terminfo entry, compiled on start-up so `TERM=xterm-ghostty`.
//! * [`process`] — the foreground process of a tty, for attributing agent sessions.

pub mod client;
pub mod fdpass;
pub mod process;
pub mod protocol;
pub mod pty;
pub mod ring;
pub mod shell_integration;
pub mod terminfo;

pub use client::PtydClient;
pub use pty::{LineDiscipline, Pty, PtyMaster, SpawnSpec};
pub use ring::Ring;

/// PTY errors.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    /// A system call failed.
    #[error("{context}: {source}")]
    Os {
        /// What we were doing.
        context: &'static str,
        /// The errno.
        #[source]
        source: std::io::Error,
    },
    /// The daemon answered with an error.
    #[error("ptyd: {0}")]
    Daemon(String),
    /// The daemon answered with something unexpected.
    #[error("ptyd: unexpected reply")]
    UnexpectedReply,
    /// The socket closed.
    #[error("ptyd: connection closed")]
    Closed,
    /// Framing failed.
    #[error(transparent)]
    Codec(#[from] slopty_proto::codec::CodecError),
}

impl PtyError {
    pub(crate) fn os(context: &'static str, source: impl Into<std::io::Error>) -> Self {
        Self::Os { context, source: source.into() }
    }
}
