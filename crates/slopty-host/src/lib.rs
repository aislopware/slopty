//! Host core, transport-agnostic.
//!
//! A session ([`session::spawn`]) is an actor on its own OS thread (the VT engine is `!Send`): it
//! reads the PTY master, feeds the engine, coalesces frames, and fans them out to attached client
//! sinks. [`manager::Host`] owns the session table and talks to `slopty-ptyd`.
//! [`canvas::CanvasStore`] is the authoritative, persisted canvas document.
//! [`repo`] answers which repository a session's working directory is in, which only the
//! machine the shell runs on can know.

pub mod canvas;
pub mod ctl;
pub mod manager;
pub mod repo;
pub mod screen;
pub mod session;
pub mod wake;

pub use canvas::CanvasStore;
pub use manager::Host;
pub use screen::{DatagramBudget, ScreenError, ScreenStream};
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
    /// Unknown canvas item.
    #[error("no such canvas item")]
    NoSuchItem,
    /// Canvas document problem.
    #[error("canvas: {0}")]
    Canvas(String),
    /// Remote window pipeline.
    #[error(transparent)]
    Screen(#[from] ScreenError),
}
