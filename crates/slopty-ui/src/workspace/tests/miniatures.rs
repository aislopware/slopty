//! The overview's miniatures: each tile's own body drawn at the zoom, named by a label at
//! chrome size, and nothing of the kind while the overview is closed.

use super::*;

/// The overview draws every tile as its miniature, a label at its foot, and the miniature is
/// the live body: a shell's new output redraws its own grid there while a still one is replayed.
/// Closed, no tile has one.
#[gpui::test]
fn an_open_overview_draws_a_miniature_per_tile_and_none_while_closed(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let note = arrives(&view, cx, &fake, ItemKind::Note { text: "Release\n- [ ] tag".into() }, 4);
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
        let label = cx.debug_bounds(selector("miniature-label", t.item)).expect("its label");
        assert!((label.bottom() - mini.bottom()).abs() < px(0.5), "at its foot: {label:?}");
        assert!(label.size.height >= px(chrome), "chrome-sized at any zoom: {label:?}");
    }
    // Nothing is laid over a body any more: the grid itself shows above the label.
    let grid = cx.debug_bounds("terminal").expect("a shell's grid");
    let labels: Vec<_> =
        tiles.iter().filter_map(|t| cx.debug_bounds(selector("miniature-label", t.item))).collect();
    assert!(labels.iter().all(|l| !l.contains(&grid.center())), "{grid:?} under a label");

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
    assert!(busy_after > busy_before, "new output redraws its miniature");
    assert_eq!(quiet_after, quiet_before, "a still one is replayed, not built again");

    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    assert!(none_drawn(cx), "closed again, none");
}
