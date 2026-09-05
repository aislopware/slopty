//! Links in terminal text: the URL under a cell, found on the client from the line cache.
//!
//! Programs rarely emit OSC 8, so the link a user wants is usually plain text. The row is
//! joined column by column (a wide cluster fills its first column, the spacer contributes
//! nothing) so a cell column maps to a byte offset, then the longest run around that offset
//! that starts with a known scheme and ends at whitespace or a quote is the link, minus the
//! punctuation prose puts after a URL.

use slopty_grid::Line;

/// Schemes worth opening. `mailto:` has no `//`.
const SCHEMES: &[&str] =
    &["https://", "http://", "file://", "ssh://", "git://", "ftp://", "mailto:"];

/// The URL covering column `col` of `line`, if any.
#[must_use]
pub fn url_at_col(line: &Line, col: u16) -> Option<String> {
    let mut text = String::new();
    let mut offset = None;
    for (i, cell) in line.cells.iter().enumerate() {
        if i == usize::from(col) {
            offset = Some(text.len());
        }
        if cell.width.draws_text() {
            text.push_str(if cell.text.is_empty() { " " } else { cell.text.as_str() });
        }
    }
    let offset = offset?;
    url_at(&text, offset).map(str::to_owned)
}

/// The URL in `text` that covers byte `offset`.
#[must_use]
pub fn url_at(text: &str, offset: usize) -> Option<&str> {
    if offset >= text.len() {
        return None;
    }
    // The run of non-delimiter bytes around the offset.
    let is_delim = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`');
    let start = text
        .get(..offset)?
        .char_indices()
        .rev()
        .find(|&(_, c)| is_delim(c))
        .map_or(0, |(i, c)| i.saturating_add(c.len_utf8()));
    let end = text
        .get(offset..)?
        .char_indices()
        .find(|&(_, c)| is_delim(c))
        .map_or(text.len(), |(i, _)| offset.saturating_add(i));
    let run = text.get(start..end)?;
    // The scheme may sit after a bracket or a parenthesis: "(https://x)" → "https://x".
    let (scheme_at, _) = SCHEMES
        .iter()
        .filter_map(|scheme| run.find(scheme).map(|at| (at, *scheme)))
        .min_by_key(|&(at, _)| at)?;
    let candidate = run.get(scheme_at..)?;
    if scheme_at.saturating_add(start) > offset {
        return None;
    }
    let trimmed = trim_trailing(candidate);
    let scheme_len = SCHEMES.iter().find(|s| trimmed.starts_with(*s)).map_or(0, |s| s.len());
    (trimmed.len() > scheme_len).then_some(trimmed)
}

/// Drop the punctuation prose leaves after a URL and any closing bracket without an opener.
fn trim_trailing(mut s: &str) -> &str {
    loop {
        let Some(last) = s.chars().next_back() else { return s };
        let cut = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            '}' => s.matches('{').count() < s.matches('}').count(),
            _ => false,
        };
        if !cut {
            return s;
        }
        s = &s[..s.len().saturating_sub(last.len_utf8())];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_url_around_the_offset() {
        let t = "see https://example.com/a?b=1 now";
        assert_eq!(url_at(t, 4), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(t, 20), Some("https://example.com/a?b=1"));
        assert_eq!(url_at(t, 0), None);
        assert_eq!(url_at(t, 30), None);
    }

    #[test]
    fn strips_prose_punctuation_and_unbalanced_brackets() {
        assert_eq!(url_at("(https://a.b/c).", 3), Some("https://a.b/c"));
        assert_eq!(
            url_at("https://en.wikipedia.org/wiki/Rust_(x)", 5),
            Some("https://en.wikipedia.org/wiki/Rust_(x)")
        );
        assert_eq!(url_at("<https://a.b>", 3), Some("https://a.b"));
        assert_eq!(url_at("\"https://a.b/\",", 3), Some("https://a.b/"));
    }

    #[test]
    fn bare_scheme_and_prefix_before_scheme_do_not_count() {
        assert_eq!(url_at("https://", 2), None);
        assert_eq!(url_at("x=https://a.b", 0), None);
        assert_eq!(url_at("x=https://a.b", 4), Some("https://a.b"));
        assert_eq!(url_at("mailto:a@b.c", 3), Some("mailto:a@b.c"));
    }
}
