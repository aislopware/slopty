//! Answering Claude Code's permission prompts from the conversation face.
//!
//! Claude Code runs the `PermissionRequest` hook when it is about to ask the person for
//! permission to use a tool. A hook that prints a decision answers for them; one that prints
//! nothing leaves the TUI to show its own dialog. So `slopty hook` is registered for that event
//! synchronously ([`crate::hooks`]): after posting the hook as usual it asks the worker for a
//! decision over the control socket and waits, up to [`WAIT`], for one of
//! - allow once;
//! - allow always, handing back the permission updates Claude Code suggested;
//! - deny, with a message for the model;
//! - no decision: the TUI's dialog appears, as it would with no hook at all.
//!
//! **The socket contract.** The request is one line of JSON, `{"cmd": "permission",
//! "session": <SessionId>, "payload": <the forwarded hook, as a JSON string>, "wait_ms": <u64>}`
//! ([`RelayRequest`]), and the reply one line, `{"reply": "permission", "decision": {"kind":
//! "pass" | "allow" | "allow_always" | "deny", …}}` ([`RelayReply`]). These are shaped to be
//! a `CtlRequest::Permission(PermissionAsk)` and `CtlReply::Permission(PermissionAnswer)` on the
//! worker's own enums, which tag the same way. The worker holds the reply while a client shows
//! the face and answers from its card; with no such client, or when the person turns back to
//! the TUI, it answers [`Decision::Pass`] at once, and it never holds past `wait_ms`. The
//! payload is the same hook the relay already posted as `CtlRequest::Hook`; the worker must not
//! apply it to the tracker a second time.
//!
//! A worker that does not know the request closes the connection without a reply; so does one
//! that is not running. Either way the relay reads no decision and prints nothing, at once.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slopty_core::SessionId;

/// The `timeout` the `PermissionRequest` entry is registered with, in seconds: Claude Code's
/// own default for command hooks. Past it, Claude Code cancels the hook and shows its dialog.
pub const HOOK_TIMEOUT_S: u32 = 600;

/// How long the relay waits for a decision: the hook's timeout less a margin, so the relay
/// answers "no decision" itself before Claude Code gives up on it.
pub const WAIT: Duration = Duration::from_secs(HOOK_TIMEOUT_S as u64 - 5);

/// The relay's question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAsk {
    /// The terminal session the agent runs in (`SLOPTY_SESSION`).
    pub session: SessionId,
    /// The `PermissionRequest` hook as the relay forwards it ([`crate::Hook::trimmed`]): the
    /// tool, its input and the suggested permission updates.
    pub payload: String,
    /// How long the relay waits; the worker answers before then.
    pub wait_ms: u64,
}

/// The line the relay sends: `{"cmd":"permission", …}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum RelayRequest {
    /// Ask for a decision.
    Permission(PermissionAsk),
}

/// The worker's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAnswer {
    /// What to tell Claude Code.
    pub decision: Decision,
}

/// The line the worker replies: `{"reply":"permission","decision":{…}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum RelayReply {
    /// The decision.
    Permission(PermissionAnswer),
}

/// A decision on one permission request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// No decision: Claude Code shows its own dialog.
    Pass,
    /// Allow this call.
    Allow,
    /// Allow this call and apply these permission updates, normally what the request's
    /// `permission_suggestions` offered (an allow rule, a mode, a directory).
    AllowAlways {
        /// The updates, as Claude Code's `updatedPermissions` entries.
        updated_permissions: Vec<Value>,
    },
    /// Refuse the call.
    Deny {
        /// Why, for the model.
        message: String,
        /// Also stop the turn.
        interrupt: bool,
    },
}

impl Decision {
    /// What the hook prints for Claude Code: `hookSpecificOutput.decision` as the hooks
    /// reference defines it for `PermissionRequest`; `None` (print nothing) for no decision.
    #[must_use]
    pub fn hook_output(&self) -> Option<Value> {
        let decision = match self {
            Self::Pass => return None,
            Self::Allow => json!({ "behavior": "allow" }),
            Self::AllowAlways { updated_permissions } => {
                json!({ "behavior": "allow", "updatedPermissions": updated_permissions })
            }
            Self::Deny { message, interrupt: false } => {
                json!({ "behavior": "deny", "message": message })
            }
            Self::Deny { message, interrupt: true } => {
                json!({ "behavior": "deny", "message": message, "interrupt": true })
            }
        };
        Some(json!({
            "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire lines are the shapes the worker's `CtlRequest`/`CtlReply` will carry.
    #[test]
    fn the_socket_lines_are_tagged_like_the_control_protocol() {
        let session = SessionId::new();
        let ask = RelayRequest::Permission(PermissionAsk {
            session,
            payload: "{}".to_owned(),
            wait_ms: 1_000,
        });
        let line = serde_json::to_value(&ask).expect("json");
        assert_eq!(
            line,
            json!({ "cmd": "permission", "session": session, "payload": "{}", "wait_ms": 1_000 })
        );
        let reply: RelayReply = serde_json::from_value(json!({
            "reply": "permission",
            "decision": { "kind": "deny", "message": "no", "interrupt": false },
        }))
        .expect("reply");
        assert_eq!(
            reply,
            RelayReply::Permission(PermissionAnswer {
                decision: Decision::Deny { message: "no".to_owned(), interrupt: false }
            })
        );
        assert_eq!(Decision::Pass.hook_output(), None);
        let stop = Decision::Deny { message: "stop".to_owned(), interrupt: true };
        assert_eq!(
            stop.hook_output().map(|o| o["hookSpecificOutput"]["decision"].clone()),
            Some(json!({ "behavior": "deny", "message": "stop", "interrupt": true }))
        );
    }
}
