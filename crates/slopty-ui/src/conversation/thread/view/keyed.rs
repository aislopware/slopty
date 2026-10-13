//! The thread's buttons on the keyboard: Resume, an action the palette lists and a key can be
//! bound to.
//!
//! The view answers an action only while its button would show, so the palette never offers a
//! line that does nothing.

use gpui::prelude::FluentBuilder as _;
use gpui::{Context, Div, InteractiveElement as _, Stateful};

use super::ThreadView;
use super::exited::Gone;
use crate::conversation::ResumeAgent;

impl ThreadView {
    /// `el` answering each of the thread's keyed buttons that would show now.
    pub(super) fn keyed(&self, el: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        el.when(self.resume_start(cx).is_some(), |el| {
            el.on_action(cx.listener(|this, _: &ResumeAgent, _w, cx| {
                if let Some(start) = this.resume_start(cx) {
                    this.resume(start, cx);
                }
            }))
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
