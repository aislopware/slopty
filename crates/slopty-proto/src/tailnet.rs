//! What the tailnet says of a link, as the worker's Tailscale sees it.

use serde::{Deserialize, Serialize};

/// How packets between a worker and a client travel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPath {
    /// Straight between the two machines over UDP.
    Direct,
    /// Through another node of the tailnet acting as a relay.
    PeerRelay,
    /// Through a Tailscale DERP server: TCP, and often a detour.
    Derp {
        /// The DERP region, e.g. `fra`.
        region: String,
    },
}
