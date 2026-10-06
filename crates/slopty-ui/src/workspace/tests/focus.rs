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
        .filter(|fill| fill.a > 0.0)
        .collect()
}

/// The lines the last frame drew round `at`: the borders of the quads as large as it or
/// larger, a panel's ring and its corners' cover, and no smaller mark inside it (a cursor).
fn edges_at(cx: &mut VisualTestContext, at: Bounds<Pixels>) -> Vec<gpui::Hsla> {
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let at = at.scale(scale);
    let round = |q: &gpui::Quad| {
        let b = q.bounds;
        b.origin.x.0 <= at.origin.x.0 + 0.5
            && b.origin.y.0 <= at.origin.y.0 + 0.5
            && b.right().0 >= at.right().0 - 0.5
            && b.bottom().0 >= at.bottom().0 - 0.5
    };
    let mut edges: Vec<gpui::Hsla> = quads
        .into_iter()
        .filter(|q| q.border_widths.top.0 > 0.0 && round(q))
        .filter(|q| at.contains(&q.content_mask.bounds.center()))
        .map(|q| q.border_color)
        .collect();
    edges.sort_by(|a, b| a.l.total_cmp(&b.l));
    edges
}

/// Whether `tile`'s title paints in the primary tone.
fn title_leads(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, tile: TileRef) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let focused = focused(view, cx) == Some(tile);
    let theme = Theme::default();
    focused && tile::title_ink(&theme, focused) == theme.surfaces.text
}

/// With two tiles in view each header is its pane's tab row on the chrome step, its one tab on
/// the pane's ground. The focused one's title leads in the primary tone at the medium weight
/// and the other's steps back a tier to the secondary tone at the regular weight, and the
/// focused pane's tab alone wears the focus green along its top, all going with the focus. No
/// ring or frame says it: neither pane draws an edge round itself.
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
    let (chrome, content) =
        (crate::colors::hsla(theme.surfaces.chrome), crate::colors::hsla(theme.content()));
    // The green along the tab's top, inside its sash lines.
    let marked = |cx: &mut VisualTestContext, tile: TileRef| {
        let tab = cx.debug_bounds(selector("lone-tab", tile.item)).expect("its tab");
        let green = crate::colors::hsla(theme.surfaces.focus);
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        quads.iter().any(|q| {
            let b = q.bounds;
            q.background.as_solid() == Some(green)
                && (b.origin.y.0 / scale - f32::from(tab.top())).abs() < 0.5
                && (b.size.height.0 / scale - slopty_theme::stroke::MARK).abs() < 0.6
                && b.origin.x.0 / scale >= f32::from(tab.left()) - 0.5
                && b.right().0 / scale <= f32::from(tab.right()) + 0.5
                && b.size.width.0 / scale >= f32::from(tab.size.width) - 2.0
        })
    };
    for tile in [first, second] {
        let header = cx.debug_bounds(selector("title", tile.item)).expect("drawn");
        let fills = fills_at(cx, header);
        assert!(fills.contains(&chrome), "{tile:?} is on the chrome: {fills:?}");
        let tab = cx.debug_bounds(selector("lone-tab", tile.item)).expect("its tab");
        let fills = fills_at(cx, tab);
        assert!(fills.contains(&content), "{tile:?}'s tab opens into its pane: {fills:?}");
    }
    assert!(marked(cx, second), "the focused pane's tab wears the green");
    assert!(!marked(cx, first), "the other's does not");
    let edges = |cx: &mut VisualTestContext, tile: TileRef| {
        let bounds = cx.debug_bounds(selector("item", tile.item)).expect("drawn");
        edges_at(cx, bounds)
    };
    let (focused_edges, other_edges) = (edges(cx, second), edges(cx, first));
    assert!(focused_edges.is_empty(), "no ring round the focused pane: {focused_edges:?}");
    assert!(other_edges.is_empty(), "nor round the other: {other_edges:?}");
    assert_eq!(tile::title_ink(&theme, true), theme.surfaces.text);
    assert_eq!(tile::title_ink(&theme, false), theme.surfaces.text_secondary);

    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    assert!(title_leads(&view, cx, first), "the tone goes with the focus");
    assert!(!title_leads(&view, cx, second), "and leaves the other");
    assert!(marked(cx, first) && !marked(cx, second), "and the green with it");
}
