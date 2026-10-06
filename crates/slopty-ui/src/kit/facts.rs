//! A line of facts parted by the quiet middle dot that wraps between facts and never leaves a
//! dot at either end of a line (`docs/decisions/ui.md`, "How surfaces adapt to their room").
//!
//! Before it, facts and their dots were siblings in a wrapping flex row, so a narrow board's
//! pipeline row ended its first line in a dot that parted nothing. Here each fact is laid out at
//! its own width, the facts are set into lines as words are, and a dot is drawn only between two
//! facts on one line. A fact wider than the whole line has the line to itself and narrows to it,
//! ending in its own ellipsis.

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, Pixels,
    Refineable as _, Size, Style, StyleRefinement, Styled, Window, point, px,
};
use slopty_theme::Theme;

/// Facts set into lines with dots between them on a line; [`facts_row`] makes one.
pub struct FactsRow {
    id: ElementId,
    style: StyleRefinement,
    /// Each fact, after the first with the dot that parts it from the one before.
    facts: Vec<(Option<AnyElement>, AnyElement)>,
    /// Makes a dot.
    dot: gpui::Hsla,
}

/// A row, `id`, of facts parted by middle dots in `theme`'s separator ink, wrapping between
/// facts. Its `gap` parts a dot from the facts beside it and its lines from each other.
#[must_use]
pub fn facts_row(id: impl Into<ElementId>, theme: &Theme) -> FactsRow {
    FactsRow {
        id: id.into(),
        style: StyleRefinement::default(),
        facts: Vec::new(),
        dot: crate::palette::separator_ink(theme),
    }
}

impl FactsRow {
    /// One more fact, after a dot when it is not the first.
    #[must_use]
    pub fn fact(mut self, element: impl IntoElement) -> Self {
        let ix = self.facts.len();
        let dot = (ix > 0).then(|| {
            let id = self.id.clone();
            gpui::div()
                .debug_selector(move || format!("{id}-dot-{ix}"))
                .flex_none()
                .text_color(self.dot)
                .child("\u{b7}")
                .into_any_element()
        });
        // In a block of its own, which takes the width it is laid out at, so a fact wider than
        // the line narrows to it: a flex row laid out as a root keeps its content's width.
        let fact = gpui::div().child(element).into_any_element();
        self.facts.push((dot, fact));
        self
    }

    /// Every fact of `facts`, in order.
    #[must_use]
    pub fn facts<E: IntoElement>(self, facts: impl IntoIterator<Item = E>) -> Self {
        facts.into_iter().fold(self, Self::fact)
    }
}

/// Where a fact goes on its line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FactAt {
    /// Its line, from the first.
    pub line: usize,
    /// Its leading edge on the line.
    pub x: Pixels,
    /// Its width, narrowed to the line when wider.
    pub width: Pixels,
    /// The leading edge of the dot before it, when one is drawn: only between facts on a line.
    pub dot: Option<Pixels>,
}

/// The facts of `widths` set into lines of `room`, as words are.
///
/// A fact goes on the line after a `gap`, a dot of `dot` and a `gap` when it fits there, else
/// it starts the next line with no dot. A fact wider than `room` has a line to itself, narrowed
/// to it.
#[must_use]
pub fn wrap_facts(widths: &[Pixels], dot: Pixels, gap: Pixels, room: Pixels) -> Vec<FactAt> {
    let room = room.max(px(0.0));
    let mut placed: Vec<FactAt> = Vec::with_capacity(widths.len());
    let mut line = 0_usize;
    let mut x = px(0.0);
    for (ix, width) in widths.iter().enumerate() {
        let width = (*width).min(room);
        let parted = x + gap + dot + gap;
        let at = if ix == 0 {
            FactAt { line, x: px(0.0), width, dot: None }
        } else if parted + width <= room {
            FactAt { line, x: parted, width, dot: Some(x + gap) }
        } else {
            line = line.saturating_add(1);
            FactAt { line, x: px(0.0), width, dot: None }
        };
        x = at.x + width;
        placed.push(at);
    }
    placed
}

/// Each line's height: the tallest of the facts of `heights` placed on it at `placed`.
fn line_heights(placed: &[FactAt], heights: &[Pixels]) -> Vec<Pixels> {
    let mut lines: Vec<Pixels> = Vec::new();
    for (at, height) in placed.iter().zip(heights) {
        if lines.len() <= at.line {
            lines.resize(at.line.saturating_add(1), px(0.0));
        }
        if let Some(line) = lines.get_mut(at.line) {
            *line = (*line).max(*height);
        }
    }
    lines
}

/// What a facts row measured before it was laid out: each fact and dot at its own width.
pub struct MeasuredFacts {
    sizes: Vec<Size<Pixels>>,
    dot: Pixels,
}

impl std::fmt::Debug for MeasuredFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasuredFacts").field("sizes", &self.sizes).field("dot", &self.dot).finish()
    }
}

/// The room a row is offered: its width when known, the widest line it would set otherwise.
fn offered(known: Option<Pixels>, available: AvailableSpace, widths: &[Pixels]) -> Pixels {
    let widest = widths.iter().copied().fold(px(0.0), Pixels::max);
    known.unwrap_or(match available {
        AvailableSpace::Definite(room) => room,
        AvailableSpace::MinContent => widest,
        AvailableSpace::MaxContent => px(f32::MAX),
    })
}

impl FactsRow {
    /// The text style it gives what it lays out, as a `div` gives its children.
    fn given_text(&self) -> Option<gpui::TextStyleRefinement> {
        let mut style = Style::default();
        style.refine(&self.style);
        style.text_style().cloned()
    }
}

impl Element for FactsRow {
    type PrepaintState = Vec<AnyElement>;
    type RequestLayoutState = MeasuredFacts;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, MeasuredFacts) {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            let mut style = Style::default();
            style.refine(&self.style);
            let natural =
                Size { width: AvailableSpace::MaxContent, height: AvailableSpace::MaxContent };
            let mut dot = px(0.0);
            let mut sizes = Vec::with_capacity(self.facts.len());
            for (parting, fact) in &mut self.facts {
                if let Some(parting) = parting {
                    dot = parting.layout_as_root(natural, window, cx).width;
                }
                sizes.push(fact.layout_as_root(natural, window, cx));
            }
            let widths: Vec<Pixels> = sizes.iter().map(|s| s.width).collect();
            let heights: Vec<Pixels> = sizes.iter().map(|s| s.height).collect();
            let rem = window.rem_size();
            let (gap, row_gap) = (
                style.gap.width.to_pixels(px(0.0).into(), rem),
                style.gap.height.to_pixels(px(0.0).into(), rem),
            );
            let layout = window.request_measured_layout(style, move |known, available, _w, _cx| {
                let room = offered(known.width, available.width, &widths);
                let placed = wrap_facts(&widths, dot, gap, room);
                let lines = line_heights(&placed, &heights);
                let count = u16::try_from(lines.len()).unwrap_or(u16::MAX);
                let height = lines.iter().fold(px(0.0), |sum, h| sum + *h)
                    + row_gap * f32::from(count.saturating_sub(1));
                let width = placed.iter().map(|at| at.x + at.width).fold(px(0.0), Pixels::max);
                Size { width: known.width.unwrap_or(width), height: known.height.unwrap_or(height) }
            });
            (layout, MeasuredFacts { sizes, dot })
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut MeasuredFacts,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            let mut style = Style::default();
            style.refine(&self.style);
            let rem = window.rem_size();
            let gap = style.gap.width.to_pixels(bounds.size.width.into(), rem);
            let row_gap = style.gap.height.to_pixels(bounds.size.height.into(), rem);
            let sizes = &request_layout.sizes;
            let widths: Vec<Pixels> = sizes.iter().map(|s| s.width).collect();
            let heights: Vec<Pixels> = sizes.iter().map(|s| s.height).collect();
            let placed = wrap_facts(&widths, request_layout.dot, gap, bounds.size.width);
            let lines = line_heights(&placed, &heights);
            let tops: Vec<Pixels> = lines
                .iter()
                .scan(bounds.origin.y, |top, height| {
                    let at = *top;
                    *top += *height + row_gap;
                    Some(at)
                })
                .collect();
            let mut shown = Vec::with_capacity(self.facts.len().saturating_mul(2));
            for ((parting, mut fact), (at, size)) in
                std::mem::take(&mut self.facts).into_iter().zip(placed.iter().zip(sizes))
            {
                let top = tops.get(at.line).copied().unwrap_or(bounds.origin.y);
                let high = lines.get(at.line).copied().unwrap_or(size.height);
                let centred = |height: Pixels| top + (high - height).max(px(0.0)) / 2.0;
                if let (Some(mut parting), Some(x)) = (parting, at.dot) {
                    let natural = Size {
                        width: AvailableSpace::MaxContent,
                        height: AvailableSpace::MaxContent,
                    };
                    let dot = parting.layout_as_root(natural, window, cx);
                    parting.prepaint_at(
                        point(bounds.origin.x + x, centred(dot.height)),
                        window,
                        cx,
                    );
                    shown.push(parting);
                }
                let height = if at.width < size.width {
                    let room = Size {
                        width: AvailableSpace::Definite(at.width),
                        height: AvailableSpace::MaxContent,
                    };
                    fact.layout_as_root(room, window, cx).height
                } else {
                    size.height
                };
                fact.prepaint_at(point(bounds.origin.x + at.x, centred(height)), window, cx);
                shown.push(fact);
            }
            shown
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut MeasuredFacts,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            for element in prepaint {
                element.paint(window, cx);
            }
        });
    }
}

impl std::fmt::Debug for FactsRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactsRow")
            .field("id", &self.id)
            .field("facts", &self.facts.len())
            .finish_non_exhaustive()
    }
}

impl IntoElement for FactsRow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for FactsRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, VisualTestContext, div};

    use super::*;

    /// Set as words are: a dot only between two facts on one line, a fact too wide for any
    /// line on a line of its own and narrowed to it.
    #[test]
    fn facts_set_into_lines_with_dots_only_between_facts_on_a_line() {
        let (dot, gap) = (px(4.0), px(4.0));
        let widths = [px(60.0), px(80.0), px(40.0)];
        // All on one line: 60 + 12 + 80 + 12 + 40.
        let one = wrap_facts(&widths, dot, gap, px(204.0));
        assert!(one.iter().all(|at| at.line == 0), "{one:?}");
        assert_eq!(
            one.iter().map(|at| at.dot).collect::<Vec<_>>(),
            [None, Some(px(64.0)), Some(px(156.0))]
        );
        // A point less: the last starts the second line, with no dot before it.
        let two = wrap_facts(&widths, dot, gap, px(203.0));
        assert_eq!(two.get(2), Some(&FactAt { line: 1, x: px(0.0), width: px(40.0), dot: None }));
        // Narrower than a fact: that fact narrows to the line, on its own.
        let narrow = wrap_facts(&widths, dot, gap, px(70.0));
        assert_eq!(narrow.iter().map(|at| at.line).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(narrow.get(1).map(|at| at.width), Some(px(70.0)));
        assert!(narrow.iter().all(|at| at.dot.is_none()));
    }

    struct Facts {
        width: f32,
    }

    impl Render for Facts {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let fact = |id: &'static str, width: f32| {
                div().debug_selector(move || id.to_owned()).w(px(width)).max_w_full().h(px(16.0))
            };
            div().w(px(self.width)).child(
                facts_row("facts", &Theme::default())
                    .gap_x(px(4.0))
                    .gap_y(px(2.0))
                    .text_size(px(12.0))
                    .fact(fact("a", 60.0))
                    .fact(fact("b", 80.0))
                    .fact(fact("c", 40.0))
                    .fact(fact("d", 50.0)),
            )
        }
    }

    /// Laid out for real at any width, a line never starts or ends with a dot: every dot drawn
    /// has a fact before it and after it on its own line, and the row is as tall as its lines.
    #[gpui::test]
    fn a_wrapped_fact_row_never_ends_or_starts_a_line_with_a_separator(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, _| Facts { width: 400.0 });
        let laid = |cx: &mut VisualTestContext, id: &str| cx.debug_bounds(gpui_leak(id));
        for width in [400.0, 200.0, 150.0, 90.0, 50.0] {
            view.update(cx, |facts, cx| {
                facts.width = width;
                cx.notify();
            });
            cx.run_until_parked();
            let facts: Vec<Bounds<Pixels>> = ["a", "b", "c", "d"]
                .iter()
                .map(|id| laid(cx, id).unwrap_or_else(|| panic!("{id} at {width}")))
                .collect();
            for fact in &facts {
                assert!(fact.right() <= px(width) + px(0.5), "{fact:?} inside {width}");
            }
            for (ix, pair) in facts.windows(2).enumerate() {
                let dot = laid(cx, &format!("facts-dot-{}", ix.saturating_add(1)));
                let [before, after] = pair else {
                    continue;
                };
                let same_line = (before.top() - after.top()).abs() < px(0.5);
                assert_eq!(dot.is_some(), same_line, "dot {ix} at {width}: {before:?} {after:?}");
                if let Some(dot) = dot {
                    assert!(before.right() <= dot.left() && dot.right() <= after.left());
                }
            }
        }
    }

    /// A selector made at run time, for `debug_bounds`, which takes a static one.
    fn gpui_leak(id: &str) -> &'static str {
        Box::leak(id.to_owned().into_boxed_str())
    }
}
