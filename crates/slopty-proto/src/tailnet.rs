//! What the tailnet says: of a link, as the worker's Tailscale sees it, and of the local
//! Tailscale itself.

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

/// Where a Tailscale daemon stands: its backend state (`ipn.State`), by its own name, as its
/// `LocalAPI` spells it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BackendState {
    /// Just started, with no state yet.
    NoState,
    /// Another user of the machine owns the daemon.
    InUseOtherUser,
    /// Signed out: the node must log in.
    NeedsLogin,
    /// Logged in, waiting for an admin to approve the machine.
    NeedsMachineAuth,
    /// Turned off by its user.
    Stopped,
    /// Connecting.
    Starting,
    /// Logged in and up.
    Running,
    /// A state this build does not know.
    #[serde(other)]
    Other,
}

impl std::fmt::Display for BackendState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoState => "NoState",
            Self::InUseOtherUser => "InUseOtherUser",
            Self::NeedsLogin => "NeedsLogin",
            Self::NeedsMachineAuth => "NeedsMachineAuth",
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Running => "Running",
            Self::Other => "an unknown state",
        })
    }
}
