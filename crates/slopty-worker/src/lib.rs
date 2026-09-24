//! Worker core, transport-agnostic.
//!
//! A session ([`session::spawn`]) is an actor on its own OS thread (the VT engine is `!Send`): it
//! reads the PTY master, feeds the engine, coalesces frames, and fans them out to attached client
//! sinks. [`manager::Worker`] owns the session table and talks to `slopty-ptyd`.
//! [`items::ItemStore`] is the authoritative, persisted item registry.
//! [`repo`] answers which repository a session's working directory is in, which only the
//! machine the shell runs on can know; [`file::read`] reads a file for a file card.
//! [`orchestrate::Orchestrator`] answers the verbs the server forwards (open, type, read, wait,
//! files, [`ports`]); [`caps`] says what this worker can do.

pub mod caps;
pub mod clip;
pub mod ctl;
pub mod file;
pub mod find;
pub mod items;
pub mod manager;
pub mod orchestrate;
pub mod ports;
pub mod repo;
pub mod screen;
pub mod session;
pub mod wake;
pub mod xfer;

pub use items::ItemStore;
pub use manager::Worker;
pub use screen::{DatagramSink, ScreenError, ScreenStream};
pub use session::{ClientSink, SessionHandle};

/// Worker errors.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
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
    /// Unknown item.
    #[error("no such item")]
    NoSuchItem,
    /// Item registry problem.
    #[error("items: {0}")]
    Items(String),
    /// Remote window pipeline.
    #[error(transparent)]
    Screen(#[from] ScreenError),
}
