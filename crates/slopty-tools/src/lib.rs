//! The orchestration verbs as one contract, whatever carries them (`docs/decisions/topology.md`).
//!
//! The server's MCP endpoint, `slopty mcp` and the `slopty` CLI all resolve the same names,
//! print the same JSON and list the same tools, because they all run this crate over their own
//! [`Dispatch`]: the server's hub answers verbs in-process, the CLI sends them down its link.
//!
//! * [`resolve`]: worker names and id prefixes, `worker/session` handles, to ids.
//! * [`ops`]: each verb once, resolved, sent, and its one expected answer taken apart.
//! * [`bulk`]: files of any size up and down, in parts.
//! * [`view`]: the answers as JSON for scripts and models, and as text for a person.
//! * [`tools`]: the MCP tools, their schemas, and a call run end to end.
//! * [`mcp`]: those tools as an MCP server over any [`Dispatch`].

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod budget;
pub mod bulk;
pub mod mcp;
pub mod ops;
pub mod resolve;
pub mod tools;
pub mod view;

use slopty_core::SessionId;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::project::{PROJECT_ENV, ProjectId, TASK_ENV, TaskId};

/// Where verbs go: something that answers each with its [`Outcome`], a failure included.
///
/// Calls may be in flight together; each answer finds its own caller.
pub trait Dispatch: Send + Sync {
    /// Answer `verb`, done once per `key` when it changes something.
    fn send(&self, key: Option<IdempotencyKey>, verb: Verb)
    -> impl Future<Output = Outcome> + Send;

    /// Answer `verb`, which needs no key.
    fn call(&self, verb: Verb) -> impl Future<Output = Outcome> + Send {
        self.send(None, verb)
    }

    /// Whether a file the caller names on its own machine is one this side can read and write:
    /// the CLI and `slopty mcp` run on the caller's machine, the server's endpoint does not.
    fn local_files(&self) -> bool {
        true
    }

    /// Where the caller runs: the defaults a project verb takes when it names no project,
    /// task or terminal. Only a caller on its own machine has an environment to read.
    fn scope(&self) -> Scope {
        Scope::default()
    }
}

/// Where the caller runs, as a Slopty session's environment says: its terminal, and for an
/// agent Slopty started for a task, the project and the task.
///
/// An agent's `task_create` then makes a subtask of its own task in its own project, and its
/// `task_update` moves its own task, with nothing to name.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Scope {
    /// The terminal it runs in (`SLOPTY_SESSION`).
    pub session: Option<SessionId>,
    /// Its project ([`PROJECT_ENV`]).
    pub project: Option<ProjectId>,
    /// Its task ([`TASK_ENV`]).
    pub task: Option<TaskId>,
}

impl Scope {
    /// This process's, from its environment; a variable that does not parse is none.
    #[must_use]
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            session: var(slopty_proto::ctl::SESSION_ENV).and_then(|v| v.trim().parse().ok()),
            project: var(PROJECT_ENV).and_then(|v| v.trim().parse().ok()),
            task: var(TASK_ENV).and_then(|v| v.parse().ok()),
        }
    }
}

/// A verb that failed, or a name that did not resolve.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
#[error("{message} ({code:?})")]
pub struct ToolError {
    /// What kind of failure.
    pub code: ErrorCode,
    /// For a human or a model to read.
    pub message: String,
}

impl ToolError {
    /// An error with `code` and `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// An argument that does not fit.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Invalid, message)
    }

    /// An answer that is not the one the verb calls for, or an error answer.
    #[must_use]
    pub fn unexpected(outcome: Outcome) -> Self {
        match outcome {
            Outcome::Error { code, message } => Self { code, message },
            other => Self::new(
                ErrorCode::Failed,
                format!("the server answered with something else: {other:?}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_outcome_carries_its_message_and_code() {
        let e = ToolError::unexpected(Outcome::Error {
            code: ErrorCode::UnknownTerminal,
            message: "no such terminal".to_owned(),
        });
        assert_eq!(e.to_string(), "no such terminal (UnknownTerminal)");
        let e = ToolError::unexpected(Outcome::Done);
        assert_eq!(e.code, ErrorCode::Failed);
        assert!(e.message.contains("something else: Done"), "{e}");
    }
}
