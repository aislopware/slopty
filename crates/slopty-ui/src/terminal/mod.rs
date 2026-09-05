//! The terminal: a view entity that owns the session state and an element that paints it.

pub mod conversation;
mod element;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement};
pub use view::{
    CloseFind, Copy, Find, FindNext, FindPrev, Paste, Selection, TerminalView, TerminalViewEvent,
    key_bindings,
};
