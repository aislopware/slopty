//! Tasks a project runs on a schedule (`docs/decisions/projects.md`, "A project runs tasks on
//! a schedule the person sets").
//!
//! A schedule keeps a task's spec, what starts for it, and when ([`When`], read in the
//! person's time zone). Each time it falls due the server makes the task and starts it as a
//! `task_spawn` would, under the project's placement, limits and budget. A run whose last
//! task is still under way is skipped, so runs never pile up, and a run missed while the
//! server was down runs once when it is back. Only the person sets a schedule, since every
//! run spends the plan.

use jiff::Timestamp;
use slopty_core::WallMs;
use slopty_proto::project::{
    SCHEDULES_MAX, Schedule, ScheduleRun, ScheduleSpec, TaskLaunch, WHEN_MAX, ZONE_MAX,
};

use super::when::{When, zone};
use super::{
    Change, Changed, Moment, ProjectId, ProjectStatus, Projects, Refused, Running, STATUS_MAX,
    Task, TaskState, clipped, invalid, unknown_project,
};

/// What makes a schedule's task its own: its number, as the task's metadata says.
pub(crate) const SCHEDULE_KEY: &str = "schedule";

/// `ms` as jiff's timestamp.
fn stamp(ms: WallMs) -> Timestamp {
    i64::try_from(ms.as_millis())
        .ok()
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .unwrap_or(Timestamp::MAX)
}

/// `at` as the server's clock reads it.
fn wall(at: Timestamp) -> WallMs {
    WallMs::from_millis(u64::try_from(at.as_millisecond()).unwrap_or(0))
}

/// The server's own time zone, by its IANA name: UTC when it has none.
fn own_zone() -> String {
    jiff::tz::TimeZone::system().iana_name().unwrap_or("UTC").to_owned()
}

/// When `spec` runs next after `now`: none while it is paused.
///
/// # Errors
/// Its rule or zone is wrong, or it names no time to come.
fn next_of(spec: &ScheduleSpec, now: WallMs) -> Result<Option<WallMs>, Refused> {
    let when = When::parse(&spec.when).map_err(invalid)?;
    let zone = zone(&spec.zone).map_err(invalid)?;
    if spec.paused {
        return Ok(None);
    }
    let next = when.next(stamp(now), &zone).ok_or_else(|| {
        invalid(format!("{:?} names no time in the next eight years", spec.when.trim()))
    })?;
    Ok(Some(wall(next)))
}

/// `spec` in words, for the timeline.
fn words(spec: &ScheduleSpec) -> String {
    let paused = if spec.paused { ", paused" } else { "" };
    format!("\"{}\" at {} in {}{paused}", spec.task.title, spec.when, spec.zone)
}

/// Whether the task a run made is still under way.
const fn under_way(t: &Task) -> bool {
    !matches!(t.state, TaskState::Done | TaskState::Merged | TaskState::Failed)
}

impl Projects {
    /// Set a schedule of `id`: schedule `number` anew, or a new one when none is named.
    ///
    /// # Errors
    /// The spec is wrong (its task as [`Self::create_task`] would refuse it, its rule, its
    /// zone), the schedule is unknown, or the project has [`SCHEDULES_MAX`] already.
    pub(crate) fn set_schedule(
        &mut self,
        id: &ProjectId,
        number: Option<u32>,
        spec: ScheduleSpec,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        let spec = self.checked_schedule(id, spec)?;
        let next_ms = next_of(&spec, now)?;
        let record = self.record(id)?;
        let schedules = &mut record.project.schedules;
        let text = if let Some(n) = number {
            let held = schedules
                .iter_mut()
                .find(|s| s.id == n)
                .ok_or_else(|| invalid(format!("project {id} has no schedule {n}")))?;
            let text = format!("Schedule {n} is now {}.", words(&spec));
            held.spec = spec;
            held.next_ms = next_ms;
            text
        } else {
            if schedules.len() >= SCHEDULES_MAX {
                return Err(invalid(format!("a project keeps at most {SCHEDULES_MAX} schedules")));
            }
            let n = schedules.iter().map(|s| s.id).max().unwrap_or(0).saturating_add(1);
            let text = format!("Schedule {n} set: {}.", words(&spec));
            schedules.push(Schedule { id: n, spec, next_ms, last: None, created_ms: now });
            text
        };
        let entry = record.log(None, Moment::Note { text }, now);
        let updates = vec![record.record_update(Some(entry))];
        Ok((self.status(id, None, running)?, updates))
    }

    /// Take schedule `number` of `id` away; the tasks it made stay.
    ///
    /// # Errors
    /// The schedule is unknown.
    pub(crate) fn delete_schedule(
        &mut self,
        id: &ProjectId,
        number: u32,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        let record = self.record(id)?;
        let before = record.project.schedules.len();
        record.project.schedules.retain(|s| s.id != number);
        if record.project.schedules.len() == before {
            return Err(invalid(format!("project {id} has no schedule {number}")));
        }
        let text = format!("Schedule {number} taken away.");
        let entry = record.log(None, Moment::Note { text }, now);
        let updates = vec![record.record_update(Some(entry))];
        Ok((self.status(id, None, running)?, updates))
    }

    /// The schedules due at `now`, each one's next run moved past it; the changes that keeps,
    /// and when the next falls due.
    pub(crate) fn schedules_due(
        &mut self,
        now: WallMs,
    ) -> (Vec<(ProjectId, u32)>, Vec<Change>, Option<WallMs>) {
        let mut due = Vec::new();
        let mut changes = Vec::new();
        for (id, record) in &mut self.records {
            let mut moved = false;
            for schedule in &mut record.project.schedules {
                if schedule.next_ms.is_some_and(|at| at <= now) {
                    due.push((id.clone(), schedule.id));
                    schedule.next_ms = next_of(&schedule.spec, now).ok().flatten();
                    moved = true;
                }
            }
            if moved {
                changes.push(record.record_update(None));
            }
        }
        let next = self
            .records
            .values()
            .flat_map(|r| r.project.schedules.iter().filter_map(|s| s.next_ms))
            .min();
        (due, changes, next)
    }

    /// Make the task of a run of schedule `number`, with what starts for it. A run whose last
    /// task is under way makes none.
    ///
    /// # Errors
    /// Why no task was made: the schedule is unknown, its last run is under way, or the task
    /// was refused ([`Self::create_task`]).
    pub(crate) fn run_schedule(
        &mut self,
        id: &ProjectId,
        number: u32,
        now: WallMs,
    ) -> Changed<(Task, TaskLaunch)> {
        let record = self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let schedule = record
            .project
            .schedules
            .iter()
            .find(|s| s.id == number)
            .ok_or_else(|| invalid(format!("project {id} has no schedule {number}")))?;
        let last = schedule.last.as_ref().and_then(|l| l.task);
        if let Some(going) = last.and_then(|t| record.task(t).ok()).filter(|t| under_way(t)) {
            return Err(invalid(format!(
                "its last run, task {}, is still under way, so this run is skipped",
                going.id
            )));
        }
        let mut spec = schedule.spec.task.clone();
        spec.metadata = Some(format!("{{\"{SCHEDULE_KEY}\":{number}}}"));
        let launch = schedule.spec.launch.clone();
        let (task, mut changes) = self.create_task(id, spec, now)?;
        changes.extend(self.schedule_ran(id, number, Some(task.id), None, now));
        Ok(((task, launch), changes))
    }

    /// Keep how a run of schedule `number` went: the task it made, and why it made none or
    /// could not start it.
    pub(crate) fn schedule_ran(
        &mut self,
        id: &ProjectId,
        number: u32,
        task: Option<super::TaskId>,
        why: Option<&str>,
        now: WallMs,
    ) -> Vec<Change> {
        let Ok(record) = self.record(id) else { return Vec::new() };
        let Some(schedule) = record.project.schedules.iter_mut().find(|s| s.id == number) else {
            return Vec::new();
        };
        let why = why.map(|w| clipped(w, STATUS_MAX));
        schedule.last = Some(ScheduleRun { at_ms: now, task, why: why.clone() });
        let Some(why) = why else { return vec![record.record_update(None)] };
        let text = format!("Schedule {number} ran, but {why}");
        let entry = record.log(task, Moment::Note { text }, now);
        vec![record.record_update(Some(entry))]
    }

    /// `spec` as a schedule keeps it, checked as its task would be when made: its zone named,
    /// the server's own when it named none.
    fn checked_schedule(
        &self,
        id: &ProjectId,
        spec: ScheduleSpec,
    ) -> Result<ScheduleSpec, Refused> {
        let bounds = self.policy.bounds_for(Some(id));
        self.records.get(id).ok_or_else(|| unknown_project(id))?;
        let ScheduleSpec { mut task, launch, when, zone: named, paused } = spec;
        if task.parent.is_some() || !task.depends_on.is_empty() {
            return Err(invalid(
                "a schedule's task hangs from no other and depends on none: each run stands alone",
            ));
        }
        task.title = super::titled(&task.title, bounds)?;
        if task.brief.len() > usize::try_from(bounds.brief_max).unwrap_or(usize::MAX) {
            return Err(super::over_bound("brief_max", task.brief.len(), bounds.brief_max.into()));
        }
        let kind = task.kind.trim().to_owned();
        task.kind = kind;
        if task.kind.len() > super::KIND_MAX {
            return Err(invalid(format!("a kind is at most {} bytes", super::KIND_MAX)));
        }
        crate::placement::check(&task.placement, bounds.comprehension_depth)?;
        task.owns = super::claimable(&task.owns, bounds.owns_max)?;
        if task.read_only && !task.owns.is_empty() {
            return Err(invalid("a read-only task owns no paths"));
        }
        task.verifier = super::verifier(task.verifier)?;
        task.metadata = None;
        let when = when.trim().to_owned();
        if when.len() > WHEN_MAX {
            return Err(invalid(format!("a schedule's rule is at most {WHEN_MAX} bytes")));
        }
        let zone = Some(named.trim().to_owned()).filter(|z| !z.is_empty()).unwrap_or_else(own_zone);
        if zone.len() > ZONE_MAX {
            return Err(invalid(format!("a time zone's name is at most {ZONE_MAX} bytes")));
        }
        Ok(ScheduleSpec { task, launch, when, zone, paused })
    }
}

#[cfg(test)]
mod tests;
