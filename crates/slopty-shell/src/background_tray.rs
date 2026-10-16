// slopty: Slopty's own file (not in zeron).
//! slopty: the Background tray, above the composer in zeron's tray stack: everything a chat
//! left at work beside its turn (`docs/decisions/ui.md`, "The Background tray").
//!
//! It lists the chat's own agent first ("main", what it does, how long, the tokens it wrote),
//! then each subagent, command, monitor, loop and wakeup the view model lists
//! (`slopty_view_doc::work::WorkTray`), with its kind, its step, its elapsed time and its tokens
//! (the TUI's agents list and its `/tasks` view). A row that needs the person sits first, amber;
//! its request is answered in this chat's dock. ✕ stops a row (`slopty.StopWork`), "Stop all"
//! every live one (`slopty.StopAllWork`). A click scrolls the transcript to the call that
//! started it ([`reveal_call`]); a card or a notice in the transcript opens the tray at its row
//! ([`crate::transcript::reveal_work`]). Its header toggles it, and ⌥B focuses it.
//!
//! What ended stays under a "Finished" fold; housekeeping of the agent's own (a memory pass) is
//! left out, as the TUI folds it away.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::Focusable as _;
use gpui::{
    AnyElement, App, Context, FocusHandle, KeyDownEvent, SharedString, Window, actions, div,
    prelude::*, px,
};
use slopty_view_doc::calls;
use slopty_view_doc::work::{WorkRow, WorkTray, duration_words, tokens_short};

use crate::composer::{Composer, QUEUE_COMPOSER_OVERLAP};
use crate::icons::{self, icon};
use crate::motion;
use crate::settings::widgets::ActionHint as _; // slopty: tooltips name their key.
use crate::theme::Theme;

actions!(background_tray, [
    /// Open the Background tray and give it the keyboard (⌥B).
    FocusBackgroundTray
]);

/// The key context the tray's keys answer in.
pub const TRAY_CONTEXT: &str = "BackgroundTray";

const HEADER_HEIGHT: f32 = 32.0;
const ROW_HEIGHT: f32 = 28.0;
const ROW_PAD_X: f32 = 8.0;
const ROW_RADIUS: f32 = crate::queue::ROW_RADIUS;
const TEXT_SIZE: f32 = 12.5;
const SUB_SIZE: f32 = 11.5;
const GLYPH_SLOT: f32 = 14.0;
/// slopty: a row control's hit target (the house's 24 minimum).
const ACTION_SIZE: f32 = 24.0;
/// How far a row a subagent started steps in under its parent.
const DEPTH_STEP: f32 = 14.0;
/// How long a row a reveal opened at stays marked.
const MARK_SHOWN: Duration = Duration::from_millis(1600);

/// Bind the tray's keys. Call once at boot, beside the composer's.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("alt-b", FocusBackgroundTray, Some(crate::composer::MESSAGE_COMPOSER_CONTEXT)),
        gpui::KeyBinding::new("alt-b", FocusBackgroundTray, Some(TRAY_CONTEXT)),
    ]);
}

/// The hook from the tray to the transcript: a tray row's click asks the transcript of chat
/// `chat_id` to scroll to the call `part_id` that started it, and to open it when it is a
/// subagent's card. The main transcript observes this global; `seq` grows with every ask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallReveal {
    /// The chat whose transcript scrolls.
    pub chat_id: String,
    /// The call's tool part.
    pub part_id: String,
    /// One more than the ask before it.
    pub seq: u64,
}

impl gpui::Global for CallReveal {}

/// Ask the transcript of chat `chat_id` to scroll to its call `part_id` ([`CallReveal`]).
pub fn reveal_call(chat_id: &str, part_id: &str, cx: &mut App) {
    let seq = cx.try_global::<CallReveal>().map_or(1, |last| last.seq.wrapping_add(1));
    cx.set_global(CallReveal { chat_id: chat_id.to_owned(), part_id: part_id.to_owned(), seq });
}

/// What the tray keeps between frames.
pub(crate) struct TrayState {
    /// Whether each chat's tray is open; a chat with a row that needs the person opens by itself
    /// until the person closes it.
    open: HashMap<String, bool>,
    /// The chat whose tray a reveal holds up even with nothing at work.
    held: Option<String>,
    /// The row a reveal opened at, and since when.
    marked: Option<(String, Instant)>,
    /// The Finished fold is open.
    finished_open: bool,
    /// The row whose details show under it, by id (the TUI's "Shell details").
    detail: Option<String>,
    /// Rows asked to stop, until they stop.
    stopping: HashSet<String>,
    /// "Stop all" asked, until nothing is left to stop.
    stopping_all: bool,
    /// The keyboard's row, by id.
    active: Option<String>,
    pub(crate) focus: Option<FocusHandle>,
    pub(crate) scroll: gpui::ScrollHandle, // slopty: the composer's rail follows it.
    /// The last reveal taken.
    seen: u64,
    mark_task: Option<gpui::Task<()>>,
}

impl Default for TrayState {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
            held: None,
            marked: None,
            finished_open: false,
            detail: None,
            stopping: HashSet::new(),
            stopping_all: false,
            active: None,
            focus: None,
            scroll: gpui::ScrollHandle::new(),
            seen: 0,
            mark_task: None,
        }
    }
}

/// The rows a tray draws, housekeeping left out: what is at work (needs-you first, as the view
/// model orders them), then what ended.
pub(crate) fn split_rows(tray: &WorkTray) -> (Vec<&WorkRow>, Vec<&WorkRow>) {
    let (mut live, ended): (Vec<&WorkRow>, Vec<&WorkRow>) =
        tray.rows.iter().filter(|row| !row.ambient).partition(|row| row.live || row.needs_you.is_some());
    // What needs the person sits first, the rest in the agent's order; then each kind's rows
    // together under its section, sections in the order they first come.
    live.sort_by_key(|row| row.needs_you.is_none());
    let mut order: Vec<&'static str> = Vec::new();
    for row in &live {
        let name = section(&row.kind);
        if !order.contains(&name) {
            order.push(name);
        }
    }
    live.sort_by_key(|row| order.iter().position(|name| *name == section(&row.kind)));
    (live, ended)
}

/// The section a row of `kind` sits under, as the TUI's tasks dialog heads them.
pub(crate) fn section(kind: &str) -> &'static str {
    match kind {
        WorkRow::SUBAGENT | WorkRow::TEAMMATE => "Agents",
        WorkRow::COMMAND => "Shells",
        WorkRow::MONITOR => "Monitors",
        WorkRow::MCP => "MCP tasks",
        WorkRow::REMOTE => "Cloud agents",
        WorkRow::WORKFLOW => "Dynamic workflows",
        "wakeup" | "loop" | "cron" | "schedule" => "Scheduled",
        _ => "Other",
    }
}

/// The live rows' sections with their counts, in drawn order, when there is more than one:
/// "Shells (2)", "Agents (1)".
pub(crate) fn sections(live: &[&WorkRow]) -> Vec<(&'static str, usize)> {
    let mut counted: Vec<(&'static str, usize)> = Vec::new();
    for row in live {
        let name = section(&row.kind);
        match counted.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count = count.saturating_add(1),
            None => counted.push((name, 1)),
        }
    }
    if counted.len() > 1 { counted } else { Vec::new() }
}

/// A row's time at `now`: a live row ticks from when it began, an ended one says how long it ran.
pub(crate) fn row_elapsed(row: &WorkRow, now: i64) -> Option<String> {
    let ms = if row.live {
        u64::try_from(now.saturating_sub(row.started_at)).ok()?
    } else if let Some(ms) = row.duration_ms {
        ms
    } else {
        u64::try_from(row.ended_at?.saturating_sub(row.started_at)).ok()?
    };
    // slopty: a live row's timer pads its seconds, so it holds its width as it counts.
    Some(if row.live {
        crate::transcript::ticking_elapsed(i64::try_from(ms / 1_000).unwrap_or(i64::MAX))
    } else {
        duration_words(ms)
    })
}

/// A row's right-hand figures at `now`: "2m 5s · 12 tool uses · 41k tokens".
pub(crate) fn row_figures(row: &WorkRow, now: i64) -> String {
    [row_elapsed(row, now), Some(row.figures.clone()).filter(|f| !f.is_empty())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the row says under its title: what it asks of the person, else its step, else how it
/// ended, else the last line it printed.
pub(crate) fn row_subline(row: &WorkRow) -> Option<(String, bool)> {
    if let Some(ask) = row.needs_you.as_ref().filter(|ask| !ask.trim().is_empty()) {
        return Some((ask.clone(), false));
    }
    if let Some(step) = row.step.as_ref().filter(|s| !s.trim().is_empty()) {
        return Some((step.clone(), false));
    }
    if !row.live && let Some(outcome) = row.outcome.as_ref().filter(|o| !o.trim().is_empty()) {
        return Some((outcome.clone(), false));
    }
    let tail = row.output.as_deref()?.lines().rev().find(|line| !line.trim().is_empty())?;
    Some((tail.trim_end().to_owned(), true))
}

/// The header's words: what needs the person, else the counts.
pub(crate) fn header_words(tray: &WorkTray) -> String {
    let needs = tray.counts.needs_you;
    let counts = if tray.summary.is_empty() { tray.counts.summary() } else { tray.summary.clone() };
    match (needs, counts.is_empty()) {
        (0, true) => "Nothing running".to_owned(),
        (0, false) => counts,
        (n, true) => format!("{n} needs you"),
        (n, false) => format!("{n} needs you · {counts}"),
    }
}

/// The glyph of a row's kind.
fn kind_icon(kind: &str) -> &'static str {
    match kind {
        WorkRow::COMMAND => icons::TERMINAL,
        WorkRow::MONITOR => icons::EYE,
        WorkRow::SUBAGENT | WorkRow::TEAMMATE => icons::BOT,
        WorkRow::WORKFLOW => icons::WIDGET,
        WorkRow::MCP => icons::GLOBAL,
        WorkRow::REMOTE => icons::CLOUD,
        "wakeup" | "loop" | "cron" | "schedule" => icons::CLOCK_CIRCLE,
        _ => icons::LIST,
    }
}

impl Composer {
    /// A reveal from the transcript: open this chat's tray at its row.
    pub(crate) fn on_work_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(reveal) = cx.try_global::<crate::transcript::WorkReveal>().cloned() else { return };
        if reveal.seq == self.tray.seen {
            return;
        }
        self.tray.seen = reveal.seq;
        if self.state.read(cx).selected_chat.as_deref() != Some(reveal.chat_id.as_str()) {
            return;
        }
        self.tray.open.insert(reveal.chat_id.clone(), true);
        self.tray.held = Some(reveal.chat_id);
        if self.dock.tray.as_deref().is_some_and(|tray| {
            tray.rows.iter().any(|row| row.id == reveal.row_id && !row.live && row.needs_you.is_none())
        }) {
            self.tray.finished_open = true;
        }
        self.mark_row(reveal.row_id, cx);
        cx.notify();
    }

    fn mark_row(&mut self, row: String, cx: &mut Context<Self>) {
        self.tray.active = Some(row.clone());
        self.tray.marked = Some((row, Instant::now()));
        self.tray.mark_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARK_SHOWN).await;
            this.update(cx, |this, cx| {
                this.tray.marked = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// The rows changed: forget stops that took.
    pub(crate) fn tray_rows_changed(&mut self, _cx: &mut Context<Self>) {
        let Some(tray) = self.dock.tray.clone() else { return };
        self.tray
            .stopping
            .retain(|id| tray.rows.iter().any(|row| row.id == *id && row.live));
        if !tray.can_stop_all {
            self.tray.stopping_all = false;
        }
    }

    /// Whether the selected chat's tray is open.
    fn tray_open(&self, chat: &str, tray: &WorkTray) -> bool {
        self.tray.open.get(chat).copied().unwrap_or(tray.counts.needs_you > 0)
    }

    /// Open the tray and give it the keyboard (⌥B).
    pub(crate) fn focus_background_tray(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return };
        let Some(tray) = self.dock.tray.clone() else { return };
        if tray.rows.iter().all(|row| row.ambient) {
            return;
        }
        self.tray.open.insert(chat.clone(), true);
        self.tray.held = Some(chat);
        let (live, ended) = split_rows(&tray);
        self.tray.active = live.first().or(ended.first()).map(|row| row.id.clone());
        let focus = self.tray.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Toggle the selected chat's tray (its header).
    fn toggle_tray(&mut self, chat: String, open: bool, cx: &mut Context<Self>) {
        self.tray.open.insert(chat.clone(), !open);
        if open && self.tray.held.as_deref() == Some(chat.as_str()) {
            self.tray.held = None;
        }
        cx.notify();
    }

    /// Stop row `row` (`slopty.StopWork`).
    pub(crate) fn stop_row(&mut self, row: String, cx: &mut Context<Self>) {
        if self.tray.stopping.insert(row.clone()) {
            let params = serde_json::json!({ "rowId": row });
            self.dock_call_or(calls::STOP_WORK, params, "Couldn't stop it", move |this| {
                this.tray.stopping.remove(&row);
            }, cx);
            cx.notify();
        }
    }

    /// Stop every live row (`slopty.StopAllWork`).
    fn stop_all(&mut self, cx: &mut Context<Self>) {
        if !self.tray.stopping_all {
            self.tray.stopping_all = true;
            self.dock_call_or(calls::STOP_ALL_WORK, serde_json::json!({}), "Couldn't stop the work", |this| {
                this.tray.stopping_all = false;
            }, cx);
            cx.notify();
        }
    }

    /// A row's click: scroll the transcript to its call, mark it here.
    fn open_row(&mut self, row: &WorkRow, cx: &mut Context<Self>) {
        let Some(chat) = self.state.read(cx).selected_chat.clone() else { return };
        if let Some(part) = &row.call_part_id {
            reveal_call(&chat, part, cx);
        }
        self.mark_row(row.id.clone(), cx);
        cx.notify();
    }

    /// The rows the keyboard walks, in drawn order.
    fn walk_rows(&self, tray: &WorkTray) -> Vec<WorkRow> {
        let (live, ended) = split_rows(tray);
        let mut rows: Vec<WorkRow> = live.into_iter().cloned().collect();
        if self.tray.finished_open {
            rows.extend(ended.into_iter().cloned());
        }
        rows
    }

    fn on_tray_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tray) = self.dock.tray.clone() else { return };
        let rows = self.walk_rows(&tray);
        let at = self.tray.active.as_ref().and_then(|id| rows.iter().position(|row| row.id == *id));
        let key = event.keystroke.key.as_str();
        let cmd = event.keystroke.modifiers.platform;
        match key {
            "up" | "down" if !rows.is_empty() => {
                let next = match (at, key) {
                    (None, _) => 0,
                    (Some(ix), "up") => ix.saturating_sub(1),
                    (Some(ix), _) => (ix + 1).min(rows.len() - 1),
                };
                self.tray.active = Some(rows[next].id.clone());
            }
            "enter" => {
                if let Some(row) = at.map(|ix| rows[ix].clone()) {
                    self.open_row(&row, cx);
                }
            }
            // The keyboard row's details, open and closed.
            "right" | "left" => {
                if let Some(row) = at.map(|ix| rows[ix].clone()) {
                    self.tray.detail = (key == "right").then_some(row.id);
                }
            }
            "." if cmd => {
                if let Some(row) = at.map(|ix| rows[ix].clone()).filter(|row| row.can_stop && row.live) {
                    self.stop_row(row.id, cx);
                }
            }
            "backspace" | "delete" => {
                if let Some(row) = at.map(|ix| rows[ix].clone()).filter(|row| row.can_stop && row.live) {
                    self.stop_row(row.id, cx);
                }
            }
            "escape" => {
                let focus = self.input.focus_handle(cx);
                window.focus(&focus, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The Background tray, or `None` while the chat has nothing at work (and no reveal holds
    /// it up). `inset`: how far it steps in from the composer's sides.
    pub(crate) fn render_background_tray(
        &mut self,
        inset: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let chat = self.state.read(cx).selected_chat.clone()?;
        let tray = self.dock.tray.clone()?;
        if self.dock.chat.as_deref() != Some(chat.as_str()) {
            return None;
        }
        let (live, ended) = split_rows(&tray);
        let held = self.tray.held.as_deref() == Some(chat.as_str());
        if live.is_empty() && !(held && !ended.is_empty()) {
            if !held {
                self.tray.held = None;
            }
            return None;
        }
        let theme = Theme::of(cx).clone();
        let open = self.tray_open(&chat, &tray);
        let now = chrono::Utc::now().timestamp_millis();
        let focus = self.tray.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        let focused = focus.contains_focused(window, cx);

        let header = self.tray_header(&chat, &tray, open, &theme, cx);
        let mut surface = crate::queue::queue_panel_surface(&theme, cx.has_active_drag())
            .debug_selector(|| "background-tray".into())
            .child(header);
        if open {
            let mut list = div().flex().flex_col();
            if tray.main.working {
                list = list.child(self.tray_main_row(&tray, now, &theme));
            }
            let sections = sections(&live);
            let mut heading = None;
            for row in &live {
                let name = section(&row.kind);
                if heading != Some(name)
                    && let Some((_, count)) = sections.iter().find(|(n, _)| *n == name)
                {
                    heading = Some(name);
                    list = list.child(tray_section(name, *count, &theme));
                }
                list = list.child(self.tray_row(row, now, focused, &theme, cx));
            }
            if !ended.is_empty() {
                list = list.child(self.tray_finished_fold(ended.len(), &theme, cx));
                if self.tray.finished_open {
                    for row in &ended {
                        list = list.child(self.tray_row(row, now, focused, &theme, cx));
                    }
                }
            }
            let rows = crate::edge_fade::edge_faded(
                Theme::TRANSCRIPT_FADE_BAND,
                true,
                true,
                div()
                    .id("background-tray-rows")
                    .max_h(window.viewport_size().height * 0.3)
                    .overflow_y_scroll()
                    .track_scroll(&self.tray.scroll)
                    .child(list),
            )
            .fade_overflow_y(&self.tray.scroll);
            // slopty: the house's floating rail over a long list (the composer's one rail serves
            // the tray while no `/` or `@` list is open).
            let rail = self
                .active_popup_scroll()
                .is_none()
                .then(|| crate::popover::rail(self, "background-tray-scrollbar", &theme, cx))
                .flatten();
            surface = surface.child(
                div()
                    .id("background-tray-list")
                    .relative()
                    .mt(px(2.0))
                    .pb(px(6.0))
                    .on_hover(cx.listener(Self::on_popup_list_hover))
                    .child(rows)
                    .children(rail),
            );
        }
        Some(
            div()
                .id("background-tray")
                .role(gpui::Role::Group)
                .aria_label("Background work")
                .key_context(TRAY_CONTEXT)
                .track_focus(&focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.on_tray_key(event, window, cx)
                }))
                .mx(px(inset))
                .mb(px(-(Theme::SPACE_SM + QUEUE_COMPOSER_OVERLAP)))
                .child(crate::frost::frosted(
                    crate::queue::PANEL_RADIUS,
                    crate::frost::MENU_BLUR,
                    surface,
                ))
                .into_any_element(),
        )
    }

    /// "Background · 3 commands · 1 subagent", the state's dot, Stop all, the chevron.
    fn tray_header(
        &self,
        chat: &str,
        tray: &WorkTray,
        open: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let needs = tray.counts.needs_you > 0;
        let words = header_words(tray);
        let wakes = tray.rows.iter().any(|row| row.live && row.wakes);
        // slopty: working is no news: a muted pulse; amber only for what needs the person.
        let dot_color = if needs { theme.warning } else if wakes { theme.text_muted } else { theme.text_faint };
        let toggle_hover: SharedString = "background-tray-toggle-hover".into();
        let label = if open { "Collapse background work" } else { "Expand background work" };
        let toggle_chat = chat.to_owned();
        let toggle = div()
            .id("background-tray-toggle")
            .role(gpui::Role::Button)
            .aria_label(label)
            .aria_value(SharedString::from(words.clone()))
            .aria_expanded(open)
            .flex_1()
            .min_w_0()
            .h(px(HEADER_HEIGHT))
            .px(px(ROW_PAD_X))
            .rounded(px(ROW_RADIUS))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .bg(motion::hover_blend(&toggle_hover, gpui::transparent_black(), crate::theme::ink(0.05)))
            .on_hover(motion::hover_listener(toggle_hover))
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_tray(toggle_chat.clone(), open, cx)))
            .child(
                div()
                    .w(px(GLYPH_SLOT))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(state_dot("background-tray-dot", dot_color, wakes && !needs)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(TEXT_SIZE))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child("Background"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(crate::typography::ui_rems(TEXT_SIZE))
                    .text_color(if needs { theme.warning } else { theme.text_faint })
                    .child(SharedString::from(words)),
            )
            .child(
                icon(if open { icons::ALT_ARROW_DOWN } else { icons::ALT_ARROW_UP })
                    .size(px(12.0))
                    .text_color(theme.text_muted.opacity(0.7)),
            );
        let stop_all = (open && tray.can_stop_all).then(|| {
            let stopping = self.tray.stopping_all;
            let key: SharedString = "background-tray-stop-all".into();
            div()
                .id("background-tray-stop-all")
                .role(gpui::Role::Button)
                .aria_label(if stopping { "Stopping all…" } else { "Stop all" })
                .flex_none()
                .h(px(ACTION_SIZE))
                .px(px(8.0))
                .flex()
                .items_center()
                .rounded(px(6.0))
                .text_size(crate::typography::ui_rems(SUB_SIZE))
                .text_color(motion::hover_blend(&key, theme.text_muted, theme.text))
                .when(!stopping, |el| {
                    el.cursor_pointer()
                        .bg(motion::hover_blend(&key, gpui::transparent_black(), crate::theme::wash(0.11)))
                        .on_click(cx.listener(|this, _, _, cx| this.stop_all(cx)))
                })
                .on_hover(motion::hover_listener(key))
                .child(if stopping { "Stopping…" } else { "Stop all" })
        });
        div()
            .flex()
            .items_center()
            .gap(px(2.0))
            .child(toggle)
            .children(stop_all)
            .into_any_element()
    }

    /// The chat's own agent, first: "main", what it does, how long, the tokens it wrote.
    fn tray_main_row(&self, tray: &WorkTray, now: i64, theme: &Theme) -> AnyElement {
        let main = &tray.main;
        let elapsed = main
            .started_at
            .and_then(|at| u64::try_from(now.saturating_sub(at)).ok())
            .map(duration_words);
        let tokens = main.tokens.filter(|t| *t > 0).map(|t| format!("↓ {} tokens", tokens_short(t)));
        let figures = [elapsed, tokens].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        let step = main.step.clone().unwrap_or_default();
        let label = [Some("main".to_owned()), Some(step.clone()).filter(|s| !s.is_empty()), Some(figures.clone()).filter(|f| !f.is_empty())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        div()
            .id("background-tray-main")
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(label))
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PAD_X))
            .flex()
            .items_center()
            .gap(px(8.0))
            .text_size(crate::typography::ui_rems(TEXT_SIZE))
            // slopty: the main agent at work: a muted pulse, not the accent.
            .child(div().w(px(GLYPH_SLOT)).flex_none().flex().justify_center().child(state_dot(
                "background-tray-main-dot",
                theme.text_muted,
                true,
            )))
            .child(div().flex_none().font_weight(gpui::FontWeight::MEDIUM).text_color(theme.text).child("main"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(step)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(crate::typography::ui_rems(SUB_SIZE))
                    .font_features(crate::typography::tabular())
                    .text_color(theme.text_faint)
                    .child(SharedString::from(figures)),
            )
            .into_any_element()
    }

    /// One piece of background work.
    fn tray_row(
        &self,
        row: &WorkRow,
        now: i64,
        tray_focused: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = row.id.clone();
        let key: SharedString = format!("background-row-{id}").into();
        let needs = row.needs_you.is_some();
        let stopping = self.tray.stopping.contains(&row.id);
        let marked = self.tray.marked.as_ref().is_some_and(|(marked, _)| *marked == row.id);
        let active = tray_focused && self.tray.active.as_deref() == Some(row.id.as_str());
        let figures = row_figures(row, now);
        let subline = row_subline(row);
        let state = if stopping { "stopping".to_owned() } else { row.state_words.clone() };
        let mono = matches!(row.kind.as_str(), WorkRow::COMMAND | WorkRow::MONITOR);
        let lead = row.agent_type.clone().map(|kind| match &row.model {
            Some(model) => format!("{kind} · {model}"),
            None => kind,
        });
        let label = [
            Some(row.noun.clone()),
            lead.clone(),
            Some(row.title.clone()),
            Some(state.clone()),
            subline.as_ref().map(|(words, _)| words.clone()),
            Some(figures.clone()).filter(|f| !f.is_empty()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let glyph: AnyElement = if needs {
            state_dot(format!("background-row-dot-{id}"), theme.warning, false)
        } else if row.live {
            // slopty: work under way is no news (a muted pulse); done a muted check; a stop or an
            // expiry is faint; only a failure is loud.
            state_dot(format!("background-row-dot-{id}"), theme.text_muted, row.wakes && row.state != "paused")
        } else {
            match row.state.as_str() {
                "completed" => icon(icons::CHECK).size(px(11.0)).text_color(theme.text_muted).into_any_element(),
                "failed" => state_dot(format!("background-row-dot-{id}"), theme.danger, false),
                _ => state_dot(format!("background-row-dot-{id}"), theme.text_faint, false),
            }
        };
        let state_color = if needs {
            theme.warning
        } else if row.state == "failed" {
            theme.danger
        } else {
            theme.text_faint
        };
        let show_state = needs || stopping || !row.live || row.state != "running";
        let open_row = row.clone();
        let stop = (row.live && row.can_stop).then(|| {
            let stop_id = row.id.clone();
            let stop_key: SharedString = format!("background-row-stop-{id}").into();
            div()
                .id(stop_key.clone())
                .role(gpui::Role::Button)
                .flex_none()
                .size(px(ACTION_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.0))
                .opacity(if active || stopping { 1.0 } else { motion::hover_t(&key) })
                .when(!stopping, |el| {
                    el.cursor_pointer()
                        .bg(motion::hover_blend(&stop_key, gpui::transparent_black(), crate::theme::wash(0.11)))
                        .on_hover(motion::hover_listener(stop_key.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.stop_row(stop_id.clone(), cx)
                        }))
                })
                .hint(if stopping { "Stopping…" } else { "Stop" })
                // slopty: Stop draws the composer's stop square, not a close cross.
                .child(div().size(px(7.0)).rounded(px(2.0)).bg(theme.text_muted))
        });
        let bg = if marked || active {
            crate::theme::card_selected_bg()
        } else {
            motion::hover_blend(&key, crate::theme::wash(0.0), theme.element_hover)
        };
        // Its details, open under it, and the button that opens them.
        let detail_open = self.tray.detail.as_deref() == Some(row.id.as_str());
        let detail = detail_open.then(|| row_detail(row, now, theme));
        let detail_id = row.id.clone();
        let details_key: SharedString = format!("background-row-details-hover-{id}").into();
        let details = div()
            .id(SharedString::from(format!("background-row-details-{id}")))
            .role(gpui::Role::Button)
            .aria_expanded(detail_open)
            .flex_none()
            .size(px(ACTION_SIZE))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .opacity(if active || detail_open { 1.0 } else { motion::hover_t(&key) })
            .cursor_pointer()
            .bg(motion::hover_blend(&details_key, gpui::transparent_black(), crate::theme::wash(0.11)))
            .on_hover(motion::hover_listener(details_key.clone()))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let open = this.tray.detail.as_deref() == Some(detail_id.as_str());
                this.tray.detail = (!open).then(|| detail_id.clone());
                cx.notify();
            }))
            .hint(if detail_open { "Hide details" } else { "Show details" })
            .child(
                icon(if detail_open { icons::ALT_ARROW_DOWN } else { icons::ALT_ARROW_RIGHT })
                    .size(px(12.0))
                    .text_color(theme.text_muted),
            );
        let line = div()
            .id(key.clone())
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(label))
            .aria_selected(active || marked)
            .min_h(px(ROW_HEIGHT))
            .pl(px(ROW_PAD_X + DEPTH_STEP * row.depth as f32))
            .pr(px(4.0))
            .py(px(4.0))
            .rounded(px(ROW_RADIUS))
            .bg(bg)
            .on_hover(motion::hover_listener(key.clone()))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.open_row(&open_row, cx)))
            .flex()
            .items_start()
            .gap(px(8.0))
            .child(
                div()
                    .w(px(GLYPH_SLOT))
                    .h(px(18.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(glyph),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(18.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(crate::typography::ui_rems(TEXT_SIZE))
                            .child(icon(kind_icon(&row.kind)).size(px(12.0)).flex_none().text_color(theme.text_faint))
                            .when_some(lead, |el, lead| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .max_w(px(160.0))
                                        .truncate()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(theme.text)
                                        .child(SharedString::from(lead)),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .when(mono, |el| el.font_family(theme.font_mono.clone()))
                                    .text_color(if row.live { theme.text.opacity(0.9) } else { theme.text_muted })
                                    .child(SharedString::from(row.title.clone())),
                            )
                            .when(show_state, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .text_size(crate::typography::ui_rems(SUB_SIZE))
                                        .text_color(state_color)
                                        .child(SharedString::from(state.clone())),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(crate::typography::ui_rems(SUB_SIZE))
                                    .font_features(crate::typography::tabular())
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(figures)),
                            ),
                    )
                    .when_some(subline, |el, (words, output)| {
                        el.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(crate::typography::ui_rems(SUB_SIZE))
                                .line_height(crate::typography::ui_rems(16.0))
                                .when(output, |el| el.font_family(theme.font_mono.clone()))
                                .text_color(if needs { theme.warning } else { theme.text_faint })
                                .child(SharedString::from(words)),
                        )
                    }),
            )
            .child(details)
            .children(stop)
            .into_any_element();
        match detail {
            Some(detail) => div().flex().flex_col().child(line).child(detail).into_any_element(),
            None => line,
        }
    }

    /// "Finished · N": the rows that ended, folded.
    fn tray_finished_fold(&self, count: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let open = self.tray.finished_open;
        let hover: SharedString = "background-tray-finished-hover".into();
        div()
            .id("background-tray-finished")
            .role(gpui::Role::Button)
            .aria_label(SharedString::from(format!("Finished · {count}")))
            .aria_expanded(open)
            .h(px(24.0))
            .px(px(ROW_PAD_X))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .bg(motion::hover_blend(&hover, gpui::transparent_black(), crate::theme::ink(0.05)))
            .on_hover(motion::hover_listener(hover))
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .on_click(cx.listener(|this, _, _, cx| {
                this.tray.finished_open = !this.tray.finished_open;
                cx.notify();
            }))
            .child(
                div()
                    .w(px(GLYPH_SLOT))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .child(icon(if open { icons::ALT_ARROW_DOWN } else { icons::ALT_ARROW_RIGHT }).size(px(12.0)).text_color(theme.text_faint)),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(SUB_SIZE))
                    .text_color(theme.text_faint)
                    .child(SharedString::from(format!("Finished · {count}"))),
            )
            .into_any_element()
    }
}

/// The most output lines a row's details show, as the TUI's task detail does.
const DETAIL_LINES: usize = 10;

/// A row's details, as the TUI's task detail dialog words them: what it is, how it stands and
/// for how long, what it runs, and the last lines it printed.
pub(crate) fn detail_lines(row: &WorkRow, now: i64) -> (String, Vec<String>, Vec<String>) {
    let title = match row.kind.as_str() {
        WorkRow::COMMAND => "Shell details",
        WorkRow::MONITOR => "Monitor details",
        WorkRow::MCP => "MCP task",
        WorkRow::SUBAGENT | WorkRow::TEAMMATE => "Agent details",
        WorkRow::REMOTE => "Cloud agent",
        WorkRow::WORKFLOW => "Workflow",
        _ => "Details",
    };
    let mut facts = Vec::new();
    let status = match row.outcome.as_deref().filter(|o| !o.trim().is_empty()) {
        Some(outcome) if !row.live => format!("Status: {} · {outcome}", row.state_words),
        _ => format!("Status: {}", row.state_words),
    };
    facts.push(status);
    if let Some(elapsed) = row_elapsed(row, now) {
        facts.push(if row.live { format!("Running for {elapsed}") } else { format!("Ran for {elapsed}") });
    }
    let what = match row.kind.as_str() {
        WorkRow::COMMAND | WorkRow::MONITOR => "Command",
        WorkRow::MCP => "Tool",
        _ => "Task",
    };
    facts.push(format!("{what}: {}", row.title));
    if let Some(step) = row.step.as_ref().filter(|s| !s.trim().is_empty()) {
        facts.push(step.clone());
    }
    if !row.figures.is_empty() {
        facts.push(row.figures.clone());
    }
    let printed: Vec<&str> = row.output.as_deref().map(|o| o.trim_end().lines().collect()).unwrap_or_default();
    let output = if printed.is_empty() {
        if matches!(row.kind.as_str(), WorkRow::COMMAND | WorkRow::MONITOR) {
            vec!["No output available".to_owned()]
        } else {
            Vec::new()
        }
    } else {
        let from = printed.len().saturating_sub(DETAIL_LINES);
        let shown = printed.len().saturating_sub(from);
        let mut lines: Vec<String> = printed.iter().skip(from).map(|l| (*l).to_owned()).collect();
        lines.push(if shown < printed.len() {
            format!("Showing {shown} of {} lines", printed.len())
        } else {
            format!("Showing {shown} {}", if shown == 1 { "line" } else { "lines" })
        });
        lines
    };
    (title.to_owned(), facts, output)
}

/// A row's details under it.
fn row_detail(row: &WorkRow, now: i64, theme: &Theme) -> AnyElement {
    let (title, facts, output) = detail_lines(row, now);
    let last = output.len().saturating_sub(1);
    div()
        .id(SharedString::from(format!("background-row-detail-{}", row.id)))
        .role(gpui::Role::Group)
        .aria_label(SharedString::from(title.clone()))
        .ml(px(ROW_PAD_X + GLYPH_SLOT + 8.0))
        .mr(px(ROW_PAD_X))
        .mb(px(4.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .text_size(crate::typography::ui_rems(SUB_SIZE))
        .text_color(theme.text_muted)
        .child(div().font_weight(gpui::FontWeight::MEDIUM).text_color(theme.text).child(SharedString::from(title)))
        .children(facts.into_iter().map(|fact| div().min_w_0().truncate().child(SharedString::from(fact))))
        .when(!output.is_empty(), |el| {
            el.child(
                div()
                    .mt(px(2.0))
                    .px(px(6.0))
                    .py(px(4.0))
                    .rounded(px(5.0))
                    .bg(theme.element_hover)
                    .flex()
                    .flex_col()
                    .font_family(theme.font_mono.clone())
                    .children(output.into_iter().enumerate().map(|(ix, text)| {
                        div()
                            .min_w_0()
                            .truncate()
                            .when(ix == last, |el| el.text_color(theme.text_faint))
                            .child(SharedString::from(text))
                    })),
            )
        })
        .into_any_element()
}

/// A section's heading over its rows: "Shells (2)".
fn tray_section(name: &str, count: usize, theme: &Theme) -> AnyElement {
    let words = SharedString::from(format!("{name} ({count})"));
    div()
        .id(SharedString::from(format!("background-tray-section-{name}")))
        .role(gpui::Role::Heading)
        .aria_label(words.clone())
        .h(px(20.0))
        .px(px(ROW_PAD_X))
        .flex()
        .items_end()
        .text_size(crate::typography::ui_rems(SUB_SIZE))
        .text_color(theme.text_faint)
        .child(words)
        .into_any_element()
}

/// A 6 px state dot; `pulse`: it breathes, for work that will wake the agent.
fn state_dot(key: impl Into<SharedString>, color: gpui::Hsla, pulse: bool) -> AnyElement {
    let dot = div().size(px(6.0)).rounded_full().bg(color);
    if !pulse {
        return dot.into_any_element();
    }
    div()
        .relative()
        .size(px(6.0))
        .child(div().absolute().inset_0().child(crate::loaders::pulse_glow(key, 3.0, color, 0.35, 1.0)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::work::{MainRow, WorkCounts};

    pub(crate) fn row(id: &str, kind: &str, live: bool) -> WorkRow {
        WorkRow {
            id: id.into(),
            kind: kind.into(),
            noun: kind.into(),
            title: format!("{id} title"),
            agent_type: None,
            model: None,
            state: if live { "running".into() } else { "completed".into() },
            state_words: if live { "running".into() } else { "done".into() },
            live,
            wakes: live,
            awaited: false,
            ambient: false,
            needs_you: None,
            step: None,
            tokens: None,
            tool_uses: None,
            duration_ms: None,
            figures: String::new(),
            started_at: 0,
            ended_at: None,
            outcome: None,
            output: None,
            timeout_ms: None,
            persistent: false,
            call_part_id: None,
            chat_id: None,
            parent: None,
            depth: 0,
            can_stop: live,
        }
    }

    /// Live rows and what needs the person come before what ended; housekeeping is left out.
    #[test]
    fn rows_split_live_from_ended_and_drop_housekeeping() {
        // What needs the person comes first, wherever the agent listed it.
        let mut memory = row("task:m", "dream", true);
        memory.ambient = true;
        let mut asks = row("chat:c", WorkRow::SUBAGENT, false);
        asks.needs_you = Some("Allow cargo test?".into());
        let tray = WorkTray {
            main: MainRow::default(),
            rows: vec![row("task:a", WorkRow::COMMAND, true), asks, row("task:b", WorkRow::COMMAND, false), memory],
            counts: WorkCounts { commands: 1, needs_you: 1, ..WorkCounts::default() },
            summary: "1 command".into(),
            can_stop_all: true,
        };
        let (live, ended) = split_rows(&tray);
        assert_eq!(live.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["chat:c", "task:a"]);
        assert_eq!(ended.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["task:b"]);
        assert_eq!(header_words(&tray), "1 needs you · 1 command");
    }

    /// Live rows of more than one kind sit under their kinds' sections, each kind's rows
    /// together in the order the kinds first come; one kind has no heading.
    #[test]
    fn live_rows_sit_under_their_kinds_sections() {
        let tray = WorkTray {
            main: MainRow::default(),
            rows: vec![
                row("task:a", WorkRow::COMMAND, true),
                row("chat:b", WorkRow::SUBAGENT, true),
                row("task:c", WorkRow::COMMAND, true),
            ],
            counts: WorkCounts::default(),
            summary: String::new(),
            can_stop_all: true,
        };
        let (live, _) = split_rows(&tray);
        assert_eq!(live.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["task:a", "task:c", "chat:b"]);
        assert_eq!(sections(&live), [("Shells", 2), ("Agents", 1)]);
        assert!(sections(&live[..2]).is_empty(), "one kind needs no heading");
    }

    /// A row's details say what it is, how it stands, what it runs and its last ten lines,
    /// with how many show; a command that printed nothing says so.
    #[test]
    fn a_rows_details_read_as_the_tuis() {
        let mut shell = row("task:a", WorkRow::COMMAND, false);
        shell.title = "cargo test".into();
        shell.state_words = "failed".into();
        shell.outcome = Some("exit code 101".into());
        shell.duration_ms = Some(83_000);
        shell.output = Some((1..=12).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n"));
        let (title, facts, output) = detail_lines(&shell, 0);
        assert_eq!(title, "Shell details");
        assert_eq!(facts, ["Status: failed · exit code 101", "Ran for 1m 23s", "Command: cargo test"]);
        assert_eq!(output.first().map(String::as_str), Some("line 3"));
        assert_eq!(output.last().map(String::as_str), Some("Showing 10 of 12 lines"));
        shell.output = None;
        assert_eq!(detail_lines(&shell, 0).2, ["No output available"]);
    }

    /// A live row ticks from when it began; an ended one says how long it ran, with its figures.
    #[test]
    fn a_row_reads_its_time_and_figures() {
        let mut live = row("task:a", WorkRow::SUBAGENT, true);
        live.started_at = 1_000;
        live.figures = "12 tool uses · 41k tokens".into();
        // slopty: a ticking time keeps its width: two-digit seconds.
        assert_eq!(row_figures(&live, 126_000), "2m 05s · 12 tool uses · 41k tokens");
        let mut ended = row("task:b", WorkRow::COMMAND, false);
        ended.duration_ms = Some(45_000);
        assert_eq!(row_figures(&ended, 999_999), "45s");
        ended.output = Some("one\ntwo\n\n".into());
        assert_eq!(row_subline(&ended), Some(("two".into(), true)));
    }
}
