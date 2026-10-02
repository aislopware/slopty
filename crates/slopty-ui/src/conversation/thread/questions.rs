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
//! ([`choice`]). A multi-choice answer joins its picks with `", "` and puts the words of one's
//! own last, as Claude Code's own dialog does.

use gpui::{
    AnyElement, App, AppContext as _, Entity, IntoElement as _, ParentElement as _, SharedString,
    Styled as _, Window,
};
use gpui_kit::component::input::InputState;
use gpui_kit::component::questionnaire::{
    Questionnaire, QuestionnaireActions, QuestionnaireAnswer, QuestionnaireChoice,
    QuestionnaireChoiceDefinition, QuestionnaireChoices, QuestionnaireError, QuestionnaireInput,
    QuestionnaireInputDefinition, QuestionnaireItem, QuestionnaireItemDefinition,
    QuestionnaireNext, QuestionnairePrevious, QuestionnaireShortcutMode, QuestionnaireState,
    QuestionnaireSubmission, QuestionnaireSubmit, QuestionnaireTitle,
};
use gpui_kit::component::{Sizable as _, Size};
use slopty_proto::thread::detail::{Answer, Question};

/// What a question with answers calls the field for one of one's own.
pub const OTHER: &str = "Other";

/// What the field of a question that offers nothing says before anything is typed.
pub const WRITTEN: &str = "Your answer";

/// The choice that answers `questions` with `answers`.
///
/// What was written, for one question that offers nothing (a pi dialog asking for words);
/// otherwise the answers as JSON, one per question keyed by its text, which is what Claude
/// Code's `AskUserQuestion` takes.
#[must_use]
pub fn choice(questions: &[Question], answers: &[Answer]) -> String {
    match (questions, answers) {
        ([question], [answer]) if question.options.is_empty() => answer.answer.clone(),
        _ => serde_json::to_string(answers).unwrap_or_default(),
    }
}

/// An answer [`choice`] made, in words for the line that stands for the card once answered:
/// the answers in their order, or the choice itself when it holds none.
#[must_use]
pub fn words(choice: &str) -> String {
    serde_json::from_str::<Vec<Answer>>(choice).map_or_else(
        |_| choice.to_owned(),
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
            Answer { question: question.text.clone(), answer: answer.join(", ") }
        })
        .collect()
}

/// Questions being answered: their questionnaire, one item per question named by its place,
/// one choice per answer named by its place, so two that read the same stay two.
pub struct Questions {
    questions: Vec<Question>,
    state: Entity<QuestionnaireState>,
}

impl std::fmt::Debug for Questions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Questions")
            .field("questions", &self.questions.len())
            .finish_non_exhaustive()
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
                let field = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
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
        Self { questions, state }
    }

    /// The questions.
    #[must_use]
    pub fn questions(&self) -> &[Question] {
        &self.questions
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
        self.questions.get(name.parse::<usize>().ok()?)
    }

    /// Whether the keyboard is somewhere in the questionnaire.
    #[must_use]
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.state.read(cx).focus_handle().contains_focused(window, cx)
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
    pub fn element(&self, lead: Vec<AnyElement>) -> AnyElement {
        let state = &self.state;
        let items = self.questions.iter().enumerate().map(|(ix, question)| {
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
                .child(QuestionnaireInput::new(state, name.clone()))
                .child(QuestionnaireError::new(state, name))
        });
        Questionnaire::new(state)
            .with_size(Size::Small)
            .children(items)
            .child(
                QuestionnaireActions::new(state)
                    .flex_wrap()
                    .children(lead)
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

    /// One question that offers nothing is answered in its words; anything else is the
    /// answers keyed by each question's text, which the Claude Code adapter reads back.
    #[test]
    fn a_choice_is_the_words_for_a_lone_written_question_and_json_otherwise() {
        let written = [question("Commit message?", &[], false)];
        assert_eq!(choice(&written, &[answer("Commit message?", "Fix it")]), "Fix it");

        let asked = [question("Which layout?", &["Split", "Stacked"], false)];
        let answered = [answer("Which layout?", "Split")];
        let sent = choice(&asked, &answered);
        assert_eq!(sent, r#"[{"question":"Which layout?","answer":"Split"}]"#);
        let read: Vec<slopty_proto::conversation::Answer> = serde_json::from_str(&sent).unwrap();
        assert_eq!(read[0].answer, "Split", "the adapter's own type reads it");

        let two = [question("Name?", &[], false), question("Why?", &[], false)];
        let both = [answer("Name?", "a"), answer("Why?", "b")];
        assert!(choice(&two, &both).starts_with('['), "two written questions are JSON");
    }

    /// The answered line reads the answers, not their JSON; a plain choice reads as itself.
    #[test]
    fn the_words_of_an_answer_read_its_answers() {
        let sent = choice(
            &[question("A?", &["x"], false), question("B?", &["y", "z"], true)],
            &[answer("A?", "x"), answer("B?", "y, z")],
        );
        assert_eq!(words(&sent), "x; y, z");
        assert_eq!(words("Fix it"), "Fix it");
    }
}
