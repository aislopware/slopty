//! The overview's miniatures: each tile's own body drawn at the zoom, a tile of words summed
//! up over it at chrome size, and nothing of the kind while the overview is closed.

use super::*;

/// The overview draws every tile as its miniature. A tile of words is summed up over its whole
/// body, not shown as unreadable text a few points high, and once the overview rests the body
/// under a summary is not drawn: a shell's new output there costs no frame of its grid. Only
/// the focused one's grid is drawn, as it keeps the keyboard. Closed, no tile has one.
#[gpui::test]
fn an_open_overview_draws_a_miniature_per_tile_and_none_while_closed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let note = arrives(&view, cx, &fake, ItemKind::Folder { path: "/w/release".into() }, 4);
    let file = arrives(&view, cx, &fake, ItemKind::File { path: "/w/src/main.rs".into() }, 5);
    for (session, _) in &shells {
        view.update_in(cx, |v, _w, cx| v.term_event(*session, frame(&["~ % cargo test", ""]), cx));
    }
    cx.run_until_parked();
    let tiles: Vec<TileRef> = shells.iter().map(|(_, t)| *t).chain([note, file]).collect();
    let none_drawn = |cx: &mut VisualTestContext| {
        tiles.iter().all(|t| cx.debug_bounds(selector("miniature", t.item)).is_none())
    };
    assert!(none_drawn(cx), "no miniature while the overview is closed");

    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    let zoom = view.read_with(cx, |v, _| v.drawn.zoom.get());
    assert!(zoom < tile::SHAPES_BELOW, "five columns zoom under half: {zoom}");
    let chrome = Theme::default().typography.icon_large();
    for t in &tiles {
        let pane = cx.debug_bounds(selector("item", t.item)).expect("the tile");
        let mini = cx.debug_bounds(selector("miniature", t.item)).expect("its miniature");
        assert!(pane.contains(&mini.center()), "{mini:?} inside {pane:?}");
        let label = cx.debug_bounds(selector("miniature-label", t.item)).expect("its summary");
        assert!((label.bottom() - mini.bottom()).abs() < px(0.5), "to its foot: {label:?}");
        assert!((label.top() - mini.top()).abs() < px(0.5), "over its whole body: {label:?}");
        let name = cx.debug_bounds(selector("shapes-label", t.item)).expect("its name row");
        assert!(name.size.height >= px(chrome), "chrome-sized at any zoom: {name:?}");
    }
    // The focused grid is drawn under its summary, keeping the keyboard, not read.
    let grid = cx.debug_bounds("terminal").expect("the focused shell's grid");
    let labels: Vec<_> =
        tiles.iter().filter_map(|t| cx.debug_bounds(selector("miniature-label", t.item))).collect();
    assert!(labels.iter().any(|l| l.contains(&grid.center())), "{grid:?} under its summary");

    let (quiet, busy) = (shells[0].0, shells[1].0);
    let term = |s: SessionId, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.terminal(s).cloned()).expect("a shell's view")
    };
    let (busy_view, quiet_view) = (term(busy, cx), term(quiet, cx));
    let renders = |cx: &mut VisualTestContext| {
        (busy_view.read_with(cx, |t, _| t.renders()), quiet_view.read_with(cx, |t, _| t.renders()))
    };
    let (busy_before, quiet_before) = renders(cx);
    view.update_in(cx, |v, _w, cx| v.term_event(busy, frame(&["~ % cargo test", "ok"]), cx));
    cx.run_until_parked();
    let (busy_after, quiet_after) = renders(cx);
    assert_eq!(busy_after, busy_before, "a covered shell's output draws nothing");
    assert_eq!(quiet_after, quiet_before, "nor does a still one");

    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    assert!(none_drawn(cx), "closed again, none");
}

/// A summary always says three facts about the tile's own work and quotes its text: a shell
/// how it stands, what it ran last and how that ended, and where it is, over its last rows; a
/// program run bare what it runs; a Markdown checklist its kind and length, its progress and
/// its folder, over its first lines. What it quotes is copied as the overview opens and again
/// when a command starts or ends, never with each line of output, so a busy shell under a
/// resting overview draws nothing.
#[gpui::test]
fn a_summary_says_three_facts_and_quotes_its_text(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let key = fake.key;
    let [(shell, shell_tile), (program, program_tile), _] = three_shells(&view, cx, &fake);
    let mut bare = summary(program, None);
    bare.command = vec!["/bin/cat".to_owned()];
    view.update_in(cx, |v, _w, cx| {
        v.session_opened(key, summary(shell, Some("/Users/me/oss/slopty")), cx);
        v.session_opened(key, bare, cx);
    });
    let typed = SemanticMark::Prompt { exit: None, input: Some(2) };
    let failed = SemanticMark::Prompt { exit: Some(1), input: Some(2) };
    let rows = [("$ cargo test", typed), ("error: 2 failed", SemanticMark::Output), ("$ ", failed)];
    view.update_in(cx, |v, _w, cx| {
        v.term_event(shell, marked_frame(1, &rows, 2), cx);
        v.term_event(program, frame(&["one", "two", ""]), cx);
    });
    let path = "/w/RELEASE.md";
    let file = arrives(&view, cx, &fake, ItemKind::File { path: path.to_owned() }, 4);
    let text = slopty_proto::file::FileRead::Text {
        text: "# Release\n\n- [x] build\n- [ ] notarise\n- [ ] tag\n".to_owned(),
        size: 46,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    view.update_in(cx, |v, _w, cx| v.file_read(key, path, &text, cx));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    let says = |tile: TileRef, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.summary_words(tile, cx))
    };

    let (facts, tail) = says(shell_tile, cx);
    assert_eq!(facts[0], "At the prompt");
    assert_eq!(facts[1], "cargo test \u{b7} Exit 1");
    assert!(facts[2].contains("slopty"), "where it is: {facts:?}");
    assert_eq!(tail.last().map(String::as_str), Some("$"), "its rows, the newest last: {tail:?}");
    assert!(tail.iter().any(|r| r == "error: 2 failed"), "{tail:?}");

    let (facts, tail) = says(program_tile, cx);
    assert_eq!(facts[0], "Running cat");
    assert_eq!(facts[1], "80 \u{d7} 24", "a quieter fact, never a blank");
    assert_eq!(facts[2], "studio", "its machine, where nothing says its place");
    assert_eq!(tail, ["one", "two"]);

    let (facts, tail) = says(file, cx);
    assert!(facts[0].starts_with("Markdown \u{b7} "), "its kind and length: {facts:?}");
    assert_eq!(facts[1], "1 of 3 done");
    assert!(!facts[2].is_empty(), "its folder: {facts:?}");
    assert_eq!(tail.first().map(String::as_str), Some("# Release"), "its first lines: {tail:?}");

    // Output under the resting overview is not copied: the quote stays as it was.
    view.update_in(cx, |v, _w, cx| v.term_event(program, frame(&["one", "two", "three"]), cx));
    cx.run_until_parked();
    assert_eq!(says(program_tile, cx).1, ["one", "two"], "copied once, not per line");
    // A command that ends is a moment the shell's rows are read again.
    let passed = SemanticMark::Prompt { exit: Some(0), input: Some(2) };
    let rows = [("$ ls", typed), ("Cargo.toml", SemanticMark::Output), ("$ ", passed)];
    view.update_in(cx, |v, _w, cx| v.term_event(shell, marked_frame(2, &rows, 2), cx));
    cx.run_until_parked();
    let (facts, tail) = says(shell_tile, cx);
    assert_eq!(facts[1], "ls \u{b7} Done");
    assert!(tail.iter().any(|r| r == "Cargo.toml"), "{tail:?}");

    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    assert!(says(shell_tile, cx).1.is_empty(), "closed, nothing copied is kept");
}
