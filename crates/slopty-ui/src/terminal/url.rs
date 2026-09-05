//! Links in terminal text: the link under a cell, found on the client from the line cache.
//!
//! An OSC 8 run the program marked wins (`Line::links`, filled by the host engine). Most
//! programs do not emit OSC 8, so otherwise the row is joined column by column (a wide cluster
//! fills its first column, the spacer contributes nothing) so a cell column maps to a byte
//! offset, then the longest run around that offset that starts with a known scheme and ends
//! at whitespace or a quote is the link, minus the punctuation prose puts after a URL.

use std::ops::Range;

use slopty_grid::Line;

/// A link under a cell: the columns it covers on its line and where it goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkSpan {
    /// First column.
    pub start: u16,
    /// One past the last column.
    pub end: u16,
    /// Target.
    pub url: String,
}

/// The link covering column `col` of `line`: the OSC 8 run when the program marked one, else
/// the URL found in the row's text.
#[must_use]
pub fn link_at_col(line: &Line, col: u16) -> Option<LinkSpan> {
    if let Some(link) = line.link_at(col) {
        return Some(LinkSpan { start: link.col, end: link.end(), url: link.uri.clone() });
    }
    text_link_at_col(line, col)
}

/// Schemes worth opening. `mailto:` has no `//`.
const SCHEMES: &[&str] =
    &["https://", "http://", "file://", "ssh://", "git://", "ftp://", "mailto:"];

/// The plain-text URL covering column `col` of `line`, if any, with the columns it spans.
#[must_use]
pub fn text_link_at_col(line: &Line, col: u16) -> Option<LinkSpan> {
    let mut text = String::new();
    let mut offset = None;
    // Byte offset where each text-drawing column starts, with its column span.
    let mut starts: Vec<(u16, u16, usize)> = Vec::with_capacity(line.cells.len());
    for (i, cell) in line.cells.iter().enumerate() {
        let i = u16::try_from(i).unwrap_or(u16::MAX);
        if i == col {
            offset = Some(text.len());
        }
        if cell.width.draws_text() {
            starts.push((i, cell.width.columns(), text.len()));
            text.push_str(if cell.text.is_empty() { " " } else { cell.text.as_str() });
        }
    }
    let range = url_range_at(&text, offset?)?;
    let url = text.get(range.clone())?.to_owned();
    let mut covered = starts.iter().filter(|&&(_, _, at)| range.contains(&at));
    let (start, span, _) = *covered.next()?;
    let (last, last_span, _) = covered.next_back().copied().unwrap_or((start, span, 0));
    Some(LinkSpan { start, end: last.saturating_add(last_span), url })
}

/// The URL in `text` that covers byte `offset`.
#[must_use]
pub fn url_at(text: &str, offset: usize) -> Option<&str> {
    text.get(url_range_at(text, offset)?)
}

/// Byte range of the URL in `text` that covers byte `offset`.
fn url_range_at(text: &str, offset: usize) -> Option<Range<usize>> {
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
    let from = start.saturating_add(scheme_at);
    (trimmed.len() > scheme_len).then(|| from..from.saturating_add(trimmed.len()))
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
    use slopty_grid::{Cell, Hyperlink, Style};

    use super::*;

    #[test]
    fn text_links_come_with_their_columns() {
        let mut line = Line::from_text("go https://a.b/字x now", 24, Style::DEFAULT);
        // Make 字 a real wide cell followed by its spacer, shifting "x" one column right.
        line.cells[15] = Cell::wide("字", Style::DEFAULT);
        line.cells[16] = Cell::spacer_tail(Style::DEFAULT);
        line.cells[17] = Cell::narrow('x', Style::DEFAULT);
        line.cells[18] = Cell::narrow(' ', Style::DEFAULT);
        let span = text_link_at_col(&line, 5).unwrap();
        assert_eq!(span.url, "https://a.b/字x");
        assert_eq!((span.start, span.end), (3, 18), "the wide cell's spacer is inside the span");
        assert_eq!(text_link_at_col(&line, 2), None);
        assert_eq!(text_link_at_col(&line, 19), None);
    }

    #[test]
    fn osc8_runs_win_over_the_text_scan() {
        let mut line = Line::from_text("docs https://x.y", 20, Style::DEFAULT);
        line.links.push(Hyperlink { col: 0, len: 4, uri: "https://real/".to_owned() });
        let span = link_at_col(&line, 1).unwrap();
        assert_eq!((span.start, span.end, span.url.as_str()), (0, 4, "https://real/"));
        let span = link_at_col(&line, 7).unwrap();
        assert_eq!((span.start, span.end, span.url.as_str()), (5, 16, "https://x.y"));
    }

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
