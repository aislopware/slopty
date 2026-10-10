//! The thread's buttons on the keyboard: Review and Resume, each an action the palette lists
//! and a key can be bound to.
//!
//! The view answers an action only while its button would show, so the palette never offers a
//! line that does nothing.

use gpui::prelude::FluentBuilder as _;
use gpui::{Context, Div, InteractiveElement as _, Stateful};

use super::exited::Gone;
use super::{ThreadView, ThreadViewEvent};
use crate::conversation::thread::activity::Activity;
use crate::conversation::{ResumeAgent, ReviewChanges};

impl ThreadView {
    /// `el` answering each of the thread's keyed buttons that would show now.
    pub(super) fn keyed(&self, el: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        let thread = self.thread;
        el.when(self.reviewable(cx), |el| {
            el.on_action(cx.listener(move |_this, _: &ReviewChanges, _w, cx| {
                cx.emit(ThreadViewEvent::Review { thread });
            }))
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

    /// How the exited agent is taken up again, while the exited line offers "Resume".
    fn resume_start(&self, cx: &Context<Self>) -> Option<slopty_proto::thread::wire::Start> {
        match self.gone(cx)? {
            Gone::Resume(start) if !self.hub.read(cx).resuming(self.thread) => Some(*start),
            _ => None,
        }
    }
}
