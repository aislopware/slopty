//! Zoom and pan inside a stream tile: where the picture is drawn over the tile's body, and
//! which point of the picture a point of the body is.
//!
//! The picture keeps its aspect. At fit it is drawn as large as it goes whole, centred in the
//! body, and the body's own surface shows beside it ([`fit`]): that rectangle is the *frame*.
//! Every quantity of a [`Zoom`] is a fraction of the frame, so a tile that changes size (a
//! rotation, a column resized) keeps the same part of the picture in view with no rescaling. At
//! fit the picture fills the frame (`scale` 1, `origin` 0). Zoomed, it is drawn `scale` times the
//! frame's size with its top-left at `origin`, which is never positive and never lets an edge of
//! the picture come inside the frame: a zoomed picture always covers the whole frame.

/// How a stream is drawn over its tile's body.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Zoom {
    /// The picture's size over the frame's; 1 at fit.
    scale: f32,
    /// The picture's top-left, in fractions of the frame's size from the frame's top-left.
    origin: (f32, f32),
}

impl Default for Zoom {
    fn default() -> Self {
        Self::FIT
    }
}

/// Past this, a scale is not fit any more: a pinch that returns lands a hair off 1.
const FIT_EPSILON: f32 = 1e-3;

impl Zoom {
    /// The picture filling the frame.
    pub const FIT: Self = Self { scale: 1.0, origin: (0.0, 0.0) };

    /// The picture's size over the frame's.
    #[must_use]
    pub const fn scale(self) -> f32 {
        self.scale
    }

    /// The picture's top-left in fractions of the frame.
    #[must_use]
    pub const fn origin(self) -> (f32, f32) {
        self.origin
    }

    /// Whether the picture fills the frame and no more.
    #[must_use]
    pub fn is_fit(self) -> bool {
        self.scale <= 1.0 + FIT_EPSILON
    }

    /// `scale` held to `1..=max` and the origin to where the picture covers the frame.
    #[must_use]
    pub fn clamped(self, max: f32) -> Self {
        let scale = self.scale.clamp(1.0, max.max(1.0));
        let hold = |o: f32| o.clamp(1.0 - scale, 0.0);
        Self { scale, origin: (hold(self.origin.0), hold(self.origin.1)) }
    }

    /// Magnified by `factor` about `at` (a point of the frame, in fractions of it): the point of
    /// the picture under `at` stays under it, unless a clamp moves it.
    #[must_use]
    pub fn about(self, at: (f32, f32), factor: f32, max: f32) -> Self {
        let scale = (self.scale * factor).clamp(1.0, max.max(1.0));
        let (fx, fy) = self.to_picture(at);
        Self { scale, origin: (fx.mul_add(-scale, at.0), fy.mul_add(-scale, at.1)) }.clamped(max)
    }

    /// Moved by `by` (fractions of the frame), the way the fingers moved.
    #[must_use]
    pub fn panned(self, by: (f32, f32), max: f32) -> Self {
        Self { origin: (self.origin.0 + by.0, self.origin.1 + by.1), ..self }.clamped(max)
    }

    /// The point of the picture under `at` (a point of the frame, both in fractions): 0 at the
    /// picture's top-left edge, 1 at its bottom-right.
    #[must_use]
    pub fn to_picture(self, at: (f32, f32)) -> (f32, f32) {
        ((at.0 - self.origin.0) / self.scale, (at.1 - self.origin.1) / self.scale)
    }

    /// Where the point `f` of the picture is drawn, in fractions of the frame.
    #[must_use]
    pub const fn to_frame(self, f: (f32, f32)) -> (f32, f32) {
        (f.0.mul_add(self.scale, self.origin.0), f.1.mul_add(self.scale, self.origin.1))
    }

    /// A double tap at `at`: fit when zoomed, else `target` about the tapped point.
    #[must_use]
    pub fn toggled(self, at: (f32, f32), target: f32, max: f32) -> Self {
        if self.is_fit() { self.about(at, target / self.scale, max) } else { Self::FIT }
    }

    /// Panned as little as it takes to draw the point `f` of the picture at least `margin`
    /// (fractions of the frame) inside the frame's edges: a cursor kept in view.
    #[must_use]
    pub fn revealing(self, f: (f32, f32), margin: (f32, f32), max: f32) -> Self {
        let (x, y) = self.to_frame(f);
        let push = |p: f32, m: f32| {
            let m = m.clamp(0.0, 0.5);
            if p < m {
                m - p
            } else if p > 1.0 - m {
                1.0 - m - p
            } else {
                0.0
            }
        };
        self.panned((push(x, margin.0), push(y, margin.1)), max)
    }
}

/// Where a picture sits at fit inside a body: its top-left and size, in the body's unit from the
/// body's top-left.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Frame {
    /// The picture's top-left.
    pub origin: (f32, f32),
    /// The picture's size.
    pub size: (f32, f32),
}

/// The frame a picture of `picture` pixels takes in a body of `body`: as large as it goes whole,
/// its aspect kept, centred. A picture of no size yet takes the whole body.
#[must_use]
pub fn fit(body: (f32, f32), picture: (u32, u32)) -> Frame {
    if picture.0 == 0 || picture.1 == 0 {
        return Frame { origin: (0.0, 0.0), size: body };
    }
    #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
    let (w, h) = (picture.0 as f32, picture.1 as f32);
    let scale = (body.0 / w).min(body.1 / h).max(0.0);
    let size = (w * scale, h * scale);
    Frame { origin: ((body.0 - size.0) / 2.0, (body.1 - size.1) / 2.0), size }
}

/// The scale at which one pixel of the target is one pixel of the device: the target's native
/// width over the frame's width in device pixels. Below 1 when fit already magnifies.
#[must_use]
pub fn one_to_one(native_width: f32, frame_width: f32, scale_factor: f32) -> f32 {
    let device = (frame_width * scale_factor).max(1.0);
    native_width.max(1.0) / device
}

/// How far a pinch may go: twice one to one, and never less than twice fit.
#[must_use]
pub fn max_scale(one_to_one: f32) -> f32 {
    2.0 * one_to_one.max(1.0)
}

/// Where a double tap at fit goes: one to one, or twice fit when fit already magnifies.
#[must_use]
pub fn double_tap_target(one_to_one: f32) -> f32 {
    if one_to_one > 1.0 + FIT_EPSILON { one_to_one } else { 2.0 }
}

/// What the readout says: `Fit`, or the size against one to one (`100 %` is pixel for pixel).
#[must_use]
pub fn readout(zoom: Zoom, one_to_one: f32) -> String {
    if zoom.is_fit() {
        return "Fit".to_owned();
    }
    let percent = zoom.scale / one_to_one.max(f32::EPSILON) * 100.0;
    format!("{percent:.0}%")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    /// At fit a point of the frame is the same point of the picture, and back again.
    #[test]
    fn at_fit_the_frame_is_the_picture() {
        let z = Zoom::FIT;
        for at in [(0.0, 0.0), (0.25, 0.75), (1.0, 1.0)] {
            assert!(near(z.to_picture(at), at), "{at:?}");
            assert!(near(z.to_frame(at), at), "{at:?}");
        }
    }

    /// A picture of another aspect keeps its own: a wide one spans the body's width with equal
    /// bars above and below, a tall one its height with bars beside it, one of the body's own
    /// aspect fills it, and one of no size yet takes the whole body.
    #[test]
    fn fit_keeps_the_pictures_aspect_and_centres_it() {
        let wide = fit((400.0, 300.0), (1600, 800));
        assert_eq!(wide, Frame { origin: (0.0, 50.0), size: (400.0, 200.0) });
        let tall = fit((400.0, 300.0), (300, 600));
        assert_eq!(tall, Frame { origin: (125.0, 0.0), size: (150.0, 300.0) });
        let same = fit((400.0, 300.0), (800, 600));
        assert_eq!(same, Frame { origin: (0.0, 0.0), size: (400.0, 300.0) });
        assert_eq!(fit((400.0, 300.0), (0, 600)).size, (400.0, 300.0));
        // The e2e's case: a 1.6 window in a body taller than wide.
        let f = fit((440.0, 500.0), (1280, 800));
        assert!(near(f.size, (440.0, 275.0)), "{f:?}");
        assert!(near(f.origin, (0.0, 112.5)), "{f:?}");
        assert!((f.size.0 / f.size.1 - 1.6).abs() < 1e-4);
    }

    /// A pinch keeps the point under the fingers where it was, at the frame's middle, at a
    /// corner and off-centre, and mapping there and back is the identity at any zoom.
    #[test]
    fn a_pinch_keeps_the_point_under_the_fingers() {
        for at in [(0.5, 0.5), (0.2, 0.7), (0.9, 0.1)] {
            let before = Zoom::FIT.to_picture(at);
            let z = Zoom::FIT.about(at, 2.0, 8.0);
            assert!((z.scale() - 2.0).abs() < 1e-6, "{z:?}");
            assert!(near(z.to_picture(at), before), "{at:?}: {z:?}");
            let again = z.about(at, 1.5, 8.0);
            assert!(near(again.to_picture(at), before), "a second step too: {again:?}");
            for p in [(0.0, 0.0), (0.3, 0.6), (1.0, 1.0)] {
                assert!(near(again.to_frame(again.to_picture(p)), p), "{p:?}");
            }
        }
    }

    /// At the frame's corner the picture's corner stays put: zooming about the top-left keeps
    /// the origin at zero, and about the bottom-right keeps the far edge on the frame's.
    #[test]
    fn zooming_at_an_edge_keeps_that_edge() {
        let z = Zoom::FIT.about((0.0, 0.0), 3.0, 8.0);
        assert!(near(z.origin(), (0.0, 0.0)), "{z:?}");
        let z = Zoom::FIT.about((1.0, 1.0), 3.0, 8.0);
        assert!(near(z.origin(), (-2.0, -2.0)), "{z:?}");
        assert!(near(z.to_picture((1.0, 1.0)), (1.0, 1.0)), "the corner on the corner");
    }

    /// The scale stays between fit and the maximum, and a pan never uncovers the frame: the
    /// picture's edges stop at the frame's.
    #[test]
    fn scale_and_pan_are_clamped() {
        let z = Zoom::FIT.about((0.5, 0.5), 0.2, 4.0);
        assert_eq!(z, Zoom::FIT, "no smaller than fit");
        let z = Zoom::FIT.about((0.5, 0.5), 100.0, 4.0);
        assert!((z.scale() - 4.0).abs() < 1e-6, "no larger than the maximum: {z:?}");
        let z = Zoom::FIT.about((0.5, 0.5), 2.0, 4.0);
        assert!(near(z.origin(), (-0.5, -0.5)), "{z:?}");
        let far = z.panned((10.0, -10.0), 4.0);
        assert!(near(far.origin(), (0.0, -1.0)), "left and bottom edges hold: {far:?}");
        assert!(near(far.to_picture((0.0, 0.0)), (0.0, 0.5)));
        assert!(near(far.to_picture((1.0, 1.0)), (0.5, 1.0)));
        assert_eq!(Zoom::FIT.panned((0.3, 0.3), 4.0), Zoom::FIT, "nothing to pan at fit");
        // A clamp after a bigger maximum shrank: the origin follows the scale back in.
        let shrunk = Zoom::FIT.about((1.0, 1.0), 4.0, 4.0).clamped(2.0);
        assert!(near(shrunk.origin(), (-1.0, -1.0)), "{shrunk:?}");
    }

    /// A double tap at fit goes to one to one about the tapped point; any zoom goes back to fit.
    /// Where fit already magnifies, it goes to twice fit.
    #[test]
    fn a_double_tap_toggles_fit_and_one_to_one() {
        let one = one_to_one(5120.0, 390.0, 3.0);
        assert!((one - 5120.0 / 1170.0).abs() < 1e-4, "{one}");
        let max = max_scale(one);
        let at = (0.3, 0.6);
        let z = Zoom::FIT.toggled(at, double_tap_target(one), max);
        assert!((z.scale() - one).abs() < 1e-4, "{z:?}");
        assert!(near(z.to_picture(at), at), "the tapped point stays under the finger");
        assert_eq!(z.toggled(at, double_tap_target(one), max), Zoom::FIT, "and back");
        let small = one_to_one(400.0, 800.0, 2.0);
        assert!((double_tap_target(small) - 2.0).abs() < f32::EPSILON, "{small}");
        assert!((max_scale(small) - 2.0).abs() < f32::EPSILON);
        assert!((max_scale(one) / one - 2.0).abs() < 1e-4);
    }

    /// Revealing pans only as far as the margin needs, and not at all for a point in view.
    #[test]
    fn revealing_pans_just_enough() {
        let z = Zoom::FIT.about((0.5, 0.5), 4.0, 8.0);
        assert_eq!(z.revealing(z.to_picture((0.5, 0.5)), (0.1, 0.1), 8.0), z, "in view");
        // A point just off the right edge comes in to the margin.
        let f = z.to_picture((1.2, 0.5));
        let r = z.revealing(f, (0.1, 0.1), 8.0);
        assert!(near(r.to_frame(f), (0.9, 0.5)), "{r:?}");
        // The picture's own corner can only reach the frame's corner.
        let r = z.revealing((1.0, 1.0), (0.1, 0.1), 8.0);
        assert!(near(r.to_frame((1.0, 1.0)), (1.0, 1.0)), "{r:?}");
    }

    /// The readout says fit, or the size against one to one.
    #[test]
    fn the_readout_is_against_one_to_one() {
        assert_eq!(readout(Zoom::FIT, 2.0), "Fit");
        let z = Zoom::FIT.about((0.5, 0.5), 2.0, 8.0);
        assert_eq!(readout(z, 2.0), "100%");
        assert_eq!(readout(z, 1.0), "200%");
        assert_eq!(readout(z, 4.0), "50%");
    }
}
