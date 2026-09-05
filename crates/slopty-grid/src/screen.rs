//! The visible grid plus cursor, and the row-level update protocol applied to it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{Line, TermModes};

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
    /// The new contents.
    pub line: Line,
}

/// Errors from applying updates.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
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

/// The visible grid: exactly `rows` lines of `cols` cells, a cursor, and mode bits.
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
        Self {
            cols,
            rows,
            lines: (0..rows).map(|_| Arc::new(Line::blank(cols))).collect(),
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

    /// Replace one row.
    pub fn apply(&mut self, update: RowUpdate) -> Result<(), ScreenError> {
        let got = update.line.cols();
        if got != self.cols {
            return Err(ScreenError::WidthMismatch { got, cols: self.cols });
        }
        let slot = self
            .lines
            .get_mut(usize::from(update.row))
            .ok_or(ScreenError::RowOutOfRange { row: update.row, rows: self.rows })?;
        *slot = Arc::new(update.line);
        Ok(())
    }

    /// [`Self::apply`] for a row the caller already shares with the scrollback.
    ///
    /// # Errors
    ///
    /// As [`Self::apply`]: a width mismatch or a row past the bottom.
    pub fn apply_shared(&mut self, row: u16, line: Arc<Line>) -> Result<(), ScreenError> {
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

    /// Scroll the visible content up by `n` rows (content moves up, blank rows enter at the
    /// bottom) and return the lines that left the top. Used by clients that maintain a local
    /// scrollback from row updates, and by the prediction engine when it speculates a newline.
    pub fn scroll_up(&mut self, n: u16) -> Vec<Arc<Line>> {
        let n = usize::from(n.min(self.rows));
        let evicted: Vec<Arc<Line>> = self.lines.drain(..n).collect();
        self.lines.extend((0..n).map(|_| Arc::new(Line::blank(self.cols))));
        evicted
    }

    /// Rows whose content differs between `self` and `other` (same size assumed; a size mismatch
    /// reports every row).
    #[must_use]
    pub fn changed_rows(&self, other: &Self) -> Vec<u16> {
        if self.cols != other.cols || self.rows != other.rows {
            return (0..self.rows).collect();
        }
        self.lines
            .iter()
            .zip(&other.lines)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .filter_map(|(i, _)| u16::try_from(i).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;

    #[test]
    fn apply_rejects_bad_geometry() {
        let mut s = Screen::new(10, 3);
        let row_err = s.apply(RowUpdate { row: 3, line: Line::blank(10) }).unwrap_err();
        assert_eq!(row_err, ScreenError::RowOutOfRange { row: 3, rows: 3 });
        let width_err = s.apply(RowUpdate { row: 0, line: Line::blank(9) }).unwrap_err();
        assert_eq!(width_err, ScreenError::WidthMismatch { got: 9, cols: 10 });
    }

    #[test]
    fn scroll_up_evicts_top_rows_in_order() {
        let mut s = Screen::new(5, 3);
        for (i, t) in ["one", "two", "three"].iter().enumerate() {
            s.apply(RowUpdate {
                row: u16::try_from(i).unwrap(),
                line: Line::from_text(t, 5, Style::DEFAULT),
            })
            .unwrap();
        }
        let gone = s.scroll_up(2);
        assert_eq!(gone.iter().map(|l| l.text()).collect::<Vec<_>>(), ["one", "two"]);
        assert_eq!(s.line(0).unwrap().text(), "three");
        assert!(s.line(1).unwrap().is_blank());
        assert!(s.line(2).unwrap().is_blank());
        assert_eq!(s.scroll_up(99).len(), 3, "clamped to the screen height");
    }

    #[test]
    fn changed_rows_reports_only_differences() {
        let a = Screen::new(5, 2);
        let mut b = a.clone();
        b.apply(RowUpdate { row: 1, line: Line::from_text("x", 5, Style::DEFAULT) }).unwrap();
        assert_eq!(a.changed_rows(&b), vec![1]);
        let c = Screen::new(6, 2);
        assert_eq!(a.changed_rows(&c), vec![0, 1]);
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
}
