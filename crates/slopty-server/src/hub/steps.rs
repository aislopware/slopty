//! What the server does for a task around its agent, each shown as the task's step
//! ([`TaskStep`]) so no wait is silent: a clone made on the worker a task is placed on when
//! that worker has none of the project's repository, and the task's branch brought to the
//! orchestrator's clone once it is done.
//!
//! The branch comes as a git bundle of its own commits, read in parts from the worker it ran
//! on and uploaded into the orchestrator's worker, which fetches it as
//! [`Task::home_branch`] (`docs/decisions/projects.md`). Merging it stays with the
//! orchestrator.

use std::collections::HashMap;

use slopty_core::{WallMs, WorkerId, XferId};
use slopty_proto::orchestration::{BUNDLES, BranchBundle, ErrorCode, Outcome, UploadPart, Verb};
use slopty_proto::project::{ProjectId, StepKind, StepState, Task, TaskId, TaskStep};
use slopty_proto::terminal::RepoId;

use super::{Hub, State};

/// The most bytes of a bundle read or uploaded in one request.
const PART: u64 = 4 << 20;

/// The steps under way, and the clones the server had made.
#[derive(Debug, Default)]
pub(super) struct Steps {
    next_clone: u64,
    /// Each clone under way, by the number its progress names: the task it is for.
    cloning: HashMap<u64, (ProjectId, TaskId, WorkerId)>,
    /// The clones made, by worker: their paths and repositories.
    made: HashMap<WorkerId, Vec<(String, RepoId)>>,
    /// The tasks whose branch is on its way home, one trip each at a time; `true` when it was
    /// reported done again meanwhile, so the trip goes once more for the newer commits.
    homing: HashMap<(ProjectId, TaskId), bool>,
}

impl Steps {
    /// The clones the server had made on `worker`: a worker's `repos` fact holds them beside
    /// its shells' (`facts_of`).
    pub(super) fn made_on(&self, worker: WorkerId) -> &[(String, RepoId)] {
        self.made.get(&worker).map_or(&[], Vec::as_slice)
    }
}

/// What a forwarded verb that failed said.
fn said(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Error { message, .. } => message.clone(),
        other => format!("an unexpected answer: {other:?}"),
    }
}

impl Hub {
    /// Move `task`'s step on, and push the change.
    fn step(&self, state: &mut State, (project, task): (&ProjectId, TaskId), step: TaskStep) {
        match state.projects.set_step(project, task, step, WallMs::now()) {
            Ok(updates) => self.projects_moved(state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "a step for no task"),
        }
    }

    /// [`Self::step`] to `now`, from its own lock.
    fn step_now(
        &self,
        (project, task): (&ProjectId, TaskId),
        kind: StepKind,
        worker: WorkerId,
        now: StepState,
    ) {
        let step = TaskStep { kind, worker, state: now, since_ms: WallMs::now() };
        self.step(&mut self.inner.state.lock(), (project, task), step);
    }

    /// Clone `url` onto `worker` for `task`, showing how it goes: the clone's path there.
    ///
    /// # Errors
    /// Why the clone failed, as the step says it too.
    pub(super) async fn clone_for(
        &self,
        (project, task): (&ProjectId, TaskId),
        worker: WorkerId,
        url: String,
    ) -> Result<String, String> {
        let clone = {
            let mut state = self.inner.state.lock();
            state.steps.next_clone = state.steps.next_clone.wrapping_add(1);
            let clone = state.steps.next_clone;
            state.steps.cloning.insert(clone, (project.clone(), task, worker));
            clone
        };
        let starting = StepState::Running { phase: "Starting".to_owned(), percent: None };
        self.step_now((project, task), StepKind::Clone, worker, starting);
        let outcome = self.forward(None, Verb::CloneRepo { worker, url, clone }).await;
        let mut state = self.inner.state.lock();
        state.steps.cloning.remove(&clone);
        let (ended, cloned) = match outcome {
            Outcome::Cloned { path, repo } => {
                let made = state.steps.made.entry(worker).or_default();
                made.retain(|(at, _)| *at != path);
                made.push((path.clone(), repo));
                (StepState::Done { detail: path.clone() }, Ok(path))
            }
            other => {
                let why = said(&other);
                (StepState::Failed { why: why.clone() }, Err(why))
            }
        };
        let step =
            TaskStep { kind: StepKind::Clone, worker, state: ended, since_ms: WallMs::now() };
        self.step(&mut state, (project, task), step);
        drop(state);
        cloned
    }

    /// How far clone `clone` has come, from its worker: its task's card shows it, in steps of
    /// five percent so a clone does not flood every client.
    pub(super) fn clone_moved(
        &self,
        state: &mut State,
        clone: u64,
        phase: String,
        percent: Option<u8>,
    ) {
        let Some((project, task, worker)) = state.steps.cloning.get(&clone).cloned() else {
            return;
        };
        let shown = state.projects.task(&project, task).ok().and_then(|t| match &t.step {
            Some(TaskStep { state: StepState::Running { phase, percent }, .. }) => {
                Some((phase.clone(), *percent))
            }
            _ => None,
        });
        let bucket = |p: Option<u8>| p.map(|p| p.min(100) / 5);
        if let Some((was, at)) = shown
            && was == phase
            && bucket(at) == bucket(percent)
        {
            return;
        }
        let step = TaskStep {
            kind: StepKind::Clone,
            worker,
            state: StepState::Running { phase, percent },
            since_ms: WallMs::now(),
        };
        self.step(state, (&project, task), step);
    }

    /// Bring `task`'s branch to its project's orchestrator's clone, in the background, once
    /// its agent says it is done. A trip under way goes again once it ends, for what the
    /// agent committed since.
    pub(super) fn bring_home_soon(
        &self,
        state: &mut State,
        (project, task): (ProjectId, TaskId),
        branch: Option<String>,
    ) {
        let key = (project, task);
        if let Some(again) = state.steps.homing.get_mut(&key) {
            *again = true;
            return;
        }
        state.steps.homing.insert(key.clone(), false);
        let hub = self.clone();
        tokio::spawn(async move {
            let (project, task) = &key;
            loop {
                hub.bring_home(project, *task, branch.clone()).await;
                let mut state = hub.inner.state.lock();
                if state.steps.homing.remove(&key) != Some(true) {
                    break;
                }
                state.steps.homing.insert(key.clone(), false);
            }
        });
    }

    /// Bring `task`'s branch (`branch`, else the one its card names) from the worker it ran on
    /// to the clone its orchestrator works in, as [`Task::home_branch`]. Nothing to do, and no
    /// step, when it ran in that clone or a worktree of it, or names no branch.
    async fn bring_home(&self, project: &ProjectId, task: TaskId, branch: Option<String>) {
        let at = (project, task);
        let trip = match self.trip(project, task, branch) {
            Ok(Some(trip)) => trip,
            Ok(None) => return,
            Err((worker, why)) => {
                self.step_now(at, StepKind::Home, worker, StepState::Failed { why });
                return;
            }
        };
        let to = trip.to.0;
        let packing = StepState::Running { phase: "Bundling".to_owned(), percent: None };
        self.step_now(at, StepKind::Home, to, packing);
        let carried = match self.carry(at, &trip, Some(trip.target.clone())).await {
            Err(Carry::Lacking) => self.carry(at, &trip, None).await,
            other => other,
        };
        let ended = match carried {
            Ok(detail) => StepState::Done { detail },
            Err(Carry::Lacking) => StepState::Failed {
                why: "the orchestrator's clone lacks the branch's history".to_owned(),
            },
            Err(Carry::Failed(why)) => StepState::Failed { why },
        };
        self.step_now(at, StepKind::Home, to, ended);
    }

    /// Where `task`'s branch is and where it goes. `None` when there is nothing to carry: no
    /// branch, no agent, no orchestrator, or the branch is in the orchestrator's clone
    /// already. An error, with the worker it would go to, when it should go and cannot.
    fn trip(
        &self,
        project: &ProjectId,
        task: TaskId,
        branch: Option<String>,
    ) -> Result<Option<Trip>, (WorkerId, String)> {
        let state = self.inner.state.lock();
        let (Ok(record), Ok(card)) =
            (state.projects.project(project), state.projects.task(project, task))
        else {
            return Ok(None);
        };
        let (Some(id), Some(orchestrator)) = (record.repo_id.as_ref(), record.orchestrator) else {
            return Ok(None);
        };
        let (Some(branch), Some(assigned)) =
            (branch.or_else(|| card.branch.clone()), card.assignment.as_ref())
        else {
            return Ok(None);
        };
        let from = assigned.term;
        let to = orchestrator.worker;
        let session_repo = |term: slopty_proto::orchestration::TermRef| {
            let entry = state.workers.get(&term.worker)?;
            let s = entry.sessions.iter().find(|s| s.id == term.session)?;
            s.repo_id.as_ref().is_some_and(|other| other.same(id)).then(|| s.repo.clone())?
        };
        // The orchestrator's own checkout while it is still in the project's repository, else
        // any clone of it on that worker.
        let Some(to_repo) =
            session_repo(orchestrator).or_else(|| super::projects::clone_on(&state, record, to))
        else {
            let why = "the orchestrator's worker has no clone of the project's repository";
            return Err((to, why.to_owned()));
        };
        let Some(from_repo) = card
            .worktree
            .clone()
            .or_else(|| session_repo(from))
            .or_else(|| super::projects::clone_on(&state, record, from.worker))
        else {
            let why = format!("no clone of the project's repository is known where {branch} is");
            return Err((to, why));
        };
        let trip = Trip {
            from: (from.worker, from_repo),
            to: (to, to_repo),
            branch,
            into: Task::home_branch(project, task),
            target: record.target.clone(),
        };
        drop(state);
        Ok((!trip.there()).then_some(trip))
    }

    /// Bundle the branch on its worker, send the bundle across, and fetch it there: what
    /// arrived, in words.
    async fn carry(
        &self,
        at: (&ProjectId, TaskId),
        trip: &Trip,
        target: Option<String>,
    ) -> Result<String, Carry> {
        let (from, to) = (trip.from.0, trip.to.0);
        let bundle = Verb::BundleBranch {
            worker: from,
            repo: trip.from.1.clone(),
            branch: trip.branch.clone(),
            target: target.clone(),
        };
        let BranchBundle { path, name, size, digest, head, .. } =
            match self.forward(None, bundle).await {
                Outcome::Bundle(made) => *made,
                other => return Err(Carry::Failed(said(&other))),
            };
        let upload = XferId::new();
        let into = format!("{BUNDLES}/{name}");
        let mut offset = 0;
        while offset < size {
            let read = Verb::ReadFile {
                worker: from,
                path: path.clone(),
                offset,
                length: Some(PART.min(size.saturating_sub(offset))),
            };
            let bytes = match self.forward(None, read).await {
                Outcome::File { bytes, .. } if !bytes.is_empty() => bytes,
                other => return Err(Carry::Failed(said(&other))),
            };
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let part = UploadPart::Bytes { offset, bytes };
            let sent = Verb::Upload { worker: to, path: into.clone(), upload, part };
            if let failed @ Outcome::Error { .. } = self.forward(None, sent).await {
                return Err(Carry::Failed(said(&failed)));
            }
            offset = offset.saturating_add(len);
            let percent =
                offset.saturating_mul(100).checked_div(size).and_then(|p| u8::try_from(p).ok());
            let phase = StepState::Running { phase: "Sending".to_owned(), percent };
            self.step_now(at, StepKind::Home, to, phase);
        }
        let finish = UploadPart::Finish { size, digest, mode: None };
        let finished = Verb::Upload { worker: to, path: into, upload, part: finish };
        if let failed @ Outcome::Error { .. } = self.forward(None, finished).await {
            return Err(Carry::Failed(said(&failed)));
        }
        let fetch = Verb::FetchBundle {
            worker: to,
            repo: trip.to.1.clone(),
            bundle: name,
            branch: trip.branch.clone(),
            into: trip.into.clone(),
            head,
        };
        match self.forward(None, fetch).await {
            Outcome::Fetched { branch, head } => {
                let short = head.get(..7).unwrap_or(&head);
                Ok(format!("{} as {branch} at {short} in {}", trip.branch, trip.to.1))
            }
            Outcome::Error { code: ErrorCode::Conflict, .. } if target.is_some() => {
                Err(Carry::Lacking)
            }
            other => Err(Carry::Failed(said(&other))),
        }
    }
}

/// A branch's way home.
struct Trip {
    /// The worker it ran on, and the clone or worktree it is in there.
    from: (WorkerId, String),
    /// The orchestrator's worker, and its clone.
    to: (WorkerId, String),
    branch: String,
    /// The branch it lands as there ([`Task::home_branch`]).
    into: String,
    /// The branch work lands on.
    target: String,
}

impl Trip {
    /// Whether the branch is in the orchestrator's clone already: it ran in that clone or a
    /// worktree under it.
    fn there(&self) -> bool {
        let root = self.to.1.trim_end_matches('/');
        self.from.0 == self.to.0
            && (self.from.1 == root
                || self.from.1.strip_prefix(root).is_some_and(|rest| rest.starts_with('/')))
    }
}

/// Why a bundle did not arrive.
enum Carry {
    /// The receiving clone lacks the fork point the bundle starts after, even after fetching
    /// its origin.
    Lacking,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trip(from: (WorkerId, &str), to: (WorkerId, &str)) -> Trip {
        Trip {
            from: (from.0, from.1.to_owned()),
            to: (to.0, to.1.to_owned()),
            branch: "b".to_owned(),
            into: "slopty/p/1".to_owned(),
            target: "main".to_owned(),
        }
    }

    #[test]
    fn a_branch_is_home_only_in_that_clone_on_that_worker() {
        let (studio, linux) = (WorkerId::new(), WorkerId::new());
        let tree = "/w/demo/.claude/worktrees/slopty-demo-1";
        assert!(trip((studio, tree), (studio, "/w/demo")).there());
        assert!(trip((studio, "/w/demo"), (studio, "/w/demo/")).there());
        assert!(!trip((studio, "/w/demo-2"), (studio, "/w/demo")).there(), "a sibling clone");
        assert!(!trip((linux, tree), (studio, "/w/demo")).there(), "another machine");
    }
}
