//! Worker core, transport-agnostic.
//!
//! A session ([`session::spawn`]) is an actor on its own OS thread (the VT engine is `!Send`): it
//! reads the PTY master, feeds the engine, coalesces frames, and fans them out to attached client
//! sinks. [`manager::Worker`] owns the session table and talks to `slopty-ptyd`.
//! [`items::ItemStore`] is the authoritative, persisted item registry.
//! [`repo`] answers which repository a session's working directory is in, which only the
//! machine the shell runs on can know; [`file::read`] reads a file for a file tile,
//! [`listing::folder`] a directory for a folder tile, [`fsop::apply`] makes, moves or trashes an
//! entry for one, and [`search`] searches the files under a directory for text.
//! [`orchestrate::Orchestrator`] answers the verbs the server forwards (open, type, read, wait,
//! files, [`ports`]); [`caps`] says what this worker can do, and [`facts`] what it is and has
//! for a project's placement.

pub mod caps;
pub mod changes;
pub mod clip;
pub mod compress;
pub mod conversation;
pub mod facts;
pub mod file;
pub mod find;
#[cfg(target_os = "macos")]
mod fsevents;
pub mod fsop;
pub mod fswatch;
pub mod handoff;
pub mod items;
pub mod listing;
pub mod manager;
pub mod orchestrate;
pub mod platform;
pub mod ports;
pub mod repo;
pub mod restore;
pub mod screen;
pub mod search;
pub mod session;
pub mod thread;
pub mod wake;
pub mod xfer;

pub use items::ItemStore;
pub use manager::Worker;
pub use screen::{DatagramSink, ScreenError, ScreenStream};
pub use session::{ClientSink, DragData, DragFetch, MAX_DROP_REP_BYTES, SessionHandle};

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

impl WorkerError {
    /// This failure of a request about a terminal, as its client is told it.
    #[must_use]
    pub fn term_error(&self) -> slopty_proto::terminal::TermError {
        use slopty_proto::terminal::TermError;
        match self {
            Self::NoSuchSession
            | Self::SessionClosed
            | Self::Pty(slopty_pty::PtyError::Daemon(
                slopty_proto::ptyd::PtydError::NoSuchSession,
            )) => TermError::NoSuchSession,
            Self::Pty(e) => TermError::Write(e.to_string()),
            Self::Engine(_) | Self::NoSuchItem | Self::Items(_) | Self::Screen(_) => {
                TermError::Engine(self.to_string())
            }
        }
    }
}
