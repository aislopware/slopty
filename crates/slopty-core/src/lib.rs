//! Identifiers, clocks and small shared types.
//!
//! This crate has no platform code and no I/O. Everything above it (protocol, grid, worker, client)
//! agrees on these types, so they are deliberately few and boring.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod id;
mod shell;
mod time;

pub use id::{ClientId, DisplayId, ItemId, SessionId, StreamId, WindowId, WorkerId, XferId};
pub use shell::shell_quote;
pub use time::{Duration, MonoTime, WallMs};
