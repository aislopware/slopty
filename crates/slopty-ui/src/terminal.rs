//! The terminal: a view entity that owns the session state and an element that paints it.

pub mod attachment;
pub mod conversation;
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
    ClearScreen, CloseFind, Copy, CopyConversation, CopyLastOutput, Find, FindNext, FindPrev,
    NextPrompt, NoteLastBlock, Paste, PlacedImage, PrevPrompt, RerunLast, Selection, TOOK_MIN,
    TerminalView, TerminalViewEvent, ToggleConversation, key_bindings, took_label,
};
