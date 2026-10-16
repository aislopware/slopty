//! No file system events here: every repository is looked at every `POLL`.

use std::sync::Arc;

use super::Repo;

/// Never a watch.
#[derive(Debug)]
pub(super) struct Watch;

impl Watch {
    /// No events to follow `repo` by.
    pub(super) fn start(_repo: Arc<Repo>, _watches: usize) -> Option<Self> {
        None
    }
}
