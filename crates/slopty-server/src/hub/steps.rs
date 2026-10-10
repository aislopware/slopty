//! What the server does for a task around its agent, each shown as the task's step
//! ([`TaskStep`]) so no wait is silent: a clone made on the worker a task is placed on when
//! that worker has none of the project's repository, and the task's branch brought to the
//! orchestrator's clone once it is done.
//!
//! The branch comes as a git bundle of its own commits, read in parts from the worker it ran
//! on and uploaded into the orchestrator's worker, which fetches it as
//! [`Task::home_branch`] (`docs/decisions/projects.md`). Merging it stays with the
//! orchestrator.

use std::collections::{HashMap, HashSet};

use slopty_core::{WallMs, WorkerId, XferId};
use slopty_proto::orchestration::{
    BUNDLES, BranchBundle, ErrorCode, Outcome, TermRef, UploadPart, Verb,
};
use slopty_proto::project::{
    Commits, Project, ProjectId, StepKind, StepState, Task, TaskId, TaskStep,
};
use slopty_proto::terminal::{RepoId, SessionState};

use super::queue::short;
use super::{Hub, State, projects};
use crate::project::Keep;

/// The most bytes of a bundle read or uploaded in one request.
const PART: u64 = 4 << 20;

/// The steps under way, and the clones the server had made.
#[derive(Debug, Default)]
pub(super) struct Steps {
    next_clone: u64,
    /// Each clone under way, by the number its progress names: the task it is for.
    cloning: HashMap<u64, (ProjectId, TaskId, WorkerId)>,
    /// The tasks whose branch is on its way home, one trip each at a time; `true` when it was
    /// reported done again meanwhile, so the trip goes once more for the newer commits.
    homing: HashMap<(ProjectId, TaskId), bool>,
    /// The tasks the person asked to merge whose branch is on its way home first.
    merge_after: HashSet<(ProjectId, TaskId)>,
    /// The verifiers a restart of the server left running, by task: their terminal, the
    /// commits they check and since when, for the lane to follow rather than run again.
    reattach: HashMap<(ProjectId, TaskId), (TermRef, Commits, WallMs)>,
}

impl Steps {
    /// The verifier of `task` a restart of the server left running, once: its terminal, the
    /// commits it checks and since when.
    pub(super) fn left_running(
        &mut self,
        task: &(ProjectId, TaskId),
    ) -> Option<(TermRef, Commits, WallMs)> {
        self.reattach.remove(task)
    }

    /// The merges the person asked for that wait for their branch to come home.
    pub(super) fn merges(&self) -> Vec<(ProjectId, TaskId)> {
        let mut merges: Vec<_> = self.merge_after.iter().cloned().collect();
        merges.sort();
        merges
    }

    /// Take up the merges a store kept waiting: each goes once its branch is home, as the
    /// step bringing it is taken up again.
    pub(super) fn adopt_merges(&mut self, merges: impl IntoIterator<Item = (ProjectId, TaskId)>) {
        self.merge_after.extend(merges);
    }
}

/// What a forwarded verb that failed said.
pub(super) fn said(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Error { message, .. } => message.clone(),
        other => format!("an unexpected answer: {other:?}"),
    }
}

impl Hub {
    /// Move `task`'s step on, and push the change.
    pub(super) fn step(
        &self,
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
        step: TaskStep,
    ) {
        match state.projects.set_step(project, task, step, WallMs::now()) {
            Ok(updates) => self.projects_moved(state, updates),
            Err(refused) => tracing::debug!(%project, %task, ?refused, "a step for no task"),
        }
    }

    /// [`Self::step`] to `now`, from its own lock.
    pub(super) fn step_now(
        &self,
        (project, task): (&ProjectId, TaskId),
        kind: StepKind,
        worker: WorkerId,
        now: StepState,
    ) {
        let step = TaskStep {
            kind,
            worker,
            state: now,
            since_ms: WallMs::now(),
            term: None,
            commits: None,
        };
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
                projects::cloned(&mut state, worker, &path, &repo);
                (StepState::Done { detail: path.clone() }, Ok(path))
            }
            other => {
                let why = said(&other);
                (StepState::Failed { why: why.clone() }, Err(why))
            }
        };
        let step = TaskStep {
            kind: StepKind::Clone,
            worker,
            state: ended,
            since_ms: WallMs::now(),
            term: None,
            commits: None,
        };
        self.step(&mut state, (project, task), step);
        drop(state);
        cloned
    }

    /// Take up the steps under way on `worker` when the server stopped, now that it is back.
    ///
    /// A clone is made again (the worker answers one already there as it is), and a branch on
    /// its way home goes again: both only set names the server alone uses. A verifier whose
    /// terminal still runs there is followed as it is. Anything else, and a verifier whose
    /// terminal is gone, is the lane's to do again from the store.
    /// Nothing here runs a step a second time that could do harm twice: a merge and a rebase
    /// start over from the target as it is, as the lane would after any failure.
    pub(super) fn resume_steps(&self, state: &mut State, worker: WorkerId) {
        for (project, task, step) in state.projects.resumable(worker) {
            let running = step.term.filter(|term| {
                state.workers.get(&term.worker).is_some_and(|e| {
                    e.sessions
                        .iter()
                        .any(|s| s.id == term.session && s.state == SessionState::Running)
                })
            });
            tracing::info!(%project, %task, kind = ?step.kind, "a step taken up after a restart");
            match step.kind {
                StepKind::Clone => {
                    let url = state
                        .projects
                        .project(&project)
                        .ok()
                        .and_then(|p| p.repo_id.as_ref()?.url.clone());
                    let Some(url) = url else {
                        let why = "no address to clone from is known now".to_owned();
                        let failed = TaskStep { state: StepState::Failed { why }, ..step };
                        self.step(state, (&project, task), failed);
                        continue;
                    };
                    let hub = self.clone();
                    tokio::spawn(async move {
                        if let Err(why) = hub.clone_for((&project, task), worker, url).await {
                            tracing::info!(%project, %task, why, "a clone taken up failed");
                        }
                    });
                }
                // A branch goes home only once both ends are here: the worker it ran on may
                // register after the orchestrator's, and the trip waits for it then.
                StepKind::Home => match away_from(state, &project, task) {
                    Some(from) => state.projects.resume_on(from, (project, task), step),
                    None => self.bring_home_soon(state, (project, task), None),
                },
                StepKind::Verify => {
                    if let (Some(term), Some(commits)) = (running, step.commits) {
                        state
                            .steps
                            .reattach
                            .insert((project, task), (term, commits, step.since_ms));
                    }
                }
                StepKind::Merge | StepKind::Rebase => {}
            }
        }
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
            term: None,
            commits: None,
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
                let home = hub.bring_home(project, *task, branch.clone()).await;
                // What the work did to the tests is read once the last trip is in.
                let again = hub.inner.state.lock().steps.homing.get(&key) == Some(&true);
                if home && !again {
                    hub.read_tests((project, *task)).await;
                }
                let mut state = hub.inner.state.lock();
                if state.steps.homing.remove(&key) != Some(true) {
                    let merge = state.steps.merge_after.remove(&key);
                    if merge {
                        let (project, task) = (project.clone(), *task);
                        projects::keep(&mut state, Keep::Merge { project, task, waits: false });
                    }
                    if home && merge {
                        hub.merge_asked(&mut state, (project, *task));
                    } else if home {
                        hub.verify_soon(&mut state, (project, *task));
                    }
                    break;
                }
                state.steps.homing.insert(key.clone(), false);
            }
        });
    }

    /// The person asked for `task`'s merge: when its branch is on another machine, or in
    /// another clone, it comes home first and the merge is asked once it is there, so the
    /// queue judges what the agent has now. Whether it went home for it.
    pub(super) fn merge_when_home(
        &self,
        state: &mut State,
        (project, task): (&ProjectId, TaskId),
    ) -> bool {
        let away = matches!(route_in(state, project, task, None), Ok(Some(trip)) if !trip.there());
        if away {
            state.steps.merge_after.insert((project.clone(), task));
            projects::keep(state, Keep::Merge { project: project.clone(), task, waits: true });
            self.bring_home_soon(state, (project.clone(), task), None);
        }
        away
    }

    /// Bring `task`'s branch (`branch`, else the one its card names) from the worker it ran on
    /// to the clone its orchestrator works in, as [`Task::home_branch`]. Nothing to do, and no
    /// step, when it ran in that clone or a worktree of it, or names no branch. Whether it is
    /// there now, or never had to go.
    async fn bring_home(&self, project: &ProjectId, task: TaskId, branch: Option<String>) -> bool {
        let at = (project, task);
        let trip = match self.trip(project, task, branch) {
            Ok(Some(trip)) => trip,
            Ok(None) => return true,
            Err((worker, why)) => {
                self.step_now(at, StepKind::Home, worker, StepState::Failed { why });
                return false;
            }
        };
        let to = trip.to.0;
        let packing = StepState::Running { phase: "Bundling".to_owned(), percent: None };
        self.step_now(at, StepKind::Home, to, packing);
        let shown = (StepKind::Home, to, "Sending");
        let carried = match self.carry(at, &trip, Some(trip.target.clone()), shown).await {
            Err(Carry::Lacking) => self.carry(at, &trip, None, shown).await,
            other => other,
        };
        let ended = match carried {
            Ok(Arrived { detail, .. }) => StepState::Done { detail },
            Err(Carry::Lacking) => StepState::Failed {
                why: "the orchestrator's clone lacks the branch's history".to_owned(),
            },
            Err(Carry::NothingNew(why) | Carry::Failed(why)) => StepState::Failed { why },
        };
        let home = matches!(ended, StepState::Done { .. });
        self.step_now(at, StepKind::Home, to, ended);
        home
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
        let route = self.route(project, task, branch)?;
        Ok(route.filter(|trip| !trip.there()))
    }

    /// Where `task`'s work is in the orchestrator's clone, once its branch is home or when it
    /// ran there: `None` with no branch, agent, orchestrator or known repository.
    ///
    /// # Errors
    /// Why there is no such clone, or none where the branch is.
    pub(super) fn landed(
        &self,
        project: &ProjectId,
        task: TaskId,
    ) -> Result<Option<Landed>, String> {
        let route = self.route(project, task, None).map_err(|(_, why)| why)?;
        Ok(route.map(|trip| {
            let branch = if trip.there() { trip.branch } else { trip.into };
            Landed { worker: trip.to.0, clone: trip.to.1, branch }
        }))
    }

    /// Send the project's target, as the orchestrator's clone has it, to the clone `task`'s
    /// agent works in on another machine, as [`Task::target_branch`]: the merge queue gave the
    /// task back to rebase onto it, and with pushing off the forge never saw what the queue
    /// merged. Shown on `task`'s merge step on `worker` as it goes.
    pub(super) async fn send_target(
        &self,
        (project, task): (&ProjectId, TaskId),
        worker: WorkerId,
    ) -> Onto {
        let route = match self.route(project, task, None) {
            Ok(Some(route)) if !route.there() => route,
            Ok(_) => return Onto::Here,
            Err((_, why)) => return Onto::Failed(why),
        };
        let back = Trip {
            from: route.to,
            to: route.from,
            branch: route.target.clone(),
            into: Task::target_branch(project),
            target: route.target,
        };
        self.send_back((project, task), back, (StepKind::Merge, worker)).await
    }

    /// Send what `task`'s new worktree starts from to `clone` on `worker`, where it is about to
    /// start, and answer the branch it starts from there. That is the project's target as the
    /// orchestrator's clone has it, as [`Task::target_branch`]: with pushing off the forge never
    /// saw what the merge queue merged, and a task elsewhere would start from the worker's stale
    /// copy of the target. For a task that starts from the done, checked work of `on`
    /// ([`Task::start_from`]), it is that work's branch as the orchestrator's clone holds it, as
    /// [`Task::home_branch`] of `on`. Shown on `task`'s clone step as it goes, and settled there
    /// once it ends.
    ///
    /// # Errors
    /// Why it could not be sent, or why `on`'s work is in no clone the server knows.
    pub(super) async fn send_start_to(
        &self,
        (project, task): (&ProjectId, TaskId),
        (worker, clone): (WorkerId, String),
        on: Option<TaskId>,
    ) -> Result<String, String> {
        let (back, target) = match self.start_from(project, (worker, clone), on)? {
            StartFrom::There(base) => return Ok(base),
            StartFrom::Send(back) => {
                let target = back.target.clone();
                (*back, target)
            }
        };
        let what = on.map_or_else(|| target.clone(), |on| format!("task {on}'s work"));
        let onto = self.send_back((project, task), back, (StepKind::Clone, worker)).await;
        let (ended, started) = match onto {
            Onto::Here => (None, Ok(target)),
            Onto::Sent { head, branch } => {
                let detail =
                    format!("{what} as the orchestrator's clone has it, at {}", short(&head));
                (Some(StepState::Done { detail }), Ok(branch))
            }
            // Nothing the clone lacks: the target is all there is to start from.
            Onto::Forge => {
                let detail = format!("{target} from its origin");
                (Some(StepState::Done { detail }), Ok(target))
            }
            Onto::Failed(why) => {
                let why = format!("{what} could not be sent to the task's clone: {why}");
                (Some(StepState::Failed { why: why.clone() }), Err(why))
            }
        };
        if let Some(ended) = ended {
            self.step_now((project, task), StepKind::Clone, worker, ended);
        }
        started
    }

    /// Whether a task's new worktree in `clone` on `worker`, starting from the work of `on`
    /// or the target, needs that sent there first: a start that takes a while.
    pub(super) fn sends_start(
        &self,
        project: &ProjectId,
        (worker, clone): (WorkerId, String),
        on: Option<TaskId>,
    ) -> bool {
        matches!(self.start_from(project, (worker, clone), on), Ok(StartFrom::Send(_)))
    }

    /// Where a task's new worktree in `clone` on `worker` starts ([`Self::send_start_to`]): a
    /// branch there already, or a trip that brings it.
    ///
    /// # Errors
    /// Why `on`'s work is in no clone the server knows, or the orchestrator's has no clone.
    fn start_from(
        &self,
        project: &ProjectId,
        (worker, clone): (WorkerId, String),
        on: Option<TaskId>,
    ) -> Result<StartFrom, String> {
        let target = {
            let state = self.inner.state.lock();
            state.projects.project(project).ok().map(|record| record.target.clone())
        };
        let Some(target) = target else { return Err(format!("no project {project}")) };
        let (from, branch, into) = if let Some(on) = on {
            let Some(Landed { worker, clone, branch }) = self.landed(project, on)? else {
                return Err(format!("task {on}'s work is in no clone the server knows"));
            };
            ((worker, clone), branch, Task::home_branch(project, on))
        } else {
            let from = {
                let state = self.inner.state.lock();
                state.projects.project(project).ok().map(|r| orchestrator_clone(&state, r))
            };
            match from {
                Some(Ok(Some(from))) => (from, target.clone(), Task::target_branch(project)),
                None | Some(Ok(None)) => return Ok(StartFrom::There(target)),
                Some(Err((_, why))) => return Err(why),
            }
        };
        let back = Trip { from, to: (worker, clone), branch, into, target };
        Ok(if back.there() {
            StartFrom::There(back.branch)
        } else {
            StartFrom::Send(Box::new(back))
        })
    }

    /// Carry the target back along `back`, showing it on `task`'s step of `kind` on `worker`.
    async fn send_back(
        &self,
        (project, task): (&ProjectId, TaskId),
        back: Trip,
        (kind, worker): (StepKind, WorkerId),
    ) -> Onto {
        let label = format!("Sending {} to the task's clone", back.branch);
        let shown = (kind, worker, label.as_str());
        let phase = StepState::Running { phase: label.clone(), percent: None };
        self.step_now((project, task), kind, worker, phase);
        let at = (project, task);
        let carried = match self.carry(at, &back, Some(back.target.clone()), shown).await {
            Err(Carry::Lacking) => self.carry(at, &back, None, shown).await,
            other => other,
        };
        match carried {
            Ok(Arrived { head, .. }) => Onto::Sent { branch: back.into, head },
            Err(Carry::NothingNew(_)) => Onto::Forge,
            Err(Carry::Lacking) => {
                Onto::Failed("the task's clone lacks the target's history".to_owned())
            }
            Err(Carry::Failed(why)) => Onto::Failed(why),
        }
    }

    /// Where `task`'s branch is and where it goes, [`Self::trip`]'s answer whether or not it
    /// is in the orchestrator's clone already.
    fn route(
        &self,
        project: &ProjectId,
        task: TaskId,
        branch: Option<String>,
    ) -> Result<Option<Trip>, (WorkerId, String)> {
        let state = self.inner.state.lock();
        let route = route_in(&state, project, task, branch);
        drop(state);
        route
    }

    /// Bundle the branch on its worker, send the bundle across, and fetch it there, showing
    /// each part sent as `shown`'s step (its kind, its worker, and the phase's words).
    async fn carry(
        &self,
        at: (&ProjectId, TaskId),
        trip: &Trip,
        target: Option<String>,
        (kind, worker, sending): (StepKind, WorkerId, &str),
    ) -> Result<Arrived, Carry> {
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
                Outcome::Error { code: ErrorCode::NothingNew, message } => {
                    return Err(Carry::NothingNew(message));
                }
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
            let phase = StepState::Running { phase: sending.to_owned(), percent };
            self.step_now(at, kind, worker, phase);
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
                let detail = format!("{} as {branch} at {short} in {}", trip.branch, trip.to.1);
                Ok(Arrived { detail, head })
            }
            Outcome::Error { code: ErrorCode::Conflict, .. } if target.is_some() => {
                Err(Carry::Lacking)
            }
            other => Err(Carry::Failed(said(&other))),
        }
    }
}

/// The branches the server named in clones for `project`'s tasks, by worker and clone: each
/// task's work brought home ([`Task::home_branch`]) in the orchestrator's clone, and the
/// project's target sent to a task's clone on another machine ([`Task::target_branch`]). What
/// letting the project go drops ([`Verb::DropBranches`]).
pub(super) fn server_branches(
    state: &State,
    project: &ProjectId,
) -> Vec<((WorkerId, String), Vec<String>)> {
    let Ok(tasks) = state.projects.project_tasks(project) else { return Vec::new() };
    let mut named: HashMap<(WorkerId, String), Vec<String>> = HashMap::new();
    for task in tasks {
        let Ok(Some(trip)) = route_in(state, project, task, None) else { continue };
        if trip.there() {
            continue;
        }
        named.entry(trip.to).or_default().push(trip.into);
        let target = Task::target_branch(project);
        let there = named.entry(trip.from).or_default();
        if !there.contains(&target) {
            there.push(target);
        }
    }
    named.into_iter().collect()
}

/// [`Hub::route`] in `state`.
fn route_in(
    state: &State,
    project: &ProjectId,
    task: TaskId,
    branch: Option<String>,
) -> Result<Option<Trip>, (WorkerId, String)> {
    let (Ok(record), Ok(card)) =
        (state.projects.project(project), state.projects.task(project, task))
    else {
        return Ok(None);
    };
    let Some(id) = record.repo_id.as_ref() else { return Ok(None) };
    let (Some(branch), Some(assigned)) =
        (branch.or_else(|| card.branch.clone()), card.assignment.as_ref())
    else {
        return Ok(None);
    };
    let Some((to, to_repo)) = orchestrator_clone(state, record)? else { return Ok(None) };
    let from = assigned.term;
    let session_repo = |term: TermRef| session_repo(state, id, term);
    let Some(from_repo) = card
        .worktree
        .clone()
        .or_else(|| session_repo(from))
        .or_else(|| projects::clone_on(state, record, from.worker))
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
    Ok(Some(trip))
}

/// The worker `task`'s branch is on, when it is not linked now: a Home trip taken up after a
/// restart waits for it.
fn away_from(state: &State, project: &ProjectId, task: TaskId) -> Option<WorkerId> {
    let card = state.projects.task(project, task).ok()?;
    let from = card.assignment.as_ref()?.term.worker;
    let linked = state.workers.get(&from).is_some_and(|e| e.link.is_some());
    (!linked).then_some(from)
}

/// The clone of `term`'s session when it is in the repository `id`.
fn session_repo(state: &State, id: &RepoId, term: TermRef) -> Option<String> {
    let entry = state.workers.get(&term.worker)?;
    let s = entry.sessions.iter().find(|s| s.id == term.session)?;
    s.repo_id.as_ref().is_some_and(|other| other.same(id)).then(|| s.repo.clone())?
}

/// The orchestrator's worker and clone in `record`'s project: its own checkout while it is
/// still in the project's repository, else any clone of it on that worker. `None` for a
/// project with no repository or no orchestrator.
///
/// # Errors
/// The orchestrator's worker, which has no clone of the repository, and why.
fn orchestrator_clone(
    state: &State,
    record: &Project,
) -> Result<Option<(WorkerId, String)>, (WorkerId, String)> {
    let (Some(id), Some(orchestrator)) = (record.repo_id.as_ref(), record.orchestrator) else {
        return Ok(None);
    };
    let to = orchestrator.worker;
    let clone =
        session_repo(state, id, orchestrator).or_else(|| projects::clone_on(state, record, to));
    let why = "the orchestrator's worker has no clone of the project's repository";
    clone.map(|clone| Some((to, clone))).ok_or_else(|| (to, why.to_owned()))
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

/// Where a task's work is in the orchestrator's clone.
#[derive(Clone, Debug)]
pub(super) struct Landed {
    /// The orchestrator's worker.
    pub worker: WorkerId,
    /// Its clone.
    pub clone: String,
    /// The branch the work is there as: brought home, or its own when it ran in that clone.
    pub branch: String,
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

/// Where a task's new worktree starts ([`Hub::start_from`]).
enum StartFrom {
    /// From this branch, in its clone already.
    There(String),
    /// From what this trip brings to its clone first.
    Send(Box<Trip>),
}

/// A branch that arrived.
struct Arrived {
    /// What arrived, in words.
    detail: String,
    /// The commit it is at.
    head: String,
}

/// Why a bundle did not arrive.
enum Carry {
    /// The receiving clone lacks the fork point the bundle starts after, even after fetching
    /// its origin.
    Lacking,
    /// The branch has no commit beyond the target the forge has.
    NothingNew(String),
    Failed(String),
}

/// Where the target is for a task the merge queue gave back ([`Hub::send_target`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Onto {
    /// The task works in the orchestrator's clone, or nothing runs for it: the target is
    /// there as it is.
    Here,
    /// Sent to the task's clone as `branch`, at `head`.
    Sent {
        /// [`Task::target_branch`].
        branch: String,
        /// The commit it is at.
        head: String,
    },
    /// The forge has it all: the task's clone fetches its origin.
    Forge,
    /// It could not be sent, and why.
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
