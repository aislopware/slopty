//! Connection establishment.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, HostId};

use crate::terminal::SessionSummary;

/// What kind of client is connecting; hosts use it for defaults (e.g. touch-sized hit targets).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClientKind {
    /// The macOS app.
    Mac,
    /// The iPad app.
    IPad,
    /// The iPhone app.
    IPhone,
    /// A headless tool (`slopty` CLI, tests).
    Tool,
}

bitflags! {
    /// Optional features. A host never uses a capability the client did not announce.
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Caps: u32 {
        /// Can decode HEVC Main.
        const HEVC = 1 << 0;
        /// Can decode HEVC Main10 (HDR).
        const HEVC_MAIN10 = 1 << 1;
        /// Can decode H.264 (fallback only).
        const H264 = 1 << 2;
        /// Can play Opus audio.
        const OPUS = 1 << 3;
        /// Accepts kitty graphics image payloads on session streams.
        const KITTY_GRAPHICS = 1 << 4;
        /// Runs client-side prediction and wants `input_ack` in frames.
        const PREDICTION = 1 << 5;
    }
}

/// First message from a client.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Hello {
    /// Must equal [`crate::PROTOCOL_VERSION`].
    pub protocol: u16,
    /// Stable identity of this client installation.
    pub client: ClientId,
    /// Kind.
    pub kind: ClientKind,
    /// Human-readable name shown on the host ("Cong's iPad").
    pub name: String,
    /// App version string.
    pub app_version: String,
    /// Features.
    pub caps: Caps,
    /// One-time pairing token from the host's pairing ticket. Present on the first connection
    /// only; afterwards the transport identity (the client's endpoint key) is the credential.
    pub pair_token: Option<[u8; 32]>,
}

/// Host's acceptance.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HelloAck {
    /// Host protocol version (equals the client's, or `Rejected` was sent instead).
    pub protocol: u16,
    /// Host identity.
    pub host: HostId,
    /// Host name ("mac-studio").
    pub name: String,
    /// Host app version.
    pub app_version: String,
    /// Host features.
    pub caps: Caps,
    /// Sessions currently alive on the host, so the client can reattach immediately.
    pub sessions: Vec<SessionSummary>,
}

/// Why a `Hello` was refused.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Rejection {
    /// Protocol mismatch.
    ProtocolVersion {
        /// What the host speaks.
        host: u16,
    },
    /// The client is not paired with this host.
    NotPaired,
    /// Too many clients.
    Busy,
}
