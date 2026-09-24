//! The workers this app has added: one link each, all of them feeding one workspace.

use std::time::Duration;

use slopty_client::HostLink;
use slopty_client::layout::WorkerKey;
use slopty_core::WorkerId;

/// GPUI actions for the workers.
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        workers,
        [
            /// Open the panel that adds a worker by address.
            AddWorker,
        ]
    );
}

/// The workspace's key for a worker: its id's 128 bits.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// One added worker as the app keeps it, beside what the workspace keeps.
pub struct WorkerSlot {
    /// Its identity (the known-workers store's key).
    pub id: WorkerId,
    /// The workspace's key for it.
    pub key: WorkerKey,
    /// Display name (from the store, refreshed by each `HelloAck`).
    pub name: String,
    /// The live link, to abandon it when the worker is forgotten.
    pub link: Option<std::sync::Weak<HostLink>>,
}

impl std::fmt::Debug for WorkerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerSlot")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl WorkerSlot {
    /// A slot for an added worker, before its first connection attempt.
    #[must_use]
    pub const fn new(id: WorkerId, name: String) -> Self {
        Self { id, key: worker_key(id), name, link: None }
    }
}

/// Backoff between connection attempts: 1 s after a drop, doubling per failure, capped.
#[must_use]
pub fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((1_u64 << failures.min(4)).min(10))
}
