//! Which tile has the keyboard: said by its title's tone and, while two or more tiles show, a
//! line along the top of the focused header, every header on its own content.

use gpui::{Bounds, Pixels};
use slopty_theme::Contrast;

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

/// The fills the last frame painted exactly over `at`.
fn fills_at(cx: &mut VisualTestContext, at: Bounds<Pixels>) -> Vec<gpui::Hsla> {
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
        })
        .filter_map(|q| q.background.as_solid())
        .collect()
}

/// With two tiles in view neither header is a band: both sit on the content. The focused one's
/// title leads in the primary tone and its header carries a line along its top in the text's
/// tone, set back a step; the other's title is muted and carries none. A lone tile needs no
/// saying. The line goes with the focus.
#[gpui::test]
fn the_focused_header_carries_a_text_line_while_two_tiles_show(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert_eq!(line_in_header(cx, first), None, "one tile needs no saying");
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    assert_eq!(line_in_header(cx, second), Some(true), "along the focused header's top");

    let theme = Theme::default();
    let line = cx.debug_bounds("focus-line").expect("drawn");
    let ink = crate::colors::hsla(theme.surfaces.focus);
    assert!(fills_at(cx, line).contains(&ink), "in the text's tone, set back a step");
    let header = cx.debug_bounds(selector("title", second.item)).expect("drawn");
    let inset = px(theme.radii.sm);
    assert!(line.left() >= header.left() + inset, "inset at its start: {line:?} {header:?}");
    assert!((line.size.height - px(slopty_theme::stroke::MARK)).abs() < px(0.01), "1.5 pt");
    let content = theme.content();
    let seen = theme.surfaces.focus.over(content);
    assert!(seen.contrast(content) >= 3.0, "seen over the content: {}", seen.contrast(content));
    let content = crate::colors::hsla(theme.content());
    let panel = crate::colors::hsla(theme.surfaces.panel);
    for tile in [first, second] {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
        let fills = fills_at(cx, header);
        assert!(fills.contains(&content), "{tile:?} sits on the content: {fills:?}");
        assert!(!fills.contains(&panel), "{tile:?} is no band");
    }
    assert_eq!(tile::title_ink(&theme, true), theme.surfaces.text);
    assert_eq!(tile::title_ink(&theme, false), theme.surfaces.text_muted);

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert_eq!(line_in_header(cx, first), Some(true), "it goes with the focus");
}

/// Under Increase Contrast the line is the text's whole tone: a lone tile still carries none,
/// and with a second in view the focused one's header does, and it follows the focus.
#[gpui::test]
fn under_increase_contrast_the_focus_line_is_whole(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update(cx, |v, cx| {
        let mut theme = Theme { contrast: Contrast::Increased, ..Theme::default() };
        theme.derive_chrome();
        v.set_theme(theme, cx);
    });
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert_eq!(line_in_header(cx, first), None, "one tile needs no saying");

    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    assert_eq!(line_in_header(cx, second), Some(true), "along the focused header's top");
    let line = cx.debug_bounds("focus-line").expect("drawn");
    let text = view.read_with(cx, |v, _| crate::colors::hsla(v.theme().surfaces.text));
    assert!(fills_at(cx, line).contains(&text), "in the text's tone");

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert_eq!(line_in_header(cx, first), Some(true), "it goes with the focus");
}
