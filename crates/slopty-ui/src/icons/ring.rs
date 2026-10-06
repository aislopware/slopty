//! The circle glyphs drawn by us, where an SF Symbol cannot be crisp at 1x: an empty ring, a
//! dashed ring and a ring with a pie of its share (`.research/status-color-2026-10-06.md` §3.1).
//!
//! One silhouette for every state, as Linear's statuses are, so the eye compares fill and hue
//! and not shapes. The ring's outer edge sits on the device grid and its stroke is a whole
//! number of device pixels, so at 1x it is one crisp pixel wide; SF's `circle.dashed` at that
//! size drew gaps under a device pixel and read as a plain circle.

use std::f32::consts::TAU;

use gpui::{
    AnyElement, Bounds, Hsla, IntoElement as _, PathBuilder, Pixels, Styled as _, Window, canvas,
    point, px,
};

/// Which ring.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ring {
    /// The stroke alone: up next, idle where a mark is required.
    Empty,
    /// Six dashes round the circle: waiting on its own background work.
    Dashed,
    /// The stroke and a sector of this share (0 to 1) of a turn, from twelve o'clock clockwise:
    /// a stage under way, as Linear's in-progress glyph is.
    Pie(f32),
}

/// The circle's diameter as a share of the glyph's side.
const DIAMETER: f32 = 12.0 / 14.0;
/// The stroke's width as a share of the side.
const STROKE: f32 = 1.5 / 14.0;
/// The pie's radius as a share of the side.
const PIE: f32 = 3.5 / 14.0;
/// How many dashes a dashed ring has.
const DASHES: u16 = 6;
/// The share of each dash's sixth of the turn it draws; the rest is its gap.
const DASH: f32 = 0.6;

/// Where a ring's parts lie, in points, from its glyph's side and the device's scale.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Geometry {
    /// The centre, from the glyph's top left corner.
    pub centre: (f32, f32),
    /// The stroke's middle line's radius.
    pub radius: f32,
    /// The stroke's width.
    pub stroke: f32,
    /// The arcs the stroke draws, as turns clockwise from twelve o'clock: one whole turn, or
    /// the dashes.
    pub arcs: Vec<(f32, f32)>,
    /// The pie's radius and the turn it sweeps, from twelve o'clock clockwise.
    pub pie: Option<(f32, f32)>,
}

impl Ring {
    /// Its parts in a glyph `side` points square on a display of `scale` device pixels a point.
    ///
    /// The stroke is a whole number of device pixels, at least one, and the circle's outer
    /// edge spans a whole number of them from the glyph's corner, so the edge lands on pixel
    /// boundaries and the stroke covers whole pixels where the circle meets its box.
    pub(crate) fn geometry(self, side: f32, scale: f32) -> Geometry {
        let scale = scale.max(1.0);
        let device = |points: f32| points * scale;
        let stroke = device(STROKE * side).floor().max(1.0) / scale;
        let outer = device(DIAMETER * side).round().max(2.0) / scale;
        let inset = (device((side - outer) / 2.0)).round() / scale;
        let centre = (inset + outer / 2.0, inset + outer / 2.0);
        let radius = (outer - stroke) / 2.0;
        let arcs = match self {
            Self::Empty | Self::Pie(_) => vec![(0.0, 1.0)],
            Self::Dashed => {
                let each = 1.0 / f32::from(DASHES);
                let gap = each * (1.0 - DASH);
                (0..DASHES)
                    .map(|i| {
                        let from = f32::from(i).mul_add(each, gap / 2.0);
                        (from, each.mul_add(DASH, from))
                    })
                    .collect()
            }
        };
        let pie = match self {
            Self::Pie(share) if share > 0.0 => Some((PIE * side, share.min(1.0))),
            _ => None,
        };
        Geometry { centre, radius, stroke, arcs, pie }
    }

    /// The ring `side` square, in `color`.
    #[must_use]
    pub fn draw(self, side: Pixels, color: Hsla) -> AnyElement {
        canvas(|_, _, _| {}, move |bounds, (), window, _cx| self.paint(bounds, color, window))
            .flex_none()
            .size(side)
            .into_any_element()
    }

    /// Paints the ring in `bounds`.
    fn paint(self, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let g = self.geometry(side, window.scale_factor());
        let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let at = |radius: f32, turn: f32| {
            let (sin, cos) = (turn * TAU).sin_cos();
            point(
                px(radius.mul_add(sin, left + g.centre.0)),
                px((-radius).mul_add(cos, top + g.centre.1)),
            )
        };
        let r = point(px(g.radius), px(g.radius));
        for &(from, to) in &g.arcs {
            let mut path = PathBuilder::stroke(px(g.stroke));
            path.move_to(at(g.radius, from));
            // A whole turn is two halves: an arc from a point to itself draws nothing.
            if to - from >= 1.0 {
                path.arc_to(r, px(0.0), false, true, at(g.radius, from + 0.5));
                path.arc_to(r, px(0.0), false, true, at(g.radius, from));
                path.close();
            } else {
                path.arc_to(r, px(0.0), to - from > 0.5, true, at(g.radius, to));
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        if let Some((radius, sweep)) = g.pie {
            let centre = point(px(left + g.centre.0), px(top + g.centre.1));
            let pr = point(px(radius), px(radius));
            let mut path = PathBuilder::fill();
            if sweep >= 1.0 {
                path.move_to(at(radius, 0.0));
                path.arc_to(pr, px(0.0), false, true, at(radius, 0.5));
                path.arc_to(pr, px(0.0), false, true, at(radius, 0.0));
            } else {
                path.move_to(centre);
                path.line_to(at(radius, 0.0));
                path.arc_to(pr, px(0.0), sweep > 0.5, true, at(radius, sweep));
            }
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inline slot's glyph side, as the chrome draws a status at 1x.
    const SIDE: f32 = 14.0;

    /// At 1x the ring's stroke is one device pixel and its outer edge lies on pixel
    /// boundaries all round, so it is drawn crisp, not smeared over two half-lit pixels; at 2x
    /// it is three device pixels, the 1.5 points it is drawn at.
    #[test]
    fn a_ring_is_one_device_pixel_at_1x() {
        let g = Ring::Empty.geometry(SIDE, 1.0);
        assert!((g.stroke - 1.0).abs() < f32::EPSILON, "{g:?}");
        let outer = g.radius + g.stroke / 2.0;
        for edge in [g.centre.0 - outer, g.centre.0 + outer, g.centre.1 - outer, g.centre.1 + outer]
        {
            assert!((edge - edge.round()).abs() < 1e-4, "an edge off the grid: {edge} in {g:?}");
        }
        assert!(g.centre.0 > 0.0 && g.centre.0 + outer <= SIDE, "inside the glyph: {g:?}");
        let two = Ring::Empty.geometry(SIDE, 2.0);
        assert!(two.stroke.mul_add(2.0, -3.0).abs() < 1e-4, "{two:?}");
    }

    /// A dashed ring's six gaps are each two device pixels or more along the circle at 1x,
    /// so it never reads as a plain circle, and its dashes stand evenly round it.
    #[test]
    fn a_dashed_ring_shows_its_gaps_at_1x() {
        let g = Ring::Dashed.geometry(SIDE, 1.0);
        assert_eq!(g.arcs.len(), 6);
        let around = TAU * g.radius;
        let mut ends: Vec<(f32, f32)> = g.arcs.clone();
        ends.push((g.arcs[0].0 + 1.0, g.arcs[0].1 + 1.0));
        for pair in ends.windows(2) {
            let gap = (pair[1].0 - pair[0].1) * around;
            // The stroke's butt ends stand square, so the gap is the arc between them.
            assert!(gap >= 2.0, "a gap of {gap:.2} px in {g:?}");
        }
        let first = g.arcs[0];
        assert!((first.0 + first.1 - 1.0 / 6.0).abs() < 1e-4, "centred on its sixth: {first:?}");
    }

    /// A pie fills its share of the turn from twelve o'clock clockwise, inside the ring, and
    /// one of no share draws nothing but the ring.
    #[test]
    fn the_pie_fills_its_share_clockwise_from_twelve() {
        let g = Ring::Pie(0.25).geometry(SIDE, 1.0);
        let (radius, sweep) = g.pie.expect("a pie");
        assert!((sweep - 0.25).abs() < f32::EPSILON);
        assert!(radius < g.radius - g.stroke / 2.0, "inside the ring: {g:?}");
        assert_eq!(g.arcs, [(0.0, 1.0)], "with the whole ring round it");
        assert_eq!(Ring::Pie(0.0).geometry(SIDE, 1.0).pie, None);
        assert_eq!(
            Ring::Pie(2.0).geometry(SIDE, 1.0).pie.map(|p| p.1),
            Some(1.0),
            "at most a turn"
        );
    }
}
