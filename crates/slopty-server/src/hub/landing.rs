//! A finished goal's pull request, followed on the forge (`docs/decisions/projects.md`, "A
//! landed goal's pull request is followed on every device").
//!
//! [`Verb::ProjectLand`] opens it and keeps it on the project ([`Project::landed`]). While it
//! is open the server reads it again through the orchestrator's worker ([`Verb::PullOf`], the
//! person's own `gh` or `glab`): every [`LIVELY`] while its checks run, every [`SETTLED`] once
//! they rest, and no more once it merged or closed. What changed reaches every device with the
//! project, and the timeline says when it comes to stand otherwise, its merge among it.

use std::collections::HashMap;
use std::time::Duration;

use slopty_core::{WallMs, WorkerId};
use slopty_proto::git::Forge;
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::project::{Project, ProjectId};
use slopty_proto::thread::wire::{PullSeen, PullStands};
use tokio::time::Instant;

use super::steps::orchestrator_clone;
use super::{Hub, WeakHub};

/// How often the open goals' pull requests are looked at.
const TICK: Duration = Duration::from_secs(15);
/// How soon one is read again while its checks run: a failed check reaches the person within
/// the minute.
const LIVELY: Duration = Duration::from_secs(60);
/// How soon once its checks rest, or the forge could not say.
const SETTLED: Duration = Duration::from_mins(5);

/// Where an open goal's pull request is read: the project, the orchestrator's worker and
/// clone, the goal's branch, and whether its checks still run.
pub(super) struct Watched {
    project: ProjectId,
    worker: WorkerId,
    repo: String,
    branch: String,
    lively: bool,
}

impl Watched {
    /// Whether its checks still run, so it is read again soon.
    #[must_use]
    pub(super) const fn lively(&self) -> bool {
        self.lively
    }
}

impl Hub {
    /// Read the open goals' pull requests again as they fall due, until the hub is gone.
    pub async fn watch_landings(hub: WeakHub) {
        let mut due: HashMap<ProjectId, Instant> = HashMap::new();
        loop {
            tokio::time::sleep(TICK).await;
            let Some(hub) = hub.upgrade() else { return };
            let now = Instant::now();
            let open = hub.landings();
            due.retain(|project, _| open.iter().any(|w| w.project == *project));
            for watched in open {
                if due.get(&watched.project).is_some_and(|at| *at > now) {
                    continue;
                }
                let pace = if watched.lively() { LIVELY } else { SETTLED };
                due.insert(watched.project.clone(), now.checked_add(pace).unwrap_or(now));
                let Watched { project, worker, repo, branch, .. } = watched;
                hub.read_landed(&project, worker, repo, branch).await;
            }
        }
    }

    /// The goals whose pull request is open, where the orchestrator's worker is linked.
    pub(super) fn landings(&self) -> Vec<Watched> {
        let state = self.inner.state.lock();
        state
            .projects
            .landing()
            .into_iter()
            .filter_map(|project| {
                let (worker, repo) = orchestrator_clone(&state, &project).ok().flatten()?;
                state.workers.get(&worker)?.link.as_ref()?;
                let lively = project.landed.as_ref().is_some_and(|p| p.running > 0);
                Some(Watched {
                    branch: project.target.clone(),
                    project: project.id,
                    worker,
                    repo,
                    lively,
                })
            })
            .collect()
    }

    /// Read `project`'s goal's pull request, of `branch` in the clone `repo` on `worker`, and
    /// keep what the forge says. `None` when the forge said nothing of one.
    pub(super) async fn read_landed(
        &self,
        project: &ProjectId,
        worker: WorkerId,
        repo: String,
        branch: String,
    ) -> Option<PullSeen> {
        let read = self.forward(None, Verb::PullOf { worker, repo, branch }).await;
        let Outcome::PullOf { pull: Some(pull) } = read else {
            tracing::debug!(%project, ?read, "the goal's pull request did not read");
            return None;
        };
        let mut state = self.inner.state.lock();
        let changes = state.projects.landed_seen(project, pull.clone(), WallMs::now());
        self.projects_moved(&mut state, changes);
        drop(state);
        Some(pull)
    }
}

/// What a goal's pull request is known as before the forge is read: what opening it said, its
/// forge read off the project's `origin`.
pub(super) fn opened(record: &Project, (number, url): (u32, &str), base: &str) -> PullSeen {
    let origin = record.repo_id.as_ref().and_then(|id| id.origin.as_deref()).unwrap_or(url);
    PullSeen {
        forge: if origin.contains("gitlab") { Forge::GitLab } else { Forge::GitHub },
        number,
        url: url.to_owned(),
        title: record.title.clone(),
        base: base.to_owned(),
        stands: PullStands::Waiting,
        failed: 0,
        failed_first: None,
        running: 0,
    }
}
