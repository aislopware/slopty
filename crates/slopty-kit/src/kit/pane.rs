//! Tiling's two pieces: a pane's surface and the sash between two panes.
//!
//! Panes meet edge to edge on the window's one ground, parted by the one 1 pt line, 7 % of the
//! ink (`docs/decisions/workspace.md`, tiling). A pane is square and wears nothing of its own: no
//! stand, no ring, no shadow. The line between two is the sash, which the pointer finds across
//! a hit area wider than the line and drags to resize the two.

use gpui::{
    Axis, Bounds, Div, ElementId, InteractiveElement as _, ParentElement as _, Pixels, Stateful,
    Styled as _, div, px,
};
use slopty_theme::{Theme, stroke};

use crate::colors::hsla;

/// A pane's surface: square, on the window's one ground, clipped to itself.
///
/// No stand, no ring, no shadow and no radius: the sashes between panes are the only lines.
/// The caller adds the identity, the size or position, and the children.
#[must_use]
pub fn pane_surface(theme: &Theme) -> Div {
    div().relative().overflow_hidden().bg(hsla(theme.surfaces.ground))
}

/// The sash on the 1 pt `line` between two panes, `line` in its parent's coordinates.
///
/// `axis` is the way the line runs: [`Axis::Vertical`] between panes side by side, dragged
/// left and right under the column-resize cursor; [`Axis::Horizontal`] between stacked panes,
/// dragged up and down under the row-resize cursor.
///
/// What comes back is the hit area, laid absolute and centred on the line,
/// [`slopty_theme::Density::sash`] across; the caller adds the mouse handlers. It paints the one
/// line ([`slopty_theme::Surfaces::stroke`]) at rest, under the pointer and while dragged alike,
/// as `MonoCode`'s does: the resize cursor says it can be dragged.
#[must_use]
pub fn sash(
    id: impl Into<ElementId>,
    theme: &Theme,
    axis: Axis,
    line: Bounds<Pixels>,
) -> Stateful<Div> {
    let thin = px(stroke::LINE);
    let across = px(theme.density.sash);
    let centre = line.center();
    let area = div().id(id).absolute().flex().justify_center().items_center();
    let mark = div().flex_none().bg(hsla(theme.surfaces.stroke));
    match axis {
        Axis::Vertical => area
            .left(centre.x - across / 2.0)
            .top(line.origin.y)
            .w(across)
            .h(line.size.height)
            .cursor_col_resize()
            .child(mark.h_full().w(thin)),
        Axis::Horizontal => area
            .top(centre.y - across / 2.0)
            .left(line.origin.x)
            .h(across)
            .w(line.size.width)
            .cursor_row_resize()
            .child(mark.w_full().h(thin)),
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Axis, Bounds, Context, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, Window, div, point, px, size,
    };
    use slopty_theme::{Theme, Variant};

    use super::{pane_surface, sash};
    use crate::colors::hsla;

    /// A pane is the ground alone: square, with no border and no shadow.
    #[test]
    fn a_pane_is_square_on_the_ground() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let mut pane = pane_surface(&theme);
            let style = pane.style();
            let ground = Some(gpui::Fill::from(hsla(theme.surfaces.ground)));
            assert_eq!(style.background, ground, "{variant:?}");
            assert!(style.corner_radii.top_left.is_none(), "{variant:?}: square");
            assert!(style.border_widths.top.is_none(), "{variant:?}: no ring");
            assert!(style.box_shadow.is_none(), "{variant:?}: no shadow");
        }
    }

    struct Split {
        theme: Theme,
        axis: Axis,
    }

    impl Render for Split {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let line = match self.axis {
                Axis::Vertical => Bounds::new(point(px(200.0), px(0.0)), size(px(1.0), px(300.0))),
                Axis::Horizontal => {
                    Bounds::new(point(px(0.0), px(150.0)), size(px(400.0), px(1.0)))
                }
            };
            div().relative().size_full().child(sash("s", &self.theme, self.axis, line))
        }
    }

    fn drawn(cx: &mut TestAppContext, variant: Variant, axis: Axis) -> (f32, Vec<gpui::Quad>) {
        let theme = Theme::new(variant);
        let (_view, cx) = cx.add_window_view(|_, _| Split { theme, axis });
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.run_until_parked();
        cx.update(|w, _| (w.scale_factor(), w.painted_quads()))
    }

    /// The sash paints the one 1 pt line on its own line's place, in both variants and along
    /// both axes, and nothing else: no green, no wider line.
    #[gpui::test]
    fn a_sash_is_the_one_line(cx: &mut TestAppContext) {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let stroke = hsla(theme.surfaces.stroke);
            for axis in [Axis::Vertical, Axis::Horizontal] {
                let (scale, quads) = drawn(cx, variant, axis);
                let line = quads
                    .iter()
                    .find(|q| q.background.as_solid() == Some(stroke))
                    .unwrap_or_else(|| panic!("{variant:?} {axis:?}: the line"));
                let (across, start) = match axis {
                    Axis::Vertical => (line.bounds.size.width.0, line.bounds.origin.x.0),
                    Axis::Horizontal => (line.bounds.size.height.0, line.bounds.origin.y.0),
                };
                let at = if axis == Axis::Vertical { 200.0 } else { 150.0 };
                assert!(
                    (across / scale - 1.0).abs() < 0.01,
                    "{variant:?} {axis:?}: 1 pt, {across}"
                );
                assert!((start / scale - at).abs() < 0.01, "{variant:?} {axis:?}: on its line");
                let focus = hsla(theme.surfaces.focus);
                assert!(
                    quads.iter().all(|q| q.background.as_solid() != Some(focus)),
                    "{variant:?} {axis:?}: no green"
                );
            }
        }
    }

    /// The pointer finds a sash across the density's hit area centred on its line, not only
    /// on the line's one point.
    #[test]
    fn a_sash_is_found_across_its_hit_area() {
        let theme = Theme::default();
        let line = Bounds::new(point(px(200.0), px(10.0)), size(px(1.0), px(300.0)));
        let mut area = sash("s", &theme, Axis::Vertical, line);
        let style = area.style();
        let across = theme.density.sash;
        assert_eq!(style.size.width, Some(px(across).into()), "the density's sash");
        let left = 200.5 - across / 2.0;
        assert_eq!(style.inset.left, Some(px(left).into()), "centred on the line");
        assert_eq!(style.inset.top, Some(px(10.0).into()), "along it");
        assert_eq!(style.mouse_cursor, Some(gpui::CursorStyle::ResizeColumn), "resizes columns");
    }
}
