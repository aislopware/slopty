// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The notice chip: the tinted warning card the transcript draws.

use gpui::prelude::*;
use gpui::{App, Div, FontWeight, Hsla, SharedString, Window, div, px};

use crate::motion::HoverFade as _;
use crate::theme::Theme;

/// The warning notice the transcript draws: a tinted rounded chip (`border <accent>/[0.16]` over
/// a `<accent>/[0.05]` wash, in the amber palette, never a bare stroke) with a header row and the
/// message below.
///
/// The header holds the danger triangle on its 20 px accent-washed tile, the medium label and a
/// tiny copy button pinned to its top-right corner, since a notice's words are meant to be pasted,
/// not screenshotted. The message wraps instead of truncating: it carries exit statuses and
/// stderr, and a one-line ellipsis was what made aislopware/slopty#95 undiagnosable from its
/// screenshot. The caller chains its own chrome (`w_full().overflow_hidden()`).
pub fn notice_chip(theme: &Theme, label: &'static str, message: impl Into<SharedString>) -> Div {
    let (accent, muted) = (theme.warning, theme.warning_muted);
    let message = message.into();
    let copy_message = message.clone();
    let header_icon = div()
        .flex_none()
        .size(px(20.0))
        .rounded(px(6.0))
        .bg(accent.opacity(0.12))
        .flex()
        .items_center()
        .justify_center()
        .child(
            crate::icons::icon(crate::icons::DANGER_TRIANGLE)
                .size(px(12.0))
                .text_color(muted.opacity(0.8)),
        );
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(accent.opacity(0.16))
        .bg(accent.opacity(0.05))
        .px(px(10.0))
        .py(px(8.0))
        // Chrome follows the font-size setting.
        .text_size(crate::typography::ui_rems(12.0))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(header_icon)
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted.opacity(0.8))
                        .child(label),
                )
                .child(div().flex_1())
                .child({
                    // The copy swaps to a check for the house's span and says "Copied", keeping
                    // its size: a 24 px target, its pad taken back.
                    let key = copy_key(&copy_message);
                    let hover = SharedString::from(format!("{key}-hover"));
                    let tip_key = key.clone();
                    let click_key = key.clone();
                    div()
                        .id(SharedString::from(key.clone()))
                        .role(gpui::Role::Button)
                        .aria_label("Copy message")
                        .flex_none()
                        .size(px(24.0))
                        .m(px(-2.0))
                        .rounded(px(6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .bg(crate::motion::hover_blend(
                            &hover,
                            gpui::transparent_black(),
                            accent.opacity(0.12),
                        ))
                        .hover_fade(hover)
                        // A tab stop with the shell's ring.
                        .tab_index(0)
                        .map(|el| crate::surface_chrome::focus_ring(el, theme))
                        // Copying never takes the click on to what holds the notice.
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                copy_message.to_string(),
                            ));
                            crate::motion::copy_feedback(click_key.clone(), cx);
                        })
                        .tooltip(move |window, cx| {
                            let words = if crate::motion::copied(&tip_key, cx) {
                                "Copied"
                            } else {
                                "Copy message"
                            };
                            crate::settings::widgets::text_tooltip(words)(window, cx)
                        })
                        .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                        .child(CopyGlyph { key: key.into(), color: muted.opacity(0.8) })
                }),
        )
        .child(div().min_w_0().w_full().text_color(theme.text.opacity(0.8)).child(message))
}

/// The copy span's key for a notice's words.
fn copy_key(message: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    message.hash(&mut hasher);
    format!("notice-copy-{:x}", hasher.finish())
}

/// A copy control's glyph, the check while its span runs.
#[derive(IntoElement)]
struct CopyGlyph {
    key: SharedString,
    color: Hsla,
}

impl RenderOnce for CopyGlyph {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        crate::icons::icon(crate::motion::copy_icon(&self.key, cx))
            .size(px(12.0))
            .text_color(self.color)
    }
}
