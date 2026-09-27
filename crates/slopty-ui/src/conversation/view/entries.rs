//! One row of the list, drawn: a prompt, a fold, an answer, thinking, a call at its level, a
//! group, a live block, the working line, a pending message.
//!
//! Every row sits on one reading column: the list's inset from the tile's edge, the text no
//! wider than [`READING`]. A call's title leads with a fixed square (its kind, or how it is
//! doing), so every verb starts on one edge and what a call shows under its title starts
//! there too: the scientific layout, a gutter of marks and a column of words.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_kit::component::text::TextView;
use slopty_proto::conversation::{
    Body, Clipped, Compact, Entry, LiveKind, Note, NoteKind, Prompt, ThreadId, ToolCall,
};

use super::ConversationView;
use crate::colors::hsla;
use crate::conversation::model::Expanded;
use crate::conversation::rows::{self, Fold, Level, Row, ToolKind};
use crate::conversation::tools::{self, State};
use crate::icons::{IconName, IconSize, Status};

/// The widest the reading column grows, in points at zoom 1: past it a line of prose is too
/// long to read, and a wide tile keeps its words in one column centred.
pub(super) const READING: f32 = 800.0;

impl ConversationView {
    /// Row `ix` of the list.
    pub(super) fn render_row(
        &self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let first = ix == 0;
        let body = match &row {
            Row::Prompt { id } => self.prompt_row(id, first, cx),
            Row::Fold { id, fold, open } => self.fold_row(id, fold, *open, cx),
            Row::Entry { id, level } => self.entry_row(id, *level, cx),
            Row::Group { kind, ids, open } => self.group_row(*kind, ids, *open, cx),
            Row::Live { id } => self.live_row(id),
            Row::Working => self.working_row(),
            Row::Pending { index } => self.pending_row(*index),
        };
        let last = ix.saturating_add(1) == self.rows.len();
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(self.z(self.theme.spacing.inset()))
            .when(last, |el| el.pb(self.z(self.theme.spacing.lg)))
            .child(div().w_full().max_w(self.z(READING)).min_w_0().child(body))
            .into_any_element()
    }

    /// The row's thread.
    fn shown_thread(&self) -> Option<&crate::conversation::model::Thread> {
        self.model.thread(&self.thread)
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.shown_thread()?.entry(id)
    }

    /// A clipped text as shown: whole once expanded.
    pub(super) fn text_of<'a>(&'a self, clipped: &'a Clipped) -> &'a str {
        match &clipped.full {
            Some(reference) => match self.model.expanded(&self.thread, reference) {
                Some(Expanded::Whole(whole)) => &whole.text,
                Some(Expanded::Gone) | None => &clipped.text,
            },
            None => &clipped.text,
        }
    }

    /// "Show all 240 lines", under a clipped text not yet expanded; asks the worker for it.
    pub(super) fn expand_link(
        &self,
        key: &str,
        clipped: &Clipped,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let reference = clipped.full.clone()?;
        let state = self.model.expanded(&self.thread, &reference);
        let s = self.theme.surfaces;
        if matches!(state, Some(Expanded::Gone)) {
            return Some(
                div()
                    .text_size(self.z(self.theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child("The transcript no longer has the rest")
                    .into_any_element(),
            );
        }
        if state.is_some() {
            return None;
        }
        let label = SharedString::from(format!(
            "Show all {}",
            tools::count(u64::from(clipped.lines), "line", "lines")
        ));
        let id = SharedString::from(format!("expand-{key}"));
        let selector = id.to_string();
        Some(
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(id))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .text_size(self.z(self.theme.typography.meta()))
                    .text_color(hsla(s.accent))
                    .cursor_pointer()
                    .hover(gpui::Styled::underline)
                    .child(label),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.expand(reference.clone(), cx)))
            .into_any_element(),
        )
    }

    /// Markdown at the face's size.
    pub(super) fn markdown(&self, id: String, text: &str) -> AnyElement {
        self.markdown_view(id, text).into_any_element()
    }

    /// [`Self::markdown`] in `tone` rather than the text's own ink.
    fn markdown_in(&self, id: String, text: &str, tone: slopty_theme::Rgb) -> AnyElement {
        self.markdown_view(id, text).text_color(hsla(tone)).into_any_element()
    }

    fn markdown_view(&self, id: String, text: &str) -> TextView {
        let mono = self.mono();
        TextView::markdown(ElementId::Name(id.into()), SharedString::from(text.to_owned()))
            .style(crate::markdown::style(&self.theme, &mono, self.zoom))
            .selectable(true)
    }

    /// The square every mark sits in, the width of a large icon.
    pub(super) fn slot(&self) -> Div {
        div()
            .flex_none()
            .size(self.z(self.theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_center()
    }

    /// Where what sits under a title starts: past its slot and the gap after it.
    pub(super) fn indent(&self) -> gpui::Pixels {
        self.z(self.theme.typography.icon_large() + self.theme.spacing.sm)
    }

    pub(super) fn icon(&self, name: IconName, tone: slopty_theme::Rgb) -> AnyElement {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(self.z(self.theme.typography.icon()))
            .into_any_element()
    }

    // ----- prompts ---------------------------------------------------------------------

    fn prompt_row(&self, id: &str, first: bool, cx: &Context<Self>) -> AnyElement {
        let Some(Entry { body: Body::Prompt(prompt), .. }) = self.entry(id) else {
            return div().into_any_element();
        };
        let prompt = prompt.clone();
        let theme = &self.theme;
        let top = if first { theme.spacing.md } else { theme.spacing.xl };
        self.prompt_card(&prompt, id, cx)
            .debug_selector({
                let id = id.to_owned();
                move || format!("prompt-{id}")
            })
            .mt(self.z(top))
            .mb(self.z(theme.spacing.sm))
            .into_any_element()
    }

    /// What the person sent, on the panel: a command as its name in the mono face, then its
    /// words.
    fn prompt_card(&self, prompt: &Prompt, key: &str, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let text = self.text_of(&prompt.text).to_owned();
        let label = match &prompt.command {
            Some(command) if command == "!" => format!("!{text}"),
            Some(command) => format!("{command} {text}"),
            None => text.clone(),
        };
        let command = prompt.command.clone().map(|command| {
            div()
                .flex_none()
                .font_family(self.mono())
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.accent))
                .child(SharedString::from(command))
        });
        let images = (prompt.images > 0).then(|| {
            div()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(tools::count(prompt.images.into(), "image", "images")))
        });
        let expand = self.expand_link(&format!("prompt-{key}"), &prompt.text, cx);
        div()
            .id(ElementId::Name(SharedString::from(format!("prompt-card-{key}"))))
            .role(Role::Article)
            .aria_label(SharedString::from(label))
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border_subtle))
            .child(
                div().flex().items_baseline().gap(self.z(theme.spacing.sm)).children(command).when(
                    !text.is_empty(),
                    |el| {
                        el.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_normal()
                                .child(SharedString::from(text)),
                        )
                    },
                ),
            )
            .children(images)
            .children(expand)
    }

    fn pending_row(&self, index: usize) -> AnyElement {
        let Some(pending) = self.model.pending().get(index) else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let s = theme.surfaces;
        let word = if pending.queued { "Queued" } else { "Sending" };
        div()
            .debug_selector(move || format!("pending-{index}"))
            .id(ElementId::Name(SharedString::from(format!("pending-{index}"))))
            .role(Role::Article)
            .aria_label(SharedString::from(format!("{word}: {}", pending.text)))
            .mt(self.z(theme.spacing.lg))
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.md))
            .border_1()
            .border_color(hsla(s.border))
            .text_color(hsla(s.text_secondary))
            .child(div().whitespace_normal().child(SharedString::from(pending.text.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(self.icon(IconName::Clock, s.text_muted))
                    .child(word),
            )
            .into_any_element()
    }

    // ----- folds and groups --------------------------------------------------------------

    fn fold_row(&self, id: &str, fold: &Fold, open: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let key = rows::fold_key(id);
        let selector = format!("fold-{id}");
        let mut parts: Vec<AnyElement> = vec![
            div()
                .text_color(hsla(s.text_secondary))
                .child(SharedString::from(fold.lead()))
                .into_any_element(),
        ];
        let dot = || div().text_color(hsla(s.text_muted)).child("\u{b7}").into_any_element();
        if let Some(steps) = fold.steps_label() {
            parts.push(dot());
            parts.push(
                div()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(steps))
                    .into_any_element(),
            );
        }
        if fold.added > 0 || fold.removed > 0 {
            parts.push(dot());
            parts.push(self.changes_label(fold.added, fold.removed));
        }
        if fold.failed > 0 {
            parts.push(dot());
            parts.push(
                div()
                    .text_color(hsla(s.error))
                    .child(SharedString::from(format!("{} failed", fold.failed)))
                    .into_any_element(),
            );
        }
        let chevron = if open { IconName::ChevronDown } else { IconName::ChevronRight };
        crate::a11y::tab_stop(
            crate::kit::tabular(div())
                .id(ElementId::Name(SharedString::from(selector.clone())))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(fold.label()))
                .aria_expanded(open)
                .group("fold")
                .w_full()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .h(self.z(theme.density.row))
                .text_size(self.z(theme.typography.small()))
                .cursor_pointer()
                .child(self.slot().child(self.icon(chevron, s.text_muted)))
                .children(parts)
                .child(
                    div()
                        .flex_1()
                        .ml(self.z(theme.spacing.sm))
                        .h(gpui::px(1.0))
                        .bg(hsla(s.border_subtle))
                        .group_hover("fold", |el| el.bg(hsla(s.border))),
                ),
            s.accent,
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key.clone(), cx)))
        .into_any_element()
    }

    /// `+a −r` in the success and error tones.
    pub(super) fn changes_label(&self, added: u32, removed: u32) -> AnyElement {
        let s = self.theme.surfaces;
        crate::kit::tabular(div())
            .flex()
            .gap(self.z(self.theme.spacing.xs))
            .child(div().text_color(hsla(s.success)).child(SharedString::from(format!("+{added}"))))
            .child(
                div()
                    .text_color(hsla(s.error))
                    .child(SharedString::from(format!("\u{2212}{removed}"))),
            )
            .into_any_element()
    }

    fn group_row(
        &self,
        kind: ToolKind,
        ids: &[String],
        open: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entries: Vec<&Entry> = ids.iter().filter_map(|id| self.entry(id)).collect();
        let (icon, verb, meta) = match kind {
            ToolKind::Tasks => {
                (IconName::ListChecks, "Kept the task list", tools::tasks_kept(&entries))
            }
            _ => (IconName::Search, "Explored", tools::explored(&entries)),
        };
        let first = ids.first().cloned().unwrap_or_default();
        let key = rows::group_key(&first);
        let running = entries.iter().any(|e| match &e.body {
            Body::Tool(call) => call.result.is_none(),
            _ => false,
        });
        self.title_line(
            &format!("group-{first}"),
            TitleParts {
                mark: if running { Mark::Running } else { Mark::Icon(icon) },
                verb: verb.to_owned(),
                subject: None,
                code: false,
                meta: Some(meta),
                meta_tone: None,
                expandable: Some(open),
            },
            key,
            cx,
        )
    }

    // ----- entries ---------------------------------------------------------------------

    fn entry_row(&self, id: &str, level: Level, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.entry(id).cloned() else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        match &entry.body {
            Body::Text(text) => {
                let shown = self.text_of(text).to_owned();
                div()
                    .debug_selector({
                        let id = id.to_owned();
                        move || format!("answer-{id}")
                    })
                    .id(ElementId::Name(SharedString::from(format!("answer-{id}"))))
                    .role(Role::Article)
                    .aria_label(SharedString::from(first_line(&shown)))
                    .py(self.z(theme.spacing.xs))
                    .text_size(self.z(theme.typography.ui_size))
                    .line_height(gpui::relative(theme.typography.markdown_line_height))
                    .child(self.markdown(format!("md-{}-{id}", self.session), &shown))
                    .children(self.expand_link(id, text, cx))
                    .into_any_element()
            }
            Body::Thinking(text) => self.thinking_block(id, self.text_of(text), cx),
            Body::Tool(call) => self.tool_row(&entry, call, level, cx),
            Body::Compact(compact) => self.compact_row(id, compact, level, cx),
            Body::Interrupted { during_tool } => {
                let words =
                    if *during_tool { "Interrupted while a tool ran" } else { "Interrupted" };
                div()
                    .id(ElementId::Name(SharedString::from(format!("esc-{id}"))))
                    .role(Role::Status)
                    .aria_label(words)
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .h(self.z(theme.density.row))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(self.slot().child(self.icon(IconName::CirclePause, s.text_muted)))
                    .child(words)
                    .into_any_element()
            }
            Body::Note(note) => self.note_row(id, note, cx),
            Body::Prompt(prompt) => self.prompt_card(prompt, id, cx).into_any_element(),
        }
    }

    fn thinking_block(&self, id: &str, text: &str, _cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .id(ElementId::Name(SharedString::from(format!("thinking-row-{id}"))))
            .role(Role::Article)
            .aria_label("Thinking")
            .py(self.z(theme.spacing.xs))
            .flex()
            .gap(self.z(theme.spacing.sm))
            .child(self.slot().child(self.icon(IconName::Brain, s.text_muted)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl(self.z(theme.spacing.sm))
                    .border_l_2()
                    .border_color(hsla(s.border_subtle))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .italic()
                    .whitespace_normal()
                    .debug_selector({
                        let id = id.to_owned();
                        move || format!("thinking-{id}")
                    })
                    .child(SharedString::from(text.to_owned())),
            )
            .into_any_element()
    }

    fn compact_row(
        &self,
        id: &str,
        compact: &Compact,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let tokens = match (compact.pre_tokens, compact.post_tokens) {
            (Some(pre), Some(post)) => {
                format!("{} \u{2192} {} tokens", tools::tokens(pre), tools::tokens(post))
            }
            (Some(pre), None) => format!("{} tokens", tools::tokens(pre)),
            _ => String::new(),
        };
        let words = match compact.trigger.as_deref() {
            Some("auto") => "Compacted on its own",
            _ => "Compacted",
        };
        let label =
            if tokens.is_empty() { words.to_owned() } else { format!("{words} \u{b7} {tokens}") };
        let key = rows::entry_key(id);
        let open = level == Level::Full || self.toggled.contains(&key);
        let rule = || div().flex_1().h(gpui::px(1.0)).bg(hsla(s.border_subtle));
        let summary = compact.summary.as_ref().filter(|_| open).map(|summary| {
            div()
                .pl(self.indent())
                .pb(self.z(theme.spacing.sm))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(
                    self.markdown(format!("compact-{}-{id}", self.session), self.text_of(summary)),
                )
        });
        let selector = format!("compact-{id}");
        div()
            .flex()
            .flex_col()
            .my(self.z(theme.spacing.md))
            .child(
                crate::a11y::tab_stop(
                    crate::kit::tabular(div())
                        .id(ElementId::Name(SharedString::from(selector.clone())))
                        .debug_selector(move || selector)
                        .role(Role::Button)
                        .aria_label(SharedString::from(label.clone()))
                        .aria_expanded(open)
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.sm))
                        .h(self.z(theme.density.row))
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .cursor_pointer()
                        .child(rule())
                        .child(self.icon(IconName::Scissors, s.text_muted))
                        .child(SharedString::from(label))
                        .child(rule()),
                    s.accent,
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key.clone(), cx))),
            )
            .children(summary)
            .into_any_element()
    }

    fn note_row(&self, id: &str, note: &Note, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let text = self.text_of(&note.text).to_owned();
        let (icon, tone) = match note.kind {
            NoteKind::ApiError => (IconName::CircleAlert, s.error),
            NoteKind::Command => (IconName::SquareTerminal, s.text_muted),
            NoteKind::Info => (IconName::Info, s.text_muted),
        };
        let body = if note.kind == NoteKind::Command {
            self.code_text(&text, s.text_secondary).into_any_element()
        } else {
            div()
                .whitespace_normal()
                .text_color(hsla(if rows::note_is_error(note.kind) {
                    s.error
                } else {
                    s.text_secondary
                }))
                .child(SharedString::from(text.clone()))
                .into_any_element()
        };
        div()
            .id(ElementId::Name(SharedString::from(format!("note-{id}"))))
            .role(Role::Note)
            .aria_label(SharedString::from(first_line(&text)))
            .py(self.z(theme.spacing.xs))
            .flex()
            .gap(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.small()))
            .child(self.slot().child(self.icon(icon, tone)))
            .child(
                div().flex_1().min_w_0().child(body).children(self.expand_link(id, &note.text, cx)),
            )
            .into_any_element()
    }

    /// A call: its title line, then what its level shows under it.
    fn tool_row(
        &self,
        entry: &Entry,
        call: &ToolCall,
        level: Level,
        cx: &Context<Self>,
    ) -> AnyElement {
        let tasks =
            self.shown_thread().map(crate::conversation::model::Thread::tasks).unwrap_or_default();
        let title = tools::title(call, tasks);
        let kind = rows::tool_kind(call);
        let key = rows::entry_key(&entry.id);
        let body = match level {
            Level::Title => None,
            Level::Summary | Level::Full => self.tool_body(entry, call, level, cx),
        };
        let expandable = Self::expandable(call, level);
        let meta_tone = match title.state {
            State::Failed => Some(self.theme.surfaces.error),
            _ => None,
        };
        let mark = match title.state {
            State::Running => Mark::Running,
            State::Failed => Mark::Status(Status::Failed),
            State::Stopped => Mark::Icon(IconName::CirclePause),
            State::Done => Mark::Icon(title.icon),
        };
        let changes = match (kind, &title.meta) {
            (ToolKind::Change, Some(meta)) if meta.starts_with('+') => {
                let (added, removed) = rows::entry_changes(entry);
                Some(self.changes_label(added, removed))
            }
            _ => None,
        };
        let line = self.title_line(
            &format!("tool-{}", entry.id),
            TitleParts {
                mark,
                verb: title.verb,
                subject: title.subject,
                code: title.code,
                meta: if changes.is_some() { None } else { title.meta },
                meta_tone,
                expandable: expandable.then_some(level == Level::Full),
            },
            key,
            cx,
        );
        let line = match changes {
            Some(changes) => div()
                .relative()
                .child(line)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(self.z(self.theme.typography.icon_large() + self.theme.spacing.xs))
                        .flex()
                        .items_center()
                        .text_size(self.z(self.theme.typography.meta()))
                        .child(changes),
                )
                .into_any_element(),
            None => line,
        };
        div()
            .flex()
            .flex_col()
            .child(line)
            .children(
                body.map(|body| {
                    div().pl(self.indent()).pb(self.z(self.theme.spacing.sm)).child(body)
                }),
            )
            .into_any_element()
    }

    /// Whether a click on a call's title shows more or less of it.
    fn expandable(call: &ToolCall, level: Level) -> bool {
        level == Level::Full || super::blocks::has_body(call)
    }

    /// A title line: the mark in its slot, the verb, the subject, a fact at the end, and the
    /// chevron when a click opens it. The whole line is the button.
    pub(super) fn title_line(
        &self,
        id: &str,
        parts: TitleParts,
        key: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let label = [Some(parts.verb.clone()), parts.subject.clone(), parts.meta.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let mark = match parts.mark {
            Mark::Running => crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.accent),
            ),
            Mark::Status(status) => crate::icons::status_icon(
                theme,
                status,
                self.z(theme.typography.icon()),
                hsla(status.tone(theme)),
            ),
            Mark::Icon(icon) => self.icon(icon, s.text_muted),
        };
        let subject = parts.subject.map(|subject| {
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(hsla(s.text))
                .when(parts.code, |el| {
                    el.font_family(self.mono()).text_size(self.z(theme.typography.meta()))
                })
                .child(SharedString::from(subject))
        });
        let meta = parts.meta.map(|meta| {
            crate::kit::tabular(div())
                .flex_none()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(parts.meta_tone.unwrap_or(s.text_muted)))
                .child(SharedString::from(meta))
        });
        let chevron = parts.expandable.map(|open| {
            self.slot()
                .invisible()
                .group_hover("title", gpui::Styled::visible)
                .when(open, gpui::Styled::visible)
                .child(self.icon(
                    if open { IconName::ChevronDown } else { IconName::ChevronRight },
                    s.text_muted,
                ))
        });
        let selector = id.to_owned();
        let line = div()
            .id(ElementId::Name(SharedString::from(id.to_owned())))
            .debug_selector(move || selector)
            .group("title")
            .role(if parts.expandable.is_some() { Role::Button } else { Role::ListItem })
            .aria_label(SharedString::from(label))
            .when_some(parts.expandable, gpui::StatefulInteractiveElement::aria_expanded)
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .h(self.z(theme.density.row))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
            .child(self.slot().child(mark))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(parts.verb)),
                    )
                    .children(subject)
                    .child(div().flex_1())
                    .children(meta),
            )
            .children(chevron);
        match parts.expandable {
            Some(_) => crate::a11y::tab_stop(
                line.cursor_pointer().hover(move |el| el.bg(hsla(s.raised))),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key.clone(), cx)))
            .into_any_element(),
            None => line.into_any_element(),
        }
    }

    // ----- what is live ------------------------------------------------------------------

    fn live_row(&self, id: &slopty_proto::conversation::LiveId) -> AnyElement {
        let Some(block) = self.model.live_block_at(id) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let key = format!("{}-{}-{}", id.turn, id.step, id.block);
        match &block.kind {
            // What the model is writing reads a step lighter than what the transcript settled,
            // until the entry for it comes and takes its place.
            LiveKind::Text => div()
                .debug_selector({
                    let key = key.clone();
                    move || format!("live-{key}")
                })
                .id(ElementId::Name(SharedString::from(format!("live-{key}"))))
                .role(Role::Article)
                .aria_label(SharedString::from(format!("Writing: {}", first_line(&block.text))))
                .py(self.z(theme.spacing.xs))
                .line_height(gpui::relative(theme.typography.markdown_line_height))
                .child(self.markdown_in(
                    format!("live-md-{}-{key}", self.session),
                    &block.text,
                    s.text_secondary,
                ))
                .into_any_element(),
            LiveKind::Thinking => div()
                .id(ElementId::Name(SharedString::from(format!("live-{key}"))))
                .role(Role::Article)
                .aria_label("Thinking")
                .debug_selector(move || format!("live-{key}"))
                .py(self.z(theme.spacing.xs))
                .flex()
                .gap(self.z(theme.spacing.sm))
                .child(self.slot().child(self.icon(IconName::Brain, s.text_muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl(self.z(theme.spacing.sm))
                        .border_l_2()
                        .border_color(hsla(s.border_subtle))
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .italic()
                        .whitespace_normal()
                        .child(SharedString::from(block.text.clone())),
                )
                .into_any_element(),
            LiveKind::Tool { name, .. } => {
                let input = tools::preparing(&block.text);
                div()
                    .id("live-tool")
                    .debug_selector(move || format!("live-{key}"))
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("Preparing {name}")))
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .h(self.z(theme.density.row))
                    .text_size(self.z(theme.typography.small()))
                    .child(self.slot().child(crate::icons::status_icon(
                        theme,
                        Status::Working,
                        self.z(theme.typography.icon()),
                        hsla(s.accent),
                    )))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(self.mono())
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(input)),
                    )
                    .into_any_element()
            }
        }
    }

    fn working_row(&self) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let thinking = self.model.live(&ThreadId::Main).any(|(_, b)| b.kind == LiveKind::Thinking);
        let since = self.agent.as_ref().map_or(0, |a| a.since_ms);
        let elapsed = (since > 0).then(|| rows::took(super::now_ms().saturating_sub(since)));
        let word = if thinking { "Thinking" } else { "Working" };
        crate::kit::tabular(div())
            .id("working")
            .debug_selector(|| "working".to_owned())
            .role(Role::Status)
            .aria_label(word)
            .mt(self.z(theme.spacing.xs))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .h(self.z(theme.density.row))
            .text_size(self.z(theme.typography.small()))
            .child(self.slot().child(crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.accent),
            )))
            .child(div().text_color(hsla(s.text_secondary)).child(word))
            .children(
                elapsed.map(|e| div().text_color(hsla(s.text_muted)).child(SharedString::from(e))),
            )
            .into_any_element()
    }
}

/// What a title line's slot shows.
#[derive(Clone, Copy, Debug)]
pub(super) enum Mark {
    /// The working spinner.
    Running,
    /// A status's mark in its tone.
    Status(Status),
    /// An icon in the muted tone.
    Icon(IconName),
}

/// The pieces of a title line.
#[derive(Clone, Debug)]
pub(super) struct TitleParts {
    pub mark: Mark,
    pub verb: String,
    pub subject: Option<String>,
    pub code: bool,
    pub meta: Option<String>,
    pub meta_tone: Option<slopty_theme::Rgb>,
    /// `Some(open)` when a click opens or closes it.
    pub expandable: Option<bool>,
}

/// The first line of a text with something on it.
pub(super) fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned()
}
