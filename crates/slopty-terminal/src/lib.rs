//! Slopty's terminal: a view entity that owns a session's state and an element that paints it.
//!
//! The rows are painted straight from the grid, with no widget tree between. Around the grid the
//! view holds its find bar, copy mode, links and paths, pictures, pastes and drops, and its own
//! commands ([`commands`]), which the app's keymap takes into its one table. It is a client
//! view of the term stream, drawn with the chrome kit (`slopty-kit`); the views around it
//! (`slopty-ui`) sit on it, and it never reaches up to them (`docs/decisions/ui.md`, "The
//! terminal is a crate of its own").

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod copy_mode;
mod element;
pub mod latency;
pub mod metrics;
pub mod progress;
mod scrollbar;
mod sprite;
pub mod url;
mod view;

#[cfg(test)]
pub(crate) use element::captions_drawn;
pub use element::{CellMetrics, Prepared, TerminalElement};
#[cfg(any(test, feature = "e2e"))]
pub use element::{family_picks, rows_prepared};
pub use view::{
    AttachBlock, AttachProbe, AttachSelection, BACK_TO_LIVE, COPY_MODE, COPY_MODE_DONE, CTX,
    ClearScreen, ClipHook, ClipPaste, CloseFind, Copy, CopyBlockOutput, CopyLastOutput, CopyMode,
    Find, FindNext, FindPrev, GridFrame, Guesses, KeyCommand, LinkArrival, NextPrompt, Paste,
    PlacedImage, PrevPrompt, RerunLast, SEARCH_CTX, ScrollPageDown, ScrollPageUp, ScrollToBottom,
    ScrollToTop, SelectAll, Selection, SendEscape, TOOK_MIN, TerminalView, TerminalViewEvent,
    commands, escape,
};
#[cfg(target_os = "macos")]
pub use view::{DropHook, DropNews, SinkDropped};
