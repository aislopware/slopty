//! Which tile has the keyboard: said by its title's tone alone, every header on its own content.

use gpui::{Bounds, Pixels};

use super::*;

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

/// Whether `tile`'s title paints in the primary tone.
fn title_leads(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let focused = focused(view, cx) == Some(tile);
    let theme = Theme::default();
    focused && tile::title_ink(&theme, focused) == theme.surfaces.text
}

/// With two tiles in view neither header is a band and nothing is drawn over either: both sit
/// on the content. The focused one's title leads in the primary tone at the medium weight and
/// the other's steps back a tier to the secondary tone at the regular weight, and both go with
/// the focus. No line, ring or frame says it.
#[gpui::test]
fn the_focused_tile_is_said_by_its_titles_tone_and_weight(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert!(title_leads(&view, cx, second), "the focused title leads");
    assert!(!title_leads(&view, cx, first), "the other is muted");

    let theme = Theme::default();
    let content = crate::colors::hsla(theme.content());
    let panel = crate::colors::hsla(theme.surfaces.panel);
    for tile in [first, second] {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
        let fills = fills_at(cx, header);
        assert!(fills.contains(&content), "{tile:?} sits on the content: {fills:?}");
        assert!(!fills.contains(&panel), "{tile:?} is no band");
        let top = Bounds::new(header.origin, size(header.size.width, px(2.0)));
        let over: Vec<_> = fills_at(cx, top).into_iter().filter(|f| *f != content).collect();
        assert!(over.is_empty(), "{tile:?}: nothing drawn along its top: {over:?}");
    }
    assert_eq!(tile::title_ink(&theme, true), theme.surfaces.text);
    assert_eq!(tile::title_ink(&theme, false), theme.surfaces.text_secondary);

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert!(title_leads(&view, cx, first), "the tone goes with the focus");
    assert!(!title_leads(&view, cx, second), "and leaves the other");
}
