//! The terminal frame model.
//!
//! The host's VT engine produces [`Screen`] snapshots and per-row updates; the client stores them,
//! keeps scrollback in a [`Scrollback`] cache keyed by absolute line index, and renders from
//! [`Line`]s. Nothing here parses escape sequences or touches a font: this crate is pure data and
//! the operations on it (apply a row update, scroll a viewport, look up a cell).
//!
//! Widths and grapheme boundaries are decided by the engine on the host and carried explicitly in
//! every [`Cell`], so a client never re-segments text and can never disagree with the host about
//! where a column starts.

#![forbid(unsafe_code)]

mod cell;
mod line;
mod modes;
mod screen;
mod scrollback;
mod style;

pub use cell::{Cell, CellText, CellWidth, HyperlinkId};
pub use line::{Line, LineFlags, SemanticMark};
pub use modes::TermModes;
pub use screen::{Cursor, CursorShape, RowUpdate, Screen, ScreenError};
pub use scrollback::{LineIndex, Scrollback, ScrollbackStats};
pub use style::{Color, Style, StyleFlags, Underline};
