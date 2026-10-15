//! The settings pages' calls: the agents a machine offers (Settings → Agents), as its worker's
//! caps name them, and the person's one setting for how freely they act (`[agents] autonomy`
//! in `settings.toml`, which the server reads for each start).
//!
//! An agent is offered wherever its program is installed, unless the person switched it off
//! here: the agents switched off are kept in `interface/agents-off.json` under Slopty's data
//! directory, and no picker of this app offers them. Slopty installs nothing and updates
//! nothing; the person installs an agent on the machine, and its worker says so.

use gpui::{AppContext as _, Context, Task};
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
            methods::GET_AGENT_SETTINGS => return Some(agent_settings(None, cx)),
            methods::SET_AGENT_SETTINGS => {
                let Some(autonomy) = super::param(params, "autonomy").map(str::to_owned) else {
                    return Some(Task::ready(Err(RpcError::BadParams("autonomy".to_owned()))));
                };
                return Some(agent_settings(Some(autonomy), cx));
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

/// `[agents] autonomy` as `settings.toml` holds it, after writing `set` there when given; off
/// the UI thread, since it is a file.
fn agent_settings(
    set: Option<String>,
    cx: &Context<WorkspaceView>,
) -> Task<Result<RpcReply, RpcError>> {
    cx.background_spawn(async move {
        let path = slopty_settings::path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(RpcError::Failed(format!("{}: {e}", path.display()))),
        };
        let autonomy = if let Some(set) = set {
            if !AUTONOMIES.contains(&set.as_str()) {
                return Err(RpcError::BadParams(format!("no autonomy {set}")));
            }
            let written = edit::write(&text, "agents", "autonomy", &edit::quoted(&set));
            if let Some(error) = slopty_settings::Settings::parse(&written).error {
                return Err(RpcError::Failed(error.to_string()));
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| RpcError::Failed(e.to_string()))?;
            }
            std::fs::write(&path, written).map_err(|e| RpcError::Failed(e.to_string()))?;
            set
        } else {
            match edit::read(&text, "agents", "autonomy") {
                Some(edit::Value::Str(now)) if AUTONOMIES.contains(&now.as_str()) => now,
                _ => AUTONOMIES[0].to_owned(),
            }
        };
        value(&serde_json::json!({ "autonomy": autonomy }))
    })
}
