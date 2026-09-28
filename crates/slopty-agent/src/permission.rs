//! Answering Claude Code's permission prompts from the conversation face.
//!
//! Claude Code runs the `PermissionRequest` hook when it is about to ask the person for
//! permission to use a tool. A hook that prints a decision answers for them; one that prints
//! nothing leaves the TUI to show its own dialog. So `slopty hook` is registered for that event
//! synchronously ([`crate::hooks`]): instead of posting the hook as it does every other event, it
//! asks the worker for a decision over the control socket and waits, up to [`WAIT`], for one of
//! - allow once;
//! - allow always, handing back the permission updates Claude Code suggested;
//! - deny, with a message for the model;
//! - no decision: the TUI's dialog appears, as it would with no hook at all.
//!
//! **The socket contract.** The request is one line of JSON, `{"cmd": "permission",
//! "session": <SessionId>, "payload": <the forwarded hook, as a JSON string>, "wait_ms": <u64>}`
//! ([`CtlRequest::Permission`] carrying a [`PermissionAsk`]), and the reply one line,
//! `{"reply": "permission", "decision": {"kind": "pass" | "allow" | "allow_always" | "deny", …}}`
//! ([`CtlReply::Permission`] carrying a [`PermissionAnswer`]); the types are the control
//! protocol's, in [`slopty_proto::ctl`]. The relay keeps its end of the socket open while it
//! waits, so the worker knows the question is withdrawn when Claude Code gives up on the hook.
//! The worker holds the reply while a client follows the session and answers from the prompt
//! it is shown ([`prompt`], [`decision`]); with no follower, or when the last one leaves, it
//! answers [`Decision::Pass`], and it never holds past `wait_ms`. The ask is the only request
//! this hook makes: the worker parses the payload once, takes it in as any hook (the tracker, the
//! followers' board), then holds it for the answer.
//!
//! [`CtlRequest::Permission`]: slopty_proto::ctl::CtlRequest::Permission
//! [`CtlReply::Permission`]: slopty_proto::ctl::CtlReply::Permission
//! [`PermissionAsk`]: slopty_proto::ctl::PermissionAsk
//! [`PermissionAnswer`]: slopty_proto::ctl::PermissionAnswer
//!
//! A worker that is not running reads nothing; the relay then prints nothing, at once.

use std::time::Duration;

use serde_json::{Value, json};
use slopty_core::SessionId;
use slopty_proto::conversation::{Grant, PermissionPrompt, Suggestion, Verdict};
use slopty_proto::ctl::Decision;

use crate::{Hook, HookEvent};

/// The `timeout` the `PermissionRequest` entry is registered with, in seconds: Claude Code's
/// own default for command hooks. Past it, Claude Code cancels the hook and shows its dialog.
pub const HOOK_TIMEOUT_S: u32 = 600;

/// How long the relay waits for a decision: the hook's timeout less a margin, so the relay
/// answers "no decision" itself before Claude Code gives up on it.
pub const WAIT: Duration = Duration::from_secs(HOOK_TIMEOUT_S as u64 - 5);

/// What the hook prints for Claude Code on `decision`: `hookSpecificOutput.decision` as the
/// hooks reference defines it for `PermissionRequest`; `None` (print nothing) for no decision.
#[must_use]
pub fn hook_output(decision: &Decision) -> Option<Value> {
    let output = match decision {
        Decision::Pass => return None,
        Decision::Allow => json!({ "behavior": "allow" }),
        Decision::AllowAlways { updated_permissions } => {
            json!({ "behavior": "allow", "updatedPermissions": updated_permissions })
        }
        Decision::Deny { message, interrupt: false } => {
            json!({ "behavior": "deny", "message": message })
        }
        Decision::Deny { message, interrupt: true } => {
            json!({ "behavior": "deny", "message": message, "interrupt": true })
        }
    };
    Some(json!({
        "hookSpecificOutput": { "hookEventName": HookEvent::PermissionRequest, "decision": output }
    }))
}

/// The decision a person's verdict makes.
///
/// `suggestions` are the hook payload's ([`Hook`]) `permission_suggestions`: "allow always"
/// hands every one of them back, as Claude Code's own "Yes, and always …" does. A denial
/// without words gets some.
#[must_use]
pub fn decision(verdict: &Verdict, suggestions: Option<&Value>) -> Decision {
    match verdict {
        Verdict::Allow => Decision::Allow,
        Verdict::AllowAlways => Decision::AllowAlways {
            updated_permissions: suggestions.and_then(Value::as_array).cloned().unwrap_or_default(),
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

/// The prompt a follower is shown for a `PermissionRequest` hook: the call as the conversation
/// will show it ([`crate::conversation::proposed`]) and what "allow always" would grant.
#[must_use]
pub fn prompt(
    session: SessionId,
    ask: u64,
    hook: &Hook,
    asked_ms: u64,
    until_ms: u64,
) -> PermissionPrompt {
    let tool = hook.tool_name.clone().unwrap_or_default();
    let input = hook.tool_input.clone().unwrap_or(Value::Null);
    PermissionPrompt {
        session,
        ask,
        detail: crate::conversation::proposed(&tool, &input),
        tool,
        suggestions: hook.permission_suggestions.as_ref().map(suggestions).unwrap_or_default(),
        mode: hook.permission_mode.clone(),
        asked_ms,
        until_ms,
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

    /// A decision prints what the hooks reference defines, and no decision prints nothing.
    /// (The socket lines are pinned beside `CtlRequest` in `slopty-proto`.)
    #[test]
    fn a_decision_is_the_output_the_hooks_reference_defines() {
        assert_eq!(hook_output(&Decision::Pass), None);
        let stop = Decision::Deny { message: "stop".to_owned(), interrupt: true };
        assert_eq!(
            hook_output(&stop).map(|o| o["hookSpecificOutput"]["decision"].clone()),
            Some(json!({ "behavior": "deny", "message": "stop", "interrupt": true }))
        );
    }

    /// The suggestions of the `permission` capture, and the kinds the SDK documents besides,
    /// typed; "allow always" hands them back as they came.
    #[test]
    fn a_prompt_words_the_call_and_what_always_would_grant() {
        let hook = Hook::parse(
            &json!({
                "hook_event_name": "PermissionRequest", "permission_mode": "default",
                "tool_name": "Edit",
                "tool_input": {
                    "file_path": "/work/a.rs", "old_string": "fn a() {\n    1\n}",
                    "new_string": "fn a() {\n    2\n}"
                },
                "permission_suggestions": [
                    { "type": "addDirectories", "directories": ["/work"], "destination": "session" },
                    { "type": "setMode", "mode": "acceptEdits", "destination": "session" },
                    { "type": "addRules", "behavior": "allow", "destination": "localSettings",
                      "rules": [{ "toolName": "Bash", "ruleContent": "npm test:*" }, { "toolName": "Read" }] },
                    { "type": "removeDirectories", "directories": ["/tmp"] }
                ]
            })
            .to_string(),
        )
        .expect("hook");
        let prompt = prompt(SessionId::nil(), 7, &hook, 1_000, 2_000);
        assert_eq!(
            (prompt.tool.as_str(), prompt.ask, prompt.mode.as_deref()),
            ("Edit", 7, Some("default"))
        );
        let slopty_proto::conversation::ToolDetail::Edit(edit) = &prompt.detail else {
            panic!("an edit: {:?}", prompt.detail);
        };
        assert_eq!(
            (edit.path.as_str(), edit.patch.added, edit.patch.removed),
            ("/work/a.rs", 1, 1)
        );
        assert_eq!(
            edit.patch.hunks.first().map(|h| h.lines.clone()),
            Some(vec![
                " fn a() {".to_owned(),
                "-    1".to_owned(),
                "+    2".to_owned(),
                " }".to_owned()
            ])
        );
        let session = Some("session".to_owned());
        assert_eq!(
            prompt.suggestions,
            [
                Suggestion {
                    grant: Grant::Directories { directories: vec!["/work".to_owned()] },
                    destination: session.clone()
                },
                Suggestion {
                    grant: Grant::Mode { mode: "acceptEdits".to_owned() },
                    destination: session
                },
                Suggestion {
                    grant: Grant::Rules {
                        behavior: "allow".to_owned(),
                        rules: vec!["Bash(npm test:*)".to_owned(), "Read".to_owned()]
                    },
                    destination: Some("localSettings".to_owned())
                },
                Suggestion {
                    grant: Grant::Other { kind: "removeDirectories".to_owned() },
                    destination: None
                },
            ]
        );
        let given = hook.permission_suggestions.as_ref();
        let always = decision(&Verdict::AllowAlways, given);
        assert_eq!(
            always,
            Decision::AllowAlways {
                updated_permissions: given.and_then(Value::as_array).cloned().unwrap_or_default()
            }
        );
        assert_eq!(decision(&Verdict::Allow, None), Decision::Allow);
        let silent = decision(&Verdict::Deny { message: " ".to_owned(), interrupt: false }, None);
        assert!(
            matches!(silent, Decision::Deny { message, interrupt: false } if !message.trim().is_empty())
        );
    }
}
