//! gpui-kit's theme on Slopty's tokens.
//!
//! The widgets borrowed from gpui-kit (inputs, the composer's textarea, Markdown `TextView`)
//! colour themselves from gpui-kit's own global [`gpui_kit::component::Theme`]. Rather than
//! keep two palettes, [`sync`] switches that theme to the matching mode and then writes the
//! Slopty surface tokens over the colours those widgets read, so a text field, a code block
//! or a link looks like the chrome around it in both variants.

use gpui::{App, Div, InteractiveElement as _, Styled as _, div, px};
use gpui_kit::base::text::TextViewDefaults;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use slopty_theme::{Theme, Variant, alpha};

use crate::colors::{hsla, hsla_alpha};

/// How large an overlay grows on a desktop.
///
/// A phone gets whatever the margins leave. Two sizes, because a list of commands and a file
/// being edited want different room, and a third would be a size nobody could name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// A list typed at: the command palette, the pickers.
    List,
    /// A document edited: the settings file.
    Editor,
}

impl Overlay {
    /// Width and height ceilings, in points.
    #[must_use]
    pub const fn bounds(self) -> (f32, f32) {
        match self {
            Self::List => (560.0, 520.0),
            Self::Editor => (640.0, 720.0),
        }
    }
}

/// The backdrop an overlay sits on: the canvas dimmed, the dialog near the top so a phone's
/// keyboard — which rises from the bottom — covers fewer of its rows.
///
/// The caller adds the identity, the key handling and the dismiss on a click through.
#[must_use]
pub fn backdrop(theme: &Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_start()
        .justify_center()
        .pt(px(theme.spacing.xl * 2.0))
        .bg(hsla_alpha(theme.surfaces.canvas, alpha::SCRIM))
}

/// The shell every overlay wears: one radius, one hairline border, one elevation, the UI
/// font. `min_w_0` so an unwrapped title cannot hold the box wider than a phone.
///
/// The caller adds the identity, the accessibility role and label, and the children.
#[must_use]
pub fn dialog(theme: &Theme, size: Overlay) -> Div {
    let (w, h) = size.bounds();
    let s = &theme.surfaces;
    div()
        .w_full()
        .min_w_0()
        .max_w(px(w))
        .max_h(px(h))
        .mx(px(theme.spacing.md))
        .flex()
        .flex_col()
        .rounded(px(theme.radii.md))
        .border_1()
        .border_color(hsla(s.border))
        .bg(hsla(s.panel))
        .shadow_sm()
        .text_size(px(theme.typography.ui_size))
        .font_family(theme.typography.ui_family.clone())
        .text_color(hsla(s.text))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
}

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
    c.selection = hsla_alpha(s.accent, alpha::TINT);
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
    c.scrollbar_thumb = hsla_alpha(s.text_muted, alpha::PRESSED);
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

    /// The chrome under `dir`, without its test modules: every line of Rust up to the first
    /// `#[cfg(test)]`, with its file and one-based line number.
    fn chrome_lines(dir: &str) -> Vec<(String, usize, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, String)>) {
            let entries = std::fs::read_dir(dir).expect("the crate's sources are readable");
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("a source file reads");
                    let name = path.display().to_string();
                    for (ix, line) in text.lines().enumerate() {
                        if line.contains("#[cfg(test)]") {
                            break;
                        }
                        out.push((name.clone(), ix.saturating_add(1), line.to_owned()));
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits in the workspace")
            .join(dir);
        let mut out = Vec::new();
        walk(&root, &mut out);
        assert!(!out.is_empty(), "found no sources under {dir}");
        out
    }

    /// The ruling in `docs/decisions/ui.md`, as a check rather than a paragraph: chrome takes
    /// its transparencies from `alpha` and wears one elevation. A raw opacity or a second
    /// shadow is how a design system becomes a pile of one-offs, so neither compiles.
    #[test]
    fn chrome_paints_from_the_tokens() {
        // Every step is written `0.` or `1.0`, so a digit after the comma is a raw opacity.
        let raw_alpha = |line: &str| {
            line.contains("hsla_alpha(")
                && (line.contains(", 0.") || line.contains(", 1.0") || line.contains("{ 1.0 }"))
        };
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                if raw_alpha(&line) {
                    wrong.push(format!("{file}:{line_no}: a raw opacity, not an `alpha` step"));
                }
                for other in ["shadow_md()", "shadow_lg()", "shadow_xl()", "shadow_2xl()"] {
                    if line.contains(other) {
                        wrong.push(format!("{file}:{line_no}: a second elevation ({other})"));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The two overlay sizes differ in both directions, and a list is the smaller of them: a
    /// size that is nearly another size is a size nobody chose.
    #[test]
    fn the_overlay_sizes_are_two_and_they_differ() {
        let (lw, lh) = Overlay::List.bounds();
        let (ew, eh) = Overlay::Editor.bounds();
        assert!(lw < ew && lh < eh, "a list is the smaller overlay");
        assert!(ew - lw >= 40.0 && eh - lh >= 40.0, "far enough apart to tell apart");
    }
}
