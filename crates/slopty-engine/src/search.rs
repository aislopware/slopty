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

/// What to look for: plain text with smart case, or a regular expression.
#[derive(Clone, Debug)]
pub enum Pattern {
    /// Substring; `folded` is the needle lower-cased when the search is case-insensitive.
    Plain {
        /// The needle, lower-cased when `insensitive`.
        needle: String,
        /// Match case-insensitively (the needle had no upper-case letter).
        insensitive: bool,
    },
    /// A compiled regex (smart case folded into the pattern).
    Regex(regex::Regex),
}

impl Pattern {
    /// Build a pattern; a regex that does not compile is the error message.
    pub fn new(needle: &str, regex: bool) -> Result<Self, String> {
        let insensitive = !needle.chars().any(char::is_uppercase);
        if regex {
            let source = if insensitive { format!("(?i){needle}") } else { needle.to_owned() };
            return regex::RegexBuilder::new(&source)
                .size_limit(1 << 20)
                .build()
                .map(Self::Regex)
                .map_err(|e| e.to_string());
        }
        let needle = if insensitive { fold(needle) } else { needle.to_owned() };
        Ok(Self::Plain { needle, insensitive })
    }

    /// Whether the pattern can match anything at all.
    fn is_empty(&self) -> bool {
        match self {
            Self::Plain { needle, .. } => needle.is_empty(),
            Self::Regex(re) => re.as_str().is_empty(),
        }
    }

    /// Hits in one line as `(first char, char count)`; empty matches are skipped.
    fn hits(&self, line: &str) -> Vec<(usize, usize)> {
        match self {
            Self::Plain { needle, insensitive } => {
                let folded: String;
                let haystack = if *insensitive {
                    folded = fold(line);
                    &folded
                } else {
                    line
                };
                if !haystack.contains(&**needle) {
                    return Vec::new();
                }
                let count = needle.chars().count();
                haystack
                    .match_indices(&**needle)
                    .map(|(byte, _)| (char_index(haystack, byte), count))
                    .collect()
            }
            Self::Regex(re) => re
                .find_iter(line)
                .filter(|m| !m.as_str().is_empty())
                .map(|m| (char_index(line, m.start()), m.as_str().chars().count()))
                .collect(),
        }
    }
}

/// Chars before byte offset `byte`.
fn char_index(s: &str, byte: usize) -> usize {
    s.get(..byte).map_or(0, |head| head.chars().count())
}

/// Find `pattern` in `text`, whose first line is absolute line `base`.
///
/// Smart case: a needle without an upper-case letter matches case-insensitively. Hits never
/// span rows (soft-wrapped lines are searched row by row).
#[must_use]
pub fn find(text: &str, pattern: &Pattern, base: LineIndex, max: u32) -> Found {
    if pattern.is_empty() {
        return Found::default();
    }
    let mut hits: Vec<SearchMatch> = Vec::new();
    let mut total: u32 = 0;
    let keep = usize::try_from(max).unwrap_or(usize::MAX);
    for (row, line) in text.split('\n').enumerate() {
        let found = pattern.hits(line);
        if found.is_empty() {
            continue;
        }
        let index = LineIndex(base.0.saturating_add(u64::try_from(row).unwrap_or(u64::MAX)));
        let widths = Widths::new(line);
        for (first, count) in found {
            let (col, len) = widths.span(first, count);
            total = total.saturating_add(1);
            hits.push(SearchMatch { line: index, col, len });
        }
    }
    let excess = hits.len().saturating_sub(keep);
    if excess > 0 {
        hits.drain(..excess);
    }
    Found { total, matches: hits }
}

/// Lower-case a line character by character so char indices line up with the original.
fn fold(s: &str) -> String {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
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
        assert!(Pattern::new("(", true).is_err());
        assert!(Pattern::new("(", false).is_ok());
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

    #[test]
    fn wide_characters_count_two_cells() {
        // 日本 is two wide cells each; "x" starts at column 4.
        let found = find("日本x", "x", LineIndex(0), 10);
        assert_eq!(found.matches, vec![m(0, 4, 1)]);
        let found = find("日本x", "本x", LineIndex(0), 10);
        assert_eq!(found.matches, vec![m(0, 2, 3)]);
    }

    #[test]
    fn empty_needle_finds_nothing() {
        assert_eq!(find("anything", "", LineIndex(0), 10), Found::default());
    }
}
