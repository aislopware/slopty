//! What the tailnet says: of a link, as the worker's Tailscale sees it, and of the local
//! Tailscale itself.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long a link stays on DERP before it is said.
///
/// A path starts on DERP while a direct one is found, and a direct path whose pongs stopped
/// reads as DERP for a few seconds too (`docs/decisions/transport.md`, "A link that stays on
/// DERP says so").
pub const DERP_NOTICE_AFTER: Duration = Duration::from_secs(10);

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

impl LinkPath {
    /// Whether packets detour through a Tailscale DERP server.
    #[must_use]
    pub const fn relayed(&self) -> bool {
        matches!(self, Self::Derp { .. })
    }

    /// What a person reads about a link that stays on DERP, `None` for any other path.
    #[must_use]
    pub fn relay_note(&self) -> Option<String> {
        match self {
            Self::Derp { region } if region.is_empty() => {
                Some("Relayed through Tailscale — adds latency".to_owned())
            }
            Self::Derp { region } => Some(format!("Relayed via {region} — adds latency")),
            Self::Direct | Self::PeerRelay => None,
        }
    }

    /// The one-line fix for a link that stays on DERP: a peer relay on a machine that is
    /// always on, which Tailscale tries before DERP.
    #[must_use]
    pub const fn relay_fix() -> &'static str {
        "Run a Tailscale peer relay on the Slopty server: `slopty server relay`"
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Only DERP is relayed, and its note names the region when there is one.
    #[test]
    fn only_derp_says_it_is_relayed() {
        let derp = LinkPath::Derp { region: "fra".to_owned() };
        assert!(derp.relayed());
        assert_eq!(derp.relay_note().as_deref(), Some("Relayed via fra — adds latency"));
        let nowhere = LinkPath::Derp { region: String::new() };
        assert_eq!(
            nowhere.relay_note().as_deref(),
            Some("Relayed through Tailscale — adds latency")
        );
        for path in [LinkPath::Direct, LinkPath::PeerRelay] {
            assert!(!path.relayed());
            assert_eq!(path.relay_note(), None);
        }
    }
}
