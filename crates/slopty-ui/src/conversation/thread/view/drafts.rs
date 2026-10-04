//! A draft the worker holds for the person ([`Delivery::Draft`]): the account a thread carried
//! over into a new one ("Continue in…"), or a turn's message taken back to edit from there.
//!
//! It stands in the tray whole, in a field of its own, so it is read and changed where it is
//! rather than dragged into the composer: it can be long (a 32 KiB account). Send gives the
//! agent the words as they stand (a change first, then the send, in that order), Save keeps a
//! change on the worker, and Discard takes the draft back. None of it needs the agent to take a
//! message mid-turn: the worker owns a draft.

use std::collections::HashMap;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Delivery, IntentId, PendingState};

use super::ThreadView;
use crate::colors::hsla;
use crate::conversation::thread::activity::Queued;
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

/// Lines a draft's field shows before it scrolls.
const DRAFT_ROWS: usize = 12;

/// A draft's field, and the words the worker last held for it.
pub(super) struct DraftField {
    field: Entity<TextareaState>,
    held: String,
}

impl ThreadView {
    /// Give each draft the worker holds a field of its own, and let go of those that went.
    /// A field the person has not changed follows the worker's words; one they have changed
    /// keeps theirs.
    pub(super) fn settle_drafts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let held: Vec<(IntentId, String)> = self
            .state(cx)
            .map(|st| {
                st.pending
                    .iter()
                    .filter(|p| p.delivery == Delivery::Draft)
                    .filter(|p| !matches!(p.state, PendingState::Sending))
                    .map(|p| (p.intent, p.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.drafts.retain(|id, _| held.iter().any(|(h, _)| h == id));
        for (id, text) in held {
            match self.drafts.get_mut(&id) {
                Some(draft) if draft.held != text => {
                    let untouched = *draft.field.read(cx).value() == *draft.held;
                    draft.held.clone_from(&text);
                    if untouched {
                        draft.field.update(cx, |f, cx| f.set_value(text, window, cx));
                    }
                }
                Some(_) => {}
                None => {
                    let field =
                        cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, DRAFT_ROWS));
                    field.update(cx, |f, cx| f.set_value(text.clone(), window, cx));
                    cx.observe(&field, |_, _, cx| cx.notify()).detach();
                    self.drafts.insert(id, DraftField { field, held: text });
                }
            }
        }
    }

    /// The words in a draft's field now.
    fn draft_words(&self, pending: IntentId, cx: &gpui::App) -> Option<String> {
        self.drafts.get(&pending).map(|d| d.field.read(cx).value().to_string())
    }

    /// Keep the field's words on the worker, where they differ from what it holds.
    fn save_draft(&self, pending: IntentId, cx: &mut Context<Self>) {
        let Some(draft) = self.drafts.get(&pending) else { return };
        let text = draft.field.read(cx).value().to_string();
        if text != draft.held && !text.trim().is_empty() {
            let _id = self.intent(Intent::Edit { pending, text }, cx);
        }
    }

    /// Send the draft as it stands: a change goes first, then the send.
    fn send_draft(&self, pending: IntentId, cx: &mut Context<Self>) {
        if self.draft_words(pending, cx).is_some_and(|w| w.trim().is_empty()) {
            return;
        }
        self.save_draft(pending, cx);
        let _id = self.intent(Intent::Promote { pending }, cx);
    }

    /// A draft in the tray: what it is, its whole words in a field, and Discard, Save while
    /// changed, and Send.
    pub(super) fn draft_card(&self, queued: &Queued, cx: &Context<Self>) -> Option<AnyElement> {
        let draft = self.drafts.get(&queued.intent)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let pending = queued.intent;
        let words = draft.field.read(cx).value();
        let changed = *words != *draft.held;
        let empty = words.trim().is_empty();
        let busy = queued.withdrawing || queued.going;
        let refused = match &queued.edit {
            Some(crate::conversation::thread::activity::Edit::Refused { reason, .. }) => {
                Some(format!("Not changed: {reason}"))
            }
            _ => None,
        };
        let state = if queued.withdrawing { "Discarding" } else { "Draft" };
        let button = |id: String, label: &'static str, kind: ButtonKind, off: bool| {
            self.button(id, label, kind).when(off, |el| el.opacity(slopty_theme::alpha::PRESSED))
        };
        Some(
            div()
                .id(ElementId::Name(format!("draft-{pending}").into()))
                .debug_selector(move || format!("draft-{pending}"))
                .role(Role::Group)
                .aria_label("Draft")
                .w_full()
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.md))
                .py(self.z(theme.spacing.sm))
                .text_size(self.z(theme.typography.small()))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.xs))
                        .text_color(hsla(s.text_muted))
                        .child(self.icon(IconName::Pencil, s.text_muted))
                        .child(div().flex_1().child(state))
                        .child(
                            button(
                                format!("draft-discard-{pending}"),
                                "Discard",
                                ButtonKind::Ghost,
                                busy,
                            )
                            .on_click(cx.listener(
                                move |this, _ev, _w, cx| {
                                    let _id = this.intent(Intent::Withdraw { pending }, cx);
                                },
                            )),
                        )
                        .when(changed, |el| {
                            el.child(
                                button(
                                    format!("draft-save-{pending}"),
                                    "Save",
                                    ButtonKind::Secondary,
                                    busy || empty,
                                )
                                .on_click(cx.listener(
                                    move |this, _ev, _w, cx| {
                                        this.save_draft(pending, cx);
                                    },
                                )),
                            )
                        })
                        .child(
                            button(
                                format!("draft-send-{pending}"),
                                "Send",
                                ButtonKind::Primary,
                                busy || empty,
                            )
                            .on_click(cx.listener(
                                move |this, _ev, _w, cx| {
                                    this.send_draft(pending, cx);
                                },
                            )),
                        ),
                )
                .child(
                    kit::sunk(div(), theme, theme.hair())
                        .w_full()
                        .px(self.z(theme.spacing.xs))
                        .rounded(self.z(theme.radii.md))
                        .border(kit::hair(theme))
                        .border_color(hsla(s.border))
                        .bg(hsla(s.elevated))
                        .text_color(hsla(s.text))
                        .child(
                            Textarea::new(&draft.field)
                                .with_size(Size::Small)
                                .appearance(false)
                                .bordered(false)
                                .aria_label("Draft"),
                        ),
                )
                .children(
                    refused
                        .map(|why| div().text_color(hsla(s.warn)).child(SharedString::from(why))),
                )
                .into_any_element(),
        )
    }
}

/// The drafts' fields, by the intent each draft was sent under.
pub(super) type Drafts = HashMap<IntentId, DraftField>;
