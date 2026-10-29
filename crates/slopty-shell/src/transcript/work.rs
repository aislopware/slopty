//! slopty: background work as the transcript shows it, Slopty's own (not in zeron): a
//! background task's notice as a quiet row, the agent's quiet notices (a request tried again
//! counts down to its next attempt in place), and what a tool call says beyond zeron's chip: its
//! result in the TUI's words and its timer.
//!
//! The words are the view model's (`slopty_view_doc::part_kinds`); this module only draws them
//! and ticks what runs from the times they carry. It reads, never acts: moving a call to the
//! background or stopping a task is the agent's TUI's (`docs/decisions/agents.md`, "TUI-first").

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
    /// The redraw of what ticks, while anything drawn ticks.
    tick: Option<Task<()>>,
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

/// What a tool chip's header adds from its call's facts: the result line and the timer.
pub(super) struct ChipFacts {
    /// slopty: the time a running timer reads (`Transcript::clock`), ms since the epoch.
    pub now: i64,
}

impl ChipFacts {
    /// The trailing words of `facts` on the chip of part `part_id`, or nothing.
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
        let words = words?;
        let tint = if facts.failed {
            theme.danger
        } else {
            theme.text_faint
        };
        Some(
            div()
                .id(SharedString::from(format!("{part_id}-result")))
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex_none()
                .max_w(px(260.0))
                .truncate()
                .text_size(crate::typography::ui_rems(11.0))
                // slopty: a timer that ticks holds its width.
                .font_features(crate::typography::tabular())
                .text_color(tint)
                .child(SharedString::from(words))
                .into_any_element(),
        )
    }
}

impl Transcript {
    /// The chat this transcript shows, which its pages and charts name.
    pub(crate) fn work_chat(&self) -> Option<String> {
        self.doc_override.clone().or_else(|| self.chat_id.clone())
    }

    /// What a chip's header adds from its call's facts, for this transcript.
    pub(super) fn chip_facts(&self) -> ChipFacts {
        ChipFacts { now: self.clock.timestamp_millis() }
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

    fn toggle_work_body(&mut self, id: SharedString, cx: &mut Context<Self>) {
        if !self.work.open.remove(&id) {
            self.work.open.insert(id);
        }
        cx.notify();
    }

    /// A background task's notice: a dot in its status's colour, its words, the task's
    /// description muted on the right; a notice with a body (a monitor's event, an agent's
    /// report) opens it under the line.
    pub(super) fn render_task_note(
        &mut self,
        row: &Row,
        task: &TaskRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.work.open.contains(&row.id);
        // slopty: the description on the right only where the words do not already say it
        // (`Background command "Build" completed` names its task).
        let title = Some(task.title.as_str()).filter(|t| !t.is_empty() && !task.text.contains(*t));
        let label = match title {
            Some(title) => format!("{} · {title}", task.text),
            None => task.text.clone(),
        };
        let line = div()
            .id(SharedString::from(format!("{}-task", row.id)))
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(label))
            .aria_value(SharedString::from(task.status.clone()))
            .h(px(NOTICE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .text_size(px(super::TOOL_TEXT_SIZE))
            .text_color(theme.text_muted)
            .child(lead_slot(Some(
                div().size(px(6.0)).rounded_full().bg(task_tint(task, theme)).into_any_element(),
            )))
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
            .when_some(title, |line, title| {
                line.child(
                    div()
                        .flex_none()
                        .max_w(px(240.0))
                        .truncate()
                        .text_color(theme.text_faint)
                        .child(SharedString::from(title.to_owned())),
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
    /// went through. `trailing` (a done line's Copy) follows its words.
    pub(super) fn render_notice(
        &mut self,
        row: &Row,
        note: &NoteRow,
        trailing: Option<AnyElement>,
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
        // slopty: a plain notice (the done line, "+N more tasks") is no list item: no dot (#4).
        let plain = !matches!(note.tone.as_str(), NoteLine::ERROR | NoteLine::WARNING | NoteRow::SUCCESS);
        let lead: Option<AnyElement> = if retrying {
            Some(
                crate::loaders::mini_glyph_spinner(
                    format!("{}-retry", row.id),
                    2.0,
                    theme.glyph,
                    cx.entity_id(),
                    cx,
                )
                .into_any_element(),
            )
        } else {
            (!plain).then(|| div().size(px(6.0)).rounded_full().bg(tint).into_any_element())
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
            .child(lead_slot(lead))
            .child(
                div()
                    .min_w_0()
                    .when(trailing.is_none(), |words| words.flex_1())
                    .truncate()
                    .child(SharedString::from(words)),
            )
            .children(trailing)
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

/// slopty: a notice's lead (its dot, or a retry's spinner) centred on the activity trunk, its
/// words starting on the group summaries' edge, [`super::ACTIVITY_TITLE_X`] (#4).
fn lead_slot(lead: Option<AnyElement>) -> gpui::Div {
    div()
        .w(px(super::ACTIVITY_TRUNK_X * 2.0))
        .mr(px(super::ACTIVITY_TITLE_X - super::ACTIVITY_TRUNK_X * 2.0 - Theme::SPACE_SM))
        .flex_none()
        .flex()
        .justify_center()
        .children(lead)
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
