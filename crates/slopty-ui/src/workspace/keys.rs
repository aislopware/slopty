//! How the workspace keys workers: a worker's key from its server id and back.

use slopty_client::WorkerKey;
use slopty_core::WorkerId;

/// `id` as the workspace keys workers: the one the app gives the server's worker ids.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// The worker id behind `key`: [`worker_key`] the other way. A UUID's simple form is its
/// 128 bits in hex, which is all a key holds.
pub(super) fn worker_id(key: WorkerKey) -> Option<WorkerId> {
    format!("{:032x}", key.value()).parse().ok()
}
