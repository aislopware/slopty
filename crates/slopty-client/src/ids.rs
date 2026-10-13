//! How the client names a worker and an item on one: [`WorkerKey`] and [`ItemRef`].

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use slopty_core::ItemId;

/// An opaque worker identity: whatever the caller keys its workers by, squeezed into 128 bits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkerKey(u128);

impl WorkerKey {
    /// A key from its value.
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// The value.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

impl fmt::Display for WorkerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl fmt::Debug for WorkerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WorkerKey({self})")
    }
}

impl Serialize for WorkerKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkerKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        u128::from_str_radix(&text, 16).map(Self).map_err(serde::de::Error::custom)
    }
}

/// An item on a worker: a terminal, a remote window or a display, by the worker it lives on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ItemRef {
    /// The worker the item lives on.
    pub worker: WorkerKey,
    /// The item.
    pub item: ItemId,
}
