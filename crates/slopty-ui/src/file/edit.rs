//! What a file tile works out about a text on its own, as plain functions.
//!
//! How the file indents and ends its lines, a block of lines commented, moved or copied, the
//! bracket that matches one at the caret, and where "go to line" lands. The view applies the
//! results to the editor ([`super::FileView`]); nothing here knows about GPUI.

use std::borrow::Cow;

use gpui_kit::component::input::{Rope, RopeExt as _};

use crate::highlight::Comment;

/// How a file indents: what Tab puts in, and how wide a tab is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Indent {
    /// Tab puts in a tab, not spaces.
    pub hard_tabs: bool,
    /// Columns per level: the spaces Tab puts in, or a tab's width.
    pub width: usize,
}

impl Indent {
    /// A file that says nothing about its indentation (no indented line): four spaces, the
    /// default of most editors and style guides.
    pub const DEFAULT: Self = Self { hard_tabs: false, width: 4 };

    /// How the status line names it: "Tabs" or "Spaces: 4".
    #[must_use]
    pub fn label(self) -> String {
        if self.hard_tabs { "Tabs".to_owned() } else { format!("Spaces: {}", self.width) }
    }
}

/// Lines the indentation guess reads at most: a file's habit shows long before, and the guess
/// stays a bounded cost on a 16 MiB file.
const INDENT_SAMPLE_LINES: usize = 10_000;

/// The widest indentation step the guess believes; a larger jump is alignment.
const INDENT_WIDEST: usize = 8;

/// How `text` indents, from its own lines.
///
/// Tabs when more indented lines start with a tab than with a space. Otherwise the width is
/// the indent *increase* between a line and the one before it seen most often (2 to 8
/// columns, the narrower on a tie, since a file stepping by 2 also steps by 4), Helix's and
/// Lapce's histogram: a dedent can drop several levels at once, an indent rarely climbs more
/// than one. Blank lines and the ` *` lines of a block comment, which align rather than
/// indent, are skipped. A file with no indented line gets [`Indent::DEFAULT`].
#[must_use]
pub fn detect_indent(text: &str) -> Indent {
    let (mut tabbed, mut spaced) = (0_usize, 0_usize);
    let mut steps = [0_usize; INDENT_WIDEST + 1];
    let mut previous = 0_usize;
    for line in text.split('\n').take(INDENT_SAMPLE_LINES) {
        let body = line.trim_start_matches([' ', '\t']);
        if body.is_empty() || body.starts_with('*') {
            continue;
        }
        let lead = line.len().saturating_sub(body.len());
        if line.starts_with('\t') {
            tabbed = tabbed.saturating_add(1);
            continue;
        }
        if lead > 0 {
            spaced = spaced.saturating_add(1);
        }
        let step = lead.saturating_sub(previous);
        if let Some(count) = steps.get_mut(step).filter(|_| step >= 2) {
            *count = count.saturating_add(1);
        }
        previous = lead;
    }
    if tabbed > spaced {
        return Indent { hard_tabs: true, width: Indent::DEFAULT.width };
    }
    let best = (2..=INDENT_WIDEST)
        .filter_map(|width| steps.get(width).map(|&count| (width, count)))
        .filter(|&(_, count)| count > 0)
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)));
    best.map_or(Indent::DEFAULT, |(width, _)| Indent { hard_tabs: false, width })
}

/// How a file ends its lines and whether it opens with a byte-order mark: kept apart from the
/// text the editor holds, and put back on save.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Format {
    /// Every line ends `\r\n`.
    pub crlf: bool,
    /// The file opens with UTF-8's byte-order mark.
    pub bom: bool,
}

impl Format {
    /// The text as the editor holds it, and the file's format, for a read that drops the
    /// file's final newline (`final_newline` says it had one): a BOM taken off, and `\r\n`
    /// made `\n` when every line ends so. A file that mixes the two keeps its bytes as they
    /// are, so a save changes no line the person did not touch.
    #[must_use]
    pub fn split(text: &str, final_newline: bool) -> (Cow<'_, str>, Self) {
        let (text, bom) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (rest, true),
            None => (text, false),
        };
        // The read took the last line's `\n`, and left its `\r`.
        let last_cr = final_newline && text.ends_with('\r');
        let newlines = text.matches('\n').count().saturating_add(usize::from(final_newline));
        let crlfs = text.matches("\r\n").count().saturating_add(usize::from(last_cr));
        let crlf = newlines > 0 && crlfs == newlines;
        let text = if crlf {
            let body = if last_cr { text.get(..text.len().saturating_sub(1)) } else { Some(text) };
            Cow::Owned(body.unwrap_or_default().replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(text)
        };
        (text, Self { crlf, bom })
    }

    /// The file's bytes for the editor's `text` (`\n` its line break), ending with a newline
    /// when `final_newline`.
    #[must_use]
    pub fn join(self, text: &str, final_newline: bool) -> String {
        let mut out = String::with_capacity(text.len().saturating_add(text.len() >> 5));
        if self.bom {
            out.push('\u{feff}');
        }
        if self.crlf {
            out.push_str(&text.replace('\n', "\r\n"));
        } else {
            out.push_str(text);
        }
        if final_newline {
            out.push_str(self.newline());
        }
        out
    }

    /// The line break the file uses.
    #[must_use]
    pub const fn newline(self) -> &'static str {
        if self.crlf { "\r\n" } else { "\n" }
    }

    /// What the status line says of it beyond the usual (UTF-8, LF): "CRLF", "UTF-8 with BOM".
    #[must_use]
    pub fn label(self) -> Option<String> {
        match (self.crlf, self.bom) {
            (false, false) => None,
            (true, false) => Some("CRLF".to_owned()),
            (false, true) => Some("UTF-8 with BOM".to_owned()),
            (true, true) => Some("CRLF, UTF-8 with BOM".to_owned()),
        }
    }
}

/// A block of whole lines rewritten: the bytes it covers and what they become, with where
/// the selection goes after it. Offsets are bytes of the editor's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEdit {
    /// What is replaced.
    pub range: std::ops::Range<usize>,
    /// What replaces it.
    pub text: String,
    /// The selection after the edit.
    pub selection: std::ops::Range<usize>,
}

/// The lines a selection covers, first and last (0-based): a selection that ends at the start
/// of a line does not take that line.
#[must_use]
pub fn selected_lines(text: &Rope, selection: &std::ops::Range<usize>) -> (usize, usize) {
    let first = text.offset_to_point(selection.start).row;
    let end = text.offset_to_point(selection.end);
    let last = if end.row > first && end.column == 0 { end.row.saturating_sub(1) } else { end.row };
    (first, last)
}

/// The byte range of lines `first..=last`, without the last one's newline.
fn lines_range(text: &Rope, first: usize, last: usize) -> std::ops::Range<usize> {
    text.line_start_offset(first)..text.line_end_offset(last)
}

/// Comment the selected lines with `comment`, or uncomment them when they all are.
///
/// A line comment goes in at the shallowest indent of the lines, followed by a space, and
/// blank lines are left alone, as Zed and VS Code do; uncommenting takes the token and one
/// space after it. A language with only a block comment wraps the selection, or the caret's
/// line past its indent, and unwraps text already wrapped. The selection keeps its text.
#[must_use]
pub fn toggle_comment(
    text: &Rope,
    selection: &std::ops::Range<usize>,
    comment: Comment,
) -> Option<LineEdit> {
    match comment {
        Comment::Line(token) => toggle_line_comment(text, selection, token),
        Comment::Block(open, close) => toggle_block_comment(text, selection, open, close),
    }
}

fn toggle_line_comment(
    text: &Rope,
    selection: &std::ops::Range<usize>,
    token: &str,
) -> Option<LineEdit> {
    let (first, last) = selected_lines(text, selection);
    let range = lines_range(text, first, last);
    let block = text.slice(range.clone()).to_string();
    let lines: Vec<&str> = block.split('\n').collect();
    let indent_of =
        |line: &str| line.len().saturating_sub(line.trim_start_matches([' ', '\t']).len());
    let filled: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if filled.is_empty() {
        return None;
    }
    let commented = filled.iter().all(|l| l.trim_start_matches([' ', '\t']).starts_with(token));
    let column = filled.iter().map(|l| indent_of(l)).min().unwrap_or(0);
    // Per line: the byte the edit is at, bytes removed, bytes put in.
    let mut out = String::with_capacity(
        block.len().saturating_add(lines.len().saturating_mul(token.len().saturating_add(1))),
    );
    let mut edits: Vec<(usize, usize, usize)> = Vec::with_capacity(lines.len());
    for (ix, line) in lines.iter().enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        if line.trim().is_empty() {
            out.push_str(line);
            edits.push((0, 0, 0));
        } else if commented {
            let at = indent_of(line);
            let after = line.get(at.saturating_add(token.len())..).unwrap_or_default();
            let removed = token.len().saturating_add(usize::from(after.starts_with(' ')));
            out.push_str(line.get(..at).unwrap_or_default());
            out.push_str(line.get(at.saturating_add(removed)..).unwrap_or_default());
            edits.push((at, removed, 0));
        } else {
            out.push_str(line.get(..column).unwrap_or_default());
            out.push_str(token);
            out.push(' ');
            out.push_str(line.get(column..).unwrap_or_default());
            edits.push((column, 0, token.len().saturating_add(1)));
        }
    }
    // `after`: an offset at a token's place moves past a token put in (a caret, a
    // selection's end); else it stays before it (a selection's start, so the lines stay whole).
    let map = |offset: usize, after: bool| {
        if offset > range.end {
            // Past the block (a selection ending on the next line's start): it moves by
            // what the block grew or shrank.
            return offset.saturating_add(out.len()).saturating_sub(block.len());
        }
        let point = text.offset_to_point(offset);
        let row = point.row.saturating_sub(first);
        let (at, removed, put) = edits.get(row).copied().unwrap_or_default();
        let column = point.column;
        let column = if column < at || (column == at && (put == 0 || !after)) {
            column
        } else if column < at.saturating_add(removed) {
            at
        } else {
            column.saturating_add(put).saturating_sub(removed)
        };
        let start: usize = out.split('\n').take(row).map(|l| l.len().saturating_add(1)).sum();
        range.start.saturating_add(start).saturating_add(column)
    };
    let selection = if selection.is_empty() {
        let caret = map(selection.start, true);
        caret..caret
    } else {
        map(selection.start, false)..map(selection.end, true)
    };
    Some(LineEdit { range, text: out, selection })
}

fn toggle_block_comment(
    text: &Rope,
    selection: &std::ops::Range<usize>,
    open: &str,
    close: &str,
) -> Option<LineEdit> {
    let range = if selection.is_empty() {
        let row = text.offset_to_point(selection.start).row;
        let line = text.slice_line(row).to_string();
        let start = text.line_start_offset(row);
        let lead = line.len().saturating_sub(line.trim_start().len());
        let body = line.trim();
        if body.is_empty() {
            return None;
        }
        start.saturating_add(lead)..start.saturating_add(lead).saturating_add(body.len())
    } else {
        selection.clone()
    };
    let inner = text.slice(range.clone()).to_string();
    let wrapped = inner.strip_prefix(open).and_then(|rest| rest.strip_suffix(close));
    let (out, shift_in): (String, isize) = if let Some(body) = wrapped {
        let body = body.strip_prefix(' ').unwrap_or(body);
        let body = body.strip_suffix(' ').unwrap_or(body);
        let removed = inner.len().saturating_sub(body.len());
        (body.to_owned(), 0_isize.saturating_sub_unsigned(removed))
    } else {
        let out = format!("{open} {inner} {close}");
        let put = out.len().saturating_sub(inner.len());
        (out, isize::try_from(put).unwrap_or(0))
    };
    let selection = if selection.is_empty() {
        let caret = selection.start.clamp(range.start, range.end);
        let moved = if wrapped.is_some() {
            caret.saturating_sub(open.len().saturating_add(1)).max(range.start)
        } else {
            caret.saturating_add(open.len()).saturating_add(1)
        };
        moved..moved
    } else {
        range.start..range.end.saturating_add_signed(shift_in)
    };
    Some(LineEdit { range, text: out, selection })
}

/// Swap the selected lines with the line above (`up`) or below, the selection going with
/// them; none at the top or the bottom.
#[must_use]
pub fn move_lines(text: &Rope, selection: &std::ops::Range<usize>, up: bool) -> Option<LineEdit> {
    let (first, last) = selected_lines(text, selection);
    let rows = text.lines_len();
    let block = text.slice(lines_range(text, first, last)).to_string();
    if up {
        let above = first.checked_sub(1)?;
        let neighbour = text.slice_line(above).to_string();
        let range = lines_range(text, above, last);
        let shift = neighbour.len().saturating_add(1);
        Some(LineEdit {
            range,
            text: format!("{block}\n{neighbour}"),
            selection: selection.start.saturating_sub(shift)..selection.end.saturating_sub(shift),
        })
    } else {
        let below = last.saturating_add(1);
        if below >= rows {
            return None;
        }
        let neighbour = text.slice_line(below).to_string();
        let range = lines_range(text, first, below);
        let shift = neighbour.len().saturating_add(1);
        Some(LineEdit {
            range,
            text: format!("{neighbour}\n{block}"),
            selection: selection.start.saturating_add(shift)..selection.end.saturating_add(shift),
        })
    }
}

/// The selected lines written twice, the selection on the copy below.
#[must_use]
pub fn duplicate_lines(text: &Rope, selection: &std::ops::Range<usize>) -> LineEdit {
    let (first, last) = selected_lines(text, selection);
    let range = lines_range(text, first, last);
    let block = text.slice(range.clone()).to_string();
    let shift = block.len().saturating_add(1);
    LineEdit {
        range,
        text: format!("{block}\n{block}"),
        selection: selection.start.saturating_add(shift)..selection.end.saturating_add(shift),
    }
}

/// The brackets that pair, opening then closing.
const BRACKETS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// Bytes a bracket match scans at most each way: past it a match is far off screen anyway,
/// and the scan stays a bounded cost on each caret move whatever the file.
pub const BRACKET_SCAN_BYTES: usize = 64 << 10;

/// The bracket next to the caret and the one it pairs with, as byte offsets, opening first.
///
/// The bracket is the one just after the caret, else the one just before it. Brackets of the other
/// kinds are not counted; none when it is unmatched within [`BRACKET_SCAN_BYTES`].
#[must_use]
pub fn matching_bracket(text: &Rope, caret: usize) -> Option<(usize, usize)> {
    let after = text.chars_at(caret).next().map(|c| (caret, c));
    let before =
        text.chars_at(caret).reversed().next().map(|c| (caret.saturating_sub(c.len_utf8()), c));
    [after, before].into_iter().flatten().find_map(|(at, c)| pair_of(text, at, c))
}

/// The bracket `c` at byte `at` and the one that pairs with it, opening first.
fn pair_of(text: &Rope, at: usize, c: char) -> Option<(usize, usize)> {
    let (open, close, forward) = BRACKETS.iter().find_map(|&(o, k)| {
        if c == o {
            Some((o, k, true))
        } else if c == k {
            Some((o, k, false))
        } else {
            None
        }
    })?;
    let mut depth = 0_usize;
    let mut scanned = 0_usize;
    if forward {
        let mut offset = at;
        for ch in text.chars_at(at) {
            if ch == open {
                depth = depth.saturating_add(1);
            } else if ch == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((at, offset));
                }
            }
            offset = offset.saturating_add(ch.len_utf8());
            scanned = scanned.saturating_add(ch.len_utf8());
            if scanned > BRACKET_SCAN_BYTES {
                return None;
            }
        }
    } else {
        let mut offset = at.saturating_add(c.len_utf8());
        for ch in text.chars_at(offset).reversed() {
            offset = offset.saturating_sub(ch.len_utf8());
            if ch == close {
                depth = depth.saturating_add(1);
            } else if ch == open {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((offset, at));
                }
            }
            scanned = scanned.saturating_add(ch.len_utf8());
            if scanned > BRACKET_SCAN_BYTES {
                return None;
            }
        }
    }
    None
}

/// Where "go to line" lands for what was typed: `42`, `42:7` or `42,7` (1-based, as an error
/// message names a place), as a 0-based line and character column clamped into the text; none
/// for anything else.
#[must_use]
pub fn line_target(typed: &str, text: &Rope) -> Option<usize> {
    let typed = typed.trim().trim_start_matches(':');
    let (line, column) = match typed.split_once([':', ',']) {
        Some((line, column)) => (line.trim(), Some(column.trim())),
        None => (typed, None),
    };
    let line: usize = line.parse().ok()?;
    let column: usize = match column {
        Some(c) if !c.is_empty() => c.parse().ok()?,
        _ => 1,
    };
    let row = line.saturating_sub(1).min(text.lines_len().saturating_sub(1));
    let start = text.line_start_offset(row);
    let body = text.slice_line(row);
    let bytes: usize = body.chars().take(column.saturating_sub(1)).map(char::len_utf8).sum();
    Some(start.saturating_add(bytes))
}

#[cfg(test)]
mod tests;
