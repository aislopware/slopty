//! "Edit…": an edit or a written file allowed as the person changed it.
//!
//! Where the request offers parts of its call to change ([`Request::editable`]: an Edit's new
//! text, a Write's content), "Edit…" stands after the plain allow. It puts each part in a field
//! of its own, in the code face, in place of the answers; "Allow edited" sends the plain allow's
//! answer as [`Editable::choice`] with the fields as they are now, and Claude Code runs the call
//! with them, checked against its rules again. Cancel brings the answers back.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AskId, Editable, Effect, Request};

use super::ThreadView;
use crate::colors::hsla;
use crate::kit::ButtonKind;

/// The most rows a field grows to before it scrolls.
const ROWS: usize = 16;

/// A request whose call is being changed before it is allowed.
pub(super) struct Editing {
    ask: AskId,
    fields: Vec<(String, Entity<TextareaState>)>,
}

/// Whether `request` can be allowed as changed: it offers parts of its call to change and a
/// plain allow to send them with.
#[must_use]
pub fn editable(request: &Request) -> bool {
    !request.editable.is_empty()
        && request.kind == Request::APPROVAL
        && request.options.iter().any(|c| c.effect == Effect::Allow && c.scope.is_none())
}

/// The words of the field for `field`, named as the call names it.
fn field_label(field: &str) -> String {
    match field {
        "new_string" => "The new text".to_owned(),
        "content" => "The file's content".to_owned(),
        other => format!("The {}", other.replace('_', " ")),
    }
}

impl ThreadView {
    /// The "Edit…" button of `request`, when its call can be changed.
    pub(super) fn edit_button(&self, request: &Request, cx: &Context<Self>) -> Option<AnyElement> {
        if !editable(request) {
            return None;
        }
        let ask = request.id.clone();
        let parts = request.editable.clone();
        Some(
            self.button(format!("edit-{}", ask.0), "Edit\u{2026}", ButtonKind::Ghost)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.start_call_edit(ask.clone(), &parts, window, cx);
                }))
                .into_any_element(),
        )
    }

    /// Change `ask`'s call: each of `parts` in a field, the first taking the keyboard.
    pub(super) fn start_call_edit(
        &mut self,
        ask: AskId,
        parts: &[Editable],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let fields: Vec<(String, Entity<TextareaState>)> = parts
            .iter()
            .map(|part| {
                let state = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, ROWS));
                state.update(cx, |s, cx| s.set_value(part.text.clone(), window, cx));
                (part.field.clone(), state)
            })
            .collect();
        if let Some((_, first)) = fields.first() {
            first.update(cx, |f, cx| f.focus(window, cx));
        }
        self.denying = None;
        self.editing = Some(Editing { ask, fields });
        cx.notify();
    }

    /// Allow the call with the fields as they are now.
    pub(super) fn allow_edited(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else { return };
        let fields = editing
            .fields
            .iter()
            .map(|(field, state)| (field.clone(), state.read(cx).value().to_string()))
            .collect();
        let choice = Editable::choice(&fields);
        let _id = self.intent(Intent::Answer { ask: editing.ask, choice, message: None }, cx);
        cx.notify();
    }

    /// Put the fields away: the request's answers come back.
    fn cancel_call_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// In place of `request`'s answers while its call is changed: a field per part, then
    /// Cancel and "Allow edited".
    pub(super) fn edit_row(&self, request: &Request, cx: &Context<Self>) -> Option<AnyElement> {
        let editing = self.editing.as_ref().filter(|e| e.ask == request.id)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = request.id.0.clone();
        let fields = editing.fields.iter().map(|(field, state)| {
            let label = SharedString::from(field_label(field));
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xxs))
                .child(
                    div()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .rounded(self.z(theme.radii.sm))
                        .bg(hsla(s.panel))
                        .font_family(self.mono())
                        .text_color(hsla(s.text))
                        .child(
                            Textarea::new(state)
                                .with_size(Size::Small)
                                .text_size(self.z(theme.typography.small()))
                                .aria_label(label),
                        ),
                )
        });
        Some(
            div()
                .id("thread-edit-call")
                .debug_selector(move || format!("edit-call-{id}"))
                .role(Role::Group)
                .aria_label("Allow as edited")
                .w_full()
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xs))
                .children(fields)
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap(self.z(theme.spacing.xxs))
                        .child(self.button("edit-cancel", "Cancel", ButtonKind::Ghost).on_click(
                            cx.listener(|this, _ev, window, cx| this.cancel_call_edit(window, cx)),
                        ))
                        .child(
                            self.button("edit-allow", "Allow edited", ButtonKind::Primary)
                                .on_click(cx.listener(|this, _ev, _w, cx| this.allow_edited(cx))),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, Choice, Editable, Effect, Request, RequestState};

    use super::editable;

    fn choice(id: &str, effect: Effect, scope: Option<&str>) -> Choice {
        Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: scope.map(str::to_owned),
            stops: false,
        }
    }

    /// An approval that offers parts of its call and a plain allow can be allowed as edited;
    /// one that offers no part, only a standing grant, or that is a question cannot.
    #[test]
    fn only_an_approval_with_parts_and_a_plain_allow_is_editable() {
        let part = Editable { field: "new_string".to_owned(), text: "b".to_owned() };
        let request = |kind: &str, options, editable: Vec<Editable>| Request {
            editable,
            id: AskId("a".to_owned()),
            item: None,
            kind: kind.to_owned(),
            title: "Allow Edit?".to_owned(),
            text: None,
            options,
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: slopty_core::WallMs::ZERO,
            until_ms: None,
        };
        let allow =
            || vec![choice("allow", Effect::Allow, None), choice("deny", Effect::Deny, None)];
        assert!(editable(&request(Request::APPROVAL, allow(), vec![part.clone()])));
        assert!(!editable(&request(Request::APPROVAL, allow(), Vec::new())));
        let standing = vec![choice("always", Effect::Allow, Some("Edit in src/"))];
        assert!(!editable(&request(Request::APPROVAL, standing, vec![part.clone()])));
        assert!(!editable(&request(Request::QUESTION, allow(), vec![part])));
    }
}
