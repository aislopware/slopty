//! slopty: the Inspector, a read-only view of the selected session beside its terminal
//! (`docs/decisions/inspector.md`).
//!
//! The person works in the agent's own TUI; the inspector is where they look into what the
//! session did. Its frame is fixed (`inspector.md`, "Fixed frame"): one line each for the header
//! (status, branch), what it is doing now (its tasks and running work, opened as a card) and the
//! files it changed (opened as a card), then its timeline, whose last row is the TUI's own prompt
//! box, mirrored live (the prompts queued behind the turn, the draft, its pasted pictures). It
//! sends nothing to the agent: copying, opening a file and looking at a picture are its only
//! actions, and the TUI does the rest.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseDownEvent, ObjectFit, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task, WeakEntity, Window,
    canvas, div, img, prelude::FluentBuilder as _, px,
};
use slopty_view_doc::{MessagePart, MessageRole, PromptBox, PromptImage, SessionMessageEntry, WorkRow, WorkTray, calls};
use slopty_view_proto::{ChatIndicator, TodoItem, TodoStatus, ToolCall};

use crate::attachments::{AttachmentKey, AttachmentSnapshot, PreviewImage};
use crate::icons::{self, icon};
use crate::loaders::status_mark::{MARK_SMALL, StatusMark};
use crate::motion::{HoverFade as _, hover_blend};
use crate::state::AppState;
use crate::theme::Theme;
use crate::transcript::Transcript;
use crate::typography::ui_rems;

/// The height of each of the inspector's fixed lines: the header, Now and Changes.
pub(crate) const LINE: f32 = 28.0;
/// The inspector's side padding.
const PAD: f32 = 12.0;
/// The width of the card Now and Changes open to.
const CARD_WIDTH: f32 = 320.0;
/// The tallest a card's list grows before it scrolls.
const CARD_LIST: f32 = 360.0;
/// The most prompts queued behind the turn shown, the next to run first.
const QUEUED_SHOWN: usize = 4;
/// The most lines of the prompt box's draft shown before it ends in an ellipsis.
const DRAFT_LINES: usize = 6;
/// The side of a pasted picture's thumbnail.
const THUMB: f32 = 40.0;
/// The wash a pressable row fades to under the pointer.
const ROW_WASH: f32 = 0.03;

/// What a press in the inspector asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorEvent {
    /// Show `path`'s diff in Changes.
    OpenChange { path: String },
    /// Show the subagent `chat_id`'s own timeline.
    OpenSubagent { chat_id: String },
    /// Back from a subagent's timeline to its parent's.
    Back { chat_id: String },
}

/// One file the session changed, with its lines added and removed over the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangedFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// The files the session's tool calls edited or wrote, in the order first touched, each once,
/// with its counts summed. A write with no diff counts its lines as added.
pub(crate) fn changed_files(entries: &[SessionMessageEntry]) -> Vec<ChangedFile> {
    let mut out: Vec<ChangedFile> = Vec::new();
    let mut add = |path: &str, additions: u64, deletions: u64| {
        if let Some(file) = out.iter_mut().find(|file| file.path == path) {
            file.additions += additions;
            file.deletions += deletions;
        } else {
            out.push(ChangedFile { path: path.to_owned(), additions, deletions });
        }
    };
    for entry in entries.iter().filter(|entry| entry.role == MessageRole::Assistant) {
        for part in &entry.parts {
            let MessagePart::Tool { call, diff, diff_stats, is_error, .. } = part else { continue };
            if *is_error {
                continue;
            }
            if let Some(stats) = diff_stats.as_ref().filter(|stats| !stats.is_empty()) {
                for stat in stats {
                    add(&stat.path, stat.additions, stat.deletions);
                }
                continue;
            }
            if let Some(diff) = diff {
                let stat = slopty_view_doc::diff_stat(diff);
                add(&stat.path, stat.additions, stat.deletions);
                continue;
            }
            match call {
                ToolCall::WriteFile { path, content } => {
                    let lines = content.as_deref().map_or(0, |text| text.lines().count() as u64);
                    add(path, lines, 0);
                }
                ToolCall::EditFile { path, .. } | ToolCall::ApplyPatch { path: Some(path) } => {
                    add(path, 0, 0);
                }
                _ => {}
            }
        }
    }
    out
}

/// `path` as the session's folder `cwd` says it: inside it, relative to it; else whole. A folder
/// under the home folder (`~/…`) is matched in any home.
fn in_folder<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    let Some(cwd) = cwd.map(|cwd| cwd.trim_end_matches('/')).filter(|cwd| !cwd.is_empty()) else {
        return path;
    };
    let inside = |root: &str| path.strip_prefix(root)?.strip_prefix('/').filter(|rest| !rest.is_empty());
    match cwd.strip_prefix('~') {
        Some(under_home) => ["/Users/", "/home/"]
            .iter()
            .find_map(|home| {
                let user = path.strip_prefix(home)?.split('/').next()?;
                inside(&format!("{home}{user}{under_home}"))
            })
            .or_else(|| inside(&format!("/root{under_home}"))),
        None => inside(cwd),
    }
    .unwrap_or(path)
}

/// The task list the agent wrote last, unless it cleared it.
pub(crate) fn latest_tasks(entries: &[SessionMessageEntry]) -> Option<Vec<TodoItem>> {
    let items = entries
        .iter()
        .rev()
        .filter(|entry| entry.role == MessageRole::Assistant)
        .flat_map(|entry| entry.parts.iter().rev())
        .find_map(|part| match part {
            MessagePart::Tool { call: ToolCall::Todo { items }, .. } => Some(items),
            _ => None,
        })?;
    (!items.is_empty()).then(|| items.clone())
}

/// What the Now line says of `tasks` (the list the agent wrote last) and `running` (its
/// subagents and commands at work): "Tasks 2/4 · Write the tests" and "2 running", each when
/// there is any.
pub(crate) fn now_words(tasks: Option<&[TodoItem]>, running: usize) -> (Option<String>, Option<String>) {
    let tasks = tasks.map(|items| {
        let done = items.iter().filter(|item| item.status() == TodoStatus::Completed).count();
        let doing = items.iter().find(|item| item.status() == TodoStatus::InProgress);
        match doing {
            Some(item) => format!("Tasks {done}/{} · {}", items.len(), crate::transcript::single_line(&item.text)),
            None => format!("Tasks {done}/{}", items.len()),
        }
    });
    (tasks, (running > 0).then(|| format!("{running} running")))
}

/// Where the TUI's placeholders stand in the prompt box's `text`: a pasted picture's
/// (`[Image #2]`) and a long paste's (`[Pasted text #1 +5 lines]`), drawn apart from the words.
pub(crate) fn placeholders(text: &str) -> Vec<std::ops::Range<usize>> {
    const OPENS: [&str; 2] = ["[Image #", "[Pasted text #"];
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((start, open)) = OPENS
        .iter()
        .filter_map(|open| text[from..].find(open).map(|at| (from + at, open.len())))
        .min_by_key(|(at, _)| *at)
    {
        let rest = &text[start + open..];
        match rest.find([']', '[', '\n']) {
            Some(close) if rest.as_bytes()[close] == b']' => {
                let end = start + open + close + 1;
                out.push(start..end);
                from = end;
            }
            // Left open, it is words; look on from after its opening.
            _ => from = start + open,
        }
    }
    out
}

/// The card a fixed line opens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Card {
    /// The tasks and the running work.
    Now,
    /// The files changed.
    Changes,
}

/// What the inspector reads off the transcript, kept until it changes: worked out once per
/// revision, never per frame (`inspector.md`, "Frame time").
#[derive(Default)]
struct Facts {
    /// The revision and chat they were read at.
    read_at: Option<(u64, Option<String>)>,
    files: Arc<[ChangedFile]>,
    tasks: Option<Arc<[TodoItem]>>,
}

/// The inspector of the selected session.
pub struct Inspector {
    state: Entity<AppState>,
    transcript: Entity<Transcript>,
    /// The TUI's prompt box, the timeline's last row.
    mirror: Entity<PromptMirror>,
    /// The chat whose dock and work are watched.
    watched: Option<String>,
    tray: Option<Arc<WorkTray>>,
    watch: Vec<Task<()>>,
    /// The time it draws with, taken when anything it shows changes.
    clock: chrono::DateTime<chrono::Utc>,
    facts: Facts,
    /// The card open under its line, the keyboard's while it is (Esc closes it), where the
    /// keyboard goes back to when it closes, and where each line was last drawn (a press on
    /// the open card's own line shuts it, not shut and opened again).
    card: Option<Card>,
    card_focus: FocusHandle,
    card_return: Option<FocusHandle>,
    lines: [Rc<Cell<Bounds<Pixels>>>; 2],
    /// The picture open in the lightbox, the keyboard's while it is, and where the keyboard
    /// goes back to when it closes.
    preview: Option<PreviewImage>,
    preview_focus: FocusHandle,
    preview_return: Option<FocusHandle>,
    _observe: Subscription,
}

impl EventEmitter<InspectorEvent> for Inspector {}

impl Inspector {
    pub fn new(state: Entity<AppState>, transcript: Entity<Transcript>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| {
            this.sync_watch(cx);
            this.clock = crate::clock::utc(cx);
            cx.notify();
        });
        let weak = cx.weak_entity();
        let mirror = cx.new(|_| PromptMirror::new(state.clone(), weak));
        transcript.update(cx, |transcript, cx| transcript.set_footer(Some(mirror.clone().into()), cx));
        let mut inspector = Self {
            state,
            transcript,
            mirror,
            watched: None,
            tray: None,
            watch: Vec::new(),
            clock: crate::clock::utc(cx),
            facts: Facts::default(),
            card: None,
            card_focus: cx.focus_handle(),
            card_return: None,
            lines: [Rc::default(), Rc::default()],
            preview: None,
            preview_focus: cx.focus_handle(),
            preview_return: None,
            _observe: observe,
        };
        inspector.sync_watch(cx);
        inspector
    }

    /// The timeline it shows.
    pub fn transcript(&self) -> &Entity<Transcript> {
        &self.transcript
    }

    /// Keep the selected chat's dock and work watched; the watch follows the selection.
    fn sync_watch(&mut self, cx: &mut Context<Self>) {
        let chat = self.state.read(cx).selected_chat.clone();
        if self.watched == chat {
            return;
        }
        self.watched = chat.clone();
        self.tray = None;
        self.card = None;
        self.mirror.update(cx, |mirror, cx| mirror.show(None, cx));
        self.watch.clear();
        let (Some(chat), Some(engine)) = (chat, self.state.read(cx).engine().cloned()) else {
            return;
        };
        let params = serde_json::json!({ "chatId": chat });
        for method in [calls::WATCH_DOCK, calls::WATCH_WORK] {
            let (engine, params, chat) = (engine.clone(), params.clone(), chat.clone());
            self.watch.push(cx.spawn(async move |this, cx| {
                let Ok(mut rx) = engine.client().subscribe_checked(method, params).await else {
                    return;
                };
                while let Some(value) = rx.recv().await {
                    let alive = this.update(cx, |this, cx| {
                        if this.watched.as_deref() != Some(chat.as_str()) {
                            return;
                        }
                        if method == calls::WATCH_DOCK {
                            if let Ok(dock) = serde_json::from_value::<slopty_view_doc::ChatDock>(value) {
                                this.mirror.update(cx, |mirror, cx| mirror.show(dock.prompt, cx));
                            }
                        } else if let Ok(tray) = serde_json::from_value::<WorkTray>(value) {
                            this.tray = Some(Arc::new(tray));
                        }
                        this.clock = crate::clock::utc(cx);
                        cx.notify();
                    });
                    if alive.is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// The files changed and the task list, read again only once the transcript or the
    /// selection changed.
    fn facts(&mut self, cx: &Context<Self>) -> (Arc<[ChangedFile]>, Option<Arc<[TodoItem]>>) {
        let state = self.state.read(cx);
        let at = (state.transcript_revision, state.selected_chat.clone());
        if self.facts.read_at.as_ref() != Some(&at) {
            self.facts = Facts {
                files: changed_files(&state.transcript).into(),
                tasks: latest_tasks(&state.transcript).map(Into::into),
                read_at: Some(at),
            };
        }
        (self.facts.files.clone(), self.facts.tasks.clone())
    }

    /// Open `preview` in the lightbox, the keyboard with it until it closes.
    fn show_picture(&mut self, preview: PreviewImage, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_return = window.focused(cx);
        preview.viewer.reset();
        self.preview = Some(preview);
        window.focus(&self.preview_focus, cx);
        cx.notify();
    }

    /// Open `card` under its line, the keyboard with it, or shut it if it is the one open.
    fn toggle_card(&mut self, card: Card, window: &mut Window, cx: &mut Context<Self>) {
        if self.card == Some(card) {
            self.close_card(window, cx);
            return;
        }
        if self.card.is_none() {
            self.card_return = window.focused(cx);
        }
        self.card = Some(card);
        window.focus(&self.card_focus, cx);
        cx.notify();
    }

    /// Shut the open card, the keyboard back where it was if the card still holds it.
    fn close_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.card.take().is_none() {
            return;
        }
        if let Some(back) = self.card_return.take()
            && self.card_focus.is_focused(window)
        {
            window.focus(&back, cx);
        }
        cx.notify();
    }

    /// One of the fixed lines: `id`, its height [`LINE`] whatever it says, truncating, never
    /// wrapping; a press opens `card` when there is one, and the line keeps where it was drawn.
    fn line(&self, id: &'static str, card: Option<Card>, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let open = card.is_some_and(|card| self.card == Some(card));
        let at = card.map(|card| self.lines[card as usize].clone());
        div()
            .id(id)
            .debug_selector(move || id.into())
            .relative()
            .flex_none()
            .h(px(LINE))
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(PAD))
            .overflow_hidden()
            .border_b_1()
            .border_color(theme.border)
            .whitespace_nowrap()
            .when_some(card, |line, card| {
                let fade = format!("{id}-press");
                line.role(gpui::Role::Button)
                    .aria_expanded(open)
                    .cursor_pointer()
                    .bg(if open {
                        theme.ink(ROW_WASH)
                    } else {
                        hover_blend(&fade, gpui::transparent_black(), theme.ink(ROW_WASH))
                    })
                    .hover_fade(fade)
                    .on_click(cx.listener(move |this, _, window, cx| this.toggle_card(card, window, cx)))
            })
            .children(at.map(|at| {
                canvas(move |bounds, _, _| at.set(bounds), |_, _, _, _| ()).absolute().inset_0()
            }))
    }

    /// The trailing chevron of a line that opens a card.
    fn chevron(open: bool, theme: &Theme) -> AnyElement {
        icon(if open { icons::ALT_ARROW_UP } else { icons::ALT_ARROW_DOWN })
            .size(px(11.0))
            .flex_none()
            .text_color(theme.text_faint)
            .into_any_element()
    }

    /// A line's name, quieter than what it says.
    fn name(name: &'static str, theme: &Theme) -> AnyElement {
        div()
            .flex_none()
            .text_size(ui_rems(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text_muted)
            .child(name)
            .into_any_element()
    }

    /// The card `card` opens to, under its line: the keyboard's while open, shut by Esc or a
    /// press outside it (a press on its own line shuts it through the line).
    fn card(&self, card: Card, content: Vec<AnyElement>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let line = self.lines[card as usize].clone();
        let body = crate::popover::popover_card(theme)
            .id(match card {
                Card::Now => "inspector-now-card",
                Card::Changes => "inspector-changes-card",
            })
            .track_focus(&self.card_focus)
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_card(window, cx);
                }
            }))
            .on_mouse_down_out(cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                if !line.get().contains(&event.position) {
                    this.close_card(window, cx);
                }
            }))
            .child(
                div()
                    .id("inspector-card-list")
                    .flex()
                    .flex_col()
                    .max_h(px(CARD_LIST))
                    .overflow_y_scroll()
                    .children(content),
            );
        crate::popover::anchored_menu_below_gap("inspector-card", body.into_any_element(), None, 0.0)
    }

    /// The session's status and branch, on one line that is always drawn; a subagent's timeline
    /// adds its task's name and the way back to its parent. The session's own title is not said
    /// again here: its terminal's title bar and its sidebar row say it.
    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row().cloned();
        let status = chat.as_ref().map_or(ChatIndicator::Idle, |chat| state.display_status_for(chat, self.clock));
        // slopty: the one state vocabulary, its need's own glyph included (`layout.md`, "A state
        // is a glyph").
        let session = chat.as_ref().and_then(|chat| state.session_for(&chat.id));
        let mark = crate::shell::marks::Mark::of(status, session, self.clock);
        let parent = chat
            .as_ref()
            .filter(|chat| chat.subagent)
            .and_then(|chat| chat.parent_chat_id.clone());
        let title: SharedString = chat
            .as_ref()
            .and_then(|chat| chat.title.clone().or_else(|| chat.last_message_preview.clone()))
            .map_or_else(|| "Session".into(), |title| crate::transcript::single_line(&title).into());
        let branch = chat.as_ref().and_then(|chat| chat.branch.clone());
        let task = parent.is_some().then_some(title);
        self.line("inspector-header", None, theme, cx)
            .gap(px(10.0))
            .when_some(parent, |line, parent| {
                let ink = hover_blend("inspector-back", theme.text_muted, theme.text);
                line.child(
                    div()
                        .id("inspector-back")
                        .role(gpui::Role::Button)
                        .flex_none()
                        .cursor_pointer()
                        .hover_fade("inspector-back")
                        .aria_label("Back to its parent")
                        .tooltip(crate::settings::widgets::text_tooltip("Back to its parent"))
                        // An SVG paints only in a colour of its own.
                        .child(icon(icons::ARROW_LEFT).size(px(12.0)).text_color(ink))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(InspectorEvent::Back { chat_id: parent.clone() });
                        })),
                )
            })
            .children(task.map(|title| {
                div()
                    .id("inspector-title")
                    .debug_selector(|| "inspector-title".into())
                    .role(gpui::Role::Heading)
                    .aria_label(title.clone())
                    .min_w_0()
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title)
            }))
            .children(mark.word().zip(mark.drawn()).map(|(words, mark)| {
                div()
                    .id("inspector-status")
                    .role(gpui::Role::Status)
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.0))
                    .aria_label(words)
                    .child(StatusMark::new("inspector-status-mark", mark).size(MARK_SMALL))
                    .child(div().text_size(ui_rems(12.0)).text_color(theme.text).child(words))
            }))
            .children(branch.map(|branch| {
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap(px(3.0))
                    .child(icon(icons::GIT_BRANCH).size(px(11.0)).flex_none().text_color(theme.text_faint))
                    .child(div().min_w_0().truncate().text_size(ui_rems(12.0)).text_color(theme.text_muted).child(branch))
            }))
            .into_any_element()
    }

    /// What the session is doing now, on one line ("Tasks 2/4 · Write the tests", "2
    /// running"), opening to its task list and its running work.
    fn render_now(&self, tasks: Option<&[TodoItem]>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let working = self
            .state
            .read(cx)
            .selected_chat_row()
            .is_some_and(|chat| self.state.read(cx).display_status_for(chat, self.clock) == ChatIndicator::Working);
        // A list all done leaves once the turn ends, as the TUI's own does.
        let tasks = tasks.filter(|items| working || items.iter().any(|item| item.status() != TodoStatus::Completed));
        let running: &[WorkRow] = self.tray.as_ref().map(|tray| tray.rows.as_slice()).unwrap_or_default();
        let (said, at_work) = now_words(tasks, running.len());
        let has = tasks.is_some() || !running.is_empty();
        let open = has && self.card == Some(Card::Now);
        let line = self
            .line("inspector-now", has.then_some(Card::Now), theme, cx)
            .aria_label(SharedString::from(match (&said, &at_work) {
                (Some(said), Some(at_work)) => format!("Now: {said}, {at_work}"),
                (Some(said), None) | (None, Some(said)) => format!("Now: {said}"),
                (None, None) => "Now: nothing".to_owned(),
            }))
            .child(Self::name("Now", theme))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_rems(12.0))
                    .text_color(if said.is_some() { theme.text } else { theme.text_faint })
                    .child(said.clone().unwrap_or_else(|| if at_work.is_some() { String::new() } else { "Nothing".to_owned() })),
            )
            .children(at_work.map(|words| {
                div().flex_none().text_size(ui_rems(11.0)).text_color(theme.text_muted).child(words)
            }))
            .when(has, |line| line.child(Self::chevron(open, theme)));
        if !open {
            return line.into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(items) = tasks {
            for (ix, item) in items.iter().enumerate() {
                let (glyph, color, text) = match item.status() {
                    TodoStatus::Completed => (icons::CHECK, theme.success, theme.text_faint),
                    TodoStatus::InProgress => (icons::ARROW_RIGHT, theme.accent, theme.text),
                    TodoStatus::Pending => (icons::LIST, theme.text_faint, theme.text_muted),
                };
                let words = item.text.clone();
                rows.push(
                    div()
                        .id(("inspector-task", ix))
                        .role(gpui::Role::ListItem)
                        .aria_label(SharedString::from(match item.status() {
                            TodoStatus::Completed => format!("Done: {words}"),
                            TodoStatus::InProgress => format!("Doing: {words}"),
                            TodoStatus::Pending => format!("To do: {words}"),
                        }))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .min_h(px(22.0))
                        .px(px(6.0))
                        .child(icon(glyph).size(px(11.0)).flex_none().text_color(color))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_rems(12.0))
                                .text_color(text)
                                .when(item.status() == TodoStatus::Completed, |el| el.line_through())
                                .child(crate::transcript::single_line(&words)),
                        )
                        .into_any_element(),
                );
            }
        }
        for (ix, row) in running.iter().enumerate() {
            let glyph = if row.kind == WorkRow::SUBAGENT { icons::BOT } else { icons::TERMINAL };
            let detail = row.step.clone().unwrap_or_else(|| row.state_words.clone());
            let chat_id = row.chat_id.clone();
            let fade = format!("inspector-work-{ix}");
            rows.push(
                div()
                    .id(("inspector-work", ix))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_h(px(24.0))
                    .px(px(6.0))
                    .rounded(px(crate::popover::MENU_ITEM_RADIUS))
                    .role(if chat_id.is_some() { gpui::Role::Button } else { gpui::Role::ListItem })
                    .when(chat_id.is_some(), |el| {
                        el.cursor_pointer()
                            .bg(hover_blend(&fade, gpui::transparent_black(), theme.ink(ROW_WASH)))
                            .hover_fade(fade)
                    })
                    .aria_label(SharedString::from(format!("{} {}", row.noun, row.title)))
                    .child(icon(glyph).size(px(12.0)).flex_none().text_color(theme.text_muted))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(ui_rems(12.0))
                            .text_color(theme.text)
                            .child(crate::transcript::single_line(&row.title)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(140.0))
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.text_faint)
                            .child(crate::transcript::single_line(&detail)),
                    )
                    .when_some(chat_id, |el, chat_id| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.close_card(window, cx);
                            cx.emit(InspectorEvent::OpenSubagent { chat_id: chat_id.clone() });
                        }))
                    })
                    .into_any_element(),
            );
        }
        line.child(self.card(Card::Now, rows, theme, cx)).into_any_element()
    }

    /// The files the session changed, on one line ("Changes 3 files +12 −4"), opening to the
    /// files, each a press away from its diff.
    fn render_changes(&self, files: &[ChangedFile], theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let count = files.len();
        let has = count > 0;
        let open = has && self.card == Some(Card::Changes);
        let (adds, dels) = files.iter().fold((0, 0), |(a, d), file| (a + file.additions, d + file.deletions));
        let counts = |additions: u64, deletions: u64| {
            div()
                .flex()
                .flex_none()
                .gap(px(4.0))
                .font_family(theme.font_mono.clone())
                .text_size(ui_rems(11.0))
                .when(additions > 0, |el| el.child(div().text_color(theme.diff_add).child(format!("+{additions}"))))
                .when(deletions > 0, |el| el.child(div().text_color(theme.diff_del).child(format!("\u{2212}{deletions}"))))
        };
        let said = match count {
            0 => "None".to_owned(),
            1 => "1 file".to_owned(),
            _ => format!("{count} files"),
        };
        let line = self
            .line("inspector-changes", has.then_some(Card::Changes), theme, cx)
            .aria_label(SharedString::from(format!("Changes: {said}, +{adds} \u{2212}{dels}")))
            .child(Self::name("Changes", theme))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_rems(12.0))
                    .text_color(if has { theme.text } else { theme.text_faint })
                    .child(said),
            )
            .child(counts(adds, dels))
            .when(has, |line| line.child(Self::chevron(open, theme)));
        if !open {
            return line.into_any_element();
        }
        let cwd = self.state.read(cx).selected_chat_row().and_then(|chat| chat.cwd.clone());
        let rows = files
            .iter()
            .enumerate()
            .map(|(ix, file)| {
                let said = in_folder(&file.path, cwd.as_deref());
                let (dir, name) = match said.rsplit_once('/') {
                    Some((dir, name)) => (Some(dir.to_owned()), name.to_owned()),
                    None => (None, said.to_owned()),
                };
                let path = file.path.clone();
                let fade = format!("inspector-file-{ix}");
                div()
                    .id(("inspector-file", ix))
                    .role(gpui::Role::Button)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_h(px(24.0))
                    .px(px(6.0))
                    .rounded(px(crate::popover::MENU_ITEM_RADIUS))
                    .cursor_pointer()
                    .bg(hover_blend(&fade, gpui::transparent_black(), theme.ink(ROW_WASH)))
                    .hover_fade(fade)
                    .aria_label(SharedString::from(format!("Open the diff of {said}")))
                    .child(
                        crate::file_icons::icon(crate::file_icons::FileIconIdentity::file(&file.path), theme.appearance)
                            .size(px(12.0))
                            .flex_none(),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .gap(px(6.0))
                            .overflow_hidden()
                            .child(div().flex_none().text_size(ui_rems(12.0)).text_color(theme.text).child(name))
                            .children(dir.map(|dir| {
                                div().min_w_0().truncate().text_size(ui_rems(11.0)).text_color(theme.text_faint).child(dir)
                            })),
                    )
                    .child(counts(file.additions, file.deletions))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_card(window, cx);
                        cx.emit(InspectorEvent::OpenChange { path: path.clone() });
                    }))
                    .into_any_element()
            })
            .collect();
        line.child(self.card(Card::Changes, rows, theme, cx)).into_any_element()
    }
}

/// The agent's prompt box as its TUI holds it, mirrored live as the timeline's last row, as the
/// TUI keeps it under the conversation ([`Transcript::set_footer`]): the prompts queued behind
/// the turn under way, then the draft, its placeholders drawn apart, and the pictures pasted
/// into it, each a press away from the inspector's lightbox. It is drawn only while the box holds
/// something, with no caret: what it says is typed in the TUI, and nothing here types.
pub struct PromptMirror {
    state: Entity<AppState>,
    /// The inspector whose lightbox a picture opens in.
    inspector: WeakEntity<Inspector>,
    prompt: Option<PromptBox>,
    /// The reads of the pictures under way, and the wake-ups that try a failed one again, by
    /// their paths.
    picture_loads: HashMap<String, Task<()>>,
}

impl PromptMirror {
    fn new(state: Entity<AppState>, inspector: WeakEntity<Inspector>) -> Self {
        Self { state, inspector, prompt: None, picture_loads: HashMap::new() }
    }

    /// Mirror `prompt`, as the TUI's box holds it now.
    fn show(&mut self, prompt: Option<PromptBox>, cx: &mut Context<Self>) {
        let prompt = prompt.filter(|prompt| !prompt.is_empty());
        if self.prompt != prompt {
            self.prompt = prompt;
            cx.notify();
        }
    }

    /// Picture `path` of the prompt box, as the timeline reads its own: from the chat's device,
    /// through the same cache, read once and tried again after a failure's wait.
    fn picture(&mut self, path: &str, cx: &mut Context<Self>) -> AttachmentSnapshot {
        use crate::attachments::{attachment_snapshot_for, begin_load_for};
        let state = self.state.read(cx);
        let local = state.local_device_id.clone();
        let Some(device) = state.selected_chat_row().map(|chat| chat.device_id.clone()).or_else(|| local.clone())
        else {
            return AttachmentSnapshot::Error { retry_in: std::time::Duration::MAX };
        };
        let key = AttachmentKey::new(&device, path, None);
        if begin_load_for(&key) {
            self.load_picture(key.clone(), path, (local.as_deref() != Some(device.as_str())).then_some(device), cx);
        }
        let snapshot = attachment_snapshot_for(&key);
        if let AttachmentSnapshot::Error { retry_in } = snapshot
            && retry_in != std::time::Duration::MAX
            && !self.picture_loads.contains_key(path)
        {
            let wake = path.to_owned();
            self.picture_loads.insert(
                wake.clone(),
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(retry_in + std::time::Duration::from_millis(60)).await;
                    let _gone = this.update(cx, |this, cx| {
                        this.picture_loads.remove(&wake);
                        cx.notify();
                    });
                }),
            );
        }
        snapshot
    }

    /// Read the picture `key` names, from `target` when it is another device's.
    fn load_picture(&mut self, key: AttachmentKey, path: &str, target: Option<String>, cx: &mut Context<Self>) {
        use crate::attachments::{AttachmentLoadGuard, read_attachment_image, store_error_for, store_loaded_for};
        let Some(engine) = self.state.read(cx).engine().cloned() else {
            store_error_for(&key);
            return;
        };
        let (claim, path) = (AttachmentLoadGuard(key.clone()), path.to_owned());
        let read = path.clone();
        let task = cx.spawn(async move |this, cx| {
            let _claim = claim;
            match read_attachment_image(&engine, cx.background_executor(), target.as_deref(), &read, None).await {
                Some(loaded) => store_loaded_for(&key, loaded.name.into(), loaded.image),
                None => store_error_for(&key),
            }
            let _gone = this.update(cx, |this, cx| {
                this.picture_loads.remove(&read);
                cx.notify();
            });
        });
        self.picture_loads.insert(path, task);
    }

    /// A picture pasted into the prompt box, as a thumbnail: once its bytes are here, a press
    /// opens it in the inspector's lightbox; until then, a still wash; one that cannot be read,
    /// a dashed frame.
    fn render_picture(&self, picture: &PromptImage, snapshot: AttachmentSnapshot, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = SharedString::from(format!("Image #{}", picture.number));
        let id = SharedString::from(format!("inspector-picture-{}", picture.path));
        let frame = div().id(id.clone()).flex_none().size(px(THUMB)).rounded(px(6.0)).overflow_hidden().border_1();
        match snapshot {
            AttachmentSnapshot::Loaded(image) => {
                let preview = PreviewImage::new(name.clone(), image.image.clone());
                frame
                    .role(gpui::Role::Button)
                    .aria_label(name.clone())
                    .border_color(hover_blend(&id, theme.hairline(0.11), theme.hairline(0.28)))
                    .bg(theme.ink(0.035))
                    .cursor_pointer()
                    .hover_fade(id)
                    .tooltip(crate::settings::widgets::text_tooltip(format!("Open {name}")))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let preview = preview.clone();
                        let _gone = this.inspector.update(cx, |inspector, cx| inspector.show_picture(preview, window, cx));
                    }))
                    .child(img(image.image).size(px(THUMB - 2.0)).rounded(px(5.0)).object_fit(ObjectFit::Cover))
                    .into_any_element()
            }
            AttachmentSnapshot::Loading => frame
                .role(gpui::Role::Image)
                .aria_label(name)
                .border_color(theme.hairline(0.08))
                .bg(theme.ink(0.055))
                .into_any_element(),
            AttachmentSnapshot::Error { .. } => frame
                .role(gpui::Role::Image)
                .aria_label(SharedString::from(format!("{name}, unavailable")))
                .border_dashed()
                .border_color(theme.hairline(0.14))
                .bg(theme.ink(0.025))
                .into_any_element(),
        }
    }
}

impl gpui::Render for PromptMirror {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(prompt) = self.prompt.clone() else {
            return div().into_any_element();
        };
        let theme = Theme::of(cx).clone();
        let queued = (!prompt.queued.is_empty()).then(|| format!("{} queued", prompt.queued.len()));
        let mut section = div()
            .id("inspector-prompt")
            .debug_selector(|| "inspector-prompt".into())
            .role(gpui::Role::Group)
            .aria_label("Prompt box")
            .flex()
            .flex_col()
            .mt(px(8.0))
            .pt(px(4.0))
            .border_t_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(24.0))
                    .child(Inspector::name("Prompt", &theme))
                    .children(queued.map(|queued| {
                        div().text_size(ui_rems(11.0)).text_color(theme.text_faint).child(queued)
                    })),
            );
        for (ix, words) in prompt.queued.iter().take(QUEUED_SHOWN).enumerate() {
            let line = crate::transcript::single_line(words);
            section = section.child(
                div()
                    .id(("inspector-queued", ix))
                    .role(gpui::Role::ListItem)
                    .aria_label(SharedString::from(format!("Queued: {line}")))
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_h(px(20.0))
                    .text_size(ui_rems(12.0))
                    .child(div().flex_none().text_color(theme.text_faint).child("\u{276F}"))
                    .child(div().min_w_0().truncate().text_color(theme.text_muted).child(line)),
            );
        }
        let later = prompt.queued.len().saturating_sub(QUEUED_SHOWN);
        if later > 0 {
            section = section.child(
                div().pl(px(14.0)).text_size(ui_rems(11.0)).text_color(theme.text_faint).child(format!("{later} more")),
            );
        }
        let text = prompt.text.trim_end();
        if !text.trim().is_empty() {
            let text = SharedString::from(text.to_owned());
            let marks = placeholders(&text);
            section = section.child(
                div()
                    .id("inspector-draft")
                    .role(gpui::Role::Paragraph)
                    .aria_label(text.clone())
                    .when(!prompt.queued.is_empty(), |el| el.mt(px(4.0)))
                    .overflow_hidden()
                    .line_clamp(DRAFT_LINES)
                    .text_ellipsis()
                    .text_size(ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(crate::popover::search_highlight_ranges(text, marks, &theme)),
            );
        }
        if !prompt.images.is_empty() {
            let mut strip = div().flex().flex_wrap().gap(px(6.0)).pt(px(6.0));
            for picture in &prompt.images {
                let snapshot = self.picture(&picture.path, cx);
                strip = strip.child(self.render_picture(picture, snapshot, &theme, cx));
            }
            section = section.child(strip);
        }
        section.into_any_element()
    }
}

impl gpui::Render for Inspector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let (files, tasks) = self.facts(cx);
        let header = self.render_header(&theme, cx);
        let now = self.render_now(tasks.as_deref(), &theme, cx);
        let changes = self.render_changes(&files, &theme, cx);
        div()
            .id("inspector")
            .debug_selector(|| "inspector".into())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .child(now)
            .child(changes)
            .child(
                div()
                    .id("inspector-timeline")
                    .debug_selector(|| "inspector-timeline".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.clone()),
            )
            .children(self.preview.clone().map(|preview| {
                let weak = cx.weak_entity();
                crate::attachments::lightbox(
                    window,
                    &preview,
                    &self.preview_focus,
                    move |window, cx| {
                        let back = weak.update(cx, |this, cx| {
                            this.preview = None;
                            cx.notify();
                            this.preview_return.take()
                        });
                        if let Ok(Some(focus)) = back {
                            window.focus(&focus, cx);
                        }
                    },
                    cx,
                )
            }))
    }
}

/// The chat a plain terminal is in the sidebar: `terminal:<session>` (the view model's
/// `TERMINAL_CHAT`). It has no record to inspect.
pub const TERMINAL_CHAT: &str = "terminal:";

/// Whether chat `chat` is an agent's session rather than a plain terminal.
pub fn is_agent_session(chat: &str) -> bool {
    !chat.starts_with(TERMINAL_CHAT)
}

/// Whether the inspector serves chat `chat`: a Claude Code session, whose record Slopty reads.
/// A plain terminal has none, and Codex's and pi's sessions are their rows alone.
pub fn inspects(state: &AppState, chat: &str) -> bool {
    use slopty_view_proto::HarnessId;
    is_agent_session(chat)
        && state
            .chats
            .iter()
            .find(|row| row.id == chat)
            .and_then(|row| row.config.as_ref())
            .is_none_or(|config| matches!(config.harness, HarnessId::CLAUDE_CODE | HarnessId::MOCK))
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::ToolDiffStat;

    fn assistant(parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry {
            id: "m".to_owned(),
            role: MessageRole::Assistant,
            parts,
            created_at: 0,
            device_id: "d".to_owned(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        }
    }

    fn edit(path: &str, additions: u64, deletions: u64) -> MessagePart {
        MessagePart::Tool {
            id: "t".to_owned(),
            call: ToolCall::EditFile { path: path.to_owned(), old_string: None, new_string: None },
            is_error: false,
            resolved: true,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: Some(vec![ToolDiffStat { path: path.to_owned(), additions, deletions }]),
            subagent_ref: None,
            subagent_status: None,
            subagent_tail: None,
            facts: None,
        }
    }

    /// A changed file is said from the session's folder, however that folder is written; one
    /// outside it, or a folder only sharing its first letters, is said whole.
    #[test]
    fn a_changed_file_is_said_from_the_sessions_folder() {
        assert_eq!(in_folder("/Users/me/app/src/a.rs", Some("/Users/me/app")), "src/a.rs");
        assert_eq!(in_folder("/Users/me/app/src/a.rs", Some("~/app/")), "src/a.rs");
        assert_eq!(in_folder("/home/me/app/a.rs", Some("~/app")), "a.rs");
        assert_eq!(in_folder("/Users/me/apples/a.rs", Some("/Users/me/app")), "/Users/me/apples/a.rs");
        assert_eq!(in_folder("/srv/a.rs", None), "/srv/a.rs");
        assert_eq!(in_folder("src/a.rs", Some("/Users/me/app")), "src/a.rs");
    }

    #[test]
    fn a_file_changed_twice_is_one_row_with_its_counts_summed() {
        let entries = [assistant(vec![edit("a.rs", 2, 1), edit("b.rs", 5, 0)]), assistant(vec![edit("a.rs", 3, 4)])];
        let files = changed_files(&entries);
        assert_eq!(
            files,
            [
                ChangedFile { path: "a.rs".into(), additions: 5, deletions: 5 },
                ChangedFile { path: "b.rs".into(), additions: 5, deletions: 0 },
            ]
        );
    }

    fn task(text: &str, status: TodoStatus) -> TodoItem {
        TodoItem::new(text, status)
    }

    /// The Now line says the tasks done of all and the one under way, then what runs; with
    /// nothing, it says nothing, and the line stays.
    #[test]
    fn the_now_line_says_the_tasks_and_what_runs() {
        let items = [
            task("Read", TodoStatus::Completed),
            task("Write the tests", TodoStatus::InProgress),
            task("Ship", TodoStatus::Pending),
        ];
        assert_eq!(
            now_words(Some(&items), 2),
            (Some("Tasks 1/3 · Write the tests".to_owned()), Some("2 running".to_owned()))
        );
        assert_eq!(now_words(Some(&items[..1]), 0), (Some("Tasks 1/1".to_owned()), None));
        assert_eq!(now_words(None, 1), (None, Some("1 running".to_owned())));
        assert_eq!(now_words(None, 0), (None, None));
    }

    /// The TUI's placeholders are found wherever they stand, a picture's and a long paste's
    /// alike; brackets of the person's own, or one left open, are words.
    #[test]
    fn the_prompt_boxs_placeholders_are_drawn_apart_from_its_words() {
        let text = "Look at [Image #1] and [Pasted text #2 +5 lines], not [this] or [Image #3";
        let found: Vec<&str> = placeholders(text).into_iter().map(|range| &text[range]).collect();
        assert_eq!(found, ["[Image #1]", "[Pasted text #2 +5 lines]"]);
        assert_eq!(placeholders("[Image #2][Image #1]").len(), 2, "side by side");
        let open = "[Image #3 then [Image #1]";
        let found: Vec<&str> = placeholders(open).into_iter().map(|range| &open[range]).collect();
        assert_eq!(found, ["[Image #1]"], "one left open never swallows the next");
        assert!(placeholders("[Image #4\n]").is_empty(), "a placeholder is one line");
        assert!(placeholders("no pictures").is_empty());
        assert!(placeholders("").is_empty());
    }
}
