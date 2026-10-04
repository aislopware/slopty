//! A project's schedules, set by the person and run when they fall due
//! (`docs/decisions/projects.md`, "A project runs tasks on a schedule the person sets").

use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::project::ProjectId;

use super::{Hub, State, error, keyed, live, remember};
use crate::hub::steps::said;
use crate::project::{Caller, Running};

impl Hub {
    /// Set, or take away, a schedule of `project` ([`Verb::ScheduleSet`],
    /// [`Verb::ScheduleDelete`]), answered with the project: the person's alone, since every
    /// run spends the plan.
    pub(in crate::hub) fn schedule_change(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        verb: &Verb,
    ) -> Outcome {
        if caller == Caller::Agent {
            return error(
                ErrorCode::Forbidden,
                "every run of a schedule spends the plan, so only the person sets one; ask them, \
                 or start the task once with task_spawn",
            );
        }
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if let Some(key) = &key
            && let Some(answer) = keyed(state, caller, key, verb)
        {
            return answer;
        }
        let now = WallMs::now();
        let (terminals, agents) = live(state);
        let starting = state.starting.clone();
        let running = Running { terminals: &terminals, agents: &agents, starting: &starting };
        let changed = match verb.clone() {
            Verb::ScheduleSet { project, schedule, spec } => {
                state.projects.set_schedule(&project, schedule, *spec, &running, now)
            }
            Verb::ScheduleDelete { project, schedule } => {
                state.projects.delete_schedule(&project, schedule, &running, now)
            }
            _ => Err(error(ErrorCode::Invalid, "not a schedule's change")),
        };
        let outcome = match changed {
            Ok((status, updates)) => {
                self.projects_moved(state, updates);
                Outcome::Project(Box::new(status))
            }
            Err(refused) => refused,
        };
        if let Some(key) = key {
            remember(state, caller, key, verb, &outcome);
        }
        drop(guard);
        // Its next run may come before anything the delivery loop waits for.
        self.inner.deliver.notify_one();
        outcome
    }

    /// Run schedule `number` of `project` now, as the person asks ([`Verb::ScheduleRun`]) or
    /// as it falls due: its task made and started, and answered.
    pub(in crate::hub) async fn schedule_run(&self, project: ProjectId, number: u32) -> Outcome {
        let made = {
            let mut state = self.inner.state.lock();
            let now = WallMs::now();
            match state.projects.run_schedule(&project, number, now) {
                Ok((made, changes)) => {
                    self.projects_moved(&mut state, changes);
                    drop(state);
                    Ok(made)
                }
                Err(refused) => {
                    let why = said(&refused);
                    let changes =
                        state.projects.schedule_ran(&project, number, None, Some(&why), now);
                    self.projects_moved(&mut state, changes);
                    drop(state);
                    Err(refused)
                }
            }
        };
        let (task, launch) = match made {
            Ok(made) => made,
            Err(refused) => return refused,
        };
        let outcome = self.start_task_once(None, &project, task.id, launch).await;
        if let Outcome::Error { message, .. } = &outcome {
            let mut state = self.inner.state.lock();
            let why = format!("task {} did not start: {message}", task.id);
            let changes = state.projects.schedule_ran(
                &project,
                number,
                Some(task.id),
                Some(&why),
                WallMs::now(),
            );
            self.projects_moved(&mut state, changes);
            drop(state);
        }
        outcome
    }

    /// Start every schedule due at `wall`, and say when the next falls due, as the delivery
    /// loop's clock reads it.
    pub(in crate::hub) fn schedules_due(
        &self,
        state: &mut State,
        wall: WallMs,
        now: tokio::time::Instant,
    ) -> Option<tokio::time::Instant> {
        let (due, changes, next) = state.projects.schedules_due(wall);
        self.projects_moved(state, changes);
        for (project, number) in due {
            tracing::info!(%project, schedule = number, "a schedule fell due");
            let hub = self.clone();
            tokio::spawn(async move { hub.schedule_run(project, number).await });
        }
        next.and_then(|at| now.checked_add(Duration::from_millis(at.millis_since(wall))))
    }
}
