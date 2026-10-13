//! Chrome text: a label shaped once at its size and painted from that shaping.
//!
//! GPUI's text element shapes its text and measures itself through taffy, so a label redrawn
//! every frame (a tile's title, a pill, a badge) is shaped and laid out again each time. This
//! element shapes the label once (a global cache swept per frame, like the terminal's words),
//! sizes itself by its shaped width instead of a measure callback, and paints each glyph
//! through `Window::paint_glyph`. It paints what GPUI's text element paints: the same shaping,
//! the same baseline, the same glyph origins.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use gpui::{
    App, BorrowAppContext as _, Bounds, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, Point, ShapedGlyph, ShapedLine,
    ShapedRun, SharedString, Style, TextRun, Window, point, px,
};

/// A label of the item chrome (title, pill, badge, heading), shaped and painted at `base`.
#[derive(Debug)]
pub struct ChromeText {
    text: SharedString,
    base: Pixels,
}

impl ChromeText {
    /// `text` at `base` points, in the font and colour of the enclosing text style.
    #[must_use]
    pub fn new(text: impl Into<SharedString>, base: Pixels) -> Self {
        Self { text: text.into(), base }
    }
}

impl IntoElement for ChromeText {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// A label shaped at its base size.
struct Shaped {
    line: ShapedLine,
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

/// How the glyphs are painted: the size and the colour.
#[derive(Clone, Copy)]
struct Paint {
    /// The painted font size.
    size: Pixels,
    color: Hsla,
}

/// Where a line goes: its left edge and its baseline.
#[derive(Clone, Copy)]
struct Place {
    x: Pixels,
    baseline: Pixels,
}

/// Paint one glyph at the painted size.
fn paint_glyph(
    window: &mut Window,
    at: Point<Pixels>,
    run: &ShapedRun,
    glyph: &ShapedGlyph,
    paint: Paint,
) {
    let Paint { size, color } = paint;
    let painted = if glyph.is_emoji {
        window.paint_emoji(at, run.font_id, glyph.id, size)
    } else {
        window.paint_glyph(at, run.font_id, glyph.id, size, color)
    };
    if let Err(e) = painted {
        tracing::debug!(error = %e, "paint chrome glyph");
    }
}

/// Where each of `layout`'s glyphs lands at `place`. The x advances by the
/// shaped deltas and the y is the baseline plus the glyph's own vertical offset (a combining
/// mark, a vertically positioned glyph), as GPUI's own line paint places them, so every glyph
/// lands where the text element would put it.
fn glyph_origins(
    layout: &LineLayout,
    place: Place,
) -> Vec<(&ShapedRun, &ShapedGlyph, Point<Pixels>)> {
    let mut x = place.x;
    let mut prev = px(0.0);
    let mut out = Vec::new();
    let glyphs = layout.runs.iter().flat_map(|run| run.glyphs.iter().map(move |g| (run, g)));
    for (run, glyph) in glyphs {
        x += glyph.position.x - prev;
        prev = glyph.position.x;
        out.push((run, glyph, point(x, place.baseline + glyph.position.y)));
    }
    out
}

/// Paint `line`'s glyphs at `place`.
fn paint_line(window: &mut Window, line: &ShapedLine, place: Place, paint: Paint) {
    for (run, glyph, at) in glyph_origins(line.layout(), place) {
        paint_glyph(window, at, run, glyph, paint);
    }
}

impl Element for ChromeText {
    type PrepaintState = ();
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
        // The parent set its text size to `base`; the line height follows that, as it does
        // for GPUI's text element.
        let line_height = style.line_height_in_pixels(window.rem_size());
        let font_size = self.base;

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
                Rc::new(Shaped { line })
            });
            Rc::clone(entry)
        });

        // The element's own width is always its text's: an auto-sized parent (a pill) grows
        // to fit it.
        let width: gpui::DefiniteLength = shaped.line.width.into();
        let layout_style = Style {
            size: gpui::Size { width: width.into(), height: line_height.into() },
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
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Laid,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Laid,
        _prepaint: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        let paint = Paint { size: request_layout.font_size, color: request_layout.color };
        let layout = request_layout.shaped.line.layout();
        // GPUI centres the line in its line height: the same padding, the same baseline.
        let padding_top = (bounds.size.height - (layout.ascent + layout.descent)) / 2.0;
        let baseline = bounds.origin.y + padding_top + layout.ascent;
        let shaped = Rc::clone(&request_layout.shaped);
        window.paint_layer(bounds, |window| {
            let place = Place { x: bounds.origin.x, baseline };
            paint_line(window, &shaped.line, place, paint);
        });
    }
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

    /// A glyph's vertical offset rides on the baseline.
    #[test]
    fn a_glyphs_vertical_offset_is_added_to_the_baseline() {
        let layout = layout();
        let at = Place { x: px(100.0), baseline: px(50.0) };
        let origins: Vec<Point<Pixels>> =
            glyph_origins(&layout, at).into_iter().map(|(_, _, at)| at).collect();
        assert_eq!(origins, [point(px(100.0), px(50.0)), point(px(110.0), px(52.0))]);
    }
}
