//! "New agent…", the start by steps (⌘T starts at once, [`super::tabs`]): the palette asks which
//! agent, then on which machine, then in which folder, and the agent's thread opens in a tile of
//! its own. Each step lists the last choice first, so ↩ ↩ ↩ starts the last combination again, and
//! a step with one choice is passed over. The palette's "New `agent` agent" lines start at the
//! machine.
//!
//! What a machine can start comes from its own link: the agents its capabilities found
//! installed, Claude Code, Codex, pi and every ACP agent, so a machine reached with no server
//! offers all it has. The folders are the focused tile's on that machine, then every place work
//! stands or stood there, newest first ([`WorkspaceView::recent_places`]): where its shells
//! stand, where the agent's threads work, the last start's folder, and where the agent's past
//! sessions ran, as the machine lists them when its link comes up and again as the step opens.
//! Its home ends them; after them comes a new worktree of each repository they are in, so
//! agents can work one repository side by side, then a clone of each repository another machine
//! has and this one does not ([`super::clone_here`]).
//!
//! The folder step ends with "Resume a past session…": every machine up that has the agent lists
//! its sessions from the agent's own record, the last prompted first (`ThreadRequest::Sessions`),
//! in a step that opens at once saying it reads them. The step's own machine leads, and a
//! session on another says which machine it is on. What the person types there finds the listed
//! ones at once, and once the field rests the machines are asked too: each searches every prompt
//! the agent recorded there, so a session older than the list is found by what was asked in it,
//! wherever it ran. A session picked opens the thread kept of it, its agent taken up again if it
//! exited, or starts the agent on it, on its machine, in its own words.
//!
//! "New project…" runs the same steps for the agent that will orchestrate the project. It lists
//! only the agents that run in a terminal, since a project's orchestrator is one, and offers no
//! past sessions. Its last step starts the agent at once ([`StartOrchestrator`]), and the
//! "New project" sheet opens over its terminal's tile.

use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Task, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::WallMs;
use slopty_proto::ClientMsg;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::{PastSession, PastSessions, ThreadRequest};

use super::WorkspaceView;
use super::actions::{
    NewAgent, NewAgentOf, NewAgentOn, NewProject, NewProjectOf, NewProjectOn, ResumePastSession,
    ResumeSession, StartOrchestrator, StartThread,
};
use super::projects::agent_label;
use crate::conversation::thread::find;
use crate::icons::{Status, Symbol};
use crate::palette::{CommandPalette, PaletteItem};

/// What is said when no machine can start an agent.
pub(super) const NO_AGENT: &str = "No machine has an agent to start";

/// What the agent step's field says.
const PICK_AGENT: &str = "Which agent";

/// The palette's line that starts a project with a new agent as its orchestrator.
pub(super) const NEW_PROJECT: &str = "New project\u{2026}";

/// What "New project…"'s agent step says: only an agent in a terminal can orchestrate.
pub(super) const PICK_ORCHESTRATOR: &str = "Which agent runs the project, in a terminal";

/// What "New project…" says when no machine has an agent that runs in a terminal.
pub(super) const NO_ORCHESTRATOR: &str =
    "No machine has an agent that runs in a terminal, and only one can orchestrate a project";

/// What the steps start: an agent of its own, or one to orchestrate a new project.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum For {
    /// "New agent…".
    Agent,
    /// "New project…".
    Project,
}

/// Whether a start of `agent` opens a terminal, so it can orchestrate a project: Claude Code,
/// whose TUI Slopty observes, and Codex, beside whose TUI Slopty is a second client. pi and
/// ACP agents are driven over their protocols and have no terminal until a handoff.
pub(super) fn runs_in_terminal(agent: &AgentId) -> bool {
    agent.is(AgentId::CLAUDE_CODE) || agent.is(AgentId::CODEX)
}

/// What the machine step's field says.
const PICK_MACHINE: &str = "On which machine";

/// What the folder step's field says.
const PICK_FOLDER: &str = "In which folder";

/// The folder step's line for a new worktree of a repository there, its name after it.
pub(super) const NEW_WORKTREE: &str = "New worktree of";

/// The folder step's line for a folder typed from its root: "Start in ~/w/app".
pub(super) const TYPED_FOLDER: &str = "Start in";

/// The folder step's last line.
pub(super) const RESUME_PAST: &str = "Resume a past session\u{2026}";

/// What the session step's field says.
const PICK_SESSION: &str = "Which session";

/// What the session step says until the machine has listed them.
pub(super) const READING_SESSIONS: &str = "Reading past sessions\u{2026}";

/// The most past sessions the step lists.
const SESSIONS_LISTED: u32 = 50;

/// The session step that is up, and what each machine said for it. Answers come in any order,
/// so the one for the field's words is kept only while they are still the field's; the list
/// with no words is kept for as long as the step is up.
pub(super) struct SessionsAsked {
    /// The machines asked, the step's own first, and what each said.
    machines: Vec<MachineSessions>,
    /// Whose sessions.
    agent: AgentId,
    /// The step's palette.
    step: gpui::EntityId,
    /// The field's words last asked for, trimmed; empty while they are too short to ask.
    words: String,
    /// The wait before `words` are asked for: dropped, so never sent, when they change first.
    _asking: Option<Task<()>>,
}

/// What one machine of the session step said.
struct MachineSessions {
    /// The machine.
    worker: WorkerKey,
    /// The sessions it listed with no words; `None` while it reads them.
    listed: Option<Vec<PastSession>>,
    /// Why it could list none, when it said.
    absent: Option<String>,
    /// The sessions whose prompts hold the step's words, once it answered for them.
    found: Option<Vec<PastSession>>,
}

impl MachineSessions {
    const fn new(worker: WorkerKey) -> Self {
        Self { worker, listed: None, absent: None, found: None }
    }
}

/// A folder one of an agent's past sessions ran in on a machine, as the machine listed them
/// with no words: one of the places a start offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct PastPlace {
    pub agent: AgentId,
    pub cwd: String,
    /// When the session there last changed, when the agent said.
    pub at: Option<WallMs>,
}

/// The folder step that is up: what it starts, where, and its palette, so the folders the
/// machine lists after it opened join it.
#[derive(Clone, Debug)]
pub(super) struct FolderStep {
    worker: WorkerKey,
    agent: AgentId,
    purpose: For,
    step: gpui::EntityId,
    /// The folders the machine listed for the paths typed, to complete them
    /// ([`super::folder_typing`]).
    typed: super::folder_typing::Listed,
}

impl FolderStep {
    /// The machine it starts on.
    pub(super) const fn worker(&self) -> WorkerKey {
        self.worker
    }

    /// Its palette.
    pub(super) const fn step(&self) -> gpui::EntityId {
        self.step
    }

    /// The folders listed for the paths typed in it.
    pub(super) const fn typed(&self) -> &super::folder_typing::Listed {
        &self.typed
    }
}

impl WorkspaceView {
    /// The agents `key` can start a thread of while its link is up: those its link says are
    /// installed there, Claude Code, Codex, pi and the ACP agents alike, each once.
    pub(super) fn startable_on(&self, key: WorkerKey) -> Vec<AgentId> {
        let Some(w) = self.workers.get(&key).filter(|w| w.link.is_some()) else {
            return Vec::new();
        };
        let mut out: Vec<AgentId> = Vec::new();
        for agent in w.caps.iter().flat_map(|c| &c.agents).map(|a| a.agent.clone()) {
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
        let last = self.starts.last().map(|l| &l.agent).filter(|a| offered.contains(a));
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
                PaletteItem::new(&label, Box::new(NewAgentOf { agent }), &[])
            })
            .collect()
    }

    /// "New agent…": which agent, the last one first; with one, straight to the machine. A machine
    /// the "+" menu chose first is not asked again: its agents, then its folders.
    pub fn new_agent(&mut self, _: &NewAgent, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.new_on.take().filter(|k| self.workers.contains_key(k));
        let mut agents = match chosen {
            Some(worker) => self.startable_on(worker),
            None => self.startable_agents(),
        };
        if let Some(at) =
            self.starts.last().and_then(|last| agents.iter().position(|a| *a == last.agent))
        {
            let agent = agents.remove(at);
            agents.insert(0, agent);
        }
        match (agents.as_slice(), chosen) {
            ([], _) => self.show_notice(NO_AGENT.to_owned(), cx),
            ([agent], Some(worker)) => self.pick_folder(agent, worker, For::Agent, window, cx),
            ([agent], None) => self.pick_machine(agent, For::Agent, window, cx),
            _ => {
                let lines = agents
                    .into_iter()
                    .map(|agent| {
                        let label = agent_label(&agent);
                        let mark = crate::icons::Mark::agent(&agent.0);
                        let action: Box<dyn gpui::Action> = match chosen {
                            Some(worker) => Box::new(NewAgentOn { agent, worker }),
                            None => Box::new(NewAgentOf { agent }),
                        };
                        PaletteItem::new(&label, action, &[]).with_icon(mark)
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
        self.pick_machine(&of.agent, For::Agent, window, cx);
    }

    /// A machine picked: in which folder.
    pub(super) fn new_agent_on(
        &mut self,
        on: &NewAgentOn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_folder(&on.agent, on.worker, For::Agent, window, cx);
    }

    /// "New project…": which agent will orchestrate it, of those that run in a terminal, the
    /// last one started first; with one, straight to the machine.
    pub(super) fn new_project(
        &mut self,
        _: &NewProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut agents: Vec<AgentId> =
            self.startable_agents().into_iter().filter(runs_in_terminal).collect();
        if let Some(at) =
            self.starts.last().and_then(|last| agents.iter().position(|a| *a == last.agent))
        {
            let agent = agents.remove(at);
            agents.insert(0, agent);
        }
        match agents.as_slice() {
            [] => self.show_notice(NO_ORCHESTRATOR.to_owned(), cx),
            [agent] => self.pick_machine(agent, For::Project, window, cx),
            _ => {
                let lines = agents
                    .into_iter()
                    .map(|agent| {
                        let label = agent_label(&agent);
                        let mark = crate::icons::Mark::agent(&agent.0);
                        PaletteItem::new(&label, Box::new(NewProjectOf { agent }), &[])
                            .with_icon(mark)
                    })
                    .collect();
                self.open_step(lines, PICK_ORCHESTRATOR, window, cx);
            }
        }
    }

    /// An orchestrator's agent picked: on which machine.
    pub(super) fn new_project_of(
        &mut self,
        of: &NewProjectOf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_machine(&of.agent, For::Project, window, cx);
    }

    /// An orchestrator's machine picked: in which folder.
    pub(super) fn new_project_on(
        &mut self,
        on: &NewProjectOn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pick_folder(&on.agent, on.worker, For::Project, window, cx);
    }

    /// The machines that can start `agent`: the last start's first, then the one the focus is
    /// on, then the rest by name; with one, straight to the folder.
    fn pick_machine(
        &mut self,
        agent: &AgentId,
        purpose: For,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut machines: Vec<WorkerKey> = self
            .workers
            .keys()
            .copied()
            .filter(|k| self.startable_on(*k).contains(agent))
            .collect();
        machines.sort_by_key(|k| self.worker_name(*k).to_lowercase());
        let last = self.starts.last().map(|l| l.worker);
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
            [worker] => self.pick_folder(agent, *worker, purpose, window, cx),
            _ => {
                let lines = machines
                    .into_iter()
                    .map(|worker| {
                        let name = self.worker_name(worker);
                        let agent = agent.clone();
                        let action: Box<dyn gpui::Action> = match purpose {
                            For::Agent => Box::new(NewAgentOn { agent, worker }),
                            For::Project => Box::new(NewProjectOn { agent, worker }),
                        };
                        PaletteItem::new(&name, action, &[]).with_icon(self.machine_glyph(worker))
                    })
                    .collect();
                self.open_step(lines, PICK_MACHINE, window, cx);
            }
        }
    }

    /// The repository `cwd` is in on `worker`: as a shell or a thread standing there reported
    /// it; else the one of those `cwd` is inside; else `cwd` itself when a folder tile there
    /// lists a `.git`.
    fn repo_at(&self, worker: WorkerKey, cwd: &str, cx: &gpui::App) -> Option<String> {
        let w = self.workers.get(&worker)?;
        let shells = w.sessions.values().map(|s| (s.cwd.clone(), s.repo.clone()));
        let threads = self.places_on(worker, cx).into_iter().map(|p| (p.cwd, p.repo));
        let known: Vec<(Option<String>, String)> =
            shells.chain(threads).filter_map(|(at, repo)| Some((at, repo?))).collect();
        let standing = known.iter().find(|(at, _)| at.as_deref() == Some(cwd));
        let inside = || {
            known.iter().find(|(_, repo)| {
                cwd == repo || cwd.strip_prefix(repo.as_str()).is_some_and(|r| r.starts_with('/'))
            })
        };
        if let Some((_, repo)) = standing.or_else(inside) {
            return Some(repo.clone());
        }
        let cloned = self
            .folders
            .values()
            .map(|f| f.read(cx))
            .any(|f| f.path() == cwd && f.entries().iter().any(|e| e.name == ".git"));
        cloned.then(|| cwd.to_owned())
    }

    /// The folder step for `agent` on `worker`; the machine is asked again where the agent's
    /// past sessions ran, and the folders it lists join the step that is still up.
    fn pick_folder(
        &mut self,
        agent: &AgentId,
        worker: WorkerKey,
        purpose: For,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = self.folder_lines(agent, worker, purpose, cx);
        self.open_step(lines, PICK_FOLDER, window, cx);
        if let Some(palette) = self.palette.clone() {
            let listed = super::folder_typing::Listed::default();
            let (typed, known) = (agent.clone(), listed.clone());
            palette.update(cx, |p, cx| {
                p.set_typed(move |text| typed_folder(text, worker, &typed, purpose, &known), cx);
            });
            let step = palette.entity_id();
            let agent = agent.clone();
            self.folder_step = Some(FolderStep { worker, agent, purpose, step, typed: listed });
        }
        self.ask_past_places(worker, Some(agent));
    }

    /// The folders `agent` may start in on `worker`, each once: the focused tile's there, then
    /// every place work stands or stood there, newest first ([`Self::recent_places`]), then its
    /// home. A new worktree of each repository among them follows, then, for an agent of its
    /// own, "Resume a past session…".
    fn folder_lines(
        &self,
        agent: &AgentId,
        worker: WorkerKey,
        purpose: For,
        cx: &gpui::App,
    ) -> Vec<PaletteItem> {
        let here = self.focused().filter(|t| t.worker == worker).and_then(|_| self.active_cwd());
        let recent = self.recent_places(Some(agent), cx).into_iter().filter(|p| p.worker == worker);
        let mut folders: Vec<String> = Vec::new();
        for cwd in here.into_iter().chain(recent.map(|p| p.cwd)) {
            if !folders.contains(&cwd) {
                folders.push(cwd);
            }
        }
        let home = "~".to_owned();
        if !folders.iter().any(|f| *f == home || Some(f.as_str()) == self.home_of(worker)) {
            folders.push(home);
        }
        let home = self.home_of(worker);
        // One new worktree for each repository the folders are in, from the first folder in
        // it: the worker makes it from that repository's clone, and the agent stands there.
        let mut repos: Vec<String> = Vec::new();
        let mut worktrees: Vec<PaletteItem> = Vec::new();
        // The last start's new worktree there, when it made one: first, so ↩ makes another.
        let last =
            self.starts.last().filter(|l| l.worktree && l.worker == worker && l.agent == *agent);
        let last_repo = last.and_then(|l| self.repo_at(worker, &l.cwd, cx));
        let mut first: Option<PaletteItem> = None;
        for cwd in &folders {
            let Some(repo) = self.repo_at(worker, cwd, cx).filter(|r| !repos.contains(r)) else {
                continue;
            };
            let name = super::tile::place_name(&repo, Some(&repo), home).unwrap_or_default();
            let action = start(purpose, worker, agent, cwd.clone(), true);
            let shown = format!("{NEW_WORKTREE} {name}");
            let line =
                PaletteItem::new(&shown, action, &[]).with_icon(crate::icons::GitGlyph::Branch);
            if first.is_none() && last_repo.as_ref() == Some(&repo) {
                first = Some(line);
            } else {
                worktrees.push(line);
            }
            repos.push(repo);
        }
        let mut lines: Vec<PaletteItem> = folders
            .into_iter()
            .map(|cwd| {
                let shown = super::tile::cwd_tail(&cwd, home);
                let action = start(purpose, worker, agent, cwd, false);
                PaletteItem::new(&shown, action, &[]).with_icon(Symbol::Folder)
            })
            .collect();
        lines.extend(worktrees);
        if let Some(first) = first {
            lines.insert(0, first);
        }
        lines.extend(self.clone_lines(agent, worker, purpose));
        if purpose == For::Agent {
            let past = Box::new(ResumePastSession { worker, agent: agent.clone() });
            lines.push(PaletteItem::new(RESUME_PAST, past, &[]));
        }
        lines
    }

    /// Keep the starts in `path`, and begin from those a previous run kept there. Set before
    /// the workers are added.
    pub fn set_starts_file(&mut self, path: std::path::PathBuf) {
        self.starts = slopty_client::starts::Starts::read(&path);
        self.starts_file = Some(path);
    }

    /// `start` went, with its draft's `chips` when it had one: it is the last start, and the
    /// chips its agent's next draft begins on. They are written off the UI thread, after the
    /// write under way.
    pub(super) fn start_went(
        &mut self,
        start: slopty_client::starts::LastStart,
        chips: Option<slopty_client::starts::Chips>,
        cx: &Context<Self>,
    ) {
        self.starts.went(start, chips);
        let Some(path) = self.starts_file.clone() else { return };
        let (starts, before) = (self.starts.clone(), self.starts_writing.take());
        self.starts_writing = Some(cx.background_spawn(async move {
            if let Some(before) = before {
                before.await;
            }
            if let Err(e) = starts.write(&path) {
                tracing::warn!(path = %path.display(), error = %e, "starts save");
            }
        }));
    }

    /// Ask `key` for its agents' past sessions with no words, `agent`'s alone when given: the
    /// folders they ran in are places a start offers, and the session step lists them. One
    /// such ask is out per machine and agent at a time: while one is, its answer feeds every
    /// step waiting, and nothing more is sent. A machine out of reach is not asked.
    pub(super) fn ask_past_places(&mut self, key: WorkerKey, agent: Option<&AgentId>) {
        if !self.workers.get(&key).is_some_and(super::Worker::is_linked) {
            return;
        }
        if !self.listing.insert((key, agent.cloned())) {
            return;
        }
        self.send(
            key,
            ClientMsg::Thread(ThreadRequest::Sessions {
                agent: agent.cloned(),
                cwd: None,
                query: String::new(),
                limit: SESSIONS_LISTED,
            }),
        );
    }

    /// `key` listed past sessions with no words, `agent`'s alone or every agent's: the folders
    /// they ran in replace those kept of them, and join the folder step still up there.
    fn keep_past_places(
        &mut self,
        key: WorkerKey,
        agent: Option<&AgentId>,
        sessions: &[PastSession],
        cx: &mut Context<Self>,
    ) {
        let kept = self.past_places.entry(key).or_default();
        kept.retain(|p| agent.is_some_and(|a| *a != p.agent));
        for session in sessions {
            let Some(cwd) = session.cwd.clone() else { continue };
            let at = session.updated_ms;
            match kept.iter_mut().find(|p| p.agent == session.agent && p.cwd == cwd) {
                Some(place) => place.at = place.at.max(at),
                None => kept.push(PastPlace { agent: session.agent.clone(), cwd, at }),
            }
        }
        let Some(step) = self.folder_step.clone() else { return };
        if step.worker != key || agent.is_some_and(|a| *a != step.agent) {
            return;
        }
        let Some(palette) = self.palette.clone().filter(|p| p.entity_id() == step.step) else {
            self.folder_step = None;
            return;
        };
        let lines = self.folder_lines(&step.agent, step.worker, step.purpose, cx);
        palette.update(cx, |p, cx| p.set_items(lines, cx));
    }

    /// "Resume a past session…": the machine, and every other one up that has the agent, is
    /// asked for the agent's sessions, and the step that lists them opens at once, saying it
    /// reads them.
    pub(super) fn resume_past_session(
        &mut self,
        ask: &ResumePastSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ResumePastSession { worker, agent } = ask.clone();
        if !self.workers.get(&worker).is_some_and(super::Worker::is_linked) {
            let text = format!("{} is out of reach", self.worker_name(worker));
            self.show_notice(text, cx);
            return;
        }
        let others = self
            .workers
            .keys()
            .copied()
            .filter(|key| *key != worker && self.startable_on(*key).contains(&agent));
        let machines: Vec<WorkerKey> = std::iter::once(worker).chain(others).collect();
        for key in &machines {
            self.ask_past_places(*key, Some(&agent));
        }
        self.open_step(Vec::new(), PICK_SESSION, window, cx);
        if let Some(palette) = self.palette.clone() {
            palette.update(cx, |p, cx| p.set_empty(READING_SESSIONS, cx));
            self.sessions_asked = Some(SessionsAsked {
                machines: machines.into_iter().map(MachineSessions::new).collect(),
                agent,
                step: palette.entity_id(),
                words: String::new(),
                _asking: None,
            });
        }
    }

    /// The session step's field says `text`. The listed sessions it finds show at once; once
    /// the field rests [`find::ASK_AFTER`], words of [`find::ASK_FROM`] characters or more are
    /// asked of each machine, which searches every prompt the agent recorded there. New words
    /// drop the last answers and the ask still waiting. A machine out of reach by then is not
    /// asked.
    pub(super) fn ask_sessions(
        &mut self,
        palette: &Entity<CommandPalette>,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(asked) = self.sessions_asked.as_mut().filter(|a| a.step == palette.entity_id())
        else {
            return;
        };
        let typed = text.trim();
        let words = if typed.chars().count() >= find::ASK_FROM { typed } else { "" };
        if words == asked.words {
            return;
        }
        let (agent, step) = (asked.agent.clone(), asked.step);
        let machines: Vec<WorkerKey> = asked.machines.iter().map(|m| m.worker).collect();
        let query = words.to_owned();
        let asking = (!words.is_empty()).then(|| {
            let (agent, query) = (agent.clone(), query.clone());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(find::ASK_AFTER).await;
                let _gone = this.update(cx, |this, _cx| {
                    let up = this.palette.as_ref().is_some_and(|p| p.entity_id() == step);
                    for worker in machines
                        .into_iter()
                        .filter(|w| up && this.workers.get(w).is_some_and(super::Worker::is_linked))
                    {
                        let ask = ThreadRequest::Sessions {
                            agent: Some(agent.clone()),
                            cwd: None,
                            query: query.clone(),
                            limit: SESSIONS_LISTED,
                        };
                        this.send(worker, ClientMsg::Thread(ask));
                    }
                });
            })
        });
        let mut machines = std::mem::take(&mut asked.machines);
        for machine in &mut machines {
            machine.found = None;
        }
        *asked = SessionsAsked { machines, agent, step, words: query, _asking: asking };
        self.show_sessions(cx);
    }

    /// `key` listed an agent's past sessions, with no words or for the field's: the step
    /// waiting on them lists them, or says why there are none; a list with no words anywhere
    /// is also where the starts' places learn the folders they ran in. An answer nothing waits on,
    /// for another step, or for words the field no longer says, is dropped.
    pub fn past_sessions(&mut self, key: WorkerKey, past: PastSessions, cx: &mut Context<Self>) {
        if past.cwd.is_none() && past.query.is_empty() {
            self.listing.remove(&(key, past.agent.clone()));
            if past.absent.is_none() {
                self.keep_past_places(key, past.agent.as_ref(), &past.sessions, cx);
            }
        }
        let Some(asked) = self.sessions_asked.as_mut() else {
            return;
        };
        if past.agent.as_ref() != Some(&asked.agent) || past.cwd.is_some() {
            return;
        }
        let words = asked.words.clone();
        let Some(machine) = asked.machines.iter_mut().find(|m| m.worker == key) else {
            return;
        };
        if past.query.is_empty() {
            machine.listed = Some(past.sessions);
            machine.absent = past.absent;
        } else if past.query == words {
            machine.found = Some(past.sessions);
        } else {
            return;
        }
        if let Some(cut) = past.cut {
            tracing::info!(%cut, "past sessions listed in part");
        }
        self.show_sessions(cx);
    }

    /// The session step's lines: the sessions found for the field's words, best first and the
    /// step's own machine's first, then the listed ones they leave out, for the field to find
    /// among. A session on another machine than the step's says which.
    fn show_sessions(&self, cx: &mut Context<Self>) {
        let Some(asked) = &self.sessions_asked else {
            return;
        };
        let Some(palette) = self.palette.clone().filter(|p| p.entity_id() == asked.step) else {
            return;
        };
        let Some(own) = asked.machines.first().map(|m| m.worker) else {
            return;
        };
        let now = crate::clock::now(cx).as_millis();
        let same = |a: &PastSession, b: &PastSession| a.agent == b.agent && a.native == b.native;
        let line = |worker: WorkerKey, session: &PastSession| {
            let machine = (worker != own).then(|| self.worker_name(worker));
            session_line(worker, session.clone(), self.home_of(worker), machine, now)
        };
        let found = asked
            .machines
            .iter()
            .flat_map(|m| m.found.iter().flatten().map(move |session| line(m.worker, session)));
        let left = asked.machines.iter().flat_map(|m| {
            let found = m.found.as_deref().unwrap_or_default();
            m.listed
                .iter()
                .flatten()
                .filter(move |l| !found.iter().any(|f| same(f, l)))
                .map(move |session| line(m.worker, session))
        });
        let lines: Vec<PaletteItem> = found.chain(left).collect();
        let reading = asked.machines.iter().any(|m| {
            m.absent.is_none()
                && (m.listed.is_none() || (!asked.words.is_empty() && m.found.is_none()))
        });
        let agent = agent_label(&asked.agent);
        let alone = asked.machines.len() == 1;
        let absent = asked.machines.iter().map(|m| m.absent.as_ref());
        let empty = match absent.collect::<Option<Vec<&String>>>() {
            Some(said) if alone => said.first().map_or_else(String::new, |s| (*s).clone()),
            Some(_) => format!("No machine has past {agent} sessions"),
            None if reading => READING_SESSIONS.to_owned(),
            None if asked.words.is_empty() && alone => {
                format!("{} has no past {agent} sessions", self.worker_name(own))
            }
            None if asked.words.is_empty() => format!("No machine has past {agent} sessions"),
            None if alone => {
                format!("No past {agent} prompt on {} says that", self.worker_name(own))
            }
            None => format!("No past {agent} prompt on any machine says that"),
        };
        palette.update(cx, |p, cx| {
            p.set_items(lines, cx);
            p.set_empty(empty, cx);
        });
    }

    /// A past session picked. The thread kept of it opens where it runs; one whose agent
    /// exited opens and is taken up again through its agent's door; a session with no thread
    /// here starts its agent on it, in the agent's own words.
    pub(super) fn resume_session(
        &mut self,
        pick: &ResumeSession,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let worker = pick.worker;
        let session = &pick.session;
        let kept = session.thread.and_then(|t| Some((t, self.thread_stand(t)?.exited)));
        match kept {
            Some((thread, false)) => self.open_thread(worker, thread, cx),
            Some((thread, true)) => self.reopen_thread(worker, thread, None, cx),
            None => {
                let cwd = session.cwd.clone().unwrap_or_else(|| "~".to_owned());
                let (agent, args) = (session.agent.clone(), session.resume.clone());
                self.start_resumed(worker, agent, cwd, args, cx);
            }
        }
    }

    /// One step of the choice, as the palette.
    pub(super) fn open_step(
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

/// A folder typed from its root in the folder step (`/…`, `~/…`, `~`): the line that starts
/// `agent` there on `worker`, then the folders the machine listed that complete it
/// ([`super::folder_typing`]). Anything else adds none; the step's own lines are found by it.
fn typed_folder(
    text: &str,
    worker: WorkerKey,
    agent: &AgentId,
    purpose: For,
    listed: &super::folder_typing::Listed,
) -> Vec<PaletteItem> {
    let typed = text.trim();
    if super::folder_typing::split(typed).is_none() {
        return Vec::new();
    }
    let cwd = if typed == "/" { typed } else { typed.trim_end_matches('/') }.to_owned();
    // As typed: it is the person's own spelling of where.
    let shown = format!("{TYPED_FOLDER} {cwd}");
    let action = start(purpose, worker, agent, cwd, false);
    let mut lines = vec![PaletteItem::new(&shown, action, &[]).with_icon(Symbol::Folder)];
    for path in listed.completing(typed) {
        let action = start(purpose, worker, agent, path.clone(), false);
        lines.push(PaletteItem::new(&path, action, &[]).with_icon(Symbol::Folder));
    }
    lines
}

/// The folder step's action for a line: start `agent` on `worker` in `cwd`, as an agent of its
/// own or as a new project's orchestrator.
pub(super) fn start(
    purpose: For,
    worker: WorkerKey,
    agent: &AgentId,
    cwd: String,
    worktree: bool,
) -> Box<dyn gpui::Action> {
    let agent = agent.clone();
    match purpose {
        For::Agent => Box::new(StartThread { worker, agent, cwd, worktree }),
        For::Project => Box::new(StartOrchestrator { worker, agent, cwd, worktree }),
    }
}

/// The session step's line for `session` on `worker`: what it is about (its title, else the
/// last prompt that matched, else its id), where it ran (on `machine`, named when it is not the
/// step's) and how long ago, found as well by its prompts; marked running while a live agent
/// holds it.
fn session_line(
    worker: WorkerKey,
    session: PastSession,
    home: Option<&str>,
    machine: Option<String>,
    now: u64,
) -> PaletteItem {
    let prompt = session.prompts.first().map(|p| crate::kit::first_line(&p.text).to_owned());
    let label = session
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| prompt.clone().filter(|p| !p.is_empty()))
        .unwrap_or_else(|| {
            format!("Session {}", session.native.chars().take(8).collect::<String>())
        });
    let cwd = session.cwd.as_deref().map(|cwd| super::tile::cwd_tail(cwd, home));
    let cwd = match (cwd, machine) {
        (Some(cwd), Some(machine)) => Some(format!("{cwd} on {machine}")),
        (None, Some(machine)) => Some(format!("on {machine}")),
        (cwd, None) => cwd,
    };
    let age =
        session.updated_ms.map(|at| Duration::from_millis(now.saturating_sub(at.as_millis())));
    let about = session.prompts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join("\n");
    // A session a live agent holds elsewhere is marked running: taking it up is refused.
    let running = session.facts.contains_key(slopty_proto::thread::wire::PAST_RUNNING);
    let mark = crate::icons::Mark::agent(&session.agent.0);
    let action = Box::new(ResumeSession { worker, session: Box::new(session) });
    PaletteItem::new(&label, action, &[])
        .with_icon(mark)
        .with_status(running.then_some(Status::Running))
        .in_dir(cwd)
        .aged(age)
        .about((!about.is_empty()).then_some(about))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use slopty_proto::thread::AgentId;
    use slopty_proto::thread::wire::{PAST_RUNNING, PastSession};

    use super::*;

    /// A session the worker says a live agent holds is marked running in the session step,
    /// before the person picks it; one nothing holds is not.
    #[test]
    fn a_session_a_live_agent_holds_is_marked_running() {
        let session = |facts: BTreeMap<String, String>| PastSession {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            native: "0b6f6c55-6a41-4f4e-9e0c-3a8f3a1f2b7d".to_owned(),
            cwd: Some("/src/app".to_owned()),
            title: Some("Fix the parser".to_owned()),
            updated_ms: None,
            thread: None,
            resume: Vec::new(),
            facts,
            prompts: Vec::new(),
        };
        let worker = WorkerKey::new(1);
        let held = [(PAST_RUNNING.to_owned(), "interactive".to_owned())].into();
        assert_eq!(
            session_line(worker, session(held), None, None, 0).status,
            Some(Status::Running)
        );
        assert_eq!(session_line(worker, session(BTreeMap::new()), None, None, 0).status, None);
    }
}
