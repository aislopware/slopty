//! The prompt outline: one short bar per prompt the person sent, stacked at the transcript's
//! right edge and centred on its height, as `MonoCode`'s is (`docs/decisions/ui.md`, "The
//! prompt outline stands at the transcript's right edge").
//!
//! The prompt in view is lit. The pointer on a bar lifts it and its neighbours, as a dock
//! magnifies, and shows the prompt's start beside it with the answer's. A press takes the
//! transcript to that prompt. The outline needs two prompts, and a tile at least
//! [`OUTLINE_FROM`] wide, so it never crowds the reading column.
//!
//! While there are more bars than the stack has room for, their gap closes first, down to a
//! point, and then a window of them slides to keep the prompt in view inside it.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, Context, ElementId, FollowMode, InteractiveElement as _, IntoElement as _,
    ListOffset, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px, relative,
};
use slopty_proto::thread::ItemBody;

use super::ThreadView;
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::thread::rows::Row;
use crate::kit;

/// The narrowest tile the outline shows in, in points: `MonoCode`'s 58 rem.
pub(super) const OUTLINE_FROM: f32 = 928.0;

/// The fewest prompts worth an outline.
const MIN_PROMPTS: usize = 2;

/// A bar's thickness, in points.
const BAR_HEIGHT: f32 = 2.0;

/// A bar's length at rest, and lifted under the pointer.
const BAR_WIDTH: f32 = 11.0;
const BAR_LIFTED: f32 = 24.0;

/// The room between two bars, at most and at least.
const GAP: f32 = 10.0;
const GAP_MIN: f32 = 1.0;

/// The tallest the stack grows, and its share of the transcript's height.
const STACK_MAX: f32 = 330.0;
const STACK_SHARE: f32 = 0.75;

/// How strongly a bar is inked at rest, and lit.
const IDLE: f32 = 0.15;
const LIT: f32 = 0.85;

/// How many bars each side of the one under the pointer lift with it.
const RIPPLE: usize = 2;

/// The preview's width, in points.
const PREVIEW_WIDTH: f32 = 288.0;

/// The bars on show: the first and past the last of the prompts, and the room between two.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Stack {
    pub start: usize,
    pub end: usize,
    pub gap: f32,
}

/// The bars for `count` prompts in a stack `budget` points tall, the one at `active` in view:
/// one bar per prompt while they fit, the gap closing first; past that a window that keeps
/// `active` inside and prefers the newest.
pub(super) fn stack(count: usize, active: Option<usize>, budget: f32) -> Stack {
    // A count of bars is small, and a stack's points are few hundred: both fit an f32 exactly.
    #[expect(clippy::cast_possible_truncation, reason = "a stack fits a few hundred bars")]
    #[expect(clippy::cast_sign_loss, reason = "the budget is never negative")]
    let fit = (((budget + GAP_MIN) / (BAR_HEIGHT + GAP_MIN)).floor().max(1.0)) as usize;
    let newest = count.saturating_sub(fit);
    let start = active.map_or(newest, |a| a.min(newest));
    let end = start.saturating_add(fit).min(count);
    let shown = end.saturating_sub(start);
    #[expect(clippy::cast_precision_loss, reason = "a stack fits a few hundred bars")]
    let gap = if shown > 1 {
        let (bars, gaps) = (shown as f32, shown.saturating_sub(1) as f32);
        (bars.mul_add(-BAR_HEIGHT, budget) / gaps).floor().clamp(GAP_MIN, GAP)
    } else {
        0.0
    };
    Stack { start, end, gap }
}

/// How far a bar lifts, from 0 to 1: all the way under the pointer, less with each bar away,
/// none past [`RIPPLE`].
pub(super) fn lift(index: usize, hovered: Option<usize>) -> f32 {
    let Some(hovered) = hovered else { return 0.0 };
    let distance = index.abs_diff(hovered);
    if distance > RIPPLE {
        return 0.0;
    }
    let (near, span) =
        (RIPPLE.saturating_add(1).saturating_sub(distance), RIPPLE.saturating_add(1));
    #[expect(clippy::cast_precision_loss, reason = "the ripple is a few bars")]
    let lifted = near as f32 / span as f32;
    lifted
}

impl ThreadView {
    /// The rows that are prompts the person sent, in order.
    pub(in crate::conversation::thread) fn prompt_rows(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| matches!(row, Row::User { .. }).then_some(ix))
            .collect()
    }

    /// Which of `prompts` is in view: the last at the transcript's end, else the topmost one
    /// inside the view, else the last above it, else the first.
    fn prompt_in_view(&self, prompts: &[usize]) -> Option<usize> {
        let last = prompts.len().checked_sub(1)?;
        if self.list.is_following_tail() || self.list.is_scrolled_to_end() == Some(true) {
            return Some(last);
        }
        let view = self.list.viewport_bounds();
        let inside = prompts.iter().position(|&row| {
            self.list
                .bounds_for_item(row)
                .is_some_and(|b| b.bottom() > view.top() && b.top() < view.bottom())
        });
        let top = self.list.logical_scroll_top().item_ix;
        inside.or_else(|| prompts.iter().rposition(|&row| row < top)).or(Some(0))
    }

    /// Take the transcript to the prompt at row `row`.
    fn go_to_prompt(&mut self, row: usize, cx: &mut Context<Self>) {
        self.list.set_follow_mode(FollowMode::Normal);
        self.list.scroll_to(ListOffset { item_ix: row, offset_in_item: px(0.0) });
        self.outline_hovered = None;
        cx.notify();
    }

    /// The prompt at row `row` and the start of its answer, each a line or two, for the
    /// preview: the words the person sent, then the agent's first words after them.
    fn prompt_preview(&self, row: usize, cx: &App) -> (String, Option<String>) {
        let words = match self.rows.get(row) {
            Some(Row::User { item }) => match self.item(row, item, cx).map(|i| &i.body) {
                Some(ItemBody::User(message)) => super::super::find::said(message),
                _ => String::new(),
            },
            _ => String::new(),
        };
        let reply =
            self.rows.iter().enumerate().skip(row.saturating_add(1)).find_map(|(ix, r)| match r {
                Row::User { .. } => Some(None),
                Row::Text { item } => match self.item(ix, item, cx).map(|i| &i.body) {
                    Some(ItemBody::Text(text)) => Some(Some(text.text.clone())),
                    _ => None,
                },
                _ => None,
            });
        (words, reply.flatten().filter(|r| !r.trim().is_empty()))
    }

    /// The outline, while the tile is wide enough and the thread holds two prompts or more.
    pub(super) fn outline(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.width < OUTLINE_FROM {
            return None;
        }
        let prompts = self.prompt_rows();
        if prompts.len() < MIN_PROMPTS {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let height = f32::from(self.list.viewport_bounds().size.height);
        let budget =
            if height > 0.0 { (height * STACK_SHARE).floor().min(STACK_MAX) } else { STACK_MAX };
        let active = self.prompt_in_view(&prompts);
        let stack = stack(prompts.len(), active, budget);
        let hovered = self.outline_hovered.and_then(|row| prompts.iter().position(|&p| p == row));
        let bars = (stack.start..stack.end).filter_map(|at| {
            let row = *prompts.get(at)?;
            let lit = hovered.map_or_else(|| active == Some(at), |h| h == at);
            let width = (BAR_LIFTED - BAR_WIDTH).mul_add(lift(at, hovered), BAR_WIDTH);
            let (words, reply) = self.prompt_preview(row, cx);
            let shown = if active == Some(at) { ", in view" } else { "" };
            let label =
                format!("Prompt {}{shown}: {}", at.saturating_add(1), kit::first_line(&words));
            let preview = (hovered == Some(at)).then(|| self.preview(words, reply));
            Some(
                div()
                    .id(ElementId::Name(format!("outline-{at}").into()))
                    .debug_selector(move || format!("outline-{at}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(label))
                    .relative()
                    .flex_none()
                    .w_full()
                    .h(px(BAR_HEIGHT + stack.gap))
                    .flex()
                    .items_center()
                    .justify_end()
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_none()
                            .h(px(BAR_HEIGHT))
                            .w(px(width))
                            .bg(hsla_alpha(s.text, if lit { LIT } else { IDLE })),
                    )
                    .children(preview)
                    .on_hover(cx.listener(move |this, over: &bool, _w, cx| {
                        let was = this.outline_hovered;
                        if *over {
                            this.outline_hovered = Some(row);
                        } else if was == Some(row) {
                            this.outline_hovered = None;
                        }
                        if was != this.outline_hovered {
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_prompt(row, cx))),
            )
        });
        Some(
            div()
                .id("thread-outline")
                .debug_selector(|| "thread-outline".to_owned())
                .role(Role::Toolbar)
                .aria_label("Prompts")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(theme.spacing.md))
                .w(px(BAR_LIFTED))
                .flex()
                .flex_col()
                .justify_center()
                .child(div().w_full().flex().flex_col().children(bars))
                .into_any_element(),
        )
    }

    /// The card beside a bar under the pointer: the prompt's start, then its answer's, muted,
    /// each at most two lines. It stands to the bar's left, centred on it.
    fn preview(&self, words: String, reply: Option<String>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let line =
            |text: String, tone| {
                div().w_full().line_clamp(2).text_ellipsis().text_color(hsla(tone)).child(
                    SharedString::from(text.split_whitespace().collect::<Vec<_>>().join(" ")),
                )
            };
        div()
            .absolute()
            .right(relative(1.0))
            .top_0()
            .h_0()
            .flex()
            .items_center()
            .pr(px(theme.spacing.sm))
            .child(
                kit::elevate(div(), theme)
                    .debug_selector(|| "thread-outline-preview".to_owned())
                    .w(px(PREVIEW_WIDTH))
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xs))
                    .p(px(theme.spacing.sm))
                    .rounded(px(theme.radii.lg))
                    .text_size(px(theme.typography.small()))
                    .child(line(words, s.text))
                    .children(reply.map(|r| line(r, s.text_muted))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Few prompts stand a full gap apart; many close the gap first, then slide a window that
    /// keeps the prompt in view and prefers the newest.
    #[test]
    fn the_stack_closes_its_gap_then_slides() {
        assert_eq!(stack(3, Some(0), 330.0), Stack { start: 0, end: 3, gap: GAP });
        let crowded = stack(60, None, 330.0);
        assert_eq!((crowded.start, crowded.end), (0, 60));
        assert!(crowded.gap < GAP && crowded.gap >= GAP_MIN, "{crowded:?}");
        let fit = 110;
        let slid = stack(200, None, 330.0);
        assert_eq!((slid.start, slid.end, slid.gap), (200 - fit, 200, GAP_MIN), "the newest");
        assert_eq!(stack(200, Some(5), 330.0).start, 5, "the prompt in view stays inside");
    }

    /// The bar under the pointer lifts all the way, its neighbours less, and none past the
    /// ripple.
    #[test]
    fn a_bar_lifts_with_its_neighbours() {
        assert!((lift(4, Some(4)) - 1.0).abs() < f32::EPSILON);
        assert!(lift(3, Some(4)) > lift(2, Some(4)) && lift(2, Some(4)) > 0.0);
        assert!(lift(1, Some(4)).abs() < f32::EPSILON);
        assert!(lift(4, None).abs() < f32::EPSILON);
    }
}
