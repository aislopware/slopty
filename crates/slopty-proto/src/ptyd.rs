//! What the pty daemon answers when a request fails. The daemon holds the shells across worker
//! restarts; the worker tells a gone session from one another connection holds by the variant.

use serde::{Deserialize, Serialize};

/// Why the pty daemon refused a request.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, thiserror::Error)]
pub enum PtydError {
    /// A spawn named a session the daemon already holds.
    #[error("the session already exists")]
    SessionExists,
    /// The session ended, or never was.
    #[error("no such session")]
    NoSuchSession,
    /// Another connection (an older worker) holds the session.
    #[error("attached by another connection")]
    AttachedElsewhere,
    /// The OS refused: a fork, an exec, a pty that could not be opened.
    #[error("{0}")]
    Os(String),
}
