//! slopty: a subagent's card, where its parent's call started it (Slopty's own; zeron's spawn
//! chip opens a right-pane tab instead).
//!
//! A subagent lives only in its parent's chat (`docs/decisions/ui.md`, "Subagents live in their
//! parent's chat"): this card, the Background tray above the composer, and the drill-in. The
//! card is zeron's chip anatomy: an icon tile, the task as its label, its kind and model faint
//! on the right; under it the subagent's step and figures, live while it runs and settled to
//! its outcome after. A click (or ↵) opens the child's transcript in place, capped in height
//! and scrolling on its own, following its end while it runs; the newest running child of the
//! turn under way opens itself while it is what the parent waits on, and closes once the turn
//! moves on, unless the person opened or closed it. Open (⌘↵) drills into the child as the
//! session view; Stop (⌘.) stops it; Escape closes the card.

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, Context, Entity, FocusHandle, SharedString, Subscription, div, prelude::*,
    px,
};
use slopty_view_doc::{SubagentProgress, SubagentStatus};

use super::{CHIP_CARD_HEIGHT, ToolItem, Transcript, TranscriptEvent, subagent_tab_title};
use crate::theme::Theme;
use crate::motion::HoverFade as _;

/// The step line under a card's header.
const STEP_LINE_HEIGHT: f32 = 20.0;
/// slopty: what a card's quiet control (Stop, Open) takes in its header's row.
const QUIET_FOOTPRINT: f32 = 18.0;
/// The least and most room a card's child transcript may take; it takes only its rows' height
/// within it.
const CHILD_MIN_HEIGHT: f32 = 160.0;
const CHILD_MAX_HEIGHT: f32 = 480.0;
/// The share of the window's height a child transcript opens to, within those.
const CHILD_VIEWPORT_SHARE: f32 = 0.4;
/// A window narrower than this is a phone's (`shell/compact.rs`): a card there drills in.
const PHONE_BELOW: f32 = 700.0;
/// Where a card's lines start: past its icon tile, under its title. Its child's rows start there
/// too.
pub(super) const CHILD_INSET: f32 = Theme::SPACE_SM + 18.0 + Theme::SPACE_SM;
/// The lines of its task a card's open run shows.
pub(super) const TASK_LINES: usize = 3;

/// What subagent cards keep between frames.
#[derive(Default)]
pub(super) struct SubagentsState {
    /// Cards the person opened (`true`) or closed: their word wins over the frontier rule.
    toggled: HashMap<SharedString, bool>,
    /// Child transcripts open in place, by the child's chat.
    open: HashMap<String, InlineChild>,
    /// Each card's keyboard focus, by card.
    focus: HashMap<SharedString, FocusHandle>,
    /// Tray rows asked to stop, until their card settles.
    stopping: HashSet<String>,
    /// Each card's run, as open as it was last drawn: whether it was open, and its height then.
    drawn: HashMap<SharedString, (bool, f32)>,
    /// Cards opening or closing: the height each moves from, and since when.
    moving: HashMap<SharedString, (f32, std::time::Instant)>,
    /// The window's size as of the last frame.
    pub(super) window: gpui::Size<gpui::Pixels>,
}

/// A child's transcript, open in its card.
struct InlineChild {
    transcript: Entity<Transcript>,
    /// Its rows' height as last laid out, which the card takes up to its room.
    rows: std::rc::Rc<std::cell::Cell<Option<f32>>>,
    _events: Subscription,
}

/// A subagent as its card reads it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Card {
    /// The task, the card's label.
    pub title: SharedString,
    /// Its kind and model, faint on the right: "Explore · haiku".
    pub kind: Option<SharedString>,
    /// Its run's chat, where Slopty keeps one: what opens in place and drills in.
    pub chat: Option<String>,
    /// Its tray row, which Stop stops.
    pub row: Option<String>,
    pub running: bool,
    pub failed: bool,
    /// The state word ([`SubagentProgress::RUNNING`] and the rest).
    pub state: String,
    /// The step line's left: its step while it runs, its outcome after.
    pub step: Option<String>,
    /// The step line's right, time left out: "12 tools · 41k tokens".
    pub figures: String,
    /// When it began, for a running card's elapsed time.
    pub started_at: Option<i64>,
    /// How long it ran, once it says.
    pub duration_ms: Option<u64>,
    /// What it asks of the person, answered in the parent's dock.
    pub needs_you: Option<String>,
}

impl Card {
    /// The card of spawn call `tool`: its progress where the view model gave it, else what the
    /// chip itself carries.
    pub fn of(tool: &ToolItem) -> Self {
        let progress = tool.facts.as_ref().and_then(|facts| facts.subagent_progress.as_ref());
        let status_running = matches!(tool.subagent_status, Some(SubagentStatus::Running));
        let status_failed = matches!(tool.subagent_status, Some(SubagentStatus::Failed));
        let model = tool.call.subagent_model().map(str::to_owned);
        let Some(progress) = progress else {
            return Self {
                title: subagent_tab_title(&tool.call),
                kind: model.map(SharedString::from),
                chat: tool.subagent_ref.as_ref().map(ToString::to_string),
                row: tool.subagent_ref.as_ref().map(|chat| format!("chat:{chat}")),
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
                step: None,
                figures: String::new(),
                started_at: None,
                duration_ms: None,
                needs_you: None,
            };
        };
        let kind = [progress.agent_type.clone(), progress.model.clone().or(model)]
            .into_iter()
            .flatten()
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        let step = if progress.running {
            progress.step.clone()
        } else {
            progress.outcome.clone().or_else(|| progress.step.clone())
        };
        Self {
            title: subagent_tab_title(&tool.call),
            kind: (!kind.is_empty()).then(|| kind.into()),
            chat: progress
                .chat_id
                .clone()
                .or_else(|| tool.subagent_ref.as_ref().map(ToString::to_string)),
            row: progress.row.clone(),
            running: progress.running,
            failed: tool.is_error || progress.state == SubagentProgress::FAILED,
            state: progress.state.clone(),
            step,
            figures: progress.figures.clone(),
            started_at: progress.started_at,
            duration_ms: progress.duration_ms,
            needs_you: progress.needs_you.clone(),
        }
    }

    /// The step line's right at `now`: its figures and how long it has run.
    pub fn figures_at(&self, now: i64) -> String {
        let elapsed = if self.running {
            self.started_at
                .map(|start| u64::try_from(now.saturating_sub(start)).unwrap_or(0))
        } else {
            self.duration_ms
        };
        let elapsed = elapsed.map(slopty_view_doc::duration_words);
        [Some(self.figures.clone()).filter(|f| !f.is_empty()), elapsed]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

impl SubagentsState {
    /// Close every child open in place: the transcript shows another chat now, or goes.
    pub fn close_all(&mut self, state: &Entity<crate::state::AppState>, cx: &mut gpui::App) {
        let children: Vec<String> = self.open.drain().map(|(chat, _)| chat).collect();
        self.toggled.clear();
        self.stopping.clear();
        self.drawn.clear();
        self.moving.clear();
        if children.is_empty() {
            return;
        }
        state.update(cx, |state, _| {
            for chat in &children {
                state.unwatch_subagent_doc(chat);
            }
        });
    }

    /// The child transcripts open in place now (tests).
    #[cfg(test)]
    pub fn open_children(&self) -> Vec<String> {
        let mut open: Vec<String> = self.open.keys().cloned().collect();
        open.sort();
        open
    }

    /// The most room a child transcript open in a card takes: a share of the window's height.
    pub(super) fn child_room(&self) -> f32 {
        let viewport = f32::from(self.window.height);
        (viewport * CHILD_VIEWPORT_SHARE).clamp(CHILD_MIN_HEIGHT, CHILD_MAX_HEIGHT)
    }
}

/// The words that started a run: its task, as its parent's agent wrote it, quiet, and cut to
/// `lines` (its card's [`TASK_LINES`]; the whole of it drilled into).
pub(super) fn task_words(row: &SharedString, text: &SharedString, lines: Option<usize>, theme: &Theme) -> AnyElement {
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
    .into_any_element()
}

impl Transcript {
    // slopty: S2's Background tray asks to show the call a row came from.
    /// Scroll to the call [`crate::background_tray::CallReveal`] names, its subagent card open.
    pub(super) fn on_call_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = cx.try_global::<crate::background_tray::CallReveal>().cloned() else {
            return;
        };
        if self.chat_id.as_deref() != Some(ask.chat_id.as_str()) {
            return;
        }
        let tail = format!("#{}", ask.part_id);
        let found = self.rows.iter().enumerate().find_map(|(at, row)| match &row.kind {
            super::RowKind::ToolGroup { tools, .. } => tools
                .iter()
                .position(|tool| tool.part_id == ask.part_id)
                .map(|ix| (at, Some(format!("{}#s{ix}", row.id)))),
            _ => row.id.ends_with(&tail).then_some((at, None)),
        });
        let Some((at, card)) = found else { return };
        if let Some(key) = card {
            self.subagents.toggled.insert(SharedString::from(key), true);
            self.list.remeasure_items(at..at + 1);
        }
        self.scroll_to_row(at, cx);
        cx.notify();
    }

    /// Whether the card `key` of a child is open: the person's word, else the frontier rule.
    fn card_open(&self, key: &SharedString, frontier: bool) -> bool {
        self.subagents.toggled.get(key).copied().unwrap_or(frontier)
    }

    /// Open or close the card `key` of child `chat`.
    fn toggle_card(&mut self, key: SharedString, open_now: bool, cx: &mut Context<Self>) {
        let row = key.rsplit_once("#s").map_or("", |(row, _)| row).to_owned();
        self.subagents.toggled.insert(key, !open_now);
        // slopty: the card's row grows or shrinks by the run under it.
        self.row_resized(&row, cx);
    }

    /// The child `chat`'s transcript, open in place in a card of row `row`: watched from its first
    /// showing.
    fn inline_child(
        &mut self,
        row: &SharedString,
        chat: &str,
        live: bool,
        cx: &mut Context<Self>,
    ) -> (Entity<Transcript>, Option<f32>) {
        if let Some(child) = self.subagents.open.get(chat) {
            return (child.transcript.clone(), child.rows.get());
        }
        let state = self.state.clone();
        state.update(cx, |state, cx| state.watch_subagent_doc(chat.to_owned(), cx));
        let transcript = cx.new(|cx| Transcript::for_doc(state, chat.to_owned(), live, cx));
        if let Some(links) = self.workspace_link.clone() {
            transcript.update(cx, |child, _| child.set_workspace_link_handler(links));
        }
        // A card inside the child drills in, or reveals the tray, as one here does.
        let events = cx.subscribe(&transcript, |_, _, event: &TranscriptEvent, cx| {
            cx.emit(event.clone());
        });
        let rows = std::rc::Rc::default();
        let card = cx.weak_entity();
        transcript.update(cx, |child, _| child.fit_to(card, row.clone(), std::rc::Rc::clone(&rows)));
        self.subagents.open.insert(
            chat.to_owned(),
            InlineChild {
                transcript: transcript.clone(),
                rows,
                _events: events,
            },
        );
        (transcript, None)
    }

    /// Close the child `chat` open in place.
    fn close_child(&mut self, chat: &str, cx: &mut Context<Self>) {
        if self.subagents.open.remove(chat).is_some() {
            self.state
                .update(cx, |state, _| state.unwatch_subagent_doc(chat));
        }
    }

    /// Stop the subagent of tray row `row`.
    fn stop_subagent(&mut self, row: String, cx: &mut Context<Self>) {
        if self.subagents.stopping.insert(row.clone()) {
            self.stop_work(row, cx);
            cx.notify();
        }
    }

    /// The card of spawn call `tool`, the `ix`th of group row `row_id`. `frontier`: it is the
    /// newest running child of the turn under way, which opens itself. `rail`: it sits in a
    /// fold's rail (compact mode), where it never opens in place.
    pub(super) fn render_subagent_card(
        &mut self,
        row_id: &SharedString,
        ix: usize,
        tool: &ToolItem,
        frontier: bool,
        rail: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = Card::of(tool);
        let key = SharedString::from(format!("{row_id}#s{ix}"));
        let width = f32::from(self.subagents.window.width);
        let phone = width > 0.0 && width < PHONE_BELOW;
        let can_open_in_place = card.chat.is_some() && !rail && !phone;
        let open = can_open_in_place && self.card_open(&key, frontier && card.running);
        // Opening or closing, by the person or the frontier, the run slides from where it was
        // drawn: its height eases to its own, or to none.
        let moment = crate::clock::now(cx);
        let reduced = crate::motion::reduced_motion(cx);
        let drawn = self.subagents.drawn.get(&key).copied();
        if let Some((was_open, height)) = drawn
            && was_open != open
            && !reduced
        {
            self.subagents.moving.insert(key.clone(), (height, moment));
        }
        let moving = self.subagents.moving.get(&key).copied();
        // A card the frontier opened closes with it, its child let go once it slid shut.
        if let Some(chat) = card.chat.as_deref()
            && !open
            && moving.is_none()
            && self.subagents.open.contains_key(chat)
        {
            self.close_child(chat, cx);
        }
        if !card.running && let Some(row) = &card.row {
            self.subagents.stopping.remove(row);
        }
        if card.running {
            self.tick_soon(cx);
        }
        let stopping = card.row.as_ref().is_some_and(|row| self.subagents.stopping.contains(row));
        let focus = self
            .subagents
            .focus
            .entry(key.clone())
            .or_insert_with(|| cx.focus_handle())
            .clone();
        let now = self.clock.timestamp_millis();
        let figures = card.figures_at(now);

        // The trailing glyph: the working spinner, the calm clock of one waiting on its own work,
        // the amber dot of one waiting on the person.
        let glyph: Option<AnyElement> = match card.state.as_str() {
            _ if card.needs_you.is_some() || card.state == SubagentProgress::NEEDS_YOU => Some(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.warning)
                    .into_any_element(),
            ),
            SubagentProgress::WAITING => Some(
                crate::icons::icon(crate::icons::CLOCK_CIRCLE)
                    .size(px(12.0))
                    .text_color(theme.text_muted)
                    .into_any_element(),
            ),
            _ if card.running => Some(
                crate::loaders::mini_glyph_spinner(
                    format!("subagent-card-{key}"),
                    2.0,
                    theme.glyph,
                    cx.entity_id(),
                    cx,
                )
                .into_any_element(),
            ),
            _ => None,
        };
        // slopty: the card's hover, faded; its quiet controls fade in with it, each laid out in
        // an 18 pt footprint and pressed over 24 (44 on a touch screen: `press_target`, M25).
        let card_fade = format!("{key}-card-hover");
        let shown = crate::motion::hover_t(&card_fade);
        let quiet = |id: String, label: &'static str, glyph: &'static str| {
            let fade = format!("{id}-hover");
            crate::surface_chrome::press_target(SharedString::from(id), QUIET_FOOTPRINT)
                .role(gpui::Role::Button)
                .aria_label(label)
                .rounded(px(6.0))
                .cursor_pointer()
                .opacity(shown)
                // slopty: a tab stop, shown while it has the keyboard.
                .tab_index(0)
                .focus_visible({
                    let ring = crate::surface_chrome::ring(theme, crate::surface_chrome::RING_OFFSET);
                    move |s| s.opacity(1.0).outline_ring(ring)
                })
                .bg(crate::motion::hover_blend(&fade, gpui::transparent_black(), crate::theme::wash(0.11)))
                .hover_fade(fade)
                .tooltip(crate::settings::widgets::text_tooltip(label))
                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                .child(
                    crate::icons::icon(glyph)
                        .size(px(12.0))
                        .text_color(theme.text_muted),
                )
        };
        let stop = card
            .row
            .clone()
            .filter(|_| card.running && !stopping)
            .map(|row| {
                quiet(format!("{key}-stop"), "Stop subagent", crate::icons::CLOSE).on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stop_subagent(row.clone(), cx);
                    }),
                )
            });
        let drill = card.chat.clone().map(|chat| {
            quiet(format!("{key}-open"), "Open subagent", crate::icons::ARROW_UP_RIGHT).on_click(
                cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(TranscriptEvent::OpenSubagent {
                        chat_id: chat.clone(),
                    });
                }),
            )
        });
        // How open the run is, 0 to 1, as it slides; the chevron turns with it, as a tool
        // group's does.
        let (from, since) = moving.unwrap_or((0.0, moment));
        let room = self.subagents.child_room();
        let full = |rows: Option<f32>| rows.map_or(room, |rows| (rows + 1.0).min(room));
        let target_rows = card
            .chat
            .as_deref()
            .and_then(|chat| self.subagents.open.get(chat))
            .and_then(|child| child.rows.get());
        let target = if open { full(target_rows) } else { 0.0 };
        let spec = super::user_resize_spec((target - from).abs());
        let t = moving.map_or(1.0, |_| {
            let raw = moment.saturating_duration_since(since).as_secs_f32() / spec.total().as_secs_f32();
            spec.curve.eval(raw.min(1.0))
        });
        let height = crate::motion::lerp(from, target, t);
        let openness = if full(target_rows) > 0.0 { (height / full(target_rows)).clamp(0.0, 1.0) } else { 0.0 };
        let chevron = can_open_in_place.then(|| {
            div()
                .size(px(18.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    crate::icons::icon(crate::icons::ALT_ARROW_DOWN)
                        .size(px(12.0))
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(
                            -std::f32::consts::FRAC_PI_2 * (1.0 - openness),
                        )))
                        .text_color(theme.text_faint),
                )
        });
        let label_tint = if card.failed { theme.danger } else { theme.text };
        let header = div()
            .h(px(CHIP_CARD_HEIGHT - 2.0))
            .w_full()
            .min_w_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(Theme::SPACE_SM))
            .px(px(Theme::SPACE_SM))
            .child(
                div()
                    .size(px(18.0))
                    .flex_none()
                    .rounded(px(5.0))
                    .bg(crate::theme::ink(0.08))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        crate::icons::icon(crate::icons::BOT)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(label_tint.opacity(0.9))
                    .child(card.title.clone()),
            )
            .when_some(card.kind.clone(), |row, kind| {
                row.child(
                    div()
                        .flex_none()
                        .max_w(px(200.0))
                        .truncate()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(kind),
                )
            })
            .children(stop)
            .children(drill)
            .children(glyph.map(|glyph| div().flex_none().child(glyph)))
            .children(chevron);
        let step_line = (card.step.is_some() || !figures.is_empty()).then(|| {
            div()
                .h(px(STEP_LINE_HEIGHT))
                .w_full()
                .min_w_0()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(Theme::SPACE_SM))
                .pl(px(Theme::SPACE_SM + 18.0 + Theme::SPACE_SM))
                .pr(px(Theme::SPACE_SM))
                .pb(px(2.0))
                .text_size(crate::typography::ui_rems(11.5))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_color(if card.failed {
                            theme.danger
                        } else {
                            theme.text_muted
                        })
                        .child(SharedString::from(if stopping {
                            "Stopping…".to_owned()
                        } else {
                            card.step.clone().unwrap_or_default()
                        })),
                )
                .when(!figures.is_empty(), |line| {
                    line.child(
                        div()
                            .flex_none()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(figures.clone())),
                    )
                })
        });
        let needs_you = card.needs_you.clone().map(|ask| {
            div()
                .w_full()
                .min_w_0()
                .pl(px(Theme::SPACE_SM + 18.0 + Theme::SPACE_SM))
                .pr(px(Theme::SPACE_SM))
                .pb(px(Theme::SPACE_XS))
                .truncate()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.warning)
                .child(SharedString::from(format!("Needs you · {ask}")))
        });
        let sliding = moving.is_some() && t < 1.0;
        let child = card.chat.clone().filter(|_| open || sliding).map(|chat| {
            let (transcript, rows) = self.inline_child(row_id, &chat, card.running, cx);
            // As tall as its rows, up to its room: a short run leaves no room under its last
            // line. Before its rows laid out, the room. It slides open over its whole height,
            // so nothing in it moves as it shows. The hairline over it is its own pixel.
            let whole = if open { full(rows) } else { from.max(height) };
            div()
                .id(SharedString::from(format!("{key}-child")))
                .role(gpui::Role::Group)
                .aria_label(SharedString::from(format!("Transcript of {}", card.title)))
                .w_full()
                .flex_none()
                .overflow_hidden()
                .h(px(height))
                .opacity(openness)
                .child(
                    div()
                        .w_full()
                        .h(px(whole))
                        .border_t_1()
                        .border_color(crate::theme::hairline(0.06))
                        .child(transcript),
                )
        });
        // Each frame of the slide measures the card's row again, until it lands.
        self.subagents.drawn.insert(key.clone(), (open, height));
        let slide = if sliding {
            let row = row_id.clone();
            let this = cx.weak_entity();
            Some(
                gpui::canvas(
                    move |_, window, _| {
                        window.on_next_frame(move |_, cx| {
                            this.update(cx, |this, cx| this.row_resized(&row, cx)).ok();
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_0(),
            )
        } else {
            if moving.is_some() {
                self.subagents.moving.remove(&key);
                if !open {
                    // Slid shut: let its child go, and the row take its closed height.
                    if let Some(chat) = card.chat.as_deref() {
                        self.close_child(chat, cx);
                    }
                    // After this layout, which holds the list's state.
                    let row = row_id.clone();
                    let this = cx.weak_entity();
                    cx.defer(move |cx| {
                        this.update(cx, |this, cx| this.row_resized(&row, cx)).ok();
                    });
                }
            }
            None
        };
        let value = [card.step.clone(), Some(figures).filter(|f| !f.is_empty())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let toggle_key = key.clone();
        let key_chat = card.chat.clone();
        let key_row = card.row.clone().filter(|_| card.running);
        div()
            .id(key.clone())
            .track_focus(&focus)
            .group("subagent-card")
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Subagent {}", card.title)))
            .aria_value(SharedString::from(value))
            .when(can_open_in_place, |card| card.aria_expanded(open))
            .my(px(Theme::SPACE_XS))
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(super::TRANSCRIPT_CARD_RADIUS))
            .border_1()
            .border_color(if card.needs_you.is_some() {
                theme.warning.opacity(0.35)
            } else {
                theme.border
            })
            .text_size(px(super::TOOL_LABEL_SIZE))
            .line_height(px(super::TOOL_LABEL_LINE_HEIGHT))
            .cursor_pointer()
            .bg(crate::motion::hover_blend(&card_fade, crate::theme::ink(0.03), crate::theme::ink(0.05)))
            .hover_fade(card_fade)
            .on_click(cx.listener({
                let key = toggle_key.clone();
                let chat = card.chat.clone();
                move |this, _, window, cx| {
                    window.focus(&this.subagents.focus[&key], cx);
                    if can_open_in_place {
                        this.toggle_card(key.clone(), open, cx);
                    } else if let Some(chat) = chat.clone() {
                        // A phone, or a fold's rail: the card drills in.
                        cx.emit(TranscriptEvent::OpenSubagent { chat_id: chat });
                    }
                }
            }))
            .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                let keys = &event.keystroke;
                let command = keys.modifiers.platform;
                match keys.key.as_str() {
                    "enter" if command => {
                        if let Some(chat) = key_chat.clone() {
                            cx.emit(TranscriptEvent::OpenSubagent { chat_id: chat });
                        }
                    }
                    "enter" if can_open_in_place => this.toggle_card(toggle_key.clone(), open, cx),
                    "." if command => {
                        if let Some(row) = key_row.clone() {
                            this.stop_subagent(row, cx);
                        }
                    }
                    "escape" if open => this.toggle_card(toggle_key.clone(), open, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .relative()
            .child(header)
            .children(step_line)
            .children(needs_you)
            .children(child)
            .children(slide)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::ToolFacts;
    use slopty_view_proto::ToolCall;

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
            diff_ref: None,
            subagent_ref: Some("child".into()),
            subagent_status: Some(SubagentStatus::Running),
            subagent_tail: None,
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

    /// A running card reads its step, and its figures with the time it has run; settled, its
    /// outcome and the time it took.
    #[test]
    fn a_card_reads_its_step_then_its_outcome() {
        let mut progress = SubagentProgress {
            agent_type: Some("Explore".into()),
            model: Some("haiku".into()),
            running: true,
            state: SubagentProgress::RUNNING.into(),
            step: Some("Reading parser.rs".into()),
            tool_uses: Some(12),
            tokens: Some(41_000),
            started_at: Some(1_000),
            figures: "12 tools · 41k tokens".into(),
            chat_id: Some("child-chat".into()),
            row: Some("task:a1".into()),
            ..SubagentProgress::default()
        };
        let card = Card::of(&spawn(Some(progress.clone())));
        assert_eq!(card.title.as_ref(), "Count lines");
        assert_eq!(card.kind.as_deref(), Some("Explore · haiku"));
        assert_eq!(card.chat.as_deref(), Some("child-chat"));
        assert_eq!(card.step.as_deref(), Some("Reading parser.rs"));
        assert_eq!(card.figures_at(124_000), "12 tools · 41k tokens · 2m 3s");
        progress.running = false;
        progress.state = SubagentProgress::DONE.into();
        progress.outcome = Some("Done".into());
        progress.duration_ms = Some(65_000);
        let settled = Card::of(&spawn(Some(progress)));
        assert_eq!(settled.step.as_deref(), Some("Done"));
        assert_eq!(settled.figures_at(999_999), "12 tools · 41k tokens · 1m 5s");
    }

    /// Without progress from the view model, the card stands on what the chip carries.
    #[test]
    fn a_card_without_progress_names_its_chat_and_row() {
        let card = Card::of(&spawn(None));
        assert!(card.running);
        assert_eq!(card.chat.as_deref(), Some("child"));
        assert_eq!(card.row.as_deref(), Some("chat:child"));
        assert_eq!(card.figures_at(5), "");
    }
}
