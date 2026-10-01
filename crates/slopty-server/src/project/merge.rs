//! The merge queue's record (`docs/decisions/projects.md`, "A task is done when its verifier
//! passes; the server merges one at a time").
//!
//! The queue is the tasks themselves: a task waits in it while its [`Merge`] is queued, in the
//! order it joined, so a server that restarts takes the queue up where it stood. What a
//! project's lane does next ([`Projects::next_job`]) and each move it makes to a task, as one
//! change with at most one timeline entry ([`Projects::advance`]), are here; the lane itself,
//! which asks the workers, is the hub's.

use slopty_core::WallMs;
use slopty_proto::project::{
    FINDING_MAX, FINDING_PATH_MAX, FINDINGS_MAX, REVIEW_SUMMARY_MAX, ReviewRun, StepKind, StepState,
};

use super::{
    Change, Changed, Merge, Moment, ProjectId, Projects, Record, Refused, SUMMARY_MAX, Task,
    TaskId, TaskState, TaskStep, VerifierRun, clipped, invalid, overlapping, unknown_project,
};

/// What a project's lane does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Job {
    /// Run the verifier on a task's branch as it is.
    Verify(TaskId),
    /// Merge the task at the head of the queue.
    Merge(TaskId),
    /// Start a fresh-context reviewer on a task's work, verified or with no verifier.
    Review(TaskId),
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
    /// What its reviewer said.
    pub reviewed: Option<ReviewRun>,
    /// Forget what was judged of earlier work first, its step with it: the work changed.
    pub fresh: bool,
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

/// What a task being checked waits for next: its verifier, its reviewer, or nothing the lane
/// does (a reviewer at work, or one that ended without a verdict, which the person settles).
fn check_of(record: &Record, t: &Task) -> Option<Job> {
    let review = t.step.as_ref().filter(|s| s.kind == StepKind::Review);
    let reviewing = review.is_some_and(|s| s.term.is_some() && s.running());
    let stopped = review.is_some_and(|s| matches!(s.state, StepState::Failed { .. }));
    if reviewing || (stopped && t.reviewed.is_none()) {
        return None;
    }
    let verifies = t.verifier.is_some() || record.project.verifier.is_some();
    let verified = t.verified.as_ref().is_some_and(|r| r.passed) || !verifies;
    if record.project.review.is_some() && verified && t.reviewed.is_none() {
        Some(Job::Review(t.id))
    } else {
        Some(Job::Verify(t.id))
    }
}

impl Projects {
    /// What `id`'s lane does next: for the task waiting longest to be checked, its verifier or
    /// its reviewer; else the head of the queue. Checking comes first, since an agent waits on
    /// the answer and every pass feeds the queue. A reviewer runs beside the lane, so a task
    /// whose reviewer is at work waits without holding the rest.
    pub(crate) fn next_job(&self, id: &ProjectId) -> Option<Job> {
        let record = self.records.get(id)?;
        let mut checking: Vec<&Task> =
            record.tasks.iter().filter(|t| t.state == TaskState::Verifying).collect();
        checking.sort_by_key(|t| (t.updated_ms, t.id));
        checking
            .into_iter()
            .find_map(|t| check_of(record, t))
            .or_else(|| queue_of(record).first().copied().map(Job::Merge))
    }

    /// Whether `id` has a reviewer read each task's work before it merges.
    pub(crate) fn reviews(&self, id: &ProjectId) -> bool {
        self.records.get(id).is_some_and(|r| r.project.review.is_some())
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
        if advance.fresh {
            t.verified = None;
            t.reviewed = None;
            t.step = None;
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
        if let Some(run) = advance.reviewed {
            t.reviewed = Some(bounded(run));
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
        self.may_merge(id, task)?;
        let record = self.record(id)?;
        let t = record.task(task)?;
        let checks = t.verifier.is_some()
            || record.project.verifier.is_some()
            || record.project.review.is_some();
        let from = t.state;
        let advance = if checks {
            Advance {
                state: Some(TaskState::Verifying),
                merge: Queue::Leave,
                fresh: true,
                ..Advance::default()
            }
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

    /// Why the person may not ask for `task`'s merge now, if they may not: it is merged
    /// already, it only reads, or its paths are another live task's since it let them go.
    ///
    /// # Errors
    /// That reason.
    pub(crate) fn may_merge(&self, id: &ProjectId, task: TaskId) -> Result<(), Refused> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
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
        Ok(())
    }
}

/// `run` within the bounds a card carries ([`ReviewRun::MAX_BYTES`]): its texts clipped, and
/// the findings past [`FINDINGS_MAX`] counted rather than kept, those that block kept first.
pub(crate) fn bounded(mut run: ReviewRun) -> ReviewRun {
    let verdict = &mut run.verdict;
    verdict.summary = clipped(&verdict.summary, REVIEW_SUMMARY_MAX);
    verdict.findings.sort_by_key(|f| !f.blocking);
    let past = verdict.findings.len().saturating_sub(FINDINGS_MAX);
    verdict.findings.truncate(FINDINGS_MAX);
    for f in &mut verdict.findings {
        f.path = f.path.as_deref().map(|p| clipped(p, FINDING_PATH_MAX));
        f.severity = clipped(&f.severity, FINDING_PATH_MAX);
        f.body = clipped(&f.body, FINDING_MAX);
    }
    run.more = run.more.saturating_add(u16::try_from(past).unwrap_or(u16::MAX));
    run
}

#[cfg(test)]
mod tests;
