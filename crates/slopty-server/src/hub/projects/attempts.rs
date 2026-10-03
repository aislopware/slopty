//! Attempts at a task, started and picked (`docs/decisions/projects.md`, "A task is tried by
//! several agents at once, and the attempt picked lands").
//!
//! Each attempt starts as a task's start does, all at once, each on a worker no other attempt
//! took where another fits, so the attempts spread over the machines before they share one.
//! Picking one closes every other attempt's agent and frees its worktree, keeping its branch.

use slopty_core::{WallMs, WorkerId};
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::project::{ProjectId, TaskId, TaskLaunch};
use tokio::task::JoinSet;

use super::{Hub, error, keyed, live, remember};
use crate::project::Caller;

impl Hub {
    /// Have an agent try `task` for each of `launches` at once ([`Verb::TaskAttempts`]),
    /// answered with the task. Where a project waits for the person to start its tasks, an
    /// agent's attempts are proposed instead.
    pub(in crate::hub) async fn task_attempts(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        project: ProjectId,
        task: TaskId,
        launches: Vec<TaskLaunch>,
    ) -> Outcome {
        let verb =
            Verb::TaskAttempts { project: project.clone(), task, launches: launches.clone() };
        if let Some(key) = &key
            && let Some(answer) = keyed(&mut self.inner.state.lock(), caller, key, &verb)
        {
            return answer;
        }
        let hub = self.clone();
        let tried = tokio::spawn(async move {
            let outcome = hub.try_at_once(caller, key.as_ref(), &project, task, launches).await;
            if let Some(key) = key {
                remember(&mut hub.inner.state.lock(), caller, key, &verb, &outcome);
            }
            outcome
        });
        tried
            .await
            .unwrap_or_else(|e| error(ErrorCode::Failed, &format!("the attempts ended: {e}")))
    }

    /// Make the attempts and start each, saying on an attempt's card why it did not start.
    /// Refused as a whole when none started.
    async fn try_at_once(
        &self,
        caller: Caller,
        key: Option<&IdempotencyKey>,
        project: &ProjectId,
        task: TaskId,
        launches: Vec<TaskLaunch>,
    ) -> Outcome {
        let made = {
            let mut state = self.inner.state.lock();
            match state.projects.attempt(project, task, launches.len(), WallMs::now()) {
                Ok((made, changes)) => {
                    self.projects_moved(&mut state, changes);
                    made
                }
                Err(refused) => return refused,
            }
        };
        let asks = caller == Caller::Agent
            && self.inner.state.lock().projects.project(project).is_ok_and(|p| p.ask_to_start);
        let mut taken: Vec<WorkerId> = launches.iter().filter_map(|l| l.pin).collect();
        let mut starts = JoinSet::new();
        for (attempt, mut launch) in made.iter().copied().zip(launches) {
            if launch.pin.is_none()
                && let Some(worker) = self.apart(project, attempt, &launch, &taken).await
            {
                launch.pin = Some(worker);
                taken.push(worker);
            }
            let hub = self.clone();
            let project = project.clone();
            let key = key.map(|k| k.part(&format!("attempt-{attempt}")));
            starts.spawn(async move {
                let outcome = if asks {
                    hub.propose(&project, attempt, launch).await
                } else {
                    hub.start_task_once(key, &project, attempt, launch).await
                };
                (attempt, outcome)
            });
        }
        let mut refused = Vec::new();
        while let Some(joined) = starts.join_next().await {
            let (attempt, outcome) = match joined {
                Ok(started) => started,
                Err(e) => {
                    refused.push(error(ErrorCode::Failed, &format!("a start ended: {e}")));
                    continue;
                }
            };
            if let Outcome::Error { message, .. } = &outcome {
                let mut state = self.inner.state.lock();
                let changes = state.projects.not_started(project, attempt, message, WallMs::now());
                self.projects_moved(&mut state, changes);
                drop(state);
                refused.push(outcome);
            }
        }
        if refused.len() < made.len() {
            return self.tried(project, task);
        }
        let named: Vec<String> = made.iter().map(ToString::to_string).collect();
        match refused.into_iter().next() {
            Some(Outcome::Error { code, message }) => Outcome::Error {
                code,
                message: format!(
                    "attempts {} at task {task} were made and none started: {message}; start \
                     each with task_spawn once that is settled",
                    named.join(", ")
                ),
            },
            _ => self.tried(project, task),
        }
    }

    /// `task` as it is now.
    fn tried(&self, project: &ProjectId, task: TaskId) -> Outcome {
        let state = self.inner.state.lock();
        state
            .projects
            .task(project, task)
            .map_or_else(|r| r, |t| Outcome::Task(Box::new(t.clone())))
    }

    /// A worker that fits `attempt` and that no attempt in `taken` went to, the best ranked:
    /// none when every fitting worker is taken, and the attempt goes where its start would.
    async fn apart(
        &self,
        project: &ProjectId,
        attempt: TaskId,
        launch: &TaskLaunch,
        taken: &[WorkerId],
    ) -> Option<WorkerId> {
        let (placement, gathered) = {
            let mut state = self.inner.state.lock();
            let wanted = Self::placement_for(&state, project, attempt, launch).ok()?;
            let gathered = Self::candidates(&mut state, Some(project)).wanting(&wanted);
            drop(state);
            (wanted.placement, gathered)
        };
        let ranked = self.rank(placement, gathered).await.ok()?;
        ranked.iter().find(|s| s.fits && !taken.contains(&s.worker)).map(|s| s.worker)
    }

    /// Pick `attempt` to land ([`Verb::TaskPick`]): every other attempt's agent is closed and
    /// its worktree freed, and the attempt picked joins the merge queue once its work is done
    /// and checked. Answered with the task tried.
    pub(in crate::hub) fn task_pick(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        project: &ProjectId,
        attempt: TaskId,
    ) -> Outcome {
        let verb = Verb::TaskPick { project: project.clone(), attempt };
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if let Some(key) = &key
            && let Some(answer) = keyed(state, caller, key, &verb)
        {
            return answer;
        }
        let outcome = match state.projects.pick(project, attempt, WallMs::now()) {
            Ok((picked, changes)) => {
                self.projects_moved(state, changes);
                let (terminals, _) = live(state);
                for (lost, last) in picked.lost {
                    self.let_go(state, (project, lost));
                    let Some((term, open)) = last else { continue };
                    let free = state
                        .projects
                        .to_free(project, lost)
                        .map(|(worktree, landed)| (project.clone(), lost, worktree, landed));
                    self.close_and_free(term, open && terminals.contains(&term), free);
                }
                if picked.queued {
                    self.kick(state, project);
                }
                Outcome::Task(Box::new(picked.task))
            }
            Err(refused) => refused,
        };
        if let Some(key) = key {
            remember(state, caller, key, &verb, &outcome);
        }
        drop(guard);
        outcome
    }
}
