//! gpui-kit's theme on Slopty's tokens.
//!
//! The widgets borrowed from gpui-kit (inputs, the composer's textarea, Markdown `TextView`)
//! colour themselves from gpui-kit's own global [`gpui_kit::component::Theme`]. Rather than
//! keep two palettes, [`sync`] switches that theme to the matching mode and then writes the
//! Slopty surface tokens over the colours those widgets read, so a text field, a code block
//! or a link looks like the chrome around it in both variants.

use gpui::App;
use gpui_kit::base::text::TextViewDefaults;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use slopty_theme::{Theme, Variant};

use crate::colors::{hsla, hsla_alpha};

/// Point gpui-kit's theme at `theme`: mode, then the colours its widgets read.
pub fn sync(theme: &Theme, cx: &mut App) {
    let mode = match theme.variant() {
        Variant::Dark => ThemeMode::Dark,
        Variant::Light => ThemeMode::Light,
    };
    KitTheme::change(mode, None, cx);
    let s = &theme.surfaces;
    let kit = KitTheme::global_mut(cx);
    let c = &mut kit.colors;
    c.background = hsla(s.panel);
    c.foreground = hsla(s.text);
    c.border = hsla(s.border);
    c.input = hsla(s.border);
    c.ring = hsla(s.accent);
    c.caret = hsla(s.accent);
    c.selection = hsla_alpha(s.accent, slopty_theme::alpha::TINT_STRONG);
    c.accent = hsla(s.raised);
    c.accent_foreground = hsla(s.text);
    c.muted = hsla(s.raised);
    c.muted_foreground = hsla(s.text_muted);
    c.secondary = hsla(s.raised);
    c.secondary_foreground = hsla(s.text);
    c.secondary_hover = hsla(s.overlay);
    c.secondary_active = hsla(s.overlay);
    c.primary = hsla(s.accent);
    c.primary_foreground = hsla(s.accent_fg);
    c.link = hsla(s.accent);
    c.link_hover = hsla(s.accent);
    c.link_active = hsla(s.accent);
    c.popover = hsla(s.panel);
    c.popover_foreground = hsla(s.text);
    c.list = hsla(s.panel);
    c.list_hover = hsla(s.raised);
    c.list_active = hsla(s.overlay);
    c.danger = hsla(s.error);
    c.success = hsla(s.success);
    c.warning = hsla(s.warn);
    c.scrollbar_thumb = hsla_alpha(s.text_muted, slopty_theme::alpha::TINT_PRESSED);
    KitTheme::sync_base(cx);
    // `sync_base` reinstalls the Markdown defaults, so the code colouring goes on after it.
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(crate::highlight::code_block(theme.clone()))
        .install(cx);
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    /// Both variants land in gpui-kit's theme: mode and the token colours.
    #[gpui::test]
    fn the_kit_theme_follows_the_tokens(cx: &TestAppContext) {
        cx.update(gpui_kit::init);
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            cx.update(|cx| sync(&theme, cx));
            cx.update(|cx| {
                let kit = KitTheme::global(cx);
                assert_eq!(kit.mode.is_dark(), variant == Variant::Dark);
                assert_eq!(kit.colors.background, hsla(theme.surfaces.panel));
                assert_eq!(kit.colors.foreground, hsla(theme.surfaces.text));
                assert_eq!(kit.colors.primary, hsla(theme.surfaces.accent));
                assert_eq!(kit.colors.primary_foreground, hsla(theme.surfaces.accent_fg));
                assert_eq!(kit.colors.border, hsla(theme.surfaces.border));
                assert_eq!(kit.colors.ring, hsla(theme.surfaces.accent));
                assert!(
                    TextViewDefaults::global(cx).has_code_block_highlighter(),
                    "fenced code is coloured after the sync"
                );
            });
        }
    }
}
