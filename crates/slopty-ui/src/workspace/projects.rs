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
use slopty_client::server::ServerCaller;
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::agent::AgentStatus;
use slopty_proto::items::ItemKind;
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
use slopty_proto::project::{
    LimitsChange, ProjectId, ProjectUpdate, ProjectsPart, ReviewVerdict, TaskId,
};

use super::WorkspaceView;
use super::actions::ToggleProjectBoard;
use super::agents::agent_ask_line;
use crate::icons::Status;
use crate::project::model::{Board, Lane, Projects, TaskAction};
use crate::project::{AgentSeen, Node, ProjectEvent, ProjectView, Seen, StartProject};

/// What the palette and the header call turning a tile to its board.
pub(crate) const SHOW_BOARD: &str = "Show project board";
/// What a terminal says when asked for a board it has none of.
pub(crate) const NO_PROJECT: &str = "No project runs in this terminal";
/// What a board's action says when this client has no server to send it to.
pub(crate) const NO_SERVER: &str = "No server to send it to";
/// What "Start a project here" says away from a terminal.
pub(crate) const NO_TERMINAL: &str = "Stand in a terminal to start a project there";
/// What the person says approving a task's work from the board.
pub(crate) const APPROVED_HERE: &str = "Approved by the person";

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
    /// How the boards' actions reach the server, while the app is linked to one.
    pub caller: Option<ServerCaller>,
    /// A project just started here, whose board shows once the mirror hears of it.
    pub opening: Option<ProjectId>,
}

/// `id` as the workspace keys workers: the one the app gives the server's worker ids.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// The worker id behind `key`: [`worker_key`] the other way. A UUID's simple form is its
/// 128 bits in hex, which is all a key holds.
fn worker_id(key: WorkerKey) -> Option<WorkerId> {
    format!("{:032x}", key.value()).parse().ok()
}

/// A project name made from `name` (a directory's), as [`ProjectId`] takes it, and not one
/// of `taken`: lowercase, every run of anything else a dash, and `-2`, `-3`… when it is.
fn project_name(name: &str, taken: impl Fn(&ProjectId) -> bool) -> Option<ProjectId> {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Room for the widest suffix the loop below can add.
    slug.truncate(ProjectId::MAX_LEN - 4);
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "project" } else { slug };
    (1..1000_u16)
        .map(|n| if n == 1 { slug.to_owned() } else { format!("{slug}-{n}") })
        .filter_map(|name| ProjectId::new(name).ok())
        .find(|id| !taken(id))
}

impl WorkspaceView {
    /// How the boards' actions reach the server: set while the app is linked to one, and
    /// cleared when it lets the server go.
    pub fn set_server_caller(&mut self, caller: Option<ServerCaller>) {
        self.projects.caller = caller;
    }

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

    /// A board asked for a terminal the server runs for a task, a verifier's or a reviewer's:
    /// its tile, focused, or `gone` when there is none.
    fn open_output(&mut self, session: SessionId, gone: &str, cx: &mut Context<Self>) {
        if self.tile_of_session(session).is_none() {
            self.show_notice(gone.to_owned(), cx);
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

    /// Something a board shows changed: they are handed it in the next frame. A project just
    /// started here shows its board as soon as it is heard of.
    fn projects_moved(&mut self, cx: &mut Context<Self>) {
        if let Some(project) =
            self.projects.opening.take_if(|p| self.projects.mirror.get(p).is_some())
        {
            self.open_project(&project, cx);
        }
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
            cx.subscribe(&view, move |this, _view, event: &ProjectEvent, cx| match event {
                ProjectEvent::Open(node) => this.open_node(&asked, *node, cx),
                ProjectEvent::Output(term) => {
                    this.open_output(term.session, "The verifier's terminal has closed", cx);
                }
                ProjectEvent::Reviewer(term) => {
                    this.open_output(term.session, "The reviewer's session has closed", cx);
                }
                ProjectEvent::Act(task, action) => this.act_on_task(&asked, *task, *action, cx),
                ProjectEvent::SetPush(push) => {
                    let verb = Verb::ProjectSet {
                        project: asked.clone(),
                        orchestrator: None,
                        verifier: None,
                        review: None,
                        push: Some(*push),
                        limits: LimitsChange::default(),
                        metadata: None,
                    };
                    this.send_to_server(verb, |_, _| (), cx);
                }
                ProjectEvent::Delete => {
                    this.send_to_server(
                        Verb::ProjectDelete { project: asked.clone() },
                        |_, _| (),
                        cx,
                    );
                }
                ProjectEvent::Say(text) => this.show_notice(text.clone(), cx),
            });
        self.projects.subscriptions.insert(project.clone(), subscription);
        self.projects.views.insert(project, view);
    }

    /// A board's action on a task, as the person's word to the server: a merge and a retry
    /// both put the task in the merge queue, which checks it afresh; an approval stands over
    /// the reviewer's.
    fn act_on_task(
        &mut self,
        project: &ProjectId,
        task: TaskId,
        action: TaskAction,
        cx: &mut Context<Self>,
    ) {
        let project = project.clone();
        let verb = match action {
            TaskAction::Merge | TaskAction::Retry => Verb::TaskMerge { project, task },
            TaskAction::Approve => Verb::TaskReview {
                project,
                task,
                verdict: ReviewVerdict {
                    approved: true,
                    summary: APPROVED_HERE.to_owned(),
                    findings: Vec::new(),
                },
            },
        };
        self.send_to_server(verb, |_, _| (), cx);
    }

    /// Send `verb` to the server and hand its answer to `then`; a refusal is said as a
    /// notice, in the server's words.
    fn send_to_server(
        &mut self,
        verb: Verb,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(caller) = self.projects.caller.clone() else {
            self.show_notice(NO_SERVER.to_owned(), cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| match outcome {
                Outcome::Error { message, .. } => this.show_notice(message, cx),
                _ => then(this, cx),
            })
        })
        .detach();
    }

    /// "Start a project here": make a project of the focused terminal's repository, with the
    /// terminal as its orchestrator, and show its board once the server has it. It is named
    /// for the directory and lands on the branch checked out.
    pub(super) fn start_project(
        &mut self,
        _: &StartProject,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else {
            self.show_notice(NO_TERMINAL.to_owned(), cx);
            return;
        };
        if let Some(project) =
            self.projects.mirror.of_orchestrator(session).map(|b| b.project.id.clone())
        {
            self.open_project(&project, cx);
            return;
        }
        let Some(worker) = self.worker_of_session(session).and_then(worker_id) else {
            self.show_notice(NO_TERMINAL.to_owned(), cx);
            return;
        };
        let summary = self.summary(session);
        let Some(repo) = summary.and_then(|s| s.repo.clone().or_else(|| s.cwd.clone())) else {
            self.show_notice("This terminal has no directory to start a project in".to_owned(), cx);
            return;
        };
        let target = summary.and_then(|s| s.branch.clone()).unwrap_or_else(|| "main".to_owned());
        let title = repo.trim_end_matches('/').rsplit('/').next().unwrap_or(&repo).to_owned();
        let mirror = &self.projects.mirror;
        let Some(project) = project_name(&title, |id| mirror.get(id).is_some()) else {
            self.show_notice(format!("No name is left for a project of {title}"), cx);
            return;
        };
        let verb = Verb::ProjectCreate {
            project: project.clone(),
            title,
            repo,
            target,
            verifier: None,
            review: None,
            push: false,
            orchestrator: Some(TermRef { worker, session }),
            limits: LimitsChange::default(),
            metadata: None,
        };
        self.send_to_server(
            verb,
            move |this, cx| {
                // The server's word of the new project may come before or after its answer.
                if this.projects.mirror.get(&project).is_some() {
                    this.open_project(&project, cx);
                } else {
                    this.projects.opening = Some(project);
                }
            },
            cx,
        );
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
