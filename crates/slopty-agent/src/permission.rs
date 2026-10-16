//! Answering the permissions a driven Claude Code asks for.
//!
//! Run with `--permission-prompt-tool=stdio`, Claude Code asks about each tool call it may not
//! run alone with a `can_use_tool` control request, and waits for the answer for as long as it
//! takes. The answer is the permission result Claude Code's SDK defines ([`result`]):
//! - allow once, with the call's input;
//! - answer an `AskUserQuestion`, or approve an `ExitPlanMode` plan: an allow that carries the
//!   call's input, with the answers for a question;
//! - allow always, handing back the permission updates Claude Code suggested;
//! - deny, with a message for the model, and stop the turn when the person says so.

use serde_json::{Value, json};

use crate::conversation::{Grant, Suggestion, Verdict};

/// What Claude Code is told of a permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run the call with this input.
    Allow {
        /// The input it runs with.
        updated_input: Value,
    },
    /// Run it, and apply these permission updates.
    AllowAlways {
        /// The input it runs with.
        updated_input: Value,
        /// The updates, as Claude Code suggested them.
        updated_permissions: Vec<Value>,
    },
    /// Refuse it.
    Deny {
        /// Why, for the model.
        message: String,
        /// Also stop the turn.
        interrupt: bool,
    },
}

/// The tools whose permission is the person's answer rather than a yes or no.
const ASKS_THE_PERSON: [&str; 2] = ["AskUserQuestion", "ExitPlanMode"];

/// Whether a permission for `tool` is a yes or no that "Allow" or "Deny" answers whole.
///
/// Such a prompt may be answered from a notification or the inbox, without the conversation in
/// view. A question wants its answers and a plan wants reading, so those wait for the
/// conversation face.
#[must_use]
pub fn approvable(tool: &str) -> bool {
    !tool.is_empty() && !ASKS_THE_PERSON.contains(&tool)
}

/// The decision a person's verdict on a permission for a call with `input` makes.
///
/// `suggestions` are the updates Claude Code suggested.
///
/// "Allow always" hands back every suggestion, as Claude Code's own "Yes, and always …" does. An
/// answer carries the call's input with the answers, keyed by each question's text as
/// `AskUserQuestion` reads them. A denial without words gets some.
#[must_use]
pub fn decision(verdict: &Verdict, input: &Value, suggestions: &Value) -> Decision {
    match verdict {
        Verdict::Allow => Decision::Allow { updated_input: input.clone() },
        Verdict::Answer { answers } => {
            let mut input = input.clone();
            let answers: serde_json::Map<String, Value> = answers
                .iter()
                .map(|a| (a.question.clone(), Value::String(a.answer.clone())))
                .collect();
            if let Some(fields) = input.as_object_mut() {
                fields.insert("answers".to_owned(), Value::Object(answers));
            }
            Decision::Allow { updated_input: input }
        }
        Verdict::AllowAlways => Decision::AllowAlways {
            updated_input: input.clone(),
            updated_permissions: suggestions.as_array().cloned().unwrap_or_default(),
        },
        Verdict::Deny { message, interrupt } => Decision::Deny {
            message: if message.trim().is_empty() {
                "The person declined this in Slopty.".to_owned()
            } else {
                message.clone()
            },
            interrupt: *interrupt,
        },
    }
}

/// The permission result Claude Code takes for `decision`.
#[must_use]
pub fn result(decision: &Decision) -> Value {
    match decision {
        Decision::Allow { updated_input } => {
            json!({ "behavior": "allow", "updatedInput": updated_input })
        }
        Decision::AllowAlways { updated_input, updated_permissions } => json!({
            "behavior": "allow",
            "updatedInput": updated_input,
            "updatedPermissions": updated_permissions,
        }),
        Decision::Deny { message, interrupt: false } => {
            json!({ "behavior": "deny", "message": message })
        }
        Decision::Deny { message, interrupt: true } => {
            json!({ "behavior": "deny", "message": message, "interrupt": true })
        }
    }
}

/// Claude Code's permission updates (`permission_suggestions`, the SDK's `PermissionUpdate`),
/// typed for a client to word. One of a kind this version does not know is kept by its type.
#[must_use]
pub fn suggestions(value: &Value) -> Vec<Suggestion> {
    let strings = |update: &Value, key: &str| -> Vec<String> {
        update
            .get(key)
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let rule = |rule: &Value| {
        let tool = rule.get("toolName").and_then(Value::as_str)?;
        Some(match rule.get("ruleContent").and_then(Value::as_str) {
            Some(content) => format!("{tool}({content})"),
            None => tool.to_owned(),
        })
    };
    let text = |update: &Value, key: &str| {
        update.get(key).and_then(Value::as_str).map(str::to_owned).unwrap_or_default()
    };
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|update| {
            let kind = text(update, "type");
            let grant = match kind.as_str() {
                "addRules" | "replaceRules" => Grant::Rules {
                    behavior: text(update, "behavior"),
                    rules: update
                        .get("rules")
                        .and_then(Value::as_array)
                        .map(|rules| rules.iter().filter_map(rule).collect())
                        .unwrap_or_default(),
                },
                "setMode" => Grant::Mode { mode: text(update, "mode") },
                "addDirectories" => {
                    Grant::Directories { directories: strings(update, "directories") }
                }
                _ => Grant::Other { kind },
            };
            Suggestion {
                grant,
                destination: update.get("destination").and_then(Value::as_str).map(str::to_owned),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::Answer;

    /// A tool call is a yes or no an approver answers; a question and a plan are not.
    #[test]
    fn only_a_tool_call_is_approvable_from_outside_the_conversation() {
        assert!(approvable("Bash"));
        assert!(approvable("mcp__github__create_issue"));
        assert!(!approvable("AskUserQuestion"));
        assert!(!approvable("ExitPlanMode"));
        assert!(!approvable(""));
    }

    /// The suggestions Claude Code makes, typed; "allow always" hands them back as they came,
    /// with the call's input, and a silent denial gets words.
    #[test]
    fn always_hands_back_what_claude_code_suggested() {
        let given = json!([
            { "type": "addDirectories", "directories": ["/work"], "destination": "session" },
            { "type": "setMode", "mode": "acceptEdits", "destination": "session" },
            { "type": "addRules", "behavior": "allow", "destination": "localSettings",
              "rules": [{ "toolName": "Bash", "ruleContent": "npm test:*" }, { "toolName": "Read" }] },
            { "type": "removeDirectories", "directories": ["/tmp"] }
        ]);
        let typed = suggestions(&given);
        assert_eq!(typed.len(), 4);
        assert_eq!(
            typed[2].grant,
            Grant::Rules {
                behavior: "allow".to_owned(),
                rules: vec!["Bash(npm test:*)".to_owned(), "Read".to_owned()]
            }
        );
        assert_eq!(typed[3].grant, Grant::Other { kind: "removeDirectories".to_owned() });
        let input = json!({ "command": "npm test" });
        let always = result(&decision(&Verdict::AllowAlways, &input, &given));
        assert_eq!(always["updatedPermissions"], given);
        assert_eq!(always["updatedInput"], input);
        let silent = Verdict::Deny { message: " ".to_owned(), interrupt: true };
        let denied = result(&decision(&silent, &input, &Value::Null));
        assert_eq!(
            (denied["behavior"].as_str(), denied["interrupt"].as_bool()),
            (Some("deny"), Some(true))
        );
        assert_ne!(denied["message"].as_str().unwrap_or_default().trim(), "");
    }

    /// An answer to `AskUserQuestion` is an allow whose input is the call's own with the
    /// answers keyed by each question's text.
    #[test]
    fn an_answer_carries_the_calls_input() {
        let input = json!({ "questions": [{ "question": "Which layout?", "options": [] }] });
        let answers =
            vec![Answer { question: "Which layout?".to_owned(), answer: "Split".to_owned() }];
        let answered = result(&decision(&Verdict::Answer { answers }, &input, &Value::Null));
        assert_eq!(answered["behavior"], "allow");
        assert_eq!(answered["updatedInput"]["answers"]["Which layout?"], "Split");
        assert_eq!(answered["updatedInput"]["questions"], input["questions"]);
    }
}
