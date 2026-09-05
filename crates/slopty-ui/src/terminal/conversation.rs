//! The conversation view: a Claude Code session's transcript read like a chat, shown in
//! place of the grid, with a composer to answer from.
//!
//! The host tails the agent's transcript file and streams [`TranscriptEntry`]s; the view keeps
//! them in a bottom-aligned [`gpui::list`] in tail-follow mode (a chat log: new entries append
//! at the bottom and the view stays there unless the reader scrolled up, when a "↓ latest"
//! pill offers the way back). Long parts (thinking, a tool's input, a tool's result past its
//! first lines) start folded and open on a click. Under the list sits the composer, a
//! multi-line input whose ↩ types the text into the session followed by Enter, and above it
//! the attention row: Allow / Deny when the agent waits for a permission.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable as _,
    FollowMode, InteractiveElement as _, IntoElement, ListAlignment, ListState, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, list, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use slopty_proto::agent::{Clipped, TranscriptBody, TranscriptEntry, TranscriptUpdate};
use slopty_theme::Theme;

use crate::colors::{hsla, hsla_alpha};
use crate::terminal::view::TerminalView;

/// Lines of a tool result shown before it is opened.
pub const RESULT_PREVIEW_LINES: usize = 4;
/// The composer grows with its text up to this many rows, then scrolls.
const COMPOSER_MAX_ROWS: usize = 6;

/// What the agent is waiting for, as the view shows it above the composer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Attention {
    /// A tool needs permission: Allow / Deny.
    Permission {
        /// The tool.
        tool: String,
        /// The human already answered; waiting for the host to report the next state.
        answered: Option<bool>,
    },
    /// The agent asked something: the composer has the focus.
    Prompt,
}

/// A session's conversation as received so far.
pub struct Conversation {
    /// Entries, oldest first; shared with the list's render closure.
    entries: Rc<[TranscriptEntry]>,
    list: ListState,
    /// Entries whose folded part is open; shared with the list's render closure.
    open: Rc<HashSet<usize>>,
    /// The text being written.
    composer: Entity<TextareaState>,
    _composer_events: Subscription,
}

impl std::fmt::Debug for Conversation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conversation").field("entries", &self.entries.len()).finish_non_exhaustive()
    }
}

impl Conversation {
    /// Empty, waiting for the host's snapshot. The composer's ↩ calls
    /// [`TerminalView::submit_composer`].
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<TerminalView>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message Claude  (↩ send, ⇧↩ newline)")
                .auto_grow(1, COMPOSER_MAX_ROWS)
                .submit_on_enter(true)
        });
        let events = cx.subscribe_in(&composer, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.submit_composer(window, cx);
            }
        });
        let list = ListState::new(0, ListAlignment::Bottom, px(512.0));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            entries: Rc::from([]),
            list,
            open: Rc::new(HashSet::new()),
            composer,
            _composer_events: events,
        }
    }

    /// The entries shown.
    #[must_use]
    pub fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Whether the list follows new entries (the reader has not scrolled up).
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.list.is_following_tail()
    }

    /// Back to the bottom, following again.
    pub fn pin(&self) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// Whether entry `ix`'s folded part is open.
    #[must_use]
    pub fn is_open(&self, ix: usize) -> bool {
        self.open.contains(&ix)
    }

    /// Open or fold entry `ix`.
    pub fn toggle(&mut self, ix: usize) {
        let mut open = HashSet::clone(&self.open);
        if !open.remove(&ix) {
            open.insert(ix);
        }
        self.open = Rc::new(open);
        self.list.splice(ix..ix.saturating_add(1), 1);
    }

    /// The composer's focus.
    #[must_use]
    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// Put the caret in the composer.
    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |input, cx| input.focus(window, cx));
    }

    /// What is written in the composer.
    #[must_use]
    pub fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// Take the composer's text, leaving it empty.
    #[must_use]
    pub fn take_composer_text(&self, window: &mut Window, cx: &mut App) -> String {
        let text = self.composer_text(cx);
        self.composer.update(cx, |input, cx| input.clean(window, cx));
        text
    }

    /// Take a slice from the host: a reset replaces everything (and pins the view to the
    /// bottom again), otherwise the entries append.
    pub fn apply(&mut self, update: TranscriptUpdate) {
        let mut entries = if update.reset { Vec::new() } else { self.entries.to_vec() };
        let before = entries.len();
        entries.extend(update.entries);
        let after = entries.len();
        self.entries = Rc::from(entries);
        if update.reset {
            self.open = Rc::new(HashSet::new());
            self.list.reset(after);
            self.pin();
        } else {
            self.list.splice(before..before, after.saturating_sub(before));
        }
    }

    /// The chat, filling its container: the list, the attention row, the composer.
    #[must_use]
    pub fn render(
        &self,
        attention: Option<&Attention>,
        theme: &Theme,
        ui_size: f32,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let entries = Rc::clone(&self.entries);
        let open = Rc::clone(&self.open);
        let theme = theme.clone();
        let empty = entries.is_empty();
        let view = cx.entity();
        let s = theme.surfaces.clone();
        div()
            .id("conversation")
            .debug_selector(|| "conversation".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(s.panel))
            .text_size(px(ui_size))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .min_h(px(0.0))
                    .when(empty, |el| {
                        el.flex()
                            .items_center()
                            .justify_center()
                            .text_color(hsla(s.text_muted))
                            .child("waiting for the agent's conversation…")
                    })
                    .when(!empty, |el| {
                        let theme = theme.clone();
                        el.child(
                            list(
                                self.list.clone(),
                                move |ix, _window: &mut Window, _cx: &mut App| {
                                    entries.get(ix).map_or_else(
                                        || div().into_any_element(),
                                        |e| {
                                            entry(ix, e, open.contains(&ix), &view, &theme, ui_size)
                                        },
                                    )
                                },
                            )
                            .size_full(),
                        )
                    })
                    .when(!empty && !self.pinned(), |el| {
                        el.child(
                            div()
                                .id("conversation-latest")
                                .debug_selector(|| "conversation-latest".to_owned())
                                .absolute()
                                .bottom(px(ui_size * 0.6))
                                .right(px(ui_size * 1.2))
                                .px(px(ui_size * 0.7))
                                .py(px(ui_size * 0.25))
                                .rounded(px(ui_size))
                                .bg(hsla(s.accent))
                                .text_color(hsla(s.canvas))
                                .text_size(px(ui_size * 0.85))
                                .shadow_md()
                                .cursor_pointer()
                                .child("↓ latest")
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    if let Some(c) = this.conversation() {
                                        c.pin();
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when_some(attention, |el, attention| {
                el.child(attention_row(attention, &theme, ui_size, cx))
            })
            .child(self.composer_row(&theme, ui_size, cx))
            .into_any_element()
    }

    /// The composer: the growing text field and a send button.
    fn composer_row(&self, theme: &Theme, ui_size: f32, cx: &Context<TerminalView>) -> AnyElement {
        let s = &theme.surfaces;
        div()
            .id("composer")
            .debug_selector(|| "composer".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(ui_size * 0.5))
            .px(px(ui_size * 0.7))
            .py(px(ui_size * 0.5))
            .border_t_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(ui_size * 0.6))
                    .py(px(ui_size * 0.3))
                    .rounded(px(ui_size * 0.6))
                    .border_1()
                    .border_color(hsla(s.border))
                    .bg(hsla(s.canvas))
                    .child(Textarea::new(&self.composer).appearance(false).bordered(false)),
            )
            .child(
                div()
                    .id("composer-send")
                    .debug_selector(|| "composer-send".to_owned())
                    .flex_none()
                    .w(px(ui_size * 2.0))
                    .h(px(ui_size * 2.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui_size))
                    .bg(hsla(s.accent))
                    .text_color(hsla(s.canvas))
                    .cursor_pointer()
                    .child("↑")
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .on_click(
                        cx.listener(|this, _ev, window, cx| this.submit_composer(window, cx)),
                    ),
            )
            .into_any_element()
    }
}

/// What the agent waits for, with the one-tap answers, above the composer.
fn attention_row(
    attention: &Attention,
    theme: &Theme,
    ui_size: f32,
    cx: &Context<TerminalView>,
) -> AnyElement {
    let s = &theme.surfaces;
    let row = div()
        .id("conversation-attention")
        .debug_selector(|| "conversation-attention".to_owned())
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(ui_size * 0.6))
        .px(px(ui_size))
        .py(px(ui_size * 0.4))
        .border_t_1()
        .border_color(hsla(s.border))
        .bg(hsla_alpha(theme.terminal.palette(3), 0.12))
        .text_size(px(ui_size * 0.9));
    let button = |id: &'static str, label: &'static str, accent: bool| {
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .px(px(ui_size * 0.8))
            .py(px(ui_size * 0.2))
            .rounded(px(ui_size * 0.4))
            .bg(hsla_alpha(if accent { s.accent } else { s.text_muted }, 0.3))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .child(label)
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
    };
    match attention {
        Attention::Permission { tool, answered: None } => row
            .child(div().flex_1().child(SharedString::from(format!("Claude wants to use {tool}"))))
            .child(
                button("conversation-allow", "Allow", true)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(true, cx))),
            )
            .child(
                button("conversation-deny", "Deny", false)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(false, cx))),
            )
            .into_any_element(),
        Attention::Permission { tool, answered: Some(allowed) } => row
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(format!(
                "{tool}: {}",
                if *allowed { "allowed" } else { "denied" }
            )))
            .into_any_element(),
        Attention::Prompt => row
            .text_color(hsla(s.text_muted))
            .child("Claude is waiting for your answer")
            .into_any_element(),
    }
}

/// One entry: the human's prompt in an accent bubble, the agent's Markdown, a folded line
/// for thinking, a tool call that opens on its input, a tool result.
fn entry(
    ix: usize,
    entry: &TranscriptEntry,
    open: bool,
    view: &Entity<TerminalView>,
    theme: &Theme,
    ui_size: f32,
) -> AnyElement {
    let s = &theme.surfaces;
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let row = div()
        .id(ElementId::NamedInteger("conversation-entry".into(), u64::try_from(ix).unwrap_or(0)))
        .debug_selector(move || format!("conversation-entry-{ix}"))
        .w_full()
        .px(px(ui_size))
        .py(px(ui_size * 0.35));
    let time = clock(entry.at).map(|t| {
        div()
            .flex_none()
            .text_size(px(ui_size * 0.7))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(t))
    });
    let toggle = {
        let view = view.clone();
        move |_ev: &gpui::ClickEvent, _window: &mut Window, cx: &mut App| {
            view.update(cx, |v, cx| v.toggle_entry(ix, cx));
        }
    };
    match &entry.body {
        TranscriptBody::User { text } => row
            .flex()
            .items_end()
            .justify_end()
            .gap(px(ui_size * 0.5))
            .children(time)
            .child(
                div()
                    .max_w(px(ui_size * 40.0))
                    .px(px(ui_size * 0.8))
                    .py(px(ui_size * 0.45))
                    .rounded(px(ui_size * 0.6))
                    .bg(hsla_alpha(s.accent, 0.22))
                    .child(SharedString::from(text.clone())),
            )
            .into_any_element(),
        TranscriptBody::Assistant { markdown } => row
            .flex()
            .items_end()
            .gap(px(ui_size * 0.5))
            .child(div().flex_1().min_w(px(0.0)).child(TextView::markdown(
                ElementId::NamedInteger("conversation-md".into(), u64::try_from(ix).unwrap_or(0)),
                SharedString::from(markdown.clone()),
            )))
            .children(time)
            .into_any_element(),
        TranscriptBody::Thinking { text } => row
            .flex()
            .flex_col()
            .gap(px(ui_size * 0.3))
            .text_size(px(ui_size * 0.85))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .cursor_pointer()
                    .child(SharedString::from(fold_label(open, "thinking", text)))
                    .on_click(toggle),
            )
            .when(open, |el| el.child(clipped_block(text, &mono, ui_size)))
            .into_any_element(),
        TranscriptBody::ToolUse { name, summary, input } => row
            .flex()
            .flex_col()
            .gap(px(ui_size * 0.3))
            .text_size(px(ui_size * 0.85))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .flex()
                    .gap(px(ui_size * 0.5))
                    .cursor_pointer()
                    .child(SharedString::from(format!("{} {name}", arrow(open))))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(mono.clone())
                            .child(SharedString::from(summary.clone())),
                    )
                    .on_click(toggle),
            )
            .when(open, |el| el.child(clipped_block(input, &mono, ui_size)))
            .into_any_element(),
        TranscriptBody::ToolResult { tool, output, is_error } => {
            let (shown, hidden) = preview(output, open);
            let color = if *is_error { theme.terminal.palette(1) } else { s.text_muted };
            row.flex()
                .flex_col()
                .gap(px(ui_size * 0.3))
                .text_size(px(ui_size * 0.85))
                .text_color(hsla(color))
                .child(
                    div()
                        .id("fold")
                        .cursor_pointer()
                        .child(SharedString::from(format!(
                            "{} {}{}",
                            if hidden > 0 || open { arrow(open) } else { "↳" },
                            tool.as_deref().unwrap_or("result"),
                            if *is_error { " failed" } else { "" }
                        )))
                        .on_click(toggle),
                )
                .when(!shown.text.is_empty(), |el| el.child(clipped_block(&shown, &mono, ui_size)))
                .into_any_element()
        }
    }
}

/// A clipped text as a mono block, with the count of what the host or the fold dropped.
fn clipped_block(text: &Clipped, mono: &str, ui_size: f32) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .pl(px(ui_size * 1.2))
        .font_family(mono.to_owned())
        .whitespace_normal()
        .child(SharedString::from(text.text.clone()))
        .when(text.more_lines > 0, |el| {
            el.child(
                div().italic().child(SharedString::from(format!("{} more lines", text.more_lines))),
            )
        })
        .into_any_element()
}

/// A fold's header: "▸ thinking" / "▾ thinking", with the line count while folded.
fn fold_label(open: bool, what: &str, text: &Clipped) -> String {
    if open {
        return format!("{} {what}", arrow(open));
    }
    let lines =
        text.text.lines().count().saturating_add(usize::try_from(text.more_lines).unwrap_or(0));
    format!("{} {what} ({lines} lines)", arrow(open))
}

const fn arrow(open: bool) -> &'static str {
    if open { "▾" } else { "▸" }
}

/// A result's visible part: everything when open, else its first
/// [`RESULT_PREVIEW_LINES`], with the lines beyond them counted as more.
fn preview(output: &Clipped, open: bool) -> (Clipped, usize) {
    let total = output.text.lines().count();
    if open || total <= RESULT_PREVIEW_LINES {
        return (output.clone(), 0);
    }
    let head: Vec<&str> = output.text.lines().take(RESULT_PREVIEW_LINES).collect();
    let hidden = total.saturating_sub(RESULT_PREVIEW_LINES);
    let more = u32::try_from(hidden).unwrap_or(u32::MAX).saturating_add(output.more_lines);
    (Clipped { text: head.join("\n"), more_lines: more }, hidden)
}

/// A record's time of day in the local zone, "HH:MM".
#[must_use]
pub fn clock(at: Option<u64>) -> Option<String> {
    let ms = i64::try_from(at?).ok()?;
    let utc = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)?;
    Some(utc.with_timezone(&chrono::Local).format("%H:%M").to_string())
}
