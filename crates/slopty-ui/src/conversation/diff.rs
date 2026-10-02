//! An edit's patch as the face draws it: each line with its old and new numbers and its
//! syntax colours, in one column or paired side by side.
//!
//! Each hunk's two sides are parsed as texts of their own (the old side is its context and
//! removed lines, the new side its context and added lines), so a removed line that opens a
//! block comment does not colour the lines that replaced it. A hunk is parsed apart from the
//! next, which sits elsewhere in the file.
//!
//! Two levels of change, as git-delta and Zed's Delta draw them: a changed line's wash says
//! where, and within a removed line and the added line that replaced it, the words that differ
//! are emphasised ([`Line::emph`]), so a one-word rename in a long line is found at a glance.

use std::ops::Range;
use std::rc::Rc;

use slopty_proto::conversation::{Hunk, Patch};

use crate::highlight::{self, Span, Syntax};

/// What a line of a diff is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// In both.
    Context,
    /// Only in the new file.
    Added,
    /// Only in the old.
    Removed,
}

/// One line of a diff, ready to draw.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    /// What it is.
    pub kind: Kind,
    /// Its number in the old file.
    pub old: Option<u32>,
    /// Its number in the new file.
    pub new: Option<u32>,
    /// Its text, without the sign.
    pub text: String,
    /// Its colours, when the path names a grammar.
    pub spans: Option<Vec<Span>>,
    /// The file ends here without a newline (git's "\ No newline at end of file", which
    /// follows the line it is about).
    pub no_newline: bool,
    /// The bytes of its text that differ from the line it pairs with, a removed line with the
    /// added line that replaced it; none for a line that pairs with none.
    pub emph: Vec<Range<usize>>,
}

/// A hunk's lines.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Block {
    /// Where it starts in the new file, for the hunk's divider.
    pub new_start: u32,
    /// Its lines.
    pub lines: Vec<Line>,
}

/// The line numbers and colours of `patch` for a file at `path`.
#[must_use]
pub fn blocks(path: &str, patch: &Patch) -> Rc<[Block]> {
    let syntax = Syntax::for_path(path, "");
    patch.hunks.iter().map(|hunk| block(hunk, syntax)).collect()
}

/// The line numbers and colours of a thread's `patch` for a file at `path`.
#[must_use]
pub fn thread_blocks(path: &str, patch: &slopty_proto::thread::Patch) -> Rc<[Block]> {
    let syntax = Syntax::for_path(path, "");
    patch.hunks.iter().map(|h| block_of(h.old_start, h.new_start, &h.lines, syntax)).collect()
}

fn block(hunk: &Hunk, syntax: Option<Syntax>) -> Block {
    block_of(hunk.old_start, hunk.new_start, &hunk.lines, syntax)
}

fn block_of(
    old_start: u32,
    new_start: u32,
    hunk_lines: &[String],
    syntax: Option<Syntax>,
) -> Block {
    // Git's note about the last newline is a flag on the line before it, not a line.
    let mut parsed: Vec<(Kind, &str, bool)> = Vec::with_capacity(hunk_lines.len());
    for line in hunk_lines {
        let mut chars = line.chars();
        let kind = match chars.next() {
            Some('+') => Kind::Added,
            Some('-') => Kind::Removed,
            Some('\\') => {
                if let Some(last) = parsed.last_mut() {
                    last.2 = true;
                }
                continue;
            }
            _ => Kind::Context,
        };
        parsed.push((kind, chars.as_str(), false));
    }
    let side = |skip: Kind| -> String {
        parsed
            .iter()
            .filter(|(kind, ..)| *kind != skip)
            .map(|(_, text, _)| *text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let spans = syntax.map(|syntax| {
        (
            highlight::spans(&side(Kind::Added), syntax),
            highlight::spans(&side(Kind::Removed), syntax),
        )
    });
    let (mut old_no, mut new_no) = (old_start, new_start);
    let (mut old_at, mut new_at) = (0_usize, 0_usize);
    let mut lines = Vec::with_capacity(parsed.len());
    for (kind, text, no_newline) in parsed {
        let (old, new, from_old) = match kind {
            Kind::Context => (Some(old_no), Some(new_no), false),
            Kind::Removed => (Some(old_no), None, true),
            Kind::Added => (None, Some(new_no), false),
        };
        let coloured = match &spans {
            None => None,
            Some((old_side, _)) if from_old => old_side.get(old_at).cloned(),
            Some((_, new_side)) => new_side.get(new_at).cloned(),
        };
        if matches!(kind, Kind::Context | Kind::Removed) {
            old_no = old_no.saturating_add(1);
            old_at = old_at.saturating_add(1);
        }
        if matches!(kind, Kind::Context | Kind::Added) {
            new_no = new_no.saturating_add(1);
            new_at = new_at.saturating_add(1);
        }
        lines.push(Line {
            kind,
            old,
            new,
            text: text.to_owned(),
            spans: coloured,
            no_newline,
            emph: Vec::new(),
        });
    }
    emphasise(&mut lines);
    Block { new_start, lines }
}

/// The bytes of a line that changed.
type Emph = Vec<Range<usize>>;

/// The furthest apart two lines may be and still pair, as git-delta's `max-line-distance`.
const PAIR_DISTANCE: f64 = 0.6;

/// A run longer than this many lines on a side is left unpaired: pairing tries every
/// removal against every addition after the last pair.
const PAIR_RUN: usize = 64;

/// A line longer than this many bytes is not compared word by word.
const PAIR_BYTES: usize = 1_000;

/// Mark the words that differ in each run of removals and the additions after it.
///
/// Each removed line pairs with the first added line after the last pair that is near
/// enough ([`PAIR_DISTANCE`]); an added line passed over, and a removed line near none, stay
/// unpaired, all wash and no emphasis.
fn emphasise(lines: &mut [Line]) {
    let mut at = 0_usize;
    while at < lines.len() {
        let removed = run(lines, at, Kind::Removed);
        let added = run(lines, removed.end, Kind::Added);
        if !removed.is_empty() && !added.is_empty() && removed.len().max(added.len()) <= PAIR_RUN {
            let mut from = added.start;
            for minus in removed.clone() {
                for plus in from..added.end {
                    let (Some(old), Some(new)) = (lines.get(minus), lines.get(plus)) else {
                        break;
                    };
                    if let Some((old_emph, new_emph)) = words(&old.text, &new.text) {
                        if let Some(line) = lines.get_mut(minus) {
                            line.emph = old_emph;
                        }
                        if let Some(line) = lines.get_mut(plus) {
                            line.emph = new_emph;
                        }
                        from = plus.saturating_add(1);
                        break;
                    }
                }
            }
        }
        at = added.end.max(at.saturating_add(1));
    }
}

/// The lines of `kind` from `start` on.
fn run(lines: &[Line], start: usize, kind: Kind) -> Range<usize> {
    let len = lines.get(start..).unwrap_or_default().iter().take_while(|l| l.kind == kind).count();
    start..start.saturating_add(len)
}

/// A line's tokens: each run of word characters, and every other character on its own.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut word: Option<usize> = None;
    for (at, c) in text.char_indices() {
        let wordy = c.is_alphanumeric() || c == '_';
        match (word, wordy) {
            (None, true) => word = Some(at),
            (Some(start), false) => {
                out.extend(text.get(start..at));
                word = None;
            }
            _ => {}
        }
        if !wordy {
            out.extend(text.get(at..at.saturating_add(c.len_utf8())));
        }
    }
    if let Some(start) = word {
        out.extend(text.get(start..));
    }
    out
}

/// The bytes that differ between `old` and `new`, when they are near enough to pair: the
/// changed width over the whole, unchanged words counted on both sides, whitespace not at all.
fn words(old: &str, new: &str) -> Option<(Emph, Emph)> {
    if old.len() > PAIR_BYTES || new.len() > PAIR_BYTES {
        return None;
    }
    let (a, b) = (tokens(old), tokens(new));
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &a, &b);
    let width = |toks: &[&str]| -> usize {
        toks.iter().filter(|t| !t.trim().is_empty()).map(|t| t.chars().count()).sum()
    };
    let (mut changed, mut kept) = (0_usize, 0_usize);
    let (mut old_changed, mut new_changed) = (Vec::new(), Vec::new());
    for op in &ops {
        let (old_span, new_span) = (op.old_range(), op.new_range());
        let old_toks = a.get(old_span.clone()).unwrap_or_default();
        let new_toks = b.get(new_span.clone()).unwrap_or_default();
        if matches!(op, similar::DiffOp::Equal { .. }) {
            kept = kept.saturating_add(width(old_toks).saturating_mul(2));
        } else {
            changed = changed.saturating_add(width(old_toks)).saturating_add(width(new_toks));
            old_changed.push(old_span);
            new_changed.push(new_span);
        }
    }
    let total = changed.saturating_add(kept);
    #[expect(clippy::cast_precision_loss, reason = "a share of a line's width")]
    let distance = if total == 0 { 0.0 } else { changed as f64 / total as f64 };
    (distance <= PAIR_DISTANCE && changed > 0)
        .then(|| (bytes(&a, &old_changed), bytes(&b, &new_changed)))
}

/// Token spans as byte ranges of the line, a run that is only whitespace dropped, and two runs
/// apart by whitespace alone joined, so the emphasis does not stutter across a space.
fn bytes(toks: &[&str], spans: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut starts = Vec::with_capacity(toks.len().saturating_add(1));
    let mut at = 0_usize;
    for tok in toks {
        starts.push(at);
        at = at.saturating_add(tok.len());
    }
    starts.push(at);
    let byte = |ix: usize| starts.get(ix).copied().unwrap_or(at);
    let blank = |r: &Range<usize>| {
        toks.get(r.clone()).unwrap_or_default().iter().all(|t| t.trim().is_empty())
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<usize> = None;
    for span in spans.iter().filter(|r| !r.is_empty() && !blank(r)) {
        match (out.last_mut(), last) {
            (Some(prev), Some(end)) if blank(&(end..span.start)) => prev.end = byte(span.end),
            _ => out.push(byte(span.start)..byte(span.end)),
        }
        last = Some(span.end);
    }
    out
}

/// How a tab is drawn in a diff.
pub(crate) const TAB_SPACES: &str = "    ";

/// `text` with its tabs as spaces, and `spans` stretched to match.
#[must_use]
pub fn detab(text: &str, spans: Option<&[Span]>) -> (String, Option<Vec<Span>>) {
    if !text.contains('\t') {
        return (text.to_owned(), spans.map(<[Span]>::to_vec));
    }
    let out = text.replace('\t', TAB_SPACES);
    let spans = spans.map(|spans| {
        let mut at = 0_usize;
        spans
            .iter()
            .map(|span| {
                let end = at.saturating_add(span.len);
                let tabs = text.get(at..end).map_or(0, |piece| piece.matches('\t').count());
                at = end;
                Span {
                    len: span
                        .len
                        .saturating_add(tabs.saturating_mul(TAB_SPACES.len().saturating_sub(1))),
                    ..*span
                }
            })
            .collect()
    });
    (out, spans)
}

/// One row of a side-by-side diff: the old line on the left, the new on the right.
pub type Pair<'a> = (Option<&'a Line>, Option<&'a Line>);

/// A block's lines paired for side by side: context beside itself, a run of removals beside
/// the run of additions that follows it, row for row, the shorter side padded.
#[must_use]
pub fn pairs(block: &Block) -> Vec<Pair<'_>> {
    fn flush<'a>(out: &mut Vec<Pair<'a>>, removed: &mut Vec<&'a Line>, added: &mut Vec<&'a Line>) {
        let rows = removed.len().max(added.len());
        for ix in 0..rows {
            out.push((removed.get(ix).copied(), added.get(ix).copied()));
        }
        removed.clear();
        added.clear();
    }
    let mut out = Vec::with_capacity(block.lines.len());
    let mut removed: Vec<&Line> = Vec::new();
    let mut added: Vec<&Line> = Vec::new();
    for line in &block.lines {
        match line.kind {
            Kind::Removed => {
                if !added.is_empty() {
                    flush(&mut out, &mut removed, &mut added);
                }
                removed.push(line);
            }
            Kind::Added => added.push(line),
            Kind::Context => {
                flush(&mut out, &mut removed, &mut added);
                out.push((Some(line), Some(line)));
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
}

/// Lines of a diff quoted into a message: where they are, then the lines as a fenced diff.
///
/// Where they are is "In `src/x.rs` lines 12–18:", by the new file's numbers, the old file's
/// for removals alone. The lines keep their signs, so the model reads what was added and what
/// went.
#[must_use]
pub fn quote(path: &str, lines: &[&Line]) -> String {
    let numbers = |pick: fn(&Line) -> Option<u32>| {
        let mut n = lines.iter().filter_map(|l| pick(l));
        let first = n.next()?;
        Some(n.fold((first, first), |(lo, hi), x| (lo.min(x), hi.max(x))))
    };
    let place = match numbers(|l| l.new).or_else(|| numbers(|l| l.old)) {
        Some((lo, hi)) if lo == hi => format!(" line {lo}"),
        Some((lo, hi)) => format!(" lines {lo}\u{2013}{hi}"),
        None => String::new(),
    };
    let body = lines.iter().fold(String::new(), |mut body, l| {
        body.push(match l.kind {
            Kind::Added => '+',
            Kind::Removed => '-',
            Kind::Context => ' ',
        });
        body.push_str(&l.text);
        body.push('\n');
        body
    });
    format!("In `{path}`{place}:\n```diff\n{body}```\n")
}

/// How many lines a diff shows before it folds, at the summary level: the change is the point
/// of an edit, so it shows unasked, and a long one opens on a click.
pub const SUMMARY_LINES: usize = 12;

#[cfg(test)]
mod tests {
    use super::*;

    fn hunk(lines: &[&str]) -> Patch {
        Patch {
            hunks: vec![Hunk {
                old_start: 10,
                old_lines: 3,
                new_start: 10,
                new_lines: 3,
                heading: None,
                lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            }],
            added: 1,
            removed: 1,
            clipped_lines: 0,
            full: None,
        }
    }

    /// Lines carry both numbers where they are in both files and one where they are in one;
    /// git's note about the last newline flags the line it follows.
    #[test]
    fn lines_are_numbered_on_the_side_they_are_on() {
        let patch = hunk(&[" alpha", "-beta", "+BETA", " gamma", "\\ No newline at end of file"]);
        let blocks = blocks("notes.txt", &patch);
        let lines = &blocks[0].lines;
        let numbers: Vec<_> =
            lines.iter().map(|l| (l.kind, l.old, l.new, l.text.as_str(), l.no_newline)).collect();
        assert_eq!(
            numbers,
            [
                (Kind::Context, Some(10), Some(10), "alpha", false),
                (Kind::Removed, Some(11), None, "beta", false),
                (Kind::Added, None, Some(11), "BETA", false),
                (Kind::Context, Some(12), Some(12), "gamma", true),
            ]
        );
        assert!(lines.iter().all(|l| l.spans.is_none()), "a .txt has no grammar");
    }

    /// Each side is coloured as its own text: the string a removed line opened does not leak
    /// into the added line after it.
    #[test]
    fn each_side_is_coloured_on_its_own() {
        let patch = hunk(&[" let a = 1;", "-let s = \"open", "+let s = 2;", " let b = 3;"]);
        let blocks = blocks("main.rs", &patch);
        let added = blocks[0].lines.iter().find(|l| l.kind == Kind::Added).unwrap();
        let spans = added.spans.as_ref().unwrap();
        assert!(
            spans.iter().all(|s| s.token != highlight::Token::String),
            "the new side has no string: {spans:?}"
        );
        let removed = blocks[0].lines.iter().find(|l| l.kind == Kind::Removed).unwrap();
        assert!(
            removed.spans.as_ref().unwrap().iter().any(|s| s.token == highlight::Token::String)
        );
    }

    /// Side by side, a removal sits beside the addition that replaced it and context beside
    /// itself; an uneven run pads the shorter side.
    #[test]
    fn a_removal_pairs_with_its_replacement() {
        let patch = hunk(&[" a", "-b", "-c", "+B", " d", "+e"]);
        let blocks = blocks("x.txt", &patch);
        let rows: Vec<(Option<&str>, Option<&str>)> = pairs(&blocks[0])
            .into_iter()
            .map(|(l, r)| (l.map(|l| l.text.as_str()), r.map(|r| r.text.as_str())))
            .collect();
        assert_eq!(
            rows,
            [
                (Some("a"), Some("a")),
                (Some("b"), Some("B")),
                (Some("c"), None),
                (Some("d"), Some("d")),
                (None, Some("e")),
            ]
        );
    }

    /// A quote names the file and the new file's lines, and keeps each line's sign.
    #[test]
    fn a_quote_names_the_lines_and_keeps_their_signs() {
        let line = |kind, old, new, text: &str| Line {
            kind,
            old,
            new,
            text: text.to_owned(),
            spans: None,
            no_newline: false,
            emph: Vec::new(),
        };
        let context = line(Kind::Context, Some(11), Some(12), "fn main() {");
        let gone = line(Kind::Removed, Some(12), None, "    old();");
        let came = line(Kind::Added, None, Some(13), "    new();");
        assert_eq!(
            quote("src/x.rs", &[&context, &gone, &came]),
            "In `src/x.rs` lines 12\u{2013}13:\n```diff\n fn main() {\n-    old();\n+    new();\n```\n"
        );
        assert_eq!(quote("a.rs", &[&gone]), "In `a.rs` line 12:\n```diff\n-    old();\n```\n");
    }

    /// Within a removal and the addition that replaced it, only the words that differ are
    /// emphasised, a space between two of them joined; a line rewritten whole pairs with
    /// nothing and is all wash.
    #[test]
    fn the_words_that_differ_are_emphasised() {
        let patch = hunk(&[
            "-use collections::{Bias, HashSet};",
            "-fn old_name() {}",
            "+use collections::{HashMap};",
            "+// something else entirely",
            "+fn new_name() {}",
        ]);
        let blocks = blocks("lib.rs", &patch);
        let emph = |ix: usize| {
            let line = &blocks[0].lines[ix];
            line.emph.iter().filter_map(|r| line.text.get(r.clone())).collect::<Vec<_>>()
        };
        assert_eq!(emph(0), ["Bias, HashSet"], "two changed words and the space between, one run");
        assert_eq!(emph(2), ["HashMap"]);
        assert_eq!(emph(1), ["old_name"], "it pairs past the line rewritten whole");
        assert_eq!(emph(4), ["new_name"]);
        assert!(emph(3).is_empty(), "a line that pairs with none is all wash");
    }

    /// A tab widens to spaces, and the colours after it move with the text.
    #[test]
    fn tabs_widen_and_the_colours_follow() {
        let spans = [
            Span { len: 2, token: highlight::Token::Keyword, italic: false, bold: false },
            Span { len: 3, token: highlight::Token::String, italic: false, bold: false },
        ];
        let (text, spans) = detab("\tx\"a\"", Some(&spans));
        assert_eq!(text, "    x\"a\"");
        let lens: Vec<usize> = spans.unwrap().iter().map(|s| s.len).collect();
        assert_eq!(lens, [5, 3]);
        assert_eq!(lens.iter().sum::<usize>(), text.len());
    }
}
