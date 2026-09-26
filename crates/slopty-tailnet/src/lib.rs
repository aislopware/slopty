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
//!
//! iOS has no `LocalAPI` a third-party app can reach; an iPhone gets its workers from the Slopty
//! server and dials them over the tunnel like any other address.

#![forbid(unsafe_code)]

mod api;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod locate;
pub mod policy;
pub mod status;
pub mod whois;

pub use api::{LocalApi, LocalApiError, Pong};
pub use locate::Location;
pub use policy::{Grant, Role};
pub use status::{Node, Path, Status};
pub use whois::WhoIs;
