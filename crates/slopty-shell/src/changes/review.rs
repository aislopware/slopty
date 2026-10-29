// slopty: Slopty's own review aids over zeron's diff model (T3 Code's find in diff, whitespace
// and file tree, drawn in zeron's components): the pure halves, kept apart from the copied pane.
//! Find in a diff, the whitespace filter and the changed files as a tree.

use std::ops::Range;

use super::{DiffLine, FileDiff, LineKind};

// ---------------------------------------------------------------------------
// Find
// ---------------------------------------------------------------------------

/// One match of a find in the diff: the line it is on and its bytes in that line's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffMatch {
    pub file: u32,
    pub hunk: u32,
    pub line: u32,
    pub range: Range<usize>,
}

/// Whether `query` is matched without regard to case: it is when it has no capital of its own
/// (smart case, as editors find).
fn folds_case(query: &str) -> bool {
    !query.chars().any(char::is_uppercase)
}

fn fold(c: char, fold: bool) -> char {
    if fold { c.to_lowercase().next().unwrap_or(c) } else { c }
}

/// Every place `query` is found in `text`, as byte ranges, left to right and not overlapping.
pub fn line_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let folding = folds_case(query);
    let needle: Vec<char> = needle.into_iter().map(|c| fold(c, folding)).collect();
    let hay: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        let hit = needle
            .iter()
            .enumerate()
            .all(|(k, c)| fold(hay[i + k].1, folding) == *c);
        if hit {
            let start = hay[i].0;
            let end = hay.get(i + needle.len()).map_or(text.len(), |(at, _)| *at);
            out.push(start..end);
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// Every match of `query` across `files`, in reading order. Notes about a row (`\ No newline…`)
/// are not code and are not searched.
pub fn find_matches(files: &[FileDiff], query: &str) -> Vec<DiffMatch> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for (file_ix, file) in files.iter().enumerate() {
        for (hunk_ix, hunk) in file.hunks.iter().enumerate() {
            for (line_ix, line) in hunk.lines.iter().enumerate() {
                if line.kind == LineKind::Meta {
                    continue;
                }
                for range in line_matches(&line.text, query) {
                    out.push(DiffMatch {
                        file: file_ix as u32,
                        hunk: hunk_ix as u32,
                        line: line_ix as u32,
                        range,
                    });
                }
            }
        }
    }
    out
}

/// The next match from `active` by `delta`, wrapping at either end; the first one when none was.
pub fn step(active: Option<usize>, count: usize, delta: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(active) = active else {
        return Some(if delta < 0 { count - 1 } else { 0 });
    };
    let count = count as isize;
    Some((active as isize + delta).rem_euclid(count) as usize)
}

/// What the find's tally says: "2 of 5", "No results", or nothing before a word is typed.
pub fn tally(query: &str, active: Option<usize>, count: usize) -> String {
    if query.is_empty() {
        String::new()
    } else if count == 0 {
        "No results".to_owned()
    } else {
        format!("{} of {count}", active.map_or(0, |a| a + 1))
    }
}

/// `runs` with each of `marks`' byte ranges given its background, the runs cut where a mark
/// starts or ends.
pub fn mark_runs(
    runs: Vec<gpui::TextRun>,
    marks: &[(Range<usize>, gpui::Hsla)],
) -> Vec<gpui::TextRun> {
    if marks.is_empty() {
        return runs;
    }
    let mut cuts: Vec<usize> = marks.iter().flat_map(|(r, _)| [r.start, r.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out = Vec::with_capacity(runs.len() + cuts.len());
    let mut at = 0usize;
    for run in runs {
        let end = at + run.len;
        let mut start = at;
        for &cut in cuts.iter().filter(|&&cut| cut > at && cut < end) {
            out.push(piece(&run, start, cut, marks));
            start = cut;
        }
        out.push(piece(&run, start, end, marks));
        at = end;
    }
    out
}

fn piece(
    run: &gpui::TextRun,
    start: usize,
    end: usize,
    marks: &[(Range<usize>, gpui::Hsla)],
) -> gpui::TextRun {
    let mut piece = run.clone();
    piece.len = end - start;
    if let Some((_, color)) = marks
        .iter()
        .find(|(range, _)| range.start <= start && end <= range.end && start < end)
    {
        piece.background_color = Some(*color);
    }
    piece
}

// ---------------------------------------------------------------------------
// The scoped capture's asking
// ---------------------------------------------------------------------------

/// What a scoped `GetCheckoutDiff` adds to say that whitespace does not count
/// (`ignoreWhitespace`, `git diff -w`).
pub fn scoped_params(params: &mut serde_json::Map<String, serde_json::Value>, ignore_whitespace: bool) {
    if ignore_whitespace {
        params.insert("ignoreWhitespace".into(), serde_json::Value::Bool(true));
    }
}

// ---------------------------------------------------------------------------
// Whitespace
// ---------------------------------------------------------------------------

fn same_but_space(a: &str, b: &str) -> bool {
    a.chars()
        .filter(|c| !c.is_whitespace())
        .eq(b.chars().filter(|c| !c.is_whitespace()))
}

/// `files` as `git diff -w` reads them: a deletion and the addition it pairs with that differ
/// only in whitespace become one unchanged line (the new text, both numbers), a hunk left with
/// no change goes, and so does a file left with no hunk unless it was added, deleted, renamed or
/// is binary. The files' counts follow.
pub fn ignore_whitespace(files: &mut Vec<FileDiff>) {
    for file in files.iter_mut() {
        for hunk in &mut file.hunks {
            hunk.lines = fold_block_pairs(std::mem::take(&mut hunk.lines));
        }
        file.hunks.retain(|hunk| {
            hunk.lines
                .iter()
                .any(|line| matches!(line.kind, LineKind::Add | LineKind::Del))
        });
        file.additions = count(file, LineKind::Add);
        file.deletions = count(file, LineKind::Del);
    }
    files.retain(|file| {
        !file.hunks.is_empty()
            || file.binary
            || file.old_path.is_some()
            || !matches!(file.status, super::FileStatus::Modified)
    });
}

fn count(file: &FileDiff, kind: LineKind) -> u32 {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind == kind)
        .count() as u32
}

/// One hunk's lines with each run of deletions and the run of additions after it paired in
/// order, a pair equal but for whitespace made context.
fn fold_block_pairs(lines: Vec<DiffLine>) -> Vec<DiffLine> {
    let mut out = Vec::with_capacity(lines.len());
    let mut ix = 0;
    while ix < lines.len() {
        if lines[ix].kind != LineKind::Del {
            out.push(lines[ix].clone());
            ix += 1;
            continue;
        }
        let dels_start = ix;
        while ix < lines.len() && lines[ix].kind == LineKind::Del {
            ix += 1;
        }
        let adds_start = ix;
        while ix < lines.len() && lines[ix].kind == LineKind::Add {
            ix += 1;
        }
        let dels = &lines[dels_start..adds_start];
        let adds = &lines[adds_start..ix];
        let paired = dels.len().min(adds.len());
        let same: Vec<bool> = (0..paired)
            .map(|k| same_but_space(&dels[k].text, &adds[k].text))
            .collect();
        // Deletions first, then additions, as the patch had them; a pair that is the same
        // but for whitespace leaves both and comes back as one line in the additions' place.
        for (k, del) in dels.iter().enumerate() {
            if k < paired && same[k] {
                continue;
            }
            out.push(del.clone());
        }
        for (k, add) in adds.iter().enumerate() {
            if k < paired && same[k] {
                out.push(DiffLine {
                    kind: LineKind::Context,
                    old_no: dels[k].old_no,
                    new_no: add.new_no,
                    text: add.text.clone(),
                });
            } else {
                out.push(add.clone());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The changed files as a tree
// ---------------------------------------------------------------------------

/// One row of the changed files' tree: a folder (its single-child chain joined, `src/ui`) or a
/// file, which names its index in the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    pub file: Option<usize>,
    /// The folder's or the file's whole path.
    pub path: String,
}

#[derive(Default)]
struct Node {
    dirs: std::collections::BTreeMap<String, Node>,
    files: Vec<(String, usize)>,
}

/// The tree of `paths` (each a diff's file, by index), folders first and each level by name, a
/// folder with one folder alone in it joined to it.
pub fn diff_tree<S: AsRef<str>>(paths: &[S]) -> Vec<TreeRow> {
    let mut root = Node::default();
    for (ix, path) in paths.iter().enumerate() {
        let path = path.as_ref();
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let Some(name) = parts.pop() else { continue };
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_owned()).or_default();
        }
        node.files.push((name.to_owned(), ix));
    }
    let mut rows = Vec::new();
    walk(&root, 0, "", &mut rows);
    rows
}

fn walk(node: &Node, depth: usize, prefix: &str, rows: &mut Vec<TreeRow>) {
    for (name, child) in &node.dirs {
        let mut name = name.clone();
        let mut child = child;
        // A folder holding one folder and nothing else reads as one row.
        while child.files.is_empty() && child.dirs.len() == 1 {
            let (next, grand) = child.dirs.iter().next().expect("one folder");
            name = format!("{name}/{next}");
            child = grand;
        }
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        rows.push(TreeRow { depth, name, file: None, path: path.clone() });
        walk(child, depth + 1, &path, rows);
    }
    let mut files = node.files.clone();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, ix) in files {
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        rows.push(TreeRow { depth, name, file: Some(ix), path });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::changes::parse_patch;

    const PATCH: &str = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 fn main() {
-    let Value = 1;
+    let value = 2;
 }
diff --git a/src/ui/b.rs b/src/ui/b.rs
--- a/src/ui/b.rs
+++ b/src/ui/b.rs
@@ -1,2 +1,2 @@
-value(x)
+value( x )
 // value
";

    #[test]
    fn find_counts_and_steps_matches_across_files() {
        let files = parse_patch(PATCH);
        let matches = find_matches(&files, "value");
        // Smart case: "Value" and "value" both, and three in b.rs.
        assert_eq!(matches.len(), 5);
        assert_eq!(matches.iter().filter(|m| m.file == 1).count(), 3);
        assert_eq!(find_matches(&files, "Value").len(), 1, "a capital asks for case");
        let first = &matches[0];
        assert_eq!((first.file, first.hunk, first.line), (0, 0, 1));
        assert_eq!(&files[0].hunks[0].lines[1].text[first.range.clone()], "Value");
        assert_eq!(step(None, 5, 1), Some(0));
        assert_eq!(step(None, 5, -1), Some(4));
        assert_eq!(step(Some(4), 5, 1), Some(0), "wraps forward");
        assert_eq!(step(Some(0), 5, -1), Some(4), "wraps back");
        assert_eq!(step(Some(1), 0, 1), None);
        assert_eq!(tally("value", Some(1), 5), "2 of 5");
        assert_eq!(tally("zzz", None, 0), "No results");
        assert_eq!(tally("", None, 0), "");
        assert!(find_matches(&files, "").is_empty());
    }

    #[test]
    fn matches_keep_their_bytes_beyond_ascii() {
        let ranges = line_matches("é Été été", "été");
        assert_eq!(ranges.len(), 2);
        assert_eq!(&"é Été été"[ranges[0].clone()], "Été");
        assert!(line_matches("aaaa", "aa").len() == 2, "matches do not overlap");
    }

    #[test]
    fn marks_cut_runs_and_paint_only_their_bytes() {
        let run = |len| gpui::TextRun {
            len,
            font: gpui::font("Mono"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked = mark_runs(vec![run(4), run(6)], &[(2..6, gpui::red())]);
        let lens: Vec<usize> = marked.iter().map(|r| r.len).collect();
        assert_eq!(lens, [2, 2, 2, 4]);
        let painted: Vec<bool> = marked.iter().map(|r| r.background_color.is_some()).collect();
        assert_eq!(painted, [false, true, true, false]);
        assert_eq!(lens.iter().sum::<usize>(), 10);
    }

    #[test]
    fn ignoring_whitespace_drops_a_change_only_of_spaces() {
        let mut files = parse_patch(PATCH);
        ignore_whitespace(&mut files);
        assert_eq!(files.len(), 1, "b.rs changed only its spaces");
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));

        let mut moved = parse_patch(
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a  b\n-c\n+a b\n+d\n",
        );
        ignore_whitespace(&mut moved);
        let lines = &moved[0].hunks[0].lines;
        let kinds: Vec<LineKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [LineKind::Del, LineKind::Context, LineKind::Add]);
        assert_eq!((lines[1].old_no, lines[1].new_no), (Some(1), Some(1)));
        assert_eq!(lines[1].text, "a b", "the new side's text");
    }

    #[test]
    fn whitespace_rides_the_scoped_ask() {
        let mut params = serde_json::Map::new();
        scoped_params(&mut params, false);
        assert!(params.is_empty(), "whitespace counted: nothing added");
        scoped_params(&mut params, true);
        assert_eq!(serde_json::Value::Object(params), serde_json::json!({ "ignoreWhitespace": true }));
    }

    #[test]
    fn the_tree_puts_folders_first_and_joins_single_folders() {
        let rows = diff_tree(&["src/ui/b.rs", "src/a.rs", "README.md", "deep/one/two/c.rs"]);
        let shown: Vec<(usize, &str, Option<usize>)> =
            rows.iter().map(|r| (r.depth, r.name.as_str(), r.file)).collect();
        assert_eq!(
            shown,
            [
                (0, "deep/one/two", None),
                (1, "c.rs", Some(3)),
                (0, "src", None),
                (1, "ui", None),
                (2, "b.rs", Some(0)),
                (1, "a.rs", Some(1)),
                (0, "README.md", Some(2)),
            ]
        );
        assert_eq!(rows[4].path, "src/ui/b.rs");
    }
}
