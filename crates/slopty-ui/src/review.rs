//! The review tile: what an agent's thread changed in its working tree, read file by file.
//!
//! On the left, the files by weight, tests, fixtures, locks and generated code quieter below;
//! along the top, the span it covers (the last turn, since the person last reviewed, every
//! turn); in the middle, one diff of every file, in a column, side by side from 960 pt. Each
//! hunk and each file is kept or put back; a click on a line comments on it, and the comments
//! go to the agent as one message. "Mark reviewed" keeps everything shown, so what changes after
//! is what "Since reviewed" shows.
//!
//! It reads the worker's review frames through the thread's hub, and what the person does goes
//! through the hub's outbox like any other intent.
//!
//! * [`findings`] — what an agent's own review found, read from its answer.
//! * [`model`] — the files in order, the comments, and the picks; nothing draws.
//! * [`view`] — the tile, [`ReviewView`], for the strip to host.

pub mod findings;
pub mod model;
pub mod view;

pub use model::Scope;
pub use view::{ReviewEvent, ReviewView};

#[cfg(test)]
mod tests;
