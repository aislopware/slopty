//! An edit's patch as the face draws it: each line with its old and new numbers and its
//! syntax colours, in one column or paired side by side.
//!
//! Each hunk's two sides are parsed as texts of their own (the old side is its context and
//! removed lines, the new side its context and added lines), so a removed line that opens a
//! block comment does not colour the lines that replaced it. A hunk is parsed apart from the
//! next, which sits elsewhere in the file.

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
    /// Git's "\ No newline at end of file".
    Meta,
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

fn block(hunk: &Hunk, syntax: Option<Syntax>) -> Block {
    let parsed: Vec<(Kind, &str)> = hunk
        .lines
        .iter()
        .map(|line| {
            let mut chars = line.chars();
            let kind = match chars.next() {
                Some('+') => Kind::Added,
                Some('-') => Kind::Removed,
                Some('\\') => Kind::Meta,
                _ => Kind::Context,
            };
            let text = if kind == Kind::Meta { line.as_str() } else { chars.as_str() };
            (kind, text)
        })
        .collect();
    let side = |skip: Kind| -> String {
        parsed
            .iter()
            .filter(|(kind, _)| *kind != skip && *kind != Kind::Meta)
            .map(|(_, text)| *text)
            .collect::<Vec<_>>()
            .join("\n")
    };
    let spans = syntax.map(|syntax| {
        (
            highlight::spans(&side(Kind::Added), syntax),
            highlight::spans(&side(Kind::Removed), syntax),
        )
    });
    let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
    let (mut old_at, mut new_at) = (0_usize, 0_usize);
    let mut lines = Vec::with_capacity(parsed.len());
    for (kind, text) in parsed {
        let (old, new, from_old) = match kind {
            Kind::Context => (Some(old_no), Some(new_no), false),
            Kind::Removed => (Some(old_no), None, true),
            Kind::Added => (None, Some(new_no), false),
            Kind::Meta => (None, None, false),
        };
        let coloured = match (&spans, kind) {
            (_, Kind::Meta) | (None, _) => None,
            (Some((old_side, _)), _) if from_old => old_side.get(old_at).cloned(),
            (Some((_, new_side)), _) => new_side.get(new_at).cloned(),
        };
        if matches!(kind, Kind::Context | Kind::Removed) {
            old_no = old_no.saturating_add(1);
            old_at = old_at.saturating_add(1);
        }
        if matches!(kind, Kind::Context | Kind::Added) {
            new_no = new_no.saturating_add(1);
            new_at = new_at.saturating_add(1);
        }
        lines.push(Line { kind, old, new, text: text.to_owned(), spans: coloured });
    }
    Block { new_start: hunk.new_start, lines }
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
            Kind::Context | Kind::Meta => {
                flush(&mut out, &mut removed, &mut added);
                out.push((Some(line), Some(line)));
            }
        }
    }
    flush(&mut out, &mut removed, &mut added);
    out
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
                lines: lines.iter().map(|l| (*l).to_owned()).collect(),
            }],
            added: 1,
            removed: 1,
            clipped_lines: 0,
            full: None,
        }
    }

    /// Lines carry both numbers where they are in both files, one where they are in one,
    /// and none for git's note about the last newline.
    #[test]
    fn lines_are_numbered_on_the_side_they_are_on() {
        let patch = hunk(&[" alpha", "-beta", "+BETA", " gamma", "\\ No newline at end of file"]);
        let blocks = blocks("notes.txt", &patch);
        let lines = &blocks[0].lines;
        let numbers: Vec<(Kind, Option<u32>, Option<u32>, &str)> =
            lines.iter().map(|l| (l.kind, l.old, l.new, l.text.as_str())).collect();
        assert_eq!(
            numbers,
            [
                (Kind::Context, Some(10), Some(10), "alpha"),
                (Kind::Removed, Some(11), None, "beta"),
                (Kind::Added, None, Some(11), "BETA"),
                (Kind::Context, Some(12), Some(12), "gamma"),
                (Kind::Meta, None, None, "\\ No newline at end of file"),
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
}
