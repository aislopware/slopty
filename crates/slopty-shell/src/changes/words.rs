//! The words a changed line changed, lit inside it (Hunk's and `AgentCraft`'s intraline
//! marks; `.research/design-inspiration-2026-10-19.md` item 14). In a run of deleted lines followed
//! by a run of added ones, each at most [`MOST_PAIRED`] long, each deleted line pairs, in order,
//! with the added line most like it: lines are split into words, spaces and single marks, and two
//! are alike when they share at least half their words and marks ([`LIKENESS`]), so a line added
//! above the edited one never takes its place. The tokens a pair does not share (their longest
//! common subsequence left out) are the changed spans. A line with no partner that alike is
//! rewritten or new, not edited, and lights nothing.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "shell lint migration: slice D"
)]

use std::ops::Range;

use super::{Hunk, LineKind};

/// The longest run of deleted or added lines that pairs: past it, lines are rewritten rather than
/// edited, and lighting words in them only adds noise.
pub(super) const MOST_PAIRED: usize = 8;
/// Past this many tokens on a line, its words are not compared (the table grows as their
/// product).
const MOST_TOKENS: usize = 400;
/// The share of their words and marks two lines must have in common to read as one line edited.
const LIKENESS: f32 = 0.5;

/// The changed spans of each of `hunk`'s lines, byte ranges into its text, indexed like its lines.
pub(super) fn hunk_words(hunk: &Hunk) -> Vec<Vec<Range<usize>>> {
    let lines = &hunk.lines;
    let mut words = vec![Vec::new(); lines.len()];
    let mut at = 0;
    while at < lines.len() {
        let dels = lines[at..].iter().take_while(|line| line.kind == LineKind::Del).count();
        let adds = lines[at + dels..].iter().take_while(|line| line.kind == LineKind::Add).count();
        if dels == 0 || adds == 0 {
            at += dels.max(1) + adds;
            continue;
        }
        if dels <= MOST_PAIRED && adds <= MOST_PAIRED {
            // Each deleted line takes the likest added line after the last one taken.
            let mut next = at + dels;
            for old in at..at + dels {
                let best = (next..at + dels + adds)
                    .filter_map(|new| {
                        Some((new, changed_words(&lines[old].text, &lines[new].text)?))
                    })
                    .filter(|(_, edit)| edit.likeness >= LIKENESS)
                    .max_by(|a, b| a.1.likeness.total_cmp(&b.1.likeness));
                if let Some((new, edit)) = best {
                    words[old] = edit.old;
                    words[new] = edit.new;
                    next = new + 1;
                }
            }
        }
        at += dels + adds;
    }
    words
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
        match out.last_mut() {
            Some(run) if this != Class::Mark && last.as_ref() == Some(&this) => {
                run.end = at + ch.len_utf8();
            }
            _ => out.push(at..at + ch.len_utf8()),
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
    let (ta, tb): (Vec<&str>, Vec<&str>) =
        (a.iter().map(|r| &old[r.clone()]).collect(), b.iter().map(|r| &new[r.clone()]).collect());
    // The longest common subsequence's lengths of every pair of suffixes.
    let width = tb.len() + 1;
    let mut table = vec![0_u16; (ta.len() + 1) * width];
    for i in (0..ta.len()).rev() {
        for j in (0..tb.len()).rev() {
            table[i * width + j] = if ta[i] == tb[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let (mut kept_a, mut kept_b) = (vec![false; ta.len()], vec![false; tb.len()]);
    let (mut i, mut j) = (0, 0);
    let mut shared_word = false;
    let mut shared: u16 = 0;
    while i < ta.len() && j < tb.len() {
        if ta[i] == tb[j] {
            kept_a[i] = true;
            kept_b[j] = true;
            shared_word |= ta[i].chars().any(char::is_alphanumeric);
            shared += u16::from(!ta[i].trim().is_empty());
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
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
    use crate::changes::DiffLine;

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
        let lit: Vec<&str> = words[3].iter().map(|span| &new[span.clone()]).collect();
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
