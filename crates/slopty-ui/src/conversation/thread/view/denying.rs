//! "Deny with a reason…": a request turned down with the person's reason, in their words, for
//! the agent to read. It is the first of the other ways to deny, behind the deny's chevron
//! (`decision`).
//!
//! The request's answers give way to a field and two buttons while the reason is written. The
//! field keeps several lines, pasted or broken by ⇧↵. ↵ or Deny sends the plain deny the agent
//! offers ([`deny_choice`]) with the words as the answer's message; with nothing written it is
//! the plain deny. Each worker carries the words through
//! the agent's own door: Claude Code's and pi's denials take a reason, and the others hear it
//! as the person's next message.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{AskId, Choice, Request};

use super::ThreadView;
use crate::colors::hsla;
use crate::kit::ButtonKind;

/// What the reason's field says before anything is written.
pub const PLACEHOLDER: &str = "Why, for the agent (optional)";

/// The most lines the reason grows to before it scrolls.
const REASON_ROWS: usize = 6;

/// A request being denied with a reason.
pub(super) struct Denying {
    ask: AskId,
    choice: String,
    field: Entity<TextareaState>,
    _entered: Subscription,
}

/// The deny `request` offers that a reason goes with: its plain one, else the first. Only an
/// approval's: a question is answered in words already.
#[must_use]
pub fn deny_choice(request: &Request) -> Option<&Choice> {
    if !request.questions.is_empty() || request.kind == Request::QUESTION {
        return None;
    }
    plain_deny(&request.options)
}

pub use slopty_proto::thread::plain_deny;

impl ThreadView {
    /// Write the reason `ask` is denied with `choice`: the field takes the keyboard.
    pub(super) fn start_deny(
        &mut self,
        ask: AskId,
        choice: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(PLACEHOLDER)
                .auto_grow(1, REASON_ROWS)
                .submit_on_enter(true)
        });
        let entered = cx.subscribe(&field, |this, _field, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.deny(cx);
            }
        });
        field.update(cx, |f, cx| f.focus(window, cx));
        self.denying = Some(Denying { ask, choice, field, _entered: entered });
        cx.notify();
    }

    /// Deny with what is written, or plainly with nothing.
    pub(super) fn deny(&mut self, cx: &mut Context<Self>) {
        let Some(denying) = self.denying.take() else { return };
        let reason = denying.field.read(cx).value().trim().to_owned();
        let message = (!reason.is_empty()).then_some(reason);
        let intent = Intent::Answer { ask: denying.ask, choice: denying.choice, message };
        let _id = self.intent(intent, cx);
        cx.notify();
    }

    /// Put the reason away: the request's answers come back.
    fn cancel_deny(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.denying.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// In place of `request`'s answers while its reason is written: the field, Cancel and Deny.
    pub(super) fn deny_row(&self, request: &Request, cx: &Context<Self>) -> Option<AnyElement> {
        let denying = self.denying.as_ref().filter(|d| d.ask == request.id)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = request.id.0.clone();
        Some(
            div()
                .id("thread-deny-reason")
                .debug_selector(move || format!("deny-reason-{id}"))
                .role(Role::Group)
                .aria_label("Deny with a reason")
                .w_full()
                .flex()
                .items_end()
                .gap(self.z(theme.spacing.xs))
                .child(
                    div().min_w_0().flex_1().text_color(hsla(s.text)).child(
                        Textarea::new(&denying.field)
                            .with_size(Size::Small)
                            .text_size(self.z(theme.typography.small()))
                            .aria_label("Why"),
                    ),
                )
                .child(
                    self.button("deny-why-cancel", "Cancel", ButtonKind::Ghost).on_click(
                        cx.listener(|this, _ev, window, cx| this.cancel_deny(window, cx)),
                    ),
                )
                .child(
                    self.button("deny-why-send", "Deny", ButtonKind::Secondary)
                        .on_click(cx.listener(|this, _ev, _w, cx| this.deny(cx))),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::{AskId, Choice, Effect, Request, RequestState};

    use super::deny_choice;

    fn choice(id: &str, effect: Effect, scope: Option<&str>, stops: bool) -> Choice {
        Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: scope.map(str::to_owned),
            stops,
        }
    }

    fn request(kind: &str, options: Vec<Choice>) -> Request {
        Request {
            id: AskId("a".to_owned()),
            item: None,
            kind: kind.to_owned(),
            title: "Run it?".to_owned(),
            text: None,
            options,
            questions: Vec::new(),
            proposed: None,
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: slopty_core::WallMs::ZERO,
            until_ms: None,
        }
    }

    /// The reason goes with the plain deny, not with one that stops the turn or reaches
    /// further; with only those, the first; and an approval with no deny, or a question, offers
    /// no "Deny…".
    #[test]
    fn the_reason_goes_with_the_plain_deny() {
        let codex = request(
            Request::APPROVAL,
            vec![
                choice("accept", Effect::Allow, None, false),
                choice("cancel", Effect::Deny, None, true),
                choice("decline", Effect::Deny, None, false),
            ],
        );
        assert_eq!(deny_choice(&codex).map(|c| c.id.as_str()), Some("decline"));
        let stops_only =
            request(Request::APPROVAL, vec![choice("deny-stop", Effect::Deny, None, true)]);
        assert_eq!(deny_choice(&stops_only).map(|c| c.id.as_str()), Some("deny-stop"));
        let allow_only = request(Request::APPROVAL, vec![choice("ok", Effect::Allow, None, false)]);
        assert_eq!(deny_choice(&allow_only), None);
        let question = request(Request::QUESTION, vec![choice("no", Effect::Deny, None, false)]);
        assert_eq!(deny_choice(&question), None);
    }
}
