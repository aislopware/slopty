//! The state marks drawn by us, at Zed's geometry: a figure ten points across in a sixteen
//! point slot, its line one device pixel at 1x and two at 2x (`docs/decisions/ui.md`, "State is
//! a glyph" and "The chrome's icons are IBM Carbon's at 16").
//!
//! One silhouette for every state, as Linear's statuses are, so the eye compares fill and hue
//! and not shapes: an empty ring, a dashed ring, a ring with a pie of its share, a ring with a
//! check, a turning quarter arc on a faint track, and a filled disc with a figure knocked out of
//! it (`!`, `×` or a check). The figure's outer edge sits on the device grid and its line is a
//! whole number of device pixels, so at 1x it is one crisp pixel wide.

use std::f32::consts::TAU;

use gpui::{
    AnyElement, Bounds, Hsla, IntoElement as _, PathBuilder, Pixels, Styled as _, Window, canvas,
    point, px,
};

/// Which mark.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ring {
    /// The ring alone: up next, idle where a mark is required.
    Empty,
    /// Six dashes round the circle: waiting on its own background work.
    Dashed,
    /// The ring and a sector of this share (0 to 1) of a turn, from twelve o'clock clockwise:
    /// a stage under way, as Linear's in-progress glyph is, and the working mark held still
    /// under Reduce Motion.
    Pie(f32),
    /// A quarter of the ring turned this many steps of [`super::SPIN_STEPS`] round from twelve
    /// o'clock, over the whole ring as a faint track: the working mark, Zed's turning circle.
    Turning(u32),
    /// The ring with a check inside: verified, ready to merge.
    Checked,
    /// A filled disc with a figure knocked out of it.
    Disc(Knock),
}

/// The figure knocked out of a [`Ring::Disc`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Knock {
    /// An exclamation mark: needs the person.
    Bang,
    /// A cross: failed.
    Cross,
    /// A check: finished, or merged.
    Check,
}

/// The figure's diameter as a share of the slot's side: 10 of 16.
const DIAMETER: f32 = 10.0 / 16.0;
/// The line's width as a share of the side: 1 of 16.
const LINE: f32 = 1.0 / 16.0;
/// The pie's radius as a share of the figure's diameter, clear of the ring inside it.
const PIE: f32 = 0.25;
/// How many dashes a dashed ring has.
const DASHES: u16 = 6;
/// The share of each dash's sixth of the turn it draws; the rest is its gap.
const DASH: f32 = 0.55;
/// How much of a turn the working mark's arc sweeps.
const ARC: f32 = 0.25;
/// The opacity of the working mark's track under its arc.
const TRACK: f32 = 0.25;
/// A check's three points, as shares of the figure's diameter from its centre, y down.
const CHECK: [(f32, f32); 3] = [(-0.24, 0.02), (-0.07, 0.19), (0.25, -0.15)];
/// How thick a knocked-out check or cross is, as a share of the diameter.
const KNOCK: f32 = 0.15;
/// How far a cross's arms reach from the centre along each diagonal, as a share of the
/// diameter.
const CROSS: f32 = 0.22;
/// Where an exclamation mark's bar starts and ends, and its dot starts, as shares of the
/// diameter below the centre (negative above).
const BANG: (f32, f32, f32) = (-0.30, 0.0, 0.10);

/// A point in points from the slot's top left corner, y down.
type At = (f32, f32);

/// Where a mark's parts lie, in points, from its slot's side and the device's scale.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Geometry {
    /// The centre, from the slot's top left corner.
    pub centre: At,
    /// The figure's outer diameter.
    pub outer: f32,
    /// The ring's middle line's radius.
    pub radius: f32,
    /// The line's width.
    pub stroke: f32,
    /// The arcs the line draws in the mark's ink, as turns clockwise from twelve o'clock:
    /// one whole turn, the dashes, or the working mark's quarter.
    pub arcs: Vec<(f32, f32)>,
    /// The whole ring drawn faint under the arcs.
    pub track: bool,
    /// The pie's radius and the turn it sweeps, from twelve o'clock clockwise.
    pub pie: Option<(f32, f32)>,
    /// A line drawn in the ink inside the ring: a check.
    pub inner: Option<Vec<At>>,
    /// The figure is a filled disc, these shapes knocked out of it.
    pub disc: Option<Vec<Vec<At>>>,
}

impl Ring {
    /// Its parts in a slot `side` points square on a display of `scale` device pixels a point.
    ///
    /// The line is a whole number of device pixels, at least one, and the figure's outer edge
    /// spans a whole number of them from the slot's corner, so the edge lands on pixel
    /// boundaries and the line covers whole pixels where the circle meets its box.
    pub(crate) fn geometry(self, side: f32, scale: f32) -> Geometry {
        let scale = scale.max(1.0);
        let device = |points: f32| points * scale;
        let stroke = device(LINE * side).floor().max(1.0) / scale;
        let outer = device(DIAMETER * side).round().max(2.0) / scale;
        let inset = device((side - outer) / 2.0).round() / scale;
        let centre = (inset + outer / 2.0, inset + outer / 2.0);
        let radius = (outer - stroke) / 2.0;
        let whole = vec![(0.0, 1.0)];
        let mut geometry = Geometry {
            centre,
            outer,
            radius,
            stroke,
            arcs: Vec::new(),
            track: false,
            pie: None,
            inner: None,
            disc: None,
        };
        match self {
            Self::Empty | Self::Checked => geometry.arcs = whole,
            Self::Dashed => {
                let each = 1.0 / f32::from(DASHES);
                let gap = each * (1.0 - DASH);
                geometry.arcs = (0..DASHES)
                    .map(|i| {
                        let from = f32::from(i).mul_add(each, gap / 2.0);
                        (from, each.mul_add(DASH, from))
                    })
                    .collect();
            }
            Self::Pie(share) => {
                geometry.arcs = whole;
                geometry.pie = (share > 0.0).then(|| (PIE * outer, share.min(1.0)));
            }
            Self::Turning(step) => {
                let step = step.checked_rem(super::SPIN_STEPS).unwrap_or(0);
                #[expect(clippy::cast_precision_loss, reason = "a step of a two-dozen-step turn")]
                let from = step as f32 / super::SPIN_STEPS as f32;
                geometry.arcs = vec![(from, from + ARC)];
                geometry.track = true;
            }
            Self::Disc(knock) => geometry.disc = Some(knocked(knock, centre, outer, scale)),
        }
        if matches!(self, Self::Checked) {
            // The check inside the ring, a size down from the one a disc knocks out.
            let inside = stroke.mul_add(-2.0, outer);
            geometry.inner = Some(
                CHECK
                    .iter()
                    .map(|(x, y)| (x.mul_add(inside, centre.0), y.mul_add(inside, centre.1)))
                    .collect(),
            );
        }
        geometry
    }

    /// The mark in a slot `side` square, in `color`.
    #[must_use]
    pub fn draw(self, side: Pixels, color: Hsla) -> AnyElement {
        canvas(|_, _, _| {}, move |bounds, (), window, _cx| self.paint(bounds, color, window))
            .flex_none()
            .size(side)
            .into_any_element()
    }

    /// Paints the mark in `bounds`.
    pub(crate) fn paint(self, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let g = self.geometry(side, window.scale_factor());
        let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let on = |(x, y): At| point(px(left + x), px(top + y));
        let at = |radius: f32, turn: f32| {
            let (sin, cos) = (turn * TAU).sin_cos();
            on((radius.mul_add(sin, g.centre.0), (-radius).mul_add(cos, g.centre.1)))
        };
        let circle = |path: &mut PathBuilder, radius: f32| {
            let r = point(px(radius), px(radius));
            path.move_to(at(radius, 0.0));
            // A whole turn is two halves: an arc from a point to itself draws nothing.
            path.arc_to(r, px(0.0), false, true, at(radius, 0.5));
            path.arc_to(r, px(0.0), false, true, at(radius, 0.0));
            path.close();
        };
        let mut paint = |path: PathBuilder, ink: Hsla| {
            if let Ok(path) = path.build() {
                window.paint_path(path, ink);
            }
        };
        if g.track {
            let mut path = PathBuilder::stroke(px(g.stroke));
            circle(&mut path, g.radius);
            paint(path, color.opacity(TRACK));
        }
        for &(from, to) in &g.arcs {
            let mut path = PathBuilder::stroke(px(g.stroke));
            if to - from >= 1.0 {
                circle(&mut path, g.radius);
            } else {
                let r = point(px(g.radius), px(g.radius));
                path.move_to(at(g.radius, from));
                path.arc_to(r, px(0.0), to - from > 0.5, true, at(g.radius, to));
            }
            paint(path, color);
        }
        if let Some((radius, sweep)) = g.pie {
            let mut path = PathBuilder::fill();
            if sweep >= 1.0 {
                circle(&mut path, radius);
            } else {
                let pr = point(px(radius), px(radius));
                path.move_to(on(g.centre));
                path.line_to(at(radius, 0.0));
                path.arc_to(pr, px(0.0), sweep > 0.5, true, at(radius, sweep));
                path.close();
            }
            paint(path, color);
        }
        if let Some(line) = &g.inner {
            let mut path = PathBuilder::stroke(px(g.stroke));
            let points: Vec<_> = line.iter().copied().map(on).collect();
            path.add_polygon(&points, false);
            paint(path, color);
        }
        if let Some(holes) = &g.disc {
            // GPUI fills even-odd, so each shape inside the disc is a hole in it.
            let mut path = PathBuilder::fill();
            circle(&mut path, g.outer / 2.0);
            for hole in holes {
                let points: Vec<_> = hole.iter().copied().map(on).collect();
                path.add_polygon(&points, true);
            }
            paint(path, color);
        }
    }
}

/// The shapes `knock` cuts out of a disc `outer` across about `centre`, on a display of
/// `scale`: each a simple polygon, none overlapping another, so even-odd leaves each a hole.
fn knocked(knock: Knock, centre: At, outer: f32, scale: f32) -> Vec<Vec<At>> {
    let (cx, cy) = centre;
    match knock {
        Knock::Check => {
            let points: Vec<At> =
                CHECK.iter().map(|(x, y)| (x.mul_add(outer, cx), y.mul_add(outer, cy))).collect();
            vec![thick(&points, KNOCK * outer / 2.0)]
        }
        Knock::Cross => {
            // A plus of two arms rotated a quarter of a right angle each way: one outline.
            let (reach, half) = (CROSS * outer, KNOCK * outer / 2.0);
            let plus = [
                (half, reach),
                (half, half),
                (reach, half),
                (reach, -half),
                (half, -half),
                (half, -reach),
                (-half, -reach),
                (-half, -half),
                (-reach, -half),
                (-reach, half),
                (-half, half),
                (-half, reach),
            ];
            let turn = std::f32::consts::FRAC_1_SQRT_2;
            vec![
                plus.iter()
                    .map(|(x, y)| ((x - y).mul_add(turn, cx), (x + y).mul_add(turn, cy)))
                    .collect(),
            ]
        }
        Knock::Bang => {
            // An even count of device pixels wide about the centre, which lies on a pixel's
            // edge, so the bar and its dot are whole pixels.
            let wide = 2.0 * (KNOCK / 2.0 * outer * scale).round().max(1.0) / scale;
            let (left, right) = (cx - wide / 2.0, cx + wide / 2.0);
            let snap = |y: f32| (y * scale).round() / scale;
            let (top, bottom, dot) = (
                snap(BANG.0.mul_add(outer, cy)),
                snap(BANG.1.mul_add(outer, cy)),
                snap(BANG.2.mul_add(outer, cy)),
            );
            let rect = |top: f32, bottom: f32| {
                vec![(left, top), (right, top), (right, bottom), (left, bottom)]
            };
            vec![rect(top, bottom), rect(dot, dot + wide)]
        }
    }
}

/// The outline of `line` drawn `half` either side, mitred at its joints: one simple polygon.
fn thick(line: &[At], half: f32) -> Vec<At> {
    let normal = |a: At, b: At| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(f32::EPSILON);
        (-dy / length, dx / length)
    };
    let segments: Vec<At> = line
        .windows(2)
        .filter_map(|w| match w {
            [a, b] => Some(normal(*a, *b)),
            _ => None,
        })
        .collect();
    let offsets: Vec<At> = (0..line.len())
        .map(|i| {
            let before = segments.get(i.wrapping_sub(1)).or_else(|| segments.first());
            let after = segments.get(i).or_else(|| segments.last());
            match (before, after) {
                (Some(&a), Some(&b)) => {
                    // The mitre: the normals' sum, long enough that each side stays `half`
                    // from its segment.
                    let dot = a.0.mul_add(b.0, a.1 * b.1);
                    let k = half / (1.0 + dot).max(0.25);
                    ((a.0 + b.0) * k, (a.1 + b.1) * k)
                }
                _ => (0.0, 0.0),
            }
        })
        .collect();
    let left = line.iter().zip(&offsets).map(|(p, o)| (p.0 + o.0, p.1 + o.1));
    let right = line.iter().zip(&offsets).rev().map(|(p, o)| (p.0 - o.0, p.1 - o.1));
    left.chain(right).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon slot, as the chrome draws a status at 1x.
    const SIDE: f32 = 16.0;

    /// At 1x the figure is ten pixels across, its line one device pixel and its outer edge on
    /// pixel boundaries all round, so it is drawn crisp, not smeared over two half-lit pixels;
    /// at 2x it is twenty across and its line two.
    #[test]
    fn a_ring_is_ten_points_and_one_device_pixel_at_1x() {
        let g = Ring::Empty.geometry(SIDE, 1.0);
        assert!((g.stroke - 1.0).abs() < f32::EPSILON && (g.outer - 10.0).abs() < 1e-4, "{g:?}");
        let outer = g.radius + g.stroke / 2.0;
        for edge in [g.centre.0 - outer, g.centre.0 + outer, g.centre.1 - outer, g.centre.1 + outer]
        {
            assert!((edge - edge.round()).abs() < 1e-4, "an edge off the grid: {edge} in {g:?}");
        }
        assert!(g.centre.0 > 0.0 && g.centre.0 + outer <= SIDE, "inside the slot: {g:?}");
        let two = Ring::Empty.geometry(SIDE, 2.0);
        assert!((two.stroke - 1.0).abs() < 1e-4 && (two.outer - 10.0).abs() < 1e-4, "{two:?}");
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
            // The line's butt ends stand square, so the gap is the arc between them.
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
        assert!(radius < g.radius - g.stroke, "inside the ring with a gap: {g:?}");
        assert_eq!(g.arcs, [(0.0, 1.0)], "with the whole ring round it");
        assert_eq!(Ring::Pie(0.0).geometry(SIDE, 1.0).pie, None);
        assert_eq!(
            Ring::Pie(2.0).geometry(SIDE, 1.0).pie.map(|p| p.1),
            Some(1.0),
            "at most a turn"
        );
    }

    /// The working mark is a quarter of the ring on a faint whole track, and each step moves
    /// it on by a step's share of a turn, round to where it began.
    #[test]
    fn the_working_arc_turns_a_quarter_round_its_track() {
        let first = Ring::Turning(0).geometry(SIDE, 1.0);
        assert!(first.track, "{first:?}");
        assert_eq!(first.arcs, [(0.0, ARC)]);
        let steps = super::super::SPIN_STEPS;
        #[expect(clippy::cast_precision_loss, reason = "two dozen steps")]
        let step = 1.0 / steps as f32;
        let next = Ring::Turning(1).geometry(SIDE, 1.0);
        assert!((next.arcs[0].0 - step).abs() < 1e-6, "{next:?}");
        assert_eq!(Ring::Turning(steps).geometry(SIDE, 1.0).arcs, first.arcs, "a whole turn");
    }

    /// A disc's figures lie inside it, clear of its edge, and none crosses itself: the check
    /// and the cross as one outline each, the exclamation mark as a bar and a dot of whole
    /// pixels on both displays.
    #[test]
    fn a_disc_knocks_its_figure_out_inside_its_edge() {
        for scale in [1.0, 2.0] {
            for knock in [Knock::Bang, Knock::Cross, Knock::Check] {
                let g = Ring::Disc(knock).geometry(SIDE, scale);
                let holes = g.disc.clone().expect("a disc");
                let edge = g.outer / 2.0;
                for &(x, y) in holes.iter().flatten() {
                    let from = (x - g.centre.0).hypot(y - g.centre.1);
                    assert!(from < edge - 1.0 / scale, "{knock:?} at {scale}x reaches {from}");
                }
                assert!(g.arcs.is_empty() && g.pie.is_none(), "{knock:?}: the disc alone");
            }
            let bang = Ring::Disc(Knock::Bang).geometry(SIDE, scale).disc.expect("a disc");
            assert_eq!(bang.len(), 2, "a bar and a dot");
            for &(x, y) in bang.iter().flatten() {
                let whole = |v: f32| v.mul_add(scale, -(v * scale).round()).abs() < 1e-3;
                assert!(whole(x) && whole(y), "the bar on whole pixels at {scale}x: {x}, {y}");
            }
        }
        let cross = Ring::Disc(Knock::Cross).geometry(SIDE, 1.0).disc.expect("a disc");
        assert_eq!(cross.len(), 1, "one outline, so even-odd leaves its middle a hole");
        assert_eq!(cross[0].len(), 12);
        let check = Ring::Disc(Knock::Check).geometry(SIDE, 1.0).disc.expect("a disc");
        assert_eq!(check[0].len(), 6, "three points either side");
    }

    /// A ready ring holds a check in its line's width, inside it.
    #[test]
    fn a_checked_ring_holds_its_check_inside() {
        let g = Ring::Checked.geometry(SIDE, 1.0);
        assert_eq!(g.arcs, [(0.0, 1.0)]);
        let inner = g.inner.expect("a check");
        for (x, y) in inner {
            let from = (x - g.centre.0).hypot(y - g.centre.1);
            assert!(from < g.radius - g.stroke, "{from} inside the ring");
        }
    }

    /// A thick line's outline stands `half` off its segments at its ends and its joint.
    #[test]
    fn a_thick_line_keeps_its_width_round_the_joint() {
        let outline = thick(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], 1.0);
        assert_eq!(outline.len(), 6);
        assert!((outline[0].1 - 1.0).abs() < 1e-5, "{outline:?}");
        let joint = outline[1];
        assert!((joint.0 - 9.0).abs() < 1e-4 && (joint.1 - 1.0).abs() < 1e-4, "{outline:?}");
    }
}
