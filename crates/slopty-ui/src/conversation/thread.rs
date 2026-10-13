//! An agent's thread as this client mirrors it, built on the agent-neutral thread model
//! (`slopty_proto::thread`) and kept in step by `slopty_client::threads`.
//!
//! * [`hub`] — one worker's threads as an entity: fed by the link, kept on disk, and the one way
//!   the shell's intents reach the worker.
//! * [`git`] — a repository's git as a thread works it: the pull request's state, numbered ops and
//!   their answers.

#[cfg(test)]
pub(crate) mod fixtures;
pub mod git;
pub mod hub;

pub use hub::{HubEvent, ThreadHub};
