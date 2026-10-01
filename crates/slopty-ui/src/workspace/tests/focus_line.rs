//! Which tile has the keyboard, said by a line along the top of its header while there is more
//! than one tile to tell it from.

use super::*;

/// Whether the line runs along the top of `tile`'s header; `None` when there is no line.
fn line_in_header(cx: &mut VisualTestContext, tile: TileRef) -> Option<bool> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let line = cx.debug_bounds("focus-line")?;
    let header = cx.debug_bounds(selector("title", tile.item))?;
    Some(
        line.top() == header.top()
            && line.left() >= header.left()
            && line.right() <= header.right(),
    )
}

/// A lone tile carries no line; with a second in view, the focused one's header does, along
/// its top edge, and it follows the focus.
#[gpui::test]
fn the_focused_header_carries_a_line_once_two_tiles_are_in_view(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert_eq!(line_in_header(cx, first), None, "one tile needs no saying");

    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    assert_eq!(line_in_header(cx, second), Some(true), "along the focused header's top");

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert_eq!(line_in_header(cx, first), Some(true), "it goes with the focus");
}
