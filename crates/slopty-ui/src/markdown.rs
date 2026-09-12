//! One markdown look for every surface that draws it with gpui-kit's `TextView`.
//!
//! An assistant's turn in a conversation ([`crate::terminal::conversation`]) and a note on the
//! canvas ([`crate::note`]) read the same, because both take their sizes, colours and corners
//! from the theme through [`style`].

use gpui::{Styled as _, px};
use gpui_kit::component::text::TextViewStyle;
use slopty_theme::Theme;

use crate::colors::hsla;

/// Markdown on the theme's tokens.
///
/// Paragraphs one base unit apart, headings stepping down from the title size to the base,
/// code in the terminal mono at `small()` on the raised surface with `radii.xs` corners.
/// Colours come from the gpui-kit theme, which [`crate::kit::sync`] keeps on the same tokens.
///
/// `scale` multiplies every size, for a surface that is drawn at a zoom of its own (a canvas
/// note); the chrome passes `1.0`.
#[must_use]
pub fn style(theme: &Theme, mono: &str, scale: f32) -> TextViewStyle {
    let small = theme.typography.small() * scale;
    let code_block = gpui::StyleRefinement::default()
        .font_family(mono.to_owned())
        .text_size(px(small))
        .bg(hsla(theme.surfaces.raised))
        .rounded(px(theme.radii.xs))
        .px(px(theme.spacing.sm * scale))
        .py(px(theme.spacing.xs * scale));
    let inline_code = gpui::HighlightStyle {
        background_color: Some(hsla(theme.surfaces.raised)),
        color: Some(hsla(theme.surfaces.text)),
        ..gpui::HighlightStyle::default()
    };
    let (title, base) = (theme.typography.title(), theme.typography.ui_size);
    TextViewStyle {
        paragraph_gap: gpui::rems(theme.spacing.sm / base),
        heading_base_font_size: px(base * scale),
        heading_font_size: Some(std::sync::Arc::new(move |level: u8, _base| {
            px((title - f32::from(level.saturating_sub(1))).max(base) * scale)
        })),
        code_block,
        inline_code,
        ..TextViewStyle::default()
    }
}
