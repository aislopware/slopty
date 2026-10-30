//! A subagent, a nested step of its parent's trace (`docs/decisions/inspector.md`, "A
//! subagent is a nested step, opened in place").
//!
//! A spawn is one row like any call: its kind and task; while it runs, a heartbeat from its own
//! records ("Haiku 4.5 · Read parser.rs · 12 tools") with "quiet 3m" once they stop for a while;
//! once done, the first line of what it answered; its figures and its status. Nothing opens it by
//! itself. A press splices its steps in under it, one level in, in the same list
//! ([`Transcript::nest`]): its brief first, folded to three lines, then its steps in the parent's
//! own rows, its report last; another press takes them out again. Enter, or its Open, drills in:
//! the inspector pushes its trace on a stack of its own (`inspector.rs`), and the terminal, the
//! sidebar and the prompt box's mirror stay where they were. Subagents started
//! together share one line over their rows, "3 Explore agents · 2 working · 1m 12s".

use std::collections::{HashMap, HashSet};

use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, KeyboardButton, SharedString, div, px};
use slopty_view_doc::{SubagentProgress, SubagentStatus};

use super::{ToolItem, Transcript, subagent_tab_title};
use crate::motion::HoverFade as _;
use crate::theme::Theme;

/// The first line of what a run answered (its spawn call's result), its heading and
/// strong marks left out; none when it answered nothing (#23).
fn reply_line(tool: &ToolItem) -> Option<String> {
    let Some(super::ToolDetail::Output { lines, .. }) = tool.detail.as_deref() else {
        return None;
    };
    lines
        .iter()
        .map(|line| line.trim_start_matches('#').replace("**", "").trim().to_owned())
        .find(|line| !line.is_empty())
}

/// What a row's quiet Open takes in its line.
const QUIET_FOOTPRINT: f32 = 18.0;
/// A window narrower than this is a phone's (`shell/compact.rs`).
const PHONE_BELOW: f32 = 700.0;
/// A phone's side margin at a compact width (UIKit's `systemMinimumLayoutMargins`).
const PHONE_MARGIN: f32 = 16.0;
/// How far each level of nesting sits in from its parent's rows: the chevron's gutter, so a
/// child's chevrons and icons stand under its parent's title.
pub(super) const NEST_INDENT: f32 = 22.0;
/// The deepest level the rows move in by; deeper runs keep that indent (traces §2, a hard cap).
const NEST_LEVELS: u8 = 4;
/// A running subagent whose records have not moved for this long says for how long ("quiet
/// 3m"), as claude-code-log's live page does (traces §10.3).
const QUIET_AFTER_MS: i64 = 60_000;

/// A row's status word swaps as Amp's does (`AJ/46b413b6.js:10505-10535`): the new word
/// enters from 4 pt below over 350 ms as the old one leaves 4 pt up over 150 ms.
const STATUS_IN: crate::motion::MotionSpec =
    crate::motion::MotionSpec::new(350, crate::motion::EASE_OUT_QUINT);
const STATUS_OUT: crate::motion::MotionSpec =
    crate::motion::MotionSpec::new(150, crate::motion::EASE_OUT);
const STATUS_TRAVEL: f32 = 4.0;
/// Bound on remembered status words, as `roll_text`'s.
const STATUS_KEYS: usize = 256;

thread_local! {
    /// Each row's last status word, and the word it swaps from and since when, while it does.
    static STATUS: std::cell::RefCell<HashMap<SharedString, (SharedString, Option<(SharedString, std::time::Instant)>)>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Where `key`'s status swap stands at `now`, `text` its word: the word it leaves and how
/// far it has gone, `None` at rest. The first sight of a key never swaps; reduced motion never
/// does.
fn status_swap(
    key: &SharedString,
    text: &SharedString,
    reduced: bool,
    now: std::time::Instant,
) -> Option<(SharedString, f32)> {
    STATUS.with_borrow_mut(|words| {
        if words.len() >= STATUS_KEYS && !words.contains_key(key) {
            words.retain(|_, (_, swap)| swap.is_some());
        }
        let entry = words.entry(key.clone()).or_insert_with(|| (text.clone(), None));
        if entry.0 != *text {
            let from = std::mem::replace(&mut entry.0, text.clone());
            entry.1 = (!reduced).then_some((from, now));
        }
        let ms = entry
            .1
            .as_ref()
            .map(|(_, at)| now.saturating_duration_since(*at).as_secs_f32() * 1_000.0);
        match ms {
            Some(ms) if !reduced && ms < STATUS_IN.duration_ms as f32 => {
                entry.1.as_ref().map(|(from, _)| (from.clone(), ms))
            }
            _ => {
                entry.1 = None;
                None
            }
        }
    })
}

/// `text` as the row's status word, swapping in from the one it last said.
fn status_word(
    key: SharedString,
    text: SharedString,
    view: gpui::EntityId,
    cx: &mut gpui::App,
) -> gpui::Div {
    let (reduced, now) = (cx.reduce_motion(), crate::clock::now(cx));
    let Some((from, ms)) = status_swap(&key, &text, reduced, now) else {
        return div().min_w_0().truncate().child(text);
    };
    crate::motion::pulse_lease(view, cx);
    let arrived = STATUS_IN.progress(ms / STATUS_IN.duration_ms as f32);
    let left = STATUS_OUT.progress(ms / STATUS_OUT.duration_ms as f32);
    div()
        .relative()
        .min_w_0()
        .overflow_hidden()
        .child(
            div()
                .relative()
                .top(px(STATUS_TRAVEL * (1.0 - arrived)))
                .opacity(arrived)
                .truncate()
                .child(text),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(-STATUS_TRAVEL * left))
                .opacity(1.0 - left)
                .truncate()
                .child(from),
        )
}

/// `row` set in the tool rows' label type, as a call's row is: a subagent's row and its batch's
/// line read as steps among the calls.
fn label_type<E: Styled>(row: E) -> E {
    row.text_size(px(super::TOOL_LABEL_SIZE)).line_height(px(super::TOOL_LABEL_LINE_HEIGHT))
}

/// The lines of its brief a subagent opened in place shows until pressed.
pub(super) const TASK_LINES: usize = 3;

/// What the subagents of a transcript keep between frames.
#[derive(Default)]
pub(super) struct SubagentsState {
    /// The subagents open in place, by their row: the chat whose steps are spliced in under it.
    pub(super) open: HashMap<SharedString, String>,
    /// How deep each spliced row sits under the transcript's own, by row; a row of the
    /// transcript's own is not in it.
    pub(super) depths: HashMap<SharedString, u8>,
    /// The briefs of runs opened in place that the person pressed open past their first lines,
    /// by row.
    pub(super) briefs_open: HashSet<SharedString>,
    /// The runs open in place whose record has not arrived, "Loading its steps…" under their row.
    pub(super) loading: HashSet<SharedString>,
    /// The window's size as of the last frame.
    pub(super) window: gpui::Size<gpui::Pixels>,
}

/// A subagent as its row reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    /// Its task, as its parent's agent named it: the row's label.
    pub title: SharedString,
    /// Its kind, as the agent names it ("Explore").
    pub kind: Option<SharedString>,
    /// The model it runs on, in words ("Haiku 4.5").
    pub model: Option<SharedString>,
    /// Its run's chat, where Slopty keeps one: what opens in place and drills in.
    pub chat: Option<String>,
    pub running: bool,
    pub failed: bool,
    /// The state word ([`SubagentProgress::RUNNING`] and the rest).
    pub state: String,
    /// Its step while it runs, its outcome after.
    pub step: Option<String>,
    /// Its figures, time left out: "12 tools · 41k tokens".
    pub figures: String,
    /// When it began, for a running one's time.
    pub started_at: Option<i64>,
    /// How long it ran, once it says.
    pub duration_ms: Option<u64>,
    /// When its records last moved, while it runs.
    pub heard_at: Option<i64>,
    /// What it asks of the person, answered in its own TUI.
    pub needs_you: Option<String>,
}

/// A model's id in words: "claude-haiku-5-5" reads "Haiku 5.5", a dated
/// "claude-sonnet-4-5-20250929" "Sonnet 4.5", a bare "opus" "Opus"; an id of another maker's
/// stays as it is.
pub(crate) fn model_words(id: &str) -> String {
    let id = id.trim();
    let rest = id.strip_prefix("claude-").unwrap_or(id);
    let mut parts = rest.split('-');
    let family = parts.next().unwrap_or_default();
    if !matches!(family, "haiku" | "sonnet" | "opus" | "fable") {
        return id.to_owned();
    }
    let version: Vec<&str> = parts
        .take_while(|part| part.len() <= 2 && part.chars().all(|c| c.is_ascii_digit()))
        .collect();
    let mut chars = family.chars();
    let name: String =
        chars.next().map(|c| c.to_ascii_uppercase()).into_iter().chain(chars).collect();
    if version.is_empty() { name } else { format!("{name} {}", version.join(".")) }
}

impl Run {
    /// The subagent spawn call `tool` started: its progress where the view model gave it, else
    /// what the call itself carries.
    pub fn of(tool: &ToolItem) -> Self {
        let progress = tool.facts.as_ref().and_then(|facts| facts.subagent_progress.as_ref());
        let status_running = matches!(tool.subagent_status, Some(SubagentStatus::Running));
        let status_failed = matches!(tool.subagent_status, Some(SubagentStatus::Failed));
        let model = progress
            .and_then(|progress| progress.model.as_deref())
            .or_else(|| tool.call.subagent_model())
            .map(|model| SharedString::from(model_words(model)));
        let Some(progress) = progress else {
            return Self {
                title: subagent_tab_title(&tool.call),
                kind: None,
                model,
                chat: tool.subagent_ref.as_ref().map(ToString::to_string),
                running: status_running,
                failed: tool.is_error || status_failed,
                state: if status_running {
                    SubagentProgress::RUNNING
                } else if status_failed {
                    SubagentProgress::FAILED
                } else {
                    SubagentProgress::DONE
                }
                .to_owned(),
                step: (!status_running && !(tool.is_error || status_failed))
                    .then(|| reply_line(tool))
                    .flatten(),
                figures: String::new(),
                started_at: None,
                duration_ms: None,
                heard_at: None,
                needs_you: None,
            };
        };
        let step = if progress.running {
            progress.step.clone()
        } else if progress.state == SubagentProgress::DONE && !tool.is_error {
            // A run that went well says the first line of what it answered (#23): its
            // call's result, else its report as the view model words it, never a blank line.
            reply_line(tool).or_else(|| progress.outcome.clone())
        } else {
            progress.outcome.clone().or_else(|| progress.step.clone())
        };
        Self {
            title: subagent_tab_title(&tool.call),
            kind: progress.agent_type.clone().filter(|kind| !kind.is_empty()).map(Into::into),
            model,
            chat: progress
                .chat_id
                .clone()
                .or_else(|| tool.subagent_ref.as_ref().map(ToString::to_string)),
            running: progress.running,
            failed: tool.is_error || progress.state == SubagentProgress::FAILED,
            state: progress.state.clone(),
            step,
            figures: progress.figures.clone(),
            started_at: progress.started_at,
            duration_ms: progress.duration_ms,
            heard_at: progress.heard_at,
            needs_you: progress.needs_you.clone(),
        }
    }

    /// How long it has run at `now`, or ran, in ms, once known.
    fn elapsed_ms(&self, now: i64) -> Option<u64> {
        if self.running {
            self.started_at.map(|start| u64::try_from(now.saturating_sub(start)).unwrap_or(0))
        } else {
            self.duration_ms
        }
    }

    /// Its facts at `now`, after its step: while it runs, its heartbeat's model and figures,
    /// "quiet 3m" once its records stopped a while, and how long it has run; once done, its model,
    /// figures and how long it took.
    pub fn facts_at(&self, now: i64) -> String {
        let quiet = self
            .heard_at
            .filter(|_| self.running && self.needs_you.is_none())
            .map(|heard| now.saturating_sub(heard))
            .filter(|ms| *ms >= QUIET_AFTER_MS)
            // In whole minutes: it moves once a minute, not every second.
            .map(|ms| u64::try_from(ms - ms % QUIET_AFTER_MS).unwrap_or(0))
            .map(|ms| format!("quiet {}", slopty_view_doc::duration_words(ms)));
        [
            self.model.as_ref().map(ToString::to_string),
            Some(self.figures.clone()).filter(|figures| !figures.is_empty()),
            quiet,
            self.elapsed_ms(now).map(slopty_view_doc::duration_words),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// Its one line of facts, as a drilled-in trace heads with it: kind, model, status, figures
    /// and time ("Explore · Haiku 4.5 · Done · 3 tools · 14.6k tokens · 4s").
    pub fn summary_at(&self, now: i64) -> String {
        let status = match self.state.as_str() {
            _ if self.needs_you.is_some() => "Needs you",
            SubagentProgress::RUNNING => "Running",
            SubagentProgress::WAITING => "Waiting",
            SubagentProgress::NEEDS_YOU => "Needs you",
            SubagentProgress::FAILED => "Failed",
            SubagentProgress::STOPPED => "Stopped",
            _ => "Done",
        };
        [
            self.kind.as_ref().map(ToString::to_string),
            self.model.as_ref().map(ToString::to_string),
            Some(status.to_owned()),
            Some(self.figures.clone()).filter(|figures| !figures.is_empty()),
            self.elapsed_ms(now).map(slopty_view_doc::duration_words),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// Its status mark: the ring turning while it works, closed amber while it waits on the
    /// person, the tick once it answered, the red `!` if it failed; one stopped has none.
    pub fn mark(&self) -> Option<crate::loaders::status_mark::MarkState> {
        use crate::loaders::status_mark::MarkState;
        match self.state.as_str() {
            _ if self.needs_you.is_some() || self.state == SubagentProgress::NEEDS_YOU => {
                Some(MarkState::Waiting)
            }
            _ if self.running => Some(MarkState::Working),
            SubagentProgress::DONE => Some(MarkState::Done),
            SubagentProgress::FAILED => Some(MarkState::Failed),
            _ => None,
        }
    }
}

/// The one line over subagents started together (t3code §2.3): how many and of what kind,
/// how many still work, how many failed, and the time across them from the first start to the
/// last end, live while any runs: "3 Explore agents · 2 working · 1m 12s", then "3 Explore agents
/// finished · 1 failed · 2m 5s".
pub(super) fn batch_words(runs: &[Run], now: i64) -> String {
    let kinds: Vec<Option<&SharedString>> = runs.iter().map(|run| run.kind.as_ref()).collect();
    let noun = match kinds.first().copied().flatten() {
        Some(kind) if kinds.iter().all(|k| *k == Some(kind)) => format!("{kind} agents"),
        _ => "agents".to_owned(),
    };
    let working = runs.iter().filter(|run| run.running).count();
    let failed = runs.iter().filter(|run| run.failed).count();
    let start = runs.iter().filter_map(|run| run.started_at).min();
    let end = if working > 0 {
        Some(now)
    } else {
        runs.iter()
            .filter_map(|run| Some(run.started_at? + i64::try_from(run.duration_ms?).ok()?))
            .max()
    };
    let span = start.zip(end).map(|(start, end)| {
        slopty_view_doc::duration_words(u64::try_from(end.saturating_sub(start)).unwrap_or(0))
    });
    let head = if working > 0 {
        format!("{} {noun} · {working} working", runs.len())
    } else {
        format!("{} {noun} finished", runs.len())
    };
    [Some(head), (failed > 0).then(|| format!("{failed} failed")), span]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The words that started a run: its brief, as its parent's agent wrote it, quiet, and cut to
/// `lines` until pressed ([`TASK_LINES`]; the whole of it in a drilled-in trace).
pub(super) fn task_words(
    row: &SharedString,
    text: &SharedString,
    lines: Option<usize>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let words = div()
        .id(SharedString::from(format!("{row}-task")))
        .role(gpui::Role::Paragraph)
        .aria_label(SharedString::from(format!("Task: {text}")))
        .w_full()
        .min_w_0()
        .pl(px(Theme::SPACE_SM))
        .border_l_2()
        .border_color(crate::theme::hairline(0.10))
        .text_size(crate::typography::ui_rems(12.5))
        .line_height(crate::typography::ui_rems(18.0))
        .text_color(theme.text_muted);
    match lines {
        Some(lines) => words.line_clamp(lines).text_ellipsis(),
        None => words,
    }
    .child(text.clone())
}

impl Transcript {
    /// The window, as last drawn, is a phone's: a touch screen narrower than
    /// [`PHONE_BELOW`], where the shell puts its navigation bar for the titlebar.
    pub(super) fn on_phone(&self) -> bool {
        let width = f32::from(self.subagents.window.width);
        crate::shell::touch_screen() && width > 0.0 && width < PHONE_BELOW
    }

    /// The side gutter the rows keep in their pane: on a phone the platform's own
    /// margin (UIKit's 16 pt at a compact width), so the timeline lines up with the sheet's
    /// words; elsewhere the reading column's 48.
    pub(super) fn row_inset(&self) -> f32 {
        if self.on_phone() { PHONE_MARGIN } else { 48.0 }
    }

    /// How deep row `row` sits under the transcript's own rows: 0 for its own.
    pub(super) fn depth_of(&self, row: &SharedString) -> u8 {
        self.subagents.depths.get(row).copied().unwrap_or(0)
    }

    /// How far a row at `depth` moves in.
    pub(super) fn nest_inset(depth: u8) -> f32 {
        f32::from(depth.min(NEST_LEVELS)) * NEST_INDENT
    }

    /// Whether the subagent of row `row` is open in place.
    pub(crate) fn spawn_open(&self, row: &SharedString) -> bool {
        self.subagents.open.contains_key(row)
    }

    /// The subagent of row `row`, as its row reads it now.
    pub(crate) fn spawn_run(&self, row: &str) -> Option<Run> {
        self.rows.iter().find(|r| r.id.as_ref() == row).and_then(|r| match &r.kind {
            super::RowKind::Spawn { tool } => Some(Run::of(tool)),
            _ => None,
        })
    }

    /// The row of the subagent whose run is chat `chat`, where this transcript shows one.
    pub(crate) fn spawn_row_of(&self, chat: &str) -> Option<SharedString> {
        self.rows.iter().find_map(|row| match &row.kind {
            super::RowKind::Spawn { tool } if Run::of(tool).chat.as_deref() == Some(chat) => {
                Some(row.id.clone())
            }
            _ => None,
        })
    }

    /// Whether the run `chat` is open in place here, so its feed must stay watched.
    pub(crate) fn watches(&self, chat: &str) -> bool {
        self.subagents.open.values().any(|open| open == chat)
    }

    /// Open the subagent of row `row`, run `chat`, in place, or take its steps out again. Its
    /// feed is watched while it is open; the rows above it stay where they were.
    pub(super) fn toggle_spawn(&mut self, row: SharedString, chat: String, cx: &mut Context<Self>) {
        if self.subagents.open.remove(&row).is_some() {
            self.subagents.loading.remove(&row);
            // Its own subagents open in place close with it: the rows under it a level deeper.
            let depth = self.depth_of(&row);
            let under: Vec<SharedString> = self
                .rows
                .iter()
                .skip_while(|r| r.id != row)
                .skip(1)
                .take_while(|r| self.depth_of(&r.id) > depth)
                .map(|r| r.id.clone())
                .collect();
            let mut gone = vec![chat];
            for key in under {
                self.subagents.briefs_open.remove(&key);
                self.subagents.loading.remove(&key);
                if let Some(chat) = self.subagents.open.remove(&key) {
                    gone.push(chat);
                }
            }
            self.state.update(cx, |state, _| {
                for chat in &gone {
                    state.unwatch_subagent_doc(chat);
                }
            });
        } else {
            self.subagents.open.insert(row.clone(), chat.clone());
            self.state.update(cx, |state, cx| state.watch_subagent_doc(chat, cx));
            self.reveal_opened(row);
        }
        self.last_source = None;
        self.sync(cx);
        cx.notify();
    }

    /// Take every subagent's steps out: the transcript shows another chat now, or goes.
    pub(super) fn close_spawns(&mut self, cx: &mut gpui::App) {
        let open: Vec<String> = self.subagents.open.drain().map(|(_, chat)| chat).collect();
        self.subagents.depths.clear();
        self.subagents.briefs_open.clear();
        self.subagents.loading.clear();
        if open.is_empty() {
            return;
        }
        self.state.update(cx, |state, _| {
            for chat in &open {
                state.unwatch_subagent_doc(chat);
            }
        });
    }

    /// Hand over the runs open in place without unwatching them: the inspector, which holds
    /// other transcripts over the same feeds, decides which still serve one (`inspector.rs`).
    pub(crate) fn release_spawns(&mut self) -> Vec<String> {
        self.subagents.depths.clear();
        self.subagents.briefs_open.clear();
        self.subagents.loading.clear();
        self.subagents.open.drain().map(|(_, chat)| chat).collect()
    }

    /// `rows` with the steps of each subagent open in place spliced in under its row, a
    /// level deeper, as deep as they open: the brief, the steps and the report of its run's
    /// record, in the same rows as the parent's. Each spliced row's depth is kept for its inset.
    pub(super) fn nest(
        &mut self,
        rows: Vec<super::Row>,
        state: &crate::state::AppState,
        depth: u8,
        out: &mut Vec<super::Row>,
    ) {
        for row in rows {
            let open = match &row.kind {
                super::RowKind::Spawn { .. } => self.subagents.open.get(&row.id).cloned(),
                _ => None,
            };
            if depth > 0 {
                self.subagents.depths.insert(row.id.clone(), depth);
            }
            let row_id = row.id.clone();
            out.push(row);
            // A run's record never holds its own spawn; the cap stops a malformed one anyway.
            let Some(chat) = open.filter(|_| depth < u8::MAX - 1) else {
                continue;
            };
            let mut child = Vec::new();
            for entry in state.sub_transcript(&chat) {
                child.extend(self.rows_for(entry));
            }
            if child.is_empty() {
                self.subagents.loading.insert(row_id.clone());
                // Its record is on its way: a quiet line holds its place under the row.
                child.push(super::Row {
                    id: SharedString::from(format!("{row_id}#loading")),
                    version: 0,
                    turn_start: false,
                    kind: super::RowKind::Marker { text: "Loading its steps…".into() },
                    entry_id: row_id.clone(),
                    timestamp: None,
                    copy_text: None,
                    compact_fold: None,
                });
            } else if self.subagents.loading.remove(&row_id) {
                // Its record arrived: what the person opened comes into view.
                self.reveal_opened(row_id.clone());
            }
            self.nest(child, state, depth + 1, out);
        }
    }

    /// The row of subagent `tool`, `row_id` its key: one line at a step's height, its
    /// chevron opening its steps in place, Enter or its Open drilling in.
    pub(super) fn render_spawn(
        &mut self,
        row_id: &SharedString,
        tool: &ToolItem,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::loaders::status_mark::{MARK_SMALL, StatusMark};
        let run = Run::of(tool);
        let open = self.spawn_open(row_id);
        if run.running {
            self.tick_soon(cx);
        }
        let now = self.clock.timestamp_millis();
        let facts = run.facts_at(now);
        let hover = format!("{row_id}-hover");
        let shown = crate::motion::revealed_by_hover(&hover).max(if crate::shell::touch_screen() {
            1.0
        } else {
            0.0
        });
        let step = match (&run.needs_you, &run.step) {
            (Some(ask), _) => Some(format!("Needs you · {ask}")),
            (None, step) => step.clone(),
        };
        let step_tint = if run.needs_you.is_some() {
            theme.warning
        } else if run.failed {
            theme.danger
        } else {
            theme.text_muted
        };
        let view = cx.entity_id();
        // Its chevron's gutter, kept when it has no run to open so its icon lines up with its
        // neighbours'.
        let chevron = div().w(px(NEST_INDENT)).h(px(18.0)).flex_none().relative().children(
            run.chat.is_some().then(|| {
                crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                    .absolute()
                    .left(px(super::ACTIVITY_TRUNK_X - 7.0))
                    .top(px(2.0))
                    .size(px(14.0))
                    .with_transformation(gpui::Transformation::rotate(gpui::radians(if open {
                        0.0
                    } else {
                        -std::f32::consts::FRAC_PI_2
                    })))
                    .text_color(theme.text_muted)
                    .opacity(shown.max(if open { 1.0 } else { 0.0 }))
            }),
        );
        let drill = run.chat.clone().map(|chat| {
            let fade = format!("{row_id}-open-hover");
            let row = row_id.clone();
            crate::surface_chrome::press_target(
                SharedString::from(format!("{row_id}-open")),
                QUIET_FOOTPRINT,
            )
            .role(gpui::Role::Button)
            .aria_label("Open subagent")
            .rounded(px(6.0))
            .cursor_pointer()
            .opacity(shown)
            .tab_index(0)
            .focus_visible({
                let ring = crate::surface_chrome::ring(theme, crate::surface_chrome::RING_OFFSET);
                move |s| s.opacity(1.0).outline_ring(ring)
            })
            .bg(crate::motion::hover_blend(
                &fade,
                gpui::transparent_black(),
                crate::theme::wash(0.11),
            ))
            .hover_fade(fade)
            .tooltip(crate::settings::widgets::text_tooltip("Open subagent"))
            .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
            .child(
                crate::icons::icon(crate::icons::ARROW_UP_RIGHT)
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            )
            // A press drills in and gives this control no keyboard: it goes with the trace it
            // is in (the inspector moves the keyboard to its way back instead).
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                cx.emit(super::SubagentDrill { chat_id: chat.clone(), row: row.clone() });
            }))
        });
        let mark = run.mark().map(|state| {
            StatusMark::new(SharedString::from(format!("subagent-mark-{row_id}")), state)
                .size(MARK_SMALL)
                .into_any_element()
        });
        let value = [step.clone(), Some(facts.clone()).filter(|f| !f.is_empty())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let toggle = (row_id.clone(), run.chat.clone());
        label_type(crate::surface_chrome::key_stop_inset(div().id(row_id.clone()), theme))
            .hover_fade(hover)
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Subagent {}", run.title)))
            .aria_value(SharedString::from(value))
            .when(run.chat.is_some(), |row| row.aria_expanded(open))
            .rounded(px(4.0))
            .h(px(super::tool_group_header_height()))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .pr(px(4.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let (row, chat) = toggle.clone();
                let Some(chat) = chat else { return };
                cx.stop_propagation();
                // Enter drills in, as a trace viewer's does; a press or Space opens it in place.
                if matches!(event, ClickEvent::Keyboard(key) if key.button == KeyboardButton::Enter)
                {
                    cx.emit(super::SubagentDrill { chat_id: chat, row });
                } else {
                    this.toggle_spawn(row, chat, cx);
                }
            }))
            .child(chevron)
            .child(
                crate::icons::icon(crate::icons::BOT)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .children(
                run.kind
                    .clone()
                    .map(|kind| div().flex_none().text_color(theme.text_faint).child(kind)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if run.failed { theme.danger } else { theme.text })
                    .child(run.title.clone()),
            )
            .children(step.map(|step| {
                // The word swaps as it changes (Amp).
                status_word(SharedString::from(format!("{row_id}-status")), step.into(), view, cx)
                    .min_w_0()
                    .flex_shrink(1.0)
                    .text_color(step_tint)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(facts)),
            )
            .children(drill)
            .children(mark.map(|mark| div().flex_none().child(mark)))
            .into_any_element()
    }

    /// The one line over subagents started together, `tools` their spawns.
    pub(super) fn render_spawns(
        &mut self,
        row_id: &SharedString,
        tools: &[ToolItem],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let runs: Vec<Run> = tools.iter().map(Run::of).collect();
        if runs.iter().any(|run| run.running) {
            self.tick_soon(cx);
        }
        let words = batch_words(&runs, self.clock.timestamp_millis());
        let failed = runs.iter().any(|run| run.failed);
        label_type(div().id(SharedString::from(format!("{row_id}-batch"))))
            .role(gpui::Role::Heading)
            .aria_label(SharedString::from(words.clone()))
            .h(px(super::tool_group_header_height()))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(div().w(px(NEST_INDENT)).flex_none())
            .child(
                crate::icons::icon(crate::icons::BOT)
                    .size(px(13.0))
                    .flex_none()
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if failed { theme.danger } else { theme.text_muted })
                    .child(SharedString::from(words)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_view_doc::ToolFacts;
    use slopty_view_proto::ToolCall;

    use super::*;

    fn spawn(progress: Option<SubagentProgress>) -> ToolItem {
        ToolItem {
            part_id: "t1".into(),
            call: ToolCall::Unknown {
                name: "Agent: Count lines".into(),
                input: Some(serde_json::json!({ "description": "Count lines" })),
            },
            is_error: false,
            resolved: true,
            detail: None,
            invocation: None,
            output_ref: None,
            output_bytes: None,
            output_text: None,
            diff_ref: None,
            subagent_ref: Some("child".into()),
            subagent_status: Some(SubagentStatus::Running),
            kind: super::super::ToolItemKind::Call,
            images: std::sync::Arc::new([]),
            facts: progress.map(|progress| {
                std::sync::Arc::new(ToolFacts {
                    subagent_progress: Some(progress),
                    ..ToolFacts::default()
                })
            }),
        }
    }

    /// A running subagent reads its heartbeat (its model, its step, its figures) with the time
    /// it has run, and says "quiet" once its records stopped a minute; settled, its outcome and
    /// the time it took, and one line of facts for a drilled-in trace.
    #[test]
    fn a_run_reads_its_heartbeat_then_its_outcome() {
        let mut progress = SubagentProgress {
            agent_type: Some("Explore".into()),
            model: Some("claude-haiku-5-5".into()),
            running: true,
            state: SubagentProgress::RUNNING.into(),
            step: Some("Read parser.rs".into()),
            tool_uses: Some(12),
            tokens: Some(41_000),
            started_at: Some(1_000),
            heard_at: Some(100_000),
            figures: "12 tools · 41k tokens".into(),
            chat_id: Some("child-chat".into()),
            ..SubagentProgress::default()
        };
        let run = Run::of(&spawn(Some(progress.clone())));
        assert_eq!(run.title.as_ref(), "Count lines");
        assert_eq!(run.kind.as_deref(), Some("Explore"));
        assert_eq!(run.chat.as_deref(), Some("child-chat"));
        assert_eq!(run.step.as_deref(), Some("Read parser.rs"));
        assert_eq!(run.facts_at(124_000), "Haiku 5.5 · 12 tools · 41k tokens · 2m 3s");
        assert_eq!(run.facts_at(281_000), "Haiku 5.5 · 12 tools · 41k tokens · quiet 3m · 4m 40s");
        progress.running = false;
        progress.state = SubagentProgress::DONE.into();
        progress.duration_ms = Some(65_000);
        progress.outcome = Some("4".into());
        let settled = Run::of(&spawn(Some(progress.clone())));
        // Done, it says what it answered (#23): the view model's outcome is the report's
        // first line where the call carries no result, as Claude Code's spawn calls do.
        assert_eq!(settled.step.as_deref(), Some("4"));
        assert_eq!(
            settled.facts_at(999_999),
            "Haiku 5.5 · 12 tools · 41k tokens · 1m 5s",
            "no quiet once done"
        );
        assert_eq!(
            settled.summary_at(999_999),
            "Explore · Haiku 5.5 · Done · 12 tools · 41k tokens · 1m 5s"
        );
        let mut answered = spawn(Some(progress.clone()));
        answered.detail =
            super::super::tool_detail(Some("\n## **4** lines\n\nagentId: a1\n"), None, None)
                .map(std::sync::Arc::new);
        assert_eq!(Run::of(&answered).step.as_deref(), Some("4 lines"));
        // A run that failed or was stopped still says so.
        progress.state = SubagentProgress::FAILED.into();
        progress.outcome = Some("Failed: out of turns".into());
        let mut failed = spawn(Some(progress.clone()));
        failed.detail = answered.detail.clone();
        assert_eq!(Run::of(&failed).step.as_deref(), Some("Failed: out of turns"));
        progress.state = SubagentProgress::STOPPED.into();
        progress.outcome = Some("Stopped".into());
        let stopped = Run::of(&spawn(Some(progress)));
        assert_eq!(stopped.step.as_deref(), Some("Stopped"));
        assert_eq!(stopped.mark(), None, "a run stopped leaves no mark");
    }

    /// Without progress from the view model, the row stands on what the call carries.
    #[test]
    fn a_run_without_progress_names_its_chat() {
        let run = Run::of(&spawn(None));
        assert!(run.running);
        assert_eq!(run.chat.as_deref(), Some("child"));
        assert_eq!(run.facts_at(5), "");
    }

    /// A model's id reads as its name and version.
    #[test]
    fn a_model_reads_as_its_name() {
        assert_eq!(model_words("claude-haiku-5-5"), "Haiku 5.5");
        assert_eq!(model_words("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(model_words("opus"), "Opus");
        assert_eq!(model_words("gpt-5.1-codex"), "gpt-5.1-codex");
    }

    /// Subagents started together read on one line: how many of what kind, how many still work,
    /// the time across them; then that they finished, and how many failed.
    #[test]
    fn a_batch_says_who_still_works() {
        let run = |kind: &str, running: bool, failed: bool, started: i64, took: u64| Run {
            title: "x".into(),
            kind: Some(kind.to_owned().into()),
            model: None,
            chat: None,
            running,
            failed,
            state: String::new(),
            step: None,
            figures: String::new(),
            started_at: Some(started),
            duration_ms: (!running).then_some(took),
            heard_at: None,
            needs_you: None,
        };
        let live = [
            run("Explore", true, false, 0, 0),
            run("Explore", true, false, 2_000, 0),
            run("Explore", false, false, 1_000, 5_000),
        ];
        assert_eq!(batch_words(&live, 72_000), "3 Explore agents · 2 working · 1m 12s");
        let done =
            [run("Explore", false, false, 0, 125_000), run("Plan", false, true, 1_000, 10_000)];
        assert_eq!(batch_words(&done, 999_999), "2 agents finished · 1 failed · 2m 5s");
    }

    /// A row's status word swaps as Amp's does: the first sight stands still, a new word
    /// swaps in from the last for 350 ms, and reduced motion swaps at once.
    #[test]
    fn a_status_word_swaps_from_the_last() {
        let key = SharedString::from("run-status");
        let start = std::time::Instant::now();
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        assert_eq!(status_swap(&key, &"Thinking".into(), false, start), None, "first sight");
        let swap = status_swap(&key, &"Running tools".into(), false, at(10));
        assert_eq!(swap, Some(("Thinking".into(), 0.0)), "it swaps from the last word");
        let mid =
            status_swap(&key, &"Running tools".into(), false, at(110)).expect("still swapping");
        assert!((mid.1 - 100.0).abs() < 0.5, "{mid:?}");
        assert_eq!(status_swap(&key, &"Running tools".into(), false, at(10 + 350)), None, "landed");
        assert_eq!(status_swap(&key, &"Thinking".into(), true, at(400)), None, "reduced: at once");
    }
}
