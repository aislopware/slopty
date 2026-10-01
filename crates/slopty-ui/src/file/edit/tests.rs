use super::*;

/// `text` with `|` marking the caret, or `[` and `]` a selection: the text and the range.
fn marked(marked: &str) -> (Rope, std::ops::Range<usize>) {
    let at = |c: char| marked.find(c);
    let (text, range) = if let Some(caret) = at('|') {
        (marked.replacen('|', "", 1), caret..caret)
    } else {
        let (start, end) = (at('[').unwrap_or(0), at(']').unwrap_or(0).saturating_sub(1));
        (marked.replacen('[', "", 1).replacen(']', "", 1), start..end)
    };
    (Rope::from(text.as_str()), range)
}

/// The text after `edit`, marked as [`marked`] reads it.
fn applied(text: &Rope, edit: &LineEdit) -> String {
    let mut out = text.to_string();
    out.replace_range(edit.range.clone(), &edit.text);
    let (s, e) = (edit.selection.start, edit.selection.end);
    if s == e {
        out.insert(s, '|');
    } else {
        out.insert(e, ']');
        out.insert(s, '[');
    }
    out
}

fn toggled(text: &str, comment: Comment) -> Option<String> {
    let (rope, selection) = marked(text);
    toggle_comment(&rope, &selection, comment).map(|edit| applied(&rope, &edit))
}

#[test]
fn indentation_is_read_from_the_file() {
    let rust = "fn a() {\n    let b = 1;\n    if b {\n        c();\n    }\n}";
    assert_eq!(detect_indent(rust), Indent { hard_tabs: false, width: 4 });
    let yaml = "a:\n  b:\n    c: 1\n  d: 2\ne: 3";
    assert_eq!(detect_indent(yaml), Indent { hard_tabs: false, width: 2 });
    let go = "func a() {\n\tb := 1\n\tif b {\n\t\tc()\n\t}\n}";
    assert_eq!(detect_indent(go), Indent { hard_tabs: true, width: 4 });
    assert_eq!(detect_indent("one\ntwo\n"), Indent::DEFAULT, "nothing indented: the default");
    let doc = "/**\n * One.\n * Two.\n */\nfn a() {\n  b();\n}";
    assert_eq!(detect_indent(doc).width, 2, "a block comment's stars align, not indent");
    let aligned = "call(a,\n     b,\n     c);\nx {\n  y\n}\nz {\n  w\n}";
    assert_eq!(detect_indent(aligned).width, 2, "the step seen most wins over one alignment");
}

#[test]
fn line_endings_and_a_bom_are_kept_apart_and_put_back() {
    // As the worker reads "a\r\nb\r\nc\r\n": the last `\n` taken, its `\r` left.
    let (text, format) = Format::split("a\r\nb\r\nc\r", true);
    assert_eq!((text.as_ref(), format), ("a\nb\nc", Format { crlf: true, bom: false }));
    assert_eq!(
        format.join("a\nb\nc\nd", true),
        "a\r\nb\r\nc\r\nd\r\n",
        "a new line ends as the others"
    );
    let (text, format) = Format::split("one\r", true);
    assert_eq!((text.as_ref(), format.crlf), ("one", true), "one line ending CRLF");
    let (text, format) = Format::split("\u{feff}x\ny", false);
    assert_eq!((text.as_ref(), format), ("x\ny", Format { crlf: false, bom: true }));
    assert_eq!(format.join("x", false), "\u{feff}x");
    let mixed = "a\r\nb\nc";
    let (text, format) = Format::split(mixed, false);
    assert_eq!(text.as_ref(), mixed, "a file that mixes them keeps its bytes");
    assert_eq!(format, Format::default());
    assert_eq!(format.join(mixed, true), "a\r\nb\nc\n");
    assert_eq!(Format::split("no break", false).1, Format::default());
    assert_eq!(Format { crlf: true, bom: false }.label().as_deref(), Some("CRLF"));
    assert_eq!(Format::default().label(), None);
}

#[test]
fn a_line_comment_goes_in_at_the_shallowest_indent_and_comes_out_again() {
    let slash = Comment::Line("//");
    assert_eq!(
        toggled("fn a() {\n    b(|);\n}", slash).as_deref(),
        Some("fn a() {\n    // b(|);\n}")
    );
    assert_eq!(
        toggled("fn a() {\n    // b(|);\n}", slash).as_deref(),
        Some("fn a() {\n    b(|);\n}")
    );
    assert_eq!(
        toggled("[  a\n\n    b\n]c", slash).as_deref(),
        Some("[  // a\n\n  //   b\n]c"),
        "blank lines stay; the line the selection ends at the start of is not taken"
    );
    assert_eq!(
        toggled("[  // a\n  //   b]", slash).as_deref(),
        Some("[  a\n    b]"),
        "every line commented: all come out, with one space each"
    );
    assert_eq!(
        toggled("[// a\nb]", slash).as_deref(),
        Some("[// // a\n// b]"),
        "one line not commented: all go in"
    );
    assert_eq!(toggled("#|x", Comment::Line("#")).as_deref(), Some("|x"), "no space to take");
    assert_eq!(toggled("  |\n", slash), None, "nothing on the line to comment");
}

#[test]
fn a_block_comment_wraps_the_line_or_the_selection() {
    let css = Comment::Block("/*", "*/");
    assert_eq!(toggled("  a { b|: c }", css).as_deref(), Some("  /* a { b|: c } */"));
    assert_eq!(toggled("  /* a { b|: c } */", css).as_deref(), Some("  a { b|: c }"));
    assert_eq!(toggled("a [b] c", css).as_deref(), Some("a [/* b */] c"));
    assert_eq!(toggled("a [/* b */] c", css).as_deref(), Some("a [b] c"));
}

#[test]
fn lines_move_and_copy_with_the_selection() {
    let (rope, caret) = marked("one\ntw|o\nthree");
    let up = move_lines(&rope, &caret, true).map(|e| applied(&rope, &e));
    assert_eq!(up.as_deref(), Some("tw|o\none\nthree"));
    let down = move_lines(&rope, &caret, false).map(|e| applied(&rope, &e));
    assert_eq!(down.as_deref(), Some("one\nthree\ntw|o"));
    let (rope, top) = marked("o|ne\ntwo");
    assert!(move_lines(&rope, &top, true).is_none(), "the first line goes no higher");
    let (rope, block) = marked("[a\nb]\nc");
    let down = move_lines(&rope, &block, false).map(|e| applied(&rope, &e));
    assert_eq!(down.as_deref(), Some("c\n[a\nb]"));
    let (rope, caret) = marked("a\nb|\nc");
    assert_eq!(applied(&rope, &duplicate_lines(&rope, &caret)), "a\nb\nb|\nc");
}

#[test]
fn a_bracket_at_the_caret_finds_its_pair_across_nesting_and_lines() {
    let at = |marked_text: &str| {
        let (rope, caret) = marked(marked_text);
        matching_bracket(&rope, caret.start)
    };
    assert_eq!(at("|(a (b) c)"), Some((0, 8)), "the one after the caret");
    assert_eq!(at("(a (b) c)|"), Some((0, 8)), "else the one before it");
    assert_eq!(at("fn a() {\n  [1, 2]\n|}"), Some((7, 18)), "across lines");
    assert_eq!(at("( [ |) ]"), Some((0, 4)), "another kind does not count");
    assert_eq!(at("|( a"), None, "unmatched");
    assert_eq!(at("a|b"), None);
    let far = format!("|({}", " ".repeat(BRACKET_SCAN_BYTES.saturating_add(1)));
    assert_eq!(at(&format!("{far})")), None, "past the scan's bound: none, at a bounded cost");
}

#[test]
fn go_to_line_takes_a_line_and_a_column_and_clamps_them() {
    let text = Rope::from("one\ntwo\nthrée");
    assert_eq!(line_target("2", &text), Some(4));
    assert_eq!(line_target(" 3:4 ", &text), Some(11), "columns count characters");
    assert_eq!(line_target("3,5", &text), Some(13), "a comma as in a compiler's message");
    assert_eq!(line_target(":2", &text), Some(4));
    assert_eq!(line_target("99", &text), Some(8), "past the end: the last line");
    assert_eq!(line_target("1:99", &text), Some(3), "past the line's end: its end");
    assert_eq!(line_target("two", &text), None);
    assert_eq!(line_target("", &text), None);
}

/// What the editor's helpers cost on the UI thread, at their bounds: the bracket scan that runs
/// on every caret move or keystroke (a pair 64 KiB apart, and an unmatched bracket that scans
/// the whole bound), the indentation guess over its 10 000-line sample, and a comment toggled
/// over 1 000 lines.
#[test]
#[ignore = "timing: cargo nextest run -p slopty-ui --release --run-ignored only timing_of_the_editor_helpers --no-capture"]
fn timing_of_the_editor_helpers() {
    use std::time::{Duration, Instant};

    fn median(mut f: impl FnMut()) -> Duration {
        let mut took: Vec<Duration> = std::iter::repeat_with(|| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .take(21)
        .collect();
        took.sort();
        took.get(10).copied().unwrap_or_default()
    }
    let line =
        "    let value = compute(alpha, beta[gamma]) + other(delta); // a source-like line\n";
    let lines = |n: usize| line.repeat(n);
    let body = lines(BRACKET_SCAN_BYTES / line.len());
    let paired = Rope::from(format!("{{\n{body}}}").as_str());
    let unmatched = Rope::from(format!("{{\n{body}{body}").as_str());
    let pair = median(|| _ = std::hint::black_box(matching_bracket(&paired, 0)));
    let miss = median(|| _ = std::hint::black_box(matching_bracket(&unmatched, 0)));
    let near = median(|| _ = std::hint::black_box(matching_bracket(&paired, paired.len() / 2)));
    let sample = lines(20_000);
    let indent = median(|| _ = std::hint::black_box(detect_indent(&sample)));
    let thousand = Rope::from(lines(1_000).as_str());
    let all = 0..thousand.len();
    let comment = median(|| {
        drop(std::hint::black_box(toggle_comment(&thousand, &all, Comment::Line("//"))));
    });
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "bracket: pair 64 KiB apart {:.0} µs, unmatched {:.0} µs, no bracket at the caret {:.2} µs; \
         indent guess over 10 000 lines {:.0} µs; comment over 1 000 lines {:.0} µs",
        us(pair),
        us(miss),
        us(near),
        us(indent),
        us(comment)
    );
}
