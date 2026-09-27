//! The strip's own marks in the headless workspace: the resize handle on a divider, the drop
//! hints, the overview's blocks and the empty workspace's worker list.

use gpui::{Bounds, Modifiers, MouseButton};

use super::*;

/// Within a point: bounds are laid out in floats.
#[track_caller]
fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.5, "{a} vs {b}");
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"))
}

/// The quads the last frame painted exactly over `at`.
fn quads_at(cx: &mut VisualTestContext, at: Bounds<Pixels>) -> Vec<gpui::Quad> {
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let same = |scaled: gpui::ScaledPixels, logical: Pixels| {
        f32::from(logical).mul_add(-scale, scaled.0).abs() < 0.5
    };
    quads
        .into_iter()
        .filter(|q| {
            same(q.bounds.origin.x, at.origin.x)
                && same(q.bounds.origin.y, at.origin.y)
                && same(q.bounds.size.width, at.size.width)
                && same(q.bounds.size.height, at.size.height)
        })
        .collect()
}

fn square(q: &gpui::Quad) -> bool {
    let r = q.corner_radii;
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left].iter().all(|c| c.0 == 0.0)
}

fn accent() -> gpui::Hsla {
    crate::colors::hsla(Theme::default().surfaces.accent)
}

/// The accent as a mark: what a drop hint is drawn in.
fn mark() -> gpui::Hsla {
    crate::colors::hsla(Theme::default().surfaces.accent_fill)
}

/// Whether the last frame filled `at` with `ink`.
fn filled(cx: &mut VisualTestContext, at: Bounds<Pixels>, ink: gpui::Hsla) -> bool {
    quads_at(cx, at).iter().any(|q| q.background.as_solid() == Some(ink))
}

/// The handle between two columns straddles their divider: a 12 pt target centred on the line,
/// whose accent line lies over the divider under the pointer and while dragged. Dragging it
/// widens the column on its left by what the pointer travelled, and the next column still
/// starts where it ends.
#[gpui::test]
fn the_handle_straddles_the_divider_and_resizes_the_column(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let (left, right) =
        (bounds(cx, selector("item", first.item)), bounds(cx, selector("item", second.item)));
    near(f32::from(left.right()), f32::from(right.left()));

    let handle = bounds(cx, "divider-0");
    near(f32::from(handle.size.width), 12.0);
    near(f32::from(handle.center().x), f32::from(left.right()));
    near(f32::from(handle.top()), f32::from(left.top()));
    near(f32::from(handle.size.height), f32::from(left.size.height));
    let line = bounds(cx, "divider-line-0");
    near(f32::from(line.size.width), 1.0);
    near(f32::from(line.right()), f32::from(left.right()));
    assert!(!filled(cx, line, accent()), "the line is quiet until the pointer comes");

    let grab = handle.center();
    cx.simulate_mouse_move(grab, None, Modifiers::default());
    assert!(filled(cx, line, accent()), "the line lights under the pointer");

    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    let to = point(grab.x + px(60.0), grab.y);
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    let dragged = bounds(cx, "divider-line-0");
    assert!(filled(cx, dragged, accent()), "the line stays lit while dragged");
    near(f32::from(dragged.right()), f32::from(left.right()) + 60.0);
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    let (left_after, right_after) =
        (bounds(cx, selector("item", first.item)), bounds(cx, selector("item", second.item)));
    near(f32::from(left_after.size.width), f32::from(left.size.width) + 60.0);
    near(f32::from(right_after.left()), f32::from(left_after.right()));
}

/// A header dragged near a column's edge draws a 2 pt accent line centred on the divider the
/// new column would open, over a faint wash as wide as the column the tile brings; over the
/// middle of a column of one it washes the half the tile would take, with no corner and no
/// frame.
#[gpui::test]
fn the_drop_line_sits_on_the_divider_and_a_join_washes_its_share(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    let header = bounds(cx, selector("title", first.item)).center();
    let column = bounds(cx, selector("item", second.item));
    cx.simulate_mouse_down(header, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        point(header.x + px(20.0), header.y),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    // Just inside the second column's left edge: a new column between the first two.
    let edge = point(column.left() + px(10.0), column.center().y);
    cx.simulate_mouse_move(edge, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    let line = bounds(cx, "drop-hint");
    near(f32::from(line.size.width), 2.0);
    near(f32::from(line.center().x), f32::from(column.left()));
    near(f32::from(line.top()), f32::from(column.top()));
    near(f32::from(line.size.height), f32::from(column.size.height));
    assert!(filled(cx, line, mark()), "the line is the accent's mark");
    let room = bounds(cx, "drop-wash");
    let own = bounds(cx, selector("item", first.item));
    near(f32::from(room.left()), f32::from(column.left()));
    near(f32::from(room.size.width), f32::from(own.size.width));
    near(f32::from(room.size.height), f32::from(column.size.height));

    cx.simulate_mouse_move(column.center(), Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("drop-hint").is_none(), "joining draws no line");
    let wash = bounds(cx, "drop-wash");
    near(f32::from(wash.left()), f32::from(column.left()));
    near(f32::from(wash.size.width), f32::from(column.size.width));
    near(f32::from(wash.top()), f32::from(column.center().y));
    near(f32::from(wash.bottom()), f32::from(column.bottom()));
    let faint = mark().opacity(slopty_theme::alpha::FAINT);
    let quads = quads_at(cx, wash);
    let quad = quads.iter().find(|q| q.background.as_solid() == Some(faint)).expect("a wash");
    assert!(square(quad), "no corner: {quad:?}");
    let b = quad.border_widths;
    assert!([b.top, b.right, b.bottom, b.left].iter().all(|w| w.0 == 0.0), "no frame: {quad:?}");
    cx.simulate_mouse_up(column.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

/// In the overview a workspace with tiles is one lifted card round its panes: the elevated
/// surface a base unit wider all round, rounded (lifted by `kit::elevate`), and a 2 pt accent
/// ring round the active one only. The place for a new workspace is a card of the same size
/// under the last, only its outline, and a click on it opens that workspace. No tile's header
/// is a band of its own at that zoom: every header is the body's surface, with no hairline.
#[gpui::test]
fn the_overview_lifts_each_workspace_and_offers_a_new_one(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    // The focused tile goes down into a workspace of its own: two with tiles, then the empty.
    cx.simulate_keystrokes("cmd-alt-shift-down");
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    let theme = Theme::default();
    let pad = theme.spacing.xs;

    let card = |cx: &mut VisualTestContext, ix: usize| {
        let at = cx.debug_bounds(Box::leak(format!("overview-block-{ix}").into_boxed_str()));
        let at = at.unwrap_or_else(|| panic!("block {ix} is drawn"));
        let quads = quads_at(cx, at);
        let quad = quads
            .into_iter()
            .find(|q| q.background.as_solid() == Some(crate::colors::hsla(theme.surfaces.elevated)))
            .unwrap_or_else(|| panic!("block {ix} is on the elevated surface"));
        (at, quad)
    };
    let (first, first_quad) = card(cx, 0);
    let (second, second_quad) = card(cx, 1);
    for quad in [&first_quad, &second_quad] {
        assert!(!square(quad), "rounded: {quad:?}");
    }
    let scale = cx.update(|window, _| window.scale_factor());
    let ringed = |cx: &mut VisualTestContext, at: Bounds<Pixels>| {
        quads_at(cx, at).iter().any(|q| {
            q.border_color == accent()
                && 2.0_f32.mul_add(-scale, q.border_widths.top.0).abs() < 0.01
        })
    };
    let active = view.read_with(cx, |v, _| v.layout.active_workspace());
    assert_eq!(active, 1, "the tile moved down and the focus with it");
    assert!(ringed(cx, second), "a 2 pt accent ring round the active one");
    assert!(!ringed(cx, first), "and only that one");

    let pane = shells
        .iter()
        .filter_map(|(_, tile)| cx.debug_bounds(selector("item", tile.item)))
        .find(|b| first.contains(&b.center()))
        .expect("a pane in the first card");
    near(f32::from(pane.left() - first.left()), pad);

    // One left edge: the panes in the card, the name over it, the place for the next one.
    let new = bounds(cx, "overview-new-workspace");
    let name = bounds(cx, "overview-name-1");
    near(f32::from(name.left()), f32::from(second.left()) + pad);
    near(f32::from(new.left()), f32::from(second.left()));
    assert!(new.top() >= second.bottom() - px(0.5), "under the last card");
    near(f32::from(new.size.height), f32::from(second.size.height));
    let dashed = quads_at(cx, new).iter().any(|q| q.border_style == gpui::BorderStyle::Dashed);
    assert!(dashed, "only its outline");
    for (_, tile) in &shells {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("a header");
        let quads = quads_at(cx, header);
        let content = crate::colors::hsla(theme.content());
        assert!(!quads.is_empty(), "the header paints");
        assert!(
            quads.iter().all(|q| q.background.as_solid() == Some(content)),
            "the body's surface: {quads:?}"
        );
        assert!(quads.iter().all(|q| q.border_widths.bottom.0 == 0.0), "no hairline");
    }
    cx.simulate_click(new.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.layout.overview_open()), "the overview closes");
    let (active, count) =
        view.read_with(cx, |v, _| (v.layout.active_workspace(), v.layout.workspaces().len()));
    assert_eq!(Some(active), count.checked_sub(1), "on the empty workspace");
}

/// Below half size a tile in the overview is its header and body surfaces only: no title, and
/// its body's own surface over the grid, which stays under it with the keyboard. At rest the
/// text is back.
#[gpui::test]
fn a_small_overview_draws_tiles_as_shapes(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let first = shells[0].1;
    let name = selector("name", first.item);
    assert!(cx.debug_bounds(name).is_some(), "at rest the title is drawn");
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    let zoom = view.read_with(cx, |v, _| v.drawn_zoom);
    assert!(zoom < tile::SHAPES_BELOW, "three columns zoom under half: {zoom}");
    assert!(cx.debug_bounds(selector("item", first.item)).is_some(), "the tile is there");
    assert!(cx.debug_bounds(name).is_none(), "its title is not");
    let grid = cx.debug_bounds("terminal").expect("the grids stay, keeping the keyboard");
    let covers: Vec<Bounds<Pixels>> = shells
        .iter()
        .map(|(_, t)| cx.debug_bounds(selector("shapes", t.item)).expect("each under its surface"))
        .collect();
    assert!(covers.iter().any(|c| c.contains(&grid.center())), "{covers:?} over {grid:?}");
    // Each shape still says what it is, at a size that reads, inside its own surface.
    for ((_, t), cover) in shells.iter().zip(&covers) {
        let label = cx.debug_bounds(selector("shapes-label", t.item)).expect("each shape named");
        assert!(cover.contains(&label.center()), "{label:?} inside {cover:?}");
        assert!(label.size.height >= px(12.0), "the label is chrome-sized: {label:?}");
    }
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();
    assert!(cx.debug_bounds(selector("shapes", first.item)).is_none(), "at rest, the text");
}

/// A worker whose link is up is only its name in the empty workspace's list: no word and no
/// mark. A lost one gets the warn mark and a short word.
#[gpui::test]
fn a_healthy_worker_shows_no_word_in_the_empty_workspace(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    cx.update(|window, _cx| window.set_a11y_active(true));
    let row = |view: &Entity<WorkspaceView>, cx: &mut VisualTestContext| {
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let at = bounds(cx, "empty-worker-0");
        let (x, y, w, h) = (
            f32::from(at.origin.x),
            f32::from(at.origin.y),
            f32::from(at.size.width),
            f32::from(at.size.height),
        );
        let inside = |b: [f32; 4]| {
            b[0] >= x - 0.5
                && b[1] >= y - 0.5
                && b[0] + b[2] <= x + w + 0.5
                && b[1] + b[3] <= y + h + 0.5
        };
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let nodes: Vec<_> = tree.into_iter().filter(|n| inside(n.bounds)).collect();
        let label = nodes
            .iter()
            .find(|n| n.role == "Button")
            .and_then(|n| n.label.clone())
            .expect("the row is a labelled button");
        let marks: Vec<String> =
            nodes.iter().filter(|n| n.role == "Image").filter_map(|n| n.label.clone()).collect();
        (label, marks)
    };
    let (label, marks) = row(&view, cx);
    assert_eq!(label, "New terminal on studio");
    assert!(marks.is_empty(), "no mark for a link that is up: {marks:?}");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    let (label, marks) = row(&view, cx);
    assert_eq!(label, "New terminal on studio, reconnecting");
    assert_eq!(marks, ["Away"]);
}
