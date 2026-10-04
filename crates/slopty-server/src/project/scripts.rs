//! A project's scripts: the person's named commands (`docs/decisions/projects.md`, "A project
//! keeps the person's scripts").

use slopty_core::WallMs;
use slopty_proto::project::{SCRIPTS_MAX, Script};

use super::{Changed, Moment, ProjectId, ProjectStatus, Projects, Running, invalid};

impl Projects {
    /// Keep `script` in `id`, in place of one of its name.
    ///
    /// # Errors
    /// The project is unknown, the script cannot be kept ([`Script::refusal`]), or the project
    /// holds [`SCRIPTS_MAX`] others.
    pub(crate) fn set_script(
        &mut self,
        id: &ProjectId,
        script: Script,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        let Script { name, command, dir } = script;
        let dir = dir.map(|d| d.trim().trim_end_matches('/').to_owned());
        let script = Script { name, command: command.trim().to_owned(), dir };
        if let Some(why) = script.refusal() {
            return Err(invalid(why));
        }
        let record = self.record(id)?;
        let scripts = &mut record.project.scripts;
        let text = if let Some(held) = scripts.iter_mut().find(|s| s.name == script.name) {
            let text = format!("Script {} now runs `{}`.", script.name, script.command);
            *held = script;
            text
        } else {
            if scripts.len() >= SCRIPTS_MAX {
                return Err(invalid(format!("a project keeps at most {SCRIPTS_MAX} scripts")));
            }
            let text = format!("Script {} set: `{}`.", script.name, script.command);
            scripts.push(script);
            scripts.sort_by(|a, b| a.name.cmp(&b.name));
            text
        };
        let entry = record.log(None, Moment::Note { text }, now);
        let updates = vec![record.record_update(Some(entry))];
        Ok((self.status(id, None, running)?, updates))
    }

    /// Take script `name` away from `id`.
    ///
    /// # Errors
    /// The project or the script is unknown.
    pub(crate) fn delete_script(
        &mut self,
        id: &ProjectId,
        name: &str,
        running: &Running<'_>,
        now: WallMs,
    ) -> Changed<ProjectStatus> {
        let record = self.record(id)?;
        let before = record.project.scripts.len();
        record.project.scripts.retain(|s| s.name != name);
        if record.project.scripts.len() == before {
            return Err(invalid(format!("project {id} has no script {name}")));
        }
        let text = format!("Script {name} taken away.");
        let entry = record.log(None, Moment::Note { text }, now);
        let updates = vec![record.record_update(Some(entry))];
        Ok((self.status(id, None, running)?, updates))
    }
}
