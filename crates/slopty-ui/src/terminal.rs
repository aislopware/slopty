//! The terminal: a view entity that owns the session state and an element that paints it.

mod copy_mode;
mod element;
pub mod latency;
pub mod metrics;
pub mod progress;
mod scrollbar;
mod sprite;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement};
#[cfg(test)]
pub(crate) use element::{captions_drawn, family_picks, rows_prepared};
pub use view::{
    AttachBlock, AttachProbe, AttachSelection, BACK_TO_LIVE, COPY_MODE, COPY_MODE_DONE, CTX,
    ClearScreen, ClipHook, ClipPaste, CloseFind, Copy, CopyBlockOutput, CopyLastOutput, CopyMode,
    Find, FindNext, FindPrev, Guesses, KeyCommand, LinkArrival, NextPrompt, Paste, PlacedImage,
    PrevPrompt, RerunLast, SEARCH_CTX, ScrollPageDown, ScrollPageUp, ScrollToBottom, ScrollToTop,
    SelectAll, Selection, SendEscape, TOOK_MIN, TerminalView, TerminalViewEvent, commands, escape,
};
#[cfg(target_os = "macos")]
pub use view::{DropHook, DropNews, SinkDropped};
