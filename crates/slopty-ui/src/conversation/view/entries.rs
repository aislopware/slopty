//! One row of the list, drawn: a prompt, a fold, an answer, thinking, a call at its level, a
//! group, the files a turn changed, a live block, the working line, a pending message.
//!
//! Every row sits on one reading column, centred once the tile is wider than it. Prose (a
//! prompt, an answer) is set at the prose size and carries no frame but the prompt's bubble.
//! A call is a quiet line: a mark in a fixed square, so every verb starts on one edge, the verb
//! in the secondary tone, its subject in the text tone at the medium weight, a fact at the far
//! right. What a call shows under its title starts where the verb does.

use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClipboardItem, Context, Div, ElementId, FontWeight,
    InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};
use gpui_kit::component::text::{TextView, TextViewStyle};
use slopty_core::WallMs;
use slopty_proto::conversation::{
    Body, Clipped, Compact, Entry, LiveKind, Note, NoteKind, Prompt, ThreadId, ToolCall,
};
use slopty_theme::{Rgb, Typography, alpha};

use super::ConversationView;
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::figures;
use crate::conversation::model::Expanded;
use crate::conversation::rows::{self, Fold, Level, Row, ToolKind};
use crate::conversation::tools::{self, State};
use crate::icons::{IconName, IconSize, Status};
use crate::kit::first_line;

/// The widest the reading column grows, in points at zoom 1: between T3 Code's 768 and Amp's
/// 672, the measure of a comfortable line at the prose size.
pub(super) const READING: f32 = 720.0;

/// A call's line: its least height and the square its mark sits in.
pub(super) const TOOL_ROW: f32 = 24.0;

/// A changed file's row, whose hover shows the way into its diff.
const CHANGED_GROUP: &str = "changed-file";

/// The widest a prompt's bubble grows, as a share of the column: what is left beside it holds
/// the prompt's time and copy.
const BUBBLE: f32 = 0.85;

/// What a queued message's state says under the pointer.
const QUEUED_HINT: &str = "Sends when Claude finishes this step";

/// The gap between an answer's paragraphs, in points at the prose size.
const PARAGRAPH: f32 = 10.0;

/// The hover groups whose rows show a time and a copy.
const PROMPT_GROUP: &str = "prompt";
const ANSWER_GROUP: &str = "answer";

/// Markdown headings at the prose base: h1, h2, then the rest at the prose size.
const HEADINGS: [f32; 2] = [18.0, 16.0];

impl ConversationView {
    /// Row `ix` of the list.
    pub(super) fn render_row(
        &self,
        ix: usize,
        _window: &mut Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let rows = Rc::clone(&self.rows);
        let Some(row) = rows.get(ix) else { return div().into_any_element() };
        let first = ix == 0;
        let body = match row {
            Row::Prompt { id } => self.prompt_row(id, first, cx),
            Row::Fold { id, fold, open } => self.fold_row(id, fold, *open, cx),
            Row::Answer { id, end } => self.answer_row(id, *end, cx),
            Row::Changes { prompt } => self.changes_row(prompt, cx),
            Row::File { path } => self.file_row(path, first),
            Row::Edit { thread, id } => self.edit_row(thread, id, cx),
            Row::Entry { id, level } => self.entry_row(id, *level, cx),
            Row::Group { kind, ids, open } => self.group_row(*kind, ids, *open, cx),
            Row::Live { id } => self.live_row(id, cx),
            Row::Working => self.working_row(),
            Row::Pending { index } => self.pending_row(*index),
        };
        // A row a fold just opened settles in; the list lays it out at its full height from
        // the first frame, so only its ink moves.
        let settling = (!self.settling.is_empty()).then(|| self.settling.get(&row.key())).flatten();
        let body = match settling {
            Some(generation) if crate::kit::motion(cx) => gpui::AnimationExt::with_animation(
                div().child(body),
                ElementId::NamedInteger("settle".into(), row.key().number() ^ *generation),
                crate::kit::Pace::Settle.animation(),
                gpui::Styled::opacity,
            )
            .into_any_element(),
            _ => body,
        };
        let last = ix.saturating_add(1) == self.rows.len();
        let found = self.find_row() == Some(ix);
        let s = self.theme.surfaces;
        div()
            .w_full()
            .flex()
            .justify_center()
            .px(self.z(self.theme.spacing.inset()))
            .when(last, |el| el.pb(self.z(self.theme.spacing.lg)))
            .child(
                div()
                    .w_full()
                    .max_w(self.z(READING))
                    .min_w_0()
                    .when(found, |el| {
                        // The match the find bar is on: a wash under the whole row, the
                        // selection's hue at its faint step.
                        let wash = hsla_alpha(s.accent_fill, alpha::FAINT);
                        el.rounded(self.z(self.theme.radii.sm)).bg(wash)
                    })
                    .child(body),
            )
            .into_any_element()
    }

    /// The row's thread.
    pub(super) fn shown_thread(&self) -> Option<&crate::conversation::model::Thread> {
        self.model.thread(&self.thread)
    }

    pub(super) fn entry(&self, id: &str) -> Option<&Entry> {
        self.shown_thread()?.entry(id)
    }

    /// A clipped text as shown: whole once expanded.
    pub(super) fn text_of<'a>(&'a self, clipped: &'a Clipped) -> &'a str {
        self.text_in(&self.thread, clipped)
    }

    /// [`Self::text_of`] for a text of `thread`.
    pub(super) fn text_in<'a>(&'a self, thread: &ThreadId, clipped: &'a Clipped) -> &'a str {
        match &clipped.full {
            Some(reference) => match self.model.expanded(thread, reference) {
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
        self.expand_link_in(&self.thread, key, clipped, cx)
    }

    /// [`Self::expand_link`] for a text of `thread`.
    pub(super) fn expand_link_in(
        &self,
        thread: &ThreadId,
        key: &str,
        clipped: &Clipped,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let reference = clipped.full.clone()?;
        let state = self.model.expanded(thread, &reference);
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
        let thread = thread.clone();
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
            .on_click(cx.listener(move |_this, _ev, _w, cx| {
                Self::expand_in(thread.clone(), reference.clone(), cx);
            }))
            .into_any_element(),
        )
    }

    /// The prose look: the theme's Markdown at the prose size, headings stepping 18, 16, 14 at
    /// the strong weight, paragraphs 10 apart, code in the mono face on the raised surface.
    fn prose_style(&self) -> TextViewStyle {
        let theme = &self.theme;
        let z = self.zoom;
        let base = theme.typography.prose();
        let mono = self.mono();
        let mut style = crate::markdown::style(theme, &mono, z);
        style.paragraph_gap = gpui::rems(PARAGRAPH / base);
        style.heading_base_font_size = px(base * z);
        style.heading_font_size = Some(std::sync::Arc::new(move |level: u8, _base| {
            let size = match level {
                1 => HEADINGS[0],
                2 => HEADINGS[1],
                _ => base,
            };
            px(size * z)
        }));
        style.code_block = gpui::StyleRefinement::default()
            .font_family(mono.to_string())
            .text_size(px(theme.typography.small() * z))
            .bg(hsla(theme.surfaces.raised))
            .rounded(px(theme.radii.md * z))
            .px(px(theme.spacing.md * z))
            .py(px(theme.spacing.sm * z));
        style
    }

    /// Markdown at the prose size, its code blocks each with their language and a copy.
    pub(super) fn markdown(&self, id: String, text: &str) -> AnyElement {
        self.markdown_view(id, text).into_any_element()
    }

    fn markdown_view(&self, id: String, text: &str) -> TextView {
        let theme = std::sync::Arc::clone(&self.shared);
        let zoom = self.zoom;
        TextView::markdown(ElementId::Name(id.into()), SharedString::from(text.to_owned()))
            .style(self.prose_style())
            .selectable(true)
            .code_block_actions(move |block, _window, _cx| code_actions(&theme, zoom, block))
    }

    /// The square every mark sits in.
    pub(super) fn slot(&self) -> Div {
        div().flex_none().size(self.z(TOOL_ROW)).flex().items_center().justify_center()
    }

    /// Where what sits under a title starts: past its slot and the gap after it.
    pub(super) fn indent(&self) -> gpui::Pixels {
        self.z(TOOL_ROW + self.theme.spacing.xs)
    }

    pub(super) fn icon(&self, name: IconName, tone: Rgb) -> AnyElement {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(self.z(self.theme.typography.icon()))
            .into_any_element()
    }

    /// A small icon button that shows only while the pointer is on the row named `group`
    /// (always, under a finger), and says it copied once it did.
    pub(super) fn copy_button(
        &self,
        key: String,
        group: &'static str,
        what: &'static str,
        text: String,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let copied = self.copied.as_deref() == Some(key.as_str());
        let touch = self.theme.density == slopty_theme::Density::TOUCH;
        let selector = format!("copy-{key}");
        let label: SharedString = if copied { "Copied".into() } else { what.into() };
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(selector.clone().into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .size(self.z(self.theme.typography.icon_large()))
                .flex()
                .items_center()
                .justify_center()
                .rounded(self.z(self.theme.radii.xs))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .when(!touch && !copied, |el| {
                    el.invisible().group_hover(group, gpui::Styled::visible)
                })
                .child(self.icon(
                    if copied { IconName::Check } else { IconName::Copy },
                    if copied { s.success } else { s.text_muted },
                )),
            s.accent,
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| this.copy(key.clone(), text.clone(), cx)))
        .into_any_element()
    }

    /// A time of day at the meta size, shown with the row's copy.
    fn clock_label(&self, at_ms: WallMs, group: &'static str) -> Option<AnyElement> {
        let s = self.theme.surfaces;
        let touch = self.theme.density == slopty_theme::Density::TOUCH;
        figures::clock(at_ms).map(|time| {
            crate::kit::tabular(div())
                .flex_none()
                .text_size(self.z(self.theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .when(!touch, |el| el.invisible().group_hover(group, gpui::Styled::visible))
                .child(SharedString::from(time))
                .into_any_element()
        })
    }

    // ----- prompts ---------------------------------------------------------------------

    fn prompt_row(&self, id: &str, first: bool, cx: &Context<Self>) -> AnyElement {
        let Some(Entry { body: Body::Prompt(prompt), at_ms, .. }) = self.entry(id) else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let top = if first { theme.spacing.md } else { theme.spacing.xl };
        let words = self.prompt_words(prompt);
        div()
            .id(ElementId::Name(SharedString::from(format!("prompt-row-{id}"))))
            .debug_selector({
                let id = id.to_owned();
                move || format!("prompt-{id}")
            })
            .group(PROMPT_GROUP)
            .w_full()
            .flex()
            .items_start()
            .gap(self.z(theme.spacing.sm))
            .mt(self.z(top))
            .mb(self.z(theme.spacing.md))
            .child(self.prompt_card(prompt, id, cx))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .pt(self.z(theme.spacing.xs))
                    .children(self.clock_label(*at_ms, PROMPT_GROUP))
                    .children(self.rewind_button(id, PROMPT_GROUP, cx))
                    .child(self.copy_button(
                        format!("p:{id}"),
                        PROMPT_GROUP,
                        "Copy prompt",
                        words,
                        cx,
                    )),
            )
            .into_any_element()
    }

    /// A prompt as it was typed: a command with its name.
    pub(super) fn prompt_words(&self, prompt: &Prompt) -> String {
        let text = self.text_of(&prompt.text);
        match &prompt.command {
            Some(command) if command == "!" => format!("!{text}"),
            Some(command) if text.is_empty() => command.clone(),
            Some(command) => format!("{command} {text}"),
            None => text.to_owned(),
        }
    }

    /// What the person sent, as a bubble on the column's left edge: the raised surface at the
    /// floating radius, the prose size, a command's name in the mono face and the accent.
    fn prompt_card(&self, prompt: &Prompt, key: &str, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let text = self.text_of(&prompt.text).to_owned();
        let command = prompt.command.clone().map(|command| {
            div()
                .flex_none()
                .font_family(self.mono())
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.accent))
                .child(SharedString::from(command))
        });
        let images = (!prompt.images.is_empty())
            .then(|| self.thumbnails(&format!("prompt-{key}"), &prompt.images, cx));
        let expand = self.expand_link(&format!("prompt-{key}"), &prompt.text, cx);
        let mentions = self.mention_chips(key, &text, cx);
        div()
            .id(ElementId::Name(SharedString::from(format!("prompt-card-{key}"))))
            .role(Role::Article)
            .aria_label(SharedString::from(self.prompt_words(prompt)))
            .flex_initial()
            .min_w_0()
            .max_w(relative(BUBBLE))
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.lg))
            .bg(hsla(s.raised))
            .text_size(self.z(theme.typography.prose()))
            .line_height(relative(theme.typography.prose_line_height))
            .children(images)
            .when(command.is_some() || !text.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(self.z(theme.spacing.sm))
                        .children(command)
                        .when(!text.is_empty(), |el| {
                            el.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(self.mentioned_text(text)),
                            )
                        }),
                )
            })
            .children(mentions)
            .children(expand)
    }

    /// A message typed while Claude works, until the transcript records it: the prompt
    /// bubble's own shape, set back, and under it what is happening to it. One border style in
    /// the face, so no dashed outline.
    fn pending_row(&self, index: usize) -> AnyElement {
        let Some(pending) = self.model.pending().get(index) else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let s = theme.surfaces;
        let word = if pending.queued { "Queued" } else { "Sending" };
        let state = div()
            .id(ElementId::Name(SharedString::from(format!("pending-state-{index}"))))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(s.text_muted))
            .child(self.icon(IconName::Clock, s.text_muted))
            .child(word);
        let state = if pending.queued {
            let hint_theme = Rc::clone(&self.hint_theme);
            state
                .tooltip(move |_window, cx| {
                    let theme = Rc::clone(&hint_theme);
                    cx.new(|_| crate::kit::Hint::new(QUEUED_HINT, "", theme)).into()
                })
                .into_any_element()
        } else {
            state.into_any_element()
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .mt(self.z(theme.spacing.lg))
            .child(
                div()
                    .debug_selector(move || format!("pending-{index}"))
                    .id(ElementId::Name(SharedString::from(format!("pending-{index}"))))
                    .role(Role::Article)
                    .aria_label(SharedString::from(format!("{word}: {}", pending.text)))
                    .min_w_0()
                    .max_w(relative(BUBBLE))
                    .px(self.z(theme.spacing.md))
                    .py(self.z(theme.spacing.sm))
                    .rounded(self.z(theme.radii.lg))
                    .bg(hsla_alpha(s.raised, alpha::STRONG))
                    .text_size(self.z(theme.typography.prose()))
                    .text_color(hsla(s.text_secondary))
                    .child(
                        div().whitespace_normal().child(SharedString::from(pending.text.clone())),
                    ),
            )
            .child(state)
            .into_any_element()
    }

    // ----- folds, answers and what a turn changed --------------------------------------------

    fn fold_row(&self, id: &str, fold: &Fold, open: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let key = rows::fold_key(id);
        let selector = format!("fold-{id}");
        let dot = || crate::kit::separator(theme).into_any_element();
        let mut parts: Vec<AnyElement> = vec![
            div()
                .text_color(hsla(s.text_secondary))
                .child(SharedString::from(fold.lead()))
                .into_any_element(),
        ];
        for what in fold.what() {
            parts.push(dot());
            parts.push(div().flex_none().child(SharedString::from(what)).into_any_element());
        }
        if fold.added > 0 || fold.removed > 0 {
            parts.push(dot());
            parts.push(self.changes_label(fold.added, fold.removed));
        }
        if fold.failed > 0 {
            parts.push(dot());
            parts.push(
                div()
                    .child(SharedString::from(format!("{} failed", fold.failed)))
                    .into_any_element(),
            );
        }
        let figures = self.shown_thread().and_then(|t| t.turn(id));
        let running = self.running_model();
        let meta = figures.and_then(|t| figures::turn_meta(t, running.as_deref()));
        let hint = figures
            .map(|t| figures::turn_detail(t, self.model.meters().and_then(|m| m.context_window)))
            .filter(|h| !h.is_empty());
        let label = match &meta {
            Some(meta) => format!("{} \u{b7} {meta}", fold.label()),
            None => fold.label(),
        };
        let chevron = if open { IconName::ChevronDown } else { IconName::ChevronRight };
        let line = crate::kit::tabular(div())
            .id(ElementId::Name(SharedString::from(selector.clone())))
            .debug_selector(move || selector)
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .aria_expanded(open)
            .group("fold")
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .h(self.z(theme.density.row))
            .mb(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .child(self.slot().child(self.icon(chevron, s.text_muted)))
            .children(parts)
            .child(
                div()
                    .flex_1()
                    .mx(self.z(theme.spacing.sm))
                    .h(px(1.0))
                    .bg(hsla(s.border_subtle))
                    .group_hover("fold", |el| el.bg(hsla(s.border))),
            )
            .children(meta.map(|meta| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .child(SharedString::from(meta))
            }))
            .when_some(hint, |el, hint| {
                let theme = Rc::clone(&self.hint_theme);
                el.tooltip(move |_window, cx| {
                    let (hint, theme) = (hint.clone(), Rc::clone(&theme));
                    cx.new(|_| crate::kit::Hint::new(hint, "", theme)).into()
                })
            });
        crate::a11y::tab_stop(line, s.accent)
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key, cx)))
            .into_any_element()
    }

    /// `+a −r` as chrome draws it everywhere ([`crate::kit::changes`]), at the face's zoom.
    pub(super) fn changes_label(&self, added: u32, removed: u32) -> AnyElement {
        crate::kit::changes_at(&self.theme, added, removed, self.zoom)
            .map_or_else(|| div().into_any_element(), gpui::IntoElement::into_any_element)
    }

    /// An answer: prose on the column, no frame. The last of a settled turn ends on its time
    /// and a copy, shown while the pointer is on it.
    fn answer_row(&self, id: &str, end: bool, cx: &Context<Self>) -> AnyElement {
        let Some(Entry { body: Body::Text(text), at_ms, .. }) = self.entry(id) else {
            return div().into_any_element();
        };
        let theme = &self.theme;
        let shown = self.text_of(text);
        let foot = end.then(|| {
            div()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .h(self.z(theme.typography.icon_large()))
                .child(self.copy_button(
                    format!("a:{id}"),
                    ANSWER_GROUP,
                    "Copy answer",
                    shown.to_owned(),
                    cx,
                ))
                .children(self.clock_label(*at_ms, ANSWER_GROUP))
        });
        div()
            .debug_selector({
                let id = id.to_owned();
                move || format!("answer-{id}")
            })
            .id(ElementId::Name(SharedString::from(format!("answer-{id}"))))
            .role(Role::Article)
            .aria_label(SharedString::from(first_line(shown).to_owned()))
            .group(ANSWER_GROUP)
            .py(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.prose()))
            .line_height(relative(theme.typography.prose_line_height))
            .text_color(hsla(theme.surfaces.text))
            .child(self.markdown(format!("md-{}-{id}", self.session), shown))
            .children(self.expand_link(id, text, cx))
            .children(foot)
            .into_any_element()
    }

    /// The files a settled turn changed, under its answer: each with its directory and its
    /// `+a −r`; a click opens the session's changes at that file.
    fn changes_row(&self, prompt: &str, cx: &Context<Self>) -> AnyElement {
        let Some(thread) = self.shown_thread() else { return div().into_any_element() };
        let entries = thread.entries();
        let start =
            entries.iter().position(|e| e.id == prompt).map_or(0, |at| at.saturating_add(1));
        let end = entries
            .iter()
            .skip(start)
            .position(|e| matches!(e.body, Body::Prompt(_)))
            .map_or(entries.len(), |n| start.saturating_add(n));
        let id = &self.thread;
        let files =
            figures::files(entries.get(start..end).unwrap_or_default().iter().map(|e| (id, e)));
        let theme = &self.theme;
        let s = theme.surfaces;
        let (added, removed) = files.iter().fold((0_u32, 0_u32), |(a, r), f| {
            (a.saturating_add(f.added), r.saturating_add(f.removed))
        });
        let many = files.len() > 1;
        let touch = theme.density == slopty_theme::Density::TOUCH;
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .h(self.z(TOOL_ROW))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.icon(IconName::FilePen, s.text_muted)))
            .child(SharedString::from(format!(
                "Changed {}",
                tools::count(files.len() as u64, "file", "files")
            )))
            .when(many, |el| {
                el.child(
                    div()
                        .text_size(self.z(theme.typography.meta()))
                        .child(self.changes_label(added, removed)),
                )
            });
        let list = files.into_iter().map(|file| {
            let path = file.path.clone();
            let selector = format!("changed-{prompt}-{}", tools::file_name(&file.path));
            let dir = figures::short_dir(&file.path);
            crate::a11y::tab_stop(
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .group(CHANGED_GROUP)
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!(
                        "{}, {} lines added, {} removed",
                        tools::file_name(&file.path),
                        file.added,
                        file.removed
                    )))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .h(self.z(TOOL_ROW))
                    .pl(self.indent())
                    .pr(self.z(theme.spacing.xs))
                    .rounded(self.z(theme.radii.sm))
                    .text_size(self.z(theme.typography.small()))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .child(SharedString::from(tools::file_name(&file.path).to_owned())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(dir)),
                    )
                    // The figures follow their file; only the way into the diff sits right.
                    .child(
                        div()
                            .flex_none()
                            .text_size(self.z(theme.typography.meta()))
                            .child(self.changes_label(file.added, file.removed)),
                    )
                    .child(div().flex_1())
                    .children((!touch).then(|| {
                        div()
                            .flex_none()
                            .invisible()
                            .group_hover(CHANGED_GROUP, gpui::Styled::visible)
                            .child(self.icon(IconName::ArrowRight, s.text_muted))
                    })),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.open_changes(Some(&path), cx)))
        });
        div()
            .id(ElementId::Name(SharedString::from(format!("changes-{prompt}"))))
            .debug_selector({
                let prompt = prompt.to_owned();
                move || format!("changes-{prompt}")
            })
            .role(Role::List)
            .aria_label("Changed files")
            .flex()
            .flex_col()
            .mt(self.z(theme.spacing.xs))
            .child(head)
            .children(list)
            .into_any_element()
    }

    // ----- the session's changes -------------------------------------------------------------

    /// A file over its edits: its name at the medium weight, its directory, its `+a −r`.
    fn file_row(&self, path: &str, first: bool) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let file = self.changed_files().into_iter().find(|f| f.path == path);
        let (added, removed) = file.as_ref().map_or((0, 0), |f| (f.added, f.removed));
        let dir = figures::short_dir(path);
        div()
            .id(ElementId::Name(SharedString::from(format!("file-{path}"))))
            .debug_selector({
                let name = tools::file_name(path).to_owned();
                move || format!("file-{name}")
            })
            .role(Role::Heading)
            .aria_label(SharedString::from(tools::file_name(path).to_owned()))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .h(self.z(theme.density.row))
            .mt(self.z(if first { theme.spacing.md } else { theme.spacing.xl }))
            .text_size(self.z(theme.typography.ui_size))
            .child(self.slot().child(self.icon(
                if file.is_some_and(|f| f.created) {
                    IconName::FilePlus
                } else {
                    IconName::FilePen
                },
                s.text_muted,
            )))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(tools::file_name(path).to_owned())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(dir)),
            )
            .child(
                div()
                    .text_size(self.z(theme.typography.small()))
                    .child(self.changes_label(added, removed)),
            )
            .into_any_element()
    }

    /// One edit of the session's changes: when it was made and by whom, then its diff whole.
    fn edit_row(&self, thread: &ThreadId, id: &str, cx: &Context<Self>) -> AnyElement {
        let Some(entry) = self.model.thread(thread).and_then(|t| t.entry(id)) else {
            return div().into_any_element();
        };
        let Body::Tool(call) = &entry.body else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let title = tools::title(call, &[]);
        let who = match thread {
            ThreadId::Main => None,
            ThreadId::Agent(agent) => self.model.subagent(agent).0,
        };
        let when = figures::clock(entry.at_ms);
        let (added, _) = rows::entry_changes(entry);
        let body = match &call.detail {
            slopty_proto::conversation::ToolDetail::Edit(edit) => {
                self.patch_block(thread, &entry.id, &edit.path, &edit.patch, Level::Full, cx)
            }
            slopty_proto::conversation::ToolDetail::Write(write) => {
                self.patch_block(thread, &entry.id, &write.path, &write.patch, Level::Full, cx)
            }
            _ => None,
        };
        // A new file's words are not on the wire, only how many lines it has.
        let size = body.is_none().then(|| tools::count(u64::from(added), "line", "lines"));
        let facts: Vec<String> = [size, when, who].into_iter().flatten().collect();
        div()
            .debug_selector({
                let id = id.to_owned();
                move || format!("edit-{id}")
            })
            .flex()
            .flex_col()
            .pb(self.z(theme.spacing.sm))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .h(self.z(TOOL_ROW))
                    .pl(self.indent())
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(title.verb))
                    .child(div().flex_1())
                    .child(
                        crate::kit::tabular(div())
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(facts.join(" \u{b7} "))),
                    ),
            )
            .children(body.map(|body| div().pl(self.indent()).child(body)))
            .into_any_element()
    }

    // ----- groups and entries --------------------------------------------------------------

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
                subject: Some(meta),
                subject_quiet: true,
                code: false,
                failed: false,
                meta: None,
                took: None,
                changes: None,
                expandable: Some(open),
            },
            key,
            cx,
        )
    }

    fn entry_row(&self, id: &str, level: Level, cx: &Context<Self>) -> AnyElement {
        let Some(entry) = self.entry(id) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        match &entry.body {
            Body::Text(_) => self.answer_row(id, false, cx),
            Body::Thinking(text) => {
                let took = self
                    .shown_thread()
                    .and_then(|t| figures::thought_ms(t.entries(), t.position(id)?));
                let open = level == Level::Full;
                self.thinking_row(
                    id,
                    self.text_of(text),
                    took,
                    open,
                    false,
                    rows::entry_key(id),
                    cx,
                )
            }
            Body::Tool(call) => match &call.detail {
                slopty_proto::conversation::ToolDetail::Plan { plan } => {
                    self.plan_card(entry, call, plan, level, cx)
                }
                _ => self.tool_row(entry, call, level, cx),
            },
            Body::Compact(compact) => self.compact_row(id, compact, level, cx),
            Body::Interrupted { during_tool } => {
                let words =
                    if *during_tool { "Interrupted while a tool ran" } else { "Interrupted" };
                self.marker_line(&format!("esc-{id}"), IconName::CirclePause, words, s.text_muted)
            }
            Body::Rewound { dropped } => {
                let words = match dropped {
                    0 => "Rewound to an earlier prompt".to_owned(),
                    n => format!(
                        "Rewound \u{b7} {} set aside",
                        tools::count(u64::from(*n), "entry", "entries")
                    ),
                };
                self.rule_marker(&format!("rewound-{id}"), IconName::Undo2, &words)
            }
            Body::Note(note) => self.note_row(id, note, cx),
            Body::Prompt(prompt) => self.prompt_card(prompt, id, cx).into_any_element(),
        }
    }

    /// A quiet one-line mark in a call's slot: an interruption.
    fn marker_line(&self, id: &str, icon: IconName, words: &str, tone: Rgb) -> AnyElement {
        let theme = &self.theme;
        div()
            .id(ElementId::Name(SharedString::from(id.to_owned())))
            .role(Role::Status)
            .aria_label(SharedString::from(words.to_owned()))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .h(self.z(TOOL_ROW))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(tone))
            .child(self.slot().child(self.icon(icon, tone)))
            .child(SharedString::from(words.to_owned()))
            .into_any_element()
    }

    /// A line across the column with words in it, where the thread's history turns: a rewind.
    fn rule_marker(&self, id: &str, icon: IconName, words: &str) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let rule = || div().flex_1().h(px(1.0)).bg(hsla(s.border_subtle));
        crate::kit::tabular(div())
            .id(ElementId::Name(SharedString::from(id.to_owned())))
            .debug_selector({
                let id = id.to_owned();
                move || id
            })
            .role(Role::Status)
            .aria_label(SharedString::from(words.to_owned()))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .h(self.z(theme.density.row))
            .my(self.z(theme.spacing.md))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(rule())
            .child(self.icon(icon, s.text_muted))
            .child(SharedString::from(words.to_owned()))
            .child(rule())
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
        let rule = || div().flex_1().h(px(1.0)).bg(hsla(s.border_subtle));
        let summary = compact.summary.as_ref().filter(|_| open).map(|summary| {
            div()
                .pl(self.indent())
                .pb(self.z(theme.spacing.sm))
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
                .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key, cx))),
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
            NoteKind::Hook => (IconName::CircleAlert, s.warn),
            NoteKind::Command => (IconName::SquareTerminal, s.text_muted),
            NoteKind::Info => (IconName::Info, s.text_muted),
        };
        let retry = note.retry.map(|r| {
            let wait = crate::kit::duration(Duration::from_millis(r.in_ms));
            if r.max > 0 {
                format!("Retrying in {wait}, attempt {} of {}", r.attempt, r.max)
            } else {
                format!("Retrying in {wait}, attempt {}", r.attempt)
            }
        });
        let lead = match note.kind {
            NoteKind::Hook => Some("A hook said".to_owned()),
            _ => None,
        };
        let body = if note.kind == NoteKind::Command {
            self.code_text(&text, s.text_secondary).into_any_element()
        } else {
            div()
                .whitespace_normal()
                .text_color(hsla(s.text_secondary))
                .children(lead.map(|lead| {
                    div().text_color(hsla(s.text_muted)).child(SharedString::from(lead))
                }))
                .child(SharedString::from(text.clone()))
                .into_any_element()
        };
        let label = match &retry {
            Some(retry) => format!("{} \u{b7} {retry}", first_line(&text)),
            None => first_line(&text).to_owned(),
        };
        div()
            .id(ElementId::Name(SharedString::from(format!("note-{id}"))))
            .debug_selector({
                let id = id.to_owned();
                move || format!("note-{id}")
            })
            .role(Role::Note)
            .aria_label(SharedString::from(label))
            .py(self.z(theme.spacing.xxs))
            .flex()
            .gap(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.small()))
            .line_height(relative(theme.typography.markdown_line_height))
            .child(self.slot().child(self.icon(icon, tone)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(self.z(theme.spacing.xxs))
                    .child(body)
                    .children(retry.map(|retry| {
                        crate::kit::tabular(div())
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(retry))
                    }))
                    .children(self.expand_link(id, &note.text, cx)),
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
        let framed = body.is_some() && kind == ToolKind::Change;
        let expandable = Self::expandable(call, level);
        let mark = match title.state {
            State::Running => Mark::Running,
            State::Failed | State::Done => Mark::Icon(title.icon),
            State::Stopped => Mark::Icon(IconName::CirclePause),
        };
        // An edit's size moves into its diff's header once the diff shows.
        let changes = match (kind, &title.meta) {
            (ToolKind::Change, Some(meta)) if meta.starts_with('+') && !framed => {
                Some(rows::entry_changes(entry))
            }
            _ => None,
        };
        let meta = match (kind, title.meta) {
            (ToolKind::Change, Some(meta)) if meta.starts_with('+') => None,
            (_, meta) => meta,
        };
        let line = self.title_line(
            &format!("tool-{}", entry.id),
            TitleParts {
                mark,
                verb: title.verb,
                subject: title.subject,
                subject_quiet: false,
                code: title.code,
                failed: title.state == State::Failed,
                meta,
                took: Self::took_of(entry, call),
                changes,
                expandable: expandable.then_some(level == Level::Full),
            },
            key,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .child(line)
            .children(body.map(|body| {
                div()
                    .pl(self.indent())
                    .pt(self.z(self.theme.spacing.xxs))
                    .pb(self.z(self.theme.spacing.sm))
                    .child(body)
            }))
            .into_any_element()
    }

    /// How long a command or a subagent took, when it was long enough to say: a second or
    /// more.
    fn took_of(entry: &Entry, call: &ToolCall) -> Option<String> {
        let kind = rows::tool_kind(call);
        if !matches!(kind, ToolKind::Shell) {
            return None;
        }
        let ended = match &call.detail {
            // A background command's result comes at once; it ends on its notice.
            slopty_proto::conversation::ToolDetail::Bash(bash) if bash.background => {
                bash.finished_ms?
            }
            _ => call.result.as_ref()?.at_ms,
        };
        let took = ended.since(entry.at_ms);
        (took >= Duration::from_secs(1) && !entry.at_ms.is_zero())
            .then(|| crate::kit::duration(took))
    }

    /// Whether a click on a call's title shows more or less of it.
    fn expandable(call: &ToolCall, level: Level) -> bool {
        level == Level::Full || super::blocks::has_body(call)
    }

    /// A title line: the mark in its slot (the chevron there while the pointer is on it or it
    /// is open, when a click opens it), the verb, the subject, a ✕ when it failed, then its
    /// facts after the subject; only how long it took sits at the far right. The whole line is
    /// the button.
    pub(super) fn title_line(
        &self,
        id: &str,
        parts: TitleParts,
        key: rows::RowKey,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let failed_word = parts.failed.then(|| "failed".to_owned());
        let label = [
            Some(parts.verb.clone()),
            parts.subject.clone(),
            parts.meta.clone(),
            parts.took.clone(),
            failed_word,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        let mark = match parts.mark {
            Mark::Running => crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.text_muted),
            ),
            Mark::Icon(icon) => self.icon(icon, s.text_muted),
        };
        let slot = match parts.expandable {
            Some(open) => self.disclosure_slot(mark, open, "title"),
            None => self.slot().child(mark).into_any_element(),
        };
        let subject = parts.subject.map(|subject| {
            div()
                .min_w_0()
                .flex_shrink(1.0)
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .when(!parts.subject_quiet, |el| {
                    el.text_color(hsla(s.text)).font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                })
                .when(parts.subject_quiet, |el| el.text_color(hsla(s.text_muted)))
                .when(parts.code, |el| {
                    el.font_family(self.mono()).text_size(self.z(theme.typography.small()))
                })
                .child(SharedString::from(subject))
        });
        let failed = parts.failed.then(|| {
            crate::icons::icon(theme, IconName::X, IconSize::Inline, hsla(s.error))
                .size(self.z(theme.typography.meta()))
                .flex_none()
        });
        let quiet = |text: String| {
            crate::kit::tabular(div())
                .flex_none()
                .whitespace_nowrap()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(text))
        };
        let changes = parts.changes.map(|(added, removed)| {
            div()
                .flex_none()
                .text_size(self.z(theme.typography.meta()))
                .child(self.changes_label(added, removed))
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
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .my(self.z(theme.spacing.xxs / 2.0))
            .pr(self.z(theme.spacing.xs))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.ui_size))
            .child(slot)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(parts.verb)),
                    )
                    .children(subject)
                    .children(failed)
                    .when(changes.is_some() || parts.meta.is_some(), |el| {
                        el.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(self.z(theme.spacing.xs))
                                .pl(self.z(theme.spacing.xs))
                                .children(changes)
                                .children(parts.meta.map(quiet)),
                        )
                    })
                    .child(div().flex_1().min_w(self.z(theme.spacing.sm)))
                    .children(parts.took.map(quiet)),
            );
        match parts.expandable {
            Some(_) => crate::a11y::tab_stop(
                line.cursor_pointer().hover(move |el| el.bg(hsla(s.raised))),
                s.accent,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle(key, cx)))
            .into_any_element(),
            None => line.into_any_element(),
        }
    }

    // ----- what is live ------------------------------------------------------------------

    fn live_row(&self, id: &slopty_proto::conversation::LiveId, cx: &Context<Self>) -> AnyElement {
        let Some(block) = self.model.live_block_at(id) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let key = format!("{}-{}-{}", id.turn, id.step, id.block);
        match &block.kind {
            // What the model is writing reads a step lighter than what the transcript settled,
            // until the entry for it comes and takes its place. New words fade in, unless the
            // system asks for less motion.
            LiveKind::Text => div()
                .debug_selector({
                    let key = key.clone();
                    move || format!("live-{key}")
                })
                .id(ElementId::Name(SharedString::from(format!("live-{key}"))))
                .role(Role::Article)
                .aria_label(SharedString::from(format!("Writing: {}", first_line(&block.text))))
                .py(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.prose()))
                .line_height(relative(theme.typography.prose_line_height))
                .text_color(hsla(s.text_secondary))
                .child(
                    self.markdown_view(format!("live-md-{}-{key}", self.session), &block.text)
                        .stream_fade(crate::kit::motion(cx)),
                )
                .into_any_element(),
            LiveKind::Thinking => {
                let toggle = rows::live_key(id);
                let open =
                    (self.density == rows::Density::Verbose) != self.toggled.contains(&toggle);
                div()
                    .id(ElementId::Name(SharedString::from(format!("live-{key}"))))
                    .debug_selector(move || format!("live-{key}"))
                    .child(self.thinking_row(
                        &format!("live-{}", id.block),
                        &block.text,
                        None,
                        open,
                        true,
                        toggle,
                        cx,
                    ))
                    .into_any_element()
            }
            LiveKind::Tool { name, .. } => {
                let input = tools::preparing(&block.text);
                let (verb, subject, code) =
                    (name.clone(), (!input.is_empty()).then_some(input), true);
                div()
                    .id("live-tool")
                    .debug_selector(move || format!("live-{key}"))
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("Preparing {name}")))
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .min_h(self.z(TOOL_ROW))
                    .text_size(self.z(theme.typography.ui_size))
                    .child(self.slot().child(crate::icons::status_icon(
                        theme,
                        Status::Working,
                        self.z(theme.typography.icon()),
                        hsla(s.text_muted),
                    )))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(verb)),
                    )
                    .children(subject.map(|subject| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text_muted))
                            .when(code, |el| {
                                el.font_family(self.mono())
                                    .text_size(self.z(theme.typography.small()))
                            })
                            .child(SharedString::from(subject))
                    }))
                    .into_any_element()
            }
        }
    }

    fn working_row(&self) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let thinking = self.model.live(&ThreadId::Main).any(|(_, b)| b.kind == LiveKind::Thinking);
        let since = self.agent.as_ref().map_or(WallMs::ZERO, |a| a.since_ms);
        let elapsed = (!since.is_zero()).then(|| crate::kit::duration(WallMs::now().since(since)));
        let written = self
            .model
            .thread(&ThreadId::Main)
            .and_then(|t| t.last_turn())
            .map(|t| t.usage.output)
            .filter(|n| *n > 0)
            .map(|n| format!("{} tokens", tools::tokens(n)));
        let word = if thinking { "Thinking" } else { "Working" };
        let facts: Vec<String> = [elapsed, written].into_iter().flatten().collect();
        crate::kit::tabular(div())
            .id("working")
            .debug_selector(|| "working".to_owned())
            .role(Role::Status)
            .aria_label(word)
            .mt(self.z(theme.spacing.xs))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .text_size(self.z(theme.typography.ui_size))
            .child(self.slot().child(crate::icons::status_icon(
                theme,
                Status::Working,
                self.z(theme.typography.icon()),
                hsla(s.text_muted),
            )))
            .child(div().text_color(hsla(s.text_secondary)).child(word))
            .when(!facts.is_empty(), |el| {
                el.child(
                    div()
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(facts.join(" \u{b7} "))),
                )
            })
            .into_any_element()
    }
}

/// A fenced block's corner: its language and a copy, at the meta size.
pub(in crate::conversation) fn code_actions(
    theme: &slopty_theme::Theme,
    zoom: f32,
    block: &gpui_kit::base::text::CodeBlock,
) -> AnyElement {
    let s = theme.surfaces;
    let code = block.code().to_string();
    let lang = block.lang().filter(|l| !l.is_empty());
    div()
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs * zoom))
        .px(px(theme.spacing.xs * zoom))
        .font_family(theme.typography.ui_family.clone())
        .text_size(px(theme.typography.meta() * zoom))
        .text_color(hsla(s.text_muted))
        .children(lang.map(|lang| div().child(lang)))
        .child(
            div()
                .id("copy")
                .role(Role::Button)
                .aria_label("Copy code")
                .size(px(theme.typography.icon_large() * zoom))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.radii.xs * zoom))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(
                    crate::icons::icon(theme, IconName::Copy, IconSize::Inline, hsla(s.text_muted))
                        .size(px(theme.typography.icon() * zoom)),
                )
                .on_click(move |_ev, _window, cx| {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                }),
        )
        .into_any_element()
}

/// What a title line's slot shows.
#[derive(Clone, Copy, Debug)]
pub(super) enum Mark {
    /// The working spinner.
    Running,
    /// An icon in the muted tone.
    Icon(IconName),
}

/// The pieces of a title line.
#[derive(Clone, Debug)]
pub(super) struct TitleParts {
    pub mark: Mark,
    pub verb: String,
    pub subject: Option<String>,
    /// The subject is a summary (a group's counts), in the muted tone at the base weight.
    pub subject_quiet: bool,
    pub code: bool,
    /// It failed: a ✕ after the subject, and nothing else turns red.
    pub failed: bool,
    /// A fact about what it did, after the subject: a count, an exit code, a range.
    pub meta: Option<String>,
    /// How long it took, at the far right.
    pub took: Option<String>,
    /// Lines added and removed, after the subject.
    pub changes: Option<(u32, u32)>,
    /// `Some(open)` when a click opens or closes it.
    pub expandable: Option<bool>,
}
