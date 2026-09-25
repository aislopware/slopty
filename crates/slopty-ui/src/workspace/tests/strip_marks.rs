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

/// Whether the last frame filled `at` with `ink`.
fn filled(cx: &mut VisualTestContext, at: Bounds<Pixels>, ink: gpui::Hsla) -> bool {
    quads_at(cx, at).iter().any(|q| q.background.as_solid() == Some(ink))
}

/// The handle between two columns straddles their divider: a 6 pt target centred on the line,
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
    near(f32::from(handle.size.width), 6.0);
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
/// new column would open; over the middle of a column it washes that column faintly, with no
/// corner and no frame.
#[gpui::test]
fn the_drop_line_sits_on_the_divider_and_a_join_washes_the_column(cx: &mut TestAppContext) {
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
    assert!(filled(cx, line, accent()), "the line is the accent");

    cx.simulate_mouse_move(column.center(), Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    let wash = bounds(cx, "drop-hint");
    assert_eq!(wash, column, "the wash covers the column it joins");
    let faint = accent().opacity(slopty_theme::alpha::FAINT);
    let quads = quads_at(cx, wash);
    let quad = quads.iter().find(|q| q.background.as_solid() == Some(faint)).expect("a wash");
    assert!(square(quad), "no corner: {quad:?}");
    let b = quad.border_widths;
    assert!([b.top, b.right, b.bottom, b.left].iter().all(|w| w.0 == 0.0), "no frame: {quad:?}");
    cx.simulate_mouse_up(column.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

/// In the overview a workspace is one block of panes in a single square hairline frame, the
/// place for a new one a square dashed outline, and nothing the size of a pane has a corner.
#[gpui::test]
fn the_overview_draws_no_rounded_quads(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    cx.simulate_keystrokes("cmd-alt-o");
    cx.run_until_parked();

    let block = bounds(cx, "overview-block-0");
    let frames = quads_at(cx, block);
    let frame = frames.iter().find(|q| q.border_widths.top.0 > 0.0).expect("a frame");
    assert!(square(frame), "{frame:?}");
    assert_eq!(frame.border_style, gpui::BorderStyle::Solid);
    let zone = bounds(cx, "overview-block-1");
    let zone = quads_at(cx, zone);
    let zone = zone.iter().find(|q| q.border_widths.top.0 > 0.0).expect("a dashed outline");
    assert!(square(zone), "{zone:?}");
    assert_eq!(zone.border_style, gpui::BorderStyle::Dashed);

    // Anything as large as the smallest pane drawn is square.
    let pane = shells
        .iter()
        .filter_map(|(_, tile)| cx.debug_bounds(selector("item", tile.item)))
        .map(|b| f32::from(b.size.width).min(f32::from(b.size.height)))
        .fold(f32::MAX, f32::min);
    assert!(pane < f32::MAX, "the panes are drawn");
    let scale = cx.update(|window, _| window.scale_factor());
    let quads = cx.update(|window, _| window.painted_quads());
    let rounded: Vec<_> = quads
        .iter()
        .filter(|q| q.bounds.size.width.0.min(q.bounds.size.height.0) >= pane * scale - 0.5)
        .filter(|q| !square(q))
        .collect();
    assert!(rounded.is_empty(), "rounded blocks in the overview: {rounded:#?}");
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
    assert_eq!(label, "studio");
    assert!(marks.is_empty(), "no mark for a link that is up: {marks:?}");

    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    let (label, marks) = row(&view, cx);
    assert_eq!(label, "studio, reconnecting");
    assert_eq!(marks, ["Away"]);
}
