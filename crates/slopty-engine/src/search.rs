//! Text search over the plain-text rendering of the grid.
//!
//! The engine formats the retained history plus the screen as plain text (one line per row,
//! trailing blanks trimmed) and this module finds the needle in it. Columns are cells, so a
//! hit can be painted straight onto the grid: cluster widths come from libghostty's own
//! tables, the same ones that laid the cells out.
//!
//! A row that scrolled into history never changes again within its numbering, so [`History`]
//! keeps each one's text from the search that first formatted it, and the hits of the last
//! needle in them: a find bar refreshed while a program writes formats and scans only the rows
//! written since, and the screen.

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
    /// Absolute line of the row that starts at `text[start..]`.
    first: u64,
    /// One past the last row held.
    end: u64,
    /// Each row's text and a newline; the bytes before `start` are rows evicted since.
    text: String,
    start: usize,
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
    /// Bring the record to a history that is now rows `[base, end)` of numbering `epoch`:
    /// forget what another numbering or an eviction made wrong, and return the rows it lacks,
    /// for [`Self::append`].
    pub fn missing(&mut self, epoch: u32, base: u64, end: u64) -> Option<(u64, u64)> {
        if self.epoch != Some(epoch) || end < self.end || base >= self.end {
            self.reset(epoch, base.min(end));
        } else if base > self.first {
            self.evict(base);
        }
        (self.end < end).then_some((self.end, end))
    }

    /// The next `rows` rows below the ones held, as the formatter wrote them: one line each,
    /// blank rows at the end left out.
    pub fn append(&mut self, formatted: &str, rows: u64) {
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
    /// plain text of the rows below them. Only the rows held that the same needle has not
    /// scanned yet are scanned.
    pub fn find(
        &mut self,
        pattern: &Pattern,
        needle: &str,
        regex: bool,
        max: u32,
        screen: &str,
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
        let (text, end) = (&self.text, self.end);
        let Some(hits) = &mut self.hits else { return Found::default() };
        if hits.scanned < end {
            let unscanned = text.get(hits.scanned_at..).unwrap_or_default();
            let found = scan(unscanned.split_terminator('\n'), pattern, hits.scanned, max);
            hits.total =
                hits.total.saturating_add(found.rows.iter().map(|&(_, n)| u64::from(n)).sum());
            hits.rows.extend(found.rows);
            hits.newest.extend(found.matches);
            let keep = usize::try_from(max).unwrap_or(usize::MAX);
            let excess = hits.newest.len().saturating_sub(keep);
            hits.newest.drain(..excess);
            hits.scanned = end;
            hits.scanned_at = text.len();
        }
        let below = scan(screen.split('\n'), pattern, end, max);
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

    fn reset(&mut self, epoch: u32, at: u64) {
        *self = Self { epoch: Some(epoch), first: at, end: at, ..Self::default() };
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

/// What [`scan`] found: every row with hits and how many, and the newest `max` hits laid out.
struct Scanned {
    rows: Vec<(u64, u32)>,
    matches: Vec<SearchMatch>,
}

/// Find `pattern` in `text`, whose first line is absolute line `base`.
///
/// Smart case: a needle without an upper-case letter matches case-insensitively. Hits never
/// span rows (soft-wrapped lines are searched row by row). Every hit is counted, but columns
/// are laid out only for the `max` newest ones that are reported: the column of a hit needs
/// the cell width of every cluster before it on its row, which is a call into libghostty per
/// character, and a needle that hits every row of a long history would pay it fifty thousand
/// times for a hundred answers.
#[must_use]
pub fn find(text: &str, pattern: &Pattern, base: LineIndex, max: u32) -> Found {
    if pattern.is_empty() {
        return Found::default();
    }
    let found = scan(text.split('\n'), pattern, base.0, max);
    let total = found.rows.iter().fold(0_u32, |sum, &(_, n)| sum.saturating_add(n));
    Found { total, matches: found.matches }
}

/// Find `pattern` in `lines`, the first of which is absolute line `base`: every row that
/// hits, and the newest `max` hits with their columns.
fn scan<'a>(
    lines: impl Iterator<Item = &'a str>,
    pattern: &Pattern,
    base: u64,
    max: u32,
) -> Scanned {
    let keep = usize::try_from(max).unwrap_or(usize::MAX);
    // `(row, line, first char, char count)` of the hits still in the running.
    let mut pending: Vec<(u64, &str, usize, usize)> = Vec::new();
    let mut rows = Vec::new();
    for (row, line) in (base..).zip(lines) {
        let found = pattern.hits(line);
        if found.is_empty() {
            continue;
        }
        rows.push((row, u32::try_from(found.len()).unwrap_or(u32::MAX)));
        pending.extend(found.into_iter().map(|(first, count)| (row, line, first, count)));
        // Older hits than the newest `keep` are never reported: forget them in batches.
        if pending.len() > keep.saturating_mul(2) {
            let excess = pending.len().saturating_sub(keep);
            pending.drain(..excess);
        }
    }
    let excess = pending.len().saturating_sub(keep);
    let mut widths: Option<(u64, Widths)> = None;
    let matches = pending
        .iter()
        .skip(excess)
        .map(|&(row, line, first, count)| {
            let (col, len) = if line.is_ascii() {
                // One byte, one char, one cell.
                (u16::try_from(first).unwrap_or(u16::MAX), u16::try_from(count).unwrap_or(1).max(1))
            } else {
                let w = match &widths {
                    Some((at, w)) if *at == row => w,
                    _ => &widths.insert((row, Widths::new(line))).1,
                };
                w.span(first, count)
            };
            SearchMatch { line: LineIndex(row), col, len }
        })
        .collect();
    Scanned { rows, matches }
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

    /// Column and cell length of `count` chars starting at char `first`.
    fn span(&self, first: usize, count: usize) -> (u16, u16) {
        let last = self.starts.len().saturating_sub(1);
        let start = self.starts.get(first.min(last)).copied().unwrap_or(0);
        let end = self.starts.get(first.saturating_add(count).min(last)).copied().unwrap_or(start);
        (start, end.saturating_sub(start).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(line: u64, col: u16, len: u16) -> SearchMatch {
        SearchMatch { line: LineIndex(line), col, len }
    }

    fn find(text: &str, needle: &str, base: LineIndex, max: u32) -> Found {
        super::find(text, &Pattern::new(needle, false).expect("plain"), base, max)
    }

    fn find_re(text: &str, needle: &str, base: LineIndex, max: u32) -> Found {
        super::find(text, &Pattern::new(needle, true).expect("regex"), base, max)
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
