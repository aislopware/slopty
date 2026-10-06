//! The thread's buttons on the keyboard: Review, Take back, Compact, Branch from here and
//! Resume, each an action the palette lists and a key can be bound to.
//!
//! The view answers an action only while its button would show, so the palette never offers a
//! line that does nothing.

use gpui::prelude::FluentBuilder as _;
use gpui::{Context, Div, InteractiveElement as _, Stateful};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{ItemBody, ItemId, TurnId};

use super::exited::Gone;
use super::{ThreadView, ThreadViewEvent};
use crate::conversation::thread::activity::Activity;
use crate::conversation::thread::find;
use crate::conversation::{BranchFromHere, CompactContext, ResumeAgent, ReviewChanges, TakeBack};

impl ThreadView {
    /// `el` answering each of the thread's keyed buttons that would show now.
    pub(super) fn keyed(&self, el: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        let thread = self.thread;
        el.when(self.reviewable(cx), |el| {
            el.on_action(cx.listener(move |_this, _: &ReviewChanges, _w, cx| {
                cx.emit(ThreadViewEvent::Review { thread });
            }))
        })
        .when(self.takes_back(cx), |el| {
            el.on_action(cx.listener(|this, _: &TakeBack, _w, cx| {
                let _id = this.intent(Intent::TakeBack, cx);
            }))
        })
        .when(self.state(cx).is_some_and(super::composing::compacts), |el| {
            el.on_action(cx.listener(|this, _: &CompactContext, _w, cx| {
                let _id = this.intent(Intent::Compact, cx);
            }))
        })
        .when(self.branches(cx) && self.last_message(cx).is_some(), |el| {
            el.on_action(cx.listener(|this, _: &BranchFromHere, _w, cx| this.branch_from_last(cx)))
        })
        .when(self.resume_start(cx).is_some(), |el| {
            el.on_action(cx.listener(|this, _: &ResumeAgent, _w, cx| {
                if let Some(start) = this.resume_start(cx) {
                    this.resume(start, cx);
                }
            }))
        })
    }

    /// The last turn changed files, so the tray offers "Review".
    fn reviewable(&self, cx: &Context<Self>) -> bool {
        self.state(cx).is_some_and(|state| {
            !Activity::of(self.hub.read(cx).threads(), self.thread, state).edited.is_empty()
        })
    }

    /// The agent's own TUI holds the session and no take-back is on its way.
    fn takes_back(&self, cx: &Context<Self>) -> bool {
        self.tui_holds(cx) == Some(true) && !self.moving(cx, &Intent::TakeBack)
    }

    /// How the exited agent is taken up again, while the exited line offers "Resume".
    fn resume_start(&self, cx: &Context<Self>) -> Option<slopty_proto::thread::wire::Start> {
        match self.gone(cx)? {
            Gone::Resume(start) if !self.hub.read(cx).resuming(self.thread) => Some(*start),
            _ => None,
        }
    }

    /// The person's last message in the thread, and its turn.
    fn last_message(&self, cx: &Context<Self>) -> Option<(ItemId, TurnId)> {
        let state = self.state(cx)?;
        state
            .items
            .iter()
            .rev()
            .find(|i| matches!(i.body, ItemBody::User(_)))
            .map(|i| (i.id.clone(), i.turn))
    }

    /// "Branch from here" under the person's last message, brought into view.
    fn branch_from_last(&mut self, cx: &mut Context<Self>) {
        let Some((item, turn)) = self.last_message(cx) else { return };
        self.toggle_branch(&item, turn, cx);
        let at = self.state(cx).and_then(|st| st.items.iter().position(|i| i.id == item));
        if let Some(ix) = at.and_then(|at| find::row_of(&self.rows, &self.spans, at)) {
            self.list.scroll_to_reveal_item(ix);
        }
        cx.notify();
    }
}
