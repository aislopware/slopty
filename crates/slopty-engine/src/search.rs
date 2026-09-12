//! Text search over the plain-text rendering of the grid.
//!
//! The engine formats the retained history plus the screen as plain text (one line per row,
//! trailing blanks trimmed) and this module finds the needle in it. Columns are cells, so a
//! hit can be painted straight onto the grid: cluster widths come from libghostty's own
//! tables, the same ones that laid the cells out.

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
    let keep = usize::try_from(max).unwrap_or(usize::MAX);
    // `(row, line, first char, char count)` of the hits still in the running.
    let mut pending: Vec<(usize, &str, usize, usize)> = Vec::new();
    let mut total: u32 = 0;
    for (row, line) in text.split('\n').enumerate() {
        let found = pattern.hits(line);
        if found.is_empty() {
            continue;
        }
        total = total.saturating_add(u32::try_from(found.len()).unwrap_or(u32::MAX));
        pending.extend(found.into_iter().map(|(first, count)| (row, line, first, count)));
        // Older hits than the newest `keep` are never reported: forget them in batches.
        if pending.len() > keep.saturating_mul(2) {
            let excess = pending.len().saturating_sub(keep);
            pending.drain(..excess);
        }
    }
    let excess = pending.len().saturating_sub(keep);
    let mut widths: Option<(usize, Widths)> = None;
    let matches = pending
        .iter()
        .skip(excess)
        .map(|&(row, line, first, count)| {
            let index = LineIndex(base.0.saturating_add(u64::try_from(row).unwrap_or(u64::MAX)));
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
            SearchMatch { line: index, col, len }
        })
        .collect();
    Found { total, matches }
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
