//! GPUI views and elements, drawn with the chrome kit (`slopty-kit`).
//!
//! * [`terminal`] — the terminal view (an entity owning a `TermState`) and its element (the
//!   painter). Rows are painted straight from the grid; no intermediate widget tree.
//! * [`add_worker`] — a worker put on a machine over SSH, step by step, for the add sheet and a
//!   tile on a different build.
//! * [`workspace`] — every worker's items as tiles in scrollable columns, the titlebar, actions.
//! * [`screen`] — a remote window or display painted from decoded frames, with input forwarding.
//! * [`clipboard`] — the clipboard shared with the workers: announced, fetched on paste, echoes
//!   broken.
//! * [`conversation`] — a Claude Code terminal's conversation face: the transcript as a list, the
//!   composer that types into the same PTY, the permission card.
//! * [`folder`] — a directory on a worker, browsed in place: files open beside it, rows drag out,
//!   drops go up into it.
//! * [`picker`] — the "add a window" chooser.
//! * [`project`] — a project's board in its orchestrator's tile: the tree, the lanes, the timeline.
//! * [`search`] — search in files on a worker: the matches grouped by file as they stream in.
//! * [`paste_key`] — the system's paste button over the iOS key bar's Paste.
//! * [`keymap`] — every command a key runs, its default chords, and `[keys]` laid over them.
//! * `lints` — the chrome's source lints, as tests, over every crate it is drawn in.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod add_worker;
pub mod authorship;
pub mod browser;
pub mod clipboard;
pub mod conversation;
pub mod file;
pub mod folder;
pub mod keymap;
#[cfg(test)]
mod lints;
mod outbox;
pub mod palette;
pub mod paste_key;
pub mod picker;
pub mod project;
pub mod review;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod screen;
pub mod search;
pub mod settings_editor;
pub mod settings_form;
pub mod terminal;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod workspace;
