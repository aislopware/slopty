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

pub mod bulk;
pub mod mcp;
pub mod ops;
pub mod resolve;
pub mod tools;
pub mod view;

use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};

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

    /// Answer `verb` as part of a change: when the server cannot be reached at all, a carrier
    /// that can wait it out does, for the lookups before the change as for the change itself,
    /// so a change made off the network goes through once it is back. The same as
    /// [`Self::send`] for a carrier that cannot wait.
    fn send_waiting(
        &self,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> impl Future<Output = Outcome> + Send {
        self.send(key, verb)
    }
}

/// `D`, every verb sent as part of a change ([`Dispatch::send_waiting`]): what a tool that
/// changes something runs over.
#[derive(Debug)]
pub struct Waiting<'a, D>(pub &'a D);

impl<D: Dispatch> Dispatch for Waiting<'_, D> {
    fn send(
        &self,
        key: Option<IdempotencyKey>,
        verb: Verb,
    ) -> impl Future<Output = Outcome> + Send {
        self.0.send_waiting(key, verb)
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
