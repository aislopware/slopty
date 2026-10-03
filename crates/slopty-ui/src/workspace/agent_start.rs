//! "New agent…" (⌘⇧T), the one way to start an agent: the palette asks which agent, then on
//! which machine, then in which folder, and the agent's thread opens in a tile of its own. Each
//! step lists the last choice first, so ↩ ↩ ↩ starts the last combination again, and a step
//! with one choice is passed over. The palette's "New `agent` agent" lines start at the machine.
//!
//! What a machine can start comes from its own link (the agents its capabilities found
//! installed) and from the server's facts about it, so a machine reached with no server still
//! offers what it has. The folders are the focused shell's on that machine, then the last start's
//! there, then where its shells stand, most recent first, then its home.

use gpui::{AppContext as _, Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::agent::AgentKind;
use slopty_proto::thread::AgentId;

use super::WorkspaceView;
use super::actions::{NewAgent, NewAgentOf, NewAgentOn, StartThread};
use super::projects::agent_label;
use crate::icons::{Glyph, IconName};
use crate::palette::{CommandPalette, PaletteItem};

/// What is said when no machine can start an agent.
pub(super) const NO_AGENT: &str = "No machine has an agent to start";

/// What the agent step's field says.
const PICK_AGENT: &str = "Which agent";

/// What the machine step's field says.
const PICK_MACHINE: &str = "On which machine";

/// What the folder step's field says.
const PICK_FOLDER: &str = "In which folder";

/// The last start: what each step lists first next time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct LastStart {
    pub agent: AgentId,
    pub worker: WorkerKey,
    pub cwd: String,
}

impl WorkspaceView {
    /// The agents `key` can start a thread of while its link is up: those its link found
    /// installed, then those the server's facts name, each once.
    pub(super) fn startable_on(&self, key: WorkerKey) -> Vec<AgentId> {
        let Some(w) = self.workers.get(&key).filter(|w| w.link.is_some()) else {
            return Vec::new();
        };
        let installed = w.caps.iter().flat_map(|c| &c.agents).map(|a| match a.kind {
            AgentKind::ClaudeCode => AgentId::named(AgentId::CLAUDE_CODE),
        });
        let mut out: Vec<AgentId> = Vec::new();
        for agent in installed.chain(self.agents_on(key).iter().cloned()) {
            if !out.contains(&agent) {
                out.push(agent);
            }
        }
        out
    }

    /// Every agent some machine can start, each once.
    pub(super) fn startable_agents(&self) -> Vec<AgentId> {
        let mut out: Vec<AgentId> = Vec::new();
        for key in self.workers.keys() {
            for agent in self.startable_on(*key) {
                if !out.contains(&agent) {
                    out.push(agent);
                }
            }
        }
        out
    }

    /// The agent a start on `key` with no step of its own runs (the empty workspace's question, a
    /// folder's "agent here"): the last one started where `key` can start it, else the first it
    /// offers.
    pub(super) fn agent_for(&self, key: WorkerKey) -> Option<AgentId> {
        let offered = self.startable_on(key);
        let last = self.last_start.as_ref().map(|l| &l.agent).filter(|a| offered.contains(a));
        last.cloned().or_else(|| offered.into_iter().next())
    }

    /// The palette's "New `agent` agent" lines: each starts at the machine step.
    pub(super) fn agent_lines(&self) -> Vec<PaletteItem> {
        let mut agents = self.startable_agents();
        agents.sort_by_key(|a| agent_label(a).to_lowercase());
        agents
            .into_iter()
            .map(|agent| {
                let label = format!("New {} agent", agent_label(&agent));
                let glyph = Glyph::agent(&agent.0);
                PaletteItem::new(&label, IconName::Sparkles, Box::new(NewAgentOf { agent }), &[])
                    .with_icon(glyph)
            })
            .collect()
    }

    /// ⌘⇧T: which agent, the last one first; with one, straight to the machine. A machine the
    /// "+" menu chose first is not asked again: its agents, then its folders.
    pub fn new_agent(&mut self, _: &NewAgent, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.new_on.take().filter(|k| self.workers.contains_key(k));
        let mut agents = match chosen {
            Some(worker) => self.startable_on(worker),
            None => self.startable_agents(),
        };
        if let Some(at) =
            self.last_start.as_ref().and_then(|last| agents.iter().position(|a| *a == last.agent))
        {
            let agent = agents.remove(at);
            agents.insert(0, agent);
        }
        match (agents.as_slice(), chosen) {
            ([], _) => self.show_notice(NO_AGENT.to_owned(), cx),
            ([agent], Some(worker)) => self.pick_folder(agent, worker, window, cx),
            ([agent], None) => self.pick_machine(agent, window, cx),
            _ => {
                let lines = agents
                    .into_iter()
                    .map(|agent| {
                        let label = agent_label(&agent);
                        let glyph = Glyph::agent(&agent.0);
                        let action: Box<dyn gpui::Action> = match chosen {
                            Some(worker) => Box::new(NewAgentOn { agent, worker }),
                            None => Box::new(NewAgentOf { agent }),
                        };
                        PaletteItem::new(&label, IconName::Sparkles, action, &[]).with_icon(glyph)
                    })
                    .collect();
                self.open_step(lines, PICK_AGENT, window, cx);
            }
        }
    }

    /// An agent picked: on which machine.
    pub(super) fn new_agent_of(
        &mut self,
        of: &NewAgentOf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_machine(&of.agent, window, cx);
    }

    /// A machine picked: in which folder.
    pub(super) fn new_agent_on(
        &mut self,
        on: &NewAgentOn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_folder(&on.agent, on.worker, window, cx);
    }

    /// The machines that can start `agent`: the last start's first, then the one the focus is
    /// on, then the rest by name; with one, straight to the folder.
    fn pick_machine(&mut self, agent: &AgentId, window: &mut Window, cx: &mut Context<Self>) {
        let mut machines: Vec<WorkerKey> = self
            .workers
            .keys()
            .copied()
            .filter(|k| self.startable_on(*k).contains(agent))
            .collect();
        machines.sort_by_key(|k| self.worker_name(*k).to_lowercase());
        let last = self.last_start.as_ref().map(|l| l.worker);
        for first in [self.context_worker(), last] {
            if let Some(at) = first.and_then(|k| machines.iter().position(|m| *m == k)) {
                let key = machines.remove(at);
                machines.insert(0, key);
            }
        }
        match machines.as_slice() {
            [] => {
                let text = format!("No machine can start {} now", agent_label(agent));
                self.show_notice(text, cx);
            }
            [worker] => self.pick_folder(agent, *worker, window, cx),
            _ => {
                let lines = machines
                    .into_iter()
                    .map(|worker| {
                        let name = self.worker_name(worker);
                        let action = Box::new(NewAgentOn { agent: agent.clone(), worker });
                        PaletteItem::new(&name, IconName::Server, action, &[])
                    })
                    .collect();
                self.open_step(lines, PICK_MACHINE, window, cx);
            }
        }
    }

    /// The folders `agent` may start in on `worker`, each once: the focused shell's there, the
    /// last start's there, where its shells stand (the most recent first), then its home.
    fn pick_folder(
        &mut self,
        agent: &AgentId,
        worker: WorkerKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.focused().filter(|t| t.worker == worker).and_then(|_| self.active_cwd());
        let last = self.last_start.as_ref().filter(|l| l.worker == worker).map(|l| l.cwd.clone());
        let recent = self.recent_places().into_iter().filter(|p| p.worker == worker);
        let mut folders: Vec<String> = Vec::new();
        for cwd in here.into_iter().chain(last).chain(recent.map(|p| p.cwd)) {
            if !folders.contains(&cwd) {
                folders.push(cwd);
            }
        }
        let home = "~".to_owned();
        if !folders.iter().any(|f| *f == home || Some(f.as_str()) == self.home_of(worker)) {
            folders.push(home);
        }
        let lines = folders
            .into_iter()
            .map(|cwd| {
                let shown = super::tile::cwd_tail(&cwd, self.home_of(worker));
                let action = Box::new(StartThread { worker, agent: agent.clone(), cwd });
                PaletteItem::new(&shown, IconName::Folder, action, &[])
            })
            .collect();
        self.open_step(lines, PICK_FOLDER, window, cx);
    }

    /// One step of the choice, as the palette.
    fn open_step(
        &mut self,
        lines: Vec<PaletteItem>,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let palette = cx.new(|cx| CommandPalette::pick_step(lines, placeholder, theme, window, cx));
        self.show_palette(palette, window, cx);
    }
}
