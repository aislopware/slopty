//! What a project let go leaves on its workers goes, even from a worker that is away then
//! (`docs/decisions/projects.md`, "Letting a project go cleans up on every machine").
//!
//! Each removal is kept by the store until its worker answers it, whatever the answer: gone,
//! kept for work not committed, or not there. One the worker could not be reached for stays
//! and is asked again as the worker registers, after a restart of the server too.

use slopty_core::WorkerId;

use super::queue::unreachable;
use super::{Hub, State, projects};
use crate::project::{Cleanup, Keep};

/// The cleanups waiting for their worker's answer, and whether each is being asked now.
#[derive(Debug, Default)]
pub(super) struct Cleanups {
    waiting: Vec<(Cleanup, bool)>,
}

impl Cleanups {
    /// Take up the cleanups a store kept: each is asked once its worker registers.
    pub(super) fn adopt(&mut self, kept: impl IntoIterator<Item = Cleanup>) {
        for cleanup in kept {
            if !self.waiting.iter().any(|(c, _)| *c == cleanup) {
                self.waiting.push((cleanup, false));
            }
        }
    }

    /// Every cleanup waiting, for the store's snapshot.
    pub(super) fn kept(&self) -> Vec<Cleanup> {
        self.waiting.iter().map(|(c, _)| c.clone()).collect()
    }

    /// Those for `worker` not being asked already, now marked as being asked.
    fn take_for(&mut self, worker: WorkerId) -> Vec<Cleanup> {
        self.waiting
            .iter_mut()
            .filter(|(c, asking)| c.worker() == worker && !*asking)
            .map(|(c, asking)| {
                *asking = true;
                c.clone()
            })
            .collect()
    }
}

impl Hub {
    /// Remove `cleanups` from their workers, each kept until its worker answers.
    pub(super) fn clean_up(&self, state: &mut State, cleanups: Vec<Cleanup>) {
        let mut workers = Vec::new();
        for cleanup in cleanups {
            if state.cleanups.waiting.iter().any(|(c, _)| *c == cleanup) {
                continue;
            }
            projects::keep(state, Keep::Cleanup { cleanup: cleanup.clone(), waits: true });
            if !workers.contains(&cleanup.worker()) {
                workers.push(cleanup.worker());
            }
            state.cleanups.waiting.push((cleanup, false));
        }
        for worker in workers {
            self.clean_up_on(state, worker);
        }
    }

    /// The machine of worker `old` came back as `worker`: what waited for it waits for the new
    /// id, as the paths are the same machine's.
    pub(super) fn cleanups_moved(state: &mut State, old: WorkerId, worker: WorkerId) {
        let moved: Vec<Cleanup> = state
            .cleanups
            .waiting
            .extract_if(.., |(c, _)| c.worker() == old)
            .map(|(c, _)| c)
            .collect();
        for cleanup in moved {
            projects::keep(state, Keep::Cleanup { cleanup: cleanup.clone(), waits: false });
            let cleanup = cleanup.on(worker);
            projects::keep(state, Keep::Cleanup { cleanup: cleanup.clone(), waits: true });
            state.cleanups.waiting.push((cleanup, false));
        }
    }

    /// Ask `worker` for what waits for it, when it is linked: called as it registers.
    pub(super) fn clean_up_on(&self, state: &mut State, worker: WorkerId) {
        if tokio::runtime::Handle::try_current().is_err()
            || state.workers.get(&worker).is_none_or(|e| e.link.is_none())
        {
            return;
        }
        let asked = state.cleanups.take_for(worker);
        if asked.is_empty() {
            return;
        }
        let hub = self.clone();
        tokio::spawn(async move {
            for cleanup in asked {
                let went = hub.forward(None, cleanup.verb()).await;
                let mut state = hub.inner.state.lock();
                let at = state.cleanups.waiting.iter().position(|(c, _)| *c == cleanup);
                if unreachable(&went) {
                    tracing::info!(%worker, ?cleanup, "a cleanup waits for its worker");
                    if let Some((_, asking)) = at.and_then(|i| state.cleanups.waiting.get_mut(i)) {
                        *asking = false;
                    }
                    continue;
                }
                tracing::info!(%worker, ?cleanup, ?went, "a let-go project's cleanup");
                if let Some(i) = at {
                    state.cleanups.waiting.remove(i);
                    projects::keep(&mut state, Keep::Cleanup { cleanup, waits: false });
                }
                drop(state);
            }
        });
    }
}
