//! Background work as the transcript shows it.
//!
//! A background task's notice as a quiet row, the agent's quiet notices (a request tried again
//! counts down to its next attempt in place), and what a tool call says beyond its chip: its
//! result in the TUI's words, and each step's time in a lane of its own at the row's trailing
//! edge (`docs/decisions/inspector.md`, "A step row reads at a glance").
//!
//! The words are the view model's (`slopty_view_doc::part_kinds`); this module only draws them
//! and ticks what runs from the times they carry. It reads, never acts: moving a call to the
//! background or stopping a task is the agent's TUI's (`docs/decisions/agents.md`, "TUI-first").

use std::collections::HashSet;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, Context, SharedString, Task, div, px};
use serde::Serialize;
use slopty_view_doc::{NoteLine, NoteRow, StepTime, TaskRow, ToolFacts, ToolTimer, ran_for_words};

use super::{Row, RowKind, Transcript, fnv1a};
use crate::motion::HoverFade as _;
use crate::settings::widgets::ActionHint as _;
use crate::theme::Theme;

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
    /// When the running turn began (ms since the epoch), whose whole seconds the tick lands on.
    beat: Option<i64>,
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
        RowKind::TaskNote { task: std::sync::Arc::new(task.clone()) },
    )
}

/// A quiet notice as its row.
pub fn notice_row(entry_id: &str, part_id: &str, note: &NoteRow) -> Row {
    let mut acc = Vec::new();
    hash_into(&mut acc, note);
    row(entry_id, part_id, fnv1a(&acc), RowKind::Notice { note: std::sync::Arc::new(note.clone()) })
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

/// The width of a step's time lane, reserved on every step row and group line, so its words
/// coming, ticking, and giving way to the time of day while ⌥ is held move nothing.
pub(super) const TIME_LANE: f32 = 56.0;

/// The time of day as a step's lane says it while ⌥ is held: to the second, on the system's
/// clock, without the day period the lane has no room for ("3:42:05", "15:42:05").
fn lane_clock() -> &'static str {
    if super::clock_format().contains('p') { "%-I:%M:%S" } else { "%H:%M:%S" }
}

/// `ms` as a time of day in `tz` by `format`.
fn clock_at<Tz: chrono::TimeZone>(ms: i64, format: &str, tz: &Tz) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|utc| utc.with_timezone(tz).format(format).to_string())
}

/// What a step's time lane says at `now`: its time, ticking from its start while it runs ("12s",
/// "1m 05s"), or what it took once it ended ("<1s", "1m 5s"); with ⌥ held (`alt`), the time of
/// day it began, in `tz`. A step whose records say no time says nothing.
pub(super) fn lane_words<Tz: chrono::TimeZone>(
    time: Option<&StepTime>,
    timer: Option<&ToolTimer>,
    resolved: bool,
    now: i64,
    alt: bool,
    tz: &Tz,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    if alt {
        return clock_at(time?.started, lane_clock(), tz);
    }
    if let Some(wall) = time.and_then(StepTime::wall_ms) {
        return Some(ran_for_words(wall));
    }
    if resolved {
        return None;
    }
    let ran_ms = match (time, timer) {
        (Some(time), _) => now.saturating_sub(time.started),
        // A clock behind the agent's never runs its time backwards.
        (None, Some(timer)) => i64::try_from(timer.elapsed_ms)
            .unwrap_or(i64::MAX)
            .saturating_add(now.saturating_sub(timer.at).max(0)),
        (None, None) => return None,
    };
    Some(super::ticking_elapsed(ran_ms / 1_000))
}

/// What a step's time lane says on hover, in `tz`: when it began, to the second, and how far
/// into its turn; when it ended; and, where its agent said its run apart, the wait for the
/// person's permission apart from the run.
pub(super) fn lane_tip<Tz: chrono::TimeZone>(time: &StepTime, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let full = if super::clock_format().contains('p') { "%-I:%M:%S %p" } else { "%H:%M:%S" };
    let mut lines = Vec::new();
    if let Some(began) = clock_at(time.started, full, tz) {
        lines.push(match time.turn_started.map(|turn| time.started.saturating_sub(turn)) {
            Some(into) if into >= 0 => {
                format!(
                    "Started {began}, {} into the turn",
                    ran_for_words(u64::try_from(into).unwrap_or(0))
                )
            }
            _ => format!("Started {began}"),
        });
    }
    if let Some(ended) = time.ended.and_then(|ended| clock_at(ended, full, tz)) {
        lines.push(format!("Ended {ended}"));
    }
    if let (Some(ran), Some(waited)) = (time.ran_ms, time.waited_ms())
        && waited > 0
    {
        lines.push(format!(
            "Ran {}, after {} waiting for permission",
            ran_for_words(ran),
            ran_for_words(waited)
        ));
    }
    lines.join("\n")
}

/// What an open step's Timing says: when it started (and how far into its turn), when it ended,
/// and what it took, its run told apart from the wait for the person's permission where the
/// agent says it (`inspector.md`, "Any step opens to its whole detail").
pub(super) fn timing_words<Tz: chrono::TimeZone>(time: &StepTime, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let tip = lane_tip(time, tz);
    let split = time.ran_ms.is_some() && time.waited_ms().is_some_and(|waited| waited > 0);
    let took =
        time.wall_ms().filter(|_| !split).map(|wall| format!("Took {}", ran_for_words(wall)));
    tip.lines().map(str::to_owned).chain(took).collect::<Vec<_>>().join(" · ")
}

/// The span of steps `times`: from the first start to the last end, open while any still runs
/// (`running`); `None` when none says when it began.
pub(super) fn span<'a>(
    times: impl IntoIterator<Item = &'a StepTime>,
    running: bool,
) -> Option<StepTime> {
    let mut started: Option<i64> = None;
    let mut ended: Option<i64> = None;
    let mut turn_started = None;
    for time in times {
        started = Some(started.map_or(time.started, |s| s.min(time.started)));
        ended = ended.max(time.ended);
        turn_started = turn_started.or(time.turn_started);
    }
    Some(StepTime {
        started: started?,
        ended: ended.filter(|_| !running),
        ran_ms: None,
        turn_started,
    })
}

/// What a tool chip's header adds from its call's facts: the result line and the time lane.
pub(super) struct ChipFacts {
    /// The time a running step reads (`Transcript::clock`), ms since the epoch.
    pub now: i64,
    /// ⌥ is held: every lane says the time of day its step began.
    pub alt: bool,
}

/// The trailing words of `facts` on the chip of part `part_id`, or nothing.
pub(super) fn trail(
    part_id: &str,
    facts: &ToolFacts,
    resolved: bool,
    theme: &Theme,
) -> Option<AnyElement> {
    // A running step's time is its lane's; its trail keeps only its limit.
    let running_timer = facts.timer.as_ref().filter(|_| !resolved);
    let words = match (running_timer, &facts.result) {
        (Some(timer), _) => timer
            .timeout_ms
            .map(|limit| format!("timeout {}", slopty_view_doc::duration_words(limit))),
        (None, Some(result)) => Some(result.clone()),
        (None, None) => None,
    };
    let words = words?;
    let tint = if facts.failed { theme.danger } else { theme.text_faint };
    Some(
        div()
            .id(SharedString::from(format!("{part_id}-result")))
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(words.clone()))
            .flex_none()
            .max_w(px(260.0))
            .truncate()
            .text_size(crate::typography::ui_rems(11.0))
            // A timer that ticks holds its width.
            .font_features(crate::typography::tabular())
            .text_color(tint)
            .child(SharedString::from(words))
            .into_any_element(),
    )
}

impl ChipFacts {
    /// The time lane of step `id` (its records' `time`, else its running `timer`), always drawn
    /// at its fixed width; its tooltip says when it began, how far into its turn, when it ended
    /// and any wait for permission.
    pub fn lane(
        &self,
        id: &str,
        time: Option<&StepTime>,
        timer: Option<&ToolTimer>,
        resolved: bool,
        theme: &Theme,
    ) -> AnyElement {
        let words = lane_words(time, timer, resolved, self.now, self.alt, &chrono::Local);
        let tip = time.map(|time| lane_tip(time, &chrono::Local)).filter(|tip| !tip.is_empty());
        div()
            .id(SharedString::from(format!("{id}-time")))
            .w(px(TIME_LANE))
            .flex_none()
            .flex()
            .justify_end()
            .whitespace_nowrap()
            .overflow_hidden()
            .text_size(crate::typography::ui_rems(11.0))
            .font_features(crate::typography::tabular())
            .text_color(theme.text_faint)
            .when_some(tip, |lane, tip| {
                lane.tooltip(move |window, cx| {
                    crate::settings::widgets::text_tooltip(tip.clone())(window, cx)
                })
                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
            })
            .children(words.map(SharedString::from))
            .into_any_element()
    }
}

impl Transcript {
    /// The chat this transcript shows, which its pages and charts name.
    pub(crate) fn work_chat(&self) -> Option<String> {
        self.doc_override.clone().or_else(|| self.chat_id.clone())
    }

    /// What a chip's header adds from its call's facts, for this transcript.
    pub(super) const fn chip_facts(&self) -> ChipFacts {
        ChipFacts { now: self.clock.timestamp_millis(), alt: self.alt }
    }

    /// Draw again on the next whole second of the turn running since `started` (ms since the
    /// epoch): its time is drawn, and runs.
    pub(super) fn tick_with_turn(&mut self, started: i64, cx: &Context<Self>) {
        self.work.beat = Some(started);
        self.tick_soon(cx);
    }

    /// Draw again in a second: something drawn runs (a timer, a countdown). Each frame that
    /// draws it asks again, so the redraws stop with the last of it. It is the one tick, on
    /// the running turn's next whole second, so every time that runs moves on in one frame
    /// (`inspector.md`, "One clock").
    pub(super) fn tick_soon(&mut self, cx: &Context<Self>) {
        if self.work.tick.is_some() {
            return;
        }
        let wait = match self.work.beat {
            Some(beat) => {
                let real = crate::clock::utc(cx).timestamp_millis();
                let to_next = 1_000_i64.saturating_sub(real.saturating_sub(beat).rem_euclid(1_000));
                // Just past the second, never just short of it.
                Duration::from_millis(u64::try_from(to_next).unwrap_or(1_000).saturating_add(2))
            }
            None => TICK,
        };
        self.work.tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _gone = this.update(cx, |this, cx| {
                this.work.tick = None;
                // What runs reads the time this tick took, in every frame built after it.
                this.clock = crate::clock::utc(cx);
                cx.notify();
            });
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
        &self,
        row: &Row,
        task: &TaskRow,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.work.open.contains(&row.id);
        // The description on the right only where the words do not already say it
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
    /// went through.
    pub(super) fn render_notice(
        &mut self,
        row: &Row,
        note: &NoteRow,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.work.open.contains(&row.id);
        let retrying = note.retry.is_some() && !note.recovered;
        if retrying {
            self.tick_soon(cx);
        }
        let words = notice_words(note, self.clock.timestamp_millis());
        let tint = tone_tint(&note.tone, theme);
        // A plain notice ("+N more tasks") is no list item: no dot (#4).
        let plain =
            !matches!(note.tone.as_str(), NoteLine::ERROR | NoteLine::WARNING | NoteRow::SUCCESS);
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
            .text_color(if note.tone == NoteLine::ERROR { theme.danger } else { theme.text_muted })
            .child(lead_slot(lead))
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

/// A notice's lead (its dot, or a retry's spinner) centred on the activity trunk, its
/// words starting on the group summaries' edge, [`super::ACTIVITY_TITLE_X`] (#4).
fn lead_slot(lead: Option<AnyElement>) -> gpui::Div {
    div()
        .w(px(super::ACTIVITY_TRUNK_X * 2.0))
        .mr(px(super::ACTIVITY_TRUNK_X.mul_add(-2.0, super::ACTIVITY_TITLE_X) - Theme::SPACE_SM))
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
    // A 24 px target drawn at 18 (its pad taken back), a faded hover, and its
    // words on hover too; on a touch screen a finger's 44 around the 24 it draws (#23).
    let (press, reach) = crate::composer::finger_press(SharedString::from(id), 24.0, 24.0);
    crate::surface_chrome::focus_ring_within(press.m(px(-3.0 - reach)), theme, reach)
        .role(gpui::Role::Button)
        .aria_expanded(open)
        .cursor_pointer()
        .hover_fade(fade.clone())
        .tab_index(0) // a tab stop.
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
                .bg(crate::motion::hover_blend(
                    &fade,
                    gpui::transparent_black(),
                    crate::theme::ink(0.08),
                ))
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
        .ml(px(super::ACTIVITY_TRUNK_X.mul_add(2.0, Theme::SPACE_SM)))
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
    use slopty_view_doc::RetryLine;

    use super::*;

    /// A step's lane ticks from its start while it runs (from the agent's timer when its records
    /// say no start), says what it took once it ended, nothing when its records say no time, and
    /// the time of day it began while ⌥ is held; its tooltip says how far into its turn it began
    /// and any wait for permission.
    #[test]
    fn a_steps_lane_says_its_time() {
        let utc = chrono::Utc;
        // 2026-07-01 15:42:05 UTC.
        let started = 1_782_920_525_000;
        let running =
            StepTime { started, ended: None, ran_ms: None, turn_started: Some(started - 12_000) };
        assert_eq!(
            lane_words(Some(&running), None, false, started + 65_000, false, &utc).as_deref(),
            Some("1m 05s")
        );
        let timer = ToolTimer { elapsed_ms: 10_000, at: 1_000, timeout_ms: Some(120_000) };
        assert_eq!(
            lane_words(None, Some(&timer), false, 3_000, false, &utc).as_deref(),
            Some("12s")
        );
        assert_eq!(
            lane_words(None, Some(&timer), false, 0, false, &utc).as_deref(),
            Some("10s"),
            "a clock behind"
        );
        assert_eq!(
            lane_words(None, Some(&timer), true, 3_000, false, &utc),
            None,
            "ended, with no time said"
        );
        let ended = StepTime { ended: Some(started + 400), ..running };
        assert_eq!(lane_words(Some(&ended), None, true, 0, false, &utc).as_deref(), Some("<1s"));
        let clock =
            if crate::transcript::clock_format().contains('p') { "3:42:05" } else { "15:42:05" };
        assert_eq!(lane_words(Some(&ended), None, true, 0, true, &utc).as_deref(), Some(clock));
        let tip = lane_tip(
            &StepTime { ended: Some(started + 9_000), ran_ms: Some(1_000), ..running },
            &utc,
        );
        assert!(tip.contains("12s into the turn"), "{tip}");
        assert!(tip.contains("Ran 1s, after 8s waiting for permission"), "{tip}");
        // An open step's Timing: the same, on one line, with what it took when no wait is said.
        let timing = timing_words(
            &StepTime { ended: Some(started + 9_000), ran_ms: Some(1_000), ..running },
            &utc,
        );
        assert!(timing.contains(" · Ran 1s, after 8s waiting for permission"), "{timing}");
        assert!(!timing.contains("Took"), "the wait apart says what it took: {timing}");
        let timing = timing_words(&StepTime { ended: Some(started + 9_000), ..running }, &utc);
        assert!(timing.starts_with("Started ") && timing.ends_with(" · Took 9s"), "{timing}");
    }

    /// Steps' span runs from the first start to the last end, open while one still runs.
    #[test]
    fn a_groups_span_runs_from_its_first_start_to_its_last_end() {
        let at = |started, ended| StepTime { started, ended, ran_ms: None, turn_started: None };
        let steps = [at(5_000, Some(9_000)), at(2_000, Some(4_000)), at(6_000, Some(12_000))];
        assert_eq!(span(&steps, false).map(|s| (s.started, s.ended)), Some((2_000, Some(12_000))));
        assert_eq!(span(&steps, true).map(|s| s.ended), Some(None));
        assert_eq!(span(&[], false), None);
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
