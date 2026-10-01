//! The editor's own commands in a real tile, by their default keys: comment, move and copy
//! lines, go to line, the bracket pair, the file's indentation, its line ends and BOM, wrap.

use super::*;

/// The keyboard in the editor with the selection at `range`.
fn select(view: &Entity<FileView>, cx: &mut VisualTestContext, range: std::ops::Range<usize>) {
    view.update_in(cx, |v, window, cx| {
        v.focus(window, cx);
        v.editor().update(cx, |e, cx| e.set_selected_range(range, cx));
    });
    cx.run_until_parked();
}

fn selection(view: &Entity<FileView>, cx: &VisualTestContext) -> std::ops::Range<usize> {
    view.read_with(cx, |v, cx| v.editor().read(cx).selected_range())
}

fn keys(cx: &mut VisualTestContext, keys: &str) {
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

#[gpui::test]
fn cmd_slash_comments_the_line_in_the_file_s_language_and_undoes_in_one_step(
    cx: &mut TestAppContext,
) {
    let (view, _events, cx) = tile(cx, "/w/src/main.rs");
    arrives(&view, cx, text_read("fn a() {\n    b();\n}", true, 1));
    select(&view, cx, 13..13);
    keys(cx, "cmd-/");
    assert_eq!(text(&view, cx), "fn a() {\n    // b();\n}", "Rust's `//` at the line's indent");
    assert_eq!(selection(&view, cx), 16..16, "the caret stays on its character");
    assert!(view.read_with(cx, |v, _| v.dirty()), "a command's edit is an edit");
    keys(cx, "cmd-/");
    assert_eq!(text(&view, cx), "fn a() {\n    b();\n}", "and comes out again");
    keys(cx, "cmd-/");
    keys(cx, "cmd-z");
    assert_eq!(text(&view, cx), "fn a() {\n    b();\n}", "one ⌘Z takes a toggle back");
}

#[gpui::test]
fn cmd_slash_comments_every_selected_line_in_python(cx: &mut TestAppContext) {
    let (py, _events, cx) = tile(cx, "/w/run.py");
    arrives(&py, cx, text_read("a = 1\nb = 2", true, 1));
    select(&py, cx, 0..9);
    keys(cx, "cmd-/");
    assert_eq!(text(&py, cx), "# a = 1\n# b = 2", "Python's `#`, every selected line");
}

#[gpui::test]
fn lines_move_and_copy_with_the_option_arrows(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/list.txt");
    arrives(&view, cx, text_read("one\ntwo\nthree", true, 1));
    select(&view, cx, 5..5);
    keys(cx, "alt-down");
    assert_eq!(text(&view, cx), "one\nthree\ntwo");
    assert_eq!(selection(&view, cx), 11..11, "the caret goes with its line");
    keys(cx, "alt-up alt-up");
    assert_eq!(text(&view, cx), "two\none\nthree");
    keys(cx, "alt-up");
    assert_eq!(text(&view, cx), "two\none\nthree", "the first line goes no higher");
    keys(cx, "alt-shift-down");
    assert_eq!(text(&view, cx), "two\ntwo\none\nthree");
    assert_eq!(selection(&view, cx), 5..5, "on the copy");
}

#[gpui::test]
fn ctrl_g_goes_to_a_line_as_it_is_typed_and_esc_comes_back(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/src/lib.rs");
    arrives(&view, cx, text_read("a\nbb\nccc\ndddd", true, 1));
    select(&view, cx, 1..1);
    keys(cx, "ctrl-g");
    assert!(view.read_with(cx, |v, _| v.going_to_line()));
    assert!(cx.debug_bounds("file-goto").is_some(), "the field is drawn");
    cx.simulate_input("3:2");
    cx.run_until_parked();
    assert_eq!(selection(&view, cx), 6..6, "the caret follows what is typed");
    keys(cx, "escape");
    assert!(!view.read_with(cx, |v, _| v.going_to_line()));
    assert_eq!(selection(&view, cx), 1..1, "Esc puts it back");

    keys(cx, "ctrl-g");
    cx.simulate_input("4");
    keys(cx, "enter");
    assert!(!view.read_with(cx, |v, _| v.going_to_line()));
    assert_eq!(selection(&view, cx), 9..9, "↩ keeps it there");
    keys(cx, "x");
    assert_eq!(text(&view, cx), "a\nbb\nccc\nxdddd", "and the editor has the keyboard again");
}

#[gpui::test]
fn the_bracket_pair_at_the_caret_is_framed_and_jumped_between(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/src/lib.rs");
    arrives(&view, cx, text_read("fn a() {\n    b(c[1]);\n}", true, 1));
    select(&view, cx, 7..7);
    assert_eq!(view.read_with(cx, |v, _| v.bracket_pair()), Some((7, 22)));
    keys(cx, "cmd-shift-\\");
    assert_eq!(selection(&view, cx), 22..22, "to the closing one");
    keys(cx, "cmd-shift-\\");
    assert_eq!(selection(&view, cx), 7..7, "and back");
    select(&view, cx, 3..3);
    assert_eq!(view.read_with(cx, |v, _| v.bracket_pair()), None, "no bracket at the caret");
    select(&view, cx, 0..2);
    assert_eq!(view.read_with(cx, |v, _| v.bracket_pair()), None, "none under a selection");
}

#[gpui::test]
fn tab_follows_the_file_s_own_indentation(cx: &mut TestAppContext) {
    let (go, _events, cx) = tile(cx, "/w/main.go");
    arrives(&go, cx, text_read("func a() {\n\tb()\n}", true, 1));
    assert_eq!(go.read_with(cx, |v, _| v.indent()), Indent { hard_tabs: true, width: 4 });
    select(&go, cx, 15..15);
    keys(cx, "enter tab");
    assert_eq!(text(&go, cx), "func a() {\n\tb()\n\t\t\n}", "a tab file indents with tabs");
}

#[gpui::test]
fn tab_puts_in_the_spaces_the_file_indents_with(cx: &mut TestAppContext) {
    let (rust, _events, cx) = tile(cx, "/w/lib.rs");
    arrives(&rust, cx, text_read("fn a() {\n    b();\n}", true, 1));
    assert_eq!(rust.read_with(cx, |v, _| v.indent()), Indent { hard_tabs: false, width: 4 });
    select(&rust, cx, 0..0);
    keys(cx, "tab");
    assert_eq!(text(&rust, cx), "    fn a() {\n    b();\n}", "four spaces, as the file has");
}

#[gpui::test]
fn crlf_line_ends_and_a_bom_are_kept_through_an_edit(cx: &mut TestAppContext) {
    let (view, events, cx) = tile(cx, "/w/win.txt");
    // The worker reads "\u{feff}a\r\nb\r\n": its last `\n` off, the `\r` before it left.
    arrives(&view, cx, text_read("\u{feff}a\r\nb\r", true, 1));
    assert_eq!(text(&view, cx), "a\nb", "the editor holds plain lines");
    assert_eq!(
        view.read_with(cx, |v, _| v.format_label()).as_deref(),
        Some("CRLF, UTF-8 with BOM")
    );
    select(&view, cx, 3..3);
    keys(cx, "enter c");
    keys(cx, "cmd-s");
    assert_eq!(
        events.borrow().last(),
        Some(&FileViewEvent::Save {
            text: "\u{feff}a\r\nb\r\nc\r\n".to_owned(),
            base_modified_ms: Some(WallMs::from_millis(1)),
        }),
        "a new line ends as the file's do, and the BOM stays"
    );
    // The save's echo is the same version, so nothing changes.
    view.update(cx, |v, cx| {
        v.written(WriteResult::Saved { size: 12, modified_ms: WallMs::from_millis(2) }, cx);
    });
    arrives(&view, cx, text_read("\u{feff}a\r\nb\r\nc\r", true, 2));
    view.read_with(cx, |v, _| assert!(!v.dirty() && v.trouble().is_none(), "{v:?}"));
}

#[gpui::test]
fn markdown_wraps_and_the_palette_turns_it_off(cx: &mut TestAppContext) {
    let (view, _events, cx) = tile(cx, "/w/README.md");
    arrives(&view, cx, text_read("# Title\n\nA long paragraph.", true, 1));
    assert!(view.read_with(cx, |v, _| v.soft_wrap()), "prose wraps");
    view.update(cx, FileView::toggle_soft_wrap);
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.soft_wrap()));
}

#[gpui::test]
fn code_does_not_wrap(cx: &mut TestAppContext) {
    let (code, _events, cx) = tile(cx, "/w/lib.rs");
    arrives(&code, cx, text_read("fn a() {}", true, 1));
    assert!(!code.read_with(cx, |v, _| v.soft_wrap()), "code does not");
}

#[test]
fn the_editor_s_chrome_and_palette_lines_are_sentence_case() {
    let words: Vec<String> = editor_palette_items(&[])
        .into_iter()
        .map(|item| item.label)
        .chain([super::super::editing::GO_TO_LINE.to_owned()])
        .collect();
    for word in words {
        let mut chars = word.chars();
        assert!(chars.next().is_some_and(char::is_uppercase), "{word}");
        assert!(
            word.split(' ').skip(1).all(|w| w.chars().next().is_none_or(|c| !c.is_uppercase())),
            "{word}"
        );
    }
}
