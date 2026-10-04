//! A file tile's find and replace over its text, as plain functions.
//!
//! The matches of a query ([`crate::kit::find::Query`]), and what a replace makes of one match
//! or all of them, `$1` and `${name}` expanded for a regular expression.

use std::ops::Range;

use regex::Regex;

/// Matches a find tints and steps through at most; past it the count says so with a `+`.
pub const MATCHES_MAX: usize = 10_000;

/// The byte ranges `matcher` finds in `text`, in order, at most [`MATCHES_MAX`]; an empty
/// match is kept (a `^` marks every line start, for a replace to put something there).
#[must_use]
pub fn matches(text: &str, matcher: &Regex) -> Vec<Range<usize>> {
    matcher.find_iter(text).take(MATCHES_MAX).map(|m| m.range()).collect()
}

/// What the match at `at` becomes: `with` as typed, or with its groups expanded for a regular
/// expression. None when `at` is no longer a match of `matcher` in `text`.
#[must_use]
pub fn replacement(
    text: &str,
    matcher: &Regex,
    at: &Range<usize>,
    with: &str,
    expand: bool,
) -> Option<String> {
    let caps = matcher.captures_at(text, at.start)?;
    let whole = caps.get(0)?;
    if whole.range() != *at {
        return None;
    }
    if !expand {
        return Some(with.to_owned());
    }
    let mut out = String::new();
    caps.expand(with, &mut out);
    Some(out)
}

/// Every match replaced: the span from the first match to the end of the last, what it
/// becomes, and how many were replaced. None when nothing matches.
///
/// One span, so the editor takes the whole replace as one edit and one undo step, and what
/// lies outside it keeps its colours.
#[must_use]
pub fn replace_all(
    text: &str,
    matcher: &Regex,
    with: &str,
    expand: bool,
) -> Option<(Range<usize>, String, usize)> {
    let mut out = String::new();
    let (mut start, mut last, mut count) = (None, 0_usize, 0_usize);
    for caps in matcher.captures_iter(text) {
        let Some(whole) = caps.get(0) else { continue };
        let from = *start.get_or_insert_with(|| whole.start());
        out.push_str(text.get(last.max(from)..whole.start()).unwrap_or_default());
        if expand {
            caps.expand(with, &mut out);
        } else {
            out.push_str(with);
        }
        last = whole.end();
        count = count.saturating_add(1);
    }
    start.map(|from| (from..last, out, count))
}

/// The lines (0-based) the matches start on, each once, for a caller that counts lines.
#[must_use]
pub fn rows(text: &str, matches: &[Range<usize>]) -> Vec<usize> {
    let (mut rows, mut row, mut counted) = (Vec::new(), 0_usize, 0_usize);
    for m in matches {
        let between = text.get(counted..m.start).unwrap_or_default();
        row = row.saturating_add(between.matches('\n').count());
        counted = m.start;
        if rows.last() != Some(&row) {
            rows.push(row);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::find::Query;

    fn query(needle: &str) -> Query {
        Query { needle: needle.to_owned(), ..Query::default() }
    }

    fn found<'a>(text: &'a str, q: &Query) -> Vec<&'a str> {
        let Ok(Some(re)) = q.matcher() else { return Vec::new() };
        matches(text, &re).into_iter().filter_map(|r| text.get(r)).collect::<Vec<_>>()
    }

    #[test]
    fn a_query_matches_text_words_and_patterns_with_smart_case() {
        let text = "Foo foo food (foo)";
        assert_eq!(found(text, &query("foo")), ["Foo", "foo", "foo", "foo"], "any case");
        assert_eq!(found(text, &query("Foo")), ["Foo"], "a capital: as typed");
        let exact = Query { match_case: true, ..query("foo") };
        assert_eq!(found(text, &exact), ["foo", "foo", "foo"]);
        let words = Query { whole_word: true, ..query("foo") };
        assert_eq!(found(text, &words), ["Foo", "foo", "foo"], "not inside food");
        assert_eq!(found(text, &query("(foo)")), ["(foo)"], "text is text, not a pattern");
        let pattern = Query { regex: true, ..query("f(o+)d?") };
        assert_eq!(found(text, &pattern), ["Foo", "foo", "food", "foo"]);
        let lines = Query { regex: true, ..query("^b") };
        assert_eq!(found("ab\nbc\nb", &lines), ["b", "b"], "^ is a line's start");
        assert!(matches!(query("").matcher(), Ok(None)));
        let bad = Query { regex: true, ..query("(") };
        assert!(bad.matcher().is_err_and(|e| e.starts_with("error:")), "{:?}", bad.matcher());
    }

    #[test]
    fn a_replace_expands_groups_only_for_a_pattern() -> Result<(), String> {
        let text = "let a = 1; let b = 22;";
        let q = Query { regex: true, ..query(r"let (\w) = (\d+)") };
        let re = q.matcher()?.ok_or("an empty needle")?;
        let first = matches(text, &re).into_iter().next().unwrap_or_default();
        assert_eq!(replacement(text, &re, &first, "$1: $2", true).as_deref(), Some("a: 1"));
        assert_eq!(replacement(text, &re, &first, "$1", false).as_deref(), Some("$1"));
        assert_eq!(replacement(text, &re, &(1..3), "x", true), None, "no longer a match");
        let all = replace_all(text, &re, "${1}=$2", true);
        assert_eq!(all, Some((0..21, "a=1; b=22".to_owned(), 2)), "one span, first to last");
        assert_eq!(replace_all("zzz", &re, "x", true), None);
        Ok(())
    }

    #[test]
    fn matches_are_counted_by_line_once_each() {
        let text = "a a\nb\na";
        let Ok(Some(re)) = query("a").matcher() else { return };
        assert_eq!(rows(text, &matches(text, &re)), [0, 2]);
    }
}
