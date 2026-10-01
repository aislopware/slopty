//! A diff's lines drawn, for the thread view's edits and the review tile: each line on its
//! wash, its numbers in a gutter, its sign, then its text in its grammar's colours.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Div, Hsla, IntoElement as _, ParentElement as _, SharedString, Styled as _,
    StyledText, div, px,
};
use slopty_theme::{Rgb, Theme, alpha};

use super::diff::{Block, Kind, Line, Pair};
use crate::colors::{hsla, hsla_alpha};
use crate::highlight;

/// What every line is drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Ink<'a> {
    /// The theme.
    pub theme: &'a Theme,
    /// The chrome's zoom.
    pub zoom: f32,
    /// Digits the gutter holds: the widest line number's.
    pub digits: usize,
}

impl Ink<'_> {
    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// A diff's frame: the code face on the raised surface, the hairline round it.
    #[must_use]
    pub fn frame(&self) -> Div {
        let s = self.theme.surfaces;
        div()
            .w_full()
            .overflow_hidden()
            .rounded(self.z(self.theme.radii.md))
            .border_1()
            .border_color(hsla(s.border_subtle))
            .bg(hsla(s.panel))
            .font_family(self.mono())
            .text_size(self.z(self.theme.typography.small()))
    }

    /// The line's wash, its sign and the sign's tone.
    #[must_use]
    pub fn tone(&self, kind: Kind) -> (Option<Hsla>, &'static str, Rgb) {
        let s = self.theme.surfaces;
        match kind {
            Kind::Added => (Some(hsla_alpha(s.success_fill, alpha::FAINT)), "+", s.success),
            Kind::Removed => (Some(hsla_alpha(s.error_fill, alpha::FAINT)), "\u{2212}", s.error),
            Kind::Context => (None, " ", s.text_muted),
        }
    }

    /// A line number in the gutter, right-aligned in the gutter's width.
    #[must_use]
    pub fn number(&self, n: Option<u32>) -> Div {
        let s = self.theme.surfaces;
        let digits = f32::from(u8::try_from(self.digits.max(2)).unwrap_or(u8::MAX));
        // A tabular figure is about 0.62 of the size wide in the mono face.
        let width = self.theme.typography.small() * 0.62 * digits;
        crate::kit::tabular(div())
            .flex_none()
            .w(self.z(width))
            .flex()
            .justify_end()
            .text_color(hsla(s.text_muted))
            .children(n.map(|n| SharedString::from(n.to_string())))
    }

    /// A line's text in its grammar's colours (a context line in the secondary tone).
    #[must_use]
    pub fn text(&self, line: &Line) -> AnyElement {
        let s = self.theme.surfaces;
        let (text, spans) = super::diff::detab(&line.text, line.spans.as_deref());
        let text = if text.is_empty() { " ".to_owned() } else { text };
        let context = line.kind == Kind::Context;
        let ink = if context { s.text_secondary } else { s.text };
        let styled = match spans.filter(|sp| !sp.is_empty() && !context) {
            Some(spans) => {
                let font = gpui::font(self.mono());
                let runs = highlight::runs(text.len(), Some(&spans), &font, self.theme);
                StyledText::new(SharedString::from(text)).with_runs(runs).into_any_element()
            }
            None => SharedString::from(text).into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .whitespace_normal()
            .text_color(hsla(ink))
            .child(styled)
            .into_any_element()
    }

    /// One line in a column: both numbers, the sign, the text, on the line's wash.
    #[must_use]
    pub fn unified(&self, line: &Line) -> Div {
        let (wash, sign, sign_tone) = self.tone(line.kind);
        let spacing = self.theme.spacing;
        div()
            .w_full()
            .flex()
            .items_start()
            .gap(self.z(spacing.xs))
            .px(self.z(spacing.sm))
            .when_some(wash, gpui::Styled::bg)
            .child(self.number(line.old))
            .child(self.number(line.new))
            .child(div().flex_none().text_color(hsla(sign_tone)).child(sign))
            .child(self.text(line))
    }

    /// One row side by side: the old line on the left, the new on the right.
    #[must_use]
    pub fn split(&self, (old, new): Pair<'_>) -> Div {
        let spacing = self.theme.spacing;
        let side = |line: Option<&Line>, number: fn(&Line) -> Option<u32>| {
            let (wash, sign, sign_tone) =
                line.map_or((None, " ", self.theme.surfaces.text_muted), |l| self.tone(l.kind));
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_start()
                .gap(self.z(spacing.xs))
                .px(self.z(spacing.sm))
                .when_some(
                    wash.filter(|_| line.is_some_and(|l| l.kind != Kind::Context)),
                    gpui::Styled::bg,
                )
                .child(self.number(line.and_then(number)))
                .child(div().flex_none().text_color(hsla(sign_tone)).child(sign))
                .children(line.map(|l| self.text(l)))
        };
        div()
            .w_full()
            .flex()
            .child(side(old, |l| l.old))
            .child(div().flex_none().w(px(1.0)).bg(hsla(self.theme.surfaces.border_subtle)))
            .child(side(new, |l| l.new))
    }

    /// The divider before a hunk: where it starts in the new file.
    #[must_use]
    pub fn hunk_head(&self, block: &Block) -> Div {
        let s = self.theme.surfaces;
        div()
            .w_full()
            .px(self.z(self.theme.spacing.sm))
            .py(self.z(self.theme.spacing.xxs))
            .bg(hsla(s.canvas))
            .text_color(hsla(s.text_muted))
            .font_family(self.theme.typography.ui_family.clone())
            .text_size(self.z(self.theme.typography.meta()))
            .child(SharedString::from(format!("Line {}", block.new_start)))
    }
}

/// The digits the widest line number in `blocks` takes.
#[must_use]
pub fn digits(blocks: &[Block]) -> usize {
    let widest = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .flat_map(|l| [l.old, l.new])
        .flatten()
        .max()
        .unwrap_or(0);
    widest.checked_ilog10().map_or(1, |d| usize::try_from(d).unwrap_or(0).saturating_add(1))
}
