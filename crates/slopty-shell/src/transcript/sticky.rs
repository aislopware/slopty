// slopty: Slopty's own file (not in zeron).
//! slopty: the prompt a long turn answers, held at the top while the turn scrolls. Once the
//! person's prompt has scrolled away, a band of it spans the transcript's top edge, flush under
//! the titlebar: one quiet line of their words in the reading column, an arrow up at its end, a
//! hairline under it. It is part of the page, not a card over it: it spans the whole width, so
//! what scrolls under it is cut by one straight edge as by the window's, never a card's border
//! cut through or one row's words covered beside the next; and it never grows over the rows under
//! it. A click or Enter goes back to the prompt. It comes as the prompt goes under where it
//! draws, leaves as the prompt comes back, gives way to the next prompt as that one arrives
//! under it, and to find's card.
//!
//! Only the person's own prompts are held, and only in the conversation's own transcript: a
//! subagent's run, in its card or opened, holds none, as its task is the agent's words and the
//! card's or the tab's header already names it. So one band at most is ever on screen. It paints
//! deferred, above the transcript's top fade, which would otherwise fade it with the words it
//! stays above.

use gpui::{AnimationExt as _, AnyElement, App, Context, SharedString, Window, div, prelude::*, px};

use super::{RowKind, Transcript};
use crate::motion;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The band's height, its hairline under it.
const BAND_H: f32 = 34.0;
/// Room kept between the band and a prompt arriving under it.
const GAP: f32 = 8.0;

impl Transcript {
    /// Where the band's top sits below the transcript's: under the titlebar the primary
    /// transcript slides beneath; at the top of a pane, which pads for the titlebar itself.
    fn sticky_top(&self) -> f32 {
        if self.doc_override.is_some() { 0.0 } else { Theme::TITLEBAR_HEIGHT }
    }

    /// The band's height with its hairline.
    pub(super) const fn sticky_height() -> f32 {
        BAND_H + 1.0
    }

    /// Whether this transcript holds the person's prompts: not a subagent's run, in its card or
    /// opened, whose first row is the task the agent wrote and whose header names it.
    fn holds_prompts(&self, cx: &App) -> bool {
        if self.fit.is_some() {
            return false;
        }
        let state = self.state.read(cx);
        let chat = self
            .chat_id
            .as_deref()
            .and_then(|chat_id| state.chats.iter().find(|chat| chat.id == chat_id))
            .or_else(|| state.selected_chat_row());
        !chat.is_some_and(|chat| chat.subagent)
    }

    /// The row of the prompt to hold at the top: the last prompt before the first row read
    /// under the band, while that row is not a prompt and no prompt arrives under the band. None
    /// before the list has laid out.
    pub(super) fn sticky_prompt(&self) -> Option<usize> {
        let is_prompt = |ix: usize| matches!(&self.rows[ix].kind, RowKind::User { text, .. } if !text.is_empty());
        let len = self.rows.len();
        if len == 0 {
            return None;
        }
        let line = f32::from(self.list.viewport_bounds().top()) + self.sticky_top() + Self::sticky_height();
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
        let under = line + GAP;
        let arriving = (read + 1..len)
            .map_while(|ix| {
                let top = f32::from(self.list.bounds_for_item(ix)?.top());
                (top < under).then_some(ix)
            })
            .any(is_prompt);
        (!arriving).then_some(prompt)
    }

    /// The prompt held, from the list's last layout: none in a subagent's run, nor while find's
    /// card holds the place.
    pub(super) fn held_prompt(&self, cx: &App) -> Option<usize> {
        if self.finding() || !self.holds_prompts(cx) { None } else { self.sticky_prompt() }
    }

    /// The held prompt's band, when a prompt is held, and what draws the transcript again when
    /// this frame's layout holds another: which prompt is held is read from a layout, and this
    /// frame's is laid out after the band was chosen.
    pub(super) fn render_sticky_prompt(&self, _window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let held = self.held_prompt(cx);
        let view = cx.weak_entity();
        let watch = gpui::canvas(
            move |_, _, cx| {
                let Some(transcript) = view.upgrade() else { return };
                if transcript.read(cx).held_prompt(cx) != held {
                    cx.defer(move |cx| transcript.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
        .into_any_element();
        let mut drawn = vec![watch];
        drawn.extend(held.and_then(|ix| self.render_held_prompt(ix, cx)));
        drawn
    }

    /// The band of the prompt at row `ix`.
    fn render_held_prompt(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = &self.rows[ix];
        let RowKind::User { text, .. } = &row.kind else {
            return None;
        };
        let theme = Theme::of(cx).clone();
        let key: SharedString = format!("sticky-prompt-{}", cx.entity_id()).into();
        let hover = motion::hover_t(&key);
        let flat: SharedString = super::single_line(text).into();
        // The window's own material the transcript lies on, the titlebar's above it: frosted
        // where it is glass, so the words under it blur rather than show through.
        let fill = if theme.is_frost() { theme.glass() } else { theme.surface };
        let column = div()
            .w_full()
            .max_w(px(self.content_width))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(motion::mix(theme.text_muted, theme.text, hover))
                    .child(flat.clone()),
            )
            .child(
                crate::icons::icon(crate::icons::ARROW_UP)
                    .flex_none()
                    .size(px(12.0))
                    .text_color(motion::mix(theme.text_faint, theme.text_muted, hover)),
            );
        let band = div()
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
            .w_full()
            .h(px(Self::sticky_height()))
            .bg(fill)
            .border_b_1()
            .border_color(theme.border)
            .pl(px(48.0))
            .pr(px(48.0))
            .flex()
            .justify_center()
            .child(column);
        let band = if cx.reduce_motion() {
            band.into_any_element()
        } else {
            band.with_animation(
                SharedString::from(format!("{key}-{}", row.id)),
                motion::FADE_QUICK.animation(),
                |band, t| band.opacity(t),
            )
            .into_any_element()
        };
        Some(
            div()
                .absolute()
                .top(px(self.sticky_top()))
                .left_0()
                .right(px(self.right_reserve))
                .child(gpui::deferred(crate::frost::frosted(0.0, crate::frost::MENU_BLUR, band)))
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

    /// The prompt a long turn answers holds at the top once it has scrolled under the band;
    /// not while the prompt itself is read, nor before any prompt, nor as the next prompt
    /// arrives under it, nor while find's card holds the place.
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
            transcript.read_with(cx, |this, cx| this.held_prompt(cx))
        };

        assert_eq!(at(20, 0.0, cx), Some(5), "the turn's prompt holds");
        let shown = cx.update(|window, cx| {
            transcript.update(cx, |this, cx| this.render_sticky_prompt(window, cx).len() > 1)
        });
        assert!(shown, "and its band is drawn");
        assert_eq!(at(2, 0.0, cx), None, "no prompt before");
        assert_eq!(at(40, 0.0, cx), Some(40), "the next prompt holds once under the band");
        // The next prompt at the band's foot: itself read, then 4px under it, arriving.
        let line = Theme::TITLEBAR_HEIGHT + Transcript::sticky_height();
        transcript.update(cx, |this, cx| {
            this.list.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
            this.list.scroll_by(px(-line));
            cx.notify();
        });
        cx.run_until_parked();
        let held = transcript.read_with(cx, |this, cx| this.held_prompt(cx));
        assert_eq!(held, None, "the next prompt is itself read");
        assert_eq!(at(40, 0.0, cx), Some(40));
        transcript.update(cx, |this, cx| {
            this.list.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
            this.list.scroll_by(px(-(line + 4.0)));
            cx.notify();
        });
        cx.run_until_parked();
        let held = transcript.read_with(cx, |this, cx| this.held_prompt(cx));
        assert_eq!(held, None, "the next prompt arrives under it");
        assert_eq!(at(30, 0.0, cx), Some(5));
        transcript.update(cx, |this, _| this.find.open = true);
        let shown = cx.update(|window, cx| {
            transcript.update(cx, |this, cx| this.render_sticky_prompt(window, cx).len() > 1)
        });
        assert!(!shown, "find's card holds the place");
    }
}
