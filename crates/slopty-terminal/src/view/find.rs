//! What a terminal's find asks the worker for and what its bar says back: the query with its
//! toggles, written as one regular expression, and the tally.
//!
//! After Ely GPUI Components (`src/editor/find.rs`), Copyright (c) 2026 Ely GPUI Component
//! contributors, MIT OR Apache-2.0. The bar itself is drawn with the shell's parts
//! ([`super::TerminalView::render_search`]).

/// What the find field says before anything is typed.
pub(super) const PLACEHOLDER: &str = "Find";
/// The case toggle's name, as a screen reader and the hint say it.
pub(super) const MATCH_CASE: &str = "Match case";
/// The whole-word toggle's name.
pub(super) const WHOLE_WORD: &str = "Match whole word";
/// The pattern toggle's name.
pub(super) const REGEX: &str = "Use regular expression";
/// Match case's face: the letters it tells apart.
pub(super) const CASE_FACE: &str = "Aa";
/// Whole word's face.
pub(super) const WORD_FACE: &str = "W";
/// A pattern's face: what a regular expression is written with.
pub(super) const REGEX_FACE: &str = ".*";
/// What the tally says when nothing matches.
pub(super) const NO_MATCHES: &str = "No matches";
/// What the tally says for a pattern that does not compile; the pattern's own word is its
/// accessible value.
pub(super) const BAD_PATTERN: &str = "Not a valid pattern";
/// The bar's width: room for a word or two, the toggles, the tally and the steps.
pub(super) const WIDTH: f32 = 380.0;

/// What a find asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Query {
    /// What was typed.
    needle: String,
    /// Case as typed. Off is smart case: a needle with no capital matches in any case, one
    /// with a capital as typed.
    match_case: bool,
    /// Only where the match starts and ends at a word's edge.
    whole_word: bool,
    /// The needle is a regular expression (Rust's `regex` syntax).
    regex: bool,
}

impl Query {
    /// `needle` with these toggles.
    #[must_use]
    pub(super) fn with_needle(&self, needle: impl Into<String>) -> Self {
        Self { needle: needle.into(), ..self.clone() }
    }

    /// What was typed.
    #[must_use]
    pub(super) fn needle(&self) -> &str {
        &self.needle
    }

    /// Whether `toggle` is on.
    #[must_use]
    pub(super) const fn is_on(&self, toggle: Toggle) -> bool {
        match toggle {
            Toggle::MatchCase => self.match_case,
            Toggle::WholeWord => self.whole_word,
            Toggle::Regex => self.regex,
        }
    }

    /// The query as one regular expression in Rust's syntax, its case rule written in as a
    /// flag, so the worker searching the history matches as the bar says whatever its own
    /// default. `None` for an empty needle.
    #[must_use]
    pub(super) fn source(&self) -> Option<String> {
        if self.needle.is_empty() {
            return None;
        }
        let pattern = if self.regex { self.needle.clone() } else { regex::escape(&self.needle) };
        let pattern = if self.whole_word { format!(r"\b(?:{pattern})\b") } else { pattern };
        let exact = self.match_case || self.needle.chars().any(char::is_uppercase);
        Some(format!("(?{}){pattern}", if exact { "m-i" } else { "im" }))
    }

    /// Flip one of its toggles.
    pub(super) const fn flip(&mut self, toggle: Toggle) {
        let on = match toggle {
            Toggle::MatchCase => &mut self.match_case,
            Toggle::WholeWord => &mut self.whole_word,
            Toggle::Regex => &mut self.regex,
        };
        *on = !*on;
    }
}

/// One of a query's toggles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Toggle {
    /// [`Query::match_case`].
    MatchCase,
    /// [`Query::whole_word`].
    WholeWord,
    /// [`Query::regex`].
    Regex,
}

/// What the tally says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Tally {
    /// Nothing typed.
    Quiet,
    /// The pattern does not compile: the pattern's own word on why.
    Bad(String),
    /// The matches: the one on show (from 0) among `total`, `more` past a cap.
    Found {
        /// The match on show.
        at: Option<usize>,
        /// How many.
        total: usize,
        /// There are more than `total`.
        more: bool,
    },
}

impl Tally {
    /// The tally as the bar shows it.
    #[must_use]
    pub(super) fn words(&self) -> String {
        match self {
            Self::Quiet => String::new(),
            Self::Bad(_) => BAD_PATTERN.to_owned(),
            Self::Found { total: 0, .. } => NO_MATCHES.to_owned(),
            Self::Found { at: Some(at), total, more } => {
                format!("{} of {total}{}", at.saturating_add(1), if *more { "+" } else { "" })
            }
            Self::Found { at: None, total: 1, more: false } => "1 match".to_owned(),
            Self::Found { at: None, total, more } => {
                format!("{total}{} matches", if *more { "+" } else { "" })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use regex::RegexBuilder;

    use super::*;

    fn query(needle: &str) -> Query {
        Query { needle: needle.to_owned(), ..Query::default() }
    }

    /// The source carries its case rule, so a matcher built case-blind elsewhere still
    /// matches as the bar says.
    #[test]
    fn the_source_carries_its_case() {
        let exact = Query { match_case: true, whole_word: true, ..query("a.b") };
        let source = exact.source().unwrap();
        let blind = RegexBuilder::new(&source).case_insensitive(true).build().unwrap();
        assert!(blind.is_match("x a.b y") && !blind.is_match("A.B") && !blind.is_match("a.bc"));
        let any = query("ab").source().unwrap();
        let strict = RegexBuilder::new(&any).build().unwrap();
        assert!(strict.is_match("AB"));
    }

    #[test]
    fn the_tally_reads_one_way() {
        assert_eq!(Tally::Quiet.words(), "");
        assert_eq!(Tally::Found { at: Some(1), total: 4, more: false }.words(), "2 of 4");
        assert_eq!(Tally::Found { at: Some(0), total: 10, more: true }.words(), "1 of 10+");
        assert_eq!(Tally::Found { at: None, total: 0, more: false }.words(), NO_MATCHES);
        assert_eq!(Tally::Found { at: None, total: 1, more: false }.words(), "1 match");
        assert_eq!(Tally::Found { at: None, total: 3, more: false }.words(), "3 matches");
        assert_eq!(Tally::Bad("error: x".into()).words(), BAD_PATTERN);
    }
}
