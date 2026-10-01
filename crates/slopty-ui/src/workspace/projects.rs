//! Projects in the workspace: the server's projects mirrored, and each board shown in its
//! orchestrator's tile.
//!
//! The app hands over the server's snapshot and its changes ([`WorkspaceView::projects_part`],
//! [`WorkspaceView::project_update`]). An orchestrator's tile turns between its TUI and its
//! board (⇧⌘J, the header's button, the palette's line for the project); a board's rows open
//! the tiles of the agents they name. Everything a board draws is handed to it in the frame
//! after it changed, compared first, so a board is drawn again only when what it shows moved.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentStatus;
use slopty_proto::items::ItemKind;
use slopty_proto::project::{ProjectId, ProjectUpdate, ProjectsPart};

use super::WorkspaceView;
use super::actions::ToggleProjectBoard;
use super::agents::agent_ask_line;
use crate::icons::Status;
use crate::project::model::{Board, Lane, Projects};
use crate::project::{AgentSeen, Node, ProjectEvent, ProjectView, Seen};

/// What the palette and the header call turning a tile to its board.
pub(crate) const SHOW_BOARD: &str = "Show project board";
/// What a terminal says when asked for a board it has none of.
pub(crate) const NO_PROJECT: &str = "No project runs in this terminal";

/// What the workspace keeps about projects.
#[derive(Default)]
pub(super) struct ProjectsState {
    /// The server's projects, as last heard.
    pub mirror: Projects,
    /// Each project's board, made the first time it shows and kept while the project lives.
    pub views: HashMap<ProjectId, Entity<ProjectView>>,
    /// What each board asks of the workspace.
    pub subscriptions: HashMap<ProjectId, gpui::Subscription>,
    /// The orchestrators whose tiles show the board rather than the terminal.
    pub shown: HashSet<SessionId>,
    /// Boards that take the keyboard on the next frame.
    pub focus: HashSet<ProjectId>,
    /// Something a board shows changed since the boards were last handed what they show.
    pub dirty: bool,
}

/// `id` as the workspace keys workers: the one the app gives the server's worker ids.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

impl WorkspaceView {
    /// One part of the server's snapshot of its projects: the first part replaces them all.
    pub fn projects_part(&mut self, part: ProjectsPart, cx: &mut Context<Self>) {
        let _touched = self.projects.mirror.apply_part(part);
        self.projects_moved(cx);
    }

    /// A project changed, as the server's event `seq` says; a change the snapshot holds is
    /// dropped.
    pub fn project_update(&mut self, seq: u64, update: ProjectUpdate, cx: &mut Context<Self>) {
        if self.projects.mirror.apply_update(seq, update).is_some() {
            self.projects_moved(cx);
        }
    }

    /// The server was let go: its projects go with it, and every board turns back to its
    /// terminal.
    pub fn forget_projects(&mut self, cx: &mut Context<Self>) {
        if self.projects.mirror.is_empty() && self.projects.shown.is_empty() {
            return;
        }
        self.projects.mirror.clear();
        self.projects_moved(cx);
    }

    /// The projects as this client mirrors them.
    #[must_use]
    pub const fn projects(&self) -> &Projects {
        &self.projects.mirror
    }

    /// Whether `session`'s tile shows its project's board.
    #[must_use]
    pub fn board_shown(&self, session: SessionId) -> bool {
        self.projects.shown.contains(&session)
            && self.projects.mirror.of_orchestrator(session).is_some()
    }

    /// The board of the project `session` orchestrates, once made.
    #[must_use]
    pub fn board_view(&self, session: SessionId) -> Option<&Entity<ProjectView>> {
        let board = self.projects.mirror.of_orchestrator(session)?;
        self.projects.views.get(&board.project.id)
    }

    /// Turn `session`'s tile to its project's board, or back to its terminal.
    pub fn show_board(&mut self, session: SessionId, board: bool, cx: &mut Context<Self>) {
        let Some(project) =
            self.projects.mirror.of_orchestrator(session).map(|b| b.project.id.clone())
        else {
            return;
        };
        if board {
            self.projects.shown.insert(session);
            self.projects.focus.insert(project);
        } else {
            self.projects.shown.remove(&session);
            self.pending_focus = Some(session);
        }
        self.projects.dirty = true;
        self.changed(cx);
        cx.notify();
    }

    /// ⇧⌘J: the focused orchestrator between its terminal and its board. From an agent working
    /// on a task, its project's board, in its orchestrator's tile.
    pub(super) fn toggle_project_board(
        &mut self,
        _: &ToggleProjectBoard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else {
            self.show_notice(NO_PROJECT.to_owned(), cx);
            return;
        };
        if self.projects.mirror.of_orchestrator(session).is_some() {
            let shown = self.board_shown(session);
            self.show_board(session, !shown, cx);
            return;
        }
        match self.projects.mirror.of_agent(session).map(|(b, _)| b.project.id.clone()) {
            Some(project) => self.open_project(&project, cx),
            None => self.show_notice(NO_PROJECT.to_owned(), cx),
        }
    }

    /// Show `project`'s board in its orchestrator's tile, and go there.
    pub fn open_project(&mut self, project: &ProjectId, cx: &mut Context<Self>) {
        let Some(board) = self.projects.mirror.get(project) else { return };
        let title = board.project.title.clone();
        let Some(session) = board.project.orchestrator.map(|t| t.session) else {
            self.show_notice(format!("{title} has no orchestrator yet"), cx);
            return;
        };
        let Some(tile) = self.tile_of_session(session) else {
            self.show_notice(format!("{title}'s orchestrator has no tile here"), cx);
            return;
        };
        self.focus_tile(tile, cx);
        self.show_board(session, true, cx);
    }

    /// A board asked for a verifier's terminal: its tile, focused, or why there is none.
    fn open_output(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.tile_of_session(session).is_none() {
            self.show_notice("The verifier's terminal has closed".to_owned(), cx);
            return;
        }
        self.reveal_session(session, cx);
    }

    /// A board's row asked for its agent: its tile, focused with the keyboard in it. The
    /// orchestrator's own row turns its tile back to the terminal.
    fn open_node(&mut self, project: &ProjectId, node: Node, cx: &mut Context<Self>) {
        let Some(board) = self.projects.mirror.get(project) else { return };
        let who = node.map_or_else(|| "The orchestrator".to_owned(), |task| format!("#{task}"));
        let Some((_, session)) = board.terminal(node) else {
            let why = match node.and_then(|t| board.tasks.get(&t)) {
                Some(card) if card.assignment.is_some() => format!("{who}'s agent has ended"),
                _ => format!("{who} has no agent yet"),
            };
            self.show_notice(why, cx);
            return;
        };
        if node.is_none() && self.board_shown(session) {
            self.show_board(session, false, cx);
            return;
        }
        if self.tile_of_session(session).is_none() {
            let worker = board.worker(node).map(worker_key).and_then(|k| self.workers.get(&k));
            let why = match worker {
                Some(w) if w.link.is_none() => {
                    format!("{who} runs on {}, which is {}", w.name, w.status.text())
                }
                _ => format!("{who}'s agent has no tile here"),
            };
            self.show_notice(why, cx);
            return;
        }
        // A task agent that is itself an orchestrator's session opens on its terminal.
        self.projects.shown.remove(&session);
        self.reveal_session(session, cx);
    }

    /// The palette's line for each project: its title, how it stands, and ↩ for its board.
    pub(super) fn project_lines(&self) -> Vec<crate::palette::PaletteItem> {
        self.projects
            .mirror
            .boards()
            .map(|board| {
                let lanes = board.lanes();
                let status = lanes.first().map(|(lane, _)| lane_status(*lane));
                let (merged, total) = board.progress();
                let place = (total > 0).then(|| format!("{merged} of {total} merged"));
                crate::palette::PaletteItem::project(&board.project.title, board.project.id.clone())
                    .with_status(status)
                    .placed(place)
            })
            .collect()
    }

    /// Something a board shows changed: they are handed it in the next frame.
    fn projects_moved(&mut self, cx: &mut Context<Self>) {
        self.projects.dirty = true;
        self.changed(cx);
        cx.notify();
    }

    /// Bring the boards in step, once a frame after a change: make the board a tile shows,
    /// hand every board what it shows now, drop the boards of projects gone, and give the
    /// keyboard to a board just turned to.
    ///
    /// Returns whether it gave a board the keyboard.
    pub(super) fn sync_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !std::mem::take(&mut self.projects.dirty) {
            return false;
        }
        // A board goes with its project; while its project lives it keeps its lens and row.
        let live = &self.projects.mirror;
        self.projects.views.retain(|p, _| live.get(p).is_some());
        self.projects.subscriptions.retain(|p, _| live.get(p).is_some());
        if self.projects.shown.is_empty() {
            self.projects.focus.clear();
            return false;
        }
        let tiled: HashSet<SessionId> = self
            .layout
            .tiles()
            .filter_map(|t| match self.item(t)?.kind {
                ItemKind::Terminal { session } => Some(session),
                _ => None,
            })
            .collect();
        let mirror = &self.projects.mirror;
        self.projects.shown.retain(|s| tiled.contains(s) && mirror.of_orchestrator(*s).is_some());
        let wanted: Vec<ProjectId> = self
            .projects
            .shown
            .iter()
            .filter_map(|s| Some(mirror.of_orchestrator(*s)?.project.id.clone()))
            .collect();
        for project in &wanted {
            if !self.projects.views.contains_key(project) {
                self.make_board(project.clone(), cx);
            }
        }
        let names: BTreeMap<WorkerId, String> = self
            .projects
            .mirror
            .boards()
            .flat_map(|b| {
                let orchestrator = b.project.orchestrator.map(|t| t.worker);
                let tasks =
                    b.tasks.values().filter_map(|c| c.assignment.as_ref().map(|a| a.term.worker));
                orchestrator.into_iter().chain(tasks).collect::<Vec<_>>()
            })
            .filter_map(|id| Some((id, self.workers.get(&worker_key(id))?.name.clone())))
            .collect();
        let now = WallMs::now();
        // Only the boards on show: a hidden one is handed everything as it shows again.
        let views: Vec<(ProjectId, Entity<ProjectView>)> = wanted
            .iter()
            .filter_map(|p| Some((p.clone(), self.projects.views.get(p)?.clone())))
            .collect();
        for (project, view) in views {
            let board = self.projects.mirror.get(&project).cloned();
            let agents = board.as_ref().map(|b| self.board_agents(b)).unwrap_or_default();
            let seen = Seen { board, workers: names.clone(), agents, now };
            view.update(cx, |v, cx| v.set_seen(seen, cx));
        }
        let mut gave = false;
        for project in std::mem::take(&mut self.projects.focus) {
            if let Some(view) = self.projects.views.get(&project).cloned() {
                view.update(cx, |v, cx| v.focus(window, cx));
                gave = true;
            }
        }
        gave
    }

    fn make_board(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        let theme = self.theme.clone();
        let id = project.clone();
        let view = cx.new(|cx| ProjectView::new(id, theme, cx));
        let asked = project.clone();
        let subscription =
            cx.subscribe(&view, move |this, _view, event: &ProjectEvent, cx| match *event {
                ProjectEvent::Open(node) => this.open_node(&asked, node, cx),
                ProjectEvent::Output(term) => this.open_output(term.session, cx),
            });
        self.projects.subscriptions.insert(project.clone(), subscription);
        self.projects.views.insert(project, view);
    }

    /// How each agent of `board` is doing, as this client sees it.
    fn board_agents(&self, board: &Board) -> HashMap<SessionId, AgentSeen> {
        let sessions = std::iter::once(None)
            .chain(board.tasks.keys().copied().map(Some))
            .filter_map(|node| board.terminal(node).map(|(_, s)| s));
        sessions
            .filter_map(|session| {
                let agent = self.agent_state(session)?;
                let status = Status::of_agent(agent)?;
                let asks = matches!(agent.status, AgentStatus::Blocked(_))
                    .then(|| agent_ask_line(agent))
                    .flatten();
                Some((session, AgentSeen { status, asks }))
            })
            .collect()
    }

    /// Counts of what the projects keep, for the leak checks.
    #[cfg(test)]
    pub(super) fn project_sizes(&self) -> [(&'static str, usize); 4] {
        [
            ("projects.views", self.projects.views.len()),
            ("projects.subscriptions", self.projects.subscriptions.len()),
            ("projects.shown", self.projects.shown.len()),
            ("projects.focus", self.projects.focus.len()),
        ]
    }
}

/// The mark a project's palette line wears: its most urgent lane's.
const fn lane_status(lane: Lane) -> Status {
    match lane {
        Lane::NeedsYou => Status::NeedsYou,
        Lane::Failed => Status::Failed,
        Lane::Working | Lane::Verifying => Status::Working,
        Lane::UpNext => Status::Idle,
        Lane::ReadyToMerge | Lane::Merged => Status::Done,
    }
}
