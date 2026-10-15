//! The settings pages' calls: the agents a machine offers (Settings → Agents), as its worker's
//! caps name them, and the person's one setting for how freely they act (`[agents] autonomy`
//! in the server's own `settings.toml`, read and written through the server, which reads it for
//! each start).
//!
//! An agent is offered wherever its program is installed, unless the person switched it off
//! here: the agents switched off are kept in `interface/agents-off.json` under Slopty's data
//! directory, and no picker of this app offers them. Slopty installs nothing and updates
//! nothing; the person installs an agent on the machine, and its worker says so.

use gpui::{Context, Task};
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::settings::SettingEdit;
use slopty_proto::thread::AgentId;
use slopty_settings::edit;
use slopty_view_proto::{HarnessId, ReasoningLevel, SteeringMode};
use slopty_view_rpc::{RpcError, RpcReply, methods};

use super::value;
use crate::workspace::WorkspaceView;

/// The agents every machine is offered with, as the shell names them, with the agent ids their
/// workers give; an ACP agent is offered where it is installed, by its registry's name.
const AGENTS: [(HarnessId, &str, &str); 3] = [
    (HarnessId::CLAUDE_CODE, "Claude Code", AgentId::CLAUDE_CODE),
    (HarnessId::CODEX, "Codex", AgentId::CODEX),
    (HarnessId::PI, "pi", AgentId::PI),
];

/// The efforts each agent takes.
pub(super) fn efforts(harness: HarnessId) -> Vec<ReasoningLevel> {
    use ReasoningLevel::{High, Low, Max, Medium, Minimal, XHigh};
    match harness {
        HarnessId::CLAUDE_CODE => vec![Low, Medium, High, XHigh, Max],
        HarnessId::CODEX | HarnessId::PI => vec![Minimal, Low, Medium, High, XHigh],
        _ => Vec::new(),
    }
}

impl WorkspaceView {
    /// A settings page's call; `None` for one no page of Slopty's makes.
    pub(super) fn settings_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        let answer = match method {
            methods::LIST_HARNESSES => {
                return Some(
                    self.once_known(params, cx, |view, params, _| value(&view.harnesses(params))),
                );
            }
            methods::SET_HARNESS_ENABLED => {
                let harness = super::param(params, "harness").map(str::to_owned);
                let enabled = params.get("enabled").and_then(serde_json::Value::as_bool);
                let (Some(harness), Some(enabled)) = (harness, enabled) else {
                    return Some(Task::ready(Err(RpcError::BadParams(
                        "harness and enabled".to_owned(),
                    ))));
                };
                let path = agents_off_path();
                if self.view_model.surfaces.agents_off.file(&path, harness, !enabled) {
                    self.view_model.surfaces.agents_off.keep(path, cx);
                }
                return Some(
                    self.once_known(params, cx, |view, params, _| value(&view.harnesses(params))),
                );
            }
            methods::INSTALL_HARNESS | methods::CANCEL_INSTALL => {
                Err(RpcError::Failed("Install the agent on the machine itself".to_owned()))
            }
            // Agents update themselves on their machines; there is nothing to watch or check.
            methods::WATCH_HARNESS_UPDATES => return Some(still_empty()),
            methods::CHECK_HARNESS_UPDATES => value(&Vec::<serde_json::Value>::new()),
            methods::GET_AGENT_SETTINGS => return Some(self.agent_settings(None, cx)),
            methods::SET_AGENT_SETTINGS => {
                let Some(autonomy) = super::param(params, "autonomy") else {
                    return Some(Task::ready(Err(RpcError::BadParams("autonomy".to_owned()))));
                };
                return Some(self.agent_settings(Some(autonomy), cx));
            }
            _ => return None,
        };
        Some(Task::ready(answer))
    }

    /// The agents of the machine a page targets (the first one linked when it names none), each
    /// installed when the machine's worker can start it.
    fn harnesses(&self, params: &serde_json::Value) -> Vec<serde_json::Value> {
        let key = self.surface_device(params).or_else(|| {
            self.workers.iter().find(|(_, worker)| worker.link.is_some()).map(|(key, _)| *key)
        });
        let startable = key.map(|key| self.startable_on(key)).unwrap_or_default();
        let off = self.view_model.surfaces.agents_off.set(&agents_off_path());
        let core = AGENTS.iter().map(|(harness, name, agent)| {
            (*harness, (*name).to_owned(), startable.iter().any(|id| id.is(agent)))
        });
        let acp = startable.iter().filter_map(|id| {
            let name = id.acp_name()?;
            Some((HarnessId::named(name), name.to_owned(), true))
        });
        core.chain(acp)
            .map(|(harness, name, installed)| {
                let on = !off.contains(harness.as_str());
                serde_json::json!({
                    "id": harness,
                    "name": name,
                    "supportsSteering": true,
                    "steeringMode": SteeringMode::StepBoundary,
                    "reasoningLevels": efforts(harness),
                    "installed": installed,
                    "canInstall": false,
                    "enabled": installed && on,
                })
            })
            .collect()
    }
}

/// Where the agents the person switched off are kept.
fn agents_off_path() -> std::path::PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("agents-off.json")
}

/// A watch of an empty list that stays open, so the page does not ask again.
fn still_empty() -> Task<Result<RpcReply, RpcError>> {
    use futures::StreamExt as _;
    let frames = futures::stream::once(async { serde_json::json!([]) })
        .chain(futures::stream::pending())
        .boxed();
    Task::ready(Ok(RpcReply::Stream(frames)))
}

/// The autonomies `[agents] autonomy` takes, as the file writes them.
const AUTONOMIES: [&str; 3] = ["auto", "full", "ask"];

/// `[agents] autonomy` as the server's settings file holds it: the server decides how far its
/// agents go, on whichever machine it runs.
fn autonomy_of(text: &str) -> String {
    match edit::read(text, "agents", "autonomy") {
        Some(edit::Value::Str(now)) if AUTONOMIES.contains(&now.as_str()) => now,
        _ => AUTONOMIES[0].to_owned(),
    }
}

impl WorkspaceView {
    /// `[agents] autonomy` of the server's own settings file, after writing `set` there when
    /// given, through the server ([`Verb::Settings`] of no worker), which takes its file up again.
    fn agent_settings(
        &self,
        set: Option<&str>,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        if let Some(set) = set.filter(|set| !AUTONOMIES.contains(set)) {
            return Task::ready(Err(RpcError::BadParams(format!("no autonomy {set}"))));
        }
        let Some(caller) = self.server.clone() else {
            return Task::ready(Err(RpcError::Failed(
                "Autonomy is the server's: link a server to set it".to_owned(),
            )));
        };
        let edits = set
            .iter()
            .map(|set| SettingEdit {
                table: "agents".to_owned(),
                key: "autonomy".to_owned(),
                entry: None,
                literal: Some(edit::quoted(set)),
            })
            .collect();
        cx.spawn(async move |_, _| match caller.call(Verb::Settings { of: None, edits }).await {
            Outcome::Settings(file) => {
                value(&serde_json::json!({ "autonomy": autonomy_of(&file.text) }))
            }
            Outcome::Error { message, .. } => Err(RpcError::Failed(message)),
            _ => Err(RpcError::Failed("the server answered something else".to_owned())),
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_proto::orchestration::{Outcome, Verb};
    use slopty_proto::settings::{DaemonSettings, SettingEdit};

    use crate::workspace::WorkspaceView;

    /// Autonomy is the server's own setting, read and written through it, wherever it runs.
    #[gpui::test]
    fn autonomy_is_set_on_the_server(cx: &mut gpui::TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        let (caller, mut queue) = slopty_client::server::ServerCaller::queued();
        workspace.update(cx, |v, _| v.server = Some(caller));
        let answer = workspace.update(cx, |v, cx| v.agent_settings(Some("full"), cx));
        cx.run_until_parked();
        let (verb, reply) = queue.try_next().expect("asked of the server");
        let edit = SettingEdit {
            table: "agents".to_owned(),
            key: "autonomy".to_owned(),
            entry: None,
            literal: Some("\"full\"".to_owned()),
        };
        assert_eq!(verb, Verb::Settings { of: None, edits: vec![edit] });
        let file = DaemonSettings {
            path: "/s/settings.toml".to_owned(),
            text: "[agents]\nautonomy = \"full\"\n".to_owned(),
            tables: Vec::new(),
            problems: Vec::new(),
        };
        let _sent = reply.send(Outcome::Settings(Box::new(file)));
        cx.run_until_parked();
        let answer = futures::FutureExt::now_or_never(answer).expect("answered");
        let Ok(slopty_view_rpc::RpcReply::Value(value)) = answer else { panic!("not a value") };
        assert_eq!(value["autonomy"], "full");
    }
}
