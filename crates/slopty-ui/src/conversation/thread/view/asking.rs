//! The questions of the request on show, kept as one questionnaire while it shows.

use gpui::{App, Context, Subscription, Window};
use gpui_kit::component::questionnaire::{QuestionnaireEvent, QuestionnaireSubmission};
use slopty_proto::thread::{AskId, Request};

use super::ThreadView;
use crate::conversation::thread::activity::Activity;
use crate::conversation::thread::questions::{self, Questions};

/// The request on show that asks questions, and its questionnaire.
pub(super) struct Asking {
    ask: AskId,
    questions: Questions,
    _answered: Subscription,
}

impl Asking {
    /// The request.
    pub(super) const fn ask(&self) -> &AskId {
        &self.ask
    }

    /// Its questionnaire.
    pub(super) const fn questions(&self) -> &Questions {
        &self.questions
    }
}

impl ThreadView {
    /// The request the bar shows, of those still waiting on the person.
    pub(super) fn shown_request(&self, cx: &App) -> Option<Request> {
        let state = self.state(cx)?;
        let bar = Activity::of(self.hub.read(cx).threads(), self.thread, state);
        let waiting: Vec<_> = bar.waiting().collect();
        let at = self.asked_at.min(waiting.len().saturating_sub(1));
        waiting.get(at).map(|asked| asked.request.clone())
    }

    /// Before a frame: the questionnaire follows the request on show. A new one takes the
    /// keyboard from an empty composer, so a message being typed is never read as answers;
    /// one that goes while it holds the keyboard hands it back to the composer.
    pub(super) fn settle_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown_request(cx).filter(|r| !r.questions.is_empty());
        if self.asking.as_ref().map(|a| &a.ask) == shown.as_ref().map(|r| &r.id) {
            return;
        }
        let held = self.asking.as_ref().is_some_and(|a| a.questions.focused(window, cx));
        let free = self.composer_focused(window, cx) && self.draft(cx).trim().is_empty();
        self.asking = shown.map(|request| {
            let questions = Questions::new(request.questions, window, cx);
            let ask = request.id;
            let answered =
                cx.subscribe(questions.state(), |this, _state, event: &QuestionnaireEvent, cx| {
                    if let QuestionnaireEvent::Submit(submission) = event {
                        this.answer_questions(submission, cx);
                    }
                });
            Asking { ask, questions, _answered: answered }
        });
        match &self.asking {
            Some(asking) if held || free => asking.questions.focus(window, cx),
            None if held => self.focus(window, cx),
            _ => {}
        }
    }

    /// The questionnaire was submitted: its answers go as one answer to the request.
    fn answer_questions(&self, submission: &QuestionnaireSubmission, cx: &mut Context<Self>) {
        let Some(asking) = &self.asking else { return };
        let asked = asking.questions.questions();
        let choice = questions::choice(asked, &questions::answers(asked, submission));
        self.answer(asking.ask.clone(), choice, cx);
    }
}

#[cfg(test)]
impl ThreadView {
    /// The questionnaire on show.
    pub(in crate::conversation::thread) fn questionnaire(
        &self,
    ) -> Option<gpui::Entity<gpui_kit::component::questionnaire::QuestionnaireState>> {
        self.asking.as_ref().map(|a| a.questions.state().clone())
    }
}
