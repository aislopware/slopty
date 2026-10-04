//! A project's scripts, set and run by the person (`docs/decisions/projects.md`, "A project
//! keeps the person's scripts").

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::project::{Project, ProjectId, TaskId};

use super::{Hub, State, clone_on, error, keyed, live, remember};
use crate::hub::steps::said;
use crate::project::{Caller, Running};

/// What an agent is told when it asks to set or run a script.
const PERSON_S: &str = "a project's scripts are the person's shortcuts, run in terminals of \
                        their own; run the command yourself in your terminal, as with any other";

impl Hub {
    /// Keep, or take away, a script of a project ([`Verb::ScriptSet`],
    /// [`Verb::ScriptDelete`]), answered with the project.
    pub(in crate::hub) fn script_change(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        verb: &Verb,
    ) -> Outcome {
        if caller == Caller::Agent {
            return error(ErrorCode::Forbidden, PERSON_S);
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
            Verb::ScriptSet { project, script } => {
                state.projects.set_script(&project, script, &running, now)
            }
            Verb::ScriptDelete { project, name } => {
                state.projects.delete_script(&project, &name, &running, now)
            }
            _ => Err(error(ErrorCode::Invalid, "not a script's change")),
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
        outcome
    }

    /// Run script `name` of `project` in a terminal of the person's ([`Verb::ScriptRun`]):
    /// in `task`'s worktree on its worker, else in the project's folder on `worker`.
    pub(in crate::hub) async fn script_run(
        &self,
        caller: Caller,
        key: Option<IdempotencyKey>,
        (project, name): (ProjectId, String),
        worker: Option<WorkerId>,
        task: Option<TaskId>,
    ) -> Outcome {
        if caller == Caller::Agent {
            return error(ErrorCode::Forbidden, PERSON_S);
        }
        let start = {
            let state = self.inner.state.lock();
            script_start(&state, (&project, &name), worker, task)
        };
        match start {
            Ok(verb) => self.forward(key, verb).await,
            Err(refused) => refused,
        }
    }
}

/// The worker verb that runs script `name` of `project`, and where.
///
/// # Errors
/// The project, script or task is unknown, or there is no folder to run it in.
fn script_start(
    state: &State,
    (project, name): (&ProjectId, &str),
    worker: Option<WorkerId>,
    task: Option<TaskId>,
) -> Result<Verb, Outcome> {
    let refuse = |why: String| error(ErrorCode::Invalid, &why);
    let held = state.projects.project(project).map_err(|r| refuse(said(&r)))?;
    let Some(script) = held.scripts.iter().find(|s| s.name == name) else {
        let names: Vec<&str> = held.scripts.iter().map(|s| s.name.as_str()).collect();
        let has = if names.is_empty() { "none".to_owned() } else { names.join(", ") };
        return Err(refuse(format!("project {project} has no script {name}; it has {has}")));
    };
    let (worker, folder) = if let Some(task) = task {
        task_folder(state, project, task, worker)?
    } else {
        project_folder(state, held, worker)?
    };
    let cwd = match &script.dir {
        Some(dir) => format!("{}/{dir}", folder.trim_end_matches('/')),
        None => folder,
    };
    Ok(Verb::RunScript {
        worker,
        cwd,
        line: script.command.clone(),
        name: format!("{name} · {project}"),
        session: SessionId::new(),
    })
}

/// The worker and worktree of `task`, which `worker`, when named, must be.
///
/// # Errors
/// The task is unknown, has no worktree on a worker yet, or is on another worker.
fn task_folder(
    state: &State,
    project: &ProjectId,
    task: TaskId,
    worker: Option<WorkerId>,
) -> Result<(WorkerId, String), Outcome> {
    let refuse = |why: String| error(ErrorCode::Invalid, &why);
    let found = state.projects.task(project, task).map_err(|r| refuse(said(&r)))?;
    let on = found.assignment.as_ref().map(|a| a.term.worker);
    let (Some(on), Some(tree)) = (on, found.worktree.clone()) else {
        return Err(refuse(format!("task {task} has no worktree on a worker yet")));
    };
    if worker.is_some_and(|w| w != on) {
        return Err(refuse(format!("task {task}'s worktree is on another worker")));
    }
    Ok((on, tree))
}

/// The worker a project's script runs on (`worker`, else its orchestrator's) and its clone of
/// the project's repository there: the one a shell or the server made, else the project's
/// own path on the orchestrator's worker.
///
/// # Errors
/// No worker is named and the project has no orchestrator, or the worker has no clone.
fn project_folder(
    state: &State,
    held: &Project,
    worker: Option<WorkerId>,
) -> Result<(WorkerId, String), Outcome> {
    let refuse = |why: String| error(ErrorCode::Invalid, &why);
    let project = &held.id;
    let orchestrator = held.orchestrator.map(|o| o.worker);
    let Some(on) = worker.or(orchestrator) else {
        return Err(refuse(format!(
            "name a worker: project {project} has no orchestrator to run beside"
        )));
    };
    let own = (orchestrator == Some(on)
        && (held.repo.starts_with('/') || held.repo.starts_with('~')))
    .then(|| held.repo.clone());
    let Some(folder) = clone_on(state, held, on).or(own) else {
        let named = state.workers.get(&on).map_or_else(|| on.to_string(), |e| e.info.name.clone());
        return Err(refuse(format!(
            "{named} has no clone of project {project}'s repository; run it where one is, or \
             in a task's worktree"
        )));
    };
    Ok((on, folder))
}

#[cfg(test)]
mod tests;
