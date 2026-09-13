//! The terminal: a view entity that owns the session state and an element that paints it.

pub mod attachment;
pub mod conversation;
mod element;
pub mod latency;
pub mod metrics;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement};
#[cfg(test)]
pub(crate) use element::{family_picks, rows_prepared};
pub use view::{
    ClearScreen, CloseFind, Copy, CopyConversation, CopyLastOutput, Find, FindNext, FindPrev,
    NextPrompt, Paste, PrevPrompt, RerunLast, Selection, TerminalView, TerminalViewEvent,
    ToggleConversation, key_bindings,
};
