//! Keyboard copy mode: a cursor of its own that walks the history with vi's keys, marks a run,
//! whole lines or a block, and copies it, without the program hearing a key
//! (`docs/decisions/terminal.md`, "Keyboard copy mode").
//!
//! This is the model: where the cursor is, where the selection started, and what each key
//! means. It reads the client's mirror of the grid ([`Grid`]), so a key costs no round trip to
//! the worker; the view moves its viewport after it and draws the cursor.

use gpui::Keystroke;
use slopty_client::TermState;
use slopty_grid::{CellWidth, Line, LineFlags, LineIndex};

use crate::terminal::Selection;

/// What copy mode reads of the grid. [`TermState`] is the one the app has; the tests use a
/// list of lines.
pub(crate) trait Grid {
    /// Cells per row.
    fn cols(&self) -> u16;
    /// Rows in the viewport.
    fn rows(&self) -> u16;
    /// The oldest line the viewport can reach.
    fn oldest(&self) -> LineIndex;
    /// The bottom row of the screen.
    fn newest(&self) -> LineIndex;
    /// The line at the viewport's top row.
    fn top(&self) -> LineIndex;
    /// A line, when it is held here (history not fetched yet reads as blank).
    fn line(&self, index: LineIndex) -> Option<&Line>;
    /// The nearest prompt strictly above `index`.
    fn prompt_before(&self, index: LineIndex) -> Option<LineIndex>;
    /// The nearest prompt strictly below `index`.
    fn prompt_after(&self, index: LineIndex) -> Option<LineIndex>;
}

impl Grid for TermState {
    fn cols(&self) -> u16 {
        self.size().cols
    }

    fn rows(&self) -> u16 {
        self.size().rows
    }

    fn oldest(&self) -> LineIndex {
        LineIndex(first_visible(self).0.saturating_sub(self.history_len()))
    }

    fn newest(&self) -> LineIndex {
        first_visible(self).offset(u64::from(self.size().rows.saturating_sub(1)))
    }

    fn top(&self) -> LineIndex {
        self.index_at_row(0)
    }

    fn line(&self, index: LineIndex) -> Option<&Line> {
        Self::line(self, index)
    }

    fn prompt_before(&self, index: LineIndex) -> Option<LineIndex> {
        Self::prompt_before(self, index)
    }

    fn prompt_after(&self, index: LineIndex) -> Option<LineIndex> {
        Self::prompt_after(self, index)
    }
}

/// The screen's top line: the viewport's while it follows the output.
const fn first_visible(state: &TermState) -> LineIndex {
    state.index_at_row(0).offset(state.view_offset())
}

/// A cell: its line and column.
pub(crate) type At = (LineIndex, u16);

/// What a selection made in copy mode covers between its two ends.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// The cells between the ends in reading order (`v`, Space).
    Run,
    /// Every line from one end's to the other's, whole, soft wraps followed (`V`).
    Lines,
    /// The rectangle between the two ends (`⌃V`).
    Block,
}

/// Where a key moves the cursor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Motion {
    /// A cell left (`h`, ←), stopping at the row's start.
    Left,
    /// A cell right (`l`, →), stopping at the row's end.
    Right,
    /// A line up (`k`, ↑).
    Up,
    /// A line down (`j`, ↓).
    Down,
    /// The start of the next word (`w`): a word is a run of non-blank cells.
    WordNext,
    /// The start of this word, or of the one before (`b`).
    WordBack,
    /// The end of this word, or of the next (`e`).
    WordEnd,
    /// The row's first cell (`0`, Home).
    LineStart,
    /// The row's first non-blank cell (`^`).
    LineFirst,
    /// The row's last non-blank cell (`$`, End).
    LineEnd,
    /// The oldest line the history keeps (`g`).
    Oldest,
    /// The screen's bottom line (`G`).
    Newest,
    /// The viewport's top row (`H`).
    ViewTop,
    /// The viewport's middle row (`M`).
    ViewMiddle,
    /// The viewport's bottom row (`L`).
    ViewBottom,
    /// Half pages, up when negative: the viewport scrolls with the cursor (⇞ ⇟, `⌃B` `⌃F` a
    /// page, `⌃U` `⌃D` half of one).
    Halves(i8),
    /// The prompt above (`{`): shell integration's marks.
    PromptBack,
    /// The prompt below (`}`).
    PromptNext,
}

/// What a key in copy mode does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Command {
    /// Move the cursor (and the selection's free end with it).
    Move(Motion),
    /// Start a selection of this kind at the cursor, switch to it, or drop it when it is the
    /// kind already on.
    Select(Kind),
    /// Swap the cursor and the selection's other end (`o`).
    SwapEnds,
    /// Copy the selection and leave (`y`, ↩).
    Copy,
    /// Drop the selection; with none, leave (Esc, `⌃[`).
    Escape,
    /// Leave (`q`, `⌃C`, `⌃G`).
    Leave,
    /// Open the find bar (`/`, `?`).
    Find,
    /// The next search hit, newer (`n`); the cursor goes to it.
    NextHit,
    /// The previous search hit, older (`N`).
    PrevHit,
    /// A key copy mode has no use for: it is swallowed, never typed.
    Nothing,
}

/// What `keystroke` means in copy mode. ⌘ chords are not asked: the keymap and the app have
/// them.
#[must_use]
pub(crate) fn command(keystroke: &Keystroke) -> Command {
    use Motion as M;
    let m = keystroke.modifiers;
    if m.control && !m.alt && !m.platform {
        return match keystroke.key.as_str() {
            "b" => Command::Move(M::Halves(-2)),
            "f" => Command::Move(M::Halves(2)),
            "u" => Command::Move(M::Halves(-1)),
            "d" => Command::Move(M::Halves(1)),
            "v" => Command::Select(Kind::Block),
            "c" | "g" => Command::Leave,
            "[" => Command::Escape,
            _ => Command::Nothing,
        };
    }
    if m.control || m.alt || m.platform {
        return Command::Nothing;
    }
    let named = match keystroke.key.as_str() {
        "left" => Some(Command::Move(M::Left)),
        "right" => Some(Command::Move(M::Right)),
        "up" => Some(Command::Move(M::Up)),
        "down" => Some(Command::Move(M::Down)),
        "home" => Some(Command::Move(M::LineStart)),
        "end" => Some(Command::Move(M::LineEnd)),
        "pageup" => Some(Command::Move(M::Halves(-2))),
        "pagedown" => Some(Command::Move(M::Halves(2))),
        "escape" => Some(Command::Escape),
        "enter" => Some(Command::Copy),
        "space" => Some(Command::Select(Kind::Run)),
        _ => None,
    };
    if let Some(named) = named {
        return named;
    }
    let typed = keystroke.key_char.as_deref().unwrap_or(keystroke.key.as_str());
    let mut chars = typed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => typed_command(c),
        _ => Command::Nothing,
    }
}

/// What a typed character means in copy mode: a key's, or text an input method or a soft
/// keyboard committed.
#[must_use]
pub(crate) const fn typed_command(c: char) -> Command {
    use Motion as M;
    match c {
        'h' => Command::Move(M::Left),
        'l' => Command::Move(M::Right),
        'k' => Command::Move(M::Up),
        'j' => Command::Move(M::Down),
        'w' | 'W' => Command::Move(M::WordNext),
        'b' | 'B' => Command::Move(M::WordBack),
        'e' | 'E' => Command::Move(M::WordEnd),
        '0' => Command::Move(M::LineStart),
        '^' => Command::Move(M::LineFirst),
        '$' => Command::Move(M::LineEnd),
        'g' => Command::Move(M::Oldest),
        'G' => Command::Move(M::Newest),
        'H' => Command::Move(M::ViewTop),
        'M' => Command::Move(M::ViewMiddle),
        'L' => Command::Move(M::ViewBottom),
        '{' => Command::Move(M::PromptBack),
        '}' => Command::Move(M::PromptNext),
        'v' | ' ' => Command::Select(Kind::Run),
        'V' => Command::Select(Kind::Lines),
        'o' | 'O' => Command::SwapEnds,
        'y' | '\n' | '\r' => Command::Copy,
        'q' => Command::Leave,
        '/' | '?' => Command::Find,
        'n' => Command::NextHit,
        'N' => Command::PrevHit,
        _ => Command::Nothing,
    }
}

/// Copy mode's state: the cursor, and where a selection started.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Mode {
    /// The copy cursor.
    cursor: At,
    /// The selection's kind and its fixed end, while one is on.
    mark: Option<(Kind, At)>,
}

impl Mode {
    /// Copy mode at `cursor`, nothing selected.
    #[must_use]
    pub(crate) const fn at(cursor: At) -> Self {
        Self { cursor, mark: None }
    }

    /// Copy mode taking over a selection the pointer made: its moving end is the cursor.
    #[must_use]
    pub(crate) const fn adopting(selection: Selection) -> Self {
        let kind = if selection.block { Kind::Block } else { Kind::Run };
        Self { cursor: selection.head, mark: Some((kind, selection.anchor)) }
    }

    /// The copy cursor.
    #[must_use]
    pub(crate) const fn cursor(&self) -> At {
        self.cursor
    }

    /// Whether a selection is on.
    #[must_use]
    pub(crate) const fn selecting(&self) -> bool {
        self.mark.is_some()
    }

    /// The cursor to `at` (a search hit), the selection's other end staying.
    pub(crate) const fn go_to(&mut self, at: At) {
        self.cursor = at;
    }

    /// Esc: the selection goes, the cursor stays.
    pub(crate) const fn drop_selection(&mut self) {
        self.mark = None;
    }

    /// `v`, `V`, `⌃V`: a selection of `kind` from the cursor; the kind on already, none; another
    /// kind, the same ends as `kind`.
    pub(crate) fn select(&mut self, kind: Kind) {
        self.mark = match self.mark {
            Some((on, _)) if on == kind => None,
            Some((_, from)) => Some((kind, from)),
            None => Some((kind, self.cursor)),
        };
    }

    /// `o`: the cursor goes to the selection's other end, which stays where the cursor was.
    pub(crate) const fn swap_ends(&mut self) {
        if let Some((kind, from)) = self.mark {
            self.mark = Some((kind, self.cursor));
            self.cursor = from;
        }
    }

    /// The selection to draw and copy, as the view keeps one.
    #[must_use]
    pub(crate) fn selection(&self, grid: &impl Grid) -> Option<Selection> {
        let (kind, from) = self.mark?;
        Some(match kind {
            Kind::Run => Selection::run(from, self.cursor),
            Kind::Block => Selection { anchor: from, head: self.cursor, block: true },
            Kind::Lines => {
                let last = grid.cols().saturating_sub(1);
                let (first, end) = if from.0 <= self.cursor.0 {
                    (from.0, self.cursor.0)
                } else {
                    (self.cursor.0, from.0)
                };
                let start = (logical_start(grid, first), 0);
                let end = (logical_end(grid, end), last);
                if from.0 <= self.cursor.0 {
                    Selection::run(start, end)
                } else {
                    Selection::run(end, start)
                }
            }
        })
    }

    /// Move the cursor. Returns the lines the viewport scrolls with it (positive: up into
    /// history): a page's or half a page's worth, so the cursor keeps its row; 0 for every
    /// other motion, after which the view only brings the cursor into sight.
    pub(crate) fn move_by(&mut self, motion: Motion, grid: &impl Grid) -> i64 {
        let (line, col) = self.cursor;
        let last_col = grid.cols().saturating_sub(1);
        let (oldest, newest) = (grid.oldest(), grid.newest().max(grid.oldest()));
        let line = clamp(line, oldest, newest);
        let rows = u64::from(grid.rows().max(1));
        let mut scroll = 0;
        let to = match motion {
            Motion::Left => (line, col.saturating_sub(1)),
            // Over a wide character's tail: it is one stop.
            Motion::Right => {
                let next = col.saturating_add(1);
                let next = if is_tail(grid, (line, next)) { next.saturating_add(1) } else { next };
                (line, if next > last_col { col } else { next })
            }
            Motion::Up => (clamp(LineIndex(line.0.saturating_sub(1)), oldest, newest), col),
            Motion::Down => (clamp(line.next(), oldest, newest), col),
            Motion::WordNext => word_next(grid, (line, col)),
            Motion::WordBack => word_back(grid, (line, col)),
            Motion::WordEnd => word_end(grid, (line, col)),
            Motion::LineStart => (line, 0),
            Motion::LineFirst => (line, first_non_blank(grid, line)),
            Motion::LineEnd => (line, last_non_blank(grid, line)),
            Motion::Oldest => (oldest, 0),
            Motion::Newest => (newest, 0),
            Motion::ViewTop => (clamp(grid.top(), oldest, newest), col),
            Motion::ViewMiddle => {
                (clamp(grid.top().offset(rows.saturating_sub(1) / 2), oldest, newest), col)
            }
            Motion::ViewBottom => {
                (clamp(grid.top().offset(rows.saturating_sub(1)), oldest, newest), col)
            }
            Motion::Halves(halves) => {
                let lines = (rows / 2).max(1).saturating_mul(u64::from(halves.unsigned_abs()));
                let target = if halves < 0 {
                    LineIndex(line.0.saturating_sub(lines))
                } else {
                    line.offset(lines)
                };
                let target = clamp(target, oldest, newest);
                scroll = i64::try_from(line.0)
                    .unwrap_or(i64::MAX)
                    .saturating_sub(i64::try_from(target.0).unwrap_or(i64::MAX));
                (target, col)
            }
            Motion::PromptBack => grid.prompt_before(line).map_or((line, col), |p| (p, 0)),
            Motion::PromptNext => grid.prompt_after(line).map_or((line, col), |p| (p, 0)),
        };
        self.cursor = owner(grid, to);
        scroll
    }
}

/// `index` within `oldest..=newest`.
fn clamp(index: LineIndex, oldest: LineIndex, newest: LineIndex) -> LineIndex {
    index.max(oldest).min(newest)
}

/// Whether `index` carries on the line above it (the terminal wrapped it there).
fn continues(grid: &impl Grid, index: LineIndex) -> bool {
    grid.line(index).is_some_and(|line| line.flags.contains(LineFlags::WRAPPED))
}

/// The first row of the line `index` is part of, back over its soft wraps.
fn logical_start(grid: &impl Grid, mut index: LineIndex) -> LineIndex {
    let oldest = grid.oldest();
    while index > oldest && continues(grid, index) {
        index = LineIndex(index.0.saturating_sub(1));
    }
    index
}

/// The last row of the line `index` is part of, on over its soft wraps.
fn logical_end(grid: &impl Grid, mut index: LineIndex) -> LineIndex {
    let newest = grid.newest();
    while index < newest && continues(grid, index.next()) {
        index = index.next();
    }
    index
}

/// Whether the cell at `at` shows nothing: a blank or a line not held here. Either half of a
/// wide character reads as the character.
fn blank(grid: &impl Grid, (index, col): At) -> bool {
    let Some(line) = grid.line(index) else { return true };
    let cell = |col: u16| line.cells.get(usize::from(col));
    let owner = cell(col).and_then(|c| match c.width {
        CellWidth::Narrow | CellWidth::Wide => Some(c),
        CellWidth::SpacerTail => cell(col.checked_sub(1)?),
        CellWidth::SpacerHead => None,
    });
    owner.is_none_or(|c| c.text.as_str().trim().is_empty())
}

/// Whether the cell at `at` is the right half of a wide character.
fn is_tail(grid: &impl Grid, (index, col): At) -> bool {
    grid.line(index)
        .and_then(|line| line.cells.get(usize::from(col)))
        .is_some_and(|c| c.width == CellWidth::SpacerTail)
}

/// The cell a cursor at `at` stands on: the head of a wide character for its tail.
fn owner(grid: &impl Grid, at: At) -> At {
    if is_tail(grid, at) { (at.0, at.1.saturating_sub(1)) } else { at }
}

/// The cell after `at` in reading order, and whether a hard line break lies between them
/// (which parts words as a blank does). `None` past the newest line.
fn after(grid: &impl Grid, (index, col): At) -> Option<(At, bool)> {
    if col.saturating_add(1) < grid.cols() {
        return Some(((index, col.saturating_add(1)), false));
    }
    if index >= grid.newest() {
        return None;
    }
    let next = index.next();
    Some(((next, 0), !continues(grid, next)))
}

/// The cell before `at` in reading order, and whether a hard line break lies between them.
fn before(grid: &impl Grid, (index, col): At) -> Option<(At, bool)> {
    if let Some(col) = col.checked_sub(1) {
        return Some(((index, col), false));
    }
    if index <= grid.oldest() {
        return None;
    }
    let above = LineIndex(index.0.saturating_sub(1));
    Some(((above, grid.cols().saturating_sub(1)), !continues(grid, index)))
}

/// Whether the cells `cols` of `index` show nothing: a line not held here shows nothing. One
/// pass over the row's cells, so a blank stretch of history is crossed a row at a time.
fn blank_cells(grid: &impl Grid, index: LineIndex, cols: std::ops::RangeInclusive<u16>) -> bool {
    let Some(line) = grid.line(index) else { return true };
    let (from, to) = (usize::from(*cols.start()), usize::from(*cols.end()));
    line.cells.get(from..=to.min(line.cells.len().saturating_sub(1))).is_none_or(|cells| {
        cells.iter().all(|cell| cell.text.is_empty() || cell.text.as_str().trim().is_empty())
    })
}

/// From `at`, on past blank cells to the next word's first cell; the last cell reached when
/// none is left. A row whose rest is blank is passed in one step, not cell by cell.
fn skip_blanks_forward(grid: &impl Grid, mut at: At) -> At {
    let last = grid.cols().saturating_sub(1);
    while blank(grid, at) {
        if at.0 < grid.newest() && blank_cells(grid, at.0, at.1..=last) {
            at = (at.0.next(), 0);
            continue;
        }
        match after(grid, at) {
            Some((next, _)) => at = next,
            None => break,
        }
    }
    at
}

/// From `at`, back past blank cells to the previous word's last cell; the first cell reached
/// when none is left. A row blank up to `at` is passed in one step.
fn skip_blanks_back(grid: &impl Grid, mut at: At) -> At {
    let last = grid.cols().saturating_sub(1);
    while blank(grid, at) {
        if at.0 > grid.oldest() && blank_cells(grid, at.0, 0..=at.1) {
            at = (LineIndex(at.0.0.saturating_sub(1)), last);
            continue;
        }
        match before(grid, at) {
            Some((next, _)) => at = next,
            None => break,
        }
    }
    at
}

/// `w`: past the rest of this word, then past the blanks, to the next word's first cell.
fn word_next(grid: &impl Grid, from: At) -> At {
    let mut at = from;
    if !blank(grid, at) {
        loop {
            let Some((next, broke)) = after(grid, at) else { return at };
            at = next;
            if broke || blank(grid, at) {
                break;
            }
        }
    }
    skip_blanks_forward(grid, at)
}

/// `b`: back a cell, past blanks, to the first cell of the word reached.
fn word_back(grid: &impl Grid, from: At) -> At {
    let Some((start, _)) = before(grid, from) else { return from };
    let mut at = skip_blanks_back(grid, start);
    if blank(grid, at) {
        return at;
    }
    while let Some((prev, broke)) = before(grid, at) {
        if broke || blank(grid, prev) {
            break;
        }
        at = prev;
    }
    at
}

/// `e`: on a cell, past blanks, to the last cell of the word reached.
fn word_end(grid: &impl Grid, from: At) -> At {
    let Some((start, _)) = after(grid, from) else { return from };
    let mut at = skip_blanks_forward(grid, start);
    if blank(grid, at) {
        return at;
    }
    while let Some((next, broke)) = after(grid, at) {
        if broke || blank(grid, next) {
            break;
        }
        at = next;
    }
    at
}

/// The first non-blank cell of row `index`; 0 on a blank row.
fn first_non_blank(grid: &impl Grid, index: LineIndex) -> u16 {
    (0..grid.cols()).find(|&col| !blank(grid, (index, col))).unwrap_or(0)
}

/// The last non-blank cell of row `index` (the head of a wide character); 0 on a blank row.
fn last_non_blank(grid: &impl Grid, index: LineIndex) -> u16 {
    let col = (0..grid.cols()).rev().find(|&col| !blank(grid, (index, col))).unwrap_or(0);
    owner(grid, (index, col)).1
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;
    use slopty_grid::{Cell, Style};

    use super::*;

    /// Lines `0..` of `cols` cells, the last `rows` the screen, the viewport at its foot.
    struct Fixture {
        lines: Vec<Line>,
        cols: u16,
        rows: u16,
        top: LineIndex,
        prompts: Vec<LineIndex>,
    }

    impl Fixture {
        fn new(texts: &[&str], cols: u16, rows: u16) -> Self {
            let lines: Vec<Line> =
                texts.iter().map(|t| Line::from_text(t, cols, Style::DEFAULT)).collect();
            let top = LineIndex(lines.len().saturating_sub(usize::from(rows)) as u64);
            Self { lines, cols, rows, top, prompts: Vec::new() }
        }

        /// Line `index` carries on the one above it.
        fn wrapped(mut self, index: usize) -> Self {
            self.lines[index].flags |= LineFlags::WRAPPED;
            self
        }
    }

    impl Grid for Fixture {
        fn cols(&self) -> u16 {
            self.cols
        }

        fn rows(&self) -> u16 {
            self.rows
        }

        fn oldest(&self) -> LineIndex {
            LineIndex(0)
        }

        fn newest(&self) -> LineIndex {
            LineIndex((self.lines.len() as u64).saturating_sub(1))
        }

        fn top(&self) -> LineIndex {
            self.top
        }

        fn line(&self, index: LineIndex) -> Option<&Line> {
            self.lines.get(usize::try_from(index.0).ok()?)
        }

        fn prompt_before(&self, index: LineIndex) -> Option<LineIndex> {
            self.prompts.iter().rev().find(|&&p| p < index).copied()
        }

        fn prompt_after(&self, index: LineIndex) -> Option<LineIndex> {
            self.prompts.iter().find(|&&p| p > index).copied()
        }
    }

    fn at(line: u64, col: u16) -> At {
        (LineIndex(line), col)
    }

    /// Where `motions` take a cursor starting at `from`, one stop per motion.
    fn walk(grid: &Fixture, from: At, motion: Motion, times: usize) -> Vec<At> {
        let mut mode = Mode::at(from);
        std::iter::repeat_with(|| {
            mode.move_by(motion, grid);
            mode.cursor
        })
        .take(times)
        .collect()
    }

    /// The vi keys, the arrows and the named keys each mean their motion or act; ⌘ chords and
    /// keys copy mode has no use for mean nothing, so they are swallowed rather than typed.
    #[test]
    fn keys_mean_vi_s_motions_and_acts() {
        let key = |s: &str| command(&Keystroke::parse(s).unwrap().with_simulated_ime());
        assert_eq!(key("j"), Command::Move(Motion::Down));
        assert_eq!(key("down"), Command::Move(Motion::Down));
        assert_eq!(key("shift-down"), Command::Move(Motion::Down), "⇧ adds nothing to an arrow");
        assert_eq!(key("shift-g"), Command::Move(Motion::Newest));
        assert_eq!(key("g"), Command::Move(Motion::Oldest));
        assert_eq!(key("$"), Command::Move(Motion::LineEnd));
        assert_eq!(key("ctrl-u"), Command::Move(Motion::Halves(-1)));
        assert_eq!(key("pagedown"), Command::Move(Motion::Halves(2)));
        assert_eq!(key("v"), Command::Select(Kind::Run));
        assert_eq!(key("space"), Command::Select(Kind::Run));
        assert_eq!(key("shift-v"), Command::Select(Kind::Lines));
        assert_eq!(key("ctrl-v"), Command::Select(Kind::Block));
        assert_eq!(key("y"), Command::Copy);
        assert_eq!(key("enter"), Command::Copy);
        assert_eq!(key("escape"), Command::Escape);
        assert_eq!(key("ctrl-c"), Command::Leave);
        assert_eq!(key("q"), Command::Leave);
        assert_eq!(key("/"), Command::Find);
        assert_eq!(key("shift-n"), Command::PrevHit);
        assert_eq!(key("x"), Command::Nothing);
        assert_eq!(key("alt-j"), Command::Nothing);
        assert_eq!(key("tab"), Command::Nothing);
    }

    /// The cursor walks a cell or a line at a time, stopping at the grid's edges and at the
    /// oldest and newest lines.
    #[test]
    fn cells_and_lines_stop_at_the_edges() {
        let grid = Fixture::new(&["ab", "cd", "ef"], 4, 2);
        assert_eq!(walk(&grid, at(1, 2), Motion::Right, 3), [at(1, 3); 3]);
        assert_eq!(walk(&grid, at(1, 1), Motion::Left, 2), [at(1, 0), at(1, 0)]);
        assert_eq!(walk(&grid, at(1, 1), Motion::Up, 2), [at(0, 1), at(0, 1)]);
        assert_eq!(walk(&grid, at(1, 1), Motion::Down, 2), [at(2, 1), at(2, 1)]);
    }

    /// `w`, `b` and `e` go by runs of non-blank cells; a hard line break parts two words as a
    /// blank does, a soft wrap does not.
    #[test]
    fn words_are_runs_of_non_blank_cells() {
        let grid = Fixture::new(&["git log -p", "a", "", "  next"], 10, 2);
        let w = walk(&grid, at(0, 0), Motion::WordNext, 5);
        assert_eq!(w, [at(0, 4), at(0, 8), at(1, 0), at(3, 2), at(3, 9)], "the last: the end");
        let b = walk(&grid, at(3, 2), Motion::WordBack, 4);
        assert_eq!(b, [at(1, 0), at(0, 8), at(0, 4), at(0, 0)]);
        let e = walk(&grid, at(0, 0), Motion::WordEnd, 4);
        assert_eq!(e, [at(0, 2), at(0, 6), at(0, 9), at(1, 0)]);

        // "abc" wrapped as "ab" + "c": one word over the wrap.
        let wrapped = Fixture::new(&["x ab", "c bar"], 4, 2).wrapped(1);
        assert_eq!(walk(&wrapped, at(0, 2), Motion::WordNext, 1), [at(1, 2)]);
        assert_eq!(walk(&wrapped, at(1, 0), Motion::WordBack, 1), [at(0, 2)]);
        assert_eq!(walk(&wrapped, at(0, 2), Motion::WordEnd, 1), [at(1, 0)]);
    }

    /// `0`, `^` and `$` go to a row's start, its first non-blank cell and its last.
    #[test]
    fn a_row_s_ends() {
        let grid = Fixture::new(&["  ls -la   "], 12, 1);
        let mode = |motion| walk(&grid, at(0, 5), motion, 1)[0];
        assert_eq!(mode(Motion::LineStart), at(0, 0));
        assert_eq!(mode(Motion::LineFirst), at(0, 2));
        assert_eq!(mode(Motion::LineEnd), at(0, 7));
    }

    /// `g` and `G` reach the history's ends, `H` `M` `L` the viewport's rows, and a page
    /// moves the cursor and the viewport together.
    #[test]
    fn the_history_s_and_the_viewport_s_ends_and_pages() {
        let texts: Vec<String> = (0..20).map(|i| i.to_string()).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let grid = Fixture { top: LineIndex(10), ..Fixture::new(&texts, 4, 5) };
        let one = |motion| walk(&grid, at(12, 1), motion, 1)[0];
        assert_eq!(one(Motion::Oldest), at(0, 0));
        assert_eq!(one(Motion::Newest), at(19, 0));
        assert_eq!(one(Motion::ViewTop), at(10, 1));
        assert_eq!(one(Motion::ViewMiddle), at(12, 1));
        assert_eq!(one(Motion::ViewBottom), at(14, 1));

        let mut mode = Mode::at(at(12, 1));
        assert_eq!(mode.move_by(Motion::Halves(-2), &grid), 4, "a page: two halves of 2 rows");
        assert_eq!(mode.cursor, at(8, 1));
        assert_eq!(mode.move_by(Motion::Halves(1), &grid), -2);
        assert_eq!(mode.move_by(Motion::Halves(-2), &grid), 4);
        assert_eq!(mode.move_by(Motion::Halves(-2), &grid), 4);
        assert_eq!(mode.move_by(Motion::Halves(-2), &grid), 2, "stopped at the oldest line");
        assert_eq!(mode.cursor, at(0, 1));
    }

    /// `{` and `}` go to the prompts above and below; with none there the cursor stays.
    #[test]
    fn braces_walk_the_prompts() {
        let grid =
            Fixture { prompts: vec![LineIndex(1), LineIndex(4)], ..Fixture::new(&[""; 6], 4, 2) };
        assert_eq!(walk(&grid, at(3, 2), Motion::PromptBack, 2), [at(1, 0), at(1, 0)]);
        assert_eq!(walk(&grid, at(3, 2), Motion::PromptNext, 2), [at(4, 0), at(4, 0)]);
    }

    /// A run goes from the mark to the cursor; lines take whole rows over their soft wraps,
    /// whichever way the cursor went; a block is the rectangle; `o` swaps the ends, and the
    /// same kind again drops the selection.
    #[test]
    fn selections_of_each_kind() {
        let grid = Fixture::new(&["one", "two", "abc", "de"], 3, 2).wrapped(3);
        let mut mode = Mode::at(at(1, 1));
        assert_eq!(mode.selection(&grid), None);
        mode.select(Kind::Run);
        mode.move_by(Motion::Down, &grid);
        assert_eq!(mode.selection(&grid), Some(Selection::run(at(1, 1), at(2, 1))));

        mode.select(Kind::Lines);
        assert_eq!(
            mode.selection(&grid),
            Some(Selection::run(at(1, 0), at(3, 2))),
            "wrap followed"
        );
        mode.swap_ends();
        assert_eq!(mode.cursor, at(1, 1));
        assert_eq!(mode.selection(&grid), Some(Selection::run(at(3, 2), at(1, 0))));

        mode.select(Kind::Block);
        let block = mode.selection(&grid).unwrap();
        assert!(block.block);
        assert_eq!((block.anchor, block.head), (at(2, 1), at(1, 1)));
        mode.select(Kind::Block);
        assert_eq!(mode.selection(&grid), None, "the same kind again: none");
    }

    /// A wide character is one stop: the cursor never rests on its tail.
    #[test]
    fn a_wide_character_is_one_stop() {
        let mut grid = Fixture::new(&["a    b"], 6, 1);
        grid.lines[0].cells[2] = Cell::wide("界", Style::DEFAULT);
        grid.lines[0].cells[3] = Cell::spacer_tail(Style::DEFAULT);
        assert_eq!(walk(&grid, at(0, 2), Motion::Right, 1), [at(0, 4)], "over the tail");
        assert_eq!(walk(&grid, at(0, 4), Motion::Left, 1), [at(0, 2)], "the tail is the head");
        assert_eq!(walk(&grid, at(0, 0), Motion::WordNext, 1), [at(0, 2)]);
        assert_eq!(walk(&grid, at(0, 0), Motion::WordEnd, 1), [at(0, 2)]);
    }
}
