//! Text search over the plain-text rendering of the grid.
//!
//! The engine formats the retained history plus the screen as plain text (one line per row,
//! trailing blanks trimmed), with each row's soft-wrap flag, and this module finds the needle
//! in it. Rows a line soft-wraps over are searched as the one line the program wrote, so a hit
//! can run over the wrap. Columns are cells, so a hit can be painted straight onto the grid:
//! cluster widths come from libghostty's own tables, the same ones that laid the cells out.
//!
//! A row that scrolled into history never changes again within its numbering, so [`History`]
//! keeps each one's text from the search that first formatted it, and the hits of the last
//! needle in them: a find bar refreshed while a program writes formats and scans only the rows
//! written since, and the screen.

use std::borrow::Cow;
use std::collections::VecDeque;

use libghostty_vt::unicode::grapheme_width;
use slopty_grid::LineIndex;
use slopty_proto::terminal::SearchMatch;

/// Outcome of a search: how many hits there were and the newest `max` of them, oldest first.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Found {
    /// Every hit, listed or not.
    pub total: u32,
    /// The newest hits, ascending by line.
    pub matches: Vec<SearchMatch>,
}

/// What to look for: plain text or a regular expression, both with smart case.
///
/// Plain text is an escaped regex: the crate's literal search is memchr-fast and its case
/// folding is Unicode's, where folding the haystack line by line was an allocation per row.
#[derive(Clone, Debug)]
pub struct Pattern(regex::Regex);

impl Pattern {
    /// Build a pattern; a regex that does not compile is the error message.
    pub fn new(needle: &str, regex: bool) -> Result<Self, String> {
        let insensitive = !needle.chars().any(char::is_uppercase);
        let source = if regex { needle.to_owned() } else { regex::escape(needle) };
        regex::RegexBuilder::new(&source)
            .case_insensitive(insensitive)
            .size_limit(1 << 20)
            .build()
            .map(Self)
            .map_err(|e| e.to_string())
    }

    /// Whether the pattern can match anything at all.
    fn is_empty(&self) -> bool {
        self.0.as_str().is_empty()
    }

    /// Hits in one line as `(first char, char count)`; empty matches are skipped.
    fn hits(&self, line: &str) -> Vec<(usize, usize)> {
        self.0
            .find_iter(line)
            .filter(|m| !m.as_str().is_empty())
            .map(|m| (char_index(line, m.start()), m.as_str().chars().count()))
            .collect()
    }
}

/// Chars before byte offset `byte`.
fn char_index(s: &str, byte: usize) -> usize {
    s.get(..byte).map_or(0, |head| head.chars().count())
}

/// Search's copy of the rows that scrolled into history, as plain text, and the hits of the
/// last needle in them.
///
/// Rows are numbered as the engine numbers lines. Within one numbering (the engine's epoch) a
/// history row is never written again: rows are only appended below and evicted from the top,
/// and anything that rewrites history (a reflow, the alternate screen) starts a new numbering.
#[derive(Debug, Default)]
pub struct History {
    /// The numbering the rows are of; `None` before the first search.
    epoch: Option<u32>,
    /// The width of the grid the rows were written on, the same for a whole numbering: a line
    /// that soft-wraps fills each row but its last to it.
    cols: u16,
    /// Absolute line of the row that starts at `text[start..]`.
    first: u64,
    /// One past the last row held.
    end: u64,
    /// Each row's text and a newline; the bytes before `start` are rows evicted since.
    text: String,
    start: usize,
    /// Whether each row held soft-wraps into the next, from row `first`.
    wraps: VecDeque<bool>,
    /// The last needle's hits in the rows held.
    hits: Option<Hits>,
}

/// One needle's hits in the rows of a [`History`] it has scanned.
#[derive(Debug)]
struct Hits {
    needle: String,
    regex: bool,
    max: u32,
    /// One past the last row scanned, and where its text starts in the history's text.
    scanned: u64,
    scanned_at: usize,
    /// Rows with hits and how many, oldest first: what an eviction takes off the total.
    rows: VecDeque<(u64, u32)>,
    total: u64,
    /// The newest `max` hits, oldest first.
    newest: VecDeque<SearchMatch>,
}

impl History {
    /// Bring the record to a history that is now rows `[base, end)` of numbering `epoch`, on a
    /// grid `cols` wide: forget what another numbering or an eviction made wrong, and return
    /// the rows it lacks, for [`Self::append`].
    pub fn missing(&mut self, epoch: u32, cols: u16, base: u64, end: u64) -> Option<(u64, u64)> {
        if self.epoch != Some(epoch) || self.cols != cols || end < self.end || base >= self.end {
            self.reset(epoch, cols, base.min(end));
        } else if base > self.first {
            self.evict(base);
        }
        (self.end < end).then_some((self.end, end))
    }

    /// The next `rows` rows below the ones held, as the formatter wrote them: one line each,
    /// blank rows at the end left out. `wraps` says which soft-wrap into the next.
    pub fn append(&mut self, formatted: &str, rows: u64, wraps: &[bool]) {
        let count = usize::try_from(rows).unwrap_or(usize::MAX);
        self.wraps.extend((0..count).map(|i| wraps.get(i).copied().unwrap_or(false)));
        let mut added = 0_u64;
        for line in formatted.split('\n').take(usize::try_from(rows).unwrap_or(usize::MAX)) {
            self.text.push_str(line);
            self.text.push('\n');
            added = added.saturating_add(1);
        }
        for _ in added..rows {
            self.text.push('\n');
        }
        self.end = self.end.saturating_add(rows);
    }

    /// The text of rows `[first, end)` when numbering `epoch`'s history holds them all.
    #[must_use]
    pub fn rows(&self, epoch: u32, first: u64, end: u64) -> Option<Vec<String>> {
        if self.epoch != Some(epoch) || first < self.first || end > self.end {
            return None;
        }
        let skip = usize::try_from(first.saturating_sub(self.first)).ok()?;
        let take = usize::try_from(end.saturating_sub(first)).ok()?;
        let held = self.text.get(self.start..)?;
        Some(held.split_terminator('\n').skip(skip).take(take).map(str::to_owned).collect())
    }

    /// Find `pattern` (from `needle` and `regex`) in the rows held, then in `screen`, the
    /// rows below them with their soft-wrap flags, on the same grid. Only the rows held that
    /// the same needle has not scanned yet are scanned.
    pub fn find<'s>(
        &mut self,
        pattern: &Pattern,
        needle: &str,
        regex: bool,
        max: u32,
        screen: impl Iterator<Item = (&'s str, bool)>,
    ) -> Found {
        if pattern.is_empty() {
            return Found::default();
        }
        let same = self
            .hits
            .as_ref()
            .is_some_and(|h| h.needle == needle && h.regex == regex && h.max == max);
        if !same {
            self.hits = Some(Hits {
                needle: needle.to_owned(),
                regex,
                max,
                scanned: self.first,
                scanned_at: self.start,
                rows: VecDeque::new(),
                total: 0,
                newest: VecDeque::new(),
            });
        }
        let (text, end, first, cols) = (&self.text, self.end, self.first, self.cols);
        let Some(hits) = &mut self.hits else { return Found::default() };
        let wrap_of = |row: u64| {
            usize::try_from(row.saturating_sub(first))
                .ok()
                .and_then(|i| self.wraps.get(i))
                .copied()
                .unwrap_or(false)
        };
        // A line that soft-wraps past the last row held goes on into the screen: the rows of
        // it held are searched with the screen, and scanned for good once it ends.
        let mut complete = end;
        while complete > hits.scanned && wrap_of(complete.saturating_sub(1)) {
            complete = complete.saturating_sub(1);
        }
        let unscanned = text.get(hits.scanned_at..).unwrap_or_default();
        let from = usize::try_from(hits.scanned.saturating_sub(first)).unwrap_or(usize::MAX);
        let mut rows = unscanned.split_terminator('\n').zip(self.wraps.iter().skip(from).copied());
        let complete_rows = usize::try_from(complete.saturating_sub(hits.scanned)).unwrap_or(0);
        if complete_rows > 0 {
            let found = scan(rows.by_ref().take(complete_rows), pattern, hits.scanned, max, cols);
            hits.total =
                hits.total.saturating_add(found.rows.iter().map(|&(_, n)| u64::from(n)).sum());
            hits.rows.extend(found.rows);
            hits.newest.extend(found.matches);
            let keep = usize::try_from(max).unwrap_or(usize::MAX);
            let excess = hits.newest.len().saturating_sub(keep);
            hits.newest.drain(..excess);
            hits.scanned = complete;
            hits.scanned_at = tail_start(text, end.saturating_sub(complete), hits.scanned_at);
        }
        // The screen's rows are the caller's, the held ones this record's: one lifetime for both.
        let screen = screen.map(|(t, wraps)| -> (&str, bool) { (t, wraps) });
        let below = scan(rows.chain(screen), pattern, complete, max, cols);
        let total = hits.total.saturating_add(below.rows.iter().map(|&(_, n)| u64::from(n)).sum());
        let keep = usize::try_from(max).unwrap_or(usize::MAX);
        let older = keep.saturating_sub(below.matches.len());
        let skip = hits.newest.len().saturating_sub(older);
        let matches = hits.newest.iter().skip(skip).copied().chain(below.matches).collect();
        Found { total: u32::try_from(total).unwrap_or(u32::MAX), matches }
    }

    /// One past the last row the last needle scanned.
    #[cfg(test)]
    pub(crate) fn hits_scanned(&self) -> u64 {
        self.hits.as_ref().map_or(0, |h| h.scanned)
    }

    fn reset(&mut self, epoch: u32, cols: u16, at: u64) {
        *self = Self { epoch: Some(epoch), cols, first: at, end: at, ..Self::default() };
    }

    /// Forget the rows above `base`, which the terminal no longer keeps.
    fn evict(&mut self, base: u64) {
        let gone = base.min(self.end).saturating_sub(self.first);
        let mut at = self.start;
        for _ in 0..gone {
            at = memchr::memchr(b'\n', self.text.as_bytes().get(at..).unwrap_or_default())
                .map_or(self.text.len(), |n| at.saturating_add(n).saturating_add(1));
        }
        self.start = at;
        self.first = self.first.saturating_add(gone);
        let gone = usize::try_from(gone).unwrap_or(usize::MAX).min(self.wraps.len());
        self.wraps.drain(..gone);
        if let Some(hits) = &mut self.hits {
            while hits.rows.front().is_some_and(|&(line, _)| line < self.first) {
                if let Some((_, n)) = hits.rows.pop_front() {
                    hits.total = hits.total.saturating_sub(u64::from(n));
                }
            }
            while hits.newest.front().is_some_and(|m| m.line.0 < self.first) {
                hits.newest.pop_front();
            }
            if hits.scanned < self.first {
                (hits.scanned, hits.scanned_at) = (self.first, self.start);
            }
        }
        // The evicted rows' bytes go once they are half the text, not on every eviction.
        if self.start > self.text.len() / 2 {
            self.text.drain(..self.start);
            if let Some(hits) = &mut self.hits {
                hits.scanned_at = hits.scanned_at.saturating_sub(self.start);
            }
            self.start = 0;
        }
    }
}

/// Where the last `rows` rows of `text` (each ended by a newline) start, found from the end:
/// nearly always none, a line wrapped past the last row held, so no pass over the whole text.
/// Never before `floor`.
fn tail_start(text: &str, rows: u64, floor: usize) -> usize {
    let bytes = text.as_bytes();
    let mut at = text.len();
    for _ in 0..rows {
        let before = bytes.get(..at.saturating_sub(1)).unwrap_or_default();
        at = memchr::memrchr(b'\n', before).map_or(floor, |n| n.saturating_add(1));
    }
    at.max(floor)
}

/// Find `pattern` in `text`, whose first line is absolute line `base`, on a grid `cols` wide.
/// `wraps` says which rows soft-wrap into the next; rows past its end do not.
///
/// Smart case: a needle without an upper-case letter matches case-insensitively. Rows a line
/// soft-wraps over are one line, so a hit can run over the wrap: its `len` then counts the
/// cells in reading order from its start, over the end of its row onto the next. Every hit is
/// counted, but columns are laid out only for the `max` newest ones that are reported: the
/// column of a hit needs the cell width of every cluster before it on its row, which is a call
/// into libghostty per character, and a needle that hits every row of a long history would pay
/// it fifty thousand times for a hundred answers.
#[must_use]
pub fn find(
    text: &str,
    wraps: &[bool],
    pattern: &Pattern,
    base: LineIndex,
    max: u32,
    cols: u16,
) -> Found {
    if pattern.is_empty() {
        return Found::default();
    }
    let rows =
        text.split('\n').enumerate().map(|(i, t)| (t, wraps.get(i).copied().unwrap_or(false)));
    let found = scan(rows, pattern, base.0, max, cols);
    let total = found.rows.iter().fold(0_u32, |sum, &(_, n)| sum.saturating_add(n));
    Found { total, matches: found.matches }
}

/// What [`scan`] found: every row with hits and how many, and the newest `max` hits laid out.
struct Scanned {
    rows: Vec<(u64, u32)>,
    matches: Vec<SearchMatch>,
}

/// A line as the program wrote it: the rows it soft-wraps over, joined.
#[derive(Clone)]
struct Logical<'a> {
    /// The absolute line of its first row.
    row: u64,
    /// The rows' text, each but the last filled out with blanks to the grid's width.
    text: Cow<'a, str>,
    /// Where each row starts in `text`: its char index and its byte offset. A line of one row,
    /// nearly every line, allocates nothing.
    starts: Cow<'static, [(usize, usize)]>,
}

impl Logical<'_> {
    /// The row char `at` is on, as an index into the rows, and its char index within it.
    fn place(&self, at: usize) -> (usize, usize) {
        let part = self.starts.partition_point(|&(c, _)| c <= at).saturating_sub(1);
        let from = self.starts.get(part).map_or(0, |&(c, _)| c);
        (part, at.saturating_sub(from))
    }

    /// The text of row `part`, its fill included.
    fn part(&self, part: usize) -> &str {
        let from = self.starts.get(part).map_or(0, |&(_, b)| b);
        let to = self.starts.get(part.saturating_add(1)).map_or(self.text.len(), |&(_, b)| b);
        self.text.get(from..to).unwrap_or_default()
    }
}

/// Join the rows of one line: each row but the last is filled out to `cols` cells, as the
/// trimmed blanks at its end were, unless the next row starts with a wide cluster that did
/// not fit in the one cell left (a spacer, not a blank).
fn join<'a>(row: u64, parts: &[&'a str], cols: u16) -> Logical<'a> {
    if let [only] = parts {
        return Logical { row, text: Cow::Borrowed(only), starts: Cow::Borrowed(&[(0, 0)]) };
    }
    let mut text = String::new();
    let mut starts = Vec::with_capacity(parts.len());
    let mut chars = 0_usize;
    for (i, part) in parts.iter().enumerate() {
        starts.push((chars, text.len()));
        text.push_str(part);
        chars = chars.saturating_add(part.chars().count());
        let Some(next) = parts.get(i.saturating_add(1)) else { break };
        let mut pad = cols.saturating_sub(width_of(part));
        let lead: Vec<char> = next.chars().take(8).collect();
        if pad == 1 && !lead.is_empty() && grapheme_width(&lead).1 == 2 {
            pad = 0;
        }
        text.extend(std::iter::repeat_n(' ', usize::from(pad)));
        chars = chars.saturating_add(usize::from(pad));
    }
    Logical { row, text: Cow::Owned(text), starts: Cow::Owned(starts) }
}

/// Cells a row's text covers.
fn width_of(text: &str) -> u16 {
    if text.is_ascii() {
        u16::try_from(text.len()).unwrap_or(u16::MAX)
    } else {
        Widths::new(text).end()
    }
}

/// Find `pattern` in `rows`, each a row's text and whether it soft-wraps into the next, the
/// first of which is absolute line `base`: every line that hits (by its first row), and the
/// newest `max` hits with their columns.
fn scan<'a>(
    rows: impl Iterator<Item = (&'a str, bool)>,
    pattern: &Pattern,
    base: u64,
    max: u32,
    cols: u16,
) -> Scanned {
    let keep = usize::try_from(max).unwrap_or(usize::MAX);
    // `(line, first char, char count)` of the hits still in the running. A line of one row is
    // borrowed and copies for free; only a wrapped line with hits owns its text.
    let mut pending: Vec<(Logical<'a>, usize, usize)> = Vec::new();
    let mut hit_rows = Vec::new();
    let mut hit = |line: Logical<'a>| {
        let found = pattern.hits(&line.text);
        if found.is_empty() {
            return;
        }
        hit_rows.push((line.row, u32::try_from(found.len()).unwrap_or(u32::MAX)));
        pending.extend(found.into_iter().map(|(at, count)| (line.clone(), at, count)));
        // Older hits than the newest `keep` are never reported: forget them in batches.
        if pending.len() > keep.saturating_mul(2) {
            let excess = pending.len().saturating_sub(keep);
            pending.drain(..excess);
        }
    };
    let mut parts: Vec<&'a str> = Vec::new();
    let mut first = base;
    for (row, (text, wraps)) in (base..).zip(rows) {
        // Nearly every row is a line of its own, searched as it is.
        if parts.is_empty() && !wraps {
            hit(join(row, &[text], cols));
            continue;
        }
        if parts.is_empty() {
            first = row;
        }
        parts.push(text);
        if !wraps {
            hit(join(first, &parts, cols));
            parts.clear();
        }
    }
    if !parts.is_empty() {
        hit(join(first, &parts, cols));
    }
    let excess = pending.len().saturating_sub(keep);
    let mut widths: Option<(u64, usize, Widths)> = None;
    let mut column = |line: &Logical<'_>, part: usize, at: usize| -> u16 {
        let text = line.part(part);
        if text.is_ascii() {
            return u16::try_from(at).unwrap_or(u16::MAX);
        }
        let w = match &widths {
            Some((row, p, w)) if *row == line.row && *p == part => w,
            _ => &widths.insert((line.row, part, Widths::new(text))).2,
        };
        w.at(at)
    };
    let matches = pending
        .iter()
        .skip(excess)
        .map(|(line, at, count)| {
            let (first_part, first_at) = line.place(*at);
            let last = at.saturating_add(count.saturating_sub(1));
            let (last_part, last_at) = line.place(last);
            let col = column(line, first_part, first_at);
            let end = column(line, last_part, last_at.saturating_add(1));
            let rows_over = u16::try_from(last_part.saturating_sub(first_part)).unwrap_or(u16::MAX);
            let len = rows_over.saturating_mul(cols).saturating_add(end).saturating_sub(col).max(1);
            let row = line.row.saturating_add(u64::try_from(first_part).unwrap_or(u64::MAX));
            SearchMatch { line: LineIndex(row), col, len }
        })
        .collect();
    Scanned { rows: hit_rows, matches }
}

/// Cell column at each char boundary of a line.
struct Widths {
    /// `starts[i]` is the column where char `i` begins; one extra entry for the end.
    starts: Vec<u16>,
}

impl Widths {
    fn new(line: &str) -> Self {
        let chars: Vec<char> = line.chars().collect();
        let mut starts = Vec::with_capacity(chars.len().saturating_add(1));
        let mut col: u16 = 0;
        let mut i = 0;
        while let Some(rest) = chars.get(i..).filter(|rest| !rest.is_empty()) {
            let (consumed, width) = grapheme_width(rest);
            let consumed = consumed.max(1);
            // Every char of the cluster starts at the cluster's column.
            for _ in 0..consumed {
                starts.push(col);
            }
            col = col.saturating_add(u16::from(width));
            i = i.saturating_add(consumed);
        }
        starts.push(col);
        Self { starts }
    }

    /// The column char `at` starts at; the end of the line for one past it.
    fn at(&self, at: usize) -> u16 {
        let last = self.starts.len().saturating_sub(1);
        self.starts.get(at.min(last)).copied().unwrap_or(0)
    }

    /// The column after the last cluster.
    fn end(&self) -> u16 {
        self.starts.last().copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(line: u64, col: u16, len: u16) -> SearchMatch {
        SearchMatch { line: LineIndex(line), col, len }
    }

    fn find(text: &str, needle: &str, base: LineIndex, max: u32) -> Found {
        super::find(text, &[], &Pattern::new(needle, false).expect("plain"), base, max, 80)
    }

    fn find_re(text: &str, needle: &str, base: LineIndex, max: u32) -> Found {
        super::find(text, &[], &Pattern::new(needle, true).expect("regex"), base, max, 80)
    }

    fn find_wrapped(text: &str, wraps: &[bool], needle: &str, cols: u16) -> Found {
        let pattern = Pattern::new(needle, false).expect("plain");
        super::find(text, wraps, &pattern, LineIndex(0), 100, cols)
    }

    /// A wrapped line is one: a hit over a wrap counts its cells in reading order, the blanks
    /// trimmed at the wrap are put back, and a wide cluster pushed onto the next row leaves no
    /// blank behind.
    #[test]
    fn a_hit_runs_over_a_soft_wrap() {
        // "abcde" wraps into "fgh" on a 5-column grid.
        let found = find_wrapped("abcde\nfgh", &[true], "def", 5);
        assert_eq!(found.matches, vec![m(0, 3, 3)]);
        // Three rows of one line, the hit from the first row's last cell to the third's first.
        let found = find_wrapped("abcde\nfghij\nk", &[true, true], "efghijk", 5);
        assert_eq!(found.matches, vec![m(0, 4, 7)]);
        // "ab" and three trimmed blanks, then "cd": the blanks are put back.
        let found = find_wrapped("ab\ncd", &[true], "b   c", 5);
        assert_eq!(found.matches, vec![m(0, 1, 5)]);
        assert_eq!(find_wrapped("ab\ncd", &[true], "bc", 5).total, 0);
        // "abcd" and a spacer, then "日": the wide char did not fit in the last cell.
        let found = find_wrapped("abcd\n日x", &[true], "d日", 5);
        assert_eq!(found.matches, vec![m(0, 3, 4)]);
        // Rows that end a line stay apart.
        assert_eq!(find_wrapped("abcde\nfgh", &[false], "def", 5).total, 0);
        // A hit wholly on the second row is placed on it.
        let found = find_wrapped("abcde\nfgh", &[true], "gh", 5);
        assert_eq!(found.matches, vec![m(1, 1, 2)]);
    }

    #[test]
    fn regex_hits_with_smart_case_and_columns() {
        let text = "The fox\n\nfox FOX\nfx";
        let found = find_re(text, "f.x", LineIndex(10), 100);
        assert_eq!(found.total, 3);
        assert_eq!(found.matches, vec![m(10, 4, 3), m(12, 0, 3), m(12, 4, 3)]);
        let found = find_re(text, "F[A-Z]X", LineIndex(10), 100);
        assert_eq!(found.matches, vec![m(12, 4, 3)]);
        // Variable-length matches report their own length; empty matches are skipped.
        let found = find_re("aaa b", "a+|x*", LineIndex(0), 100);
        assert_eq!(found.matches, vec![m(0, 0, 3)]);
    }

    #[test]
    fn bad_regex_is_an_error_and_plain_never_is() {
        Pattern::new("(", true).unwrap_err();
        Pattern::new("(", false).unwrap();
    }

    #[test]
    fn smart_case_and_columns() {
        let text = "The fox\n\nfox FOX\nno";
        let found = find(text, "fox", LineIndex(10), 100);
        assert_eq!(found.total, 3);
        assert_eq!(found.matches, vec![m(10, 4, 3), m(12, 0, 3), m(12, 4, 3)]);
        let found = find(text, "FOX", LineIndex(10), 100);
        assert_eq!(found.matches, vec![m(12, 4, 3)]);
    }

    #[test]
    fn keeps_the_newest_hits_and_counts_all() {
        let text = "a\na\na\na";
        let found = find(text, "a", LineIndex(0), 2);
        assert_eq!(found.total, 4);
        assert_eq!(found.matches, vec![m(2, 0, 1), m(3, 0, 1)]);
    }

    /// The running set is trimmed in batches while scanning; the answer is still the newest
    /// `max` hits with the full count, and columns on a non-ASCII row are laid out per row.
    #[test]
    fn a_needle_that_hits_every_row_keeps_the_newest_and_counts_them_all() {
        let text = (0..1_000).map(|i| format!("日本 a{i} a")).collect::<Vec<_>>().join("\n");
        let found = find(&text, "a", LineIndex(0), 3);
        assert_eq!(found.total, 2_000);
        // Row 999 is "日本 a999 a": its "a"s are at columns 5 and 10; the third newest hit is
        // row 998's last one.
        assert_eq!(found.matches, vec![m(998, 10, 1), m(999, 5, 1), m(999, 10, 1)]);
    }

    #[test]
    fn wide_characters_count_two_cells() {
        // 日本 is two wide cells each; "x" starts at column 4.
        let found = find("日本x", "x", LineIndex(0), 10);
        assert_eq!(found.matches, vec![m(0, 4, 1)]);
        let found = find("日本x", "本x", LineIndex(0), 10);
        assert_eq!(found.matches, vec![m(0, 2, 3)]);
        // Each non-ASCII row is laid out by its own widths, never the previous row's.
        let found = find("日本x\nx日本x", "x", LineIndex(0), 10);
        assert_eq!(found.matches, vec![m(0, 4, 1), m(1, 0, 1), m(1, 5, 1)]);
    }

    #[test]
    fn empty_needle_finds_nothing() {
        assert_eq!(find("anything", "", LineIndex(0), 10), Found::default());
    }
}
