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
//! preserved — reflow on resize, reset, a write that scrolls more than the whole scrollback — the
//! frame carries a new `epoch` and clients drop their cache. The alternate screen numbers its
//! own rows under an epoch of its own, and leaving it gives the primary its epoch back.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod convert;
pub mod ghostty;
pub mod graphics;
pub mod osc133;
pub mod placeholder;
pub mod search;

pub use ghostty::{
    ClipboardSource, Compression, DropOperation, DropPoint, Dropped, GhosttyEngine, Joined, Memory,
    PasteRep, Streamed, TEXT_MIME,
};
pub use graphics::ImageUpload;
use slopty_proto::terminal::{ColorOverrides, PointerShape, ProgramStatus, Progress, TermSize};

/// Engine failure. libghostty-vt reports out-of-memory and invalid arguments; both are bugs or
/// resource exhaustion, never a consequence of PTY output, so the session is torn down.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The VT library failed.
    #[error("libghostty-vt: {0}")]
    Vt(#[from] libghostty_vt::Error),
    /// A size was rejected (zero columns/rows, past `MAX_COLS` × `MAX_ROWS`, or zero cell
    /// metrics).
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
    /// The program started or stopped asking for drops (Kitty drag and drop, OSC 72): a
    /// viewer's drag over the tile goes to it while it asks.
    DropTarget {
        /// It asks now.
        accepts: bool,
    },
    /// The program answered the drag over the terminal, for the drag's feedback.
    DropAccepted {
        /// What a drop would do.
        operation: DropOperation,
        /// The MIME types it wants of the drag, most wanted first; empty when it did not say.
        mimes: Vec<String>,
    },
    /// The program asked for the dropped type at `index` of the drop's list, whose bytes are
    /// not here: they are to be fetched and given ([`GhosttyEngine::drop_data`], or streamed
    /// with [`GhosttyEngine::drop_chunk`] and [`GhosttyEngine::drop_end`]). Told once per
    /// type until it is given or found gone.
    DropWants {
        /// Its index in the drop's list.
        index: usize,
    },
    /// The drop ended: the program concluded it, refused it, or another drag replaced it.
    DropConcluded {
        /// What the program did with the data.
        operation: DropOperation,
    },
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
    /// The program's progress report changed (`OSC 9;4`), or was dropped because the shell
    /// printed its next prompt.
    Progress(Progress),
    /// The program's status records changed (`OSC 7501`), or a prompt ended some: the whole
    /// set, by id.
    ProgramStatus(Vec<ProgramStatus>),
    /// The program asked for another pointer shape over the grid (`OSC 22`).
    Pointer(PointerShape),
}

/// Configuration for a new engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EngineConfig {
    /// Initial size.
    pub size: TermSize,
    /// Maximum scrollback lines the worker retains.
    pub scrollback_lines: u32,
}
