//! An agent's conversation as this client mirrors it: each worker's threads, over the
//! agent-neutral thread model (`slopty_proto::thread`), kept in step by
//! `slopty_client::threads`.
//!
//! * [`thread`] — each worker's thread hub, and its repositories' git.
//! * [`figures`] — a model's name as a person says it, and times of day.

pub mod figures;
pub mod thread;
