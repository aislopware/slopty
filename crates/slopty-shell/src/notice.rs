// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Shared notice chip — the tinted failure card the composer strip and the
//! transcript converged on (slopty composer.tsx `Notice` / chat-view.tsx
//! `ErrorChip`).

use gpui::{AnyElement, App, Div, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};

use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The chip's header icon treatment, which also picks its metrics: the bare
/// triangle reads lighter and rem-scales with the composer's text; the tile
/// anchors the transcript's block chip at fixed metrics.
pub enum NoticeChipIcon {
    /// Bare 14px triangle in the label color; 12px inset/radius, rem-scaled
    /// 12px text on a 16px line height (the composer's inline notice).
    Plain,
    /// 20px accent-washed tile holding a 12px triangle; 10px inset/radius,
    /// fixed 12px text (the transcript's ErrorChip).
    Tile,
}

/// The failure notice both surfaces render (slopty composer.tsx `Notice`,
/// chat-view.tsx `ErrorChip`): a tinted rounded chip — `border
/// <accent>/[0.16]` over a `<accent>/[0.05]` wash, a subtle tinted wash,
/// never a bare stroke — with a header row (DangerTriangle + medium label, a
/// tiny copy button pinned to its top-right corner — failure payloads are
/// meant to be pasted, not screenshotted) and the message below. The message WRAPS instead of truncating: failure payloads carry
/// exit statuses and stderr, and a one-line ellipsis was exactly what made
/// aislopware/slopty#95 undiagnosable from the screenshot. `warning` picks the
/// amber palette (amber-400/amber-200) over the default red
/// (red-400/red-300). Callers chain their own chrome: the composer its
/// id/dismiss click, the transcript `w_full().overflow_hidden()`.
pub fn notice_chip(
    theme: &Theme,
    warning: bool,
    label: &'static str,
    message: impl Into<SharedString>,
    icon: NoticeChipIcon,
) -> Div {
    let (accent, muted) = if warning {
        (theme.warning, theme.warning_muted)
    } else {
        (theme.danger, theme.danger_muted)
    };
    let message = message.into();
    let copy_message = message.clone();
    let tile = matches!(icon, NoticeChipIcon::Tile);
    let frame = |inset: f32| {
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .rounded(px(inset))
            .border_1()
            .border_color(accent.opacity(0.16))
            .bg(accent.opacity(0.05))
            .px(px(inset))
            .py(px(8.0))
    };
    let chip = if tile {
        // Chrome follows the font-size setting.
        frame(10.0).text_size(crate::typography::ui_rems(12.0))
    } else {
        frame(12.0)
            .text_size(crate::typography::ui_rems(12.0))
            .line_height(px(16.0))
            .text_color(muted.opacity(0.9))
    };
    let header_icon: AnyElement = if tile {
        div()
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
            )
            .into_any_element()
    } else {
        crate::icons::icon(crate::icons::DANGER_TRIANGLE)
            .size(px(14.0))
            .text_color(muted.opacity(0.9))
            .into_any_element()
    };
    // The tile pins its own label/message colors; the plain icon inherits the
    // chip-wide muted accent.
    chip.child(
        div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .child(header_icon)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .when(tile, |label| label.text_color(muted.opacity(0.8)))
                    .child(label),
            )
            .child(div().flex_1())
            .child({
                // The copy swaps to a check for the house's span and says "Copied", keeping its
                // size: a 24 px target, its pad taken back.
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
                    .bg(crate::motion::hover_blend(&hover, gpui::transparent_black(), accent.opacity(0.12)))
                    .hover_fade(hover)
                    // A tab stop with the shell's ring.
                    .tab_index(0)
                    .map(|el| crate::surface_chrome::focus_ring(el, theme))
                    // The composer chip dismisses on click; copying must
                    // not take the notice with it.
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            copy_message.to_string(),
                        ));
                        crate::motion::copy_feedback(click_key.clone(), cx);
                    })
                    .tooltip(move |window, cx| {
                        let words = if crate::motion::copied(&tip_key, cx) { "Copied" } else { "Copy message" };
                        crate::settings::widgets::text_tooltip(words)(window, cx)
                    })
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .child(CopyGlyph { key: key.into(), color: muted.opacity(0.8) })
            }),
    )
    .child(
        div()
            .min_w_0()
            .w_full()
            .when(tile, |message| message.text_color(theme.text.opacity(0.8)))
            .child(message),
    )
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
