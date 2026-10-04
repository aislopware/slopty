//! The terminal: a view entity that owns the session state and an element that paints it.

mod element;
pub mod latency;
pub mod metrics;
mod progress;
mod scrollbar;
mod sprite;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement};
#[cfg(test)]
pub(crate) use element::{captions_drawn, family_picks, rows_prepared};
pub use view::{
    AttachBlock, AttachProbe, AttachSelection, BACK_TO_LIVE, ClearScreen, ClipHook, ClipPaste,
    CloseFind, Copy, CopyBlockOutput, CopyLastOutput, Find, FindNext, FindPrev, Guesses,
    LinkArrival, NextPrompt, NoteLastBlock, Paste, PlacedImage, PrevPrompt, RerunLast,
    ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop, SelectAll, Selection, TOOK_MIN,
    TerminalView, TerminalViewEvent, key_bindings,
};
