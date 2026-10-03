//! An MCP server's form, asked through Codex (`mcpServer/elicitation/request`), as questions the
//! person answers in the GUI, and their answers as the form's content.
//!
//! Each field of the form's schema is one question, in the order of the fields' names: a text or a
//! number is answered in words, a yes-or-no by "Yes" or "No", a choice by its titles, several of
//! them where the field takes a list. The answers come back as one [`Answer`] list
//! (`detail::Answer`) and go to Codex as `{"action": "accept", "content": {…}}`, each value of its
//! field's type; an answer that does not fit its field (a word for a number, a title not offered)
//! is no answer, and nothing goes. Declining or cancelling is a choice beside the questions. A form
//! whose schema Slopty does not read, and the other kinds (a page to open, a device check), are
//! answered in Codex's own terminal.

use serde_json::{Map, Number, Value};
use slopty_proto::thread::detail::{Answer, Offered, Question};

use super::protocol::{
    McpElicitationEnumSchema, McpElicitationMultiSelectEnumSchema, McpElicitationNumberType,
    McpElicitationPrimitiveSchema, McpElicitationSchema, McpElicitationSingleSelectEnumSchema,
    McpServerElicitationRequestParamsMode,
};

/// A yes, as a yes-or-no field offers it.
pub const YES: &str = "Yes";
/// A no.
pub const NO: &str = "No";

/// What a field takes.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Text,
    Number {
        integer: bool,
    },
    Boolean,
    /// One of these values, by title.
    One(Vec<(String, String)>),
    /// Any of them.
    Many(Vec<(String, String)>),
}

/// One field of a form.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    key: String,
    kind: Kind,
    question: Question,
}

/// A form: what it says, and its fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    /// What the server says about it.
    pub message: String,
    /// Its fields, by name.
    pub fields: Vec<Field>,
}

impl Form {
    /// The form `mode` asks for, when it is one Slopty reads.
    #[must_use]
    pub fn of(mode: &McpServerElicitationRequestParamsMode) -> Option<Self> {
        let (message, schema) = match mode {
            McpServerElicitationRequestParamsMode::Form { message, requested_schema, .. } => {
                (message, requested_schema.clone())
            }
            McpServerElicitationRequestParamsMode::OpenaiForm {
                message, requested_schema, ..
            }
            | McpServerElicitationRequestParamsMode::OpenaiForm_ {
                message,
                requested_schema,
                ..
            } => (message, serde_json::from_value(requested_schema.clone()).ok()?),
            McpServerElicitationRequestParamsMode::OpenaiUserVerification { .. }
            | McpServerElicitationRequestParamsMode::Url { .. } => return None,
        };
        Some(Self { message: message.clone(), fields: fields(&schema) })
    }

    /// The questions it asks.
    #[must_use]
    pub fn questions(&self) -> Vec<Question> {
        self.fields.iter().map(|f| f.question.clone()).collect()
    }

    /// The content the answers in `choice` fill it with; `None` when they do not answer each
    /// field, or one does not fit its field.
    #[must_use]
    pub fn content(&self, choice: &str) -> Option<Value> {
        let answers = Answer::read(&self.questions(), choice)?;
        let mut content = Map::new();
        for (field, answer) in self.fields.iter().zip(answers) {
            content.insert(field.key.clone(), field.value(&answer)?);
        }
        Some(Value::Object(content))
    }
}

impl Field {
    /// `answer` as this field's value.
    fn value(&self, answer: &Answer) -> Option<Value> {
        let words = answer.answer.trim();
        match &self.kind {
            Kind::Text => Some(Value::String(words.to_owned())),
            Kind::Number { integer: true } => words.parse::<i64>().ok().map(Value::from),
            Kind::Number { integer: false } => {
                words.parse::<f64>().ok().and_then(Number::from_f64).map(Value::Number)
            }
            Kind::Boolean => match words {
                YES => Some(Value::Bool(true)),
                NO => Some(Value::Bool(false)),
                _ => None,
            },
            Kind::One(values) => valued(values, words).map(Value::String),
            Kind::Many(values) => answer
                .parts(&self.question)
                .iter()
                .map(|part| valued(values, part).map(Value::String))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array),
        }
    }
}

/// The value whose title is `title`.
fn valued(values: &[(String, String)], title: &str) -> Option<String> {
    values.iter().find(|(_, t)| t == title).map(|(value, _)| value.clone())
}

/// The fields of `schema`, each asked by its description, else its title, else its name; two
/// that would read the same are told apart by their names, since an answer is keyed by what
/// its question says.
fn fields(schema: &McpElicitationSchema) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    for (key, property) in &schema.properties {
        let (title, description, kind) = shape(property);
        let mut text = description
            .clone()
            .or_else(|| title.clone())
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| key.clone());
        if fields.iter().any(|f| f.question.text == text) {
            text = format!("{text} ({key})");
        }
        let options = match &kind {
            Kind::Boolean => vec![offered(YES), offered(NO)],
            Kind::One(values) | Kind::Many(values) => {
                values.iter().map(|(_, title)| offered(title)).collect()
            }
            Kind::Text | Kind::Number { .. } => Vec::new(),
        };
        let question = Question {
            text,
            header: Some(title.unwrap_or_else(|| key.clone())),
            options,
            multi_select: matches!(kind, Kind::Many(_)),
        };
        fields.push(Field { key: key.clone(), kind, question });
    }
    fields
}

fn offered(label: &str) -> Offered {
    Offered { label: label.to_owned(), description: None }
}

/// A field's title, description and kind.
fn shape(property: &McpElicitationPrimitiveSchema) -> (Option<String>, Option<String>, Kind) {
    use McpElicitationEnumSchema as Enum;
    use McpElicitationMultiSelectEnumSchema as Multi;
    use McpElicitationSingleSelectEnumSchema as Single;
    let untitled = |values: &[String]| values.iter().map(|v| (v.clone(), v.clone())).collect();
    match property {
        McpElicitationPrimitiveSchema::McpElicitationStringSchema(s) => {
            (s.title.clone(), s.description.clone(), Kind::Text)
        }
        McpElicitationPrimitiveSchema::McpElicitationNumberSchema(n) => {
            let integer = n.r#type == McpElicitationNumberType::Integer;
            (n.title.clone(), n.description.clone(), Kind::Number { integer })
        }
        McpElicitationPrimitiveSchema::McpElicitationBooleanSchema(b) => {
            (b.title.clone(), b.description.clone(), Kind::Boolean)
        }
        McpElicitationPrimitiveSchema::McpElicitationEnumSchema(e) => match e {
            Enum::McpElicitationSingleSelectEnumSchema(
                Single::McpElicitationUntitledSingleSelectEnumSchema(s),
            ) => (s.title.clone(), s.description.clone(), Kind::One(untitled(&s.r#enum))),
            Enum::McpElicitationSingleSelectEnumSchema(
                Single::McpElicitationTitledSingleSelectEnumSchema(s),
            ) => {
                let values = s.one_of.iter().map(|o| (o.r#const.clone(), o.title.clone()));
                (s.title.clone(), s.description.clone(), Kind::One(values.collect()))
            }
            Enum::McpElicitationMultiSelectEnumSchema(
                Multi::McpElicitationUntitledMultiSelectEnumSchema(m),
            ) => (m.title.clone(), m.description.clone(), Kind::Many(untitled(&m.items.r#enum))),
            Enum::McpElicitationMultiSelectEnumSchema(
                Multi::McpElicitationTitledMultiSelectEnumSchema(m),
            ) => {
                let values = m.items.any_of.iter().map(|o| (o.r#const.clone(), o.title.clone()));
                (m.title.clone(), m.description.clone(), Kind::Many(values.collect()))
            }
            Enum::McpElicitationLegacyTitledEnumSchema(l) => {
                let names = l.enum_names.clone().unwrap_or_else(|| l.r#enum.clone());
                let values = l.r#enum.iter().cloned().zip(names);
                (l.title.clone(), l.description.clone(), Kind::One(values.collect()))
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use slopty_proto::thread::detail::Answer;

    use super::{Form, NO, YES};
    use crate::codex::protocol::McpServerElicitationRequestParamsMode;

    fn form() -> Form {
        let mode: McpServerElicitationRequestParamsMode = serde_json::from_value(json!({
            "mode": "form",
            "message": "Where should the report go?",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "channel": {
                        "type": "string",
                        "title": "Channel",
                        "oneOf": [
                            { "const": "C01", "title": "#builds" },
                            { "const": "C02", "title": "#releases" }
                        ]
                    },
                    "count": { "type": "integer", "title": "How many runs" },
                    "notify": { "type": "boolean", "title": "Notify the team" },
                    "note": { "type": "string", "description": "A note to add" },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["ci", "nightly", "flaky"] }
                    }
                },
                "required": ["channel"]
            }
        }))
        .expect("a form as Codex sends it");
        Form::of(&mode).expect("a form Slopty reads")
    }

    fn choice(form: &Form, answers: &[&str]) -> String {
        let questions = form.questions();
        let given: Vec<Answer> = questions
            .iter()
            .zip(answers)
            .map(|(q, a)| Answer { question: q.text.clone(), answer: (*a).to_owned() })
            .collect();
        Answer::choice(&questions, &given)
    }

    /// Each field is a question, by the fields' names, offering what it takes: titles for a
    /// choice, yes or no, nothing for words; and the answers fill the form with each value of
    /// its field's type, a title as its value.
    #[test]
    fn a_form_is_asked_as_questions_and_answered_as_its_content() {
        let form = form();
        let questions = form.questions();
        let asked: Vec<(&str, Vec<&str>, bool)> = questions
            .iter()
            .map(|q| {
                (
                    q.text.as_str(),
                    q.options.iter().map(|o| o.label.as_str()).collect(),
                    q.multi_select,
                )
            })
            .collect();
        assert_eq!(
            asked,
            [
                ("Channel", vec!["#builds", "#releases"], false),
                ("How many runs", vec![], false),
                ("A note to add", vec![], false),
                ("Notify the team", vec![YES, NO], false),
                ("tags", vec!["ci", "nightly", "flaky"], true),
            ]
        );
        let content =
            form.content(&choice(&form, &["#releases", "3", "ship it", YES, "ci, flaky"]));
        assert_eq!(
            content,
            Some(json!({
                "channel": "C02",
                "count": 3,
                "note": "ship it",
                "notify": true,
                "tags": ["ci", "flaky"]
            }))
        );
    }

    /// An answer that does not fit its field fills nothing: a word for a number, a title not
    /// offered, neither yes nor no; and an answer to only some of the fields is no answer.
    #[test]
    fn an_answer_that_does_not_fit_fills_nothing() {
        let form = form();
        assert_eq!(form.content(&choice(&form, &["#releases", "three", "", NO, "ci"])), None);
        assert_eq!(form.content(&choice(&form, &["#general", "3", "", NO, "ci"])), None);
        assert_eq!(form.content(&choice(&form, &["#builds", "3", "", "maybe", "ci"])), None);
        assert_eq!(form.content(&choice(&form, &["#builds", "3"])), None);
    }

    /// A page to open or a device check is no form Slopty reads: Codex's own terminal answers it.
    #[test]
    fn a_page_or_a_device_check_is_no_form() {
        let url: McpServerElicitationRequestParamsMode = serde_json::from_value(json!({
            "mode": "url",
            "elicitationId": "e1",
            "message": "Sign in",
            "url": "https://example.com/auth"
        }))
        .expect("a page as Codex sends it");
        assert_eq!(Form::of(&url), None);
    }
}
