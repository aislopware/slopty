// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! How many subagents are running, on the titlebar's explorer toggle: a count badge on its
//! corner. slopty: the "● 3" pill on the sidebar's chat rows is gone (Amp's prune): a subagent
//! left at work reads as its agent's own work there, and the row's hover card counts them.
#![allow(
    let_underscore_drop,
    rustdoc::private_intra_doc_links,
    clippy::let_underscore_must_use,
    clippy::let_underscore_untyped,
    clippy::too_long_first_doc_paragraph,
    reason = "shell lint migration: slice A"
)]

use gpui::prelude::*;
use gpui::{
    App, Bounds, Hsla, IntoElement, RenderOnce, SharedString, TextAlign, TextRun, Window, canvas,
    div, fill, point, px,
};

use crate::theme::Theme;

/// A capped label ends in "+", whose thin arms leave the eye a wider gap
/// than the same padding after a digit; the padding after it is trimmed by
/// this much so the two ends look balanced.
const PLUS_TRIM: f32 = 1.0;
/// Where the middle of Geist Mono's digits sits above the baseline, as a
/// fraction of the font size: (726 + -16) / 2 units of 1000 for the round
/// digits, 710 / 2 for the flat ones.
const DIGIT_MID: f32 = 0.355;
/// Height of the count badge on the titlebar button.
const BADGE: f32 = 13.0;
/// Past this the count would outgrow the badge; it reads "99+".
const COUNT_CAP: u32 = 99;

/// The count as drawn: exact up to [`COUNT_CAP`], then "99+".
pub fn count_label(count: u32) -> SharedString {
    if count > COUNT_CAP { format!("{COUNT_CAP}+").into() } else { count.to_string().into() }
}

const fn is_capped(count: u32) -> bool {
    count > COUNT_CAP
}

fn shape_count(
    window: &Window,
    count: u32,
    family: &SharedString,
    weight: gpui::FontWeight,
    color: Hsla,
    font_size: gpui::Pixels,
) -> gpui::ShapedLine {
    let label = count_label(count);
    let run = TextRun {
        len: label.len(),
        font: gpui::Font {
            family: family.clone(),
            // A count that ticks keeps its width (tabular figures).
            features: crate::typography::tabular(),
            fallbacks: None,
            weight,
            style: gpui::FontStyle::Normal,
        },
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(label, font_size, &[run], None)
}

/// Where to put a line of digits so their middle sits on the middle of
/// `bounds`: the digits' baseline, snapped to a device pixel (as gpui would on
/// painting), and the top of the line box that lands gpui's own centring of
/// ascent-plus-descent on that baseline. Returns `(line_top, baseline)`.
fn digits_baseline(
    bounds: Bounds<gpui::Pixels>,
    font_size: gpui::Pixels,
    line: &gpui::ShapedLine,
    scale: f32,
) -> (f32, f32) {
    let height = f32::from(bounds.size.height);
    let digit_mid = f32::from(font_size) * DIGIT_MID;
    let baseline =
        ((f32::from(bounds.origin.y) + height / 2.0 + digit_mid) * scale).round() / scale;
    let baseline_in_box =
        (height - f32::from(line.ascent) - f32::from(line.descent)) / 2.0 + f32::from(line.ascent);
    (baseline - baseline_in_box, baseline)
}

/// Marks a titlebar button while work runs behind it: a small count badge
/// in the activity tone sits on its corner, drawn over the button so it
/// keeps its size and position among the titlebar controls. It holds still;
/// the count is the signal.
pub fn mark_titlebar_button(
    button: gpui::Stateful<gpui::Div>,
    count: u32,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let tone = theme.busy;
    button.relative().child(div().absolute().top(px(-3.0)).right(px(-3.0)).child(CountBadge {
        count,
        tone,
        family: theme.font_mono.clone(),
    }))
}

/// The count badge on the titlebar button, painted rather than laid out so
/// the digits can be centred exactly: a laid-out text box lands on a rounded
/// device pixel, which read as the digit sitting left of centre.
#[derive(IntoElement)]
struct CountBadge {
    count: u32,
    tone: Hsla,
    family: SharedString,
}

/// Padding either side of the badge's digits.
const BADGE_PAD: f32 = 3.0;

impl RenderOnce for CountBadge {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let font_size = crate::typography::ui_rems(9.0).to_pixels(window.rem_size());
        let line = shape_count(
            window,
            self.count,
            &self.family,
            gpui::FontWeight::MEDIUM, // the chrome's two weights (opencode; item 23).
            gpui::white(),
            font_size,
        );
        let capped = is_capped(self.count);
        let text_width = f32::from(line.width);
        // Capped labels trim the padding after the "+" (see [`PLUS_TRIM`]).
        let right_pad = if capped { BADGE_PAD - PLUS_TRIM } else { BADGE_PAD };
        let width = (BADGE_PAD + text_width + right_pad).max(BADGE);
        let tone = self.tone;
        canvas(
            |_, _, _| (),
            move |bounds: Bounds<gpui::Pixels>, (), window, cx| {
                let scale = window.scale_factor();
                let snap = |value: f32| (value * scale).round() / scale;
                window.paint_quad(fill(bounds, tone).corner_radii(bounds.size.height / 2.0));
                let (line_top, _) = digits_baseline(bounds, font_size, &line, scale);
                // Single digits sit centred in the badge; a capped label sits
                // at the padding, its trimmed side toward the "+".
                let left = f32::from(bounds.origin.x);
                let x = if capped {
                    left + BADGE_PAD
                } else {
                    left + (f32::from(bounds.size.width) - text_width) / 2.0
                };
                let origin = point(px(snap(x)), px(line_top));
                let _ = line.paint(origin, bounds.size.height, TextAlign::Left, None, window, cx);
            },
        )
        .w(px(width))
        .h(px(BADGE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_label_caps_at_ninety_nine() {
        assert_eq!(count_label(1).as_ref(), "1");
        assert_eq!(count_label(99).as_ref(), "99");
        assert_eq!(count_label(100).as_ref(), "99+");
    }
}
