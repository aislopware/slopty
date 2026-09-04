//! Host core, transport-agnostic.
//!
//! A session ([`session::spawn`]) is an actor on its own OS thread (the VT engine is `!Send`): it
//! reads the PTY master, feeds the engine, coalesces frames, and fans them out to attached client
//! sinks. [`manager::Host`] owns the session table and talks to `slopty-ptyd`.

pub mod ctl;
pub mod manager;
pub mod session;

pub use manager::Host;
pub use session::{ClientSink, SessionHandle};

/// Host errors.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// PTY layer.
    #[error(transparent)]
    Pty(#[from] slopty_pty::PtyError),
    /// Engine.
    #[error(transparent)]
    Engine(#[from] slopty_engine::EngineError),
    /// Unknown session.
    #[error("no such session")]
    NoSuchSession,
    /// The session actor is gone.
    #[error("session closed")]
    SessionClosed,
}
