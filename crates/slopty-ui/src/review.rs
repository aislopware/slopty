//! The review tile: what an agent's thread changed in its working tree, read file by file.
//!
//! On the left, the files by weight, tests, fixtures, locks and generated code quieter below;
//! along the top, the span it covers (the last turn, since the person last reviewed, every
//! turn, what is uncommitted, the whole branch); in the middle, one diff of every file, unified
//! at every width, the files stacked. Each hunk and each file is kept or put back; a click on a
//! line comments on it, and the comments go to the agent as one message. "Mark reviewed" keeps
//! everything shown, so what changes after is what "Since reviewed" shows.
//!
//! The keyboard walks it too ([`key_bindings`]): ↓ and ↑ (or j and k) by change, with ⇧ by
//! file; ⌘Y keeps the change it stands on and ⌘N puts it back, `c` comments on it and `v`
//! marks its file viewed. Each is in the palette.
//!
//! It reads the worker's review frames through the thread's hub, and what the person does goes
//! through the hub's outbox like any other intent.
//!
//! * [`findings`] — what an agent's own review found, read from its answer.
//! * [`model`] — the files in order, the comments, and the picks; nothing draws.
//! * [`view`] — the tile, [`ReviewView`], for a pane to host.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

pub mod findings;
pub mod model;
pub mod view;

pub use model::Scope;
pub use view::{ReviewEvent, ReviewView, TaskDoor};

gpui::actions!(
    review,
    [
        /// Stand on the next change.
        NextChange,
        /// Stand on the change above.
        PreviousChange,
        /// Stand on the next file's head.
        NextFile,
        /// Stand on the file above's head.
        PreviousFile,
        /// Keep the change the keyboard stands on, then stand on the next.
        KeepChange,
        /// Put back the change the keyboard stands on, then stand on the next.
        PutBackChange,
        /// Comment on the change the keyboard stands on.
        CommentOnChange,
        /// Mark the file the keyboard is in viewed, folding it, or not viewed.
        ToggleViewed,
        /// Show the list of files, to go to one.
        ShowFiles,
    ]
);

/// A review tile's key context while the diff has the keyboard. Its bare keys stand down while
/// a comment's field takes typing.
pub const CTX: &str = "Review";

/// The review's commands and their keys: Zed's ⌘Y and ⌘N for keep and put back, and bare keys
/// to walk, as a board's are.
#[must_use]
pub fn key_bindings() -> Vec<crate::keymap::Command> {
    use crate::keymap::{Command, Scope};
    const KEYS: &[Option<&str>] = &[Some("Review && !Input")];
    vec![
        Command::new(Scope::Review, "next_change", NextChange, &["down", "j"], KEYS),
        Command::new(Scope::Review, "previous_change", PreviousChange, &["up", "k"], KEYS),
        Command::new(Scope::Review, "next_file", NextFile, &["shift-down", "shift-j"], KEYS),
        Command::new(Scope::Review, "previous_file", PreviousFile, &["shift-up", "shift-k"], KEYS),
        Command::new(Scope::Review, "keep_change", KeepChange, &["cmd-y"], KEYS),
        Command::new(Scope::Review, "put_back_change", PutBackChange, &["cmd-n"], KEYS),
        Command::new(Scope::Review, "comment_on_change", CommentOnChange, &["c"], KEYS),
        Command::new(Scope::Review, "toggle_viewed", ToggleViewed, &["v"], KEYS),
        Command::new(Scope::Review, "show_files", ShowFiles, &["cmd-shift-o"], KEYS),
    ]
}

/// The palette's lines for a review, with their keys.
#[must_use]
pub fn palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    let line = |label: &str, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, action, bindings)
    };
    vec![
        line("Next change", Box::new(NextChange)),
        line("Previous change", Box::new(PreviousChange)),
        line("Next file", Box::new(NextFile)),
        line("Previous file", Box::new(PreviousFile)),
        line("Keep the change", Box::new(KeepChange)),
        line("Put back the change", Box::new(PutBackChange)),
        line("Comment on the change", Box::new(CommentOnChange)),
        line("Mark the file viewed", Box::new(ToggleViewed)),
        line("Show the review's files", Box::new(ShowFiles)),
    ]
}

#[cfg(test)]
mod tests;
