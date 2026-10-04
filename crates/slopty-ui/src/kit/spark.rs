//! A sparkline: a short history drawn as one line over its own range, the newest value at the
//! right edge, sliding one step left as each value comes.
//!
//! After Ely GPUI Components (`src/data_display/spark.rs`, and the one-step slide of
//! `src/charts/realtime.rs`), Copyright (c) 2026 Ely GPUI Component contributors, MIT OR
//! Apache-2.0. Reworked onto Slopty's tokens and [`super::motion`]: one line and a dot at the
//! newest value, no axes, and the slide keyed on how many values came, so a repaint between
//! them never moves it.

use gpui::accesskit::Role;
use gpui::{
    AnimationExt as _, App, Bounds, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px,
};

use super::Pace;

/// A sparkline of `values`, oldest first, in `steps` places across.
///
/// The newest is at the right edge, each older one a step left, scaled to the series' own low and
/// high. It is said as `label`, the words for its newest value.
#[derive(IntoElement)]
pub struct Spark {
    id: SharedString,
    values: Vec<f32>,
    pushed: u64,
    steps: usize,
    label: SharedString,
    tone: Hsla,
    width: Pixels,
    height: Pixels,
}

impl std::fmt::Debug for Spark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spark")
            .field("id", &self.id)
            .field("values", &self.values)
            .finish_non_exhaustive()
    }
}

impl Spark {
    /// `values` under `id`, `pushed` counting every value that ever came (it keys the slide),
    /// drawn `width` × `height` in `tone`.
    #[must_use]
    pub fn new(
        id: impl Into<SharedString>,
        values: Vec<f32>,
        pushed: u64,
        steps: usize,
        label: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            values,
            pushed,
            steps: steps.max(2),
            label: label.into(),
            tone: gpui::transparent_black(),
            width: px(0.0),
            height: px(0.0),
        }
    }

    /// The line's tone.
    #[must_use]
    pub const fn tone(mut self, tone: Hsla) -> Self {
        self.tone = tone;
        self
    }

    /// Its size, already zoomed.
    #[must_use]
    pub const fn size(mut self, width: Pixels, height: Pixels) -> Self {
        self.width = width;
        self.height = height;
        self
    }
}

/// Where each of `values` sits in a `width` × `height` box, `steps` places across, the newest
/// at the right edge pushed right by `shift` steps (1 as a new value comes in, 0 once it has
/// slid into place). The series' low is the bottom and its high the top; a flat series runs
/// across the middle. The line is kept inside the box by `inset` at top and bottom.
#[must_use]
fn points(
    values: &[f32],
    steps: usize,
    (width, height): (f32, f32),
    inset: f32,
    shift: f32,
) -> Vec<(f32, f32)> {
    let finite: Vec<f32> = values.iter().copied().filter(|v| v.is_finite()).collect();
    let (low, high) = finite
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    #[expect(clippy::cast_precision_loss, reason = "a few dozen places across")]
    let step = width / steps.saturating_sub(1).max(1) as f32;
    let room = 2.0_f32.mul_add(-inset, height).max(0.0);
    let newest = finite.len().saturating_sub(1);
    finite
        .iter()
        .enumerate()
        .map(|(ix, v)| {
            #[expect(clippy::cast_precision_loss, reason = "a few dozen places back")]
            let back = newest.saturating_sub(ix) as f32;
            let x = (shift - back).mul_add(step, width);
            let share = if high > low { (v - low) / (high - low) } else { 0.5 };
            (x, share.mul_add(-room, inset + room))
        })
        .collect()
}

impl RenderOnce for Spark {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, values, pushed, steps, label, tone, width, height } = self;
        let came = super::on_change(
            ElementId::Name(SharedString::from(format!("{id}-came"))),
            &pushed,
            window,
            cx,
        );
        let draw = move |shift: f32| {
            let values = values.clone();
            canvas(
                |_bounds, _window, _cx| {},
                move |bounds: Bounds<Pixels>, (), window, _cx| {
                    let line = px(1.5);
                    let size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                    let at = points(&values, steps, size, f32::from(line), shift);
                    let to = |(x, y): (f32, f32)| {
                        point(bounds.origin.x + px(x), bounds.origin.y + px(y))
                    };
                    let mut path = PathBuilder::stroke(line);
                    let mut first = true;
                    for p in &at {
                        if first {
                            path.move_to(to(*p));
                            first = false;
                        } else {
                            path.line_to(to(*p));
                        }
                    }
                    if at.len() > 1
                        && let Ok(path) = path.build()
                    {
                        window.paint_path(path, tone);
                    }
                    if let Some(newest) = at.last() {
                        let (c, r) = (to(*newest), line);
                        let mut dot = PathBuilder::fill();
                        dot.move_to(point(c.x + r, c.y));
                        dot.arc_to(point(r, r), px(0.0), false, true, point(c.x - r, c.y));
                        dot.arc_to(point(r, r), px(0.0), false, true, point(c.x + r, c.y));
                        dot.close();
                        if let Ok(dot) = dot.build() {
                            window.paint_path(dot, tone);
                        }
                    }
                },
            )
            .size_full()
        };
        let host = div()
            .id(ElementId::Name(id.clone()))
            .debug_selector(move || id.to_string())
            .role(Role::Image)
            .aria_label(label)
            .flex_none()
            .w(width)
            .h(height)
            .overflow_hidden();
        if came == 0 || !super::motion(cx) {
            return host.child(draw(0.0)).into_any_element();
        }
        let key = ElementId::Name(SharedString::from(format!("spark-slide-{came}")));
        host.child(
            div().size_full().with_animation(key, Pace::Settle.animation(), move |el, t| {
                el.child(draw(1.0 - t))
            }),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::points;

    /// The newest value is at the right edge, each older one a step left, the high at the top
    /// and the low at the bottom; a new value comes in a step right and slides into place.
    #[test]
    fn the_newest_is_at_the_right_on_the_series_own_range() {
        let at = points(&[10.0, 30.0, 20.0], 5, (40.0, 12.0), 1.0, 0.0);
        assert_eq!(at, [(20.0, 11.0), (30.0, 1.0), (40.0, 6.0)]);
        let coming = points(&[10.0, 30.0, 20.0], 5, (40.0, 12.0), 1.0, 1.0);
        assert_eq!(coming.last(), Some(&(50.0, 6.0)), "a step out, past the edge");
    }

    /// A flat series runs across the middle, and a value that is no number is left out.
    #[test]
    fn a_flat_series_runs_across_the_middle() {
        let at = points(&[5.0, f32::NAN, 5.0], 3, (20.0, 10.0), 0.0, 0.0);
        assert_eq!(at, [(10.0, 5.0), (20.0, 5.0)]);
        assert_eq!(points(&[], 3, (20.0, 10.0), 0.0, 0.0), []);
    }
}
