//! An agent's thread drawn from this client's mirror of it: the face for every agent, built on
//! the agent-neutral thread model (`slopty_proto::thread`) and kept in step by
//! `slopty_client::threads`.
//!
//! * [`hub`] — one worker's threads as an entity: fed by the link, kept on disk, and the one way a
//!   view's intents reach the worker.
//! * [`rows`] — a thread as the list's rows: settled turns folded, the live one whole.
//! * [`activity`] — what the bar over the composer stacks.
//! * [`questions`] — an agent's questions as a questionnaire, and the answer that goes back.
//! * [`view`] — the thread view itself.
//!
//! It stands beside the conversation face ([`super::ConversationView`]) until the old path
//! goes.

pub mod activity;
#[cfg(test)]
pub(crate) mod fixtures;
pub mod hub;
pub mod questions;
pub mod rows;
pub mod view;

pub use hub::{HubEvent, ThreadHub};
pub use view::{ThreadView, ThreadViewEvent};

#[cfg(test)]
mod tests;
