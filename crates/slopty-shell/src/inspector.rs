//! slopty: the Inspector, a read-only view of the selected session beside its terminal
//! (`docs/decisions/ui.md`, "A session is its terminal, its inspector beside it").
//!
//! The person works in the agent's own TUI; the inspector keeps only what helps inspect the
//! session, compact and quiet: a header (title, status, branch, time at work), what it is doing
//! now (its task list, the subagents and commands it has running), the files it changed with
//! their counts, its timeline, each turn rendered as the transcript renders it with tool calls
//! folded to a line and opened whole, and under it the TUI's own prompt box, mirrored live (the
//! prompts queued behind the turn, the draft, its pasted pictures). It sends nothing to the
//! agent: copying, opening a file and looking at a picture are its only actions, and the TUI does
//! the rest.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Subscription, Task, Window, div, img, prelude::FluentBuilder as _, px,
};
use slopty_view_doc::{
    ChatDock, MessagePart, MessageRole, PromptImage, SessionMessageEntry, WorkRow, WorkTray, calls,
};
use slopty_view_proto::{ChatIndicator, TodoItem, TodoStatus, ToolCall};

use crate::attachments::{AttachmentKey, AttachmentSnapshot, PreviewImage};
use crate::icons::{self, icon};
use crate::loaders::status_mark::{MARK_SMALL, MarkState, StatusMark};
use crate::motion::{HoverFade as _, hover_blend};
use crate::state::AppState;
use crate::theme::Theme;
use crate::transcript::Transcript;
use crate::typography::ui_rems;

/// The most tasks the list shows before it folds to the ones around the current task.
const TASKS_SHOWN: usize = 6;
/// The most changed files shown before "N more".
const FILES_SHOWN: usize = 8;
/// The inspector's side padding.
const PAD: f32 = 12.0;
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

/// The tasks a list of `total` shows around the current one, `current`: all of a short list,
/// else [`TASKS_SHOWN`] from one before the current one, held to the list's end.
pub(crate) fn task_window(total: usize, current: Option<usize>) -> std::ops::Range<usize> {
    if total <= TASKS_SHOWN {
        return 0..total;
    }
    let start = current.unwrap_or(total - 1).saturating_sub(1).min(total - TASKS_SHOWN);
    start..start + TASKS_SHOWN
}

/// The words a session's status says beside its mark; nothing for one at rest.
const fn status_words(status: ChatIndicator) -> Option<(&'static str, MarkState)> {
    match status {
        ChatIndicator::Working => Some(("Working", MarkState::Working)),
        ChatIndicator::AwaitingInput => Some(("Needs you", MarkState::Waiting)),
        ChatIndicator::Completed => Some(("Done", MarkState::Done)),
        ChatIndicator::Errored => Some(("Failed", MarkState::Failed)),
        ChatIndicator::Idle => None,
    }
}

/// A span of time as the header says it: `42s`, `3m 12s`, `1h 4m`.
pub(crate) fn elapsed_words(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
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

/// The inspector of the selected session.
pub struct Inspector {
    state: Entity<AppState>,
    transcript: Entity<Transcript>,
    /// The chat whose dock and work are watched.
    watched: Option<String>,
    dock: Option<Arc<ChatDock>>,
    tray: Option<Arc<WorkTray>>,
    watch: Vec<Task<()>>,
    /// The changed files shown whole, past [`FILES_SHOWN`].
    files_open: bool,
    /// The time it draws with, taken when anything it shows changes and at each whole second
    /// of the time at work: a frame drawn between says what the last one did.
    clock: chrono::DateTime<chrono::Utc>,
    /// Takes the time again at the next whole second of the time at work, while the session
    /// works.
    tick: Option<Task<()>>,
    /// The reads of the prompt box's pictures under way, and the wake-ups that try a failed one
    /// again, by their paths.
    picture_loads: HashMap<String, Task<()>>,
    /// The picture open in the lightbox, the keyboard's while it is, and where the keyboard
    /// goes back to when it closes.
    preview: Option<PreviewImage>,
    preview_focus: gpui::FocusHandle,
    preview_return: Option<gpui::FocusHandle>,
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
        let mut inspector = Self {
            state,
            transcript,
            watched: None,
            dock: None,
            tray: None,
            watch: Vec::new(),
            files_open: false,
            clock: crate::clock::utc(cx),
            tick: None,
            picture_loads: HashMap::new(),
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
        self.dock = None;
        self.tray = None;
        self.files_open = false;
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
                            if let Ok(dock) = serde_json::from_value::<ChatDock>(value) {
                                this.dock = Some(Arc::new(dock));
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

    /// While the session works, take the time again and draw at the next whole second of its
    /// time at work since `since` (in a second without one), when what the header says moves
    /// on; the transcript's working line ticks the same way.
    fn keep_ticking(&mut self, working: bool, since: Option<chrono::DateTime<chrono::Utc>>, cx: &mut Context<Self>) {
        if !working {
            self.tick = None;
            return;
        }
        if self.tick.is_some() {
            return;
        }
        let real = crate::clock::utc(cx);
        let into = since.map_or(0, |since| (real - since).num_milliseconds().rem_euclid(1000));
        let wait = std::time::Duration::from_millis(u64::try_from(1000 - into).unwrap_or(1000).clamp(1, 1000) + 2);
        self.tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _gone = this.update(cx, |this, cx| {
                this.tick = None;
                this.clock = crate::clock::utc(cx);
                cx.notify();
            });
        }));
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

    /// Open `preview` in the lightbox, the keyboard with it until it closes.
    fn show_picture(&mut self, preview: PreviewImage, window: &mut Window, cx: &mut Context<Self>) {
        self.preview_return = window.focused(cx);
        preview.viewer.reset();
        self.preview = Some(preview);
        window.focus(&self.preview_focus, cx);
        cx.notify();
    }

    fn render_header(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = self.clock;
        let state = self.state.read(cx);
        let chat = state.selected_chat_row().cloned();
        let status = chat.as_ref().map_or(ChatIndicator::Idle, |chat| state.display_status_for(chat, now));
        let session = chat.as_ref().and_then(|chat| state.session_for(&chat.id)).cloned();
        let parent = chat
            .as_ref()
            .filter(|chat| chat.subagent)
            .and_then(|chat| chat.parent_chat_id.clone());
        let title: SharedString = chat
            .as_ref()
            .and_then(|chat| chat.title.clone().or_else(|| chat.last_message_preview.clone()))
            .map_or_else(|| "Session".into(), |title| crate::transcript::single_line(&title).into());
        let branch = chat.as_ref().and_then(|chat| chat.branch.clone());
        let working = status == ChatIndicator::Working;
        let since = session.as_ref().and_then(|session| session.started_at).filter(|_| working);
        self.keep_ticking(working, since, cx);
        let at_work = since.map(|since| elapsed_words((now - since).num_milliseconds()));

        let mut meta: Vec<AnyElement> = Vec::new();
        let quiet = |text: String| {
            div()
                .flex_none()
                .text_size(ui_rems(12.0))
                .text_color(theme.text_muted)
                .child(text)
                .into_any_element()
        };
        if let Some((words, mark)) = status_words(status) {
            meta.push(
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
                    .into_any_element(),
            );
        }
        if let Some(branch) = branch {
            meta.push(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(3.0))
                    .child(icon(icons::GIT_BRANCH).size(px(11.0)).text_color(theme.text_faint))
                    .child(quiet(branch))
                    .into_any_element(),
            );
        }
        if let Some(at_work) = at_work {
            meta.push(quiet(at_work));
        }
        let mut row = div().flex().flex_wrap().items_center().gap_x(px(10.0)).gap_y(px(2.0)).mt(px(4.0));
        for item in meta {
            row = row.child(item);
        }
        div()
            .id("inspector-header")
            .debug_selector(|| "inspector-header".into())
            .flex_none()
            .px(px(PAD))
            .pt(px(10.0))
            .pb(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .when_some(parent, |el, parent| {
                el.child(
                    div()
                        .id("inspector-back")
                        .role(gpui::Role::Button)
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .mb(px(4.0))
                        .cursor_pointer()
                        .text_size(ui_rems(12.0))
                        .text_color(hover_blend("inspector-back", theme.text_muted, theme.text))
                        .hover_fade("inspector-back")
                        .aria_label("Back to its parent")
                        .child(icon(icons::ARROW_LEFT).size(px(12.0)))
                        .child("Parent session")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(InspectorEvent::Back { chat_id: parent.clone() });
                        })),
                )
            })
            .child(
                div()
                    .id("inspector-title")
                    .role(gpui::Role::Heading)
                    .aria_label(title.clone())
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(title),
            )
            .child(row)
            .into_any_element()
    }

    /// A section's heading: its name and, quieter, its count.
    fn heading(name: &'static str, count: Option<String>, theme: &Theme) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .h(px(24.0))
            .text_size(ui_rems(11.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text_muted)
            .child(name)
            .children(count.map(|count| div().text_color(theme.text_faint).child(count)))
    }

    /// What the session is doing now: its phrase, its tasks, its running work.
    fn render_now(&mut self, entries: &[SessionMessageEntry], theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let working = self
            .state
            .read(cx)
            .selected_chat_row()
            .is_some_and(|chat| self.state.read(cx).display_status_for(chat, self.clock) == ChatIndicator::Working);
        // A list all done leaves once the turn ends, as the TUI's own does.
        let tasks = latest_tasks(entries)
            .filter(|items| working || items.iter().any(|item| item.status() != TodoStatus::Completed));
        let running: Vec<WorkRow> = self
            .tray
            .as_ref()
            .map(|tray| tray.rows.clone())
            .unwrap_or_default();
        if tasks.is_none() && running.is_empty() {
            return None;
        }
        let mut section = div()
            .id("inspector-now")
            .debug_selector(|| "inspector-now".into())
            .flex_none()
            .flex()
            .flex_col()
            .px(px(PAD))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(Self::heading("Now", None, theme));
        if let Some(items) = tasks {
            let done = items.iter().filter(|item| item.status() == TodoStatus::Completed).count();
            let current = items.iter().position(|item| item.status() == TodoStatus::InProgress);
            let window = task_window(items.len(), current.or_else(|| {
                items.iter().position(|item| item.status() == TodoStatus::Pending)
            }));
            section = section.child(
                Self::heading("Tasks", Some(format!("{done}/{}", items.len())), theme).mt(px(2.0)),
            );
            for (ix, item) in items.iter().enumerate().skip(window.start).take(window.len()) {
                let (glyph, color, text) = match item.status() {
                    TodoStatus::Completed => (icons::CHECK, theme.success, theme.text_faint),
                    TodoStatus::InProgress => (icons::ARROW_RIGHT, theme.accent, theme.text),
                    TodoStatus::Pending => (icons::LIST, theme.text_faint, theme.text_muted),
                };
                let words = item.text.clone();
                section = section.child(
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
                        .min_h(px(20.0))
                        .child(icon(glyph).size(px(11.0)).flex_none().text_color(color))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui_rems(12.0))
                                .text_color(text)
                                .when(item.status() == TodoStatus::Completed, |el| el.line_through())
                                .child(crate::transcript::single_line(&words)),
                        ),
                );
            }
            let hidden = items.len() - window.len();
            if hidden > 0 {
                section = section.child(
                    div()
                        .text_size(ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .child(format!("{hidden} more")),
                );
            }
        }
        if !running.is_empty() {
            section = section.child(
                Self::heading("Running", Some(running.len().to_string()), theme).mt(px(2.0)),
            );
            for (ix, row) in running.into_iter().enumerate() {
                let glyph = if row.kind == WorkRow::SUBAGENT { icons::BOT } else { icons::TERMINAL };
                let detail = row.step.clone().unwrap_or_else(|| row.state_words.clone());
                let chat_id = row.chat_id.clone();
                section = section.child(
                    div()
                        .id(("inspector-work", ix))
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .min_h(px(22.0))
                        .rounded(px(5.0))
                        .role(if chat_id.is_some() { gpui::Role::Button } else { gpui::Role::ListItem })
                        .when(chat_id.is_some(), |el| {
                            let fade = format!("inspector-work-{ix}");
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
                            el.on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(InspectorEvent::OpenSubagent { chat_id: chat_id.clone() });
                            }))
                        }),
                );
            }
        }
        Some(section.into_any_element())
    }

    /// The files the session changed, each a press away from its diff.
    fn render_changes(&mut self, entries: &[SessionMessageEntry], theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let files = changed_files(entries);
        if files.is_empty() {
            return None;
        }
        let cwd = self.state.read(cx).selected_chat_row().and_then(|chat| chat.cwd.clone());
        let (adds, dels) = files.iter().fold((0, 0), |(a, d), file| (a + file.additions, d + file.deletions));
        let count = files.len();
        let shown = if self.files_open { count } else { count.min(FILES_SHOWN) };
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
        let mut section = div()
            .id("inspector-changes")
            .debug_selector(|| "inspector-changes".into())
            .flex_none()
            .flex()
            .flex_col()
            .px(px(PAD))
            .py(px(6.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                Self::heading("Changes", Some(format!("{count} {}", if count == 1 { "file" } else { "files" })), theme)
                    .child(div().flex_1())
                    .child(counts(adds, dels)),
            );
        for (ix, file) in files.into_iter().take(shown).enumerate() {
            let said = in_folder(&file.path, cwd.as_deref());
            let (dir, name) = match said.rsplit_once('/') {
                Some((dir, name)) => (Some(dir.to_owned()), name.to_owned()),
                None => (None, said.to_owned()),
            };
            let path = file.path.clone();
            section = section.child(
                div()
                    .id(("inspector-file", ix))
                    .role(gpui::Role::Button)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .min_h(px(22.0))
                    .px(px(4.0))
                    .mx(px(-4.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .bg(hover_blend(&format!("inspector-file-{ix}"), gpui::transparent_black(), theme.ink(ROW_WASH)))
                    .hover_fade(format!("inspector-file-{ix}"))
                    .aria_label(SharedString::from(format!("Open the diff of {said}")))
                    .child(
                        crate::file_icons::icon(
                            crate::file_icons::FileIconIdentity::file(&file.path),
                            theme.appearance,
                        )
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
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(InspectorEvent::OpenChange { path: path.clone() });
                    })),
            );
        }
        if count > shown || self.files_open && count > FILES_SHOWN {
            let open = self.files_open;
            section = section.child(
                div()
                    .id("inspector-files-more")
                    .role(gpui::Role::Button)
                    .aria_label(if open { "Fewer files".to_owned() } else { format!("{} more files", count - shown) })
                    .cursor_pointer()
                    .text_size(ui_rems(11.0))
                    .text_color(hover_blend("inspector-files-more", theme.text_faint, theme.text_muted))
                    .hover_fade("inspector-files-more")
                    .child(if open { "Fewer".to_owned() } else { format!("{} more", count - shown) })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.files_open = !this.files_open;
                        cx.notify();
                    })),
            );
        }
        Some(section.into_any_element())
    }

    /// The agent's prompt box as its TUI holds it, mirrored live under the timeline as the TUI
    /// keeps it under the conversation: the prompts queued behind the turn under way, then the
    /// draft, its placeholders drawn apart, and the pictures pasted into it, each a press away
    /// from the lightbox. It is shown only while the box holds something, with no caret: what
    /// it says is typed in the TUI, and nothing here types.
    fn render_prompt(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prompt = self.dock.as_ref()?.prompt.clone().filter(|prompt| !prompt.is_empty())?;
        let queued = (!prompt.queued.is_empty()).then(|| format!("{} queued", prompt.queued.len()));
        let mut section = div()
            .id("inspector-prompt")
            .debug_selector(|| "inspector-prompt".into())
            .role(gpui::Role::Group)
            .aria_label("Prompt box")
            .flex_none()
            .flex()
            .flex_col()
            .px(px(PAD))
            .pt(px(6.0))
            .pb(px(10.0))
            .border_t_1()
            .border_color(theme.border)
            .child(Self::heading("Prompt", queued, theme));
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
                div()
                    .pl(px(14.0))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(format!("{later} more")),
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
                    .child(crate::popover::search_highlight_ranges(text, marks, theme)),
            );
        }
        if !prompt.images.is_empty() {
            let mut strip = div().flex().flex_wrap().gap(px(6.0)).pt(px(6.0));
            for picture in &prompt.images {
                let snapshot = self.picture(&picture.path, cx);
                strip = strip.child(self.render_picture(picture, snapshot, theme, cx));
            }
            section = section.child(strip);
        }
        Some(section.into_any_element())
    }

    /// A picture pasted into the prompt box, as a thumbnail: once its bytes are here, a press
    /// opens it in the lightbox the timeline's pictures open in; until then, a still wash; one
    /// that cannot be read, a dashed frame.
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
                    .on_click(cx.listener(move |this, _, window, cx| this.show_picture(preview.clone(), window, cx)))
                    .child(
                        img(image.image)
                            .size(px(THUMB - 2.0))
                            .rounded(px(5.0))
                            .object_fit(ObjectFit::Cover),
                    )
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

impl gpui::Render for Inspector {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let entries = self.state.read(cx).transcript.clone();
        let header = self.render_header(&theme, cx);
        let now = self.render_now(&entries, &theme, cx);
        let changes = self.render_changes(&entries, &theme, cx);
        let prompt = self.render_prompt(&theme, cx);
        div()
            .id("inspector")
            .debug_selector(|| "inspector".into())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(header)
            .children(now)
            .children(changes)
            .child(
                div()
                    .id("inspector-timeline")
                    .debug_selector(|| "inspector-timeline".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.transcript.clone()),
            )
            .children(prompt)
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

    #[test]
    fn a_long_task_list_shows_the_tasks_around_the_current_one() {
        assert_eq!(task_window(4, Some(2)), 0..4);
        assert_eq!(task_window(10, Some(5)), 4..10);
        assert_eq!(task_window(10, Some(0)), 0..6);
        assert_eq!(task_window(10, None), 4..10);
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

    #[test]
    fn time_at_work_reads_in_seconds_minutes_and_hours() {
        assert_eq!(elapsed_words(42_000), "42s");
        assert_eq!(elapsed_words(192_000), "3m 12s");
        assert_eq!(elapsed_words(3_840_000), "1h 4m");
    }
}
