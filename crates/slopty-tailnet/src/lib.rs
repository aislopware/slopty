//! The local Tailscale daemon, read through its `LocalAPI`: which peers the tailnet has and how
//! each is reached, and who is behind an address that calls in.
//!
//! Slopty rides the system Tailscale client (any macOS variant, or the Tailscale app on iOS),
//! against Tailscale's control plane or a compatible one (Headscale, slopscale). Nothing here
//! joins a tailnet or carries a packet: the packets go over the OS tunnel as they always did.
//! What the daemon adds is what the tunnel cannot say:
//!
//! * [`locate`] — where this machine's `LocalAPI` listens, for each way Tailscale runs.
//! * [`LocalApi`] — `status`, `whois` and `ping` over it.
//! * [`status`] — the tailnet as the daemon sees it, and the path to each peer.
//! * [`whois`] — the node and user behind an address, with the grants it carries.
//! * [`policy`] — which of those Slopty lets in, and in what role.
//! * [`lan`] — the LAN beneath the tailnet: this machine's interfaces on it, and the magic packet
//!   that wakes a machine asleep there, which the tailnet cannot reach.
//!
//! iOS has no `LocalAPI` a third-party app can reach; an iPhone gets its workers from the Slopty
//! server and dials them over the tunnel like any other address.

// `lan` alone reads `getifaddrs(3)`; everything else stays safe.
#![deny(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod api;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod lan;
pub mod locate;
pub mod policy;
pub mod status;
pub mod whois;

pub use api::{LocalApi, LocalApiError, Pong};
pub use locate::Location;
pub use policy::{Grant, Role};
pub use status::{BackendState, Node, Path, Status};
pub use whois::WhoIs;

/// A field Go's JSON writes as `null` for an empty collection reads as the empty value.
fn null_as_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    Ok(<Option<T> as serde::Deserialize>::deserialize(de)?.unwrap_or_default())
}
