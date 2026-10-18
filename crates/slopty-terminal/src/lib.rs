//! Slopty's terminal: a view entity that owns a session's state and an element that paints it.
//!
//! The rows are painted straight from the grid, with no widget tree between. Around the grid the
//! view holds its find bar, copy mode, links and paths, pictures, pastes and drops, and its own
//! commands ([`commands`]), which [`bindings`] turns into the keys the app binds inside the
//! shell's keymap. It is a client view of the term stream whose floats (find bar, menu, pills,
//! rail) are the shell's own parts (`slopty-shell`; `docs/decisions/ui.md`, "Slopty's own views
//! draw with the shell's helpers"); the views around it (`slopty-ui`) sit on it, and it never
//! reaches up to them (`docs/decisions/ui.md`, "The terminal is a crate of its own").

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
mod sprite;
pub mod url;
mod view;

pub use element::{CellMetrics, Prepared, TerminalElement, fits};
pub use view::{
    AttachBlock, AttachProbe, AttachSelection, BACK_TO_LIVE, COPY_MODE, COPY_MODE_DONE, CTX,
    ClearScreen, ClipHook, ClipPaste, CloseFind, Copy, CopyBlockOutput, CopyLastOutput, CopyMode,
    Find, FindNext, FindPrev, GridFrame, Guesses, KeyCommand, LinkArrival, NextPrompt, Paste,
    PlacedImage, PrevPrompt, RerunLast, SEARCH_CTX, ScrollPageDown, ScrollPageUp, ScrollToBottom,
    ScrollToTop, SelectAll, Selection, SendEscape, TerminalView, TerminalViewEvent,
    ToggleMatchCase, ToggleRegex, ToggleWholeWord, bindings, commands, escape,
};
#[cfg(target_os = "macos")]
pub use view::{DropHook, DropNews, SinkDropped};

#[cfg(test)]
mod tests {
    /// The shell falls back to the face the kit bundles and registers, by the same name.
    #[test]
    fn the_shell_falls_back_to_the_bundled_symbol_face() {
        assert_eq!(slopty_shell::typography::SYMBOLS_FAMILY, slopty_kit::fonts::SYMBOLS_FAMILY);
    }
}
