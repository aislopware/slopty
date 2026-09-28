//! An `AskUserQuestion` answered from the face: the questions one at a time, the options
//! picked, and the words typed instead.
//!
//! The answer goes to Claude Code the way its own dialog's would: as the call's input with the
//! answers keyed by each question's text (`Verdict::Answer`, which the worker turns into the
//! `PermissionRequest` hook's `updatedInput`). Nothing is typed into the TUI's menu.
//!
//! A single-choice pick goes on to the next question at once, and on the last one it is the
//! answer. A multi-choice pick toggles, and Next or Answer goes on. Words typed in the field are
//! the answer to the question on show, over any pick; the options of a multi-choice answer are
//! joined with `", "`, as the TUI joins them.

use slopty_proto::conversation::{Answer, Question};

/// The questions of one held prompt, as far as they are answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answering {
    ask: u64,
    questions: Vec<Question>,
    /// The question on show.
    at: usize,
    /// The options picked, by question.
    picked: Vec<Vec<usize>>,
    /// The words typed, by question.
    typed: Vec<String>,
    /// The option the keyboard is on.
    cursor: usize,
}

/// What a pick did.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Picked {
    /// The pick shows; the person goes on when ready.
    Shown,
    /// It moved to the next question.
    Next,
    /// It was the last question's single choice: these are the answers.
    Done(Vec<Answer>),
}

impl Answering {
    /// Prompt `ask`'s questions, none answered.
    #[must_use]
    pub fn new(ask: u64, questions: Vec<Question>) -> Self {
        let count = questions.len();
        Self {
            ask,
            questions,
            at: 0,
            picked: vec![Vec::new(); count],
            typed: vec![String::new(); count],
            cursor: 0,
        }
    }

    /// The prompt these answer.
    #[must_use]
    pub const fn ask(&self) -> u64 {
        self.ask
    }

    /// The question on show.
    #[must_use]
    pub fn question(&self) -> Option<&Question> {
        self.questions.get(self.at)
    }

    /// Which question is on show, from 1, and how many there are.
    #[must_use]
    pub const fn position(&self) -> (usize, usize) {
        (self.at.saturating_add(1), self.questions.len())
    }

    /// The question on show is the last.
    #[must_use]
    pub const fn last(&self) -> bool {
        self.at.saturating_add(1) >= self.questions.len()
    }

    /// Whether option `ix` of the question on show is picked.
    #[must_use]
    pub fn is_picked(&self, ix: usize) -> bool {
        self.picked.get(self.at).is_some_and(|p| p.contains(&ix))
    }

    /// The option the keyboard is on.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The words kept for the question on show, to put back in the field.
    #[must_use]
    pub fn typed(&self) -> &str {
        self.typed.get(self.at).map_or("", String::as_str)
    }

    /// Move the keyboard's option by `delta`, held within the options.
    pub fn step(&mut self, delta: isize) {
        let count = self.question().map_or(0, |q| q.options.len());
        self.cursor = self.cursor.saturating_add_signed(delta).min(count.saturating_sub(1));
    }

    /// Pick option `ix` of the question on show.
    pub fn pick(&mut self, ix: usize) -> Picked {
        let Some(question) = self.question() else { return Picked::Shown };
        if ix >= question.options.len() {
            return Picked::Shown;
        }
        let multi = question.multi_select;
        self.cursor = ix;
        let Some(picked) = self.picked.get_mut(self.at) else { return Picked::Shown };
        if multi {
            if let Some(pos) = picked.iter().position(|p| *p == ix) {
                picked.remove(pos);
            } else {
                picked.push(ix);
                picked.sort_unstable();
            }
            return Picked::Shown;
        }
        *picked = vec![ix];
        // A pick is the answer: words left in the field would stand over it.
        if let Some(words) = self.typed.get_mut(self.at) {
            words.clear();
        }
        self.go_on(String::new())
    }

    /// Whether the question on show has an answer, the field saying `typed`.
    #[must_use]
    pub fn answered(&self, typed: &str) -> bool {
        !typed.trim().is_empty() || self.picked.get(self.at).is_some_and(|p| !p.is_empty())
    }

    /// Next or Answer, the field saying `typed`: the next question, or on the last, the
    /// answers. [`Picked::Shown`] when the question on show has no answer yet.
    pub fn go_on(&mut self, typed: String) -> Picked {
        if !self.answered(&typed) {
            return Picked::Shown;
        }
        if let Some(words) = self.typed.get_mut(self.at) {
            *words = typed;
        }
        if self.last() {
            return Picked::Done(self.answers());
        }
        self.at = self.at.saturating_add(1);
        self.cursor = 0;
        Picked::Next
    }

    /// The answers given so far, one per question answered.
    #[must_use]
    pub fn answers(&self) -> Vec<Answer> {
        self.questions
            .iter()
            .enumerate()
            .filter_map(|(ix, question)| {
                let typed = self.typed.get(ix).map_or("", |t| t.trim());
                let answer = if typed.is_empty() {
                    let picked = self.picked.get(ix)?;
                    let labels: Vec<&str> = picked
                        .iter()
                        .filter_map(|p| question.options.get(*p).map(|o| o.label.as_str()))
                        .collect();
                    if labels.is_empty() {
                        return None;
                    }
                    labels.join(", ")
                } else {
                    typed.to_owned()
                };
                Some(Answer { question: question.text.clone(), answer })
            })
            .collect()
    }
}

/// What the line under the conversation says of answers given: "Answered: Split".
#[must_use]
pub fn answered_line(answers: &[Answer]) -> String {
    let said: Vec<&str> = answers.iter().map(|a| a.answer.as_str()).collect();
    format!("Answered: {}", said.join("; "))
}

#[cfg(test)]
mod tests {
    use slopty_proto::conversation::Choice;

    use super::*;

    fn question(text: &str, options: &[&str], multi_select: bool) -> Question {
        Question {
            text: text.to_owned(),
            header: None,
            options: options
                .iter()
                .map(|label| Choice { label: (*label).to_owned(), description: None })
                .collect(),
            multi_select,
        }
    }

    /// A single choice goes on at once and is the answer on the last question; a multi choice
    /// toggles until Answer; words typed stand over a pick; the answers are keyed by question.
    #[test]
    fn questions_are_answered_one_at_a_time() {
        let mut answering = Answering::new(
            4,
            vec![
                question("Which layout?", &["Split", "Stacked"], false),
                question("Which panes?", &["Files", "Terminal", "Diff"], true),
                question("Name?", &["Default"], false),
            ],
        );
        assert_eq!(answering.position(), (1, 3));
        assert!(!answering.answered(""));
        assert_eq!(answering.go_on(String::new()), Picked::Shown, "nothing picked yet");
        assert_eq!(answering.pick(1), Picked::Next);
        assert_eq!(answering.position(), (2, 3));

        assert_eq!(answering.pick(2), Picked::Shown);
        assert_eq!(answering.pick(0), Picked::Shown);
        assert_eq!(answering.pick(2), Picked::Shown, "a second pick takes it back");
        assert!(answering.is_picked(0) && !answering.is_picked(2));
        assert_eq!(answering.pick(1), Picked::Shown);
        assert_eq!(answering.go_on(String::new()), Picked::Next);

        let done = answering.go_on("Main view".to_owned());
        assert_eq!(
            done,
            Picked::Done(vec![
                Answer { question: "Which layout?".to_owned(), answer: "Stacked".to_owned() },
                Answer {
                    question: "Which panes?".to_owned(),
                    answer: "Files, Terminal".to_owned()
                },
                Answer { question: "Name?".to_owned(), answer: "Main view".to_owned() },
            ]),
            "words typed are the last answer"
        );
        let Picked::Done(answers) = done else { return };
        assert_eq!(answered_line(&answers), "Answered: Stacked; Files, Terminal; Main view");
    }

    /// The keyboard's option stays within the question's options.
    #[test]
    fn the_keyboard_moves_within_the_options() {
        let mut answering = Answering::new(1, vec![question("Q", &["a", "b"], false)]);
        answering.step(-1);
        assert_eq!(answering.cursor(), 0);
        answering.step(1);
        answering.step(1);
        assert_eq!(answering.cursor(), 1);
        assert_eq!(
            answering.pick(answering.cursor()),
            Picked::Done(vec![Answer { question: "Q".to_owned(), answer: "b".to_owned() }])
        );
    }
}
