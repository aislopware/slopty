//! The terminal: a view entity that owns the session state and an element that paints it.

mod element;
pub mod latency;
pub mod metrics;
mod sprite;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement};
#[cfg(test)]
pub(crate) use element::{captions_drawn, family_picks, rows_prepared};
pub use view::{
    ClearScreen, CloseFind, Copy, CopyLastOutput, Find, FindNext, FindPrev, NextPrompt,
    NoteLastBlock, Paste, PlacedImage, PrevPrompt, RerunLast, Selection, TOOK_MIN, TerminalView,
    TerminalViewEvent, key_bindings, took_label,
};
