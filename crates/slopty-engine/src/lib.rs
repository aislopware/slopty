//! The host-side terminal engine.
//!
//! One [`VtEngine`] per session owns the VT state machine, turns PTY output into
//! [`Frame`] diffs for the wire, serves scrollback pages by absolute line index, and encodes
//! client input (keys, mouse, paste, focus) into the bytes a local terminal would have produced.
//!
//! The only backend today is [`GhosttyEngine`] (libghostty-vt). The trait exists so tests can use
//! a fake and so a second backend can be differential-tested against it.
//!
//! # Absolute line numbering
//!
//! Clients cache scrollback by absolute [`LineIndex`]. The engine keeps a tracked anchor on the
//! newest active row and re-derives the index of screen row 0 after every write, so eviction of
//! old scrollback never shifts indices. When numbering cannot be preserved — reflow on resize,
//! reset, alternate-screen switches, a write that scrolls more than the whole scrollback — the
//! frame's `epoch` is bumped and clients drop their cache.

pub mod convert;
pub mod ghostty;
pub mod osc133;
pub mod search;

pub use ghostty::GhosttyEngine;
use slopty_grid::{Line, LineIndex, TermModes};
use slopty_proto::input::{KeyEvent, MouseEvent};
use slopty_proto::terminal::{Frame, TermSize};

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
}

/// Configuration for a new engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EngineConfig {
    /// Initial size.
    pub size: TermSize,
    /// Maximum scrollback lines the host retains.
    pub scrollback_lines: u32,
}

/// The terminal engine interface.
pub trait VtEngine {
    /// Feed PTY output.
    fn write(&mut self, bytes: &[u8]);

    /// Resize; reflows the primary screen and invalidates line numbering.
    fn resize(&mut self, size: TermSize) -> Result<(), EngineError>;

    /// Current size.
    fn size(&self) -> TermSize;

    /// Produce the next diff if anything changed since the last call. `input_ack` is the highest
    /// key sequence number whose bytes reached the PTY before this frame's output was consumed.
    fn take_frame(&mut self, input_ack: u64) -> Result<Option<Frame>, EngineError>;

    /// Produce a frame carrying every row (attach, resync).
    fn full_frame(&mut self, input_ack: u64) -> Result<Frame, EngineError>;

    /// Scrollback lines by absolute index. Lines outside `[oldest, total)` are omitted, so the
    /// result may be shorter than `count`; it starts at `start` clamped to `oldest`.
    fn lines(&self, start: LineIndex, count: u32) -> Result<(LineIndex, Vec<Line>), EngineError>;

    /// Current terminal modes the client needs (prediction gating, wheel routing).
    fn modes(&self) -> Result<TermModes, EngineError>;

    /// Find `needle` in the retained history and the screen (see [`search::find`]);
    /// `regex` treats it as a pattern and fails with [`EngineError::Pattern`] when it does
    /// not compile.
    fn search(&self, needle: &str, regex: bool, max: u32) -> Result<search::Found, EngineError>;

    /// Encode a key event into `out`. Appends nothing for keys the terminal does not encode.
    fn encode_key(&mut self, event: &KeyEvent, out: &mut Vec<u8>) -> Result<(), EngineError>;

    /// Encode a mouse event into `out` according to the active tracking mode and format.
    fn encode_mouse(&mut self, event: &MouseEvent, out: &mut Vec<u8>) -> Result<(), EngineError>;

    /// Encode pasted text, bracketed when the program asked for it. Unsafe control characters
    /// are stripped when not bracketed.
    fn encode_paste(&mut self, text: &str, out: &mut Vec<u8>) -> Result<(), EngineError>;

    /// Encode a focus change, if the program asked to be told.
    fn encode_focus(&mut self, focused: bool, out: &mut Vec<u8>) -> Result<(), EngineError>;

    /// Side effects since the last drain.
    fn drain_events(&mut self) -> Vec<EngineEvent>;
}
