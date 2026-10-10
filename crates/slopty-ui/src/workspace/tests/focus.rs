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

/// With two tiles in view each header lies on the ground with the one line along its foot, the
/// pane's title with no pill. The focused one's title leads in the primary tone and the
/// other's steps back to the secondary tone, never a weight apart, and a dot leads each: the
/// accent's on the focused pane, empty on the other, all going with the focus (`MonoCode`'s
/// split header). No ring or frame says it: neither pane draws an edge round itself, and no
/// tab wears a coloured edge.
#[gpui::test]
fn the_focused_tile_is_said_by_its_titles_tone_and_its_dot(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    assert!(title_leads(&view, cx, second), "the focused title leads");
    assert!(!title_leads(&view, cx, first), "the other is muted");

    let theme = Theme::default();
    let accent = crate::colors::hsla(theme.surfaces.accent_fill);
    // The accent's dot inside the pane's title, the dot's side square.
    let dotted = |cx: &mut VisualTestContext, tile: TileRef| {
        let title = cx.debug_bounds(selector("lone-title", tile.item)).expect("its title");
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        quads.iter().any(|q| {
            let b = q.bounds;
            q.background.as_solid() == Some(accent)
                && (b.size.width.0 / scale - tab_look::FOCUS_DOT).abs() < 0.6
                && title.contains(&point(px(b.center().x.0 / scale), px(b.center().y.0 / scale)))
        })
    };
    let pill = crate::colors::hsla(theme.surfaces.selected);
    for tile in [first, second] {
        let title = cx.debug_bounds(selector("lone-title", tile.item)).expect("its title");
        let fills = fills_at(cx, title);
        assert!(!fills.contains(&pill), "{tile:?}: no pill: {fills:?}");
    }
    assert!(dotted(cx, second), "the focused pane's dot");
    assert!(!dotted(cx, first), "the other's is empty");
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
    assert!(dotted(cx, first) && !dotted(cx, second), "and the dot with it");
}

/// In a split, a pane of tabs leads with the focus dot as a lone pane does: centred on its row,
/// and its first tab's mark on the edge the lone pane's mark stands on, so the two headers read
/// as one grid.
#[gpui::test]
fn a_pane_of_tabs_leads_with_its_dot_on_the_lone_panes_edge(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let [(_, lone), (_, second), (_, third)] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.focus_tile(third, cx));
    cx.run_until_parked();
    let at = |cx: &mut VisualTestContext, what: &str, tile: TileRef| {
        cx.debug_bounds(selector(what, tile.item)).unwrap_or_else(|| panic!("{what} is drawn"))
    };
    let lone_mark = at(cx, "kind", lone).left() - at(cx, "title", lone).left();
    let header = at(cx, "title", third);
    let first_tab = at(cx, "tab-slot", second).left().min(at(cx, "tab-slot", third).left());
    let tabbed_mark = first_tab - header.left();
    assert!(
        f32::from(tabbed_mark - lone_mark).abs() < 0.5,
        "one edge: {tabbed_mark:?} against {lone_mark:?}"
    );
    let accent = crate::colors::hsla(Theme::default().surfaces.accent_fill);
    let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
    let dot = quads
        .iter()
        .filter(|q| q.background.as_solid() == Some(accent))
        .filter(|q| (q.bounds.size.width.0 / scale - tab_look::FOCUS_DOT).abs() < 0.6)
        .map(|q| point(px(q.bounds.center().x.0 / scale), px(q.bounds.center().y.0 / scale)))
        .find(|c| header.contains(c))
        .expect("the focused pane's dot in its row");
    let off = f32::from(dot.y - header.center().y).abs();
    assert!(off < 0.5, "centred on the row: {off} pt off");
}

/// A tab of several tiles reads as two levels, never its focused tile's pill twice: its title
/// tab says what else it holds on a second line (the other's title, then how many once there
/// are more), and the pane's tab on show takes the hover's wash where the title tab wears
/// `selected`. A tab of one tile has no second line.
#[gpui::test]
fn a_tab_of_several_tiles_reads_as_two_levels(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    let studio = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let shown_tab = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| v.layout().shown_tab().map(|t| t.id().get())).expect("a tab")
    };
    let meta = |cx: &mut VisualTestContext| {
        let n = shown_tab(cx);
        cx.debug_bounds(Box::leak(format!("title-tab-meta-{n}").into_boxed_str()))
    };
    assert!(meta(cx).is_none(), "one tile: one line");
    let second = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    one_pane(&view, cx, &[first, second]);
    view.update_in(cx, |v, _w, cx| v.focus_tile(second, cx));
    cx.run_until_parked();
    let n = shown_tab(cx);
    let title = cx.debug_bounds(Box::leak(format!("title-tab-text-{n}").into_boxed_str()));
    let (title, under) = (title.expect("its title"), meta(cx).expect("what else it holds"));
    assert!(under.top() >= title.bottom() - px(0.5), "under the title: {title:?} {under:?}");
    let bar = cx.debug_bounds("titlebar").expect("the bar").bottom();
    let said: Vec<String> = tree(cx)
        .into_iter()
        .filter(|n| n.role == "Tab" && n.bounds[1] < f32::from(bar))
        .filter_map(|n| n.label)
        .collect();
    assert!(said.iter().any(|l| l.contains(", ")), "the other tile is said: {said:?}");

    let theme = Theme::default();
    let pane_tab = cx.debug_bounds(selector("tab", second.item)).expect("the pane's tab on show");
    let fills = fills_at(cx, pane_tab);
    assert!(fills.contains(&crate::colors::hsla(theme.surfaces.hover)), "the lighter wash");
    assert!(!fills.contains(&crate::colors::hsla(theme.surfaces.selected)), "{fills:?}");

    let third = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    one_pane(&view, cx, &[first, third]);
    cx.run_until_parked();
    let said: Vec<String> = tree(cx)
        .into_iter()
        .filter(|n| n.role == "Tab" && n.bounds[1] < f32::from(bar))
        .filter_map(|n| n.label)
        .collect();
    assert!(said.iter().any(|l| l.ends_with(", 3 tiles")), "then how many: {said:?}");
}
