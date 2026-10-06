//! An agent's questions answered in the GUI: Claude Code's `AskUserQuestion`, a Codex
//! question, a pi dialog that asks for words, any request of the thread model that carries
//! questions.
//!
//! They are gpui-kit's questionnaire: one question on show at a time with its header, its
//! answers and what each means, a field for an answer of one's own under them ("Other"), and
//! a question that offers nothing is that field alone. The keyboard walks it as the
//! questionnaire's contract says: ↑/↓ move between answers, a digit takes its answer, ↵ goes
//! on with a filled one, ⌘↵ goes on from anywhere, ←/→ step between questions.
//!
//! One [`Intent::Answer`](slopty_proto::thread::wire::Intent::Answer) answers them all
//! ([`Answer::choice`]). A multi-choice answer joins its picks ([`Answer::JOIN`]) and puts the
//! words of one's own last, as Claude Code's own dialog does.

use std::cell::Cell;

use gpui::{
    AnyElement, App, AppContext as _, Entity, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use gpui_kit::component::input::TextareaState;
use gpui_kit::component::questionnaire::{
    Questionnaire, QuestionnaireActions, QuestionnaireAnswer, QuestionnaireChoice,
    QuestionnaireChoiceDefinition, QuestionnaireChoices, QuestionnaireError, QuestionnaireInput,
    QuestionnaireInputDefinition, QuestionnaireItem, QuestionnaireItemDefinition,
    QuestionnaireNext, QuestionnairePrevious, QuestionnaireShortcutMode, QuestionnaireState,
    QuestionnaireSubmission, QuestionnaireSubmit, QuestionnaireTitle,
};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::detail::{Answer, Question};
use slopty_theme::Theme;

/// What a question with answers calls the field for one of one's own.
pub const OTHER: &str = "Other";

/// What the field of a question that offers nothing says before anything is typed.
pub const WRITTEN: &str = "Your answer";

/// An answer to `questions` in words, for the line that stands for the card once answered:
/// the answers in their order ([`Answer::read`]), or the choice itself when it holds none.
#[must_use]
pub fn words(questions: &[Question], choice: &str) -> String {
    Answer::read(questions, choice).map_or_else(
        || choice.to_owned(),
        |answers| answers.into_iter().map(|a| a.answer).collect::<Vec<_>>().join("; "),
    )
}

/// The answer to each of `questions` that `submission` holds, by the questionnaire's names
/// ([`Questions::new`]): the labels picked, then the words typed.
#[must_use]
pub fn answers(questions: &[Question], submission: &QuestionnaireSubmission) -> Vec<Answer> {
    questions
        .iter()
        .enumerate()
        .map(|(ix, question)| {
            let given = submission.answer(&ix.to_string());
            let picked =
                given.into_iter().flat_map(QuestionnaireAnswer::choices).filter_map(|value| {
                    let at: usize = value.parse().ok()?;
                    question.options.get(at).map(|o| o.label.as_str())
                });
            let typed = given.and_then(|a| a.freeform()).map(|w| w.trim());
            let answer: Vec<&str> = picked.chain(typed).filter(|w| !w.is_empty()).collect();
            Answer { question: question.text.clone(), answer: answer.join(Answer::JOIN) }
        })
        .collect()
}

/// Questions being answered: their questionnaire, one item per question named by its place,
/// one choice per answer named by its place, so two that read the same stay two.
pub struct Questions {
    asked: Vec<Question>,
    state: Entity<QuestionnaireState>,
    /// The scroll of the question on show, above the answers' row: where the room is short,
    /// it scrolls and the row stays.
    body: ScrollHandle,
    /// Whether the field had the keyboard, and the window's height, as last drawn: the field
    /// is brought into view when either changes while it has the keyboard.
    field: Cell<(bool, Pixels)>,
}

impl std::fmt::Debug for Questions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Questions").field("questions", &self.asked.len()).finish_non_exhaustive()
    }
}

impl Questions {
    /// `questions`, none answered, the first on show.
    pub fn new(questions: Vec<Question>, window: &mut Window, cx: &mut App) -> Self {
        let items: Vec<QuestionnaireItemDefinition> = questions
            .iter()
            .enumerate()
            .map(|(ix, question)| {
                let written = question.options.is_empty();
                let (placeholder, label) =
                    if written { (WRITTEN, "Answer") } else { (OTHER, OTHER) };
                let field = cx.new(|cx| {
                    TextareaState::new(window, cx).auto_grow(1, 6).placeholder(placeholder)
                });
                QuestionnaireItemDefinition::new(ix.to_string(), question.text.clone())
                    .with_required(true)
                    .with_multiple(question.multi_select)
                    .with_choices(question.options.iter().enumerate().map(|(at, offered)| {
                        let choice = QuestionnaireChoiceDefinition::new(
                            at.to_string(),
                            offered.label.clone(),
                        );
                        match &offered.description {
                            Some(means) => choice.with_description(means.clone()),
                            None => choice,
                        }
                    }))
                    .with_input(QuestionnaireInputDefinition::new(field, label))
            })
            .collect();
        let state = cx.new(|cx| {
            let mut items = Some(items);
            // The names are places, so no two are alike, and no answer is taken beforehand:
            // the schema holds. Were it ever refused, the empty one, which always holds,
            // stands in rather than a panic.
            loop {
                match QuestionnaireState::new(items.take().unwrap_or_default(), cx) {
                    Ok(state) => break state.with_shortcuts(QuestionnaireShortcutMode::Numbers),
                    Err(e) => tracing::warn!(error = %e, "questions refused by the questionnaire"),
                }
            }
        });
        Self { asked: questions, state, body: ScrollHandle::new(), field: Cell::default() }
    }

    /// The questions.
    #[must_use]
    pub fn questions(&self) -> &[Question] {
        &self.asked
    }

    /// Their questionnaire.
    #[must_use]
    pub const fn state(&self) -> &Entity<QuestionnaireState> {
        &self.state
    }

    /// The question on show.
    #[must_use]
    pub fn current<'a>(&'a self, cx: &App) -> Option<&'a Question> {
        let name = self.state.read(cx).current_item()?;
        self.asked.get(name.parse::<usize>().ok()?)
    }

    /// Whether the keyboard is somewhere in the questionnaire.
    #[must_use]
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle().contains_focused(window, cx)
    }

    /// Before a frame: the field for an answer of one's own, once it takes the keyboard, or
    /// once the window's height changes while it has it (the soft keyboard rising), shows
    /// above the answers' row. The field ends the question, so the question's scroll goes to
    /// its end; any other time, the scroll is the person's.
    pub fn keep_field_in_view(&self, window: &Window, cx: &App) {
        let typing = self.state.read(cx).is_current_input_focused(window);
        let now = (typing, window.viewport_size().height);
        if typing && self.field.get() != now {
            self.body.scroll_to_bottom();
        }
        self.field.set(now);
    }

    /// Give the question on show the keyboard: its first answer, or its field when it offers
    /// none.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let state = self.state.read(cx);
        let Some(item) = state.current_item().cloned() else { return };
        let first = state
            .item_definition(&item)
            .and_then(|d| d.choices().first())
            .map(|c| c.value().clone());
        let _focused = self.state.update(cx, |state, cx| match first {
            Some(value) => state.focus_choice(&item, &value, window, cx),
            None => state.focus_input(&item, window, cx),
        });
    }

    /// The questionnaire drawn: the question on show, its answers, the field, what is
    /// missing, and the ways on, with `lead` (the request's own answers) first in their row.
    /// The question's header is the card's to show ([`Questions::current`]).
    pub fn element(&self, theme: &Theme, lead: Vec<AnyElement>) -> AnyElement {
        let state = &self.state;
        let spacing = theme.spacing;
        // The field stands at least as tall as an answer's card at this size (the kit's own
        // measure: 32 + 4), so one's own answer reads as one more answer, not a footnote, and
        // grows with the lines written in it.
        let field = gpui::px(spacing.xxl + spacing.xs);
        let items = self.asked.iter().enumerate().map(|(ix, question)| {
            let name = SharedString::from(ix.to_string());
            let answers = (!question.options.is_empty()).then(|| {
                QuestionnaireChoices::new(state, name.clone()).children(
                    (0..question.options.len())
                        .map(|at| QuestionnaireChoice::new(state, name.clone(), at.to_string())),
                )
            });
            QuestionnaireItem::new(state, name.clone())
                .child(QuestionnaireTitle::new(state, name.clone()))
                .children(answers)
                .child(crate::kit::field(
                    QuestionnaireInput::new(state, name.clone()).min_h(field),
                    theme,
                ))
                .child(QuestionnaireError::new(state, name))
        });
        // The question scrolls where the room is short; the row of answers under it is where
        // the person commits, so it never scrolls out of view (`docs/decisions/ui.md`).
        Questionnaire::new(state)
            .with_size(Size::Small)
            .min_h_0()
            .child(
                div()
                    .id("thread-question")
                    .debug_selector(|| "thread-question".to_owned())
                    .track_scroll(&self.body)
                    .w_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .children(items),
            )
            .child(
                QuestionnaireActions::new(state)
                    .flex_none()
                    .flex_wrap()
                    .pt(gpui::px(spacing.xs))
                    // The ways out of the form stand apart at the left, quiet; the way through
                    // it at the right.
                    .child(div().flex().flex_wrap().gap(gpui::px(spacing.xxs)).children(lead))
                    .child(div().flex_1())
                    .child(QuestionnairePrevious::new(state))
                    .child(QuestionnaireNext::new(state))
                    .child(QuestionnaireSubmit::new(state)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::detail::Offered;

    use super::*;

    fn question(text: &str, options: &[&str], multi_select: bool) -> Question {
        Question {
            text: text.to_owned(),
            header: None,
            options: options
                .iter()
                .map(|label| Offered { label: (*label).to_owned(), description: None })
                .collect(),
            multi_select,
        }
    }

    fn answer(question: &str, answer: &str) -> Answer {
        Answer { question: question.to_owned(), answer: answer.to_owned() }
    }

    /// The answered line reads the answers, not their JSON; a plain choice reads as itself.
    #[test]
    fn the_words_of_an_answer_read_its_answers() {
        let asked = [question("A?", &["x"], false), question("B?", &["y", "z"], true)];
        let sent = Answer::choice(&asked, &[answer("A?", "x"), answer("B?", "y, z")]);
        assert_eq!(words(&asked, &sent), "x; y, z");
        let written = [question("Name?", &[], false)];
        assert_eq!(words(&written, "Fix it"), "Fix it");
        assert_eq!(words(&asked, "deny"), "deny", "a choice that holds no answers");
    }
}
