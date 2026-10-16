//! slopty: TeX math drawn as RaTeX typesets it (`slopty-math`; zeron draws none).
//!
//! A formula on its own (`$$…$$`) is typeset to a self-contained SVG, its glyphs as paths, and
//! drawn as an image at the text's size and colour, centred like a display formula; its a11y
//! node is a math node labelled with the TeX source. One that does not parse shows its source,
//! the node's value "error" saying so. The streaming tail draws no display math (it gets no
//! media), so a half-written `$$\frac{a}` stays text until the reply moves past it.
//!
//! A formula in the line (`$…$`) is typeset in text style at its block's size (a heading's at the
//! heading's) and drawn where its TeX was, through gpui-fast's inline boxes
//! (`StyledText::with_inline_boxes`). The TeX stays the text, so selecting and copying give the
//! TeX, and a math node laid over the formula's box reads it. The line shows one box as wide as
//! the formula, its baseline on the line's, the line's height kept (`docs/decisions/ui.md`, "…
//! math sits on the line"). One that does not typeset stays its source, set like inline code.
//! The parser makes math only of a closed `$…$`, so the streaming tail typesets those too.
//!
//! Typeset formulas are kept per (source, size, colour), a bounded set: past it a new formula
//! shows its source rather than grow GPUI's image cache without end.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{AnyElement, SharedString, div, prelude::*, px};

use crate::theme::Theme;

/// The most formulas kept typeset.
const KEPT: usize = 512;

/// Room above and below a display formula.
const DISPLAY_PAD: f32 = 4.0;

/// slopty: the math face's em over the text's, as KaTeX sets it (`.katex { font-size: 1.21em }`):
/// Computer Modern's letters are smaller for their em than the prose face's, so math at the
/// text's own em reads a fifth smaller than the words around it.
pub(crate) const MATH_EM: f32 = 1.21;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    tex: String,
    size: u32,
    color: [u32; 4],
    display: bool,
}

/// A formula in the line, typeset: its SVG, its size in pixels, how far its baseline is from
/// its top, and its number among the kept formulas (its pictures are kept by it).
#[derive(Clone, Debug)]
pub(crate) struct InlineFormula {
    svg: Arc<[u8]>,
    pub width: f32,
    pub height: f32,
    pub baseline: f32,
    id: u64,
}

#[derive(Clone)]
enum Typeset {
    Ready {
        image: Arc<gpui::Image>,
        width: f32,
        height: f32,
    },
    Failed(SharedString),
}

thread_local! {
    static KEPT_FORMULAS: RefCell<HashMap<Key, Typeset>> = RefCell::new(HashMap::new());
    static KEPT_INLINE: RefCell<HashMap<Key, Option<InlineFormula>>> = RefCell::new(HashMap::new());
    static KEPT_PICTURES: RefCell<HashMap<(u64, u32), Arc<gpui::RenderImage>>> =
        RefCell::new(HashMap::new());
}

#[cfg(test)]
type Painted = (
    std::ops::Range<usize>,
    gpui::Bounds<gpui::Pixels>,
    gpui::Point<gpui::Pixels>,
    Option<gpui::Point<gpui::Pixels>>,
);

#[cfg(test)]
thread_local! {
    /// The formulas in the line painted: their TeX's range, their bounds, the baseline they sit
    /// on, and the baseline of the line's first glyph.
    pub(crate) static PAINTED: RefCell<Vec<Painted>> = const { RefCell::new(Vec::new()) };
}

/// The key of `tex` at `size` pixels in `color`.
fn key(tex: &str, size: f32, color: gpui::Hsla, display: bool) -> (Key, [f32; 4]) {
    let rgba = gpui::Rgba::from(color);
    let rgba = [rgba.r, rgba.g, rgba.b, rgba.a];
    let key = Key { tex: tex.to_owned(), size: size.to_bits(), color: rgba.map(f32::to_bits), display };
    (key, rgba)
}

/// `tex` typeset in the line at `size` pixels in `color`; `None` when it does not typeset, or
/// once the kept set is full.
pub(crate) fn inline_formula(tex: &str, size: f32, color: gpui::Hsla) -> Option<InlineFormula> {
    let (key, rgba) = key(tex.trim(), size, color, false);
    KEPT_INLINE.with(|kept| {
        let mut kept = kept.borrow_mut();
        if let Some(found) = kept.get(&key) {
            return found.clone();
        }
        if kept.len() >= KEPT {
            return None;
        }
        let options = slopty_math::Options { display: false, font_size: size * MATH_EM, color: rgba };
        let id = kept.len() as u64;
        let formula = slopty_math::render(&key.tex, &options)
            .ok()
            .filter(|formula| formula.width > 0.0 && formula.height > 0.0)
            .map(|formula| InlineFormula {
                svg: Arc::from(formula.svg.into_bytes()),
                width: formula.width,
                height: formula.height,
                baseline: formula.baseline,
                id,
            });
        kept.insert(key, formula.clone());
        formula
    })
}

/// `formula`'s picture at the window's `scale`, rasterised once per scale.
pub(crate) fn inline_picture(
    formula: &InlineFormula,
    scale: f32,
    cx: &gpui::App,
) -> Option<Arc<gpui::RenderImage>> {
    let at = (formula.id, scale.to_bits());
    KEPT_PICTURES.with(|kept| {
        if let Some(found) = kept.borrow().get(&at) {
            return Some(found.clone());
        }
        let picture = cx.svg_renderer().render_single_frame(&formula.svg, scale).ok()?;
        let mut kept = kept.borrow_mut();
        if kept.len() >= KEPT * 2 {
            kept.clear();
        }
        kept.insert(at, picture.clone());
        Some(picture)
    })
}

/// Where a formula in the line paints: its box's origin on the line's baseline at `baseline`.
pub(crate) fn inline_bounds(
    formula: &InlineFormula,
    baseline: gpui::Point<gpui::Pixels>,
) -> gpui::Bounds<gpui::Pixels> {
    gpui::Bounds::new(
        gpui::point(baseline.x, baseline.y - px(formula.baseline)),
        gpui::size(px(formula.width), px(formula.height)),
    )
}

/// `tex` typeset on its own at `size` pixels in `color`, or why it could not be; `None` once
/// the kept set is full.
fn typeset(tex: &str, size: f32, color: gpui::Hsla) -> Option<Typeset> {
    let (key, rgba) = key(tex, size, color, true);
    KEPT_FORMULAS.with(|kept| {
        let mut kept = kept.borrow_mut();
        if let Some(found) = kept.get(&key) {
            return Some(found.clone());
        }
        if kept.len() >= KEPT {
            return None;
        }
        let options = slopty_math::Options {
            display: true,
            font_size: size * MATH_EM,
            color: rgba,
        };
        let typeset = match slopty_math::render(tex, &options) {
            Ok(formula) if formula.width > 0.0 && formula.height > 0.0 => Typeset::Ready {
                image: Arc::new(gpui::Image::from_bytes(
                    gpui::ImageFormat::Svg,
                    formula.svg.into_bytes(),
                )),
                width: formula.width,
                height: formula.height,
            },
            Ok(_) => Typeset::Failed("An empty formula".into()),
            Err(error) => Typeset::Failed(error.to_string().into()),
        };
        kept.insert(key, typeset.clone());
        Some(typeset)
    })
}

/// A display formula: its typeset image, or its source when it cannot be typeset.
pub(crate) fn display_formula(
    tex: &str,
    id: SharedString,
    size: f32,
    theme: &Theme,
) -> AnyElement {
    let source = tex.trim();
    match typeset(source, size, theme.text) {
        Some(Typeset::Ready {
            image,
            width,
            height,
        }) => div()
            .id(id)
            .role(gpui::Role::Math)
            .aria_label(SharedString::from(source.to_owned()))
            .w_full()
            .py(px(DISPLAY_PAD))
            .flex()
            .justify_center()
            .overflow_x_scroll()
            .child(
                gpui::img(image)
                    .flex_none()
                    .w(px(width))
                    .h(px(height)),
            )
            .into_any_element(),
        typeset => {
            let failed = matches!(typeset, Some(Typeset::Failed(_)));
            div()
                .id(id)
                .role(gpui::Role::Math)
                .aria_label(SharedString::from(source.to_owned()))
                .when(failed, |el| el.aria_value("error"))
                .when_some(
                    match typeset {
                        Some(Typeset::Failed(why)) => Some(why),
                        _ => None,
                    },
                    |el, why| {
                        el.tooltip(crate::settings::widgets::text_tooltip(why))
                            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    },
                )
                .w_full()
                .py(px(DISPLAY_PAD))
                .font_family(theme.font_mono.clone())
                .text_size(crate::typography::ui_rems(super::render::CODE_TEXT_SIZE))
                .text_color(super::render::inline_code_text(theme))
                .child(SharedString::from(source.to_owned()))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::accesskit::Role;
    use gpui::{IntoElement, Render, Window};

    use super::super::parser::{InlineRun, InlineStyle, Math};
    use super::super::render::{
        MD_LINE_HEIGHT, MD_TEXT_SIZE, RenderOptions, flat_text_presented_element, flatten_runs,
        text_element,
    };
    use super::*;

    fn run(text: &str, math: Option<Math>) -> InlineRun {
        InlineRun { text: text.into(), style: InlineStyle { math, ..InlineStyle::default() } }
    }

    /// Two paragraphs, one with a formula in its line, one without.
    struct Lines(Rc<super::super::render::FlatText>, Rc<super::super::render::FlatText>);

    impl Render for Lines {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let opts = RenderOptions::settled("row".into());
            let line = |id: &'static str, flat: &super::super::render::FlatText| {
                div()
                    .id(id)
                    .role(Role::Group)
                    .aria_label(id)
                    .w(px(600.0))
                    .text_size(px(MD_TEXT_SIZE))
                    .line_height(px(MD_LINE_HEIGHT))
                    .child(flat_text_presented_element(flat, 0, &opts, &theme))
            };
            div().flex().flex_col().child(line("with", &self.0)).child(line("without", &self.1))
        }
    }

    /// A formula in the line typesets in place of its TeX: the line keeps its height, the
    /// formula's baseline sits on the line's, its node is labelled with the TeX, and the text
    /// (what selection copies) is still the TeX.
    #[gpui::test]
    fn a_formula_in_the_line_sits_on_its_baseline(cx: &mut gpui::TestAppContext) {
        let tex = r"\frac{a}{b}";
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        let theme = Theme::dark();
        let with = flatten_runs(
            &[run("so ", None), run(tex, Some(Math::Inline)), run(" is half", None)],
            &theme,
            false,
        );
        let without = flatten_runs(&[run("so a/b is half", None)], &theme, false);
        assert_eq!(with.math, [(3..3 + tex.len(), tex.into())], "the formula's TeX range");
        assert!(with.code_ranges.is_empty(), "a formula that typesets is not code");
        assert_eq!(with.text.as_ref(), format!("so {tex} is half"), "the text keeps the TeX");

        PAINTED.with(|painted| painted.borrow_mut().clear());
        let (with, without) = (Rc::new(with), Rc::new(without));
        let (_, cx) = cx.add_window_view(|_, _| Lines(with, without));
        cx.run_until_parked();
        let nodes = cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let update = window.a11y_tree().unwrap();
            let scale = f64::from(window.scale_factor());
            update
                .nodes
                .iter()
                .map(|(_, node)| {
                    let r = node.bounds().unwrap_or_default();
                    let label = node.label().unwrap_or_default().to_owned();
                    let rect = [r.x0, r.y0, r.x1, r.y1].map(|v| (v / scale) as f32);
                    (node.role(), label, rect[3] - rect[1], rect)
                })
                .collect::<Vec<_>>()
        });
        let height = |label: &str| {
            nodes.iter().find(|(role, l, ..)| *role == Role::Group && l == label).unwrap().2
        };
        let (boxed, plain) = (height("with"), height("without"));
        assert!((boxed - plain).abs() <= 1.0, "the line keeps its height: {boxed} vs {plain}");
        let math_node = nodes
            .iter()
            .find(|(role, label, ..)| *role == Role::Math && label == tex)
            .map(|(.., rect)| *rect)
            .expect("the formula's node reads its TeX");

        let formula = inline_formula(tex, MD_TEXT_SIZE, theme.text).unwrap();
        let painted = PAINTED.with(|painted| painted.borrow().clone());
        // Each frame paints it once; the last frame's is the one drawn.
        assert!(painted.iter().all(|p| p == &painted[0]), "the same each frame: {painted:?}");
        let Some((range, bounds, baseline, line)) = painted.last() else {
            panic!("the formula painted");
        };
        assert_eq!(*range, 3..3 + tex.len());
        assert_eq!(bounds.size, gpui::size(px(formula.width), px(formula.height)));
        let offset = f32::from(baseline.y - bounds.origin.y);
        assert!((offset - formula.baseline).abs() < 0.01, "its baseline {offset} from its top");
        let line = line.unwrap();
        // The node's bounds are the formula's box, not the paragraph's corner.
        let [x0, y0, x1, y1] = math_node;
        let (bx0, by0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (bx1, by1) = (bx0 + f32::from(bounds.size.width), by0 + f32::from(bounds.size.height));
        let inside = x0 >= bx0 - 0.5 && y0 >= by0 - 0.5 && x1 <= bx1 + 0.5 && y1 <= by1 + 0.5;
        assert!(inside && x1 - x0 > 1.0, "node {math_node:?} in the box {bounds:?}");
        assert!(x0 > 1.0, "not at the paragraph's left edge: {x0}");
        println!("MEASURE inline formula node {math_node:?}, box {bounds:?}");
        assert!(
            (f32::from(baseline.y - line.y)).abs() < 0.01,
            "on the line's baseline: {baseline:?} vs {line:?}"
        );
        println!(
            "MEASURE inline formula {tex}: {:.1}×{:.1} px, baseline {:.2} px below its top; \
             line {boxed:.1} px with it, {plain:.1} px without",
            formula.width, formula.height, formula.baseline
        );
    }

    /// A paragraph and a heading, each with a formula in its line, through the block's own text
    /// element at its own size.
    struct Sized;

    impl Render for Sized {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::of(cx).clone();
            let opts = RenderOptions::settled("sized".into());
            let runs = [run("so ", None), run(r"\frac{a}{b}", Some(Math::Inline)), run(" holds", None)];
            div()
                .w(px(600.0))
                .flex()
                .flex_col()
                .child(text_element(&runs, MD_TEXT_SIZE, MD_LINE_HEIGHT, false, 0, 0, &opts, &theme))
                .child(text_element(&runs, HEADING, 32.0, true, 1, 0, &opts, &theme))
        }
    }

    /// A heading's size, as the test sets one.
    const HEADING: f32 = 24.0;

    /// A heading's formula typesets at the heading's size: its box is the body's scaled by the
    /// sizes' ratio, within a pixel.
    #[gpui::test]
    fn a_headings_formula_is_set_at_its_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
        });
        PAINTED.with(|painted| painted.borrow_mut().clear());
        let (_, cx) = cx.add_window_view(|_, _| Sized);
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let painted = PAINTED.with(|painted| painted.borrow().clone());
        let mut heights: Vec<f32> =
            painted.iter().map(|(_, bounds, ..)| f32::from(bounds.size.height)).collect();
        heights.sort_by(f32::total_cmp);
        heights.dedup();
        let [body, heading] = heights[..] else { panic!("two sizes painted: {heights:?}") };
        let scaled = body * HEADING / MD_TEXT_SIZE;
        assert!((heading - scaled).abs() <= 1.0, "heading {heading} vs body {body} scaled {scaled}");
        println!(
            "MEASURE heading formula: {heading:.2} px at {HEADING} px, body {body:.2} px at \
             {MD_TEXT_SIZE} px, scaled {scaled:.2}"
        );
    }

    /// A fraction typesets taller than a line of text; a malformed formula says why.
    #[test]
    fn a_formula_typesets_or_says_why_not() {
        let Some(Typeset::Ready { height, width, .. }) =
            typeset(r"\frac{a}{b}", 14.0, gpui::black())
        else {
            panic!("a fraction typesets");
        };
        assert!(height > 22.0, "taller than a 22 px line: {height}");
        assert!(width > 0.0);
        assert!(matches!(typeset(r"\nosuchcommand{x}", 14.0, gpui::black()), Some(Typeset::Failed(_))));
    }

    /// slopty: a formula in 14 px prose is set at KaTeX's 1.21 em, so its letters match the
    /// words': an `x` in the line is as tall as the text's em says, not a fifth short.
    #[test]
    fn a_formula_is_set_at_katexs_em() {
        let plain = slopty_math::render("x", &slopty_math::Options {
            display: false,
            font_size: 14.0,
            color: [0.0, 0.0, 0.0, 1.0],
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let set = inline_formula("x", 14.0, gpui::black()).expect("x typesets");
        let ratio = set.height / plain.height;
        assert!((ratio - MATH_EM).abs() < 0.01, "set at {ratio} of the text's em");
        println!("MEASURE inline x: {:.2} px tall at 14 px prose (was {:.2})", set.height, plain.height);
    }
}
