//! The workers this app reaches, from the server's directory or added by address: one direct
//! link each, all of them feeding one workspace.

use std::time::Duration;

use slopty_client::WorkerLink;
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
            /// Open the panel that connects to a server, whose directory lists the workers.
            ConnectServer,
            /// Stop using the server: its workers leave, the ones added by address stay.
            DisconnectServer,
        ]
    );
}

/// The workspace's key for a worker: its id's 128 bits.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// One worker as the app keeps it, beside what the workspace keeps.
pub struct WorkerSlot {
    /// Its identity (the directory's and the known-workers store's key).
    pub id: WorkerId,
    /// It was added by address, so it stays without the server.
    pub added: bool,
    /// Wakes its connect loop out of a wait: it came back online, or the server went away and
    /// its cached address is worth a try.
    pub wake: std::sync::Arc<tokio::sync::Notify>,
    /// The workspace's key for it.
    pub key: WorkerKey,
    /// Display name (from the store, refreshed by each `HelloAck`).
    pub name: String,
    /// The live link, to abandon it when the worker is forgotten.
    pub link: Option<std::sync::Weak<WorkerLink>>,
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
    /// A slot for a worker, before its first connection attempt.
    #[must_use]
    pub fn new(id: WorkerId, name: String, added: bool) -> Self {
        let wake = std::sync::Arc::default();
        Self { id, added, wake, key: worker_key(id), name, link: None }
    }

    /// Cut a wait short.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The direct link is up.
    #[must_use]
    pub fn linked(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.strong_count() > 0)
    }
}

/// Backoff between connection attempts: 1 s after a drop, doubling per failure, capped.
#[must_use]
pub fn retry_delay(failures: u32) -> Duration {
    Duration::from_secs((1_u64 << failures.min(4)).min(10))
}
