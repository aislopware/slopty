//! Reviewing what the agent changed from the face: lines of a diff picked by a press and a
//! drag over their numbers are quoted into the draft, and the Changes pane shows either the
//! turn in progress or the whole session.
//!
//! The quote is text in the draft, so it reaches Claude Code as the rest of the message does:
//! typed into the same PTY, nothing sent behind the TUI's back.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use slopty_proto::conversation::{Body, ThreadId};

use super::{ConversationView, Pane};
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::figures::{self, FileChange};
use crate::kit::{self, ButtonKind};

/// What the Changes pane covers.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    /// What changed since the last prompt, across the session's threads.
    Turn,
    /// Everything the session changed.
    #[default]
    Session,
}

/// Rows of one diff picked to quote: which diff, how it was laid out, and the first and last
/// row picked (in the order the pointer went), while the pointer still drags or after.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Quote {
    pub thread: ThreadId,
    pub id: String,
    pub split: bool,
    pub from: usize,
    pub to: usize,
    pub dragging: bool,
}

impl Quote {
    /// The rows picked, first to last.
    pub fn range(&self) -> (usize, usize) {
        (self.from.min(self.to), self.from.max(self.to))
    }
}

impl ConversationView {
    /// What the Changes pane covers.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Show the Changes pane's `scope`.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            if self.pane == Pane::Changes {
                self.rebuild(cx);
            }
            cx.notify();
        }
    }

    /// Every file the Changes pane shows: the session's, or those changed since the last
    /// prompt, in the order each was first changed.
    #[must_use]
    pub fn changed_files(&self) -> Vec<FileChange> {
        let since = match self.scope {
            Scope::Session => None,
            Scope::Turn => self.model.thread(&ThreadId::Main).and_then(|t| {
                t.entries()
                    .iter()
                    .rev()
                    .find(|e| matches!(e.body, Body::Prompt(_)))
                    .map(|e| e.at_ms)
            }),
        };
        let mut entries: Vec<(&ThreadId, &slopty_proto::conversation::Entry)> = self
            .model
            .threads()
            .flat_map(|(id, t)| t.entries().iter().map(move |e| (id, e)))
            .filter(|(_, e)| since.is_none_or(|at| e.at_ms >= at))
            .collect();
        entries.sort_by_key(|(_, e)| e.at_ms);
        figures::files(entries)
    }

    /// The rows of diff `id` in `thread` picked to quote, first to last, when they are its.
    pub(super) fn quoted_rows(
        &self,
        thread: &ThreadId,
        id: &str,
        split: bool,
    ) -> Option<(usize, usize)> {
        self.quote
            .as_ref()
            .filter(|q| q.thread == *thread && q.id == id && q.split == split)
            .map(Quote::range)
    }

    /// Whether the pointer still drags over a diff's numbers.
    pub(super) fn quote_dragging(&self) -> bool {
        self.quote.as_ref().is_some_and(|q| q.dragging)
    }

    /// The pointer went down on row `ix` of diff `id`'s numbers: it is picked, and a drag picks
    /// on from it.
    fn start_quote(
        &mut self,
        thread: ThreadId,
        id: String,
        split: bool,
        ix: usize,
        cx: &mut Context<Self>,
    ) {
        self.quote = Some(Quote { thread, id, split, from: ix, to: ix, dragging: true });
        cx.notify();
    }

    /// The pointer dragged onto row `ix` of the diff being picked in.
    fn extend_quote(&mut self, thread: &ThreadId, id: &str, ix: usize, cx: &mut Context<Self>) {
        if let Some(quote) = &mut self.quote
            && quote.dragging
            && quote.thread == *thread
            && quote.id == id
            && quote.to != ix
        {
            quote.to = ix;
            cx.notify();
        }
    }

    /// The pointer let go: what it picked stays picked, with the way to quote it.
    pub(super) fn end_quote_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(quote) = &mut self.quote
            && quote.dragging
        {
            quote.dragging = false;
            cx.notify();
        }
    }

    /// Nothing picked any more. Whether something was.
    pub(super) fn clear_quote(&mut self, cx: &mut Context<Self>) -> bool {
        let had = self.quote.take().is_some();
        if had {
            cx.notify();
        }
        had
    }

    /// Put `text` at the end of the draft, a blank line after what is there, and give the
    /// composer the keyboard with the conversation on show.
    pub(super) fn quote_into_draft(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quote = None;
        self.close_changes(cx);
        let draft = self.draft(cx);
        let joined = match draft.trim_end() {
            "" => text.to_owned(),
            before => format!("{before}\n\n{text}"),
        };
        self.composer.update(cx, |c, cx| {
            c.set_value(joined, window, cx);
            c.focus(window, cx);
        });
        cx.notify();
    }

    /// The numbers of row `ix` of diff `id`: where a press starts picking lines to quote and a
    /// drag goes on picking.
    pub(super) fn quote_gutter(
        &self,
        thread: &ThreadId,
        id: &str,
        split: bool,
        ix: usize,
        cx: &Context<Self>,
    ) -> gpui::Stateful<Div> {
        let (down_thread, down_id) = (thread.clone(), id.to_owned());
        let (move_thread, move_id) = (thread.clone(), id.to_owned());
        div()
            .id(ElementId::Name(SharedString::from(format!("quote-{id}-{ix}"))))
            .debug_selector({
                let id = id.to_owned();
                move || format!("quote-{id}-{ix}")
            })
            .flex_none()
            .flex()
            .gap(self.z(self.theme.spacing.sm))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    this.start_quote(down_thread.clone(), down_id.clone(), split, ix, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _w, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    this.extend_quote(&move_thread, &move_id, ix, cx);
                }
            }))
    }

    /// The way to quote what is picked, floating at the right of its last row.
    pub(super) fn quote_button(&self, text: String, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let button = kit::button(theme, "quote-diff", "Quote in message", ButtonKind::Ghost)
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |this, _ev, window, cx| this.quote_into_draft(&text, window, cx)),
            );
        div()
            .absolute()
            .top_0()
            .right(self.z(theme.spacing.sm))
            .child(kit::elevate(button, theme).rounded(self.z(theme.radii.sm)))
            .into_any_element()
    }

    /// The wash over a picked row.
    pub(super) fn quote_wash(&self) -> gpui::Hsla {
        hsla_alpha(self.theme.surfaces.accent, slopty_theme::alpha::FAINT)
    }

    /// The Changes pane's two scopes, side by side, the one on show on the selected fill.
    pub(super) fn scope_switch(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let segment = |scope: Scope, id: &'static str, label: &'static str| {
            let chosen = self.scope == scope;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if chosen {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
                .h_full()
                .flex()
                .items_center()
                .px(self.z(theme.spacing.sm))
                .rounded(self.z(theme.radii.xs))
                .cursor_pointer()
                .map(|el| {
                    if chosen {
                        el.bg(hsla(s.overlay))
                            .text_color(hsla(s.text))
                            .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                    } else {
                        el.text_color(hsla(s.text_secondary))
                            .hover(move |el| el.text_color(hsla(s.text)))
                    }
                })
                .on_click(cx.listener(move |this, _ev, _w, cx| this.set_scope(scope, cx)))
                .child(label)
        };
        div()
            .id("changes-scope")
            .debug_selector(|| "changes-scope".to_owned())
            .role(Role::RadioGroup)
            .aria_label("Changes of")
            .flex_none()
            .flex()
            .gap(self.z(theme.spacing.xxs))
            .h(self.z((-2.0_f32).mul_add(theme.spacing.xs, theme.density.row)))
            .child(crate::a11y::tab_stop(segment(Scope::Turn, "scope-turn", "This turn"), s.accent))
            .child(crate::a11y::tab_stop(
                segment(Scope::Session, "scope-session", "Session"),
                s.accent,
            ))
            .into_any_element()
    }
}
