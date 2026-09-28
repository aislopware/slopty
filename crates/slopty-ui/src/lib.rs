//! GPUI views and elements.
//!
//! * [`terminal`] — the terminal view (an entity owning a `TermState`) and its element (the
//!   painter). Rows are painted straight from the grid; no intermediate widget tree.
//! * [`a11y`] — the keyboard ring and, for tests, the accessibility tree.
//! * [`workspace`] — every worker's items as tiles in scrollable columns, the titlebar, actions.
//! * [`screen`] — a remote window or display painted from decoded frames, with input forwarding.
//! * [`clipboard`] — the clipboard shared with the workers: announced, fetched on paste, echoes
//!   broken.
//! * [`conversation`] — a Claude Code terminal's conversation face: the transcript as a list, the
//!   composer that types into the same PTY, the permission card.
//! * [`note`] — a sticky note read as Markdown and edited in place, text shared through the
//!   document.
//! * [`folder`] — a directory on a worker, browsed in place: files open beside it, rows drag out,
//!   drops go up into it.
//! * [`markdown`] — the one `TextView` style every Markdown surface draws by.
//! * [`picker`] — the "add a window" chooser.
//! * [`paste_key`] — the system's paste button over the iOS key bar's Paste.
//! * [`keys`] — GPUI keystrokes → protocol key events.
//! * [`colors`] — theme tokens → GPUI colours.
//! * [`kit`] — gpui-kit's theme kept on the same tokens.
//! * [`frames`] — the UI frame-time probe (draw percentiles, cadence, drops).
//! * [`shown`] — work timed at the instant a paint reached the display.
//! * [`fonts`] — bundled `JetBrains Mono` + Nerd symbols, registered at startup.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod a11y;
pub mod browser;

pub mod chrome_text;
pub mod clipboard;
pub mod colors;
pub mod conversation;
pub mod file;
pub mod folder;
pub mod fonts;
pub mod frames;
pub mod highlight;
pub mod icons;
pub mod keys;
pub mod kit;
pub mod markdown;
pub mod note;
pub mod palette;
pub mod paste_key;
pub mod picker;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod screen;
pub mod settings_editor;
pub mod settings_form;
pub mod shown;
pub mod terminal;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod workspace;
