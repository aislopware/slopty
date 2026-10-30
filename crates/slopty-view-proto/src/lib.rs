// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! The wire types the shell and its view model share.
//!
//! Devices, projects, chats and sessions with what the shell asks of a machine's files, git
//! and terminals ([`entities`]), what an agent does ([`agent`]), and the derivations every
//! surface draws alike ([`view`]).

pub mod agent;
pub mod entities;
pub mod motion;
pub mod preview;
pub mod projectless;
pub mod view;
pub mod workspace;

pub use agent::*;
pub use entities::*;
pub use preview::*;
pub use workspace::*;

/// Parse "0.2.12" (tolerating a `-suffix`/`+build` tail on the last part) into a triple.
///
/// Comparable, so a device's `Device::version` can be gated on. `None` for anything that
/// doesn't lead with three dotted integers, which gates treat as too old.
pub fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().splitn(3, '.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next()?.parse().ok()?;
    let patch = parts.next()?;
    let patch: u64 = patch.split(['-', '+']).next().unwrap_or(patch).parse().ok()?;
    Some((major, minor, patch))
}
