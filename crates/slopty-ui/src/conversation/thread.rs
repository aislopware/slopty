//! An agent's thread drawn from this client's mirror of it: the face for every agent, built on
//! the agent-neutral thread model (`slopty_proto::thread`) and kept in step by
//! `slopty_client::threads`.
//!
//! * [`commit`] — the commit sheet over a thread's or a review's tile.
//! * [`draft`] — a thread not started yet, whose composer writes its first message.
//! * [`git`] — a repository's git as a tile works it: the commit sheet's and the pull request's
//!   state, numbered ops and their answers.
//! * [`hub`] — one worker's threads as an entity: fed by the link, kept on disk, and the one way a
//!   view's intents reach the worker.
//! * [`rows`] — a thread as the list's rows: settled turns folded, the live one whole.
//! * [`activity`] — what the bar over the composer stacks.
//! * [`questions`] — an agent's questions as a questionnaire, and the answer that goes back.
//! * [`view`] — the thread view itself.

pub mod activity;
pub mod commit;
pub mod draft;
pub mod find;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod git;
pub mod hub;
pub mod questions;
pub mod rows;
pub mod view;

pub use draft::{Draft, DraftSent};
pub use hub::{HubEvent, ThreadHub};
pub use view::{ThreadView, ThreadViewEvent};

#[cfg(test)]
mod tests;
