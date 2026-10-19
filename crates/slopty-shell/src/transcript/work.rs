//! slopty: background work as the transcript shows it, Slopty's own (not in zeron): a
//! background task's notice as a quiet row linked to its task, the agent's quiet notices (a
//! request tried again counts down to its next attempt in place), and what a tool call says
//! beyond zeron's chip: its result in the TUI's words, its timer, the task it became, and "Run in
//! background" while it blocks the turn.
//!
//! The words are the view model's (`slopty_view_doc::part_kinds`); this module only draws them
//! and ticks what runs from the times they carry. The Background tray is the dock's
//! (`background_tray.rs`); a row here reaches it through [`reveal_work`], by the tray row's id.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{AnyElement, Context, SharedString, Task, div, prelude::*, px};
use serde::Serialize;
use slopty_view_doc::{NoteLine, NoteRow, TaskRow, ToolFacts, ToolTimer};

use super::{Row, RowKind, Transcript, fnv1a};
use crate::settings::widgets::ActionHint as _;
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The height of a task notice's or a notice's line.
pub const NOTICE_ROW_HEIGHT: f32 = 26.0;

/// How often what runs (a timer, a countdown, a subagent's elapsed time) is drawn again.
const TICK: Duration = Duration::from_secs(1);

/// What the rows of this module keep between frames.
#[derive(Default)]
pub(super) struct WorkState {
    /// Notices whose body is open.
    open: HashSet<SharedString>,
    /// Calls asked to move to the background, until their part says otherwise.
    backgrounding: HashSet<String>,
    /// The redraw of what ticks, while anything drawn ticks.
    tick: Option<Task<()>>,
}

/// The hook from the transcript to the Background tray (`background_tray.rs`, the dock's): a
/// subagent's card, a task's notice or a call's task link asks the tray of chat `chat_id` to open
/// at its row `row_id`, a [`slopty_view_doc::WorkRow::id`] (`task:{id}` for one of the agent's
/// tasks, `chat:{chatId}` for a child chat no task stands for). The tray observes this global
/// (`cx.observe_global::<WorkReveal>`), opens itself, scrolls the row into view and marks it;
/// `seq` grows with every ask, so the same row asked twice opens twice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkReveal {
    /// The chat whose tray opens.
    pub chat_id: String,
    /// The tray row to open at.
    pub row_id: String,
    /// One more than the ask before it.
    pub seq: u64,
}

impl gpui::Global for WorkReveal {}

/// Ask the Background tray of chat `chat_id` to open at its row `row_id` ([`WorkReveal`]).
pub fn reveal_work(chat_id: &str, row_id: &str, cx: &mut gpui::App) {
    let seq = cx.try_global::<WorkReveal>().map_or(1, |last| last.seq.wrapping_add(1));
    cx.set_global(WorkReveal {
        chat_id: chat_id.to_owned(),
        row_id: row_id.to_owned(),
        seq,
    });
}

/// Fold `value`'s every field into a row's fingerprint: these shapes change in place.
pub fn hash_into<T: Serialize + ?Sized>(acc: &mut Vec<u8>, value: &T) {
    if serde_json::to_writer(&mut *acc, value).is_err() {
        acc.push(0xff);
    }
}

fn row(entry_id: &str, part_id: &str, version: u64, kind: RowKind) -> Row {
    Row {
        id: format!("{entry_id}#{part_id}").into(),
        version,
        turn_start: false,
        kind,
        entry_id: entry_id.to_owned().into(),
        timestamp: None,
        copy_text: None,
        compact_fold: None,
    }
}

/// A background task's notice as its row.
pub fn task_row(entry_id: &str, part_id: &str, task: &TaskRow) -> Row {
    let mut acc = Vec::new();
    hash_into(&mut acc, task);
    row(
        entry_id,
        part_id,
        fnv1a(&acc),
        RowKind::TaskNote {
            task: std::sync::Arc::new(task.clone()),
        },
    )
}

/// A quiet notice as its row.
pub fn notice_row(entry_id: &str, part_id: &str, note: &NoteRow) -> Row {
    let mut acc = Vec::new();
    hash_into(&mut acc, note);
    row(
        entry_id,
        part_id,
        fnv1a(&acc),
        RowKind::Notice {
            note: std::sync::Arc::new(note.clone()),
        },
    )
}

/// How long a call has run by `now`, and its limit: "12s", "1m 5s · timeout 2m".
pub fn timer_words(timer: &ToolTimer, now: i64) -> String {
    let since = u64::try_from(now.saturating_sub(timer.at)).unwrap_or(0);
    let elapsed = slopty_view_doc::duration_words(timer.elapsed_ms.saturating_add(since));
    match timer.timeout_ms {
        Some(limit) => format!("{elapsed} · timeout {}", slopty_view_doc::duration_words(limit)),
        None => elapsed,
    }
}

/// What a notice says at `now`: a request tried again counts down to its next attempt.
pub fn notice_words(note: &NoteRow, now: i64) -> String {
    match &note.retry {
        Some(retry) if !note.recovered => {
            let left = retry.at.map(|at| u64::try_from(at.saturating_sub(now)).unwrap_or(0));
            retry.words(left)
        }
        _ => note.text.clone(),
    }
}

/// The colour of a task's dot: done green, failed red, stopped amber, else by its tone, as the
/// TUI colours its `●`.
fn task_tint(task: &TaskRow, theme: &Theme) -> gpui::Hsla {
    match task.status.as_str() {
        "completed" => theme.success,
        "failed" => theme.danger,
        "killed" | "expired" => theme.warning,
        _ => tone_tint(&task.tone, theme),
    }
}

fn tone_tint(tone: &str, theme: &Theme) -> gpui::Hsla {
    match tone {
        NoteLine::ERROR => theme.danger,
        NoteLine::WARNING => theme.warning,
        NoteRow::SUCCESS => theme.success,
        _ => theme.text_faint,
    }
}

/// What a tool chip's header adds from its call's facts: the result line, the timer, the task
/// link and "Run in background".
pub(super) struct ChipFacts {
    /// The chat the call is in, which the tray and `slopty.Background` name.
    pub chat_id: Option<String>,
    /// Calls asked to move to the background, not yet moved.
    pub backgrounding: HashSet<String>,
    /// The transcript the chip is in, which asks for the move.
    pub transcript: gpui::WeakEntity<Transcript>,
    /// slopty: the time a running timer reads (`Transcript::clock`), ms since the epoch.
    pub now: i64,
}

impl ChipFacts {
    /// The trailing words and controls of `facts` on the chip of part `part_id`, or nothing.
    pub fn trail(
        &self,
        part_id: &str,
        facts: &ToolFacts,
        resolved: bool,
        theme: &Theme,
    ) -> Option<AnyElement> {
        let running_timer = facts.timer.as_ref().filter(|_| !resolved);
        let words = match (running_timer, &facts.result) {
            (Some(timer), _) => Some(timer_words(timer, self.now)),
            (None, Some(result)) => Some(result.clone()),
            (None, None) => None,
        };
        let background = (facts.backgroundable && !resolved && !self.backgrounding.contains(part_id))
            .then_some(self.chat_id.clone())
            .flatten();
        if words.is_none() && facts.task_row.is_none() && background.is_none() {
            return None;
        }
        let tint = if facts.failed {
            theme.danger
        } else {
            theme.text_faint
        };
        let mut trail = div().flex_none().flex().flex_row().items_center().gap(px(6.0));
        if let Some(words) = words {
            let text = div()
                .flex_none()
                .max_w(px(260.0))
                .truncate()
                .text_size(crate::typography::ui_rems(11.0))
                // slopty: a timer that ticks holds its width.
                .font_features(crate::typography::tabular())
                .text_color(tint)
                .child(SharedString::from(words.clone()));
            trail = match (&facts.task_row, &self.chat_id) {
                // The words link to the task the call became: its tray row.
                (Some(task_row), Some(chat)) => {
                    let (task_row, chat) = (task_row.clone(), chat.clone());
                    let fade = format!("{part_id}-task");
                    trail.child(
                        text.id(SharedString::from(fade.clone()))
                            .role(gpui::Role::Link)
                            .aria_label(SharedString::from(format!("{words}, in the background tray")))
                            .cursor_pointer()
                            // slopty: a faded hover, and a tab stop.
                            .text_color(crate::motion::hover_blend(&fade, tint, theme.text_muted))
                            .hover_fade(fade.clone())
                            .tab_index(0)
                            .rounded(px(4.0))
                            .map(|el| crate::surface_chrome::focus_ring(el, theme))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                reveal_work(&chat, &task_row, cx);
                            }),
                    )
                }
                _ => trail.child(
                    text.id(SharedString::from(format!("{part_id}-result")))
                        .role(gpui::Role::Status)
                        .aria_label(SharedString::from(words)),
                ),
            };
        }
        if let Some(chat) = background {
            let part = part_id.to_owned();
            let weak = self.transcript.clone();
            trail = trail.child(
                div()
                    .id(SharedString::from(format!("{part_id}-background")))
                    .role(gpui::Role::Button)
                    .aria_label("Run in background")
                    .flex_none()
                    .h(px(20.0))
                    .px(px(6.0))
                    .rounded(px(Theme::CONTROL_RADIUS))
                    .flex()
                    .items_center()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .opacity(0.0)
                    .group_hover("tool-header", |s| s.opacity(1.0))
                    // slopty: a tab stop, shown while it has the keyboard.
                    .tab_index(0)
                    .focus_visible({
                        let ring = crate::surface_chrome::ring(theme, crate::surface_chrome::RING_OFFSET);
                        move |s| s.opacity(1.0).outline_ring(ring)
                    })
                    // slopty: the faded hover, at the control strength.
                    .bg(crate::motion::hover_blend(
                        &format!("{part_id}-background"),
                        gpui::transparent_black(),
                        crate::theme::wash(0.11),
                    ))
                    .text_color(crate::motion::mix(
                        theme.text_muted,
                        theme.text,
                        crate::motion::hover_t(&format!("{part_id}-background")),
                    ))
                    .hover_fade(format!("{part_id}-background"))
                    // slopty: ⌃B moves the newest running call there (`BackgroundRunningCall`).
                    .tooltip(crate::settings::widgets::action_tooltip(
                        "Run in background",
                        crate::settings::widgets::Keys::Action(Box::new(BackgroundRunningCall)),
                    ))
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let chat = chat.clone();
                        let part = part.clone();
                        weak.update(cx, |this, cx| this.background_call(chat, part, cx))
                            .ok();
                    })
                    .child(SharedString::from("Run in background")),
            );
        }
        Some(trail.into_any_element())
    }
}

gpui::actions!(
    transcript,
    [
        /// ⌃B, Claude Code's own key: the newest running call that can go on in the
        /// background goes there, the turn going on past it.
        BackgroundRunningCall
    ]
);

impl Transcript {
    /// slopty: ⌃B: move the newest running call that can go to the background there. Whether
    /// there was one.
    pub(crate) fn background_latest(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.work_chat() else {
            return false;
        };
        let part = {
            let state = self.state.read(cx);
            let entries = match &self.doc_override {
                Some(doc) => state.sub_transcript(doc),
                None => state.transcript.as_slice(),
            };
            entries
                .iter()
                .rev()
                .flat_map(|entry| entry.parts.iter().rev())
                .find_map(|part| match part {
                    slopty_view_doc::MessagePart::Tool { id, resolved: false, facts: Some(facts), .. }
                        if facts.backgroundable && !self.work.backgrounding.contains(id) =>
                    {
                        Some(id.clone())
                    }
                    _ => None,
                })
        };
        let Some(part) = part else {
            return false;
        };
        self.background_call(chat, part, cx);
        true
    }

    /// The chat this transcript shows, which the dock's calls name.
    pub(crate) fn work_chat(&self) -> Option<String> {
        self.doc_override.clone().or_else(|| self.chat_id.clone())
    }

    /// What a chip's header adds from its call's facts, for this transcript.
    pub(super) fn chip_facts(&self, cx: &Context<Self>) -> ChipFacts {
        ChipFacts {
            chat_id: self.work_chat(),
            backgrounding: self.work.backgrounding.clone(),
            transcript: cx.weak_entity(),
            now: self.clock.timestamp_millis(),
        }
    }

    /// Draw again in a second: something drawn runs (a timer, a countdown). Each frame that
    /// draws it asks again, so the redraws stop with the last of it.
    pub(super) fn tick_soon(&mut self, cx: &mut Context<Self>) {
        if self.work.tick.is_some() {
            return;
        }
        self.work.tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TICK).await;
            this.update(cx, |this, cx| {
                this.work.tick = None;
                // slopty: what runs reads the time this tick took, in every frame built after it.
                this.clock = crate::clock::utc(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Move the running call `part_id` to the background, the turn going on past it.
    pub(super) fn background_call(&mut self, chat: String, part_id: String, cx: &mut Context<Self>) {
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        if !self.work.backgrounding.insert(part_id.clone()) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let params = serde_json::json!({ "chatId": chat, "partId": part_id });
            if let Err(err) = engine
                .client()
                .call(slopty_view_doc::calls::BACKGROUND, params)
                .await
            {
                tracing::warn!(error = %err, "moving a call to the background failed");
            }
            this.update(cx, |this, cx| {
                this.work.backgrounding.remove(&part_id);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Stop one row of the chat's background work (`slopty.StopWork`).
    pub(super) fn stop_work(&mut self, row_id: String, cx: &mut Context<Self>) {
        let Some(chat) = self.work_chat() else {
            return;
        };
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            return;
        };
        cx.spawn(async move |_, _| {
            let params = serde_json::json!({ "chatId": chat, "rowId": row_id });
            if let Err(err) = engine
                .client()
                .call(slopty_view_doc::calls::STOP_WORK, params)
                .await
            {
                tracing::warn!(error = %err, "stopping background work failed");
            }
        })
        .detach();
    }

    fn toggle_work_body(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if !self.work.open.remove(&id) {
            self.work.open.insert(id);
        }
        cx.notify();
    }

    /// A background task's notice: a dot in its status's colour, its words, the task's
    /// description muted on the right; a click opens the task in the tray, and a notice with a
    /// body (a monitor's event, an agent's report) opens it under the line.
    pub(super) fn render_task_note(
        &mut self,
        row: &Row,
        task: &TaskRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.work.open.contains(&row.id);
        let chat = self.work_chat();
        let tray_row = task.row.clone();
        let label = if task.title.is_empty() {
            task.text.clone()
        } else {
            format!("{} · {}", task.text, task.title)
        };
        let line = div()
            .id(SharedString::from(format!("{}-task", row.id)))
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(label))
            .aria_value(SharedString::from(task.status.clone()))
            .h(px(NOTICE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .cursor_pointer()
            .text_size(px(super::TOOL_TEXT_SIZE))
            // slopty: a faded hover, and a tab stop.
            .text_color(crate::motion::hover_blend(&format!("{}-task", row.id), theme.text_muted, theme.text))
            .hover_fade(format!("{}-task", row.id))
            .tab_index(0)
            .rounded(px(4.0))
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(chat) = &chat {
                    reveal_work(chat, &tray_row, cx);
                }
            }))
            .child(
                div()
                    .w(px(super::ACTIVITY_TRUNK_X * 2.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(div().size(px(6.0)).rounded_full().bg(task_tint(task, theme))),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink_1()
                    .truncate()
                    .child(SharedString::from(task.text.clone())),
            )
            .when(!task.figures.is_empty(), |line| {
                line.child(
                    div()
                        .flex_none()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(task.figures.clone())),
                )
            })
            .child(div().flex_1())
            .when(!task.title.is_empty(), |line| {
                line.child(
                    div()
                        .flex_none()
                        .max_w(px(240.0))
                        .truncate()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(task.title.clone())),
                )
            });
        let body = task.body.as_ref().filter(|body| !body.trim().is_empty());
        let toggle = body.map(|_| {
            let id = row.id.clone();
            disclosure(
                format!("{}-body", row.id),
                open,
                theme,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_work_body(id.clone(), cx);
                }),
            )
        });
        let line = line.children(toggle);
        div()
            .w_full()
            .flex()
            .flex_col()
            .font_family(theme.font_sans_fixed.clone())
            .child(line)
            .children(body.filter(|_| open).map(|body| body_block(body, theme)))
            .into_any_element()
    }

    /// A quiet notice of the agent's: a dot in its tone, its words; a request tried again
    /// counts down to its next attempt with the working spinner, and reads recovered once it
    /// went through.
    pub(super) fn render_notice(
        &mut self,
        row: &Row,
        note: &NoteRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.work.open.contains(&row.id);
        let retrying = note.retry.is_some() && !note.recovered;
        if retrying {
            self.tick_soon(cx);
        }
        let words = notice_words(note, self.clock.timestamp_millis());
        let tint = tone_tint(&note.tone, theme);
        let lead: AnyElement = if retrying {
            crate::loaders::mini_glyph_spinner(
                format!("{}-retry", row.id),
                2.0,
                theme.glyph,
                cx.entity_id(),
                cx,
            )
            .into_any_element()
        } else {
            div().size(px(6.0)).rounded_full().bg(tint).into_any_element()
        };
        let detail = note.detail.as_ref().filter(|detail| !detail.trim().is_empty());
        let toggle = detail.map(|_| {
            let id = row.id.clone();
            disclosure(
                format!("{}-detail", row.id),
                open,
                theme,
                cx.listener(move |this, _, _, cx| {
                    this.toggle_work_body(id.clone(), cx);
                }),
            )
        });
        let line = div()
            .id(SharedString::from(format!("{}-notice", row.id)))
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(words.clone()))
            .aria_value(SharedString::from(note.kind.clone()))
            .h(px(NOTICE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .text_size(px(super::TOOL_TEXT_SIZE))
            .text_color(if note.tone == NoteLine::ERROR {
                theme.danger
            } else {
                theme.text_muted
            })
            .child(
                div()
                    .w(px(super::ACTIVITY_TRUNK_X * 2.0))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(lead),
            )
            .child(div().min_w_0().flex_1().truncate().child(SharedString::from(words)))
            .children(toggle);
        div()
            .w_full()
            .flex()
            .flex_col()
            .font_family(theme.font_sans_fixed.clone())
            .child(line)
            .children(detail.filter(|_| open).map(|detail| body_block(detail, theme)))
            .into_any_element()
    }
}

/// The chevron that opens what sits under a notice.
fn disclosure(
    id: String,
    open: bool,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let fade = SharedString::from(format!("{id}-hover"));
    // slopty: a 24 px target drawn at 18 (its pad taken back), a faded hover, and its
    // words on hover too; on a touch screen a finger's 44 around the 24 it draws (#23).
    let (press, reach) = crate::composer::finger_press(SharedString::from(id), 24.0, 24.0);
    crate::surface_chrome::focus_ring_within(press.m(px(-3.0 - reach)), theme, reach)
        .role(gpui::Role::Button)
        .aria_expanded(open)
        .cursor_pointer()
        .hover_fade(fade.clone())
        .tab_index(0) // slopty: a tab stop.
        .hint(if open { "Hide details" } else { "Show details" })
        .on_click(on_click)
        .child(
            div()
                .size(px(24.0))
                .flex_none()
                .rounded(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(crate::motion::hover_blend(&fade, gpui::transparent_black(), crate::theme::ink(0.08)))
                .child(
                    crate::icons::icon(if open {
                        crate::icons::ALT_ARROW_DOWN
                    } else {
                        crate::icons::ALT_ARROW_RIGHT
                    })
                    .size(px(12.0))
                    .text_color(theme.text_faint),
                ),
        )
        .into_any_element()
}

/// What opens under a notice: mono, muted, capped.
fn body_block(text: &str, theme: &Theme) -> AnyElement {
    div()
        .ml(px(super::ACTIVITY_TRUNK_X * 2.0 + Theme::SPACE_SM))
        .mb(px(Theme::SPACE_XS))
        .max_h(px(240.0))
        .overflow_hidden()
        .rounded(px(8.0))
        .bg(crate::theme::ink(0.04))
        .px(px(10.0))
        .py(px(8.0))
        .font_family(theme.font_mono.clone())
        .text_size(px(11.5))
        .line_height(px(17.0))
        .text_color(theme.text_muted)
        .child(SharedString::from(text.trim().to_owned()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::RetryLine;

    /// A timer reads its elapsed time from when the agent said it, and its limit.
    #[test]
    fn a_timer_runs_on_from_when_it_was_said() {
        let timer = ToolTimer {
            elapsed_ms: 10_000,
            at: 1_000,
            timeout_ms: Some(120_000),
        };
        assert_eq!(timer_words(&timer, 3_000), "12s · timeout 2m");
        let open = ToolTimer {
            elapsed_ms: 0,
            at: 5_000,
            timeout_ms: None,
        };
        assert_eq!(timer_words(&open, 4_000), "0s", "a clock behind never runs backwards");
    }

    /// A retry counts down to its next attempt and reads as the agent's words once recovered.
    #[test]
    fn a_retry_counts_down_then_reads_recovered() {
        let mut note = NoteRow {
            kind: "retry".into(),
            text: "API error · recovered".into(),
            tone: NoteLine::WARNING.into(),
            retry: Some(RetryLine {
                attempt: 2,
                max: Some(10),
                at: Some(10_000),
                what: "API error".into(),
                hint: None,
            }),
            ..NoteRow::default()
        };
        assert_eq!(notice_words(&note, 5_000), "API error · Retrying in 5s · attempt 2/10");
        assert_eq!(notice_words(&note, 10_500), "API error · Retrying now · attempt 2/10");
        note.recovered = true;
        assert_eq!(notice_words(&note, 5_000), "API error · recovered");
    }
}
