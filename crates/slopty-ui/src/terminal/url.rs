//! Links in terminal text: the link under a cell, found on the client from the line cache.
//!
//! An OSC 8 run the program marked wins (`Line::links`, filled by the worker engine). Most
//! programs do not emit OSC 8, so otherwise the logical line (the row and the rows soft-wrapped
//! onto it) is joined column by column (a wide cluster fills its first column, the spacer
//! contributes nothing) so a cell maps to a byte offset, then the longest run around that
//! offset that starts with a known scheme and ends at whitespace or a quote is the link, minus
//! the punctuation prose puts after a URL. A link the terminal wrapped is one link.

use std::ops::Range;

use slopty_grid::Line;

/// A cell of a logical line: the row (0 = its first) and the column.
pub type Cell = (usize, u16);

/// A link under a cell: the cells it covers and where it goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkSpan {
    /// First cell.
    pub start: Cell,
    /// One past the last cell, on the last cell's row.
    pub end: Cell,
    /// Target.
    pub url: String,
}

/// The link covering `at` in `rows` (a logical line, top to bottom): the OSC 8 run when the
/// program marked one, else the URL found in the text.
#[must_use]
pub fn link_at(rows: &[&Line], at: Cell) -> Option<LinkSpan> {
    osc8_at(rows, at).or_else(|| text_link_at(rows, at))
}

/// The OSC 8 run under `at`, carried on over the rows it wraps onto: a run that ends at the
/// right edge and one with the same target at the next row's start are one link.
fn osc8_at(rows: &[&Line], (row, col): Cell) -> Option<LinkSpan> {
    let link = rows.get(row)?.link_at(col)?;
    let same = |line: &Line, col: u16| line.link_at(col).filter(|l| l.uri == link.uri).cloned();
    let (mut start, mut first) = ((row, link.col), link.clone());
    while first.col == 0
        && let Some(above) = start.0.checked_sub(1)
        && let Some(line) = rows.get(above)
        && let Some(run) = line.cols().checked_sub(1).and_then(|last| same(line, last))
    {
        start = (above, run.col);
        first = run;
    }
    let (mut end, mut last) = ((row, link.end()), link.clone());
    while rows.get(end.0).is_some_and(|line| last.end() >= line.cols())
        && let Some(line) = rows.get(end.0.saturating_add(1))
        && let Some(run) = same(line, 0)
    {
        end = (end.0.saturating_add(1), run.end());
        last = run;
    }
    Some(LinkSpan { start, end, url: link.uri.clone() })
}

/// Schemes worth opening. `mailto:` has no `//`.
const SCHEMES: &[&str] =
    &["https://", "http://", "file://", "ssh://", "git://", "ftp://", "mailto:"];

/// The plain-text URL covering `at` in `rows`, if any, with the cells it spans.
fn text_link_at(rows: &[&Line], at: Cell) -> Option<LinkSpan> {
    let (text, starts, offset) = joined(rows, at);
    let range = url_range_at(&text, offset?)?;
    let url = text.get(range.clone())?.to_owned();
    let (start, end) = cells_of(&starts, &range)?;
    Some(LinkSpan { start, end, url })
}

/// A file path under a cell, as compilers, linters and greps print them.
///
/// `src/main.rs:12:5`, `./notes.md`, `~/.zshrc`: the cells it covers, the path without its
/// position, and the line number when one followed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PathSpan {
    /// First cell.
    pub start: Cell,
    /// One past the last cell, on the last cell's row.
    pub end: Cell,
    /// The path as printed, without a trailing `:line[:col]`.
    pub path: String,
    /// The line number that followed the path, if any.
    pub line: Option<u32>,
}

/// The file path covering `at` in `rows` (a logical line), if the text there reads as one.
#[must_use]
pub fn path_at(rows: &[&Line], at: Cell) -> Option<PathSpan> {
    let (text, starts, offset) = joined(rows, at);
    let (range, path, line_no) = path_range_at(&text, offset?)?;
    let (start, end) = cells_of(&starts, &range)?;
    Some(PathSpan { start, end, path, line: line_no })
}

/// Where a text-drawing cell starts in the joined text: the cell, the columns it spans, and
/// its byte offset.
type Start = (Cell, u16, usize);

/// The rows' text joined cell by cell, where each text-drawing cell starts in it, and the
/// byte offset of `at`.
fn joined(rows: &[&Line], at: Cell) -> (String, Vec<Start>, Option<usize>) {
    let mut text = String::new();
    let mut offset = None;
    let mut starts: Vec<Start> = Vec::with_capacity(rows.iter().map(|l| l.cells.len()).sum());
    for (row, line) in rows.iter().enumerate() {
        for (col, cell) in line.cells.iter().enumerate() {
            let cell_at = (row, u16::try_from(col).unwrap_or(u16::MAX));
            if cell_at == at {
                offset = Some(text.len());
            }
            if cell.width.draws_text() {
                starts.push((cell_at, cell.width.columns(), text.len()));
                text.push_str(if cell.text.is_empty() { " " } else { cell.text.as_str() });
            }
        }
    }
    (text, starts, offset)
}

/// The first cell and one past the last of the cells whose text lies in `range`.
fn cells_of(starts: &[Start], range: &Range<usize>) -> Option<(Cell, Cell)> {
    let mut covered = starts.iter().filter(|&&(_, _, at)| range.contains(&at));
    let (start, span, _) = *covered.next()?;
    let ((row, col), span, _) = covered.next_back().copied().unwrap_or((start, span, 0));
    Some((start, (row, col.saturating_add(span))))
}

/// Extensions a bare `name.ext` is taken for a file by; a token with a `/` needs none.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "toml", "lock", "md", "txt", "json", "yaml", "yml", "ts", "tsx", "js", "jsx", "mjs",
    "py", "go", "java", "kt", "swift", "m", "mm", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "sh",
    "zsh", "fish", "zig", "sql", "proto", "html", "css", "scss", "xml", "plist", "env",
];

/// Byte range of the path in `text` that covers byte `offset`, the path itself and the line
/// number after it. A run of non-delimiter bytes is a path when it has a `/` or a known
/// extension and no URL scheme; a trailing `:line` or `:line:col` is read off, as is the
/// punctuation prose leaves after it.
fn path_range_at(text: &str, offset: usize) -> Option<(Range<usize>, String, Option<u32>)> {
    if offset >= text.len() {
        return None;
    }
    let is_delim = |c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '<' | '>' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',')
    };
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
    let run = trim_trailing(text.get(start..end)?);
    if run.contains("://") {
        return None;
    }
    // A grep hit's `path:12:text` (or `path:12:5:text`): the path and line end at the text.
    let run = grep_cut(run).and_then(|cut| run.get(..cut)).unwrap_or(run);
    // `path:12:5` → `path`, 12; `path:12` → `path`, 12; a lone `path` → no line.
    let mut path = run;
    let mut line = None;
    for _ in 0..2 {
        let Some((head, tail)) = path.rsplit_once(':') else { break };
        if tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        line = tail.parse::<u32>().ok();
        path = head;
    }
    let path = path.trim_end_matches(':');
    let extension = path.rsplit_once('.').map(|(name, ext)| (name, ext.to_ascii_lowercase()));
    let looks_like_file = path.contains('/')
        || extension.is_some_and(|(name, ext)| {
            !name.is_empty() && !name.ends_with('/') && SOURCE_EXTENSIONS.contains(&ext.as_str())
        });
    if path.is_empty() || !looks_like_file || path.chars().all(|c| c == '/' || c == '.') {
        return None;
    }
    Some((start..start.saturating_add(run.len()), path.to_owned(), line))
}

/// Where `path:12:` or `path:12:5:` ends in `run` (after the last digits, before the colon
/// that starts the text), when `run` reads as a grep hit.
fn grep_cut(run: &str) -> Option<usize> {
    let digits_at =
        |at: usize| run.get(at..).map_or(0, |r| r.bytes().take_while(u8::is_ascii_digit).count());
    let colon = run.find(':')?;
    let line = digits_at(colon.saturating_add(1));
    if line == 0 {
        return None;
    }
    let mut end = colon.saturating_add(1).saturating_add(line);
    // An optional column, kept when it is the end or another colon follows it.
    if run.get(end..).is_some_and(|r| r.starts_with(':')) {
        let col = digits_at(end.saturating_add(1));
        let after_col = end.saturating_add(1).saturating_add(col);
        if col > 0 && run.get(after_col..).is_some_and(|r| r.is_empty() || r.starts_with(':')) {
            end = after_col;
        }
    }
    // Only a hit with text after the location is cut; a bare `path:12:5` stays whole.
    match run.get(end..) {
        Some(rest) if rest.starts_with(':') && rest.len() > 1 => Some(end),
        _ => None,
    }
}

/// The first file path in `text` — a grep hit's `src/a.rs:12:…`, a compiler's
/// ` --> src/b.rs:3:5` — with its byte range and the line number after it.
#[must_use]
pub fn first_path(text: &str) -> Option<(Range<usize>, String, Option<u32>)> {
    let mut at_word_start = true;
    for (i, c) in text.char_indices() {
        let starts_word = at_word_start && !c.is_whitespace();
        at_word_start = c.is_whitespace();
        if starts_word && let Some(found) = path_range_at(text, i) {
            return Some(found);
        }
    }
    None
}

/// `s` as one word for a POSIX or fish shell: single-quoted, a quote inside spelled out.
#[must_use]
pub fn shell_word(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The command that opens `path` in the shell's editor: `$EDITOR`, else `vi`, at `line`
/// when one is known. Typed at a prompt, so the shell expands the variable itself.
#[must_use]
pub fn editor_command(path: &str, line: Option<u32>) -> String {
    let at = line.map(|l| format!("+{l} ")).unwrap_or_default();
    format!("${{EDITOR:-vi}} {at}{}", shell_word(path))
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
        s = s.strip_suffix(last).unwrap_or(s);
    }
}

#[cfg(test)]
mod tests {
    use slopty_grid::{Cell, Hyperlink, Style};

    use super::*;

    #[test]
    fn the_first_path_of_a_result_line_is_found_with_its_line() {
        let first = |t: &str| first_path(t).map(|(_, p, l)| (p, l));
        assert_eq!(first("src/a.rs:12:fn x() {"), Some(("src/a.rs".to_owned(), Some(12))));
        assert_eq!(first("  --> src/b.rs:3:5"), Some(("src/b.rs".to_owned(), Some(3))));
        assert_eq!(first("/tmp/w/note.txt"), Some(("/tmp/w/note.txt".to_owned(), None)));
        assert_eq!(first("Found 3 files in lib.rs"), Some(("lib.rs".to_owned(), None)));
        assert_eq!(first("no path here"), None);
        assert_eq!(first("src/a.rs:12:5:x = 1"), Some(("src/a.rs".to_owned(), Some(12))));
        assert_eq!(first("a.rs:12:34:56"), Some(("a.rs".to_owned(), Some(12))));
        assert_eq!(first("see https://x.y/z"), None, "a URL is not a path");
        assert_eq!(first(""), None);
    }

    #[test]
    fn text_links_come_with_their_columns() {
        let mut line = Line::from_text("go https://a.b/字x now", 24, Style::DEFAULT);
        // Make 字 a real wide cell followed by its spacer, shifting "x" one column right.
        line.cells[15] = Cell::wide("字", Style::DEFAULT);
        line.cells[16] = Cell::spacer_tail(Style::DEFAULT);
        line.cells[17] = Cell::narrow('x', Style::DEFAULT);
        line.cells[18] = Cell::narrow(' ', Style::DEFAULT);
        let span = text_link_at(&[&line], (0, 5)).unwrap();
        assert_eq!(span.url, "https://a.b/字x");
        assert_eq!((span.start, span.end), ((0, 3), (0, 18)), "the wide cell's spacer is inside");
        assert_eq!(text_link_at(&[&line], (0, 2)), None);
        assert_eq!(text_link_at(&[&line], (0, 19)), None);
    }

    /// A URL or a path the terminal wrapped is found whole from either row, and ends where
    /// its text does; an OSC 8 run carried on to the next row is one link too.
    #[test]
    fn a_wrapped_link_or_path_is_one() {
        let top = Line::from_text("see https://a.b", 15, Style::DEFAULT);
        let mut below = Line::from_text("/cd/ef now", 15, Style::DEFAULT);
        below.flags.insert(slopty_grid::LineFlags::WRAPPED);
        let rows = [&top, &below];
        for at in [(0, 6), (1, 2)] {
            let span = link_at(&rows, at).unwrap();
            assert_eq!(span.url, "https://a.b/cd/ef", "from {at:?}");
            assert_eq!((span.start, span.end), ((0, 4), (1, 6)));
        }
        assert_eq!(link_at(&rows, (1, 8)), None, "the word after it");

        let top = Line::from_text("at src/very/lo", 14, Style::DEFAULT);
        let below = Line::from_text("ng/name.rs:9 x", 14, Style::DEFAULT);
        let span = path_at(&[&top, &below], (1, 1)).unwrap();
        assert_eq!((span.path.as_str(), span.line), ("src/very/long/name.rs", Some(9)));
        assert_eq!((span.start, span.end), ((0, 3), (1, 12)));

        let mut top = Line::from_text("docs here", 9, Style::DEFAULT);
        top.links.push(Hyperlink { col: 5, len: 4, uri: "https://real/".to_owned() });
        let mut below = Line::from_text("ok then", 9, Style::DEFAULT);
        below.links.push(Hyperlink { col: 0, len: 2, uri: "https://real/".to_owned() });
        for at in [(0, 6), (1, 0)] {
            let span = link_at(&[&top, &below], at).unwrap();
            assert_eq!(
                (span.start, span.end, span.url.as_str()),
                ((0, 5), (1, 2), "https://real/")
            );
        }
    }

    #[test]
    fn paths_are_found_with_their_line_and_nothing_else_is() {
        let at = path_range_at;
        assert_eq!(
            at("error: src/main.rs:12:5 bad", 9),
            Some((7..23, "src/main.rs".to_owned(), Some(12)))
        );
        assert_eq!(at("at ./notes.md.", 4), Some((3..13, "./notes.md".to_owned(), None)));
        assert_eq!(at("open (~/.zshrc)", 8), Some((6..14, "~/.zshrc".to_owned(), None)));
        assert_eq!(at("see Cargo.toml:3", 6), Some((4..16, "Cargo.toml".to_owned(), Some(3))));
        assert_eq!(at("e.g. this", 1), None, "prose is not a file");
        assert_eq!(at("https://x.y/a.rs", 12), None, "a URL is a link, not a path");
        assert_eq!(at("12:30 now", 1), None);
        assert_eq!(at("   ", 1), None);
        let line = Line::from_text("in src/lib.rs:7", 20, Style::DEFAULT);
        let span = path_at(&[&line], (0, 6)).unwrap();
        assert_eq!(
            (span.start, span.end, span.path.as_str(), span.line),
            ((0, 3), (0, 15), "src/lib.rs", Some(7))
        );
        assert_eq!(path_at(&[&line], (0, 1)), None);
        assert_eq!(shell_word("it's a b"), "'it'\\''s a b'");
        assert_eq!(editor_command("a b.rs", Some(3)), "${EDITOR:-vi} +3 'a b.rs'");
        assert_eq!(editor_command("/x/y", None), "${EDITOR:-vi} '/x/y'");
    }

    #[test]
    fn osc8_runs_win_over_the_text_scan() {
        let mut line = Line::from_text("docs https://x.y", 20, Style::DEFAULT);
        line.links.push(Hyperlink { col: 0, len: 4, uri: "https://real/".to_owned() });
        let span = link_at(&[&line], (0, 1)).unwrap();
        assert_eq!((span.start, span.end, span.url.as_str()), ((0, 0), (0, 4), "https://real/"));
        let span = link_at(&[&line], (0, 7)).unwrap();
        assert_eq!((span.start, span.end, span.url.as_str()), ((0, 5), (0, 16), "https://x.y"));
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
