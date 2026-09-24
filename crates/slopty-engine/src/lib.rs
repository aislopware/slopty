//! The worker-side terminal engine.
//!
//! One [`GhosttyEngine`] per session owns the VT state machine (libghostty-vt), turns PTY
//! output into `Frame` diffs for the wire, serves scrollback pages by absolute line index, and
//! encodes client input (keys, mouse, paste, focus) into the bytes a local terminal would have
//! produced.
//!
//! # Absolute line numbering
//!
//! Clients cache scrollback by absolute [`LineIndex`](slopty_grid::LineIndex). The
//! engine keeps a tracked anchor on the newest active row and re-derives the index of screen row 0
//! after every write, so eviction of old scrollback never shifts indices. When numbering cannot be
//! preserved — reflow on resize, reset, alternate-screen switches, a write that scrolls more than
//! the whole scrollback — the frame's `epoch` is bumped and clients drop their cache.

#![forbid(unsafe_code)]

pub mod boundary;
pub mod convert;
pub mod ghostty;
pub mod graphics;
pub mod osc133;
pub mod placeholder;
pub mod search;

pub use ghostty::GhosttyEngine;
pub use graphics::ImageUpload;
use slopty_proto::terminal::{ColorOverrides, TermSize};

/// Engine failure. libghostty-vt reports out-of-memory and invalid arguments; both are bugs or
/// resource exhaustion, never a consequence of PTY output, so the session is torn down.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The VT library failed.
    #[error("libghostty-vt: {0}")]
    Vt(#[from] libghostty_vt::Error),
    /// A size was rejected (zero columns/rows or zero cell metrics).
    #[error("invalid terminal size: {0}")]
    InvalidSize(&'static str),
    /// A search pattern did not compile.
    #[error("invalid pattern: {0}")]
    Pattern(String),
}

/// Side effects the VT state machine produced while consuming PTY output.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EngineEvent {
    /// Bytes to write back to the PTY (query responses such as DA, DSR, XTVERSION).
    PtyWrite(Vec<u8>),
    /// BEL.
    Bell,
    /// Window title changed (OSC 0/2).
    Title(String),
    /// Working directory reported (OSC 7).
    Cwd(String),
    /// The program wrote to the clipboard (OSC 52). Text representation only.
    ClipboardWrite {
        /// The text.
        text: String,
    },
    /// The program asked for a desktop notification (OSC 9, OSC 777 `notify`, OSC 99).
    Notification {
        /// Its title; empty when the protocol carries none (OSC 9).
        title: String,
        /// Its body.
        body: String,
    },
    /// The program changed or reset the terminal's colours (OSC 4/10/11/12, 104/110/111/112,
    /// a full reset); the whole set now over the driver's.
    Colors(ColorOverrides),
}

/// Configuration for a new engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EngineConfig {
    /// Initial size.
    pub size: TermSize,
    /// Maximum scrollback lines the worker retains.
    pub scrollback_lines: u32,
}
