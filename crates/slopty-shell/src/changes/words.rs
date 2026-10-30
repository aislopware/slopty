//! The words a changed line changed, lit inside it (Hunk's and `AgentCraft`'s intraline
//! marks; `.research/design-inspiration-2026-10-19.md` item 14). In a run of deleted lines followed
//! by a run of added ones, each at most [`MOST_PAIRED`] long, each deleted line pairs, in order,
//! with the added line most like it: lines are split into words, spaces and single marks, and two
//! are alike when they share at least half their words and marks ([`LIKENESS`]), so a line added
//! above the edited one never takes its place. The tokens a pair does not share (their longest
//! common subsequence left out) are the changed spans. A line with no partner that alike is
//! rewritten or new, not edited, and lights nothing.

use std::ops::Range;

use super::{DiffLine, Hunk, LineKind};

/// The longest run of deleted or added lines that pairs: past it, lines are rewritten rather than
/// edited, and lighting words in them only adds noise.
pub(super) const MOST_PAIRED: usize = 8;
/// Past this many tokens on a line, its words are not compared (the table grows as their
/// product).
const MOST_TOKENS: usize = 400;
/// The share of their words and marks two lines must have in common to read as one line edited.
const LIKENESS: f32 = 0.5;

/// Each line's changed spans, byte ranges into its text, indexed like the lines.
type LineSpans = Vec<Vec<Range<usize>>>;

/// The changed spans of each of `hunk`'s lines, byte ranges into its text, indexed like its lines.
pub(super) fn hunk_words(hunk: &Hunk) -> LineSpans {
    let mut words = Vec::with_capacity(hunk.lines.len());
    let mut runs = hunk.lines.chunk_by(|a, b| a.kind == b.kind).peekable();
    while let Some(run) = runs.next() {
        let starts = |run: &[DiffLine], kind| run.first().is_some_and(|line| line.kind == kind);
        let adds = if starts(run, LineKind::Del) {
            runs.next_if(|next| starts(next, LineKind::Add))
        } else {
            None
        };
        if let Some(adds) = adds {
            let (old, new) = run_words(run, adds);
            words.extend(old);
            words.extend(new);
        } else {
            words.extend(run.iter().map(|_| Vec::new()));
        }
    }
    words
}

/// The changed spans of a run of deleted lines and the run of added lines after it.
///
/// Each deleted line takes the likest added line after the last one taken; a run past
/// [`MOST_PAIRED`] lights nothing.
fn run_words(dels: &[DiffLine], adds: &[DiffLine]) -> (LineSpans, LineSpans) {
    let mut old_words = vec![Vec::new(); dels.len()];
    let mut new_words = vec![Vec::new(); adds.len()];
    if dels.len() > MOST_PAIRED || adds.len() > MOST_PAIRED {
        return (old_words, new_words);
    }
    let mut next = 0;
    for (old, old_spans) in dels.iter().zip(&mut old_words) {
        let best = adds
            .iter()
            .enumerate()
            .skip(next)
            .filter_map(|(ix, new)| Some((ix, changed_words(&old.text, &new.text)?)))
            .filter(|(_, edit)| edit.likeness >= LIKENESS)
            .max_by(|a, b| a.1.likeness.total_cmp(&b.1.likeness));
        if let Some((ix, edit)) = best {
            *old_spans = edit.old;
            if let Some(new_spans) = new_words.get_mut(ix) {
                *new_spans = edit.new;
            }
            next = ix.saturating_add(1);
        }
    }
    (old_words, new_words)
}

/// `text` as tokens: runs of word characters, runs of spaces, and every other character alone.
fn tokens(text: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Mark,
    }
    let class = |ch: char| {
        if ch.is_alphanumeric() || ch == '_' {
            Class::Word
        } else if ch.is_whitespace() {
            Class::Space
        } else {
            Class::Mark
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<Class> = None;
    for (at, ch) in text.char_indices() {
        let this = class(ch);
        let end = at.saturating_add(ch.len_utf8());
        match out.last_mut() {
            Some(run) if this != Class::Mark && last.as_ref() == Some(&this) => run.end = end,
            _ => out.push(at..end),
        }
        last = Some(this);
    }
    out
}

/// One line edited into another: the spans of each the other does not share, and how alike they
/// are (their shared words and marks over the larger line's, spaces aside).
#[derive(Debug, PartialEq)]
struct Edit {
    old: Vec<Range<usize>>,
    new: Vec<Range<usize>>,
    likeness: f32,
}

/// `old` edited into `new`, each run of changed tokens one span; `None` when the two share no word
/// (a rewrite) or are too long to compare.
fn changed_words(old: &str, new: &str) -> Option<Edit> {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > MOST_TOKENS || b.len() > MOST_TOKENS {
        return None;
    }
    // A token's range always lies on the line's char boundaries.
    let ta: Vec<&str> = a.iter().map(|range| old.get(range.clone()).unwrap_or_default()).collect();
    let tb: Vec<&str> = b.iter().map(|range| new.get(range.clone()).unwrap_or_default()).collect();
    let lcs = suffix_lcs(&ta, &tb);
    let lcs_at = |i: usize, j: usize| lcs.get(i).and_then(|row| row.get(j)).copied().unwrap_or(0);
    // Walk one longest common subsequence from the front, marking the tokens it keeps.
    let (mut kept_a, mut kept_b) = (Vec::with_capacity(ta.len()), Vec::with_capacity(tb.len()));
    let (mut i, mut j) = (0, 0);
    let mut shared_word = false;
    let mut shared: u16 = 0;
    while let (Some(x), Some(y)) = (ta.get(i), tb.get(j)) {
        let (below, across) = (i.saturating_add(1), j.saturating_add(1));
        if x == y {
            kept_a.push(true);
            kept_b.push(true);
            shared_word |= x.chars().any(char::is_alphanumeric);
            shared = shared.saturating_add(u16::from(!x.trim().is_empty()));
            (i, j) = (below, across);
        } else if lcs_at(below, j) >= lcs_at(i, across) {
            kept_a.push(false);
            i = below;
        } else {
            kept_b.push(false);
            j = across;
        }
    }
    kept_a.resize(ta.len(), false);
    kept_b.resize(tb.len(), false);
    if !shared_word {
        return None;
    }
    let solid = |tokens: &[&str]| tokens.iter().filter(|token| !token.trim().is_empty()).count();
    let larger = u16::try_from(solid(&ta).max(solid(&tb)).max(1)).unwrap_or(u16::MAX);
    Some(Edit {
        old: spans(&a, &kept_a),
        new: spans(&b, &kept_b),
        likeness: f32::from(shared) / f32::from(larger),
    })
}

/// The longest common subsequence's length of every pair of suffixes.
///
/// Row `i`, column `j` is that of `a[i..]` and `b[j..]`; the last row and column are 0. Both sides
/// are at most [`MOST_TOKENS`] long, so every length fits a `u16`.
fn suffix_lcs(a: &[&str], b: &[&str]) -> Vec<Vec<u16>> {
    let mut rows = Vec::with_capacity(a.len().saturating_add(1));
    let mut below = vec![0_u16; b.len().saturating_add(1)];
    for x in a.iter().rev() {
        // Built right to left: `right` is the cell just filled, `down` and `diagonal` the row
        // below's cells under it and under its right neighbour.
        let mut row = vec![0_u16];
        let mut right = 0_u16;
        for ((y, &down), &diagonal) in
            b.iter().rev().zip(below.iter().rev().skip(1)).zip(below.iter().rev())
        {
            right = if x == y { diagonal.saturating_add(1) } else { down.max(right) };
            row.push(right);
        }
        row.reverse();
        rows.push(std::mem::replace(&mut below, row));
    }
    rows.push(below);
    rows.reverse();
    rows
}

/// The changed tokens of a line merged into spans, a run of them one span.
fn spans(tokens: &[Range<usize>], kept: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (token, kept) in tokens.iter().zip(kept) {
        if *kept {
            continue;
        }
        match out.last_mut() {
            Some(span) if span.end == token.start => span.end = token.end,
            _ => out.push(token.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine { kind, old_no: None, new_no: None, text: text.to_owned() }
    }

    /// An edited line lights only what changed on each side; a rewrite, a lone line and a run
    /// past eight lines light nothing.
    #[test]
    fn a_changed_line_lights_only_its_changed_words() {
        // The e2e repo's edit: a line added above the edited one does not take its place.
        let old = r#"    println!("hello");"#;
        let new = r#"    println!("hello, {name}");"#;
        let hunk = Hunk {
            header: "@@ -1,3 +1,4 @@".into(),
            lines: vec![
                line(LineKind::Context, "fn main() {"),
                line(LineKind::Del, old),
                line(LineKind::Add, r#"    let name = "surfaces";"#),
                line(LineKind::Add, new),
                line(LineKind::Context, "}"),
                line(LineKind::Add, "let lonely = 1;"),
            ],
        };
        let words = hunk_words(&hunk);
        assert_eq!(words[1], Vec::<Range<usize>>::new(), "nothing removed");
        assert!(words[2].is_empty(), "the new line is new, not edited");
        let lit: Vec<&str> = words[3].iter().filter_map(|span| new.get(span.clone())).collect();
        assert_eq!(lit, [", {name}"]);
        assert!(words[0].is_empty() && words[4].is_empty() && words[5].is_empty());

        assert_eq!(
            changed_words("let a = 1;", "let b = 2;").map(|edit| edit.old),
            Some(vec![4..5, 8..9])
        );
        assert_eq!(changed_words("alpha beta", "gamma delta"), None, "a rewrite lights nothing");

        let mut long = vec![line(LineKind::Del, "x = 1"); MOST_PAIRED + 1];
        long.extend(vec![line(LineKind::Add, "x = 2"); MOST_PAIRED + 1]);
        let words = hunk_words(&Hunk { header: String::new(), lines: long });
        assert!(words.iter().all(Vec::is_empty), "a long run is rewritten, not edited");
    }
}
