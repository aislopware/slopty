//! Which tile has the keyboard: said by its title's tone, every header on its own content, and
//! under Increase Contrast by a line along the top of the focused header as well.

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

/// With two tiles in view neither header is a band: both sit on the content, with no line
/// drawn for focus. Focus is the title's tone, primary on the focused one and muted on the
/// other, and it goes with the focus.
#[gpui::test]
fn the_headers_sit_on_their_content_and_focus_is_the_titles_tone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert_eq!(focused(&view, cx), Some(second));
    assert_eq!(line_in_header(cx, second), None, "focus draws no line");

    let theme = Theme::default();
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
}

/// Under Increase Contrast a title's tone is too quiet a sign on its own: a lone tile still
/// carries no line, but with a second in view the focused one's header does, along its top
/// edge in the text's tone, and it follows the focus.
#[gpui::test]
fn under_increase_contrast_the_focused_header_carries_a_line(cx: &mut TestAppContext) {
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
