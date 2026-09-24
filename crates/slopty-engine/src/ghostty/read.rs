//! Text views of the terminal for the orchestration verbs: the screen, retained lines by
//! absolute index, and the OSC 133 command blocks.
//!
//! All of them read libghostty's grid, never PTY bytes, and number lines the way the frames
//! do (see the crate docs). Text comes from libghostty's plain-text formatter, the one search
//! uses: one line per row, trailing blanks trimmed. When search already formatted the whole
//! history at this generation that text is sliced; otherwise only the rows asked for are
//! formatted, so a read near the end of a 50 000-line history costs its own rows, not the
//! history's ten milliseconds.

use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{Point, PointCoordinate};
use slopty_grid::{LineFlags, SemanticMark};

use super::GhosttyEngine;
use crate::{EngineError, osc133};

/// Rows above its output a command line is read from at most: a pasted script is not re-read
/// whole for its first line.
const COMMAND_ROWS: u64 = 16;

/// Rows one [`GhosttyEngine::text_since`] formats at most, so a waiter behind a flood catches
/// up in bounded steps instead of formatting the whole flood on the session's thread at once.
const SINCE_ROWS: u64 = 4096;

/// A command as its marks recorded it: the prompt (`133;A`), the output's first row (`133;C`),
/// and its end (`133;D`) with the cursor where the mark was written.
#[derive(Clone, Copy, Debug)]
pub(super) struct Block {
    prompt: u64,
    output: Option<u64>,
    end: Option<End>,
}

#[derive(Clone, Copy, Debug)]
struct End {
    line: u64,
    col: u16,
    exit: Option<u8>,
}

/// The screen as text.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScreenText {
    /// Absolute index of the top row.
    pub first: u64,
    /// Every row, top to bottom, trailing blanks trimmed.
    pub rows: Vec<String>,
    /// Cursor row (0 = top) and column.
    pub cursor: (u16, u16),
    /// The alternate screen is up.
    pub alternate: bool,
}

/// Retained lines from an absolute index on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextLines {
    /// Absolute index of the first line.
    pub first: u64,
    /// The lines, trailing blanks trimmed.
    pub lines: Vec<String>,
}

impl TextLines {
    /// One past the last line: where the next read starts.
    #[must_use]
    pub const fn next(&self) -> u64 {
        self.first.saturating_add(self.lines.len() as u64)
    }
}

/// A place in the terminal's text: an absolute line, a column (a cell) on it, and the
/// numbering the line is counted in (the frames' epoch).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    /// Absolute line.
    pub line: u64,
    /// Cell column.
    pub col: u16,
    /// Line numbering it belongs to.
    pub epoch: u32,
}

/// Text written from a [`Position`] on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TextSince {
    /// Rows that hold text, with their absolute index; the first is cut at the position's
    /// column.
    pub lines: Vec<(u64, String)>,
    /// Where to read from next: the cursor's row, whose text may still grow, or the first row
    /// not formatted yet when there were more than one read takes.
    pub next: Position,
    /// Rows between `next` and the cursor are written but not read yet: read again at once.
    pub behind: bool,
}

/// One command block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CommandBlock {
    /// What was typed at the prompt.
    pub command: String,
    /// Absolute line of its prompt.
    pub prompt_line: u64,
    /// Absolute lines of its output, `[start, end)`.
    pub output: (u64, u64),
    /// Its `133;D` arrived.
    pub finished: bool,
    /// The status `133;D` carried.
    pub exit: Option<u8>,
}

impl GhosttyEngine {
    /// Fold one OSC 133 mark into the command blocks. A prompt that ran nothing (an empty
    /// line, ⌃C, a redraw) is dropped when the next one starts; only a block with output is a
    /// command, and only its `D` counts as a command ending.
    pub(super) fn note_command_mark(&mut self, line: u64, col: u16, mark: osc133::Mark) {
        let base = self.base;
        while self.commands.front().is_some_and(|b| b.prompt < base) {
            self.commands.pop_front();
        }
        match mark {
            osc133::Mark::PromptStart => {
                if self.commands.back().is_some_and(|b| b.output.is_none()) {
                    self.commands.pop_back();
                }
                self.commands.push_back(Block { prompt: line, output: None, end: None });
            }
            osc133::Mark::OutputStart => {
                if let Some(block) = self.commands.back_mut()
                    && block.output.is_none()
                {
                    block.output = Some(line.max(block.prompt));
                }
            }
            osc133::Mark::CommandEnd { exit } => {
                if let Some(block) = self.commands.back_mut()
                    && block.output.is_some()
                    && block.end.is_none()
                {
                    block.end = Some(End { line, col, exit });
                    self.commands_ended = self.commands_ended.wrapping_add(1);
                }
            }
        }
    }

    /// Screen rows `first_y..=last_y` (screen space: history then screen) as plain text, one
    /// line per row with trailing blanks trimmed; blank rows at the end are omitted.
    pub(super) fn plain_rows(&self, first_y: u32, last_y: u32) -> Result<String, EngineError> {
        let last_col = self.size.cols.saturating_sub(1);
        let start = self.term.grid_ref(Point::Screen(PointCoordinate { x: 0, y: first_y }))?;
        let end = self.term.grid_ref(Point::Screen(PointCoordinate { x: last_col, y: last_y }))?;
        let selection = Selection::new(start, end, false);
        let options = FormatterOptions::new()
            .with_format(Format::Plain)
            .with_unwrap(false)
            .with_trim(true)
            .with_selection(&selection);
        let mut formatter = Formatter::new(&self.term, options)?;
        match formatter.format_alloc(None) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            // Blank rows format to nothing, which the allocating call reports as out of memory
            // (no buffer came back); the caller's-buffer call tells the two apart.
            Err(libghostty_vt::Error::OutOfMemory)
                if formatter.format_buf(&mut []).is_ok_and(|n| n == 0) =>
            {
                Ok(String::new())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Rows `[first, end)` by absolute index, one string per row, blank rows included. Slices
    /// search's text when it is of this generation, else formats just these rows.
    fn rows_text(&self, first: u64, end: u64) -> Result<Vec<String>, EngineError> {
        let first = first.max(self.base);
        if end <= first {
            return Ok(Vec::new());
        }
        let count = usize::try_from(end.saturating_sub(first)).unwrap_or(usize::MAX);
        let current = self.search_text.as_ref().filter(|(at, _)| *at == self.generation);
        let mut rows: Vec<String> = if let Some((_, text)) = current {
            let skip = usize::try_from(first.saturating_sub(self.base)).unwrap_or(usize::MAX);
            text.split('\n').skip(skip).take(count).map(str::to_owned).collect()
        } else {
            let y = |abs: u64| u32::try_from(abs.saturating_sub(self.base)).unwrap_or(u32::MAX);
            let text = self.plain_rows(y(first), y(end.saturating_sub(1)))?;
            text.split('\n').take(count).map(str::to_owned).collect()
        };
        rows.resize(count, String::new());
        Ok(rows)
    }

    /// Where the cursor is, as a [`Position`].
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn position(&self) -> Result<Position, EngineError> {
        let scrollback = self.term.scrollback_rows()? as u64;
        let line =
            self.base.saturating_add(scrollback).saturating_add(u64::from(self.term.cursor_y()?));
        Ok(Position { line, col: self.term.cursor_x()?, epoch: self.epoch })
    }

    /// The screen as text, with the absolute index of its top row.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn screen_text(&self) -> Result<ScreenText, EngineError> {
        let total = self.total_lines()?;
        let first = total.saturating_sub(u64::from(self.size.rows)).max(self.base);
        Ok(ScreenText {
            first,
            rows: self.rows_text(first, total)?,
            cursor: (self.term.cursor_y()?, self.term.cursor_x()?),
            alternate: self.on_alt,
        })
    }

    /// At most `max` retained lines from absolute line `since` on (the oldest retained when
    /// `None`, and never before it). Blank rows at the end of the terminal are not lines yet:
    /// a read that reaches the end stops at the last row with text, so asking again from
    /// [`TextLines::next`] picks up whatever is written below it.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn text_lines(&self, since: Option<u64>, max: u32) -> Result<TextLines, EngineError> {
        let total = self.total_lines()?;
        let first = since.unwrap_or(self.base).clamp(self.base, total);
        let end = first.saturating_add(u64::from(max)).min(total);
        let mut lines = self.rows_text(first, end)?;
        if end == total {
            while lines.last().is_some_and(String::is_empty) {
                lines.pop();
            }
        }
        Ok(TextLines { first, lines })
    }

    /// Text written from `from` on: the rest of its row from its column, then every row below
    /// that holds text, 4096 rows at most. When the numbering changed since `from` was
    /// taken (a resize, the alternate screen), the whole screen counts as written since.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn text_since(&self, from: Position) -> Result<TextSince, EngineError> {
        let total = self.total_lines()?;
        let now = self.position()?;
        let from = if from.epoch == self.epoch {
            from
        } else {
            let top = total.saturating_sub(u64::from(self.size.rows));
            Position { line: top, col: 0, epoch: self.epoch }
        };
        let first = from.line.max(self.base);
        let col = if first == from.line { from.col } else { 0 };
        let end = total.min(first.saturating_add(SINCE_ROWS));
        let mut lines = Vec::new();
        for (index, text) in (first..end).zip(self.rows_text(first, end)?) {
            let text = if index == first && col > 0 {
                let y = u32::try_from(index.saturating_sub(self.base)).unwrap_or(u32::MAX);
                self.read_line(y, self.size.cols)?.text_from(col)
            } else {
                text
            };
            if !text.is_empty() {
                lines.push((index, text));
            }
        }
        // Rows above the cursor are written; its own row may still grow.
        let resume = now.line.min(end);
        let next = if resume > first {
            Position { line: resume, col: 0, epoch: self.epoch }
        } else {
            Position { line: first, col, epoch: self.epoch }
        };
        Ok(TextSince { lines, next, behind: end < now.line })
    }

    /// Where the first command that ended at or after `from` ended (the cursor at its `133;D`),
    /// if one has. Only commands in `from`'s line numbering count.
    #[must_use]
    pub fn ended_after(&self, from: Position) -> Option<Position> {
        if from.epoch != self.epoch {
            return None;
        }
        self.commands.iter().filter_map(|b| b.end).find_map(|end| {
            ((end.line, end.col) >= (from.line, from.col)).then_some(Position {
                line: end.line,
                col: end.col,
                epoch: self.epoch,
            })
        })
    }

    /// Commands ended (`133;D` after a command's output started) since the engine started;
    /// a waiter compares two readings.
    #[must_use]
    pub const fn commands_ended(&self) -> u64 {
        self.commands_ended
    }

    /// The command blocks whose prompt is at or after absolute line `since`, oldest first: the
    /// finished ones with their status and the one running now. Output runs from the `133;C`
    /// row to the row the `133;D` was written on, that row included when the output left
    /// text on it (no final newline); a running command's runs to the cursor.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn commands(&self, since: Option<u64>) -> Result<Vec<CommandBlock>, EngineError> {
        let now = self.position()?;
        let total = self.total_lines()?;
        let from = since.unwrap_or(0).max(self.base);
        let mut out = Vec::new();
        for block in &self.commands {
            let Some(output) = block.output else { continue };
            if block.prompt < from {
                continue;
            }
            let (end, finished, exit) = match block.end {
                Some(end) => (end.line.saturating_add(u64::from(end.col > 0)), true, end.exit),
                None => (now.line.saturating_add(u64::from(now.col > 0)), false, None),
            };
            out.push(CommandBlock {
                command: self.command_line(block.prompt, output)?,
                prompt_line: block.prompt,
                output: (output, end.clamp(output, total.max(output))),
                finished,
                exit,
            });
        }
        Ok(out)
    }

    /// What was typed between a prompt and its output: the cells libghostty marked as input
    /// (written after `133;B`), soft-wrapped rows joined and continuation rows on lines of
    /// their own. A shell that never marks its input gets the last prompt row whole.
    fn command_line(&self, prompt: u64, output: u64) -> Result<String, EngineError> {
        let cols = self.size.cols;
        let mut typed = String::new();
        let mut last_row = String::new();
        let end = output.min(prompt.saturating_add(COMMAND_ROWS));
        for abs in prompt.max(self.base)..end {
            let y = u32::try_from(abs.saturating_sub(self.base)).unwrap_or(u32::MAX);
            let line = self.read_line(y, cols)?;
            let input = match line.mark {
                SemanticMark::Prompt { input: Some(col), .. }
                | SemanticMark::PromptContinuation { input: Some(col) } => {
                    Some(line.text_from(col))
                }
                SemanticMark::Input => Some(line.text()),
                _ => None,
            };
            if let Some(input) = input {
                if !typed.is_empty() && !line.flags.contains(LineFlags::WRAPPED) {
                    typed.push('\n');
                }
                typed.push_str(&input);
            }
            last_row = line.text();
        }
        let command = if typed.trim().is_empty() { last_row } else { typed };
        Ok(command.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::TermSize;

    use super::*;
    use crate::EngineConfig;

    fn engine(cols: u16, rows: u16) -> GhosttyEngine {
        GhosttyEngine::new(EngineConfig {
            size: TermSize { cols, rows, metrics: CellMetrics { cell_width: 8, cell_height: 16 } },
            scrollback_lines: 1000,
        })
        .unwrap()
    }

    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

    /// A shell session as the integration scripts write it: prompt, typed command, `C`,
    /// output, `D` with the status.
    fn run(e: &mut GhosttyEngine, command: &str, output: &str, exit: u8) {
        e.write(PROMPT);
        e.write(command.as_bytes());
        e.write(b"\r\n\x1b]133;C\x07");
        e.write(output.as_bytes());
        e.write(format!("\x1b]133;D;{exit}\x07").as_bytes());
    }

    /// A terminal nothing was written to formats to no text at all, which is not an error.
    #[test]
    fn a_blank_terminal_reads_as_blank_rows() {
        let mut e = engine(20, 3);
        let screen = e.screen_text().unwrap();
        assert_eq!(screen.rows, ["", "", ""]);
        assert!(e.text_lines(None, 10).unwrap().lines.is_empty());
        assert_eq!(e.search("x", false, 10).unwrap(), crate::search::Found::default());
    }

    #[test]
    fn the_screen_reads_as_rows_with_absolute_indices_and_the_cursor() {
        let mut e = engine(20, 4);
        e.write(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let screen = e.screen_text().unwrap();
        assert_eq!(screen.first, 1, "one scrolled into history");
        assert_eq!(screen.rows, ["two", "three", "four", "five"]);
        assert_eq!(screen.cursor, (3, 4));
        assert!(!screen.alternate);
        e.write(b"\x1b[?1049h\x1b[Hfull screen");
        let screen = e.screen_text().unwrap();
        assert!(screen.alternate);
        assert_eq!(screen.rows, ["full screen", "", "", ""]);
    }

    #[test]
    fn output_reads_from_an_index_and_stops_at_the_last_written_row() {
        let mut e = engine(20, 4);
        e.write(b"a\r\nb\r\nc\r\nd\r\ne\r\n");
        let all = e.text_lines(None, 100).unwrap();
        assert_eq!(all.first, 0);
        assert_eq!(all.lines, ["a", "b", "c", "d", "e"]);
        assert_eq!(all.next(), 5, "the empty cursor row is not a line yet");
        let page = e.text_lines(Some(1), 2).unwrap();
        assert_eq!((page.first, page.next()), (1, 3));
        assert_eq!(page.lines, ["b", "c"]);
        e.write(b"f\r\n");
        let more = e.text_lines(Some(all.next()), 100).unwrap();
        assert_eq!((more.first, more.lines), (5, vec!["f".to_owned()]));
        let past = e.text_lines(Some(1_000), 10).unwrap();
        assert!(past.lines.is_empty());
    }

    /// A long line soft-wraps onto two rows and reads as two lines: one per row, numbered as
    /// the frames number them.
    #[test]
    fn a_wrapped_line_is_one_line_per_row() {
        let mut e = engine(10, 4);
        e.write(b"0123456789abcdef\r\nnext");
        let lines = e.text_lines(None, 10).unwrap();
        assert_eq!(lines.lines, ["0123456789", "abcdef", "next"]);
    }

    /// Search's whole-history text, when current, is what a read slices; the rows come out the
    /// same either way.
    #[test]
    fn reads_agree_with_and_without_the_search_text() {
        let mut e = engine(30, 5);
        for i in 0..40 {
            e.write(format!("row {i}\r\n").as_bytes());
        }
        let formatted = e.text_lines(Some(10), 5).unwrap();
        let _found = e.search("row", false, 10).unwrap();
        assert!(e.search_text.is_some());
        let sliced = e.text_lines(Some(10), 5).unwrap();
        assert_eq!(formatted, sliced);
        assert_eq!(sliced.lines[0], "row 10");
    }

    #[test]
    fn text_since_a_position_skips_what_was_there_and_follows_the_cursor() {
        let mut e = engine(30, 5);
        e.write(b"old line\r\n$ ");
        let from = e.position().unwrap();
        assert_eq!((from.line, from.col), (1, 2));
        e.write(b"echo hi\r\nhi\r\n$ ");
        let since = e.text_since(from).unwrap();
        assert_eq!(
            since.lines,
            [(1, "echo hi".to_owned()), (2, "hi".to_owned()), (3, "$".to_owned())]
        );
        assert_eq!((since.next.line, since.next.col), (3, 0), "the cursor row may still grow");
        e.write(b"x");
        let again = e.text_since(since.next).unwrap();
        assert_eq!(again.lines, [(3, "$ x".to_owned())]);
    }

    #[test]
    fn a_numbering_change_makes_the_whole_screen_new() {
        let mut e = engine(30, 4);
        e.write(b"before\r\n");
        let from = e.position().unwrap();
        e.write(b"\x1b[?1049h\x1b[Htui text");
        let since = e.text_since(from).unwrap();
        assert_eq!(since.lines, [(0, "tui text".to_owned())]);
    }

    #[test]
    fn command_blocks_carry_the_line_the_output_and_the_status() {
        let mut e = engine(40, 10);
        run(&mut e, "echo hi", "hi\r\n", 0);
        // An empty Enter: a prompt that ran nothing is not a command.
        e.write(PROMPT);
        e.write(b"\r\n");
        run(&mut e, "false", "", 1);
        run(&mut e, "printf x", "x", 0);
        e.write(PROMPT);
        e.write(b"sleep 100\r\n\x1b]133;C\x07");
        let blocks = e.commands(None).unwrap();
        let summary: Vec<_> = blocks
            .iter()
            .map(|b| (b.command.as_str(), b.prompt_line, b.output, b.finished, b.exit))
            .collect();
        assert_eq!(
            summary,
            [
                ("echo hi", 0, (1, 2), true, Some(0)),
                ("false", 3, (4, 4), true, Some(1)),
                ("printf x", 4, (5, 6), true, Some(0)),
                // `A` starts a fresh line after output that left the cursor mid-row.
                ("sleep 100", 6, (7, 7), false, None),
            ]
        );
        assert_eq!(e.commands_ended(), 3, "the empty Enter ended nothing");
        let at = |line, col| Position { line, col, epoch: 0 };
        assert_eq!(e.ended_after(at(0, 0)), Some(at(2, 0)), "echo's D, on the row after hi");
        assert_eq!(e.ended_after(at(2, 1)), Some(at(4, 0)), "false's, past echo's");
        assert_eq!(e.ended_after(at(5, 2)), None, "sleep still runs");
        assert_eq!(e.ended_after(Position { epoch: 1, ..at(0, 0) }), None, "another numbering");
        assert_eq!(e.commands(Some(4)).unwrap().len(), 2, "blocks from line 4 on");
    }

    #[test]
    fn a_command_block_survives_a_full_screen_program() {
        let mut e = engine(40, 6);
        e.write(PROMPT);
        e.write(b"vim\r\n\x1b]133;C\x07\x1b[?1049h\x1b[Hediting\x1b[?1049l");
        e.write(b"\x1b]133;D;0\x07");
        let blocks = e.commands(None).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!((blocks[0].command.as_str(), blocks[0].exit), ("vim", Some(0)));
    }
}
