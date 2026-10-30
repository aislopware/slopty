//! Links in terminal text: the link under a cell, found on the client from the line cache.
//!
//! An OSC 8 run the program marked wins (`Line::links`, filled by the worker engine). Most
//! programs do not emit OSC 8, so otherwise the logical line (the row and the rows soft-wrapped
//! onto it) is joined column by column (a wide cluster fills its first column, the spacer
//! contributes nothing) so a cell maps to a byte offset, then the longest run around that
//! offset that starts with a known scheme and ends at whitespace or a quote is the link, minus
//! the punctuation prose puts after a URL. A link the terminal wrapped is one link.

use std::fmt::Write as _;
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

/// Where a link a program printed goes: links are opened for the machine the program runs on,
/// since that is where its files and its servers are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Destination {
    /// A `file://` link: the path on that machine, percent-decoded. `ls --hyperlink`, `rg` and
    /// `delta` name the host (`file://mini/…`), which is that machine too.
    File(String),
    /// A web page served on that machine (`localhost`, `127.0.0.0/8`, `[::1]`, `0.0.0.0`):
    /// opened through it, where the port is.
    WorkerPage(String),
    /// Anywhere else: this device's own handler.
    Here(String),
}

/// Where `url` goes ([`Destination`]).
#[must_use]
pub fn destination(url: &str) -> Destination {
    use slopty_proto::handoff::{Wary, page};
    if let Some(path) = file_url_path(url) {
        return Destination::File(path);
    }
    if page(url).is_some_and(|p| p.wary == Some(Wary::Loopback)) {
        return Destination::WorkerPage(url.to_owned());
    }
    Destination::Here(url.to_owned())
}

/// The path of a `file://` URL, its host (if any) dropped and its `%XX` escapes decoded; `None`
/// for anything else, or one that names no absolute path.
#[must_use]
pub fn file_url_path(url: &str) -> Option<String> {
    let scheme = url.get(..7).filter(|s| s.eq_ignore_ascii_case("file://"))?;
    let rest = url.get(scheme.len()..)?;
    let path = rest.get(rest.find('/')?..)?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&b) = bytes.get(at) {
        let hex = |i: usize| bytes.get(i).and_then(|c| char::from(*c).to_digit(16));
        if let (b'%', Some(hi), Some(lo)) =
            (b, hex(at.saturating_add(1)), hex(at.saturating_add(2)))
        {
            out.push(u8::try_from(hi.saturating_mul(16).saturating_add(lo)).ok()?);
            at = at.saturating_add(3);
        } else {
            out.push(b);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8(out).ok()
}

/// What quick select labels on a logical line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Found {
    /// A link: the OSC 8 target or the URL as printed.
    Link(String),
    /// A file path as printed, without the `:line` after it.
    Path {
        /// The path.
        path: String,
        /// The line number after it, if any.
        line: Option<u32>,
    },
    /// A commit hash: 7 to 40 lowercase hex digits, a digit and a letter among them.
    Commit(String),
}

impl Found {
    /// The text a copy takes: the link, the path with its line, the hash.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Link(url) => url.clone(),
            Self::Path { path, line: Some(line) } => format!("{path}:{line}"),
            Self::Path { path, line: None } => path.clone(),
            Self::Commit(hash) => hash.clone(),
        }
    }
}

/// One thing quick select can label: the cells it covers on its logical line and what it is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    /// First cell.
    pub start: Cell,
    /// One past the last cell, on the last cell's row.
    pub end: Cell,
    /// What it is.
    pub found: Found,
}

/// Every link, file path and commit hash on the logical line `rows`, in reading order.
///
/// None lies over another: the OSC 8 runs the program marked, then in the text around them the
/// URLs, the paths and the hashes, each read as [`link_at`] and [`path_at`] read them.
#[must_use]
pub fn targets(rows: &[&Line]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for (row, line) in rows.iter().enumerate() {
        for link in &line.links {
            // A run carried on from the row above is that row's link.
            let carried = link.col == 0
                && row.checked_sub(1).and_then(|above| rows.get(above)).is_some_and(|above| {
                    above
                        .cols()
                        .checked_sub(1)
                        .and_then(|last| above.link_at(last))
                        .is_some_and(|run| run.uri == link.uri)
                });
            if !carried && let Some(span) = osc8_at(rows, (row, link.col)) {
                out.push(Target { start: span.start, end: span.end, found: Found::Link(span.url) });
            }
        }
    }
    let marked = out.len();
    let (text, starts, _) = joined(rows, (usize::MAX, 0));
    let is_delim = |c: char| {
        c.is_whitespace()
            || matches!(c, '"' | '\'' | '<' | '>' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',')
    };
    let push = |range: &Range<usize>, found: Found, out: &mut Vec<Target>| {
        let Some((start, end)) = cells_of(&starts, range) else { return };
        let over = |t: &Target| start < t.end && t.start < end;
        if !out.get(..marked).unwrap_or_default().iter().any(over) {
            out.push(Target { start, end, found });
        }
    };
    let mut at = 0;
    let mut word_start = true;
    while let Some(c) = text.get(at..).and_then(|rest| rest.chars().next()) {
        if let Some(range) = url_range_at(&text, at).filter(|range| range.start == at) {
            let url = text.get(range.clone()).unwrap_or_default().to_owned();
            push(&range, Found::Link(url), &mut out);
            (at, word_start) = (range.end, false);
            continue;
        }
        if word_start && !is_delim(c) {
            if let Some((range, path, line)) =
                path_range_at(&text, at).filter(|(range, ..)| range.start == at)
            {
                push(&range, Found::Path { path, line }, &mut out);
                (at, word_start) = (range.end, false);
                continue;
            }
            if let Some(len) = text.get(at..).and_then(|rest| commit_at(rest, is_delim)) {
                let range = at..at.saturating_add(len);
                let hash = text.get(range.clone()).unwrap_or_default().to_owned();
                push(&range, Found::Commit(hash), &mut out);
                (at, word_start) = (range.end, false);
                continue;
            }
        }
        word_start = is_delim(c);
        at = at.saturating_add(c.len_utf8());
    }
    out.sort_by_key(|t| t.start);
    out
}

/// The length of the commit hash `text` starts with, if its first word (up to `is_delim`, the
/// punctuation prose leaves after it trimmed) is one: 7 to 40 lowercase hex digits with a
/// digit and a letter among them, so neither a number nor a word like `defaced` is taken.
fn commit_at(text: &str, is_delim: impl Fn(char) -> bool) -> Option<usize> {
    let word = text.find(is_delim).map_or(text, |end| text.get(..end).unwrap_or(text));
    let word = trim_trailing(word);
    let hex = word.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let mixed =
        word.bytes().any(|b| b.is_ascii_digit()) && word.bytes().any(|b| b.is_ascii_alphabetic());
    (hex && mixed && (7..=40).contains(&word.len())).then_some(word.len())
}

/// What a link's banner shows of `target`: one line that cannot pass for another target.
///
/// Ghostty's `UntrustedURL.displayString`, with a browser's rule for hosts: a host label
/// outside ASCII is shown in punycode (`xn--pple-43d.com`, never a lookalike `аpple.com`), a
/// path is shown standardised (`a//b/../c` is `a/c`, as it would be opened), and a control,
/// a direction mark or an invisible character is spelled out as `\u{…}`.
#[must_use]
pub fn display(target: &str) -> String {
    let shown = if let Some(path) = file_url_path(target) {
        tidy_path(&path)
    } else if let Some((scheme, rest)) = target.split_once("://") {
        format!("{scheme}://{}", ascii_host(rest))
    } else if target.contains('/') {
        tidy_path(target)
    } else {
        target.to_owned()
    };
    let mut out = String::with_capacity(shown.len());
    for c in shown.chars() {
        if unsafe_to_show(c) {
            let _infallible = write!(out, "\\u{{{:X}}}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether `c` could hide or reorder what a one-line target says: ghostty's list of controls,
/// direction marks, zero-width and line-breaking characters.
const fn unsafe_to_show(c: char) -> bool {
    matches!(
        c as u32,
        0x00..=0x1f
            | 0x7f..=0x9f
            | 0x061c
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2066..=0x2069
            | 0x2028..=0x2029
            | 0x2060
            | 0xfeff
    )
}

/// `path` with its empty and `.` parts dropped and each `..` taking the part before it, as
/// opening it would resolve them; a `..` at a relative path's head stays.
fn tidy_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    let tail = if path.ends_with('/') && !joined.is_empty() { "/" } else { "" };
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}{tail}"),
        (false, true) => ".".to_owned(),
        (false, false) => format!("{joined}{tail}"),
    }
}

/// The part of a URL after its `://` with each host label outside ASCII in punycode.
fn ascii_host(rest: &str) -> String {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (user, host) =
        authority.rsplit_once('@').map_or(("", authority), |(user, host)| (user, host));
    let (name, port) = if host.starts_with('[') {
        (host, "")
    } else {
        match host.rsplit_once(':') {
            Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                (name, host.get(name.len()..).unwrap_or_default())
            }
            _ => (host, ""),
        }
    };
    let labels: Vec<String> = name
        .split('.')
        .map(|label| {
            if label.is_ascii() {
                label.to_owned()
            } else {
                punycode(&label.to_lowercase())
                    .map_or_else(|| label.to_owned(), |code| format!("xn--{code}"))
            }
        })
        .collect();
    let at = if user.is_empty() && !authority.contains('@') { "" } else { "@" };
    format!("{user}{at}{}{port}{tail}", labels.join("."))
}

/// `label` in punycode (RFC 3492), without the `xn--` prefix; `None` past what its integers
/// hold.
fn punycode(label: &str) -> Option<String> {
    const BASE: u32 = 36;
    const T_MIN: u32 = 1;
    const T_MAX: u32 = 26;
    let digit = |d: u32| {
        let d = u8::try_from(d).ok()?;
        Some(char::from(if d < 26 {
            b'a'.checked_add(d)?
        } else {
            b'0'.checked_add(d.checked_sub(26)?)?
        }))
    };
    let points: Vec<u32> = label.chars().map(u32::from).collect();
    let mut out: String = label.chars().filter(char::is_ascii).collect();
    let basic = u32::try_from(out.len()).ok()?;
    let total = u32::try_from(points.len()).ok()?;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias, mut handled) = (0x80_u32, 0_u32, 72_u32, basic);
    while handled < total {
        let next = points.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add(next.checked_sub(n)?.checked_mul(handled.checked_add(1)?)?)?;
        n = next;
        for &c in &points {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        T_MIN
                    } else if k >= bias.checked_add(T_MAX)? {
                        T_MAX
                    } else {
                        k.checked_sub(bias)?
                    };
                    if q < t {
                        break;
                    }
                    let span = BASE.checked_sub(t)?;
                    out.push(digit(t.checked_add(q.checked_sub(t)?.checked_rem(span)?)?)?);
                    q = q.checked_sub(t)?.checked_div(span)?;
                    k = k.checked_add(BASE)?;
                }
                out.push(digit(q)?);
                bias = adapt(delta, handled.checked_add(1)?, handled == basic)?;
                delta = 0;
                handled = handled.checked_add(1)?;
            }
        }
        delta = delta.checked_add(1)?;
        n = n.checked_add(1)?;
    }
    Some(out)
}

/// Punycode's bias adaptation (RFC 3492 §6.1).
fn adapt(delta: u32, points: u32, first: bool) -> Option<u32> {
    const BASE: u32 = 36;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    /// `base - t_min`.
    const STEP: u32 = 35;
    /// `(base - t_min) * t_max / 2`.
    const ROOM: u32 = 455;
    let mut delta = if first { delta.checked_div(DAMP)? } else { delta.checked_div(2)? };
    delta = delta.checked_add(delta.checked_div(points)?)?;
    let mut k = 0_u32;
    while delta > ROOM {
        delta = delta.checked_div(STEP)?;
        k = k.checked_add(BASE)?;
    }
    let scaled = STEP.checked_add(1)?.checked_mul(delta)?.checked_div(delta.checked_add(SKEW)?)?;
    k.checked_add(scaled)
}

/// The URL in `text` that covers byte `offset`.
#[cfg(test)]
#[must_use]
fn url_at(text: &str, offset: usize) -> Option<&str> {
    text.get(url_range_at(text, offset)?)
}

#[cfg(test)]
mod tests {
    use slopty_grid::{Cell, Hyperlink, Style};

    use super::*;

    /// The first file path in `text` — a grep hit's `src/a.rs:12:…`, a compiler's
    /// ` --> src/b.rs:3:5` — with its byte range and the line number after it.
    fn first_path(text: &str) -> Option<(Range<usize>, String, Option<u32>)> {
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

    /// Every link, path and hash on a line, in reading order and none twice: an OSC 8 run
    /// over a printed URL is the run, a path's `:line` is read off, and neither a number, a
    /// word of hex letters nor a hash too short is taken.
    #[test]
    fn quick_select_finds_every_link_path_and_hash() {
        let mut line = Line::from_text(
            "see https://a.b/c, src/x.rs:12 at 1a2b3c4d; docs 1234567 defaced abc12",
            72,
            Style::DEFAULT,
        );
        line.links.push(Hyperlink { col: 44, len: 4, uri: "https://real/".to_owned() });
        let found: Vec<((usize, u16), Found)> =
            targets(&[&line]).into_iter().map(|t| (t.start, t.found)).collect();
        assert_eq!(
            found,
            [
                ((0, 4), Found::Link("https://a.b/c".to_owned())),
                ((0, 19), Found::Path { path: "src/x.rs".to_owned(), line: Some(12) }),
                ((0, 34), Found::Commit("1a2b3c4d".to_owned())),
                ((0, 44), Found::Link("https://real/".to_owned())),
            ]
        );
        let top = Line::from_text("at https://a.b/l", 16, Style::DEFAULT);
        let mut below = Line::from_text("ong ok", 16, Style::DEFAULT);
        below.flags.insert(slopty_grid::LineFlags::WRAPPED);
        let wrapped = targets(&[&top, &below]);
        assert_eq!(wrapped.len(), 1, "{wrapped:?}");
        assert_eq!((wrapped[0].start, wrapped[0].end), ((0, 3), (1, 3)));
        assert_eq!(Found::Path { path: "a".to_owned(), line: Some(3) }.text(), "a:3");
    }

    /// The banner's text cannot pass for another target: a host outside ASCII in punycode, a
    /// path as it would open, and what is invisible spelled out.
    #[test]
    fn a_banner_shows_where_a_link_really_goes() {
        assert_eq!(punycode("bücher").as_deref(), Some("bcher-kva"));
        assert_eq!(punycode("münchen").as_deref(), Some("mnchen-3ya"));
        assert_eq!(display("https://\u{430}pple.com/login"), "https://xn--pple-43d.com/login");
        assert_eq!(
            display("http://me@Bücher.example:8080/a?q=ü"),
            "http://me@xn--bcher-kva.example:8080/a?q=ü"
        );
        assert_eq!(display("https://[::1]:3000/x"), "https://[::1]:3000/x");
        assert_eq!(display("file:///tmp/a//b/../c"), "/tmp/a/c");
        assert_eq!(display("./src/../lib.rs:9"), "lib.rs:9");
        assert_eq!(display("../up/./x/"), "../up/x/");
        assert_eq!(display("https://a.b/x\u{202e}gpj.exe"), "https://a.b/x\\u{202E}gpj.exe");
        assert_eq!(display("https://a.b/\nnext"), "https://a.b/\\u{A}next");
    }

    /// A program's links go where the program runs: a `file://` link to its machine's path, with
    /// or without the host `ls --hyperlink` names and with its escapes decoded; a page on its
    /// loopback through it; anything else here.
    #[test]
    fn a_link_goes_to_the_machine_the_program_runs_on() {
        assert_eq!(
            destination("file:///Users/me/a%20b%25c%23d%3F%C3%A9.rs"),
            Destination::File("/Users/me/a b%c#d?é.rs".to_owned())
        );
        assert_eq!(
            destination("file://mini.local/home/me/notes.md"),
            Destination::File("/home/me/notes.md".to_owned())
        );
        assert_eq!(destination("FILE:///tmp/x#frag"), Destination::File("/tmp/x".to_owned()));
        for page in ["http://localhost:3000/", "http://127.0.0.1:8080/x", "http://[::1]:5173/"] {
            assert_eq!(destination(page), Destination::WorkerPage(page.to_owned()), "{page}");
        }
        for here in ["https://example.com/", "http://10.0.0.2/", "mailto:a@b.c", "file:"] {
            assert_eq!(destination(here), Destination::Here(here.to_owned()), "{here}");
        }
        assert_eq!(file_url_path("file://host"), None, "no path");
        assert_eq!(file_url_path("file:///%FF"), None, "not UTF-8");
        assert_eq!(file_url_path("file:///a%2"), Some("/a%2".to_owned()), "a lone % stays");
    }
}
