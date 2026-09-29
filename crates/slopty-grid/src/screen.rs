//! The visible grid plus cursor, and the row-level update protocol applied to it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Line, MAX_COLS, TermModes};

/// The tallest screen there is: the engine makes no terminal taller, and a frame claiming more
/// rows does not decode. A full-screen window at the smallest font on a portrait 6K display
/// is about 350 rows.
pub const MAX_ROWS: u16 = 1024;

/// Cursor shape as requested by DECSCUSR or the engine default.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum CursorShape {
    /// Filled block.
    #[default]
    Block,
    /// Vertical bar at the left edge.
    Bar,
    /// Underline.
    Underline,
    /// Outlined block (unfocused rendering of a block).
    BlockHollow,
}

/// Cursor state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub struct Cursor {
    /// Row within the visible screen.
    pub row: u16,
    /// Column.
    pub col: u16,
    /// Shape.
    pub shape: CursorShape,
    /// Whether the cursor is shown (DEC 25).
    pub visible: bool,
    /// Whether the cursor blinks.
    pub blink: bool,
}

/// One row replaced wholesale. The engine's dirty granularity is the row, and a row is small
/// enough that diffing inside it is not worth the protocol complexity.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RowUpdate {
    /// Row index within the visible screen.
    pub row: u16,
    /// The new contents, shared: the worker keeps the same allocation as the row its viewers
    /// hold, and a client puts it on the screen and in the scrollback as it arrived. Serde
    /// writes the line itself, so the wire is what a plain `Line` made.
    pub line: Arc<Line>,
}

/// Errors from applying updates.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScreenError {
    /// A row index beyond the screen height.
    #[error("row {row} out of range for a screen with {rows} rows")]
    RowOutOfRange {
        /// Offending row.
        row: u16,
        /// Screen height.
        rows: u16,
    },
    /// A line whose width does not match the screen.
    #[error("line has {got} cells, screen has {cols} columns")]
    WidthMismatch {
        /// Offending width.
        got: u16,
        /// Screen width.
        cols: u16,
    },
}

/// The visible grid: exactly `rows` lines of `cols` cells, a cursor, and mode bits. A size
/// past [`MAX_COLS`] × [`MAX_ROWS`] is clamped to it, whoever asked.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Screen {
    cols: u16,
    rows: u16,
    /// Shared with [`crate::Scrollback`]: applying a row puts the same allocation in both, so a
    /// line that scrolls into history is never copied.
    lines: Vec<Arc<Line>>,
    cursor: Cursor,
    modes: TermModes,
}

impl Screen {
    /// A blank screen.
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let (cols, rows) = (cols.min(MAX_COLS), rows.min(MAX_ROWS));
        Self {
            cols,
            rows,
            lines: std::iter::repeat_with(|| Arc::new(Line::blank(cols)))
                .take(usize::from(rows))
                .collect(),
            cursor: Cursor { visible: true, ..Cursor::default() },
            modes: TermModes::empty(),
        }
    }

    /// Width in columns.
    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    /// Cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Cursor, mutable.
    pub const fn cursor_mut(&mut self) -> &mut Cursor {
        &mut self.cursor
    }

    /// Modes.
    #[must_use]
    pub const fn modes(&self) -> TermModes {
        self.modes
    }

    /// Set the modes.
    pub const fn set_modes(&mut self, modes: TermModes) {
        self.modes = modes;
    }

    /// Height in rows.
    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// All lines, top to bottom.
    #[must_use]
    pub fn lines(&self) -> &[Arc<Line>] {
        &self.lines
    }

    /// One line.
    #[must_use]
    pub fn line(&self, row: u16) -> Option<&Line> {
        self.lines.get(usize::from(row)).map(AsRef::as_ref)
    }

    /// Resize, keeping the top-left content. Real reflow is the engine's job; this keeps the client
    /// consistent until the next full frame arrives.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.min(MAX_COLS), rows.min(MAX_ROWS));
        self.cols = cols;
        self.rows = rows;
        self.lines.resize_with(usize::from(rows), || Arc::new(Line::blank(cols)));
        for line in &mut self.lines {
            // `make_mut` copies only a row the scrollback also holds, and only on a resize.
            Arc::make_mut(line).resize(cols);
        }
        self.cursor.row = self.cursor.row.min(rows.saturating_sub(1));
        self.cursor.col = self.cursor.col.min(cols.saturating_sub(1));
    }

    /// Replace one row with the update's line, kept as it came (the caller may share it with
    /// the scrollback).
    ///
    /// # Errors
    ///
    /// A line whose width is not the screen's, or a row past the bottom.
    pub fn apply(&mut self, update: RowUpdate) -> Result<(), ScreenError> {
        let RowUpdate { row, line } = update;
        let got = line.cols();
        if got != self.cols {
            return Err(ScreenError::WidthMismatch { got, cols: self.cols });
        }
        let slot = self
            .lines
            .get_mut(usize::from(row))
            .ok_or(ScreenError::RowOutOfRange { row, rows: self.rows })?;
        *slot = line;
        Ok(())
    }

    /// Replace every row (a full frame).
    pub fn replace_all(&mut self, lines: Vec<Line>) -> Result<(), ScreenError> {
        let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        if rows != self.rows {
            return Err(ScreenError::RowOutOfRange { row: rows, rows: self.rows });
        }
        if let Some(bad) = lines.iter().find(|l| l.cols() != self.cols) {
            return Err(ScreenError::WidthMismatch { got: bad.cols(), cols: self.cols });
        }
        self.lines = lines.into_iter().map(Arc::new).collect();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;

    #[test]
    fn apply_rejects_bad_geometry() {
        let mut s = Screen::new(10, 3);
        let row_err = s.apply(RowUpdate { row: 3, line: Arc::new(Line::blank(10)) }).unwrap_err();
        assert_eq!(row_err, ScreenError::RowOutOfRange { row: 3, rows: 3 });
        let width_err = s.apply(RowUpdate { row: 0, line: Arc::new(Line::blank(9)) }).unwrap_err();
        assert_eq!(width_err, ScreenError::WidthMismatch { got: 9, cols: 10 });
    }

    #[test]
    fn geometry_modes_and_full_replacement_are_read_back() {
        let mut s = Screen::new(7, 2);
        assert_eq!((s.cols(), s.rows()), (7, 2));
        s.set_modes(TermModes::ALT_SCREEN | TermModes::BRACKETED_PASTE);
        assert_eq!(s.modes(), TermModes::ALT_SCREEN | TermModes::BRACKETED_PASTE);

        let row = Arc::new(Line::from_text("shared", 7, Style::DEFAULT));
        s.apply(RowUpdate { row: 1, line: Arc::clone(&row) }).unwrap();
        assert!(Arc::ptr_eq(&s.lines()[1], &row), "the same allocation is kept");
        assert_eq!(
            s.apply(RowUpdate { row: 0, line: Arc::new(Line::blank(6)) }).unwrap_err(),
            ScreenError::WidthMismatch { got: 6, cols: 7 }
        );
        assert_eq!(
            s.apply(RowUpdate { row: 2, line: Arc::new(Line::blank(7)) }).unwrap_err(),
            ScreenError::RowOutOfRange { row: 2, rows: 2 }
        );

        assert_eq!(
            s.replace_all(vec![Line::blank(7)]).unwrap_err(),
            ScreenError::RowOutOfRange { row: 1, rows: 2 }
        );
        assert_eq!(
            s.replace_all(vec![Line::blank(7), Line::blank(8)]).unwrap_err(),
            ScreenError::WidthMismatch { got: 8, cols: 7 }
        );
        assert_eq!(s.line(1).unwrap().text(), "shared", "a rejected frame changes nothing");
        s.replace_all(vec![
            Line::from_text("top", 7, Style::DEFAULT),
            Line::from_text("bottom", 7, Style::DEFAULT),
        ])
        .unwrap();
        assert_eq!(s.line(0).unwrap().text(), "top");
        assert_eq!(s.line(1).unwrap().text(), "bottom");
    }

    #[test]
    fn resize_clamps_cursor() {
        let mut s = Screen::new(80, 24);
        *s.cursor_mut() = Cursor { row: 23, col: 79, ..Cursor::default() };
        s.resize(40, 10);
        assert_eq!((s.cursor().row, s.cursor().col), (9, 39));
        assert_eq!(s.lines().len(), 10);
        assert!(s.lines().iter().all(|l| l.cols() == 40));
    }

    /// A size claimed past the ceiling is the ceiling: seven bytes of `Resized` once asked a
    /// client for 4 G cells.
    #[test]
    fn a_screen_is_never_past_the_ceiling() {
        let mut s = Screen::new(u16::MAX, 1);
        assert_eq!((s.cols(), s.rows()), (MAX_COLS, 1));
        assert_eq!(s.lines()[0].cols(), MAX_COLS);
        s.resize(2, u16::MAX);
        assert_eq!((s.cols(), s.rows()), (2, MAX_ROWS));
        assert_eq!(s.lines().len(), usize::from(MAX_ROWS));
        s.resize(MAX_COLS + 1, 1);
        assert_eq!((s.cols(), s.rows()), (MAX_COLS, 1));
    }
}
