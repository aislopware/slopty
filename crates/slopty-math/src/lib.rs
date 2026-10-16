//! TeX math typeset to SVG, for a client to draw as an image: `$…$` in a line, `$$…$$` on its
//! own (`slopty_markdown::Math`).
//!
//! `RaTeX` parses and lays out the TeX as `KaTeX` would, and writes the `KaTeX` fonts' glyphs into
//! the SVG as paths, so it is self-contained: it needs no font installed and no JavaScript, and
//! draws the same on every client. GPUI-free, so the shell, the view model and a test read it
//! alike; the shell rasterises the SVG at the size it paints ([`Formula::width`],
//! [`Formula::height`]) and sets it on the line by its [`Formula::baseline`].
//!
//! A formula that does not parse, or one too large to lay out, is a [`TypesetError`] with
//! `RaTeX`'s words for it: the transcript then shows its source.

#![forbid(unsafe_code)]

use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgColorSyntax, SvgOptions, render_to_svg_with_color_syntax};
use ratex_types::color::Color;
use ratex_types::math_style::MathStyle;

/// The longest TeX typeset: a longer one is shown as its source.
pub const TEX_MAX: usize = 8 * 1024;

/// How a formula is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// On its own (`$$…$$`, display style) rather than in the line (`$…$`, text style).
    pub display: bool,
    /// Pixels per em: the size of the text around it.
    pub font_size: f32,
    /// Its colour, red, green, blue and alpha from 0 to 1: the text's.
    pub color: [f32; 4],
}

/// A formula, typeset.
#[derive(Clone, Debug, PartialEq)]
pub struct Formula {
    /// A whole SVG document, its glyphs as paths.
    pub svg: String,
    /// Its width in pixels.
    pub width: f32,
    /// Its height in pixels, from its top to the bottom of its lowest descent.
    pub height: f32,
    /// How far its baseline is from its top, in pixels: where the line's text sits.
    pub baseline: f32,
}

/// Why a formula was not typeset.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TypesetError {
    /// The TeX is longer than [`TEX_MAX`].
    #[error("the formula is longer than {TEX_MAX} bytes")]
    TooLong,
    /// The TeX does not parse.
    #[error("{0}")]
    Parse(String),
    /// It lays out larger than `RaTeX` allows.
    #[error("{0}")]
    TooLarge(String),
}

/// `tex` typeset as `options` say.
///
/// # Errors
///
/// When it is too long, does not parse, or lays out too large.
pub fn render(tex: &str, options: &Options) -> Result<Formula, TypesetError> {
    if tex.len() > TEX_MAX {
        return Err(TypesetError::TooLong);
    }
    let nodes = ratex_parser::parser::parse(tex).map_err(|e| TypesetError::Parse(e.message))?;
    let style = if options.display { MathStyle::Display } else { MathStyle::Text };
    let [r, g, b, a] = options.color;
    let laid = layout(
        &nodes,
        &LayoutOptions::default().with_style(style).with_color(Color::new(r, g, b, a)),
    );
    validate_layout(&laid).map_err(TypesetError::TooLarge)?;
    let list = to_display_list(&laid);
    let em = f64::from(options.font_size);
    let svg = SvgOptions {
        font_size: em,
        padding: 0.0,
        stroke_width: em / 40.0 * 1.5,
        embed_glyphs: true,
        font_dir: String::new(),
    };
    let svg = render_to_svg_with_color_syntax(&list, &svg, SvgColorSyntax::Rgba);
    #[expect(clippy::cast_possible_truncation, reason = "a formula's size in pixels fits an f32")]
    let px = |ems: f64| (ems * em) as f32;
    Ok(Formula {
        svg,
        width: px(list.width),
        height: px(list.height + list.depth),
        baseline: px(list.height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(tex: &str, display: bool) -> Result<Formula, TypesetError> {
        render(tex, &Options { display, font_size: 14.0, color: [0.1, 0.2, 0.3, 1.0] })
    }

    /// A fraction set on its own stands taller than a letter in the line, its glyphs are paths
    /// in its own colour, and its baseline lies within it.
    #[test]
    fn a_fraction_is_taller_than_a_letter_and_self_contained() {
        let frac = set(r"\frac{a}{b}", true).unwrap();
        let letter = set("a", false).unwrap();
        assert!(frac.height > letter.height * 1.5, "{} against {}", frac.height, letter.height);
        assert!(frac.width > 0.0 && letter.width > 0.0);
        assert!(frac.baseline > 0.0 && frac.baseline < frac.height);
        assert!(
            frac.svg.starts_with("<svg") || frac.svg.starts_with("<?xml"),
            "{}",
            frac.svg.get(..40).unwrap_or_default()
        );
        assert!(frac.svg.contains("<path"), "glyphs as paths");
        assert!(!frac.svg.contains("<text"), "no font needed");
    }

    /// A formula that does not parse says why, and one past the cap is not tried.
    #[test]
    fn a_malformed_formula_says_why() {
        assert!(matches!(set(r"\frac{a", true), Err(TypesetError::Parse(_))));
        assert!(matches!(set(r"\nosuchcommand{x}", false), Err(TypesetError::Parse(_))));
        assert_eq!(set(&"x".repeat(TEX_MAX + 1), false), Err(TypesetError::TooLong));
    }

    /// The same formula is the same SVG every time, so a client can cache it by its source.
    #[test]
    fn typesetting_is_deterministic() {
        let tex = r"\int_0^1 x^2\,dx = \tfrac{1}{3}";
        assert_eq!(set(tex, true).unwrap(), set(tex, true).unwrap());
    }
}
