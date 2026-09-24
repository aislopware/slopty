//! GPUI views and elements.
//!
//! * [`terminal`] — the terminal view (an entity owning a `TermState`) and its element (the
//!   painter). Rows are painted straight from the grid; no intermediate widget tree.
//! * [`a11y`] — the keyboard ring and, for tests, the accessibility tree.
//! * [`workspace`] — every worker's items as tiles in scrollable columns, the titlebar, actions.
//! * [`screen`] — a remote window or display painted from decoded frames, with input forwarding.
//! * [`clipboard`] — the clipboard shared with the workers: announced, fetched on paste, echoes
//!   broken.
//! * [`note`] — a sticky note read as Markdown and edited in place, text shared through the
//!   document.
//! * [`markdown`] — the one `TextView` style every Markdown surface draws by.
//! * [`picker`] — the "add a window" chooser.
//! * [`keys`] — GPUI keystrokes → protocol key events.
//! * [`colors`] — theme tokens → GPUI colours.
//! * [`kit`] — gpui-kit's theme kept on the same tokens.
//! * [`frames`] — the UI frame-time probe (draw percentiles, cadence, drops).
//! * [`fonts`] — bundled `JetBrains Mono` + Nerd symbols, registered at startup.

pub mod a11y;

pub mod chrome_text;
pub mod clipboard;
pub mod colors;
pub mod file;
pub mod fonts;
pub mod frames;
pub mod highlight;
pub mod keys;
pub mod kit;
pub mod markdown;
pub mod note;
pub mod palette;
pub mod picker;
pub mod screen;
pub mod settings_editor;
pub mod terminal;
pub mod workspace;
