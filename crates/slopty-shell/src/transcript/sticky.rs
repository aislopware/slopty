// slopty: Slopty's own file (not in zeron).
//! slopty: an open tool group's header held at the top while its rows scroll (opencode's sticky
//! group trigger, `.research/opencode-study-2026-10-19.md` item 3): once the header has scrolled
//! past the transcript's top while its rows still reach past it, a band of it spans that edge, on
//! the pane's own ground, ending in a soft scroll edge rather than a hairline. A press folds the
//! group and goes back to it. A turn's divider holds as the same centred divider it is in the
//! list, in its own short words ("Worked for 4m 13s"), never the sentence of all its work. The
//! band paints deferred, above the transcript's top fade, which would otherwise fade it with the
//! words it stays above.
//!
//! The person's prompt is held nowhere (2026-10-20): Amp, T3 Code and opencode hold none, and
//! Claude desktop keeps its peek behind a flag; find goes back to a prompt.

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px};

use super::{RowKind, Transcript};
use crate::motion;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The group band's height, at least: opencode's 28 pt trigger, or a finger's row where the
/// divider it holds stands that tall.
const GROUP_BAND_H: f32 = 28.0;

fn band_height() -> f32 {
    super::tool_group_header_height().max(GROUP_BAND_H)
}

/// The soft scroll edge under the band: its ground fading out over the rows passing beneath
/// (`.research/design-premium-2026-10-29.md` §7.4).
const GROUP_BAND_EDGE: f32 = 24.0;

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

    /// The open tool group to hold at the transcript's top, by its header's row: a group whose
    /// header has scrolled past that edge while its rows still reach a band's height past it.
    /// The transcript lays out inside its pane, so the edge is the list's own. One at most; none
    /// while find is open.
    pub(super) fn sticky_group(&self) -> Option<usize> {
        if self.finding() {
            return None;
        }
        let (len, top) = self.row_tops()?;
        let line = f32::from(self.list.viewport_bounds().top());
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
        (top(end.min(len)) > line + band_height()).then_some(header)
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


    /// slopty: what the transcript holds at its top: the open group's band, and a watch that
    /// draws the transcript again when the next frame's layout holds another.
    pub(super) fn render_sticky_group(&self, _window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let group = self.sticky_group();
        let view = cx.weak_entity();
        let watch = gpui::canvas(
            move |_, _, cx| {
                let Some(transcript) = view.upgrade() else { return };
                if transcript.read(cx).sticky_group() != group {
                    cx.defer(move |cx| transcript.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
        .into_any_element();
        let mut drawn = vec![watch];
        drawn.extend(group.and_then(|ix| self.render_held_group(ix, cx)));
        drawn
    }

    fn render_held_group(&self, ix: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = &self.rows[ix];
        let RowKind::ToolGroup { summary, worked_secs, compact_shell, .. } = &row.kind else {
            return None;
        };
        let theme = Theme::of(cx).clone();
        // The pane's own ground, so the band reads as the divider staying put, not a bar.
        let fill = if theme.is_frost() { theme.glass() } else { theme.session_ground() };
        let key: SharedString = format!("sticky-group-{}", cx.entity_id()).into();
        let content = if *compact_shell {
            // slopty: a turn's divider holds as the divider it is, centred between its rules, in
            // its own short words ("Worked for 4m 13s"); its words in full stay what a screen
            // reader hears.
            let words: SharedString = match worked_secs {
                Some(secs) => super::worked_for_label(*secs).into(),
                None => super::divider_words(true).into(),
            };
            let label = SharedString::from(super::spoken_divider(&words, &summary.text));
            let held: SharedString = format!("{}-held", row.id).into();
            super::turn_divider(
                &held,
                label,
                super::tool_group_title(words, &[], None, &theme),
                true,
                1.0,
                None,
                &theme,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.fold_held_group(ix, cx);
                }),
            )
            .w_full()
            .into_any_element()
        } else {
            self.held_group_header(ix, &summary.text, &key, &theme, cx)
        };
        let band = div()
            .id(key)
            .w_full()
            .flex()
            .flex_col()
            // Frost blurs the band alone: blurred under its soft edge, the rows would end in a
            // hard line.
            .child(crate::frost::frosted(
                0.0,
                crate::frost::MENU_BLUR,
                div()
                    .id("sticky-group-band")
                    .debug_selector(|| "sticky-group".into()) // slopty: for the tests
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w_full()
                    .h(px(band_height()))
                    .bg(fill)
                    .px(px(self.row_inset()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(div().w_full().max_w(px(self.content_width)).min_w_0().child(content)),
            ))
            .child(div().w_full().h(px(GROUP_BAND_EDGE)).bg(gpui::linear_gradient(
                180.0,
                gpui::linear_color_stop(fill, 0.0),
                gpui::linear_color_stop(fill.opacity(0.0), 1.0),
            )));
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right(px(self.right_reserve))
                .child(gpui::deferred(band))
                .into_any_element(),
        )
    }

    /// A tool group's own header, held: its chevron and its work's sentence, a press folding it.
    fn held_group_header(
        &self,
        ix: usize,
        words: &SharedString,
        key: &SharedString,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = motion::hover_t(key);
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
                    .child(words.clone()),
            );
        div()
            .id(SharedString::from(format!("{key}-header")))
            .role(gpui::Role::Button)
            .aria_label(words.clone())
            .aria_expanded(true)
            .tab_index(0)
            .focus_visible({
                let accent = theme.accent;
                move |style| style.border_color(accent)
            })
            .hover_fade(key.clone())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.fold_held_group(ix, cx)))
            .h_full()
            .w_full()
            .flex()
            .items_center()
            .child(column)
            .into_any_element()
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
                    markdown: None,
                    attachments: Arc::default(),
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

    /// slopty: an open group's header holds under the titlebar while its rows scroll, not
    /// before its header has scrolled under, nor once folded; a press on it folds the group. A
    /// turn's divider holds in its own short words, not its work's sentence.
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
        assert_eq!(at(25, cx), Some(10), "held at the transcript's own top");
        assert_eq!(at(50, cx), None, "its rows have gone by");
        assert_eq!(at(25, cx), Some(10));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        cx.update(|window, _| window.set_a11y_active(true));
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let said = cx.update(|window, _| {
            window.a11y_tree().map(|tree| {
                tree.nodes.iter().filter_map(|(_, node)| node.label().map(str::to_owned)).collect::<Vec<_>>()
            })
        });
        assert!(
            said.is_some_and(|labels| labels.iter().any(|label| label == "Hide work, ran 30 commands")),
            "the divider's own words, then its work"
        );
        let band = cx.debug_bounds("sticky-group").expect("the band");
        cx.simulate_click(band.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let open = transcript.read_with(cx, |this, _| this.folds.get(&group).and_then(|fold| fold.open));
        assert_eq!(open, Some(false), "a press folds it");
    }
}
