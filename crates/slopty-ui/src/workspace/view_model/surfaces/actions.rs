//! A project's actions: the run scripts its repository keeps, and the person's own.
//!
//! The repository's run scripts ([`GitOp::Scripts`]) are each an action by its name, with an
//! icon from what it does. The person adds actions of their own, changes any action and removes
//! any, as zeron keeps them: on this app, beside the project, never in the repository. Changing
//! one of the repository's keeps the change in its place; removing one leaves it out here, and
//! changing it again brings it back. They are kept by machine and folder in
//! `interface/project-actions.json` under Slopty's data directory.
//!
//! An action marked to run when a worktree is made (one per project at most) runs in a chat's
//! new worktree once its thread started there, in a terminal of its own the shell shows
//! (`TakeProjectActionSetup`).

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gpui::{AppContext as _, Context, Task};
use serde::{Deserialize, Serialize};
use slopty_client::WorkerKey;
use slopty_proto::git::{GitDone, GitOp, GitOutcome, RunScript};
use slopty_view_proto::{
    ProjectAction, ProjectActionDraft, ProjectActionIcon, ProjectActionRun, ProjectActionsSnapshot,
};
use slopty_view_rpc::{RpcError, RpcReply};

use super::{param, value, within};
use crate::workspace::WorkspaceView;

/// The most actions of its own a project keeps.
const MOST: usize = 50;

/// The longest name an action has, in characters.
const NAME_MAX: usize = 80;

/// The longest command an action runs, in bytes.
const COMMAND_MAX: usize = 16 * 1024;

/// What the person made of one project's actions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Own {
    /// Their own actions, and their changes to the repository's (by its script's name).
    #[serde(default)]
    actions: Vec<ProjectAction>,
    /// The repository's they removed, by name.
    #[serde(default)]
    hidden: BTreeSet<String>,
}

/// The person's actions, by machine and folder, read from their file the first time they are
/// asked of.
#[derive(Debug, Default)]
pub(super) struct Actions {
    own: OnceCell<BTreeMap<String, Own>>,
}

/// Where the person's actions are kept.
fn actions_path() -> PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("project-actions.json")
}

/// A project's machine and folder, and the run scripts its repository keeps there.
type Scripts = (WorkerKey, String, Vec<RunScript>);

/// The key a project's actions are kept by: its machine and folder.
fn project_key(key: WorkerKey, path: &str) -> String {
    format!("{key}:{path}")
}

impl Actions {
    fn all(&self) -> &BTreeMap<String, Own> {
        self.own.get_or_init(|| {
            let path = actions_path();
            match std::fs::read(&path) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::warn!(path = %path.display(), %e, "the project actions do not read");
                    BTreeMap::new()
                }),
                Err(e) => {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(path = %path.display(), %e, "the project actions do not read");
                    }
                    BTreeMap::new()
                }
            }
        })
    }

    fn of(&self, project: &str) -> Own {
        self.all().get(project).cloned().unwrap_or_default()
    }

    /// Change the actions of `project` with `change`, kept off the UI thread when it changed
    /// them.
    fn change<T, R>(
        &mut self,
        project: &str,
        cx: &Context<T>,
        change: impl FnOnce(&mut Own) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        self.all();
        let Some(all) = self.own.get_mut() else {
            return Err(RpcError::Failed("the project actions are not here".to_owned()));
        };
        let mut own = all.get(project).cloned().unwrap_or_default();
        let before = own.clone();
        let answer = change(&mut own)?;
        if own != before {
            if own == Own::default() {
                all.remove(project);
            } else {
                all.insert(project.to_owned(), own);
            }
            let kept = all.clone();
            let path = actions_path();
            cx.background_spawn(async move {
                let written =
                    path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
                        let text =
                            serde_json::to_vec_pretty(&kept).map_err(std::io::Error::other)?;
                        std::fs::write(&path, text)
                    });
                if let Err(e) = written {
                    tracing::warn!(path = %path.display(), %e, "the project actions were not kept");
                }
            })
            .detach();
        }
        Ok(answer)
    }
}

/// A run script as the shell's project action: by its name, its icon from what it does.
fn action(script: &RunScript) -> ProjectAction {
    let said = format!("{} {}", script.name, script.line).to_lowercase();
    let icon = [
        ("test", ProjectActionIcon::Test),
        ("lint", ProjectActionIcon::Lint),
        ("clippy", ProjectActionIcon::Lint),
        ("build", ProjectActionIcon::Build),
        ("debug", ProjectActionIcon::Debug),
        ("setup", ProjectActionIcon::Configure),
        ("install", ProjectActionIcon::Configure),
    ]
    .into_iter()
    .find(|(word, _)| said.contains(word))
    .map_or(ProjectActionIcon::Play, |(_, icon)| icon);
    ProjectAction {
        id: script.name.clone(),
        name: script.name.clone(),
        command: script.line.clone(),
        icon,
        run_on_worktree_create: false,
    }
}

/// The actions a project shows: the repository's in its order, each as the person changed it
/// and less those they removed, then the person's own.
fn shown(scripts: &[RunScript], own: &Own) -> Vec<ProjectAction> {
    let changed = |id: &str| own.actions.iter().find(|a| a.id == id).cloned();
    let mut shown: Vec<ProjectAction> = scripts
        .iter()
        .filter(|script| !own.hidden.contains(&script.name))
        .map(|script| changed(&script.name).unwrap_or_else(|| action(script)))
        .collect();
    let theirs = own.actions.iter().filter(|a| !scripts.iter().any(|s| s.name == a.id));
    shown.extend(theirs.cloned());
    shown
}

/// What is wrong with `draft`, as the shell's editor says it.
fn invalid(draft: &ProjectActionDraft) -> Option<&'static str> {
    if draft.name.trim().is_empty() {
        Some("Action name is required")
    } else if draft.name.chars().count() > NAME_MAX {
        Some("Action name must not exceed 80 characters")
    } else if draft.command.trim().is_empty() {
        Some("Action command is required")
    } else if draft.command.len() > COMMAND_MAX {
        Some("Action command must not exceed 16384 bytes")
    } else {
        None
    }
}

/// Make `draft` the action `id` of `own` (a new one when `None`); `scripts` are the
/// repository's, which an id may name. The action as kept.
fn upsert(
    own: &mut Own,
    scripts: &[RunScript],
    id: Option<&str>,
    draft: &ProjectActionDraft,
) -> Result<ProjectAction, RpcError> {
    if let Some(why) = invalid(draft) {
        return Err(RpcError::Failed(why.to_owned()));
    }
    let known =
        |id: &str| own.actions.iter().any(|a| a.id == id) || scripts.iter().any(|s| s.name == id);
    let id = match id {
        Some(id) if known(id) => id.to_owned(),
        Some(id) => return Err(RpcError::Failed(format!("The project has no {id} any more"))),
        None => {
            if own.actions.iter().filter(|a| !scripts.iter().any(|s| s.name == a.id)).count()
                >= MOST
            {
                return Err(RpcError::Failed(format!("A project keeps at most {MOST} actions")));
            }
            format!("own-{}", slopty_proto::thread::IntentId::new())
        }
    };
    if draft.run_on_worktree_create {
        for other in &mut own.actions {
            other.run_on_worktree_create = false;
        }
    }
    let kept = ProjectAction {
        id: id.clone(),
        name: draft.name.trim().to_owned(),
        command: draft.command.trim().to_owned(),
        icon: draft.icon,
        run_on_worktree_create: draft.run_on_worktree_create,
    };
    own.hidden.remove(&id);
    match own.actions.iter_mut().find(|a| a.id == id) {
        Some(was) => *was = kept.clone(),
        None => own.actions.push(kept.clone()),
    }
    Ok(kept)
}

/// Remove the action `id` from what `own` shows.
fn delete(own: &mut Own, scripts: &[RunScript], id: &str) -> Result<(), RpcError> {
    let theirs = own.actions.iter().any(|a| a.id == id);
    own.actions.retain(|a| a.id != id);
    if scripts.iter().any(|s| s.name == id) {
        own.hidden.insert(id.to_owned());
    } else if !theirs {
        return Err(RpcError::Failed(format!("The project has no {id} any more")));
    }
    Ok(())
}

/// The person's action `action` as a script that runs in `cwd`: through the machine's login
/// shell, interactive, which takes the terminal over once it ends, as the repository's run
/// scripts do. The login shell is the machine's own, so it is named there, from `SHELL`.
fn own_script(action: &ProjectAction, cwd: String) -> RunScript {
    let then = format!("{}\nexec \"$SHELL\" -l", action.command);
    let start = r#"s="${SHELL:-/bin/sh}"; SHELL="$s" exec "$s" -l -i -c "$1""#;
    RunScript {
        name: action.name.clone(),
        line: action.command.clone(),
        command: ["/bin/sh", "-c", start, "sh", &then].map(str::to_owned).to_vec(),
        cwd,
        env: Vec::new(),
    }
}

impl WorkspaceView {
    /// The project `spaceId` names, and the run scripts its repository keeps there
    /// ([`GitOp::Scripts`]); none where it keeps none or is no repository.
    fn project_scripts(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<Scripts, RpcError>> {
        let Some((key, path)) = param(params, "spaceId").and_then(|space| self.view_space(space))
        else {
            return Task::ready(Err(RpcError::BadParams("a project's spaceId".to_owned())));
        };
        let rx = self.surface_git(key, path.clone(), GitOp::Scripts);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            GitOutcome::Done(GitDone::Scripts(scripts)) => Ok((key, path, scripts.list)),
            outcome => {
                tracing::debug!(?outcome, "a project's run scripts did not read");
                Ok((key, path, Vec::new()))
            }
        })
    }

    /// The project's actions after `change` to the person's own, with its run scripts read
    /// first: the snapshot the shell shows.
    fn project_actions_then(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
        change: impl FnOnce(&mut Own, &[RunScript]) -> Result<(), RpcError> + 'static,
    ) -> Task<Result<RpcReply, RpcError>> {
        let space_id = param(params, "spaceId").unwrap_or_default().to_owned();
        let scripts = self.project_scripts(params, cx);
        cx.spawn(async move |this, cx| {
            let (key, path, scripts) = scripts.await?;
            let project = project_key(key, &path);
            let own = this
                .update(cx, |view, cx| {
                    let actions = &mut view.view_model.surfaces.actions;
                    actions.change(&project, cx, |own| change(own, &scripts))?;
                    Ok::<_, RpcError>(actions.of(&project))
                })
                .map_err(super::gone)??;
            value(&ProjectActionsSnapshot {
                space_id,
                actions: shown(&scripts, &own),
                importable_actions: Vec::new(),
                project_file_issue: None,
            })
        })
    }

    /// `ListProjectActions`: the repository's run scripts and the person's own actions.
    pub(super) fn list_project_actions(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        self.project_actions_then(params, cx, |_, _| Ok(()))
    }

    /// `UpsertProjectAction`: an action of the person's own added, or any changed.
    pub(super) fn upsert_project_action(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let draft = params.get("action").cloned().map(serde_json::from_value::<ProjectActionDraft>);
        let Some(Ok(draft)) = draft else {
            return Task::ready(Err(RpcError::BadParams("action".to_owned())));
        };
        let id = param(params, "actionId").map(str::to_owned);
        self.project_actions_then(params, cx, move |own, scripts| {
            upsert(own, scripts, id.as_deref(), &draft).map(drop)
        })
    }

    /// `DeleteProjectAction`: an action removed from what the project shows.
    pub(super) fn delete_project_action(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(id) = param(params, "actionId").map(str::to_owned) else {
            return Task::ready(Err(RpcError::BadParams("actionId".to_owned())));
        };
        self.project_actions_then(params, cx, move |own, scripts| delete(own, scripts, &id))
    }

    /// `RunProjectAction`: the action `actionId` names, in a terminal of its own.
    pub(super) fn run_project_action(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(id) = param(params, "actionId").map(str::to_owned) else {
            return Task::ready(Err(RpcError::BadParams("actionId".to_owned())));
        };
        let scripts = self.project_scripts(params, cx);
        let params = params.clone();
        cx.spawn(async move |this, cx| {
            let (key, path, scripts) = scripts.await?;
            let opened = this
                .update(cx, |view, cx| {
                    let own = view.view_model.surfaces.actions.of(&project_key(key, &path));
                    let script = match own.actions.iter().find(|a| a.id == id) {
                        Some(action) => Some(own_script(action, path)),
                        None => scripts.into_iter().find(|script| script.name == id),
                    };
                    let script = script.ok_or_else(|| {
                        RpcError::Failed(format!("The project has no {id} any more"))
                    })?;
                    let name = script.name.clone();
                    Ok::<_, RpcError>((
                        name,
                        view.live_open_session(&params, Some((key, script)), cx),
                    ))
                })
                .map_err(super::gone)??;
            let (name, opened) = opened;
            let terminal = terminal_of(opened.await?)?;
            value(&ProjectActionRun { action_id: id, action_name: name, terminal })
        })
    }
}

/// The terminal a session opened for an action answered with.
fn terminal_of(opened: RpcReply) -> Result<slopty_view_proto::TerminalSession, RpcError> {
    let RpcReply::Value(terminal) = opened else {
        return Err(RpcError::Failed("the terminal did not open".to_owned()));
    };
    serde_json::from_value(terminal)
        .map_err(|e| RpcError::Failed(format!("the terminal did not read: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(name: &str, line: &str) -> RunScript {
        RunScript {
            name: name.to_owned(),
            line: line.to_owned(),
            command: vec!["sh".to_owned(), "-c".to_owned(), line.to_owned()],
            cwd: "/repo".to_owned(),
            env: Vec::new(),
        }
    }

    fn draft(name: &str, command: &str, on_worktree: bool) -> ProjectActionDraft {
        ProjectActionDraft {
            name: name.to_owned(),
            command: command.to_owned(),
            icon: ProjectActionIcon::Play,
            run_on_worktree_create: on_worktree,
        }
    }

    fn names(actions: &[ProjectAction]) -> Vec<(&str, &str)> {
        actions.iter().map(|a| (a.name.as_str(), a.command.as_str())).collect()
    }

    #[test]
    fn a_run_script_is_an_action_by_its_name_with_the_icon_of_what_it_does() {
        let dev = action(&script("dev", "bun run dev"));
        assert_eq!((dev.id.as_str(), dev.command.as_str()), ("dev", "bun run dev"));
        assert_eq!(dev.icon, ProjectActionIcon::Play);
        assert_eq!(action(&script("check", "cargo test --all")).icon, ProjectActionIcon::Test);
        assert_eq!(action(&script("ci", "cargo clippy")).icon, ProjectActionIcon::Lint);
    }

    /// The person's own actions follow the repository's; a change to one of the repository's
    /// stays in its place, a removed one is left out until it is changed again, and one of
    /// their own removed is gone.
    #[test]
    fn the_persons_actions_follow_the_repository_scripts_and_change_them_in_place() {
        let scripts = [script("dev", "bun run dev"), script("test", "bun test")];
        let mut own = Own::default();
        let added = upsert(&mut own, &scripts, None, &draft("Lint", "bun lint", false)).unwrap();
        assert_eq!(
            names(&shown(&scripts, &own)),
            [("dev", "bun run dev"), ("test", "bun test"), ("Lint", "bun lint")]
        );
        upsert(&mut own, &scripts, Some("dev"), &draft("dev", "bun run dev --open", false))
            .unwrap();
        delete(&mut own, &scripts, "test").unwrap();
        assert_eq!(
            names(&shown(&scripts, &own)),
            [("dev", "bun run dev --open"), ("Lint", "bun lint")]
        );
        upsert(&mut own, &scripts, Some("test"), &draft("test", "bun test --watch", false))
            .unwrap();
        delete(&mut own, &scripts, &added.id).unwrap();
        assert_eq!(
            names(&shown(&scripts, &own)),
            [("dev", "bun run dev --open"), ("test", "bun test --watch")]
        );
        delete(&mut own, &scripts, &added.id).expect_err("gone already");
        upsert(&mut own, &scripts, Some("nope"), &draft("x", "y", false)).expect_err("no such one");
        upsert(&mut own, &scripts, None, &draft(" ", "y", false)).expect_err("no name");
    }

    /// One action at most runs in a new worktree: marking another takes the mark off the first.
    #[test]
    fn one_action_runs_in_a_new_worktree() {
        let mut own = Own::default();
        let first = upsert(&mut own, &[], None, &draft("Setup", "bun i", true)).unwrap();
        let second = upsert(&mut own, &[], None, &draft("Seed", "bun seed", true)).unwrap();
        let marked: Vec<&str> = own
            .actions
            .iter()
            .filter(|a| a.run_on_worktree_create)
            .map(|a| a.id.as_str())
            .collect();
        assert_eq!(marked, [second.id.as_str()]);
        assert_ne!(first.id, second.id);
    }

    /// The person's action runs through the machine's login shell, which takes the terminal
    /// over once it ends.
    #[test]
    fn an_own_action_runs_in_the_login_shell_and_leaves_it() {
        let said = ProjectAction {
            id: "own-1".to_owned(),
            name: "Say".to_owned(),
            command: "echo slopty-ran".to_owned(),
            icon: ProjectActionIcon::Play,
            run_on_worktree_create: false,
        };
        let script = own_script(&said, "/w".to_owned());
        assert_eq!(script.cwd, "/w");
        let (program, args) = script.command.split_first().expect("a program");
        // Its stdin closed, the login shell it leaves to ends at once.
        let output = std::process::Command::new(program)
            .args(args)
            .env("SHELL", "/bin/sh")
            .output()
            .expect("sh runs");
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(out.contains("slopty-ran"), "{output:?}");
    }
}
