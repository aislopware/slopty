//! An open tool group's header held at the top while its rows scroll (opencode's sticky
//! group trigger, `.research/opencode-study-2026-10-19.md` item 3): once the header has scrolled
//! past the transcript's top while its rows still reach past it, a band of it spans that edge, on
//! the pane's own ground, ending in a soft scroll edge rather than a hairline. A press folds the
//! group and goes back to it. The band paints deferred, above the transcript's top fade, which
//! would otherwise fade it with the words it stays above.
//!
//! The person's prompt is held nowhere (2026-10-20): Amp, T3 Code and opencode hold none, and
//! Claude desktop keeps its peek behind a flag; find goes back to a prompt.

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Window, div, px};

use super::{RowKind, Transcript};
use crate::motion;
use crate::motion::HoverFade as _;
use crate::theme::Theme;

/// The group band's height, at least: opencode's 28 pt trigger, or a finger's row where the
/// header it holds stands that tall.
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
        let mid = usize::midpoint(read, past);
        let next = mid.saturating_add(1);
        if top(next) <= line { read = next } else { past = mid }
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
        if !self.group_open(read) || top(read) >= line {
            return None;
        }
        (top(read.saturating_add(1).min(len)) > line + band_height()).then_some(read)
    }

    /// Whether row `ix` is a tool group shown open behind a header that folds it.
    fn group_open(&self, ix: usize) -> bool {
        let Some(row) = self.rows.get(ix) else { return false };
        let RowKind::ToolGroup { tools, .. } = &row.kind else {
            return false;
        };
        let (collapses, solo) = super::group_shape(tools);
        collapses && !solo && self.steps_shown(self.folds.get(&row.id).and_then(|fold| fold.open))
    }

    /// Fold the group headed at row `ix` and glide back to it.
    fn fold_held_group(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        if !matches!(row.kind, RowKind::ToolGroup { .. }) {
            return;
        }
        let id = row.id.clone();
        let height = self.tool_group_reveals.get(&id).map_or(0.0, |reveal| reveal.rendered_height);
        self.toggle_fold(id, height, cx);
        self.scroll_to_row(ix, cx);
        cx.notify();
    }

    /// What the transcript holds at its top: the open group's band, and a watch that
    /// draws the transcript again when the next frame's layout holds another.
    pub(super) fn render_sticky_group(
        &self,
        _window: &Window,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let group = self.sticky_group();
        let view = cx.weak_entity();
        let watch = gpui::canvas(
            move |_, _, cx| {
                let Some(transcript) = view.upgrade() else { return };
                if transcript.read(cx).sticky_group() != group {
                    cx.defer(move |cx| transcript.update(cx, |_, cx| cx.notify()));
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_0()
        .into_any_element();
        let mut drawn = vec![watch];
        drawn.extend(group.and_then(|ix| self.render_held_group(ix, cx)));
        drawn
    }

    fn render_held_group(&self, ix: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let row = self.rows.get(ix)?;
        let RowKind::ToolGroup { summary, .. } = &row.kind else {
            return None;
        };
        let theme = Theme::of(cx).clone();
        // The pane's own ground, so the band reads as the header staying put, not a bar.
        let fill = if theme.is_frost() { theme.glass() } else { theme.session_ground() };
        let key: SharedString = format!("sticky-group-{}", cx.entity_id()).into();
        let content = self.held_group_header(ix, &summary.text, &key, &theme, cx);
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
                    .debug_selector(|| "sticky-group".into()) // for the tests
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
        cx: &Context<Self>,
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
                div().w(px(22.0)).flex_none().flex().items_center().justify_center().child(
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
                    turn: None,
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

    /// An open group's header holds at the transcript's top while its rows scroll, not
    /// before its header has scrolled under, nor once its rows have gone by; it says the group's
    /// sentence, and a press on it folds the group.
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
        let runs: Vec<super::super::ToolItem> = (0..30)
            .map(|n| {
                let entry = super::super::rows_for_entry(
                    &slopty_view_doc::SessionMessageEntry {
                        id: format!("run-{n}"),
                        role: slopty_view_doc::MessageRole::Assistant,
                        parts: vec![slopty_view_doc::MessagePart::Tool {
                            id: format!("call-{n}"),
                            call: slopty_view_proto::ToolCall::Exec {
                                command: format!("cargo test -p crate{n}"),
                            },
                            is_error: false,
                            resolved: true,
                            output: None,
                            output_ref: None,
                            output_bytes: None,
                            diff_ref: None,
                            diff_stats: None,
                            subagent_ref: None,
                            subagent_status: None,
                            facts: None,
                        }],
                        created_at: 0,
                        device_id: "d".into(),
                        status: None,
                        duration_ms: None,
                    },
                    &mut |_, text| Arc::new(crate::markdown::parser::parse_full(text)),
                );
                let Some(RowKind::ToolGroup { tools, .. }) =
                    entry.into_iter().next().map(|row| row.kind)
                else {
                    panic!("a call's group");
                };
                tools[0].clone()
            })
            .collect();
        transcript.update(cx, |this, cx| {
            this.rows = (0..40)
                .map(|ix| match ix {
                    5 => row(ix, Some("Fix the flaky upload test")),
                    10 => Row {
                        kind: RowKind::ToolGroup {
                            summary: "Ran 30 commands".into(),
                            tools: Arc::from(runs.as_slice()),
                        },
                        ..row(ix, None)
                    },
                    _ => row(ix, None),
                })
                .collect();
            this.folds.insert(
                group.clone(),
                super::super::FoldState { open: Some(true), ..Default::default() },
            );
            this.list.reset(40);
            this.pinned = false;
            this.list.set_follow_mode(gpui::FollowMode::Normal);
            cx.notify();
        });
        let (_fixture, cx) = cx.add_window_view(|_, _| Fixture(transcript.clone()));
        let at = |item_ix: usize, into: f32, cx: &mut gpui::VisualTestContext| {
            transcript.update(cx, |this, cx| {
                this.list.scroll_to(ListOffset { item_ix, offset_in_item: px(into) });
                cx.notify();
            });
            cx.run_until_parked();
            transcript.read_with(cx, |this, _| this.sticky_group())
        };
        assert_eq!(at(3, 0.0, cx), None, "its header is still read");
        assert_eq!(at(10, 240.0, cx), Some(10), "held at the transcript's own top");
        assert_eq!(at(20, 0.0, cx), None, "its rows have gone by");
        assert_eq!(at(10, 240.0, cx), Some(10));
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
                tree.nodes
                    .iter()
                    .filter_map(|(_, node)| node.label().map(str::to_owned))
                    .collect::<Vec<_>>()
            })
        });
        assert!(
            said.is_some_and(|labels| labels.iter().any(|label| label == "Ran 30 commands")),
            "the group's own sentence"
        );
        let band = cx.debug_bounds("sticky-group").expect("the band");
        cx.simulate_click(band.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let open =
            transcript.read_with(cx, |this, _| this.folds.get(&group).and_then(|fold| fold.open));
        assert_eq!(open, Some(false), "a press folds it");
    }
}
