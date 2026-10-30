//! The settings pages' calls: the agents a machine offers (Settings → Agents), as its worker's
//! caps name them, and the person's one setting for how freely they act (`[agents] autonomy`
//! in the server's own `settings.toml`, read and written through the server, which reads it for
//! each start).
//!
//! An agent is offered wherever its program is installed, unless the person switched it off
//! here: the agents switched off are kept in `interface/agents-off.json` under Slopty's data
//! directory, and no picker of this app offers them.
//!
//! An agent missing on a machine is installed from here as its vendor documents it: Install runs
//! the vendor's installer in a terminal on that machine, through the person's login shell, and
//! the page shows that terminal, so the person watches it and answers it as on the machine
//! itself. Its worker sees the agent arrive as it sees any install, and offers it. Slopty
//! updates nothing: agents update themselves (`docs/decisions/ui.md`, "Agents are installed
//! from Settings").

use gpui::{Context, Task};
use slopty_proto::git::RunScript;
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::settings::SettingEdit;
use slopty_proto::thread::AgentId;
use slopty_settings::edit;
use slopty_view_proto::{HarnessId, ReasoningLevel, SteeringMode};
use slopty_view_rpc::{RpcError, RpcReply, methods};

use super::value;
use crate::workspace::WorkspaceView;

/// The agents every machine is offered with, as the shell names them, with the agent ids their
/// workers give and the vendor's documented install line; an ACP agent is offered where it is
/// installed, by its registry's name.
const AGENTS: [(HarnessId, &str, &str, &str); 3] = [
    (
        HarnessId::CLAUDE_CODE,
        "Claude Code",
        AgentId::CLAUDE_CODE,
        "curl -fsSL https://claude.ai/install.sh | bash",
    ),
    (HarnessId::CODEX, "Codex", AgentId::CODEX, "npm install -g @openai/codex"),
    (
        HarnessId::PI,
        "pi",
        AgentId::PI,
        "npm install -g --ignore-scripts @earendil-works/pi-coding-agent",
    ),
];

/// The terminal that installs `name` with `line`: the person's login shell runs it in their
/// home, so the installer finds the tools (`npm`, `curl`) their own terminal does.
fn installer(name: &str, line: &str) -> RunScript {
    let start = r#"s="${SHELL:-/bin/sh}"; SHELL="$s" exec "$s" -l -i -c "$1""#;
    RunScript {
        name: format!("Install {name}"),
        line: line.to_owned(),
        command: ["/bin/sh", "-c", start, "sh", line].map(str::to_owned).to_vec(),
        cwd: "~".to_owned(),
        env: Vec::new(),
    }
}

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
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        let answer = match method {
            methods::LIST_HARNESSES => {
                self.once_known(params, cx, |view, params, _| value(&view.harnesses(params)))
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
                self.once_known(params, cx, |view, params, _| value(&view.harnesses(params)))
            }
            methods::INSTALL_HARNESS => self.install_agent(params, cx),
            methods::GET_AGENT_SETTINGS => self.agent_settings(None, cx),
            methods::SET_AGENT_SETTINGS => {
                let Some(autonomy) = super::param(params, "autonomy") else {
                    return Some(Task::ready(Err(RpcError::BadParams("autonomy".to_owned()))));
                };
                self.agent_settings(Some(autonomy), cx)
            }
            _ => return None,
        };
        Some(answer)
    }

    /// The agents of the machine a page targets (the first one linked when it names none), each
    /// installed when the machine's worker can start it.
    fn harnesses(&self, params: &serde_json::Value) -> Vec<serde_json::Value> {
        let key = self.surface_device(params).or_else(|| {
            self.workers.iter().find(|(_, worker)| worker.link.is_some()).map(|(key, _)| *key)
        });
        let startable = key.map(|key| self.startable_on(key)).unwrap_or_default();
        let off = self.view_model.surfaces.agents_off.set(&agents_off_path());
        let reachable =
            key.and_then(|key| self.workers.get(&key)).is_some_and(|worker| worker.link.is_some());
        let core = AGENTS.iter().map(|(harness, name, agent, _)| {
            let installed = startable.iter().any(|id| id.is(agent));
            (*harness, (*name).to_owned(), installed, reachable)
        });
        let acp = startable.iter().filter_map(|id| {
            let name = id.acp_name()?;
            Some((HarnessId::named(name), name.to_owned(), true, false))
        });
        core.chain(acp)
            .map(|(harness, name, installed, can_install)| {
                let on = !off.contains(harness.as_str());
                serde_json::json!({
                    "id": harness,
                    "name": name,
                    "supportsSteering": true,
                    "steeringMode": SteeringMode::StepBoundary,
                    "reasoningLevels": efforts(harness),
                    "installed": installed,
                    "canInstall": can_install,
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

impl WorkspaceView {
    /// `InstallHarness`: the vendor's installer for `params.harness`, in a terminal of its own on
    /// the machine the page targets; answers `{terminalId}`, which the page shows and closes
    /// (`CloseTerminal`) to cancel.
    fn install_agent(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let harness = super::param(params, "harness").unwrap_or_default();
        let Some((_, name, _, line)) = AGENTS.iter().find(|(id, ..)| id.as_str() == harness) else {
            return Task::ready(Err(RpcError::BadParams(format!("no installer for {harness}"))));
        };
        let Some(key) = self.surface_device(params) else {
            return Task::ready(Err(RpcError::Failed("No machine to install it on".to_owned())));
        };
        let script = installer(name, line);
        let opened = self.live_open_session(&serde_json::json!({}), Some((key, script)), cx);
        cx.spawn(async move |_, _| {
            let RpcReply::Value(terminal) = opened.await? else {
                return Err(RpcError::Failed("the installer's terminal did not open".to_owned()));
            };
            let id = terminal.get("id").cloned().unwrap_or_default();
            value(&serde_json::json!({ "terminalId": id }))
        })
    }
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

    /// An installer runs the vendor's line through the person's login shell in their home; an
    /// agent with no installer, or no machine to run it on, is refused before anything opens.
    #[gpui::test]
    fn an_install_runs_the_vendor_line_or_says_why_not(cx: &mut gpui::TestAppContext) {
        let script = super::installer("Codex", "npm install -g @openai/codex");
        assert_eq!(script.name, "Install Codex");
        assert_eq!(script.cwd, "~");
        assert_eq!(script.command.first().map(String::as_str), Some("/bin/sh"));
        assert_eq!(script.command.last().map(String::as_str), Some("npm install -g @openai/codex"));
        assert!(script.command.iter().any(|part| part.contains("-l -i -c")), "a login shell");
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        let refused = |harness: &str, cx: &mut gpui::VisualTestContext| {
            let asked = serde_json::json!({ "harness": harness });
            let answer = workspace.update(cx, |v, cx| v.install_agent(&asked, cx));
            cx.run_until_parked();
            match futures::FutureExt::now_or_never(answer).expect("answered at once") {
                Err(error) => format!("{error:?}"),
                Ok(_) => panic!("{harness} opened a terminal"),
            }
        };
        assert!(refused("cursor", cx).contains("no installer for cursor"));
        assert!(refused("codex", cx).contains("No machine"));
    }

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
