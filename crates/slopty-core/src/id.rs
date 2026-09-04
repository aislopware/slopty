//! Identifier newtypes.
//!
//! Durable identities (sessions, canvas items, hosts) are `UUIDv7` so they sort by creation time
//! and never collide across hosts. Connection-scoped identities (streams) are small integers
//! allocated by the host and only meaningful for one connection.

use core::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A fresh, time-ordered identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// The nil identifier; only valid as a placeholder in tests and defaults.
            #[must_use]
            pub const fn nil() -> Self {
                Self(Uuid::nil())
            }

            /// Wrap an existing UUID.
            #[must_use]
            pub const fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl core::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

uuid_id!(
    /// A terminal session (one PTY) on a host. Survives reconnects and host restarts.
    SessionId
);
uuid_id!(
    /// A connected client instance (one app process). Stable across reconnects of that process.
    ClientId
);
uuid_id!(
    /// A host machine. Derived from the host's iroh node identity at pairing time.
    HostId
);
uuid_id!(
    /// An item on the canvas (terminal tile, remote window tile, note, agent card).
    ItemId
);

/// A window on the host that can be streamed: the `CGWindowID`. macOS reuses these, so a
/// client always opens a stream against a listing it just received.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct WindowId(pub u32);

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "window#{}", self.0)
    }
}

/// A connection-scoped stream identity allocated by the host.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct StreamId(pub u32);

impl fmt::Display for StreamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stream#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_time_ordered() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert!(a < b, "v7 ids created in sequence must sort in creation order");
    }

    #[test]
    fn ids_round_trip_through_serde_as_strings() {
        let id = ItemId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert!(json.starts_with('"'), "transparent uuid serialises as a string");
        let back: ItemId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn ids_parse_from_display() {
        let id = HostId::new();
        let parsed: HostId = id.to_string().parse().unwrap();
        assert_eq!(id, parsed);
    }
}
