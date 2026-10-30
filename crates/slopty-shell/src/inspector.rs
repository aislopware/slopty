//! slopty: the Inspector, a read-only view of the selected session beside its terminal
//! (`docs/decisions/inspector.md`).
//!
//! The person works in the agent's own TUI; the inspector is where they look into what the
//! session did. Its frame is fixed (`inspector.md`, "Fixed frame"): one line each for the header
//! (status, branch), what it is doing now (its tasks and running work, opened as a card) and the
//! files it changed (opened as a card), then its timeline, whose last row is the TUI's own prompt
//! box, mirrored live (the prompts queued behind the turn, the draft, its pasted pictures). A
//! subagent's run drills in on a stack of the inspector's own (`inspector.md`, "A long or nested
//! run drills in, inside the inspector"): its trace takes the timeline's place under a header
//! that leads back; the terminal, the selection and the mirror stay where they were, and the
//! keyboard stays the inspector's, on the way back, so Esc goes back. It sends nothing to the
//! agent: copying, opening a file and looking at a picture are its only actions, and the TUI
//! does the rest.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
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
use crate::transcript::trace::Depth;
use crate::transcript::{Run, SubagentDrill, Transcript};
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

/// A subagent's run drilled into, on the inspector's stack.
struct Drilled {
    /// Its run's chat, whose feed is watched while it is on the stack.
    chat: String,
    /// The trace it was drilled from and its row there, which says its facts; none when it was
    /// drilled from the Now card and the trace shows no row of it.
    from: Entity<Transcript>,
    row: Option<SharedString>,
    /// Its task's name as it was drilled, for when its row is gone.
    title: SharedString,
    /// Its own trace.
    transcript: Entity<Transcript>,
    _drills: Subscription,
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
    /// The runs drilled into, the last one shown.
    stack: Vec<Drilled>,
    /// The inspector's own place for the keyboard, which a press in it takes (as a split in a
    /// terminal does); and the way back, the keyboard's after a drill while the inspector holds
    /// it, so Esc goes back a level and never reaches the TUI. Once the last way back is gone the
    /// keyboard waits on the session's trace, where its keys reach it. A drill never takes the
    /// keyboard from the terminal.
    root: FocusHandle,
    back: FocusHandle,
    focus_back: bool,
    /// The wake-up that moves a running run's time on in the header.
    tick: Option<Task<()>>,
    _drills: Subscription,
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
        let drills = cx.subscribe(&transcript, |this: &mut Self, from, event: &SubagentDrill, cx| {
            this.drill(from, event.chat_id.clone(), Some(event.row.clone()), None, cx);
        });
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
            stack: Vec::new(),
            root: cx.focus_handle(),
            back: cx.focus_handle().tab_stop(true),
            focus_back: false,
            tick: None,
            _drills: drills,
            _observe: observe,
        };
        inspector.sync_watch(cx);
        inspector
    }

    /// The timeline shown: the run drilled into last, else the session's.
    fn shown(&self) -> &Entity<Transcript> {
        self.stack.last().map_or(&self.transcript, |drilled| &drilled.transcript)
    }

    /// Drill into subagent run `chat` from `from`, its row there `row` (or, from the Now card,
    /// none, `title` naming it): its trace takes the timeline's place, following its newest step
    /// while it runs, at its brief once it ended, and the prompt box's mirror moves under it. A
    /// run already on the stack is gone back to instead. Where the inspector holds the keyboard,
    /// it moves to the way back: what held it (the row, its Open) went with the trace it was in.
    fn drill(
        &mut self,
        from: Entity<Transcript>,
        chat: String,
        row: Option<SharedString>,
        title: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if let Some(at) = self.stack.iter().position(|drilled| drilled.chat == chat) {
            self.pop_to(at + 1, cx);
            return;
        }
        let run = row.as_ref().and_then(|row| from.read(cx).spawn_run(row));
        let title = run.as_ref().map(|run| run.title.clone()).or(title).unwrap_or_else(|| "Subagent".into());
        let running = run.as_ref().is_none_or(|run| run.running);
        self.state.update(cx, |state, cx| state.watch_subagent_doc(chat.clone(), cx));
        let (state, doc) = (self.state.clone(), chat.clone());
        let depth = self.shown().read(cx).depth();
        let transcript = cx.new(|cx| {
            let mut transcript = Transcript::for_doc(state, doc, running, cx);
            transcript.set_depth(depth, cx);
            transcript
        });
        let drills = cx.subscribe(&transcript, |this: &mut Self, from, event: &SubagentDrill, cx| {
            this.drill(from, event.chat_id.clone(), Some(event.row.clone()), None, cx);
        });
        let mirror = self.mirror.clone();
        self.shown().clone().update(cx, |shown, cx| {
            shown.set_footer(None, cx);
            shown.hold_alt(false, cx);
        });
        transcript.update(cx, |transcript, cx| transcript.set_footer(Some(mirror.into()), cx));
        self.stack.push(Drilled { chat, from, row, title, transcript, _drills: drills });
        self.card = None;
        self.focus_back = true;
        cx.notify();
    }

    /// Go back to `depth` runs drilled into (0, the session's own timeline). The feeds of the
    /// runs left stop being watched, and of those they opened in place, unless a trace still
    /// held shows them too.
    fn pop_to(&mut self, depth: usize, cx: &mut Context<Self>) {
        if self.stack.len() <= depth {
            return;
        }
        // The trace shown last sets the fold depth for the one shown now: find may have moved it.
        let fold_depth = self.shown().read(cx).depth();
        let left: Vec<Drilled> = self.stack.drain(depth..).collect();
        let mut gone = Vec::new();
        for drilled in &left {
            gone.push(drilled.chat.clone());
            gone.extend(drilled.transcript.update(cx, |transcript, cx| {
                transcript.set_footer(None, cx);
                transcript.hold_alt(false, cx);
                transcript.release_spawns()
            }));
        }
        let held: Vec<Entity<Transcript>> = std::iter::once(self.transcript.clone())
            .chain(self.stack.iter().map(|drilled| drilled.transcript.clone()))
            .collect();
        gone.retain(|chat| {
            !self.stack.iter().any(|drilled| &drilled.chat == chat)
                && !held.iter().any(|transcript| transcript.read(cx).watches(chat))
        });
        self.state.update(cx, |state, _| {
            for chat in &gone {
                state.unwatch_subagent_doc(chat);
            }
        });
        let mirror = self.mirror.clone();
        self.shown().clone().update(cx, |shown, cx| shown.set_footer(Some(mirror.into()), cx));
        self.set_depth(fold_depth, cx);
        cx.notify();
    }

    /// Show every trace on the stack at `depth` (`inspector.md`, "One fold depth for the whole
    /// trace").
    fn set_depth(&mut self, depth: Depth, cx: &mut Context<Self>) {
        let traces: Vec<Entity<Transcript>> = std::iter::once(self.transcript.clone())
            .chain(self.stack.iter().map(|drilled| drilled.transcript.clone()))
            .collect();
        for trace in traces {
            trace.update(cx, |trace, cx| trace.set_depth(depth, cx));
        }
        cx.notify();
    }

    /// Whether the inspector's own keys reach it: the keyboard is on the inspector itself, its
    /// header or its way back, never in a field.
    fn keys_here(&self, window: &Window, cx: &App) -> bool {
        [&self.root, &self.back].into_iter().any(|handle| handle.is_focused(window))
            || self.shown().read(cx).holds_keys(window)
    }

    /// The fold depth's control, at the header's trailing edge: each depth a press away, the one
    /// shown marked, `[` and `]` stepping it.
    fn render_depth(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let shown = self.shown().read(cx).depth();
        div()
            .id("inspector-depth")
            .aria_label("Fold depth")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.0))
            .children(Depth::ALL.into_iter().map(|depth| {
                crate::surface_chrome::tab(SharedString::from(format!("inspector-depth-{}", depth.name())), depth == shown, theme)
                    .px(px(6.0))
                    .aria_label(depth.name())
                    .tooltip(crate::settings::widgets::text_tooltip(format!("{} · [ and ] step the depth", depth.name())))
                    .child(depth.name())
                    // A press here keeps the keyboard where it was.
                    .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_depth(depth, cx)))
            }))
            .into_any_element()
    }

    /// Whether the keyboard is the inspector's: on it or inside it, or on the pane that holds it.
    fn holds_keyboard(&self, window: &Window, cx: &mut App) -> bool {
        self.root.contains_focused(window, cx) || self.root.within_focused(window, cx)
    }

    /// After a way back, where the inspector holds the keyboard, it goes to the way back still
    /// shown, or to the session's trace: what held it went with the trace it was in.
    fn refocus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.holds_keyboard(window, cx) {
            let to = if self.stack.is_empty() { self.transcript.read(cx).keys_handle() } else { self.back.clone() };
            window.focus(&to, cx);
        }
    }

    /// The run shown, as its row reads it now, when one is drilled into and its row is there.
    fn drilled_run(&self, cx: &Context<Self>) -> Option<Run> {
        let drilled = self.stack.last()?;
        drilled.row.as_ref().and_then(|row| drilled.from.read(cx).spawn_run(row))
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
        self.pop_to(0, cx);
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

    /// The session's status and branch, on one line that is always drawn; drilled into a run,
    /// the way back instead ([`Self::render_drilled_header`]). The session's own title is not
    /// said again here: its terminal's title bar and its sidebar row say it.
    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !self.stack.is_empty() {
            return self.render_drilled_header(theme, cx);
        }
        let state = self.state.read(cx);
        let chat = state.selected_chat_row().cloned();
        let status = chat.as_ref().map_or(ChatIndicator::Idle, |chat| state.display_status_for(chat, self.clock));
        // slopty: the one state vocabulary, its need's own glyph included (`layout.md`, "A state
        // is a glyph").
        let session = chat.as_ref().and_then(|chat| state.session_for(&chat.id));
        let mark = crate::shell::marks::Mark::of(status, session, self.clock);
        let branch = chat.as_ref().and_then(|chat| chat.branch.clone());
        self.line("inspector-header", None, theme, cx)
            .gap(px(10.0))
            // A press here keeps the keyboard where it was.
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
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
            .child(div().flex_1())
            .child(self.render_depth(theme, cx))
            .into_any_element()
    }

    /// Drilled into a run: the way back, the runs it is under (each a press away), its task's
    /// name and its status mark, on the header's line.
    fn render_drilled_header(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let depth = self.stack.len();
        let parent: SharedString = match depth {
            0 | 1 => "the session".into(),
            _ => self.stack[depth - 2].title.clone(),
        };
        let run = self.drilled_run(cx);
        let title = run.as_ref().map_or_else(|| self.stack[depth - 1].title.clone(), |run| run.title.clone());
        let back_ink = hover_blend("inspector-back", theme.text_muted, theme.text);
        let back = div()
            .id("inspector-back")
            .track_focus(&self.back)
            .role(gpui::Role::Button)
            .flex_none()
            .rounded(px(4.0))
            .cursor_pointer()
            .hover_fade("inspector-back")
            .focus_visible({
                let ring = crate::surface_chrome::ring(theme, crate::surface_chrome::RING_OFFSET);
                move |s| s.outline_ring(ring)
            })
            .aria_label(SharedString::from(format!("Back to {parent}")))
            .tooltip(crate::settings::widgets::text_tooltip(format!("Back to {parent}")))
            // An SVG paints only in a colour of its own.
            .child(icon(icons::ARROW_LEFT).size(px(12.0)).text_color(back_ink))
            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.pop_to(depth - 1, cx);
                this.refocus(window, cx);
            }));
        let crumbs: Vec<AnyElement> = std::iter::once(SharedString::from("Session"))
            .chain(self.stack[..depth - 1].iter().map(|drilled| drilled.title.clone()))
            .enumerate()
            .map(|(level, name)| {
                let fade = format!("inspector-crumb-{level}");
                div()
                    .id(("inspector-crumb", level))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("Back to {name}")))
                    .min_w(px(24.0))
                    .flex_shrink(1.0)
                    .truncate()
                    .cursor_pointer()
                    .text_color(hover_blend(&fade, theme.text_faint, theme.text_muted))
                    .hover_fade(fade)
                    .child(crate::transcript::single_line(&name))
                    .on_mouse_down(gpui::MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pop_to(level, cx);
                        this.refocus(window, cx);
                    }))
                    .into_any_element()
            })
            .flat_map(|crumb| [crumb, div().flex_none().text_color(theme.text_faint).child("\u{203A}").into_any_element()])
            .collect();
        self.line("inspector-header", None, theme, cx)
            .gap(px(6.0))
            .text_size(ui_rems(12.0))
            .child(back)
            .children(crumbs)
            .child(
                div()
                    .id("inspector-title")
                    .debug_selector(|| "inspector-title".into())
                    .role(gpui::Role::Heading)
                    .aria_label(title.clone())
                    .min_w_0()
                    .flex_shrink_0()
                    .max_w(gpui::relative(0.7))
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(if run.as_ref().is_some_and(|run| run.failed) { theme.danger } else { theme.text })
                    .child(crate::transcript::single_line(&title)),
            )
            .children(run.as_ref().and_then(Run::mark).map(|mark| {
                div()
                    .flex_none()
                    .child(StatusMark::new("inspector-run-mark", mark).size(MARK_SMALL))
            }))
            .into_any_element()
    }

    /// Drilled into a run: what it does now (its step, what it asks of the person, or what it
    /// answered) and its facts, on the Now line, which opens nothing.
    fn render_drilled_now(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let run = self.drilled_run(cx);
        let now = self.clock.timestamp_millis();
        let (said, tint) = match &run {
            Some(run) if run.needs_you.is_some() => {
                (format!("Needs you · {}", run.needs_you.clone().unwrap_or_default()), theme.warning)
            }
            Some(run) => (run.step.clone().unwrap_or_default(), if run.failed { theme.danger } else { theme.text }),
            None => (String::new(), theme.text),
        };
        let facts = run.as_ref().map(|run| run.summary_at(now)).unwrap_or_default();
        self.line("inspector-now", None, theme, cx)
            .aria_label(SharedString::from(match (said.is_empty(), facts.is_empty()) {
                (false, false) => format!("Now: {said}, {facts}"),
                (false, true) => format!("Now: {said}"),
                (true, false) => format!("Now: {facts}"),
                (true, true) => "Now: nothing".to_owned(),
            }))
            .child(Self::name("Now", theme))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_size(ui_rems(12.0))
                    .text_color(tint)
                    .child(crate::transcript::single_line(&said)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .child(facts),
            )
            .into_any_element()
    }

    /// What the session is doing now, on one line ("Tasks 2/4 · Write the tests", "2
    /// running"), opening to its task list and its running work.
    fn render_now(&self, tasks: Option<&[TodoItem]>, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !self.stack.is_empty() {
            return self.render_drilled_now(theme, cx);
        }
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
            let title = SharedString::from(crate::transcript::single_line(&row.title));
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
                        // A running subagent drills in, as its row in the timeline does.
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.close_card(window, cx);
                            let from = this.transcript.clone();
                            let row = from.read(cx).spawn_row_of(&chat_id);
                            this.drill(from, chat_id.clone(), row, Some(title.clone()), cx);
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
        if std::mem::take(&mut self.focus_back) && !self.stack.is_empty() && self.holds_keyboard(window, cx) {
            window.focus(&self.back, cx);
        }
        // A running run's time moves on in the header, once a second.
        if self.tick.is_none() && self.drilled_run(cx).is_some_and(|run| run.running) {
            self.tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let _gone = this.update(cx, |this, cx| {
                    this.tick = None;
                    this.clock = crate::clock::utc(cx);
                    cx.notify();
                });
            }));
        }
        let (files, tasks) = self.facts(cx);
        let header = self.render_header(&theme, cx);
        let now = self.render_now(tasks.as_deref(), &theme, cx);
        let changes = self.render_changes(&files, &theme, cx);
        div()
            .id("inspector")
            .debug_selector(|| "inspector".into())
            .track_focus(&self.root)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Esc goes back a run where the keyboard is in the inspector; in the terminal it is
            // the TUI's.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let plain = !event.keystroke.modifiers.modified();
                match event.keystroke.key.as_str() {
                    "escape" if !this.stack.is_empty() => {
                        cx.stop_propagation();
                        let depth = this.stack.len() - 1;
                        this.pop_to(depth, cx);
                        this.refocus(window, cx);
                    }
                    // `[` and `]` step the fold depth of the whole trace.
                    key @ ("[" | "]") if plain && this.keys_here(window, cx) => {
                        cx.stop_propagation();
                        let depth = this.shown().read(cx).depth().step(if key == "[" { -1 } else { 1 });
                        this.set_depth(depth, cx);
                    }
                    _ => {}
                }
            }))
            // ⌥ held where the inspector holds the keyboard shows when every step began.
            .on_modifiers_changed(cx.listener(|this, event: &gpui::ModifiersChangedEvent, _, cx| {
                let held = event.modifiers.alt;
                this.shown().clone().update(cx, |shown, cx| shown.hold_alt(held, cx));
            }))
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
                    .child(self.shown().clone()),
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
    use slopty_view_doc::ToolDiffStat;

    use super::*;

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

    /// The window a drill is judged in: a stand-in for the session's terminal beside the pane
    /// that holds the inspector, which takes the keyboard on a press in it, as the shell's does
    /// (`shell/navigation_focus.rs`).
    struct Host {
        terminal: FocusHandle,
        pane: FocusHandle,
        inspector: Entity<Inspector>,
    }

    impl gpui::Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let pane = self.pane.clone();
            div()
                .size_full()
                .flex()
                .child(div().id("terminal").track_focus(&self.terminal).w(px(200.0)).h_full())
                .child(
                    div()
                        .id("pane")
                        .track_focus(&self.pane)
                        .capture_any_mouse_down(move |_, window, cx| {
                            if !pane.contains_focused(window, cx) {
                                window.focus(&pane, cx);
                            }
                        })
                        .flex_1()
                        .h_full()
                        .child(self.inspector.clone()),
                )
        }
    }

    /// A finished subagent's spawn, `id`, whose run is chat `chat`, doing `task`.
    fn spawn(id: &str, chat: &str, task: &str) -> MessagePart {
        MessagePart::Tool {
            id: id.to_owned(),
            call: ToolCall::Unknown {
                name: format!("Agent: {task}"),
                input: Some(serde_json::json!({ "description": task })),
            },
            is_error: false,
            resolved: true,
            output: None,
            diff: None,
            output_ref: None,
            output_bytes: None,
            diff_ref: None,
            diff_stats: None,
            subagent_ref: Some(chat.to_owned()),
            subagent_status: Some(slopty_view_doc::SubagentStatus::Done),
            subagent_tail: None,
            facts: Some(Box::new(slopty_view_doc::ToolFacts {
                subagent_progress: Some(slopty_view_doc::SubagentProgress {
                    agent_type: Some("Explore".into()),
                    state: slopty_view_doc::SubagentProgress::DONE.into(),
                    chat_id: Some(chat.into()),
                    ..slopty_view_doc::SubagentProgress::default()
                }),
                ..slopty_view_doc::ToolFacts::default()
            })),
        }
    }

    fn says(id: &str, parts: Vec<MessagePart>) -> SessionMessageEntry {
        SessionMessageEntry { id: id.to_owned(), status: Some(slopty_view_doc::MessageStatus::Complete), ..assistant(parts) }
    }

    fn words(id: &str, text: &str) -> MessagePart {
        MessagePart::Text { id: id.to_owned(), text: text.to_owned() }
    }

    /// The last frame's accessibility tree, as (role, label, centre) of each node.
    fn draw(cx: &mut gpui::VisualTestContext) -> Vec<(gpui::accesskit::Role, String, gpui::Point<Pixels>)> {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.set_a11y_active(true);
            window.refresh();
            window.draw(cx).clear(cx);
            let scale = f64::from(window.scale_factor());
            let Some(update) = window.a11y_tree() else { return Vec::new() };
            update
                .nodes
                .iter()
                .map(|(_, node)| {
                    let r = node.bounds().unwrap_or_default();
                    let centre = gpui::point(
                        px(((r.x0 + r.x1) / 2.0 / scale) as f32),
                        px(((r.y0 + r.y1) / 2.0 / scale) as f32),
                    );
                    (node.role(), node.label().unwrap_or_default().to_owned(), centre)
                })
                .collect()
        })
    }

    fn at(nodes: &[(gpui::accesskit::Role, String, gpui::Point<Pixels>)], label: &str) -> Option<gpui::Point<Pixels>> {
        nodes.iter().find(|(_, said, _)| said == label).map(|(_, _, centre)| *centre)
    }

    /// A drill stays inside the inspector. Asked while the terminal holds the keyboard, it
    /// leaves the keyboard there. A press on a run's Open (which gives the pane the keyboard)
    /// shows its trace under a header that leads back, with the keyboard on the way back, and
    /// leaves the selection and the prompt box's mirror (which moves under the trace shown)
    /// alone; a run inside it drills a level deeper, drilling one already on the stack goes back
    /// to it, and Esc goes back a level. A crumb goes back to its level and lets go of the runs'
    /// feeds, the keyboard waiting on the session's trace. Enter on a run's row drills too.
    #[gpui::test]
    fn a_drill_stays_inside_the_inspector(cx: &mut gpui::TestAppContext) {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::dark());
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            cx.set_reduce_motion(true);
        });
        let state = cx.new(|_| AppState::new());
        state.update(cx, |state, _| {
            state.chats = vec![
                serde_json::from_value(serde_json::json!({
                    "id": "chat", "title": "Chat", "deviceId": "dev", "archived": false,
                    "createdAt": chrono::Utc::now(), "cwd": "/work/app",
                }))
                .unwrap(),
            ];
            state.selected_chat = Some("chat".into());
            state.transcript_replayed = true;
            state.transcript = vec![says("reply", vec![words("lead", "Sending a scout."), spawn("spawn", "child", "Count lines")])];
            state.transcript_revision += 1;
        });
        let snapshots = |state: &Entity<AppState>, cx: &mut gpui::VisualTestContext| {
            state.update(cx, |state, _| {
                state.set_subagent_snapshot(
                    "child".into(),
                    vec![says("c", vec![words("c-lead", "Counting in the child."), spawn("inner", "grand", "Read parser")])],
                );
                state.set_subagent_snapshot("grand".into(), vec![says("g", vec![words("g-lead", "Reading in the grandchild.")])]);
            });
        };
        let transcript = cx.new(|cx| Transcript::new(state.clone(), cx));
        transcript.update(cx, |this, cx| this.sync(cx));
        let inspector = cx.new(|cx| Inspector::new(state.clone(), transcript.clone(), cx));
        let (terminal, pane) = cx.update(|cx| (cx.focus_handle(), cx.focus_handle()));
        let (_, cx) = cx.add_window_view(|_, _| Host {
            terminal: terminal.clone(),
            pane: pane.clone(),
            inspector: inspector.clone(),
        });
        cx.simulate_resize(gpui::size(px(1000.0), px(800.0)));
        snapshots(&state, cx);
        let footers = |cx: &mut gpui::VisualTestContext| {
            inspector.read_with(cx, |inspector, cx| {
                std::iter::once(&inspector.transcript)
                    .chain(inspector.stack.iter().map(|drilled| &drilled.transcript))
                    .map(|transcript| transcript.read(cx).has_footer())
                    .collect::<Vec<_>>()
            })
        };
        let depth = |cx: &mut gpui::VisualTestContext| inspector.read_with(cx, |inspector, _| inspector.stack.len());
        let (back, home) =
            inspector.read_with(cx, |inspector, cx| (inspector.back.clone(), inspector.transcript.read(cx).keys_handle()));
        let holds = |handle: &FocusHandle, cx: &mut gpui::VisualTestContext| cx.update(|window, _| handle.is_focused(window));

        // Asked while the terminal holds the keyboard, a drill leaves it there.
        cx.update(|window, cx| window.focus(&terminal, cx));
        draw(cx);
        inspector.update(cx, |inspector, cx| {
            let from = inspector.transcript.clone();
            let row = from.read(cx).spawn_row_of("child");
            inspector.drill(from, "child".into(), row, None, cx);
        });
        let nodes = draw(cx);
        assert!(at(&nodes, "Counting in the child.").is_some(), "its trace takes the timeline's place");
        assert!(holds(&terminal, cx), "the terminal keeps the keyboard");
        inspector.update(cx, |inspector, cx| inspector.pop_to(0, cx));
        snapshots(&state, cx);

        // A press on its Open: the pane takes the keyboard, and the drill puts it on the way back.
        let nodes = draw(cx);
        assert!(at(&nodes, "Counting in the child.").is_none(), "the child's steps wait for a drill");
        let open = at(&nodes, "Open subagent").expect("the run's Open");
        cx.simulate_click(open, gpui::Modifiers::default());
        let nodes = draw(cx);
        assert!(at(&nodes, "Counting in the child.").is_some(), "its trace takes the timeline's place");
        assert!(at(&nodes, "Sending a scout.").is_none(), "the session's own steps are under it");
        assert!(at(&nodes, "Back to the session").is_some(), "a way back");
        assert!(at(&nodes, "Count lines").is_some(), "its task heads it");
        assert_eq!(state.read_with(cx, |state, _| state.selected_chat.clone()).as_deref(), Some("chat"), "the selection stays");
        assert!(holds(&back, cx), "the keyboard is on the way back, so Esc never reaches the TUI");
        assert_eq!(footers(cx), [false, true], "the prompt box's mirror moves under the trace shown");

        let open = at(&nodes, "Open subagent").expect("the grandchild's Open");
        cx.simulate_click(open, gpui::Modifiers::default());
        let nodes = draw(cx);
        assert!(at(&nodes, "Reading in the grandchild.").is_some(), "a level deeper");
        assert!(at(&nodes, "Back to Count lines").is_some(), "the way back names the run it leads to");
        assert_eq!(footers(cx), [false, false, true]);
        assert!(holds(&back, cx));

        // Drilling a run already on the stack goes back to it.
        inspector.update(cx, |inspector, cx| {
            let from = inspector.transcript.clone();
            inspector.drill(from, "child".into(), None, None, cx);
        });
        assert_eq!(depth(cx), 1, "back to the run already drilled");
        assert!(at(&draw(cx), "Counting in the child.").is_some());

        // Esc goes back a level, the keyboard to the session's trace once at the session.
        cx.simulate_keystrokes("escape");
        let nodes = draw(cx);
        assert_eq!(depth(cx), 0, "Esc goes back");
        assert!(at(&nodes, "Sending a scout.").is_some(), "the session's timeline again");
        assert_eq!(footers(cx), [true], "the mirror is under the session's timeline again");
        assert!(holds(&home, cx), "the keyboard waits on the session's trace");
        assert!(state.read_with(cx, |state, _| state.sub_transcript("child").is_empty()), "the run's feed is let go");

        // A crumb goes back to its level.
        snapshots(&state, cx);
        let open = at(&draw(cx), "Open subagent").expect("the run's Open");
        cx.simulate_click(open, gpui::Modifiers::default());
        let crumb = at(&draw(cx), "Back to Session").expect("the session's crumb");
        cx.simulate_click(crumb, gpui::Modifiers::default());
        let nodes = draw(cx);
        assert_eq!(depth(cx), 0, "the crumb goes back");
        assert!(at(&nodes, "Back to the session").is_none(), "no way back at the session");
        assert!(holds(&home, cx), "the keyboard waits on the session's trace");

        // Enter on the run's row drills too.
        snapshots(&state, cx);
        let focused = |cx: &mut gpui::VisualTestContext| {
            draw(cx);
            cx.update(|window, _| {
                let update = window.a11y_tree()?;
                update.nodes.iter().find(|(id, _)| *id == update.focus).and_then(|(_, node)| node.label().map(str::to_owned))
            })
            .unwrap_or_default()
        };
        let mut stops = 0;
        while focused(cx) != "Subagent Count lines" {
            assert!(stops < 40, "Tab reaches the run's row");
            stops += 1;
            cx.update(|window, cx| window.focus_next(cx));
        }
        // A key press clicks as it comes up.
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
        draw(cx);
        assert_eq!(depth(cx), 1, "Enter drills in");
        assert!(holds(&back, cx), "the keyboard goes to the way back");
    }
}
