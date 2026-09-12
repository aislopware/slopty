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

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable as _,
    FollowMode, InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list,
    px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use slopty_proto::agent::{Clipped, TranscriptBody, TranscriptEntry, TranscriptUpdate};
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
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
        /// One line about the call (the command, the file), when the host knows it: a driven
        /// agent says what the tool would do; a watched one only names the tool.
        detail: Option<String>,
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

    /// The chat, filling its container: the list, the attention row, the composer
    /// (`composer_focused` draws its focus ring).
    #[must_use]
    pub fn render(
        &self,
        attention: Option<&Attention>,
        partial: &str,
        composer_focused: bool,
        theme: &Theme,
        cx: &Context<TerminalView>,
    ) -> AnyElement {
        let entries = Rc::clone(&self.entries);
        let open = Rc::clone(&self.open);
        let theme = theme.clone();
        let empty = entries.is_empty();
        let view = cx.entity();
        let s = theme.surfaces.clone();
        let spacing = theme.spacing;
        div()
            .id("conversation")
            .debug_selector(|| "conversation".to_owned())
            .role(Role::Group)
            .aria_label("Conversation")
            .size_full()
            .flex()
            .flex_col()
            .bg(hsla(s.panel))
            .text_size(px(theme.typography.ui_size))
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
                                        |e| entry(ix, e, open.contains(&ix), &view, &theme),
                                    )
                                },
                            )
                            .size_full(),
                        )
                    })
                    .when(!empty && !self.pinned(), |el| {
                        let latest = div()
                            .id("conversation-latest")
                            .debug_selector(|| "conversation-latest".to_owned())
                            .role(Role::Button)
                            .aria_label("Jump to latest")
                            .absolute()
                            .bottom(px(spacing.sm))
                            .right(px(spacing.md))
                            .px(px(spacing.sm))
                            .py(px(spacing.xxs))
                            .rounded(px(theme.radii.sm))
                            .bg(hsla(s.accent))
                            .text_color(hsla(s.accent_fg))
                            .text_size(px(theme.typography.small()))
                            .shadow_sm()
                            .cursor_pointer()
                            .child("↓ latest");
                        el.child(tab_stop(latest, s.accent).on_click(cx.listener(
                            |this, _ev, _window, cx| {
                                if let Some(c) = this.conversation() {
                                    c.pin();
                                }
                                cx.notify();
                            },
                        )))
                    }),
            )
            .when(!partial.is_empty(), |el| el.child(partial_row(partial, &theme)))
            .when_some(attention, |el, attention| el.child(attention_row(attention, &theme, cx)))
            .child(self.composer_row(composer_focused, &theme, cx))
            .into_any_element()
    }

    /// The composer: the growing text field (accent ring while it has the caret) and the send
    /// button, the one primary action on this surface.
    fn composer_row(&self, focused: bool, theme: &Theme, cx: &Context<TerminalView>) -> AnyElement {
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let send = theme.typography.ui_size + spacing.md;
        div()
            .id("composer")
            .debug_selector(|| "composer".to_owned())
            .role(Role::Group)
            .aria_label("Composer")
            .w_full()
            .flex_none()
            .flex()
            .items_end()
            .gap(px(spacing.sm))
            .px(px(spacing.md))
            .py(px(spacing.sm))
            .border_t_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.panel))
            .child(
                div()
                    .id("composer-field")
                    .debug_selector(|| "composer-field".to_owned())
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(spacing.sm))
                    .py(px(spacing.xs))
                    .rounded(px(theme.radii.sm))
                    .border_1()
                    .border_color(hsla(if focused { s.accent } else { s.border }))
                    .bg(hsla(s.raised))
                    .child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .aria_label("Message to Claude"),
                    ),
            )
            .child({
                let send = div()
                    .id("composer-send")
                    .debug_selector(|| "composer-send".to_owned())
                    .role(Role::Button)
                    .aria_label("Send")
                    .flex_none()
                    .w(px(send))
                    .h(px(send))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(theme.radii.sm))
                    .bg(hsla(s.accent))
                    .text_color(hsla(s.accent_fg))
                    .cursor_pointer()
                    .child("↑");
                tab_stop(send, s.accent)
                    .on_click(cx.listener(|this, _ev, window, cx| this.submit_composer(window, cx)))
            })
            .into_any_element()
    }
}

/// What the agent is writing right now, under the list and ahead of its next entry: the
/// Markdown so far, capped so a long answer scrolls in the entry it becomes.
fn partial_row(partial: &str, theme: &Theme) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let prose = gpui::relative(theme.typography.markdown_line_height);
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    div()
        .id("conversation-partial")
        .debug_selector(|| "conversation-partial".to_owned())
        .role(Role::Status)
        .aria_label("Claude is writing")
        .w_full()
        .flex_none()
        .max_h(px(theme.typography.ui_size * 12.0))
        .overflow_hidden()
        .px(px(spacing.md))
        .py(px(spacing.xs))
        .border_t_1()
        .border_color(hsla(s.border))
        .line_height(prose)
        .child(
            TextView::markdown("conversation-partial-md", SharedString::from(partial.to_owned()))
                .style(markdown_style(theme, &mono)),
        )
        .into_any_element()
}

/// What the agent waits for, with the one-tap answers, above the composer: the warn tone,
/// faint, since the agent is blocked on the human.
fn attention_row(attention: &Attention, theme: &Theme, cx: &Context<TerminalView>) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let status = match attention {
        Attention::Permission { tool, answered: None, detail: None } => {
            format!("Claude wants to use {tool}")
        }
        Attention::Permission { tool, answered: None, detail: Some(detail) } => {
            format!("Claude wants to use {tool}: {detail}")
        }
        Attention::Permission { tool, answered: Some(true), .. } => format!("{tool}: allowed"),
        Attention::Permission { tool, answered: Some(false), .. } => format!("{tool}: denied"),
        Attention::Prompt => "Claude is waiting for your answer".to_owned(),
    };
    let row = div()
        .id("conversation-attention")
        .debug_selector(|| "conversation-attention".to_owned())
        .role(Role::Status)
        .aria_label(SharedString::from(status))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(spacing.sm))
        .px(px(spacing.md))
        .py(px(spacing.sm))
        .border_t_1()
        .border_color(hsla(s.border))
        .bg(hsla_alpha(s.warn, alpha::TINT))
        .text_size(px(theme.typography.small()));
    let button = |id: &'static str, label: &'static str, accent: bool| {
        let tone = if accent { s.accent } else { s.text_secondary };
        let pill = div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Button)
            .aria_label(label)
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(theme.radii.xs))
            .bg(hsla_alpha(tone, alpha::TINT_STRONG))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_PRESSED)))
            .child(label);
        tab_stop(pill, s.accent)
    };
    match attention {
        Attention::Permission { tool, answered: None, detail } => row
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .child(SharedString::from(format!("Claude wants to use {tool}")))
                    .when_some(detail.as_ref(), |el, detail| {
                        el.child(
                            div()
                                .text_color(hsla(s.text_muted))
                                .font_family(
                                    theme
                                        .typography
                                        .mono_families
                                        .first()
                                        .cloned()
                                        .unwrap_or_default(),
                                )
                                .text_size(px(theme.typography.caption()))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(detail.clone())),
                        )
                    }),
            )
            .child(
                button("conversation-allow", "Allow", true)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(true, cx))),
            )
            .child(
                button("conversation-deny", "Deny", false)
                    .on_click(cx.listener(|this, _ev, _window, cx| this.answer(false, cx))),
            )
            .into_any_element(),
        Attention::Permission { tool, answered: Some(allowed), .. } => row
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
) -> AnyElement {
    let s = &theme.surfaces;
    let spacing = theme.spacing;
    let ui_size = theme.typography.ui_size;
    let small = theme.typography.small();
    let prose = gpui::relative(theme.typography.markdown_line_height);
    let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
    let row = div()
        .id(ElementId::NamedInteger("conversation-entry".into(), u64::try_from(ix).unwrap_or(0)))
        .debug_selector(move || format!("conversation-entry-{ix}"))
        .role(Role::ListItem)
        .aria_label(SharedString::from(entry_label(entry)))
        .w_full()
        .px(px(spacing.md))
        .py(px(spacing.xs));
    let time = clock(entry.at).map(|t| {
        div()
            .flex_none()
            .text_size(px(theme.typography.caption()))
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
            .gap(px(spacing.sm))
            .children(time)
            .child(
                div()
                    .max_w(px(ui_size * 40.0))
                    .px(px(spacing.md))
                    .py(px(spacing.sm))
                    .rounded(px(theme.radii.md))
                    .bg(hsla_alpha(s.accent, alpha::TINT_STRONG))
                    .line_height(prose)
                    .child(SharedString::from(text.clone())),
            )
            .into_any_element(),
        TranscriptBody::Assistant { markdown } => row
            .flex()
            .items_end()
            .gap(px(spacing.sm))
            .child(
                div().flex_1().min_w(px(0.0)).line_height(prose).child(
                    TextView::markdown(
                        ElementId::NamedInteger(
                            "conversation-md".into(),
                            u64::try_from(ix).unwrap_or(0),
                        ),
                        SharedString::from(markdown.clone()),
                    )
                    .style(markdown_style(theme, &mono)),
                ),
            )
            .children(time)
            .into_any_element(),
        TranscriptBody::Thinking { text } => row
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .text_size(px(small))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .cursor_pointer()
                    .child(SharedString::from(fold_label(open, "thinking", text)))
                    .on_click(toggle),
            )
            .when(open, |el| el.child(clipped_block(text, &mono, theme)))
            .into_any_element(),
        TranscriptBody::ToolUse { name, summary, input } => row
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .text_size(px(small))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .id("fold")
                    .flex()
                    .gap(px(spacing.sm))
                    .cursor_pointer()
                    .child(SharedString::from(format!("{} {name}", arrow(open))))
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(mono.clone())
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(summary.clone())),
                    )
                    .on_click(toggle),
            )
            .when(open, |el| el.child(clipped_block(input, &mono, theme)))
            .into_any_element(),
        TranscriptBody::ToolResult { tool, output, is_error } => {
            let (shown, hidden) = preview(output, open);
            let color = if *is_error { s.error } else { s.text_muted };
            row.flex()
                .flex_col()
                .gap(px(spacing.xs))
                .text_size(px(small))
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
                .when(!shown.text.is_empty(), |el| el.child(clipped_block(&shown, &mono, theme)))
                .into_any_element()
        }
    }
}

/// One entry as a screen reader hears it: who, then the first line.
#[must_use]
pub fn entry_label(entry: &TranscriptEntry) -> String {
    let first = |text: &str| text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned();
    match &entry.body {
        TranscriptBody::User { text } => format!("You: {}", first(text)),
        TranscriptBody::Assistant { markdown } => format!("Claude: {}", first(markdown)),
        TranscriptBody::Thinking { .. } => "Claude thinking".to_owned(),
        TranscriptBody::ToolUse { name, summary, .. } => format!("Tool {name}: {summary}"),
        TranscriptBody::ToolResult { tool, output, is_error } => format!(
            "Result of {}{}: {}",
            tool.as_deref().unwrap_or("a tool"),
            if *is_error { " failed" } else { "" },
            first(&output.text)
        ),
    }
}

/// Markdown in an assistant turn: paragraphs one base unit apart, headings stepping down
/// from the title size to the base, code in the terminal mono at `small()` on the raised
/// surface with `radii.xs` corners. Colours come from the gpui-kit theme, which
/// [`crate::kit::sync`] keeps on the same tokens.
fn markdown_style(theme: &Theme, mono: &str) -> TextViewStyle {
    let small = theme.typography.small();
    let code_block = gpui::StyleRefinement::default()
        .font_family(mono.to_owned())
        .text_size(px(small))
        .bg(hsla(theme.surfaces.raised))
        .rounded(px(theme.radii.xs))
        .px(px(theme.spacing.sm))
        .py(px(theme.spacing.xs));
    let inline_code = gpui::HighlightStyle {
        background_color: Some(hsla(theme.surfaces.raised)),
        color: Some(hsla(theme.surfaces.text)),
        ..gpui::HighlightStyle::default()
    };
    let (title, base) = (theme.typography.title(), theme.typography.ui_size);
    TextViewStyle {
        paragraph_gap: gpui::rems(theme.spacing.sm / base),
        heading_base_font_size: px(base),
        heading_font_size: Some(std::sync::Arc::new(move |level: u8, _base| {
            px((title - f32::from(level.saturating_sub(1))).max(base))
        })),
        code_block,
        inline_code,
        ..TextViewStyle::default()
    }
}

/// A clipped text as a mono block at `small()`, with the count of what the host or the fold
/// dropped.
fn clipped_block(text: &Clipped, mono: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .pl(px(theme.spacing.lg))
        .font_family(mono.to_owned())
        .text_size(px(theme.typography.small()))
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
