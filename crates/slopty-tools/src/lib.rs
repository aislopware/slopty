//! The orchestration verbs as one contract, whatever carries them (`docs/decisions/topology.md`).
//!
//! The server's MCP endpoint, `slopty mcp` and the `slopty` CLI all resolve the same names,
//! print the same JSON and list the same tools, because they all run this crate over their own
//! [`Dispatch`]: the server's hub answers verbs in-process, the CLI sends them down its link.
//!
//! * [`resolve`]: worker names and id prefixes, `worker/session` handles, to ids.
//! * [`ops`]: each verb once, resolved, sent, and its one expected answer taken apart.
//! * [`view`]: the answers as JSON for scripts and models, and as text for a person.
//! * [`tools`]: the MCP tools, their schemas, and a call run end to end.

#![forbid(unsafe_code)]

pub mod ops;
pub mod resolve;
pub mod tools;
pub mod view;

use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};

/// Where verbs go: something that answers each with its [`Outcome`], a failure included.
///
/// Calls may be in flight together; each answer finds its own caller.
pub trait Dispatch: Send + Sync {
    /// Answer `verb`.
    fn call(&self, verb: Verb) -> impl Future<Output = Outcome> + Send;
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
