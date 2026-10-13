//! What is asked of a worker on the shell's behalf: whether the worker can hear it now.

use gpui::Context;
use slopty_client::WorkerKey;

use super::WorkspaceView;

impl WorkspaceView {
    /// Whether `key` is linked, so what is asked of it now is heard; one out of reach is said in
    /// the log, naming `what` would have opened, and the caller answers the shell with it.
    pub(super) fn reachable_for(&self, key: WorkerKey, what: &str, _cx: &Context<Self>) -> bool {
        let Some(w) = self.workers.get(&key) else { return false };
        if w.link.is_some() {
            return true;
        }
        tracing::info!(worker = %w.name, status = %w.status.text(), "{what} did not open");
        false
    }
}
