//! The merge queue's record (`docs/decisions/projects.md`, "A task is done when its verifier
//! passes; the server merges one at a time").
//!
//! The queue is the tasks themselves: a task waits in it while its [`Merge`] is queued, in the
//! order it joined, so a server that restarts takes the queue up where it stood. What a
//! project's lane does next ([`Projects::next_job`]) and each move it makes to a task, as one
//! change with at most one timeline entry ([`Projects::advance`]), are here; the lane itself,
//! which asks the workers, is the hub's.

use slopty_core::WallMs;
use slopty_proto::project::StepState;

use super::{
    Change, Changed, Merge, Moment, ProjectId, Projects, Record, Refused, SUMMARY_MAX, Task,
    TaskId, TaskState, TaskStep, VerifierRun, clipped, invalid, overlapping,
};

/// What a project's lane does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Job {
    /// Run the verifier on a task's branch as it is.
    Verify(TaskId),
    /// Merge the task at the head of the queue.
    Merge(TaskId),
}

/// One move the lane makes to a task; what is absent stays.
#[derive(Clone, Debug, Default)]
pub(crate) struct Advance {
    /// A new state, along [`TaskState::may_become`].
    pub state: Option<TaskState>,
    /// What the server is doing for it now.
    pub step: Option<TaskStep>,
    /// What its verifier said.
    pub verified: Option<VerifierRun>,
    /// Its place in the queue, or its merge.
    pub merge: Queue,
    /// The timeline's entry for the move, when it is worth one.
    pub moment: Option<Moment>,
}

/// What a move does to a task's [`Merge`].
#[derive(Clone, Debug, Default)]
pub(crate) enum Queue {
    /// Leaves it as it is.
    #[default]
    Keep,
    /// Takes it out of the queue.
    Leave,
    /// Sets it.
    Set(Merge),
}

/// The tasks waiting in `record`'s queue, the longest waiting first.
fn queue_of(record: &Record) -> Vec<TaskId> {
    let mut queued: Vec<(WallMs, TaskId)> = record
        .tasks
        .iter()
        .filter(|t| t.state == TaskState::Done)
        .filter_map(|t| Some((t.merge.as_ref()?.queued()?, t.id)))
        .collect();
    queued.sort_unstable();
    queued.into_iter().map(|(_, id)| id).collect()
}

impl Projects {
    /// What `id`'s lane does next: the task waiting longest for its verifier, else the head of
    /// the queue. Verifying comes first, since an agent waits on the answer and every pass
    /// feeds the queue.
    pub(crate) fn next_job(&self, id: &ProjectId) -> Option<Job> {
        let record = self.records.get(id)?;
        let verifying = record
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Verifying)
            .min_by_key(|t| (t.updated_ms, t.id));
        match verifying {
            Some(t) => Some(Job::Verify(t.id)),
            None => queue_of(record).first().copied().map(Job::Merge),
        }
    }

    /// The projects with work for their lanes.
    pub(crate) fn with_jobs(&self) -> Vec<ProjectId> {
        self.records.keys().filter(|id| self.next_job(id).is_some()).cloned().collect()
    }

    /// `id`'s queue, the longest waiting first.
    #[cfg(test)]
    pub(crate) fn queue(&self, id: &ProjectId) -> Vec<TaskId> {
        self.records.get(id).map(queue_of).unwrap_or_default()
    }

    /// Move `task` as the lane says: one change, with the timeline entry it asks for.
    pub(crate) fn advance(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        advance: Advance,
        now: WallMs,
    ) -> Result<(Task, Vec<Change>), Refused> {
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        if let Some(to) = advance.state.filter(|to| *to != t.state)
            && !t.state.may_become(to)
        {
            return Err(invalid(format!("task {task} cannot go from {:?} to {to:?}", t.state)));
        }
        if let Some(to) = advance.state {
            t.state = to;
        }
        if let Some(mut step) = advance.step {
            let text = match &mut step.state {
                StepState::Running { phase, .. } => phase,
                StepState::Done { detail } => detail,
                StepState::Failed { why } => why,
            };
            *text = clipped(text, SUMMARY_MAX);
            t.step = Some(step);
        }
        if let Some(mut run) = advance.verified {
            run.summary = clipped(&run.summary, SUMMARY_MAX);
            t.verified = Some(run);
        }
        match advance.merge {
            Queue::Keep => {}
            Queue::Leave => t.merge = None,
            Queue::Set(merge) => t.merge = Some(merge),
        }
        t.updated_ms = now;
        let task_now = t.clone();
        let entry = advance.moment.map(|what| record.log(Some(task), what, now));
        Ok((task_now.clone(), vec![record.task_update(&task_now, entry)]))
    }

    /// The person asks for `task`'s merge: its verifier runs first when one applies, and a
    /// task with none joins the queue at once. A task given up takes its paths again, as any
    /// move back into a live state does.
    pub(crate) fn ask_merge(&mut self, id: &ProjectId, task: TaskId, now: WallMs) -> Changed<Task> {
        let record = self.record(id)?;
        let t = record.task(task)?;
        if t.state == TaskState::Merged {
            return Err(invalid(format!("task {task} is merged already")));
        }
        if t.read_only {
            return Err(invalid(format!("task {task} only reads, so it has nothing to merge")));
        }
        if !t.state.holds_paths()
            && let Some((theirs, path, ours)) = record.conflict(Some(task), &t.owns)
        {
            return Err(overlapping(theirs, path, &ours));
        }
        let verifies = t.verifier.is_some() || record.project.verifier.is_some();
        let from = t.state;
        let advance = if verifies {
            Advance { state: Some(TaskState::Verifying), merge: Queue::Leave, ..Advance::default() }
        } else {
            Advance {
                state: Some(TaskState::Done),
                merge: Queue::Set(Merge::Queued { since_ms: now }),
                ..Advance::default()
            }
        };
        let to = advance.state.unwrap_or(from);
        let moment = (from != to).then_some(Moment::State { from, to });
        self.advance(id, task, Advance { moment, ..advance }, now)
    }
}

#[cfg(test)]
mod tests;
