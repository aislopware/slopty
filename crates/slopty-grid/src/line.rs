//! A row of cells.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::{Cell, CellWidth, Style};

/// OSC 133 semantic prompt marks, so the client can navigate prompts and select command output.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum SemanticMark {
    /// Not marked.
    #[default]
    Unknown,
    /// Start of a shell prompt (`OSC 133;A`).
    Prompt,
    /// Start of user input (`OSC 133;B`).
    Input,
    /// Start of command output (`OSC 133;C`).
    Output,
    /// Continuation of a multi-line prompt.
    PromptContinuation,
}

bitflags! {
    /// Per-line flags.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct LineFlags: u8 {
        /// This line is a soft-wrapped continuation of the previous one (no newline between).
        const WRAPPED = 1 << 0;
        /// The line is "dirty" from the engine's point of view (only meaningful on the host).
        const DIRTY = 1 << 1;
    }
}

/// An OSC 8 hyperlink over a run of cells in one line.
///
/// Links travel as runs rather than as an id in every cell: a link-free row then costs one byte
/// (the empty run list) instead of one byte per cell (see `docs/MEASUREMENTS.md`, 2026-09-05).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Hyperlink {
    /// First cell of the run.
    pub col: u16,
    /// Cells covered (never zero).
    pub len: u16,
    /// The link target as the program gave it.
    pub uri: String,
}

impl Hyperlink {
    /// True when `col` is inside the run.
    #[must_use]
    pub const fn covers(&self, col: u16) -> bool {
        col >= self.col && col < self.col.saturating_add(self.len)
    }

    /// One past the last cell of the run.
    #[must_use]
    pub const fn end(&self) -> u16 {
        self.col.saturating_add(self.len)
    }
}

/// One row of the grid. Always exactly `cols` cells long once placed in a [`crate::Screen`].
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Line {
    /// The cells, left to right.
    pub cells: Vec<Cell>,
    /// Wrap and dirty state.
    pub flags: LineFlags,
    /// Shell integration mark at the start of this line.
    pub mark: SemanticMark,
    /// OSC 8 links on this line, left to right, non-overlapping.
    pub links: Vec<Hyperlink>,
}

impl Line {
    /// A blank line of `cols` cells.
    #[must_use]
    pub fn blank(cols: u16) -> Self {
        Self {
            cells: vec![Cell::BLANK; usize::from(cols)],
            flags: LineFlags::empty(),
            mark: SemanticMark::Unknown,
            links: Vec::new(),
        }
    }

    /// Build a line from plain text, one narrow cell per scalar, padded or truncated to `cols`.
    /// Test and placeholder helper: real lines come from the engine with proper widths.
    #[must_use]
    pub fn from_text(text: &str, cols: u16, style: Style) -> Self {
        let mut cells: Vec<Cell> =
            text.chars().take(usize::from(cols)).map(|c| Cell::narrow(c, style)).collect();
        cells.resize(usize::from(cols), Cell::BLANK);
        Self { cells, flags: LineFlags::empty(), mark: SemanticMark::Unknown, links: Vec::new() }
    }

    /// The OSC 8 link covering `col`, if any.
    #[must_use]
    pub fn link_at(&self, col: u16) -> Option<&Hyperlink> {
        self.links.iter().find(|l| l.covers(col))
    }

    /// Width in columns.
    #[must_use]
    pub fn cols(&self) -> u16 {
        u16::try_from(self.cells.len()).unwrap_or(u16::MAX)
    }

    /// Pad or truncate to `cols`, keeping a wide character from being cut in half.
    pub fn resize(&mut self, cols: u16) {
        let cols = usize::from(cols);
        if cols < self.cells.len() {
            self.cells.truncate(cols);
            if let Some(last) = self.cells.last_mut()
                && last.width == CellWidth::Wide
            {
                *last = Cell::BLANK;
            }
            let cols = u16::try_from(cols).unwrap_or(u16::MAX);
            self.links.retain_mut(|l| {
                l.len = l.len.min(cols.saturating_sub(l.col));
                l.len > 0
            });
        } else {
            self.cells.resize(cols, Cell::BLANK);
        }
    }

    /// Index of the last cell that is not blank, or `None` for an all-blank line.
    #[must_use]
    pub fn last_content_col(&self) -> Option<u16> {
        self.cells.iter().rposition(|c| !c.is_blank()).and_then(|i| u16::try_from(i).ok())
    }

    /// The visible text of the line with trailing blanks trimmed, for search, copy and summaries.
    #[must_use]
    pub fn text(&self) -> String {
        let end = self.last_content_col().map_or(0, |c| usize::from(c).saturating_add(1));
        let mut out = String::new();
        for cell in self.cells.iter().take(end) {
            if !cell.width.draws_text() {
                continue;
            }
            if cell.text.is_empty() {
                out.push(' ');
            } else {
                out.push_str(cell.text.as_str());
            }
        }
        out
    }

    /// True when every cell is blank.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.cells.iter().all(Cell::is_blank)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_trims_trailing_blanks_and_skips_spacers() {
        let mut line = Line::from_text("ab", 6, Style::DEFAULT);
        line.cells[2] = Cell::wide("字", Style::DEFAULT);
        line.cells[3] = Cell::spacer_tail(Style::DEFAULT);
        assert_eq!(line.text(), "ab字");
        assert_eq!(line.last_content_col(), Some(3), "the spacer tail is not blank");
    }

    #[test]
    fn resize_never_leaves_a_dangling_wide_head() {
        let mut line = Line::blank(4);
        line.cells[2] = Cell::wide("字", Style::DEFAULT);
        line.cells[3] = Cell::spacer_tail(Style::DEFAULT);
        line.resize(3);
        assert_eq!(line.cells.len(), 3);
        assert!(line.cells[2].is_blank(), "a wide head with no room for its tail becomes blank");
        line.resize(8);
        assert_eq!(line.cells.len(), 8);
    }

    #[test]
    fn links_are_found_by_column_and_clipped_on_resize() {
        let mut line = Line::from_text("see https://a.b now", 20, Style::DEFAULT);
        line.links.push(Hyperlink { col: 4, len: 11, uri: "https://a.b/".to_owned() });
        assert_eq!(line.link_at(3), None);
        assert_eq!(line.link_at(4).map(|l| l.uri.as_str()), Some("https://a.b/"));
        assert_eq!(line.link_at(14).map(Hyperlink::end), Some(15));
        assert_eq!(line.link_at(15), None);
        line.resize(10);
        assert_eq!(line.links[0].len, 6, "clipped to the new width");
        line.resize(4);
        assert!(line.links.is_empty(), "a run past the edge is gone");
    }

    #[test]
    fn interior_spaces_are_kept() {
        let line = Line::from_text("a b", 5, Style::DEFAULT);
        assert_eq!(line.text(), "a b");
    }
}
