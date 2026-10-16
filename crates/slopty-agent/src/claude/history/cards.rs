//! A permission card in Claude Code's TUI's words (`.research/tui-strings-2026-10-16.md` §6): its
//! title per tool ("Bash command", "Edit file", "Create file", `Use skill "review"?`,
//! "{server} - {tool} (MCP)"), the question under it, and the choices its plan and plan-mode
//! dialogs offer ("Yes, clear context (42% used) and auto-accept edits", "Yes, enter plan mode").

use serde_json::Value;
use slopty_proto::thread::{Choice, Effect};

use crate::conversation::{Grant, PermissionPrompt};

/// The plan's answer that approves it with a clear context, edits accepted from then on.
///
/// The plan goes as the first message after `/clear` ([`super::super::driven`]). The same with
/// another mode is this, a colon and the mode (`clear_choice`).
pub const PLAN_CLEAR: &str = "clear";

/// The most characters of a call's parameters a card shows; past it, it says how many there
/// were, as the TUI's card does.
const PARAMS_SHOWN: usize = 4_000;

/// What the TUI's plan-mode dialog says it does.
const ENTER_PLAN: &str = "Claude wants to enter plan mode to explore and design an implementation \
approach.\n\nIn plan mode, Claude will explore the codebase thoroughly, identify existing \
patterns, design an implementation strategy and present a plan for your approval. No code \
changes will be made until you approve the plan.";

/// The choice id that approves a plan with the context cleared and mode `mode` from then on.
#[must_use]
pub(super) fn clear_choice(mode: &str) -> String {
    if mode == "acceptEdits" { PLAN_CLEAR.to_owned() } else { format!("{PLAN_CLEAR}:{mode}") }
}

/// The mode a plan's "clear context" choice `choice` goes on in; `None` for any other choice.
#[must_use]
pub fn clear_mode(choice: &str) -> Option<&str> {
    if choice == PLAN_CLEAR {
        return Some("acceptEdits");
    }
    choice.strip_prefix(PLAN_CLEAR)?.strip_prefix(':').filter(|mode| !mode.is_empty())
}

/// The mode a plan approved with the context cleared goes on in, from the mode plan mode was
/// entered from: bypass permissions and auto mode are kept, as the TUI offers them when the
/// session has them; any other goes on accepting edits.
fn after_clear(before_plan: Option<&str>) -> &'static str {
    match before_plan {
        Some("bypassPermissions") => "bypassPermissions",
        Some("auto") => "auto",
        _ => "acceptEdits",
    }
}

/// A file's name, the last part of its path.
fn named(path: &str) -> &str {
    path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path)
}

/// `input`'s string field `key`, trimmed, when it says something.
fn text<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

/// A call's parameters as the TUI's generic card lists them: `key: value, …`, each value as
/// JSON; past [`PARAMS_SHOWN`] characters, how many there were instead.
fn params(input: &Value) -> String {
    let said = match input {
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| match value {
                Value::String(s) => format!("{key}: {s:?}"),
                other => format!("{key}: {other}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => format!("{other} (parameters are not an object — deny unless expected)"),
    };
    let chars = said.chars().count();
    if chars > PARAMS_SHOWN {
        format!("(parameters total {chars} characters — too much to show — deny unless expected)")
    } else {
        said
    }
}

/// The title of a permission card and the question under it, as the TUI's card words them for
/// the prompt's tool.
#[must_use]
pub(super) fn approval(prompt: &PermissionPrompt) -> (String, Option<String>) {
    let input = &prompt.input;
    let tool = prompt.tool.as_str();
    let path = || text(input, "file_path").or_else(|| text(input, "path")).map(named);
    let (title, question) = match tool {
        "Bash" | "PowerShell" => {
            let unsandboxed = input.get("dangerouslyDisableSandbox").and_then(Value::as_bool);
            let title = if unsandboxed == Some(true) {
                format!("{tool} command (unsandboxed)")
            } else {
                format!("{tool} command")
            };
            let command = text(input, "command").unwrap_or_default();
            let question = match text(input, "description") {
                Some(description) => format!("{command}\n\n{description}"),
                None => command.to_owned(),
            };
            (title, question)
        }
        "Edit" | "MultiEdit" => (
            "Edit file".to_owned(),
            path().map(|f| format!("Do you want to make this edit to {f}?")).unwrap_or_default(),
        ),
        "Write" => {
            let file = path().unwrap_or("the file");
            match prompt.exists {
                Some(true) => {
                    ("Overwrite file".to_owned(), format!("Do you want to overwrite {file}?"))
                }
                Some(false) => ("Create file".to_owned(), format!("Do you want to create {file}?")),
                None => ("Write file".to_owned(), format!("Do you want to write to {file}?")),
            }
        }
        "NotebookEdit" => {
            let notebook = text(input, "notebook_path").map_or("the notebook", named);
            let question = match text(input, "edit_mode") {
                Some("insert") => format!("Do you want to insert this cell into {notebook}?"),
                Some("delete") => format!("Do you want to delete this cell from {notebook}?"),
                _ => format!("Do you want to make this edit to {notebook}?"),
            };
            ("Edit notebook".to_owned(), question)
        }
        "Read" | "Glob" | "Grep" | "LS" => {
            let subject = text(input, "file_path")
                .or_else(|| text(input, "pattern"))
                .or_else(|| text(input, "path"))
                .unwrap_or_default();
            ("Read file".to_owned(), subject.to_owned())
        }
        "WebFetch" => {
            let url = text(input, "url").unwrap_or_default();
            (
                "Fetch".to_owned(),
                format!("{url}\n\nDo you want to allow Claude to fetch this content?"),
            )
        }
        "WebSearch" => {
            ("Web search".to_owned(), text(input, "query").unwrap_or_default().to_owned())
        }
        "Skill" => {
            let title = text(input, "skill")
                .map_or_else(|| "Use this skill?".to_owned(), |s| format!("Use skill \"{s}\"?"));
            (title, "Claude may use instructions, code, or files from this Skill.".to_owned())
        }
        "EnterPlanMode" => ("Enter plan mode?".to_owned(), ENTER_PLAN.to_owned()),
        _ => match tool.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
            Some((server, name)) => (format!("{server} - {name} (MCP)"), params(input)),
            None => ("Tool use".to_owned(), format!("{tool}({})", params(input))),
        },
    };
    let question = match &prompt.on_disk {
        Some(seen) if question.trim().is_empty() => seen.clone(),
        Some(seen) => format!("{question}\n\n{seen}"),
        None => question,
    };
    let question = Some(question).filter(|q| !q.trim().is_empty());
    (title, question)
}

/// The mode a plan's request suggests switching to on approval, other than plan mode itself.
fn suggested_mode(prompt: &PermissionPrompt) -> Option<&str> {
    prompt.suggestions.iter().find_map(|s| match &s.grant {
        Grant::Mode { mode } if !mode.trim().is_empty() && mode.trim() != "plan" => {
            Some(mode.trim())
        }
        _ => None,
    })
}

/// The choices a plan's approval offers, as the TUI's plan dialog words them, in its order:
/// approve with a clear context, approve in the mode the request suggests, approve and keep
/// asking about edits, and keep planning. `scope` is what the suggested mode grants, for the
/// choice that applies it.
#[must_use]
pub(super) fn plan_options(prompt: &PermissionPrompt, scope: Option<String>) -> Vec<Choice> {
    let choice = |id: String, label: String, effect, scope, stops| Choice {
        id,
        label,
        effect,
        scope,
        stops,
    };
    let mode = after_clear(prompt.before_plan.as_deref());
    let used = prompt.context_used.map(|p| format!(" ({p}% used)")).unwrap_or_default();
    let then = match mode {
        "bypassPermissions" => "bypass permissions",
        "auto" => "use auto mode",
        _ => "auto-accept edits",
    };
    let mut options = vec![choice(
        clear_choice(mode),
        format!("Yes, clear context{used} and {then}"),
        Effect::Allow,
        None,
        true,
    )];
    if let Some(suggested) = suggested_mode(prompt) {
        let label = match suggested {
            "acceptEdits" => "Yes, auto-accept edits".to_owned(),
            "auto" => "Yes, and use auto mode".to_owned(),
            "bypassPermissions" => "Yes, and bypass permissions".to_owned(),
            other => format!("Yes, and switch to {}", super::words(other)),
        };
        options.push(choice("always".to_owned(), label, Effect::Allow, scope, false));
    }
    options.extend([
        choice(
            "allow".to_owned(),
            "Yes, manually approve edits".to_owned(),
            Effect::Allow,
            None,
            false,
        ),
        choice("deny".to_owned(), "No, keep planning".to_owned(), Effect::Deny, None, false),
    ]);
    options
}

/// The choices the TUI's plan-mode dialog offers.
#[must_use]
pub(super) fn enter_plan_options() -> Vec<Choice> {
    let choice = |id: &str, label: &str, effect| Choice {
        id: id.to_owned(),
        label: label.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    vec![
        choice("allow", "Yes, enter plan mode", Effect::Allow),
        choice("deny", "No, start implementing now", Effect::Deny),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use slopty_core::WallMs;

    use super::*;
    use crate::conversation::{Suggestion, ToolDetail};

    fn prompt(tool: &str, input: Value) -> PermissionPrompt {
        PermissionPrompt {
            reason: None,
            ask: 1,
            tool: tool.to_owned(),
            call: None,
            detail: ToolDetail::Other {
                input: crate::conversation::Clipped {
                    text: String::new(),
                    lines: 0,
                    chars: 0,
                    full: None,
                },
            },
            suggestions: Vec::new(),
            mode: None,
            asked_ms: WallMs::ZERO,
            needs_presence: false,
            input,
            exists: None,
            context_used: None,
            before_plan: None,
            on_disk: None,
            default_to_no: false,
        }
    }

    /// Each tool's card reads as the TUI's: a command and what it is for, an edit's question, a
    /// new file apart from one overwritten, a skill, an MCP tool and any other tool by its
    /// parameters; parameters too long to read say so instead.
    #[test]
    fn a_permission_card_is_worded_as_the_tuis() {
        let card = |tool: &str, input: Value| approval(&prompt(tool, input));
        assert_eq!(
            card("Bash", json!({ "command": "rm -rf build", "description": "Clean the build" })),
            ("Bash command".to_owned(), Some("rm -rf build\n\nClean the build".to_owned()))
        );
        assert_eq!(
            card("Bash", json!({ "command": "curl x", "dangerouslyDisableSandbox": true })).0,
            "Bash command (unsandboxed)"
        );
        assert_eq!(
            card("Edit", json!({ "file_path": "/w/src/lib.rs" })),
            ("Edit file".to_owned(), Some("Do you want to make this edit to lib.rs?".to_owned()))
        );
        let mut write = prompt("Write", json!({ "file_path": "/w/new.txt" }));
        write.exists = Some(false);
        assert_eq!(approval(&write).0, "Create file");
        write.exists = Some(true);
        assert_eq!(approval(&write).1.as_deref(), Some("Do you want to overwrite new.txt?"));
        assert_eq!(card("Skill", json!({ "skill": "review" })).0, "Use skill \"review\"?");
        assert_eq!(
            card("mcp__db__query", json!({ "sql": "select 1" })),
            ("db - query (MCP)".to_owned(), Some("sql: \"select 1\"".to_owned()))
        );
        assert_eq!(
            card("Frob", json!({ "n": 2 })),
            ("Tool use".to_owned(), Some("Frob(n: 2)".to_owned()))
        );
        let long = card("Frob", json!({ "blob": "x".repeat(5_000) })).1.unwrap_or_default();
        assert!(long.contains("too much to show — deny unless expected"), "{long}");
        assert_eq!(card("EnterPlanMode", json!({})).0, "Enter plan mode?");
    }

    /// A plan offers the TUI's choices: clear context with how much is used, in the mode it was
    /// planned from, then the suggested mode, manual approval and planning on; the clear choice
    /// names its mode, and only it stops the turn.
    #[test]
    fn a_plan_offers_its_dialogs_choices() {
        let mut plan = prompt("ExitPlanMode", json!({ "plan": "1. Do it" }));
        plan.context_used = Some(42);
        plan.suggestions = vec![Suggestion {
            grant: Grant::Mode { mode: "acceptEdits".to_owned() },
            destination: Some("session".to_owned()),
        }];
        let labels = |options: &[Choice]| -> Vec<(String, String)> {
            options.iter().map(|c| (c.id.clone(), c.label.clone())).collect()
        };
        let options = plan_options(&plan, None);
        assert_eq!(
            labels(&options),
            [
                (
                    "clear".to_owned(),
                    "Yes, clear context (42% used) and auto-accept edits".to_owned()
                ),
                ("always".to_owned(), "Yes, auto-accept edits".to_owned()),
                ("allow".to_owned(), "Yes, manually approve edits".to_owned()),
                ("deny".to_owned(), "No, keep planning".to_owned()),
            ]
        );
        assert_eq!(options.iter().filter(|c| c.stops).count(), 1);
        plan.before_plan = Some("bypassPermissions".to_owned());
        plan.context_used = None;
        let first = plan_options(&plan, None).remove(0);
        assert_eq!(
            (first.id.as_str(), first.label.as_str()),
            ("clear:bypassPermissions", "Yes, clear context and bypass permissions")
        );
        assert_eq!(clear_mode("clear:bypassPermissions"), Some("bypassPermissions"));
        assert_eq!(clear_mode("clear"), Some("acceptEdits"));
        assert_eq!(clear_mode("clearly"), None);
        assert_eq!(clear_mode("allow"), None);
    }
}
