//! Identifiers, clocks and small shared types.
//!
//! This crate has no platform code and no I/O. Everything above it (protocol, grid, host, client)
//! agrees on these types, so they are deliberately few and boring.

mod id;
mod time;

pub use id::{ClientId, HostId, ItemId, SessionId, StreamId, WindowId};
pub use time::{Duration, MonoTime};
