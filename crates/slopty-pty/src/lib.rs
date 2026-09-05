//! PTY plumbing shared by the host daemon and the PTY custodian (`slopty-ptyd`).
//!
//! * [`pty`] — open a pseudo-terminal, spawn a child on it, resize it, do async I/O on the master.
//! * [`ring`] — the bounded byte ring ptyd drains output into while no host is attached.
//! * [`protocol`] — messages between hostd and ptyd, framed with [`slopty_proto::codec`].
//! * [`fdpass`] — `SCM_RIGHTS` transfer of the master over the Unix socket.
//! * [`client`] — the hostd side of the ptyd socket.
//! * [`shell_integration`] — bundled zsh scripts that emit OSC 133 prompt marks.

pub mod client;
pub mod fdpass;
pub mod protocol;
pub mod pty;
pub mod ring;
pub mod shell_integration;

pub use client::PtydClient;
pub use pty::{Pty, PtyMaster, SpawnSpec};
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
    /// ptyd spoke a different protocol version.
    #[error("ptyd protocol mismatch: ours {ours}, theirs {theirs}")]
    ProtocolMismatch {
        /// Our version.
        ours: u16,
        /// Theirs.
        theirs: u16,
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
