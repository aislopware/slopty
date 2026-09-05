//! GPUI views and elements.
//!
//! * [`terminal`] — the terminal view (an entity owning a `TermState`) and its element (the
//!   painter). Rows are painted straight from the grid; no intermediate widget tree.
//! * [`a11y`] — the keyboard ring and, for tests, the accessibility tree.
//! * [`canvas`] — the infinite plane: items, camera, drag/zoom, actions.
//! * [`screen`] — a remote window or display painted from decoded frames, with input forwarding.
//! * [`note`] — a sticky note edited in place, text shared through the document.
//! * [`picker`] — the "add a window" chooser.
//! * [`keys`] — GPUI keystrokes → protocol key events.
//! * [`colors`] — theme tokens → GPUI colours.
//! * [`kit`] — gpui-kit's theme kept on the same tokens.
//! * [`frames`] — the UI frame-time probe (draw percentiles, cadence, drops).
//! * [`fonts`] — bundled `JetBrains Mono` + Nerd symbols, registered at startup.

pub mod a11y;
pub mod canvas;
pub mod colors;
pub mod fonts;
pub mod frames;
pub mod keys;
pub mod kit;
pub mod note;
pub mod picker;
pub mod screen;
pub mod terminal;
