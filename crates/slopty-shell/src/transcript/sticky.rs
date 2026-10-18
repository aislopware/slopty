// slopty: Slopty's own file (not in zeron).
//! slopty: the prompt a long turn answers, held at the top while the turn scrolls, as Claude
//! desktop holds it: once the person's prompt has scrolled away under the titlebar, a one-line
//! card of it stays there, the turn read below. Hovered or focused it opens to five lines; a
//! click or Enter goes back to the prompt. It leaves as the prompt comes back into view and gives
//! way to the next prompt as that one arrives under it, and to find's card. It paints deferred, above the
//! transcript's top fade, which would otherwise fade it with the words it stays above.

use gpui::{AnyElement, Context, Pixels, SharedString, Window, div, prelude::*, px};

use super::{RowKind, Transcript, USER_LINE_HEIGHT};
use crate::motion;
use crate::popover;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The card's padding above and below its words.
const PAD_Y: f32 = 7.0;
/// How many lines a hovered or focused card opens to.
const OPEN_LINES: f32 = 5.0;
/// Room kept between the card and a prompt arriving under it.
const GAP: f32 = 8.0;

impl Transcript {
    /// Where the held prompt sits below the transcript's top: under the titlebar the primary
    /// transcript slides beneath, as find's card does.
    fn sticky_top(&self) -> f32 {
        if self.doc_override.is_some() {
            Theme::SPACE_SM
        } else {
            Theme::TITLEBAR_HEIGHT + Theme::SPACE_SM
        }
    }

    /// The row of the prompt to hold at the top, with the height its card takes at rest: the
    /// last prompt before the first row read under the card's line, while that row is not a
    /// prompt and no prompt arrives under the card. None before the list has laid out.
    pub(super) fn sticky_prompt(&self, peek: f32) -> Option<usize> {
        let is_prompt = |ix: usize| matches!(&self.rows[ix].kind, RowKind::User { text, .. } if !text.is_empty());
        let len = self.rows.len();
        if len == 0 {
            return None;
        }
        let line = f32::from(self.list.viewport_bounds().top()) + self.sticky_top();
        let bottom = |ix: usize| self.list.bounds_for_item(ix).map(|b| f32::from(b.bottom()));
        let mut read = self.list.logical_scroll_top().item_ix.min(len - 1);
        while bottom(read)? <= line {
            if read + 1 == len {
                return None;
            }
            read += 1;
        }
        if is_prompt(read) {
            return None;
        }
        let prompt = (0..read).rev().find(|&ix| is_prompt(ix))?;
        let under = line + peek + GAP;
        let arriving = (read + 1..len)
            .map_while(|ix| {
                let top = f32::from(self.list.bounds_for_item(ix)?.top());
                (top < under).then_some(ix)
            })
            .any(is_prompt);
        (!arriving).then_some(prompt)
    }

    /// The held prompt's card, when a prompt is held.
    pub(super) fn render_sticky_prompt(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let line_h: Pixels = crate::typography::ui_rems(USER_LINE_HEIGHT).to_pixels(window.rem_size());
        let line_h = f32::from(line_h);
        let peek = line_h + 2.0 * PAD_Y + 2.0;
        // Find's card holds the same place.
        if self.finding() {
            return None;
        }
        let ix = self.sticky_prompt(peek)?;
        let row = &self.rows[ix];
        let RowKind::User { text, .. } = &row.kind else {
            return None;
        };
        let theme = Theme::of(cx).clone();
        let key: SharedString = format!("sticky-prompt-{}", cx.entity_id()).into();
        let open = motion::hover_t(&key);
        let flat: SharedString = super::single_line(text).into();
        let words = if open > 0.0 {
            div().line_clamp(OPEN_LINES as usize).child(text.clone())
        } else {
            div().truncate().child(flat.clone())
        };
        let card = popover::popover_card_flush(&theme)
            .id(key.clone())
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Back to your prompt: {flat}")))
            .tab_index(0)
            .focus_visible({
                let accent = theme.accent;
                move |style| style.border_color(accent)
            })
            .hover_fade(key.clone())
            .cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.scroll_to_row(ix, cx)))
            .min_w_0()
            .max_w(px(self.content_width * 0.8))
            .max_h(px(peek + open * (OPEN_LINES - 1.0) * line_h))
            .child(
                div()
                    .bg(crate::theme::user_bubble_bg())
                    .px(px(14.0))
                    .py(px(PAD_Y))
                    .text_size(crate::typography::ui_rems(14.0))
                    .line_height(crate::typography::ui_rems(USER_LINE_HEIGHT))
                    .text_color(theme.text)
                    .child(words),
            );
        let card = popover::find_card_in(SharedString::from(format!("{key}-{}", row.id)), card);
        Some(
            div()
                .absolute()
                .top(px(self.sticky_top()))
                .left_0()
                .right_0()
                .pl(px(48.0))
                .pr(px(48.0 + self.right_reserve))
                .flex()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .max_w(px(self.content_width))
                        .min_w_0()
                        .flex()
                        .justify_end()
                        .child(gpui::deferred(card)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{Entity, ListOffset, Render};

    use super::*;
    use crate::state::AppState;
    use crate::transcript::Row;

    fn row(ix: usize, prompt: Option<&str>) -> Row {
        Row {
            id: format!("row-{ix}").into(),
            version: 0,
            turn_start: prompt.is_some(),
            kind: match prompt {
                Some(text) => RowKind::User {
                    text: text.to_owned().into(),
                    mentions: Arc::default(),
                    attachments: Arc::default(),
                    badges: Arc::default(),
                    pending: false,
                },
                None => RowKind::ErrorChip {
                    message: SharedString::from(format!("Line {ix} of the long answer")),
                },
            },
            entry_id: format!("entry-{ix}").into(),
            timestamp: None,
            copy_text: None,
            compact_fold: None,
        }
    }

    /// The prompt a long turn answers holds at the top once it has scrolled away; not while
    /// the prompt itself is read, nor before any prompt, nor as the next prompt arrives under
    /// it, nor while find's card holds the place.
    #[gpui::test]
    fn a_long_turns_prompt_holds_at_the_top(cx: &mut gpui::TestAppContext) {
        struct Fixture(Entity<Transcript>);
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().size_full().child(self.0.clone())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
        });
        let state = cx.new(|_| AppState::new());
        let transcript = cx.new(|cx| Transcript::new(state, cx));
        transcript.update(cx, |this, cx| {
            this.rows = (0..60)
                .map(|ix| match ix {
                    5 => row(ix, Some("Fix the flaky upload test")),
                    40 => row(ix, Some("Now the download one")),
                    _ => row(ix, None),
                })
                .collect();
            this.list.reset(60);
            this.pinned = false;
            this.rail_enabled = false;
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        let at = |item_ix: usize, offset: f32, cx: &mut gpui::VisualTestContext| {
            transcript.update(cx, |this, cx| {
                this.list.scroll_to(ListOffset { item_ix, offset_in_item: px(offset) });
                cx.notify();
            });
            cx.run_until_parked();
            transcript.read_with(cx, |this, _| this.sticky_prompt(40.0))
        };

        assert_eq!(at(20, 0.0, cx), Some(5), "the turn's prompt holds");
        let shown = cx.update(|window, cx| {
            transcript.update(cx, |this, cx| this.render_sticky_prompt(window, cx).is_some())
        });
        assert!(shown, "and its card is drawn");
        assert_eq!(at(2, 0.0, cx), None, "no prompt before");
        assert_eq!(at(40, 0.0, cx), None, "the next prompt is itself read");
        // The next prompt 10px under the card's line, the row above it read there.
        let line = Theme::TITLEBAR_HEIGHT + Theme::SPACE_SM;
        transcript.update(cx, |this, cx| {
            this.list.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
            this.list.scroll_by(px(-(line + 10.0)));
            cx.notify();
        });
        cx.run_until_parked();
        let held = transcript.read_with(cx, |this, _| this.sticky_prompt(40.0));
        assert_eq!(held, None, "the next prompt arrives under it");
        assert_eq!(at(30, 0.0, cx), Some(5));
        transcript.update(cx, |this, _| this.find.open = true);
        let shown = cx.update(|window, cx| {
            transcript.update(cx, |this, cx| this.render_sticky_prompt(window, cx).is_some())
        });
        assert!(!shown, "find's card holds the place");
    }
}
