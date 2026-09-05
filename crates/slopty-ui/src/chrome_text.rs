//! Item chrome text: a label shaped once at its base size and painted at the item's zoom.
//!
//! GPUI's text element shapes at the painted size and measures itself through taffy, so a zoom
//! step re-shapes every title and pill on the canvas and lays each out again; and since every
//! step of a zoom is a new font size, the glyph atlas rasterises every glyph again too. This
//! element shapes the label at its base size once (a global cache swept per frame, like the
//! terminal's words), sizes itself by arithmetic (`shaped width × k`) instead of a measure
//! callback, paints each glyph at `base × k` through `Window::paint_glyph`, and while the
//! canvas says the zoom is in motion draws from a raster on the size ladder stretched to the
//! painted size (`Window::paint_glyph_scaled`, the fork), exact again on the frame the motion
//! settles. At `k = 1` and at rest it paints what GPUI's text element paints: the same
//! shaping, the same baseline, the same glyph origins.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use gpui::{
    App, BorrowAppContext as _, Bounds, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, Point, ShapedGlyph, ShapedLine,
    ShapedRun, SharedString, Style, TextRun, Window, point, px, relative,
};

use crate::fonts;

/// A label of the item chrome (title, pill, badge, heading), shaped at `base` and painted at
/// `base × k`.
#[derive(Debug)]
pub struct ChromeText {
    text: SharedString,
    base: Pixels,
    k: f32,
    zooming: bool,
    fill: bool,
}

impl ChromeText {
    /// `text` at `base × k` points, in the font and colour of the enclosing text style.
    #[must_use]
    pub fn new(text: impl Into<SharedString>, base: Pixels, k: f32) -> Self {
        Self { text: text.into(), base, k, zooming: false, fill: false }
    }

    /// The zoom is in motion: paint from a raster on the size ladder, stretched.
    #[must_use]
    pub const fn zooming(mut self, on: bool) -> Self {
        self.zooming = on;
        self
    }

    /// Take the parent's width and end the text with an ellipsis where it does not fit
    /// (the title in its bar); otherwise the element is as wide as its text (a pill's label).
    #[must_use]
    pub const fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl IntoElement for ChromeText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// A label shaped at its base size, with the ellipsis that would end it.
struct Shaped {
    line: ShapedLine,
    ellipsis: ShapedLine,
}

/// Shaped labels keyed by (text, font, base size), kept on the App across frames and items.
#[derive(Default)]
struct ChromeCache {
    lines: HashMap<u64, Rc<Shaped>>,
    touched: HashMap<u64, u64>,
    generation: u64,
    frame: Option<u64>,
}

impl gpui::Global for ChromeCache {}

impl ChromeCache {
    /// Drop entries not used in the last two generations (a generation is a frame when the
    /// frame probe counts them, else one label).
    fn sweep(&mut self, frame: Option<u64>) {
        if let Some(frame) = frame {
            if self.frame == Some(frame) {
                return;
            }
            self.frame = Some(frame);
        }
        self.generation = self.generation.wrapping_add(1);
        let keep_after = self.generation.saturating_sub(2);
        self.touched.retain(|_, g| *g >= keep_after);
        let touched = &self.touched;
        self.lines.retain(|k, _| touched.contains_key(k));
    }
}

/// What layout learned: the shaped label, the colour and the size to paint it at.
#[derive(Debug)]
pub struct Laid {
    shaped: Rc<Shaped>,
    color: Hsla,
    font_size: Pixels,
}

impl std::fmt::Debug for Shaped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shaped").field("width", &self.line.width).finish_non_exhaustive()
    }
}

/// Where the text is cut for the ellipsis: glyphs kept, and the shaped x the ellipsis starts at.
#[derive(Debug, Clone, Copy)]
pub struct Cut {
    keep: usize,
    at: Pixels,
}

/// The x positions of every glyph in shaping order, then the line's width: the boundaries a cut
/// can fall on.
fn boundaries(line: &ShapedLine) -> Vec<Pixels> {
    let layout = line.layout();
    let mut xs: Vec<Pixels> =
        layout.runs.iter().flat_map(|r| r.glyphs.iter().map(|g| g.position.x)).collect();
    xs.push(layout.width);
    xs
}

/// The cut that fits `text` and the ellipsis into `available`, or `None` when the whole text
/// fits. Positions are in shaped (base) units; `k` scales them to the painted size.
fn cut(shaped: &Shaped, k: f32, available: Pixels) -> Option<Cut> {
    if shaped.line.width * k <= available {
        return None;
    }
    let ellipsis = shaped.ellipsis.width * k;
    let xs = boundaries(&shaped.line);
    let keep = xs.iter().rposition(|x| *x * k + ellipsis <= available).unwrap_or(0);
    xs.get(keep).map(|at| Cut { keep, at: *at })
}

/// How the glyphs are painted: the size, the raster size they come from and the colour.
#[derive(Clone, Copy)]
struct Paint {
    /// The painted font size (`base × k`).
    size: Pixels,
    /// The size the raster comes from: `size`, or a rung of the ladder while zooming.
    raster: Pixels,
    color: Hsla,
}

/// Where a line goes: its left edge, its baseline and the scale from shaped to painted units.
#[derive(Clone, Copy)]
struct Place {
    x: Pixels,
    baseline: Pixels,
    k: f32,
}

/// Paint one glyph: exact at the painted size, or a stretched ladder raster while zooming.
fn paint_glyph(
    window: &mut Window,
    at: Point<Pixels>,
    run: &ShapedRun,
    glyph: &ShapedGlyph,
    paint: Paint,
) {
    let Paint { size, raster, color } = paint;
    let painted = if glyph.is_emoji {
        window.paint_emoji(at, run.font_id, glyph.id, size)
    } else if raster == size {
        window.paint_glyph(at, run.font_id, glyph.id, size, color)
    } else {
        window.paint_glyph_scaled(at, run.font_id, glyph.id, raster, size, color)
    };
    if let Err(e) = painted {
        tracing::debug!(error = %e, "paint chrome glyph");
    }
}

/// Where each of `layout`'s first `keep` glyphs lands at `place`. The x advances by the shaped
/// deltas times `k` and the y is the baseline plus the glyph's own vertical offset times `k`
/// (a combining mark, a vertically positioned glyph), as GPUI's own line paint places them, so
/// at `k = 1` every glyph lands where the text element would put it.
fn glyph_origins(
    layout: &LineLayout,
    place: Place,
    keep: Option<usize>,
) -> Vec<(&ShapedRun, &ShapedGlyph, Point<Pixels>)> {
    let mut x = place.x;
    let mut prev = px(0.0);
    let mut out = Vec::new();
    for run in &layout.runs {
        for glyph in &run.glyphs {
            if keep.is_some_and(|keep| out.len() >= keep) {
                return out;
            }
            x += (glyph.position.x - prev) * place.k;
            prev = glyph.position.x;
            out.push((run, glyph, point(x, place.baseline + glyph.position.y * place.k)));
        }
    }
    out
}

/// Paint `line`'s glyphs at `place`, stopping after `keep` glyphs.
fn paint_line(
    window: &mut Window,
    line: &ShapedLine,
    place: Place,
    keep: Option<usize>,
    paint: Paint,
) {
    for (run, glyph, at) in glyph_origins(line.layout(), place, keep) {
        paint_glyph(window, at, run, glyph, paint);
    }
}

impl Element for ChromeText {
    type PrepaintState = Option<Cut>;
    type RequestLayoutState = Laid;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn a11y_role(&self) -> Option<gpui::accesskit::Role> {
        Some(gpui::accesskit::Role::Label)
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        node.set_value(self.text.to_string());
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Laid) {
        let style = window.text_style();
        let font = style.font();
        let color = style.color;
        // The parent set its text size to `base × k`; the line height follows that, as it
        // does for GPUI's text element.
        let line_height = style.line_height_in_pixels(window.rem_size());
        let font_size = self.base * self.k;

        if !cx.has_global::<ChromeCache>() {
            cx.set_global(ChromeCache::default());
        }
        let mut hasher = std::hash::DefaultHasher::new();
        self.text.hash(&mut hasher);
        font.hash(&mut hasher);
        f32::from(self.base).to_bits().hash(&mut hasher);
        let key = hasher.finish();
        let text_system = std::sync::Arc::clone(window.text_system());
        let frame = crate::frames::index(cx);
        let (text, base) = (self.text.clone(), self.base);
        let shaped = cx.update_global::<ChromeCache, _>(|cache, _| {
            cache.sweep(frame);
            cache.touched.insert(key, cache.generation);
            let entry = cache.lines.entry(key).or_insert_with(|| {
                let run = |len: usize| TextRun {
                    len,
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = text_system.shape_line(text.clone(), base, &[run(text.len())], None);
                let dots = SharedString::from("…");
                let ellipsis = text_system.shape_line(dots.clone(), base, &[run(dots.len())], None);
                Rc::new(Shaped { line, ellipsis })
            });
            Rc::clone(entry)
        });

        // The element's own width is always its text's: an auto-sized parent (a pill) grows
        // to fit it. `fill` caps it at the parent's width, so a title in a bar of definite
        // width is cut to an ellipsis where it does not fit.
        let width: gpui::DefiniteLength = (shaped.line.width * self.k).into();
        let max_width: gpui::Length =
            if self.fill { relative(1.0).into() } else { gpui::Length::Auto };
        let layout_style = Style {
            size: gpui::Size { width: width.into(), height: line_height.into() },
            max_size: gpui::Size { width: max_width, height: gpui::Length::Auto },
            flex_shrink: 0.0,
            ..Style::default()
        };
        let id = window.request_layout(layout_style, None, cx);
        (id, Laid { shaped, color, font_size })
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Laid,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<Cut> {
        self.fill.then(|| cut(&request_layout.shaped, self.k, bounds.size.width)).flatten()
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Laid,
        prepaint: &mut Option<Cut>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let (k, size) = (self.k, request_layout.font_size);
        let raster = if self.zooming { fonts::raster_rung(size) } else { size };
        let paint = Paint { size, raster, color: request_layout.color };
        let layout = request_layout.shaped.line.layout();
        // GPUI centres the line in its line height: the same padding, the same baseline.
        let padding_top = (bounds.size.height - (layout.ascent + layout.descent) * k) / 2.0;
        let baseline = bounds.origin.y + padding_top + layout.ascent * k;
        let cut = *prepaint;
        let shaped = Rc::clone(&request_layout.shaped);
        window.paint_layer(bounds, |window| {
            let place = Place { x: bounds.origin.x, baseline, k };
            paint_line(window, &shaped.line, place, cut.map(|c| c.keep), paint);
            if let Some(cut) = cut {
                let place = Place { x: bounds.origin.x + cut.at * k, baseline, k };
                paint_line(window, &shaped.ellipsis, place, None, paint);
            }
        });
    }
}

/// How many labels the cache holds (tests: a label shaped once serves every item and zoom).
#[cfg(test)]
pub fn cached_labels(cx: &App) -> usize {
    cx.try_global::<ChromeCache>().map_or(0, |cache| cache.lines.len())
}

#[cfg(test)]
mod tests {
    use gpui::{FontId, GlyphId};

    use super::*;

    /// Two glyphs, the second raised by 2 shaped points (a combining mark's placement).
    fn layout() -> LineLayout {
        let glyph = |x: f32, y: f32, index: usize| ShapedGlyph {
            id: GlyphId(1),
            position: point(px(x), px(y)),
            index,
            is_emoji: false,
        };
        LineLayout {
            font_size: px(10.0),
            width: px(20.0),
            ascent: px(8.0),
            descent: px(2.0),
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs: vec![glyph(0.0, 0.0, 0), glyph(10.0, 2.0, 1)],
            }],
            len: 2,
        }
    }

    /// A glyph's vertical offset rides on the baseline, scaled with the zoom, at `k = 1` too.
    #[test]
    fn a_glyphs_vertical_offset_is_added_to_the_baseline() {
        let layout = layout();
        let at_rest = Place { x: px(100.0), baseline: px(50.0), k: 1.0 };
        let origins: Vec<Point<Pixels>> =
            glyph_origins(&layout, at_rest, None).into_iter().map(|(_, _, at)| at).collect();
        assert_eq!(origins, [point(px(100.0), px(50.0)), point(px(110.0), px(52.0))]);
        let zoomed = Place { x: px(100.0), baseline: px(50.0), k: 2.0 };
        let origins: Vec<Point<Pixels>> =
            glyph_origins(&layout, zoomed, None).into_iter().map(|(_, _, at)| at).collect();
        assert_eq!(origins, [point(px(100.0), px(50.0)), point(px(120.0), px(54.0))]);
        assert_eq!(glyph_origins(&layout, zoomed, Some(1)).len(), 1, "cut after one glyph");
    }
}
