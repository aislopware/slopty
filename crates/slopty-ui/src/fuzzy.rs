//! Fuzzy matching for the palette, the window picker and a file's symbols: nucleo's scorer
//! (fzf's, as Helix runs it), so "nwt" finds "New terminal".
//!
//! Each word of the query is matched on its own, in any case, and every word must match, in any
//! order. A line is scored on its name apart from where it is (its worker, its folder, what its
//! agent was asked): a word matches the name fuzzily, but the place only as it is spelled, since
//! scattered letters find something in any long text. A word in the name outranks one only in
//! the place, and only the name's matched characters are highlighted. A name the query spells
//! whole leads. The ideas of splitting the name from the
//! folder and of ordering groups by their best line are Ely's (`navigation/palette/kinds.rs`,
//! Copyright (c) 2026 Ely GPUI Component contributors, MIT OR Apache-2.0); the scoring is
//! nucleo's rather than Ely's own.

use std::cmp::Ordering;
use std::ops::Range;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// How well a query names a line. Greater is better: a name spelled whole, then how many of
/// the query's words are in the name rather than only the place, then nucleo's score.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Rank {
    whole: bool,
    in_name: usize,
    score: u32,
}

/// A query, ready to match lines against.
pub struct Fuzzy {
    pattern: Pattern,
    /// The same words, each to be found as spelled: what the place is matched with.
    spelled: Pattern,
    /// The query as typed, lowercase and its words single-spaced: what a whole name is.
    whole: String,
    matcher: Matcher,
    name: Vec<char>,
    place: Vec<char>,
}

impl std::fmt::Debug for Fuzzy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fuzzy").field("whole", &self.whole).finish_non_exhaustive()
    }
}

impl Fuzzy {
    /// `query`'s words, in any case, accents folded. No word means everything matches.
    #[must_use]
    pub fn new(query: &str) -> Self {
        let words = |kind| Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, kind);
        let (pattern, spelled) = (words(AtomKind::Fuzzy), words(AtomKind::Substring));
        let whole = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        Self {
            pattern,
            spelled,
            whole,
            matcher: Matcher::new(Config::DEFAULT),
            name: Vec::new(),
            place: Vec::new(),
        }
    }

    /// Whether the query has no word.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pattern.atoms.is_empty()
    }

    /// How well the query names a line called `name`, found at `place`; `None` when some word
    /// is in neither.
    pub fn rank(&mut self, name: &str, place: &str) -> Option<Rank> {
        let Self { pattern, spelled, whole, matcher, name: name_buf, place: place_buf } = self;
        let whole = !whole.is_empty() && name.to_lowercase() == *whole;
        let mut rank = Rank { whole, ..Rank::default() };
        for (atom, as_spelled) in pattern.atoms.iter().zip(&spelled.atoms) {
            let in_name = atom.score(Utf32Str::new(name, name_buf), matcher);
            let score = match in_name {
                Some(score) => {
                    rank.in_name = rank.in_name.saturating_add(1);
                    score
                }
                None => as_spelled.score(Utf32Str::new(place, place_buf), matcher)?,
            };
            rank.score = rank.score.saturating_add(u32::from(score));
        }
        Some(rank)
    }

    /// Whether every word is in `text`.
    pub fn matches(&mut self, text: &str) -> bool {
        self.rank(text, "").is_some()
    }

    /// The characters of `name` the query's words matched, as byte ranges, in order and
    /// merged where they touch: what a row highlights.
    pub fn ranges(&mut self, name: &str) -> Vec<Range<usize>> {
        let Self { pattern, matcher, name: buf, .. } = self;
        let mut chars: Vec<u32> = Vec::new();
        for atom in &pattern.atoms {
            // A word found only in the place marks nothing in the name.
            let mut found = Vec::new();
            if atom.indices(Utf32Str::new(name, buf), matcher, &mut found).is_some() {
                chars.extend(found);
            }
        }
        chars.sort_unstable();
        chars.dedup();
        let starts: Vec<(usize, char)> = name.char_indices().collect();
        let mut out: Vec<Range<usize>> = Vec::with_capacity(chars.len());
        for ix in chars {
            let Some(&(at, c)) = usize::try_from(ix).ok().and_then(|ix| starts.get(ix)) else {
                continue;
            };
            let end = at.saturating_add(c.len_utf8());
            match out.last_mut() {
                Some(last) if last.end == at => last.end = end,
                _ => out.push(at..end),
            }
        }
        out
    }
}

/// Better first: what [`Rank`]'s order says, for a sort.
#[must_use]
pub fn best_first(a: &Rank, b: &Rank) -> Ordering {
    b.cmp(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked<'a>(query: &str, names: &[&'a str]) -> Vec<&'a str> {
        let mut fuzzy = Fuzzy::new(query);
        let mut kept: Vec<(Rank, &str)> =
            names.iter().filter_map(|n| Some((fuzzy.rank(n, "")?, *n))).collect();
        kept.sort_by(|a, b| best_first(&a.0, &b.0));
        kept.into_iter().map(|(_, n)| n).collect()
    }

    /// Letters in order find a line by its words' heads: "nwt" is "New terminal".
    #[test]
    fn letters_in_order_find_a_line() {
        let names = ["New note", "New terminal", "Next tab", "Move column left"];
        assert_eq!(ranked("nwt", &names).first(), Some(&"New terminal"));
        assert_eq!(ranked("mcl", &names), ["Move column left"]);
        assert_eq!(ranked("zzz", &names), Vec::<&str>::new());
        assert_eq!(ranked("", &names).len(), 4, "no word keeps everything");
    }

    /// Every word must match, in any order and any case.
    #[test]
    fn every_word_matches_in_any_order() {
        let names = ["Move column left", "Move column right"];
        assert_eq!(ranked("right col", &names), ["Move column right"]);
        assert_eq!(ranked("RIGHT", &names), ["Move column right"]);
    }

    /// The place matches only as spelled: scattered letters in a long text find nothing.
    #[test]
    fn the_place_matches_as_spelled() {
        let mut fuzzy = Fuzzy::new("login redirect");
        let asked = "fix the build, then look at the long reply it gave in detail";
        assert!(fuzzy.rank("zsh", asked).is_none(), "letters scattered over the place");
        assert!(fuzzy.rank("zsh", "the login redirect loops").is_some());
        assert!(Fuzzy::new("lgn").rank("zsh", "login").is_none());
    }

    /// A name spelled whole leads, then a word in the name beats one only in the place.
    #[test]
    fn the_name_outranks_the_place() {
        let mut fuzzy = Fuzzy::new("studio");
        let in_place = fuzzy.rank("zsh", "studio · ~/src").expect("in its place");
        let in_name = fuzzy.rank("studio logs", "laptop").expect("in its name");
        let whole = fuzzy.rank("Studio", "").expect("whole");
        assert!(in_name > in_place, "{in_name:?} {in_place:?}");
        assert!(whole > in_name, "{whole:?} {in_name:?}");
        assert!(fuzzy.rank("zsh", "laptop").is_none());
    }

    /// The highlight marks the name's matched characters as byte ranges, merged where they
    /// touch, and nothing for a word found only in the place.
    #[test]
    fn the_highlight_marks_the_names_letters() {
        let mut fuzzy = Fuzzy::new("nwt");
        assert_eq!(fuzzy.ranges("New terminal"), [0..1, 2..3, 4..5]);
        let mut fuzzy = Fuzzy::new("page");
        assert_eq!(ends(&fuzzy.ranges("Edit Page address")), [(5, 9)]);
        let mut fuzzy = Fuzzy::new("straße");
        assert_eq!(ends(&fuzzy.ranges("Straße")), [(0, 7)], "a two-byte letter inside");
        let mut fuzzy = Fuzzy::new("studio");
        assert_eq!(ends(&fuzzy.ranges("zsh")), Vec::<(usize, usize)>::new());
    }

    /// Each range as its two ends.
    fn ends(ranges: &[Range<usize>]) -> Vec<(usize, usize)> {
        ranges.iter().map(|r| (r.start, r.end)).collect()
    }
}
