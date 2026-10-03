//! Attempts at a task (`docs/decisions/projects.md`, "A task is tried by several agents at
//! once, and the attempt picked lands").
//!
//! A task is tried by up to [`ATTEMPTS_MAX`] agents at once, each an attempt: a sub-task of
//! [`ATTEMPT_KIND`] with the task's brief, placement and verifier, owning no paths, since every
//! attempt writes the same ones. Each is verified and reviewed as any task is, but none joins
//! the merge queue until it is picked ([`slopty_proto::project::Attempts::picked`]); the task tried
//! holds the paths. Picking one gives up every other, and once the attempt picked merges the task
//! tried is merged with it.

use slopty_core::WallMs;
use slopty_proto::orchestration::{ErrorCode, TermRef};
use slopty_proto::project::{ATTEMPT_KIND, ATTEMPTS_MAX, Merge, TaskSpec};

use super::{
    Change, Changed, Moment, ProjectId, Projects, Record, STATUS_MAX, Task, TaskId, TaskState,
    clipped, invalid, refuse, unknown_project,
};

/// What picking an attempt did: the task tried, as it is now, and every attempt given up for
/// it with the terminal its agent may still have open.
pub(crate) struct Picked {
    /// The task tried.
    pub task: Task,
    /// The attempts given up, each with its last assignment's terminal and whether that is
    /// open still.
    pub lost: Vec<(TaskId, Option<(TermRef, bool)>)>,
    /// Whether the attempt picked joined the merge queue now, its work checked already.
    pub queued: bool,
}

impl Record {
    /// The task `t` is an attempt at, when it is one: the task whose attempts name it.
    pub(super) fn tried(&self, t: &Task) -> Option<&Task> {
        let parent = self.task(t.parent?).ok()?;
        parent.attempts.as_ref().is_some_and(|a| a.tried.contains(&t.id)).then_some(parent)
    }

    /// Whether `t` is an attempt not picked: its work waits outside the merge queue.
    pub(super) fn unpicked(&self, t: &Task) -> bool {
        self.tried(t).and_then(|p| p.attempts.as_ref()).is_some_and(|a| a.picked != Some(t.id))
    }

    /// Whether `t` is an attempt given up for another picked.
    pub(super) fn lost(&self, t: &Task) -> bool {
        self.tried(t).and_then(|p| p.attempts.as_ref()?.picked).is_some_and(|picked| picked != t.id)
    }

    /// The task tried, merged with the attempt `attempt` picked for it, which merged now.
    pub(super) fn merged_with(&mut self, attempt: &Task, now: WallMs) -> Option<Change> {
        let parent = self.tried(attempt)?;
        let picked = parent.attempts.as_ref()?.picked == Some(attempt.id);
        if !picked || parent.state == TaskState::Merged {
            return None;
        }
        let id = parent.id;
        let t = self.task_mut(id).ok()?;
        let from = t.state;
        t.state = TaskState::Merged;
        t.merge.clone_from(&attempt.merge);
        t.updated_ms = now;
        let t = t.clone();
        let entry = self.log(Some(id), Moment::State { from, to: TaskState::Merged }, now);
        Some(self.task_update(&t, Some(entry)))
    }
}

impl Projects {
    /// Make `n` attempts at `task`, beside any it has: sub-tasks of [`ATTEMPT_KIND`] with its
    /// brief, placement, verifier and dependencies, owning nothing. The task, which no agent
    /// works on, goes running with them. Answers the attempts made.
    ///
    /// # Errors
    /// Refused when the task is an attempt itself, merged, worked on by an agent of its own or
    /// tried by an attempt picked already, or when the attempts would pass [`ATTEMPTS_MAX`],
    /// the project's tasks or its depth.
    pub(crate) fn attempt(
        &mut self,
        id: &ProjectId,
        task: TaskId,
        n: usize,
        now: WallMs,
    ) -> Changed<Vec<TaskId>> {
        let bounds = self.policy.bounds_for(Some(id));
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let t = record.task(task)?;
        if let Some(parent) = record.tried(t) {
            return Err(invalid(format!(
                "task {task} is an attempt at task {}; make more attempts at that",
                parent.id
            )));
        }
        if t.state == TaskState::Merged {
            return Err(invalid(format!("task {task} is merged; make a new task")));
        }
        if let Some(a) = t.assignment.as_ref().filter(|a| a.open()) {
            return Err(refuse(
                ErrorCode::Conflict,
                format!(
                    "task {task} has an agent of its own, {}/{}; attempts try a task no agent \
                     works on",
                    a.term.worker, a.term.session
                ),
            ));
        }
        let held = t.attempts.clone().unwrap_or_default();
        if let Some(picked) = held.picked {
            return Err(invalid(format!("attempt {picked} at task {task} is picked already")));
        }
        let total = held.tried.len().saturating_add(n);
        if n == 0 || total > ATTEMPTS_MAX {
            return Err(invalid(format!(
                "a task is tried by 1 to {ATTEMPTS_MAX} attempts, and task {task} has {} already",
                held.tried.len()
            )));
        }
        let most = usize::try_from(bounds.tasks_per_project).unwrap_or(usize::MAX);
        if record.tasks.len().saturating_add(n) > most {
            return Err(super::over_bound(
                "tasks_per_project",
                record.tasks.len().saturating_add(n),
                bounds.tasks_per_project.into(),
            ));
        }
        let title_max = usize::try_from(bounds.title_max).unwrap_or(usize::MAX);
        let specs: Vec<TaskSpec> = (held.tried.len().saturating_add(1)..=total)
            .map(|k| TaskSpec {
                parent: Some(task),
                depends_on: t.depends_on.clone(),
                kind: ATTEMPT_KIND.to_owned(),
                title: clipped(&format!("Attempt {k}: {}", t.title), title_max),
                brief: t.brief.clone(),
                owns: Vec::new(),
                read_only: t.read_only,
                placement: t.placement.clone(),
                verifier: t.verifier.clone(),
                metadata: None,
            })
            .collect();
        let mut made = Vec::with_capacity(n);
        let mut changes = Vec::new();
        for spec in specs {
            let (attempt, change) = self.create_task(id, spec, now)?;
            made.push(attempt.id);
            changes.extend(change);
        }
        let record = self.record(id)?;
        let t = record.task_mut(task)?;
        t.attempts.get_or_insert_default().tried.extend(&made);
        let from = t.state;
        if matches!(from, TaskState::Planned | TaskState::Failed) {
            t.state = TaskState::Running;
        }
        t.updated_ms = now;
        let t = t.clone();
        let named: Vec<String> = made.iter().map(ToString::to_string).collect();
        let text = format!("Tried by attempts {} at once.", named.join(", "));
        let entry = record.log(Some(task), Moment::Note { text }, now);
        changes.push(record.task_update(&t, Some(entry)));
        Ok((made, changes))
    }

    /// Say why `attempt` did not start, on its card and its timeline.
    pub(crate) fn not_started(
        &mut self,
        id: &ProjectId,
        attempt: TaskId,
        why: &str,
        now: WallMs,
    ) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Ok(t) = record.task_mut(attempt) else { return Vec::new() };
        let text = clipped(&format!("Not started: {why}"), STATUS_MAX);
        t.status = Some(text.clone());
        t.updated_ms = now;
        let t = t.clone();
        let entry = record.log(Some(attempt), Moment::Note { text }, now);
        vec![record.task_update(&t, Some(entry))]
    }

    /// Pick `attempt` to land for the task it tries: every other attempt is given up, and
    /// one picked whose work is done and checked joins the merge queue now. Picking the one
    /// picked again changes nothing.
    ///
    /// # Errors
    /// Refused when `attempt` is no attempt, or another was picked already.
    pub(crate) fn pick(&mut self, id: &ProjectId, attempt: TaskId, now: WallMs) -> Changed<Picked> {
        let record = self.record(id)?;
        let t = record.task(attempt)?;
        let Some(parent) = record.tried(t) else {
            return Err(invalid(format!(
                "task {attempt} is no attempt; task_attempts makes attempts at a task"
            )));
        };
        let (task, attempts) = (parent.id, parent.attempts.clone().unwrap_or_default());
        match attempts.picked {
            Some(picked) if picked == attempt => {
                let picked = Picked { task: parent.clone(), lost: Vec::new(), queued: false };
                return Ok((picked, Vec::new()));
            }
            Some(picked) => {
                return Err(invalid(format!("attempt {picked} at task {task} is picked already")));
            }
            None => {}
        }
        let mut changes = Vec::new();
        let mut lost = Vec::new();
        let why =
            clipped(&format!("Given up: attempt {attempt} lands for task {task}."), STATUS_MAX);
        for other in attempts.tried.iter().copied().filter(|a| *a != attempt) {
            let Ok(o) = record.task_mut(other) else { continue };
            if matches!(o.state, TaskState::Merged | TaskState::Failed) && o.assignment.is_none() {
                continue;
            }
            let from = o.state;
            o.state = TaskState::Failed;
            o.status = Some(why.clone());
            o.merge = None;
            o.updated_ms = now;
            lost.push((other, o.assignment.as_ref().map(|a| (a.term, a.open()))));
            let o = o.clone();
            let moment = if from == TaskState::Failed {
                Moment::Note { text: why.clone() }
            } else {
                Moment::State { from, to: TaskState::Failed }
            };
            let entry = record.log(Some(other), moment, now);
            changes.push(record.task_update(&o, Some(entry)));
        }
        let p = record.task_mut(task)?;
        p.attempts.get_or_insert_default().picked = Some(attempt);
        p.updated_ms = now;
        let p = p.clone();
        let text = format!("Attempt {attempt} picked to land.");
        let entry = record.log(Some(task), Moment::Note { text }, now);
        changes.push(record.task_update(&p, Some(entry)));
        let a = record.task_mut(attempt)?;
        let checked = a.verified.as_ref().is_some_and(|r| r.passed) || a.reviewed.is_some();
        let queued = a.state == TaskState::Done && a.merge.is_none() && checked;
        if queued {
            a.merge = Some(Merge::Queued { since_ms: now });
            a.updated_ms = now;
            let a = a.clone();
            changes.push(record.task_update(&a, None));
        }
        Ok((Picked { task: p, lost, queued }, changes))
    }
}

#[cfg(test)]
mod tests;
