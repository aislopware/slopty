//! "Run": the repository's own run scripts (`GitOp::Scripts`), each opened in a terminal of
//! its own on the machine the focus works on.
//!
//! The scripts are the ones the repository's other tools keep (a dev server, a test watch,
//! what their Run button starts), read on the worker from the checkout the focus works in: a
//! folder or changes tile's folder, a terminal's directory, or where a thread's agent works.
//! One script opens at once; several are a step that lists each by its name, its script muted
//! beside it, the default first. The scripts read last for that folder answer at once while
//! the worker reads them again, so a second Run is a keystroke away; a step up when the fresh
//! list comes shows it. A terminal opens through the person's login shell with the script's
//! command, folder, environment and name, as `ClientMsg::OpenSession` carries them, so the
//! shell takes the terminal over once the script ends.

use std::sync::Arc;

use gpui::{Context, Entity, EntityId, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::git::{GitOp, RunScript, RunScripts};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::{OpenSession, TermSize};

use super::WorkspaceView;
use super::actions::{RunHere, RunScriptOn};
use crate::conversation::thread::git::Said;
use crate::palette::{CommandPalette, PaletteEvent, PaletteItem};

/// The palette's line.
pub(crate) const RUN: &str = "Run";

/// The step's field, while it lists the scripts.
const PICK: &str = "Run a script";

/// What the step says while the machine reads the scripts.
const READING: &str = "Reading the run scripts\u{2026}";

/// A Run whose scripts the machine has not answered yet.
pub(super) struct RunAsked {
    /// The machine.
    key: WorkerKey,
    /// The folder they were asked in.
    path: String,
    /// The step that waits for them, while it is up.
    step: Option<EntityId>,
    /// The step says the machine is reading them, having no list to show meanwhile.
    reading: bool,
}

/// A script's line in the step: its name, its script muted beside it.
fn script_line(key: WorkerKey, script: &RunScript) -> PaletteItem {
    let action = Box::new(RunScriptOn { worker: key, script: script.clone() });
    PaletteItem::new(&script.name, action, &[]).placed(Some(script.line.clone()))
}

/// What a repository with no run scripts says.
fn none_kept(path: &str) -> String {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
    format!("{name} keeps no run scripts")
}

impl WorkspaceView {
    /// The folder "Run" reads the scripts in, with its machine: the focused folder or changes
    /// tile's folder, else where the focused tile works (a terminal's directory, a thread's
    /// agent's).
    pub(super) fn run_here(&self) -> Option<(WorkerKey, String)> {
        let tile = self.focused()?;
        let item = self.item(tile)?;
        let path = match &item.kind {
            ItemKind::Folder { path } | ItemKind::Changes { path, .. } => path.clone(),
            _ => self.cwd_of(item)?,
        };
        Some((tile.worker, path))
    }

    /// "Run": the scripts of the focus's checkout, asked of its machine. The last read of them
    /// answers at once: one opens, several are a step. With none read yet, a step says the
    /// machine is reading them until it answers.
    pub(super) fn run_here_action(
        &mut self,
        _: &RunHere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, path)) = self.run_here() else { return };
        if !self.workers.get(&key).is_some_and(super::Worker::is_linked) {
            let text = format!("{} is out of reach", self.worker_name(key));
            self.show_notice(text, cx);
            return;
        }
        let hub = self.thread_hub(key, cx);
        let known = hub.read(cx).git().repo(&path).and_then(|r| r.scripts.clone());
        tracing::info!(%key, %path, known = known.is_some(), "run");
        let _asked = hub.update(cx, |hub, cx| hub.git_op(&path, GitOp::Scripts, cx));
        match known.filter(|s| !s.list.is_empty()) {
            Some(scripts) if scripts.list.len() == 1 => {
                if let Some(script) = scripts.list.first() {
                    self.open_script(key, script, cx);
                }
            }
            Some(scripts) => {
                let lines = scripts.list.iter().map(|s| script_line(key, s)).collect();
                self.open_step(lines, PICK, window, cx);
                let step = self.palette.as_ref().map(Entity::entity_id);
                self.run_asked = Some(RunAsked { key, path, step, reading: false });
            }
            None => {
                self.open_step(Vec::new(), PICK, window, cx);
                if let Some(palette) = self.palette.clone() {
                    palette.update(cx, |p, cx| p.set_empty(READING, cx));
                }
                let step = self.palette.as_ref().map(Entity::entity_id);
                self.run_asked = Some(RunAsked { key, path, step, reading: true });
            }
        }
    }

    /// A script picked in the step.
    pub(super) fn run_script_on(
        &mut self,
        ask: &RunScriptOn,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_script(ask.worker, &ask.script, cx);
    }

    /// Open `script` on `key` in a terminal of its own, beside the focus.
    fn open_script(&mut self, key: WorkerKey, script: &RunScript, cx: &mut Context<Self>) {
        tracing::info!(%key, name = %script.name, cwd = %script.cwd, "run script");
        let spec = OpenSession {
            size: TermSize::default(),
            cwd: Some(script.cwd.clone()),
            command: script.command.clone(),
            env: script.env.clone(),
            title: Some(script.name.clone()),
            attach: false,
        };
        self.ask_session(key, spec, super::tabs::Opening::Beside, cx);
    }

    /// The link to `key` went with a Run's scripts still out: its step says so.
    pub(super) fn run_lost(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        if self.run_asked.as_ref().is_none_or(|a| a.key != key) {
            return;
        }
        let step = self.run_step();
        self.run_asked = None;
        if let Some(step) = step {
            let text =
                format!("{} went out of reach before its run scripts came", self.worker_name(key));
            step.update(cx, |p, cx| p.set_empty(text, cx));
        }
    }

    /// The step waiting on the scripts, while it is up.
    fn run_step(&self) -> Option<Entity<CommandPalette>> {
        let step = self.run_asked.as_ref()?.step?;
        self.palette.clone().filter(|p| p.entity_id() == step)
    }

    /// `key`'s repository `repo` moved: a Run waiting on its scripts has them. Its step, still
    /// up, lists them, or says there are none or why they could not be read; one that was
    /// reading them opens the one there is. A step that showed the last read takes the fresh
    /// list in its place, and opens nothing the person did not pick.
    pub(super) fn scripts_heard(&mut self, key: WorkerKey, repo: &str, cx: &mut Context<Self>) {
        if !self.run_asked.as_ref().is_some_and(|a| a.key == key && a.path == repo) {
            return;
        }
        let Some(hub) = self.held_hub(key) else { return };
        let git = hub.read(cx).git();
        if git.asking(repo, &GitOp::Scripts) {
            return;
        }
        let found = git.repo(repo);
        let scripts: Option<Arc<RunScripts>> = found.and_then(|r| r.scripts.clone());
        let why = found.and_then(|r| r.said.as_ref()).and_then(|(_, said)| match said {
            Said::Refused { why } | Said::Failed { said: why } => Some(why.clone()),
            _ => None,
        });
        let step = self.run_step();
        let reading = self.run_asked.take().is_some_and(|a| a.reading);
        let Some(step) = step else { return };
        let Some(scripts) = scripts else {
            let why = why.unwrap_or_else(|| "the machine did not answer".to_owned());
            let text = format!("The run scripts could not be read: {why}");
            step.update(cx, |p, cx| p.set_empty(text, cx));
            return;
        };
        match scripts.list.as_slice() {
            [] => step.update(cx, |p, cx| p.set_empty(none_kept(repo), cx)),
            [one] if reading => {
                self.palette_action =
                    Some(Box::new(RunScriptOn { worker: key, script: one.clone() }));
                step.update(cx, |_, cx| cx.emit(PaletteEvent::Dismiss));
            }
            several => {
                let lines = several.iter().map(|s| script_line(key, s)).collect();
                step.update(cx, |p, cx| p.set_items(lines, cx));
            }
        }
    }
}
