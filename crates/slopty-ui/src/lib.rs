//! GPUI views and elements around the terminal (`slopty-terminal`), and the model that feeds
//! the shell (`slopty-shell`).
//!
//! * [`workspace`] — every worker this client reaches: links, threads, terminals, screens.
//! * [`view_model`] — what feeds the shell from the server and the workers.
//! * [`screen`] — a remote window or display painted from decoded frames, with input forwarding.
//! * [`clipboard`] — the clipboard shared with the workers: announced, fetched on paste, echoes
//!   broken.
//! * [`conversation`] — a thread's model: its transcript, turns and approvals.
//! * [`key_takers`] — Tab, ⇧Tab and ⌃C given back to a terminal's program and a remote window.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod clipboard;
pub mod conversation;
pub mod key_takers;
mod outbox;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod screen;
pub mod view_model;
#[expect(
    unreachable_pub,
    reason = "the streaming work owns `screen` and the tile code; narrowed once it lands"
)]
pub mod workspace;
