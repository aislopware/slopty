//! The thread host on the daemon (`slopty_worker::thread`): every Claude Code session the
//! daemon sees, observed into the agent-neutral thread model beside today's conversation path.

use std::path::Path;
use std::sync::Arc;

use slopty_core::SessionId;
use slopty_worker::conversation::Seen;
use slopty_worker::orchestrate::{self, Conversations as _};
use slopty_worker::thread::Host;
use slopty_worker::thread::claude::{self, Sources};
use tokio::sync::watch;

use crate::Daemon;
use crate::follow::Orchestrated;

/// What an observed session needs of the daemon.
struct Observed(Daemon);

impl Sources for Observed {
    fn sources(&self, session: SessionId) -> orchestrate::Sources {
        Orchestrated(self.0.clone()).sources(session)
    }

    fn seen(&self, session: SessionId) -> watch::Receiver<Seen> {
        self.0.follows.lock().board.watch(session)
    }
}

/// Host the threads kept under `dir` and observe every Claude Code session into them. A host
/// that cannot open is warned of, and the daemon goes on without it.
pub fn start(daemon: &Daemon, dir: &Path) {
    match Host::open(dir, slopty_worker::thread::log::Limits::default()) {
        Ok(host) => {
            let sources: Arc<dyn Sources> = Arc::new(Observed(daemon.clone()));
            drop(claude::spawn(host, daemon.events.subscribe(), sources));
        }
        Err(e) => tracing::warn!(dir = %dir.display(), "the thread host did not open: {e}"),
    }
}
