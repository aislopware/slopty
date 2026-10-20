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

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};

use super::{RowKind, Transcript};
use crate::motion;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The band's height, its hairline under it.
const BAND_H: f32 = 34.0;
/// Room kept between the band and a prompt arriving under it.
const GAP: f32 = 8.0;
/// The group band's height, its hairline under it.
const GROUP_BAND_H: f32 = 28.0;

/// The first of `len` rows whose bottom (the next row's top, by `top`) is under `line`, by
/// halves: the rows' bottoms only grow down the list. `len` when none is.
fn first_under(len: usize, top: &impl Fn(usize) -> f32, line: f32) -> usize {
    let (mut read, mut past) = (0, len);
    while read < past {
        let mid = read + (past - read) / 2;
        if top(mid + 1) <= line { read = mid + 1 } else { past = mid }
    }
    read
}

impl Transcript {
    /// Where the band's top sits below the transcript's: under the titlebar the primary
    /// transcript slides beneath, or on a phone under its navigation bar, which stands taller
    /// (its title over the chat's place); at the top of a pane, which pads for the titlebar
    /// itself.
    pub(super) fn sticky_top(&self) -> f32 {
        if self.doc_override.is_some() {
            0.0
        } else if self.on_phone() {
            // slopty: the phone's bar (`shell/compact.rs`), whose words both end above it (#11).
            Theme::TITLEBAR_HEIGHT + Theme::TITLEBAR_TOP_PAD
        } else {
            Theme::TITLEBAR_HEIGHT
        }
    }

    /// The band's height with its hairline.
    pub(super) const fn sticky_height() -> f32 {
        BAND_H + 1.0
    }

    /// Whether this transcript holds the person's prompts: not a subagent's run, in its card or
    /// opened, whose first row is the task the agent wrote and whose header names it; and not a
    /// chat not yet listed, which may be one (read once a sync, `Transcript::holds_prompts`).
    fn holds_prompts(&self) -> bool {
        self.fit.is_none() && self.holds_prompts
    }

    /// The row of the prompt to hold at the top: the last prompt before the first row read
    /// under the band, while that row is not a prompt and no prompt arrives under the band. None
    /// before the list has laid out, and while every row fits.
    ///
    /// Rows are placed from the list's own sums of their heights and its scroll, which say where
    /// they stand in either of its scroll forms: a list held at its end keeps no top row, and
    /// asked for a row's bounds there it has none to give (#18).
    pub(super) fn sticky_prompt(&self) -> Option<usize> {
        let is_prompt = |ix: usize| matches!(&self.rows[ix].kind, RowKind::User { text, .. } if !text.is_empty());
        let (len, top) = self.row_tops()?;
        let line = f32::from(self.list.viewport_bounds().top()) + self.sticky_top() + Self::sticky_height();
        let read = first_under(len, &top, line);
        if read == len || is_prompt(read) {
            return None;
        }
        let prompt = (0..read).rev().find(|&ix| is_prompt(ix))?;
        let under = line + GAP;
        let arriving = (read + 1..len).take_while(|&ix| top(ix) < under).any(is_prompt);
        (!arriving).then_some(prompt)
    }

    /// How many rows are laid out, and where each one's top stands on screen (and so the row
    /// before's bottom): the viewport's top, less what has scrolled above it, plus the list's
    /// sum of the rows' heights before it. None before the list has laid out, and while every
    /// row fits.
    fn row_tops(&self) -> Option<(usize, impl Fn(usize) -> f32 + '_)> {
        let len = self.rows.len().min(self.list.item_count());
        let viewport = self.list.viewport_bounds();
        let content = f32::from(self.list.offset_for_item(len));
        if len == 0 || content <= f32::from(viewport.size.height) {
            return None;
        }
        let origin = f32::from(viewport.top() + self.list.scroll_px_offset_for_scrollbar().y);
        Some((len, move |ix: usize| origin + f32::from(self.list.offset_for_item(ix))))
    }

    // slopty: an open tool group's header stays in view while its rows scroll (opencode's sticky
    // group trigger, `.research/opencode-study-2026-10-19.md` item 3).
    /// The open tool group to hold under the bands above it, by its header's row, and where its
    /// band's top sits below the transcript's: a group whose header has scrolled under that place
    /// while its rows still reach a band's height past it. One at most.
    pub(super) fn sticky_group(&self) -> Option<(usize, f32)> {
        if self.finding() {
            return None;
        }
        let band_top = self.sticky_top() + if self.held_prompt().is_some() { Self::sticky_height() } else { 0.0 };
        let (len, top) = self.row_tops()?;
        let line = f32::from(self.list.viewport_bounds().top()) + band_top;
        let read = first_under(len, &top, line);
        if read == len {
            return None;
        }
        let header = match &self.rows[read].compact_fold {
            Some(group) => self.rows[..read].iter().rposition(|row| &row.id == group)?,
            None => read,
        };
        if !self.group_open(header) || top(header) >= line {
            return None;
        }
        let id = &self.rows[header].id;
        let end = header + 1 + self.rows[header + 1..].iter().take_while(|row| row.compact_fold.as_ref() == Some(id)).count();
        (top(end.min(len)) > line + Self::group_band_height()).then_some((header, band_top))
    }

    /// The group band's height with its hairline: opencode's 28 pt trigger.
    pub(super) const fn group_band_height() -> f32 {
        GROUP_BAND_H + 1.0
    }

    /// Whether row `ix` is a tool group shown open behind a header that folds it: its own rows,
    /// or the rows of the work it heads.
    fn group_open(&self, ix: usize) -> bool {
        let row = &self.rows[ix];
        let RowKind::ToolGroup { tools, auto_open, compact_shell, .. } = &row.kind else {
            return false;
        };
        let (collapses, solo) = super::group_shape(tools, *compact_shell);
        collapses && !solo && self.folds.get(&row.id).and_then(|fold| fold.open).unwrap_or(*auto_open)
    }

    /// Fold the group headed at row `ix` and glide back to it.
    fn fold_held_group(&mut self, ix: usize, cx: &mut Context<Self>) {
        let row = &self.rows[ix];
        let RowKind::ToolGroup { auto_open, .. } = &row.kind else {
            return;
        };
        let (id, auto_open) = (row.id.clone(), *auto_open);
        let height = self.tool_group_reveals.get(&id).map_or(0.0, |reveal| reveal.rendered_height);
        self.toggle_fold(id, height, auto_open, cx);
        self.scroll_to_row(ix, cx);
        cx.notify();
    }

    /// The prompt held, from the list's last layout: none in a subagent's run, nor while find's
    /// card holds the place.
    pub(super) fn held_prompt(&self) -> Option<usize> {
        if self.finding() || !self.holds_prompts() { None } else { self.sticky_prompt() }
    }

    /// The held prompt's band, when a prompt is held, and what draws the transcript again when
    /// this frame's layout holds another: which prompt is held is read from a layout, and this
    /// frame's is laid out after the band was chosen.
    pub(super) fn render_sticky_prompt(&self, _window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let held = self.held_prompt();
        let group = self.sticky_group();
        let view = cx.weak_entity();
        let watch = gpui::canvas(
            move |_, _, cx| {
                let Some(transcript) = view.upgrade() else { return };
                let now = transcript.read(cx);
                if now.held_prompt() != held || now.sticky_group() != group {
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
        drawn.extend(group.and_then(|(ix, top)| self.render_held_group(ix, top, cx)));
        drawn
    }

    /// The band of the open group headed at row `ix`, `top` below the transcript's top: its
    /// chevron turned open and its summary, on the page with a hairline under it, as the
    /// prompt's band; a press folds the group and glides back to it.
    fn render_held_group(&self, ix: usize, top: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let RowKind::ToolGroup { summary, .. } = &self.rows[ix].kind else {
            return None;
        };
        let theme = Theme::of(cx).clone();
        let key: SharedString = format!("sticky-group-{}", cx.entity_id()).into();
        let hover = motion::hover_t(&key);
        let fill = if theme.is_frost() { theme.glass() } else { theme.surface };
        let column = div()
            .w_full()
            .max_w(px(self.content_width))
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                div()
                    .w(px(22.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                            .size(px(14.0))
                            .text_color(motion::mix(theme.text_faint, theme.text_muted, hover)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(super::TOOL_LABEL_SIZE))
                    .text_color(motion::mix(theme.text_muted, theme.text, hover))
                    .child(summary.text.clone()),
            );
        let band = div()
            .id(key.clone())
            .debug_selector(|| "sticky-group".into()) // slopty: for the tests
            .role(gpui::Role::Button)
            .aria_label(summary.text.clone())
            .aria_expanded(true)
            .tab_index(0)
            .focus_visible({
                let accent = theme.accent;
                move |style| style.border_color(accent)
            })
            .hover_fade(key.clone())
            .cursor_pointer()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| this.fold_held_group(ix, cx)))
            .w_full()
            .h(px(Self::group_band_height()))
            .bg(fill)
            .border_b_1()
            .border_color(theme.border)
            .pl(px(48.0))
            .pr(px(48.0))
            .flex()
            .justify_center()
            .child(column);
        Some(
            div()
                .absolute()
                .top(px(top))
                .left_0()
                .right(px(self.right_reserve))
                .child(gpui::deferred(crate::frost::frosted(0.0, crate::frost::MENU_BLUR, band)))
                .into_any_element(),
        )
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
        // slopty: it comes and goes as the page's own header does, with no fade either way, so
        // one prompt's band never flashes out as the next takes its place (#19).
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
            this.holds_prompts = true; // the person's own chat, listed
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        let at = |item_ix: usize, offset: f32, cx: &mut gpui::VisualTestContext| {
            transcript.update(cx, |this, cx| {
                this.list.scroll_to(ListOffset { item_ix, offset_in_item: px(offset) });
                cx.notify();
            });
            cx.run_until_parked();
            transcript.read_with(cx, |this, _| this.held_prompt())
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
        let held = transcript.read_with(cx, |this, _| this.held_prompt());
        assert_eq!(held, None, "the next prompt is itself read");
        assert_eq!(at(40, 0.0, cx), Some(40));
        transcript.update(cx, |this, cx| {
            this.list.scroll_to(ListOffset { item_ix: 40, offset_in_item: px(0.0) });
            this.list.scroll_by(px(-(line + 4.0)));
            cx.notify();
        });
        cx.run_until_parked();
        let held = transcript.read_with(cx, |this, _| this.held_prompt());
        assert_eq!(held, None, "the next prompt arrives under it");
        assert_eq!(at(30, 0.0, cx), Some(5));
        transcript.update(cx, |this, _| this.find.open = true);
        let shown = cx.update(|window, cx| {
            transcript.update(cx, |this, cx| this.render_sticky_prompt(window, cx).len() > 1)
        });
        assert!(!shown, "find's card holds the place");
    }

    /// slopty: an open group's header holds under the prompt's band while its rows scroll, not
    /// before its header has scrolled under, nor once folded; a press on it folds the group.
    #[gpui::test]
    fn an_open_groups_header_holds_while_its_rows_scroll(cx: &mut gpui::TestAppContext) {
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
        let group: SharedString = "row-10".into();
        transcript.update(cx, |this, cx| {
            this.rows = (0..60)
                .map(|ix| match ix {
                    5 => row(ix, Some("Fix the flaky upload test")),
                    10 => Row {
                        kind: RowKind::ToolGroup {
                            summary: "Ran 30 commands".into(),
                            tools: Arc::default(),
                            auto_open: false,
                            worked_secs: None,
                            compact_shell: true,
                            done: None,
                        },
                        ..row(ix, None)
                    },
                    11..=45 => Row { compact_fold: Some(group.clone()), ..row(ix, None) },
                    _ => row(ix, None),
                })
                .collect();
            this.folds.insert(group.clone(), super::super::FoldState { open: Some(true), ..Default::default() });
            this.list.reset(60);
            this.pinned = false;
            this.rail_enabled = false;
            this.holds_prompts = true;
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        let at = |item_ix: usize, cx: &mut gpui::VisualTestContext| {
            transcript.update(cx, |this, cx| {
                this.list.scroll_to(ListOffset { item_ix, offset_in_item: px(0.0) });
                cx.notify();
            });
            cx.run_until_parked();
            transcript.read_with(cx, |this, _| this.sticky_group())
        };
        assert_eq!(at(3, cx), None, "its header is still read");
        let under_prompt = Theme::TITLEBAR_HEIGHT + Transcript::sticky_height();
        assert_eq!(at(25, cx), Some((10, under_prompt)), "held under the prompt's band");
        assert_eq!(at(50, cx), None, "its rows have gone by");
        assert_eq!(at(25, cx), Some((10, under_prompt)));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let band = cx.debug_bounds("sticky-group").expect("the band");
        cx.simulate_click(band.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let open = transcript.read_with(cx, |this, _| this.folds.get(&group).and_then(|fold| fold.open));
        assert_eq!(open, Some(false), "a press folds it");
    }
}
