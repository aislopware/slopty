//! A tile's panel: the content's surface standing on the canvas, rounded, ringed and, in
//! light, resting on a faint contact shadow (in dark its top edge catches the light), with the
//! canvas showing round it as the gutter. The strip's tiles, a tile closing and the empty
//! workspace's page all stand on one, and nothing else draws a tile's ground
//! (`a_tile_stands_on_a_panel` in the kit's lint-as-tests).
//!
//! GPUI clips a box's children to a rectangle, so a child that paints up to the panel's edge
//! (a remote picture, a page, a block's own background) would poke its square corners out of
//! the round ones. The panel covers its corners last, with the ground it stands on, and draws
//! its ring over them.
//!
//! Everything round the edge is painted only where it shows. A shadow's quad covers its whole
//! element and the blur round it, so the contact shadow is drawn in bands along the edges; a
//! border round an empty middle GPUI already draws as its edge strips. Drawn whole, the two
//! shadows, the corners' cover and the ring cost the GPU about four times a strip of flush
//! tiles; drawn so, a quarter more (`docs/MEASUREMENTS.md`, "Panels on the canvas: what their
//! edges cost the GPU").

use gpui::{
    BorderStyle, Bounds, ContentMask, Corners, Hsla, IntoElement, ParentElement, Pixels, Styled,
    Window, canvas, point, px, size,
};
use slopty_theme::{Theme, stroke};

use crate::colors::hsla;

/// How a panel stands on its ground.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stand {
    /// What shows round it, which covers its corners: the canvas, or the canvas laid on glass.
    pub ground: Hsla,
    /// Its corners' radius, at the zoom it is drawn at.
    pub radius: f32,
    /// Full-bleed, as a phone's screen is: square, with no ring and no shadow.
    pub flat: bool,
}

impl Stand {
    /// A panel on the canvas, `radii.md` at the zoom `k`.
    #[must_use]
    pub fn on(theme: &Theme, ground: Hsla, k: f32) -> Self {
        Self { ground, radius: theme.radii.md * k, flat: false }
    }

    /// A full-bleed panel, as a phone shows one tile at a time across its screen.
    #[must_use]
    pub const fn flat(ground: Hsla) -> Self {
        Self { ground, radius: 0.0, flat: true }
    }
}

/// `el` standing as a panel, `inside` its content: the panel's ground under it and its edge
/// over it.
///
/// `el` is placed and sized by the caller and does not clip, so the contact shadow falls
/// outside it; `inside` fills it and clips to it (the panel adds both), and its children are
/// the panel's content.
#[must_use]
pub fn panel<E: ParentElement, C: ParentElement + Styled + IntoElement>(
    el: E,
    theme: &Theme,
    stand: Stand,
    inside: C,
) -> E {
    el.child(ground(theme, stand))
        .child(inside.relative().size_full().overflow_hidden().child(edge(theme, stand)))
}

/// The panel's ground, the first thing under its content: the contact shadow round it in
/// light, its surface, and its lit top edge in dark.
fn ground(theme: &Theme, stand: Stand) -> impl IntoElement {
    let theme = theme.clone();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| paint_ground(&theme, stand, bounds, window),
    )
    .absolute()
    .inset_0()
}

/// The panel's edge, over its content and inside its clip: the corners covered with the
/// ground, then the ring.
fn edge(theme: &Theme, stand: Stand) -> impl IntoElement {
    let ring = hsla(theme.surfaces.border_subtle);
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| paint_edge(stand, ring, bounds, window),
    )
    .absolute()
    .inset_0()
}

/// The bands along `bounds`'s edges, `inward` deep inside it and `outward` out past it: the
/// top and bottom ones full width, the sides between them. Where a panel's edge is drawn, and
/// all a shadow under an opaque panel shows of itself.
fn bands(bounds: Bounds<Pixels>, inward: Pixels, outward: Pixels) -> [Bounds<Pixels>; 4] {
    let (x, y) = (bounds.origin.x, bounds.origin.y);
    let (w, h) = (bounds.size.width, bounds.size.height);
    let across = w + outward * 2.0;
    let deep = inward + outward;
    let between = (h - inward * 2.0).max(px(0.0));
    [
        Bounds::new(point(x - outward, y - outward), size(across, deep)),
        Bounds::new(point(x - outward, y + h - inward), size(across, deep)),
        Bounds::new(point(x - outward, y + inward), size(deep, between)),
        Bounds::new(point(x + w - inward, y + inward), size(deep, between)),
    ]
}

/// Draws `paint` once in each of `masks`.
fn in_each(masks: &[Bounds<Pixels>], window: &mut Window, mut paint: impl FnMut(&mut Window)) {
    for bounds in masks {
        window.with_content_mask(Some(ContentMask { bounds: *bounds }), |window| paint(window));
    }
}

fn paint_ground(theme: &Theme, stand: Stand, bounds: Bounds<Pixels>, window: &mut Window) {
    let radius = px(stand.radius);
    let corners = Corners::all(radius);
    let fill = hsla(theme.content());
    if stand.flat {
        window.paint_quad(gpui::fill(bounds, fill));
        return;
    }
    let contact = super::rest_layers(theme, stroke::HAIR, true, true);
    let (drops, lit): (Vec<_>, Vec<_>) = contact.into_iter().partition(|layer| !layer.inset);
    // How far out a layer's shade reaches: its offset and spread, and three of its blurs.
    let reach = drops
        .iter()
        .map(|l| l.offset.y.abs().max(l.offset.x.abs()) + l.spread_radius + l.blur_radius * 3.0)
        .fold(px(0.0), Pixels::max);
    if !drops.is_empty() {
        in_each(&bands(bounds, radius, reach), window, |window| {
            window.paint_drop_shadows(bounds, corners, &drops);
        });
    }
    window.paint_quad(gpui::fill(bounds, fill).corner_radii(corners));
    if !lit.is_empty() {
        // The rim lies along one edge and round its two corners: the band along that edge,
        // as deep as the rim reaches where the corners are tighter than it.
        let depth = px(stroke::HAIR + super::RIM_DEPTH).max(radius);
        let [top, bottom, ..] = bands(bounds, depth, px(0.0));
        let band = if theme.elevation.rim.top { top } else { bottom };
        in_each(&[band], window, |window| window.paint_inset_shadows(bounds, corners, &lit));
    }
}

fn paint_edge(stand: Stand, ring: Hsla, bounds: Bounds<Pixels>, window: &mut Window) {
    if stand.flat {
        return;
    }
    let radius = px(stand.radius);
    // A border of the ground whose inner edge is the panel's: out past the bounds by the
    // radius, it covers each corner outside the arc, and only that inside the panel's clip.
    // GPUI draws a border round an empty middle as the strips along its edges alone, so
    // neither this nor the ring shades the panel's middle.
    window.paint_quad(gpui::quad(
        bounds.dilate(radius),
        Corners::all(radius * 2.0),
        gpui::transparent_black(),
        radius,
        stand.ground,
        BorderStyle::Solid,
    ));
    window.paint_quad(gpui::quad(
        bounds,
        Corners::all(radius),
        gpui::transparent_black(),
        px(stroke::HAIR),
        ring,
        BorderStyle::Solid,
    ));
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, Window, div, px, size,
    };
    use slopty_theme::{Theme, Variant};

    use super::{Stand, bands, panel};
    use crate::colors::hsla;

    /// The bands round a panel hold everything its shadow shows outside it and none of its
    /// middle, which the panel covers: drawn there, a shadow is a quad as large as the panel.
    #[test]
    fn the_bands_hold_the_edge_and_leave_the_middle() {
        let panel = gpui::Bounds::new(gpui::point(px(40.0), px(30.0)), size(px(300.0), px(500.0)));
        let (inward, outward) = (px(8.0), px(7.0));
        let bands = bands(panel, inward, outward);
        let middle = gpui::Bounds::new(
            panel.origin + gpui::point(inward, inward),
            panel.size - size(inward * 2.0, inward * 2.0),
        );
        for band in &bands {
            assert!(!band.intersects(&middle), "{band:?} reaches the middle {middle:?}");
        }
        // Every point the shadow can reach outside the panel, and the panel's own rim.
        let reach = panel.dilate(outward);
        let mut points = Vec::new();
        for step in 0..=40_u8 {
            let t = f32::from(step) / 40.0;
            let x = reach.origin.x + px(0.5) + (reach.size.width - px(1.0)) * t;
            let y = reach.origin.y + px(0.5) + (reach.size.height - px(1.0)) * t;
            let (left, top) = (reach.origin.x + px(0.5), reach.origin.y + px(0.5));
            let (right, bottom) = (reach.right() - px(0.5), reach.bottom() - px(0.5));
            points.extend([gpui::point(x, top), gpui::point(x, bottom)]);
            points.extend([gpui::point(left, y), gpui::point(right, y)]);
            points
                .push(gpui::point(panel.origin.x + px(3.0), y.clamp(panel.top(), panel.bottom())));
        }
        for at in points {
            assert!(bands.iter().any(|b| b.contains(&at)), "{at:?} is in no band");
        }
        let area =
            |b: &gpui::Bounds<gpui::Pixels>| f32::from(b.size.width) * f32::from(b.size.height);
        let drawn: f32 = bands.iter().map(area).sum();
        assert!(drawn < area(&panel) / 5.0, "the bands are an edge, not the panel: {drawn}");
    }

    struct Shown {
        theme: Theme,
        flat: bool,
    }

    impl Render for Shown {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = &self.theme;
            let ground = hsla(theme.surfaces.canvas);
            let stand = if self.flat { Stand::flat(ground) } else { Stand::on(theme, ground, 1.0) };
            // A body that paints its own surface to its edges, as a remote picture does.
            let body = div().size_full().bg(gpui::red());
            let at = div()
                .debug_selector(|| "panel".to_owned())
                .absolute()
                .left(px(20.0))
                .top(px(30.0))
                .w(px(200.0))
                .h(px(300.0));
            div().size_full().bg(ground).child(panel(at, theme, stand, div().child(body)))
        }
    }

    fn shown(cx: &mut TestAppContext, variant: Variant, flat: bool) -> gpui::VisualTestContext {
        let theme = Theme::new(variant);
        let (_view, cx) = cx.add_window_view(|_, _| Shown { theme, flat });
        cx.simulate_resize(size(px(400.0), px(400.0)));
        cx.run_until_parked();
        cx.clone()
    }

    /// A panel is the content's surface rounded at `radii.md` on the canvas: a body that paints
    /// to its edges has its square corners covered by the canvas after it, and the ring is drawn
    /// over both, in the quiet border, round the panel's own edge; neither shades its middle. In
    /// light two faint contact layers fall round it, each drawn in the four bands along its
    /// edges; in dark its top edge catches the light instead, and nothing falls under it.
    #[gpui::test]
    fn a_panel_stands_on_the_canvas_with_its_corners_covered(cx: &mut TestAppContext) {
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            let mut cx = shown(cx, variant, false);
            let at = cx.debug_bounds("panel").expect("drawn");
            let (scale, quads, lines) =
                cx.update(|w, _| (w.scale_factor(), w.painted_quads(), w.painted_primitives()));
            let at = at.scale(scale);
            let radius = theme.radii.md * scale;
            let same = |a: gpui::Bounds<gpui::ScaledPixels>| {
                (a.origin.x.0 - at.origin.x.0).abs() < 0.5
                    && (a.origin.y.0 - at.origin.y.0).abs() < 0.5
                    && (a.size.width.0 - at.size.width.0).abs() < 0.5
                    && (a.size.height.0 - at.size.height.0).abs() < 0.5
            };
            let ground = quads
                .iter()
                .find(|q| same(q.bounds) && q.background.as_solid() == Some(hsla(theme.content())))
                .expect("the content's surface");
            assert!((ground.corner_radii.top_left.0 - radius).abs() < 0.01, "{ground:?}");
            let body = quads
                .iter()
                .find(|q| q.background.as_solid() == Some(gpui::red()))
                .expect("the body");
            let covers: Vec<_> = quads
                .iter()
                .filter(|q| {
                    q.border_color == hsla(theme.surfaces.canvas) && q.border_widths.top.0 > 0.0
                })
                .collect();
            // A device point in from each corner, where the arc leaves the square uncovered.
            let (left, top) = (at.left().0 + scale, at.top().0 + scale);
            let (right, bottom) = (at.right().0 - scale, at.bottom().0 - scale);
            let corners = [(left, top), (right, top), (left, bottom), (right, bottom)]
                .map(|(x, y)| gpui::point(gpui::ScaledPixels(x), gpui::ScaledPixels(y)));
            for corner in corners {
                let covered = covers.iter().any(|c| c.content_mask.bounds.contains(&corner));
                assert!(covered, "{variant:?}: the corner at {corner:?} is covered");
            }
            assert!(covers.iter().all(|c| c.order > body.order), "over the body");
            let middle = at.center();
            let rings: Vec<_> = quads
                .iter()
                .filter(|q| same(q.bounds) && q.border_color == hsla(theme.surfaces.border_subtle))
                .collect();
            assert!(!rings.is_empty(), "{variant:?}: the ring");
            for ring in &rings {
                assert!((ring.corner_radii.top_left.0 - radius).abs() < 0.01, "round its edge");
                assert!(ring.order > covers[0].order, "over the covers");
            }
            let edge = covers.iter().chain(&rings);
            assert!(edge.clone().all(|q| !q.content_mask.bounds.contains(&middle)), "no middle");
            let shadows = lines.iter().filter(|l| l.contains(" Shadow {")).count();
            let inset = lines.iter().filter(|l| l.contains(" Shadow {") && l.contains("inset: 1"));
            match variant {
                Variant::Light => {
                    assert_eq!(shadows, 2 * 4, "two contact layers, each in four bands");
                    assert_eq!(inset.count(), 0, "no lit edge on paper");
                }
                Variant::Dark => {
                    assert_eq!(shadows, 1, "the lit top edge alone");
                    assert_eq!(inset.count(), 1);
                }
            }
        }
    }

    /// A full-bleed panel, as a phone shows a tile, is its surface alone: square, with no ring,
    /// no cover and no shadow.
    #[gpui::test]
    fn a_flat_panel_is_its_surface_alone(cx: &mut TestAppContext) {
        let theme = Theme::new(Variant::Light);
        let mut cx = shown(cx, Variant::Light, true);
        let (quads, lines) = cx.update(|w, _| (w.painted_quads(), w.painted_primitives()));
        let ground = quads
            .iter()
            .find(|q| q.background.as_solid() == Some(hsla(theme.content())))
            .expect("the content's surface");
        assert_eq!(ground.corner_radii.top_left.0, 0.0, "square");
        assert!(quads.iter().all(|q| q.border_widths.top.0 == 0.0), "no ring and no cover");
        assert!(!lines.iter().any(|l| l.contains(" Shadow {")), "no shadow");
    }
}
