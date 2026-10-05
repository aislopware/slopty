//! Projects in the workspace: the server's projects mirrored, and each board shown in its
//! orchestrator's tile.
//!
//! The app hands over the server's snapshot and its changes ([`WorkspaceView::projects_part`],
//! [`WorkspaceView::project_update`]). The board is one of an orchestrator's faces, beside its
//! thread and its TUI (the header's switch, ⌘J, the palette's line for the project); its rows open
//! the tiles of the agents they name. Everything a board draws is handed to it in the frame
//! after it changed, compared first, so a board is drawn again only when what it shows moved.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Window};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_client::server::ServerCaller;
use slopty_core::{ItemId, SessionId, WallMs, WorkerId};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
use slopty_proto::project::{
    LimitsChange, ProjectId, ProjectUpdate, ProjectsPart, RunOn, TaskChange, TaskId, TaskState,
};
use slopty_proto::thread::AgentId;

use super::WorkspaceView;
use super::actions::{MakeOrchestrator, StartOrchestrator};
use super::agents::{agent_ask_text, agent_mark_of};
use crate::icons::Status;
use crate::project::create::{NewProject, ProjectSheet, SheetEvent};
use crate::project::model::{Board, Lane, Projects, RunOnPicker, TaskAction};
use crate::project::recap::{Looked, Recap};
use crate::project::{AgentSeen, Node, ProjectEvent, ProjectView, Seen, StartProject, WorkerSeen};

/// What "Start a project here" says away from an agent's terminal, and the way that works.
pub(crate) const NO_TERMINAL: &str = "A project is run by an agent in a terminal: start one with \u{201c}New project\u{2026}\u{201d}";
/// How many pages of the timeline a recap reads back from the server, past what the board
/// holds: far enough for a night away from a busy project.
const RECAP_PAGES: usize = 8;
/// What a project's orchestrator's terminal is called.
pub(crate) const ORCHESTRATOR: &str = "Orchestrator";
/// What the timeline says of a task the person cancelled from the board.
pub(crate) const CANCELLED: &str = "Cancelled by the person";
/// What "Start a project here" and "Make this agent … orchestrator" say in a plain shell.
pub(crate) const NOT_AN_AGENT: &str = "Start an agent in this terminal first: an orchestrator is an agent, and a shell never \
     hears what the board tells it";

/// The open "New project" sheet.
pub(super) struct Sheet {
    view: Entity<ProjectSheet>,
    /// The terminal whose agent orchestrates the project made.
    term: TermRef,
    /// What the dim and the sheet track: Tab stays inside ([`crate::a11y::trap`]).
    scope: gpui::FocusHandle,
    _events: gpui::Subscription,
}

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
    /// A project just started or given an orchestrator here, whose board shows once the
    /// mirror has that terminal as its orchestrator.
    pub opening: Option<(ProjectId, TermRef)>,
    /// The "New project" sheet, open over the workspace for the orchestrator it names.
    pub sheet: Option<Sheet>,
    /// The tile of an agent "New project…" started, whose sheet opens once it is its
    /// terminal's tile ([`WorkspaceView::orchestrator_started`]).
    pub orchestrating: Option<ItemId>,
    /// The "Run on" picker open on a task, per project.
    pub run_on: HashMap<ProjectId, RunOnPicker>,
    /// How far this client read each project's timeline, as its board last hid.
    pub looked: HashMap<ProjectId, Looked>,
    /// The boards on show at the last hand-over: one not among them opened since.
    pub open: HashSet<ProjectId>,
    /// What changed since the last look, for each board that opened onto news.
    pub recaps: HashMap<ProjectId, Recap>,
}

/// The last entry of `board`'s timeline, 0 for an empty one.
fn last_seq(board: &Board) -> u64 {
    board.timeline.back().map_or(0, |e| e.seq)
}

/// What the palette calls `agent`: its own name, an ACP agent by the registry's.
#[must_use]
pub fn agent_label(agent: &AgentId) -> String {
    match agent.0.as_str() {
        AgentId::CLAUDE_CODE => "Claude Code".to_owned(),
        AgentId::CODEX => "Codex".to_owned(),
        AgentId::PI => "pi".to_owned(),
        other => agent.acp_name().unwrap_or(other).to_owned(),
    }
}

/// `id` as the workspace keys workers: the one the app gives the server's worker ids.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// The person's change to `project`'s pushing.
fn set_push(project: &ProjectId, push: bool) -> Verb {
    Verb::ProjectSet {
        project: project.clone(),
        orchestrator: None,
        verifier: None,
        push: Some(push),
        limits: LimitsChange::default(),
        metadata: None,
        members: None,
    }
}

/// The person's change to how `project`'s work is checked: its verifier command, empty for
/// none.
fn set_checks(project: &ProjectId, verifier: String) -> Verb {
    Verb::ProjectSet {
        project: project.clone(),
        orchestrator: None,
        verifier: Some(verifier),
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
        members: None,
    }
}

/// The worker id behind `key`: [`worker_key`] the other way. A UUID's simple form is its
/// 128 bits in hex, which is all a key holds.
pub(super) fn worker_id(key: WorkerKey) -> Option<WorkerId> {
    format!("{:032x}", key.value()).parse().ok()
}

/// A project name made from `name` (a directory's), as [`ProjectId`] takes it, and not one
/// of `taken`: lowercase, every run of anything else a dash, and `-2`, `-3`… when it is.
pub(super) fn project_name(name: &str, taken: impl Fn(&ProjectId) -> bool) -> Option<ProjectId> {
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

    /// Tell the layout which tiles hold wide work, as it changes: a board on show, a review, a
    /// folder's changes. Their columns open out to two thirds of the strip, at least 720 pt
    /// where it has them, and go back when the work leaves; a width the person chose stays
    /// (`Layout::suit`).
    pub(super) fn suit_wide_work(&mut self) {
        let wide: HashSet<TileRef> = self
            .layout
            .tiles()
            .filter(|t| match self.item(*t).map(|i| &i.kind) {
                Some(ItemKind::Terminal { session }) => self.board_shown(*session),
                Some(ItemKind::Review { .. } | ItemKind::Changes { .. }) => true,
                _ => false,
            })
            .collect();
        if wide == self.wide {
            return;
        }
        for tile in wide.difference(&self.wide) {
            self.layout.suit(*tile, true);
        }
        for tile in self.wide.difference(&wide) {
            self.layout.suit(*tile, false);
        }
        self.wide = wide;
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

    /// Show `project`'s board in its orchestrator's tile, and go there. Where its orchestrator
    /// has no tile here, one is opened for it on its worker.
    pub fn open_project(&mut self, project: &ProjectId, cx: &mut Context<Self>) {
        let Some(board) = self.projects.mirror.get(project) else { return };
        let title = board.project.title.clone();
        let Some(term) = board.project.orchestrator else {
            self.show_notice(format!("{title} has no orchestrator yet"), cx);
            return;
        };
        let session = term.session;
        let worker = worker_key(term.worker);
        if self.tile_of_session(session).is_none()
            && self.workers.get(&worker).is_some_and(|w| w.link.is_some())
        {
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::Terminal { session },
                name: None,
                facts: BTreeMap::new(),
            };
            self.propose(worker, ItemOp::Add(item), cx);
        }
        let Some(tile) = self.tile_of_session(session) else {
            self.show_notice(format!("{title}'s orchestrator is on a machine not linked now"), cx);
            return;
        };
        self.focus_tile(tile, cx);
        self.show_board(session, true, cx);
    }

    /// A board asked for a terminal the server runs for a task, its verifier's:
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
        let board_tile =
            board.project.orchestrator.and_then(|term| self.tile_of_session(term.session));
        // A task agent that is itself an orchestrator's session opens on its terminal.
        self.projects.shown.remove(&session);
        if let (Some(from), Some(to)) = (board_tile, self.tile_of_session(session)) {
            self.keep_beside(from, to);
        }
        self.reveal_session(session, cx);
    }

    /// `to` is about to be focused from `from` and should show beside it: when `from`'s column
    /// fills the view, it gives up its full width first, as a column does for a tile opened
    /// beside it. Following the focus would otherwise leave the board cut off at the window's
    /// edge, a sliver with no gutter.
    fn keep_beside(&mut self, from: TileRef, to: TileRef) {
        let (Some(a), Some(b)) = (self.layout.position(from), self.layout.position(to)) else {
            return;
        };
        if a.workspace != b.workspace || a.column == b.column {
            return;
        }
        let full = self
            .layout
            .workspaces()
            .get(a.workspace)
            .and_then(|ws| ws.columns().get(a.column))
            .is_some_and(slopty_client::layout::Column::is_full_width);
        if full {
            self.tick();
            self.layout.focus(from);
            self.layout.toggle_full_width();
        }
    }

    /// What an agent's terminal is to a project, as it is named: "Orchestrator" for the one the
    /// person talks to, and a task's number and title ("#1 Lock the refresh row") for the
    /// agent on that task. Four terminals of one agent read alike otherwise.
    pub(super) fn project_role(&self, session: SessionId) -> Option<String> {
        let mirror = &self.projects.mirror;
        if mirror.of_orchestrator(session).is_some() {
            return Some(ORCHESTRATOR.to_owned());
        }
        let (board, task) = mirror.of_agent(session)?;
        let card = board.tasks.get(&task)?;
        let title = card.title.trim();
        Some(if title.is_empty() { format!("#{task}") } else { format!("#{task} {title}") })
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

    /// "Make this agent `project`'s orchestrator" for each project the focused terminal's
    /// agent does not orchestrate, while an agent runs there.
    pub(super) fn orchestrator_lines(&self) -> Vec<crate::palette::PaletteItem> {
        let Some(session) = self.focused_session() else { return Vec::new() };
        if self.session_agent(session).is_none() {
            return Vec::new();
        }
        self.projects
            .mirror
            .boards()
            .filter(|b| b.project.orchestrator.is_none_or(|t| t.session != session))
            .map(|board| {
                let label = format!("Make this agent {}'s orchestrator", board.project.title);
                let action = MakeOrchestrator { project: board.project.id.clone() };
                crate::palette::PaletteItem::new(&label, Box::new(action), &[])
            })
            .collect()
    }

    /// The focused terminal's agent becomes `project`'s orchestrator, and the tile shows its
    /// board. A plain shell is refused, as it never hears what the board tells it.
    pub(super) fn make_orchestrator(
        &mut self,
        make: &MakeOrchestrator,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else {
            self.show_notice(NO_TERMINAL.to_owned(), cx);
            return;
        };
        if self.session_agent(session).is_none() {
            self.show_notice(NOT_AN_AGENT.to_owned(), cx);
            return;
        }
        let Some(worker) = self.worker_of_session(session).and_then(worker_id) else {
            self.show_notice(NO_TERMINAL.to_owned(), cx);
            return;
        };
        let verb = Verb::ProjectSet {
            project: make.project.clone(),
            members: None,
            orchestrator: Some(TermRef { worker, session }),
            verifier: None,
            push: None,
            limits: LimitsChange::default(),
            metadata: None,
        };
        let (project, term) = (make.project.clone(), TermRef { worker, session });
        self.send_to_server(
            verb,
            move |this, cx| this.open_when_orchestrated(project, term, cx),
            cx,
        );
    }

    /// Show `project`'s board in `term`'s tile, now if the mirror has `term` as its orchestrator
    /// and else once it does: the server's word of the change may come before or after its
    /// answer.
    fn open_when_orchestrated(
        &mut self,
        project: ProjectId,
        term: TermRef,
        cx: &mut Context<Self>,
    ) {
        let mirror = &self.projects.mirror;
        if mirror.get(&project).is_some_and(|b| b.project.orchestrator == Some(term)) {
            self.open_project(&project, cx);
        } else {
            self.projects.opening = Some((project, term));
        }
    }

    /// Something a board shows changed: they are handed it in the next frame. A project just
    /// started or given an orchestrator here shows its board once the mirror says so.
    fn projects_moved(&mut self, cx: &mut Context<Self>) {
        if let Some((project, _)) = self.projects.opening.take_if(|(p, term)| {
            self.projects.mirror.get(p).is_some_and(|b| b.project.orchestrator == Some(*term))
        }) {
            self.open_project(&project, cx);
        }
        self.projects.dirty = true;
        self.changed(cx);
        cx.notify();
    }

    /// Bring the boards in step, once a frame after a change: make the board a tile shows,
    /// hand every board what it shows now, drop the boards of projects gone, and give the
    /// keyboard to a board just turned to.
    pub(super) fn sync_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.orchestrator_started(window, cx);
        if !std::mem::take(&mut self.projects.dirty) {
            return;
        }
        // A board goes with its project; while its project lives it keeps its lens and row.
        let live = &self.projects.mirror;
        self.projects.views.retain(|p, _| live.get(p).is_some());
        self.projects.subscriptions.retain(|p, _| live.get(p).is_some());
        self.look_at_boards(cx);
        if self.projects.shown.is_empty() {
            self.projects.focus.clear();
            return;
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
        let names: BTreeMap<WorkerId, WorkerSeen> = self
            .projects
            .mirror
            .boards()
            .flat_map(|b| {
                let orchestrator = b.project.orchestrator.map(|t| t.worker);
                let tasks = b.tasks.values().flat_map(|c| {
                    let ran = c.assignment.as_ref().map(|a| a.term.worker);
                    let step = c.step.as_ref().map(|s| s.worker);
                    [ran, c.pin, step].into_iter().flatten()
                });
                orchestrator.into_iter().chain(tasks).collect::<Vec<_>>()
            })
            .filter_map(|id| {
                let worker = self.workers.get(&worker_key(id))?;
                let os = worker.caps.as_ref().map(|c| c.os);
                Some((id, WorkerSeen { name: worker.name.clone(), os }))
            })
            .collect();
        let now = crate::clock::now(cx);
        // Only the boards on show: a hidden one is handed everything as it shows again.
        let views: Vec<(ProjectId, Entity<ProjectView>)> = wanted
            .iter()
            .filter_map(|p| Some((p.clone(), self.projects.views.get(p)?.clone())))
            .collect();
        for (project, view) in views {
            let board = self.projects.mirror.get(&project).cloned();
            let agents = board.as_ref().map(|b| self.board_agents(b)).unwrap_or_default();
            let run_on = self.projects.run_on.get(&project).cloned();
            let recap = self.projects.recaps.get(&project).cloned();
            let seen = Seen { board, workers: names.clone(), agents, now, run_on, recap };
            view.update(cx, |v, cx| v.set_seen(seen, cx));
        }
        for project in std::mem::take(&mut self.projects.focus) {
            if let Some(view) = self.projects.views.get(&project).cloned() {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
        }
    }

    /// The boards on show now against those at the last hand-over: a board that hid read its
    /// timeline to the end, and one that opened is handed what changed since this client last
    /// looked. A first look has nothing to compare with, and no recap.
    fn look_at_boards(&mut self, cx: &Context<Self>) {
        let mirror = &self.projects.mirror;
        let now: HashSet<ProjectId> = self
            .projects
            .shown
            .iter()
            .filter_map(|s| Some(mirror.of_orchestrator(*s)?.project.id.clone()))
            .collect();
        let at_ms = WallMs::now();
        let hid: Vec<ProjectId> = self.projects.open.difference(&now).cloned().collect();
        if !hid.is_empty() {
            self.layout_touched(cx);
        }
        for project in hid {
            self.projects.recaps.remove(&project);
            if let Some(board) = self.projects.mirror.get(&project) {
                self.projects.looked.insert(project, Looked { seq: last_seq(board), at_ms });
            }
        }
        let opened: Vec<ProjectId> = now.difference(&self.projects.open).cloned().collect();
        self.projects.open = now;
        for project in opened {
            self.recap(project, cx);
        }
    }

    /// What changed in `project` since this client last looked, from what its board holds,
    /// or read back from the server first when the board holds less than that.
    fn recap(&mut self, project: ProjectId, cx: &Context<Self>) {
        let Some(board) = self.projects.mirror.get(&project).cloned() else { return };
        let Some(since) = self.projects.looked.get(&project).copied() else { return };
        // A cursor past the end is another project's of the same name, made since.
        if since.seq > last_seq(&board) {
            self.projects.looked.remove(&project);
            return;
        }
        let held_from = board.timeline.front().map_or(u64::MAX, |e| e.seq);
        let caller = self.projects.caller.clone();
        let Some(caller) = caller.filter(|_| held_from > since.seq.saturating_add(1)) else {
            let partial = held_from > since.seq.saturating_add(1);
            if let Some(recap) = Recap::of(&board, since, &board.timeline, partial) {
                self.projects.recaps.insert(project, recap);
            }
            return;
        };
        Self::spawn_recap(caller, project, (since, held_from), cx);
    }

    /// Read `project`'s timeline from past `since` up to the entries its board holds, a page
    /// at a time and no more than [`RECAP_PAGES`], then recap it with what the board holds.
    fn spawn_recap(
        caller: ServerCaller,
        project: ProjectId,
        (since, held_from): (Looked, u64),
        cx: &Context<Self>,
    ) {
        let asked = project.clone();
        let read = async move {
            let mut read = Vec::new();
            let mut from = since.seq.saturating_add(1);
            for _ in 0..RECAP_PAGES {
                let verb = Verb::ProjectStatus {
                    project: asked.clone(),
                    since: Some(from),
                    timeout_ms: 0,
                };
                let Outcome::Project(status) = caller.call(verb).await else { break };
                read.extend(status.timeline.into_iter().filter(|e| e.seq < held_from));
                if status.next <= from || status.next >= held_from {
                    return (read, false);
                }
                from = status.next;
            }
            (read, true)
        };
        cx.spawn(async move |this, cx| {
            let (read, partial) = read.await;
            this.update(cx, |this, cx| {
                if !this.projects.open.contains(&project) {
                    return;
                }
                let Some(board) = this.projects.mirror.get(&project).cloned() else { return };
                let gap = read.first().is_none_or(|e| e.seq > since.seq.saturating_add(1));
                let entries = read.iter().chain(board.timeline.iter());
                if let Some(recap) = Recap::of(&board, since, entries, partial || gap) {
                    this.projects.recaps.insert(project, recap);
                    this.projects_moved(cx);
                }
            })
        })
        .detach();
    }

    /// How far this client has read each project's timeline, to keep across launches: what
    /// each board read as it last hid, and for a board on show now, all of it.
    #[must_use]
    pub fn projects_looked(&self) -> Vec<(ProjectId, Looked)> {
        let at_ms = WallMs::now();
        let mut looked = self.projects.looked.clone();
        for project in &self.projects.open {
            if let Some(board) = self.projects.mirror.get(project) {
                looked.insert(project.clone(), Looked { seq: last_seq(board), at_ms });
            }
        }
        let mut looked: Vec<(ProjectId, Looked)> = looked.into_iter().collect();
        looked.sort_by(|a, b| a.0.cmp(&b.0));
        looked
    }

    /// How far the last launch read each project's timeline: the recaps of this one start
    /// there.
    pub fn restore_projects_looked(
        &mut self,
        looked: impl IntoIterator<Item = (ProjectId, Looked)>,
    ) {
        self.projects.looked.extend(looked);
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
                ProjectEvent::Act(task, action) => this.act_on_task(&asked, *task, *action, cx),
                ProjectEvent::SetChecks { verifier } => {
                    this.send_to_server(set_checks(&asked, verifier.clone()), |_, _| (), cx);
                }
                ProjectEvent::SetPush(push) => {
                    this.send_to_server(set_push(&asked, *push), |_, _| (), cx);
                }
                ProjectEvent::Delete => {
                    this.send_to_server(
                        Verb::ProjectDelete { project: asked.clone() },
                        |_, _| (),
                        cx,
                    );
                }
                ProjectEvent::Say(text) => this.show_notice(text.clone(), cx),
                ProjectEvent::Tell(text) => this.tell_orchestrator(&asked, text.clone(), cx),
                ProjectEvent::Pin(task, run_on) => this.pin_task(&asked, *task, *run_on, cx),
                ProjectEvent::CloseRecap => {
                    if this.projects.recaps.remove(&asked).is_some() {
                        this.projects_moved(cx);
                    }
                }
                ProjectEvent::CloseRunOn => {
                    if this.projects.run_on.remove(&asked).is_some() {
                        this.projects_moved(cx);
                    }
                }
            });
        self.projects.subscriptions.insert(project.clone(), subscription);
        self.projects.views.insert(project, view);
    }

    /// A board's action on a task, as the person's word to the server: a merge and a retry
    /// both put the task in the merge queue, which checks it afresh; a next step is said to the
    /// task's agent, as the person. A cancel gives
    /// the task up with the person's word on the timeline, and a stop ends its agent's
    /// terminal, whose session its agent can take up again.
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
            TaskAction::PushAgain => Verb::TaskPush { project, task },
            TaskAction::RunOn => return self.open_run_on(&project, task, cx),
            TaskAction::Cancel => {
                let change = TaskChange {
                    state: Some(TaskState::Failed),
                    note: Some(CANCELLED.to_owned()),
                    ..TaskChange::default()
                };
                Verb::TaskUpdate { project, task, change: Box::new(change) }
            }
            TaskAction::Stop => {
                let board = self.projects.mirror.get(&project);
                let Some((worker, session)) = board.and_then(|b| b.terminal(Some(task))) else {
                    return self.show_notice(format!("#{task} has no agent running"), cx);
                };
                Verb::Close { term: TermRef { worker, session } }
            }
            TaskAction::FixCi | TaskAction::AddressComments | TaskAction::ResolveConflicts => {
                let board = self.projects.mirror.get(&project);
                let Some(text) = board.and_then(|b| b.told(task, action)) else { return };
                let asked = match action {
                    TaskAction::FixCi => "fix CI",
                    TaskAction::AddressComments => "address the comments",
                    _ => "resolve the conflicts",
                };
                let said = format!("Asked #{task}'s agent to {asked}");
                let verb = Verb::TaskTell { project, task: Some(task), text };
                return self.send_to_server(verb, move |this, cx| this.show_notice(said, cx), cx);
            }
        };
        self.send_to_server(verb, |_, _| (), cx);
    }

    /// The person's words from a board's line to its orchestrator. The server keeps them on
    /// the timeline and hands them to the orchestrator through its hooks; words it refuses go
    /// back on the line.
    fn tell_orchestrator(&self, project: &ProjectId, text: String, cx: &mut Context<Self>) {
        let Some(caller) = self.projects.caller.clone() else {
            self.give_back_words(project, text, cx);
            return;
        };
        let verb = Verb::TaskTell { project: project.clone(), task: None, text: text.clone() };
        let project = project.clone();
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| {
                if let Outcome::Error { message, .. } = outcome {
                    this.show_notice(message, cx);
                    this.give_back_words(&project, text, cx);
                }
            })
        })
        .detach();
    }

    /// Put words the server refused back on `project`'s line to its orchestrator.
    fn give_back_words(&self, project: &ProjectId, text: String, cx: &mut Context<Self>) {
        if let Some(view) = self.projects.views.get(project) {
            view.update(cx, |view, cx| view.refused(text, cx));
        }
    }

    /// Open the "Run on" picker on `task`, and ask the server for the workers' facts.
    fn open_run_on(&mut self, project: &ProjectId, task: TaskId, cx: &mut Context<Self>) {
        let picker = RunOnPicker { task, workers: None };
        self.projects.run_on.insert(project.clone(), picker);
        self.projects_moved(cx);
        let verb = Verb::WorkerFacts { worker: None };
        let asked = project.clone();
        self.ask_server(verb, cx, move |this, outcome, cx| {
            let workers = match outcome {
                Outcome::Facts(workers) => workers,
                Outcome::Error { message, .. } => {
                    this.projects.run_on.remove(&asked);
                    this.show_notice(message, cx);
                    this.projects_moved(cx);
                    return;
                }
                _ => return,
            };
            if let Some(picker) = this.projects.run_on.get_mut(&asked).filter(|p| p.task == task) {
                picker.workers = Some(workers);
                this.projects_moved(cx);
            }
        });
    }

    /// The "Run on" picker's choice: the task runs there, or wherever its placement chooses.
    fn pin_task(
        &mut self,
        project: &ProjectId,
        task: TaskId,
        run_on: RunOn,
        cx: &mut Context<Self>,
    ) {
        self.projects.run_on.remove(project);
        self.projects_moved(cx);
        let change = TaskChange { run_on: Some(run_on), ..TaskChange::default() };
        let verb = Verb::TaskUpdate { project: project.clone(), task, change: Box::new(change) };
        self.send_to_server(verb, |_, _| (), cx);
    }

    /// Send `verb` and hand whatever the server answers to `then`.
    fn ask_server(
        &self,
        verb: Verb,
        cx: &Context<Self>,
        then: impl FnOnce(&mut Self, Outcome, &mut Context<Self>) + 'static,
    ) {
        let Some(caller) = self.projects.caller.clone() else { return };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| then(this, outcome, cx))
        })
        .detach();
    }

    /// Send `verb` to the server and hand its answer to `then`; a refusal is said as a
    /// notice, in the server's words.
    pub(super) fn send_to_server(
        &self,
        verb: Verb,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) {
        let Some(caller) = self.projects.caller.clone() else { return };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| match outcome {
                Outcome::Error { message, .. } => this.show_failure(message, cx),
                _ => then(this, cx),
            })
        })
        .detach();
    }

    /// "Start a project here": the "New project" sheet for the focused terminal's agent as
    /// its orchestrator, named for the terminal's directory, its repository and the branch
    /// checked out filled in. A terminal that orchestrates one already shows that one, and a
    /// plain shell is refused: nothing in it would hear what the board tells it. Away from a
    /// terminal (a thread tile's agent has none) it points to "New project…".
    pub(super) fn start_project(
        &mut self,
        _: &StartProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else {
            self.show_notice(NO_TERMINAL.to_owned(), cx);
            return;
        };
        self.open_project_sheet(session, window, cx);
    }

    /// The last step of "New project…": `agent` starts at once in its own tile, with no first
    /// message, and is what each step lists first next time. Its sheet opens once the tile is
    /// its terminal's ([`Self::orchestrator_started`]).
    pub(super) fn start_orchestrator(
        &mut self,
        start: &StartOrchestrator,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StartOrchestrator { worker, agent, cwd, worktree } = start.clone();
        let last = super::agent_start::LastStart { agent: agent.clone(), worker, cwd: cwd.clone() };
        self.last_start = Some(last);
        let item = ItemId::new();
        let starting = super::starting::Starting::new(worker, agent, cwd, None);
        self.open_starting(item, starting.in_worktree(worktree, item), cx);
        self.send_start(item, None, cx);
        self.projects.orchestrating = Some(item);
    }

    /// Once a frame: the agent "New project…" started is in its terminal's tile, so the
    /// "New project" sheet opens for it. While it starts, or its tile waits on its worker's
    /// word, nothing yet; a start that failed or a tile closed lets it go.
    fn orchestrator_started(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.projects.orchestrating else { return };
        if self.starting.has(item) || self.projects.sheet.is_some() {
            return;
        }
        let Some(tile) = self.layout.tiles().find(|t| t.item == item) else {
            self.projects.orchestrating = None;
            return;
        };
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            return;
        };
        self.projects.orchestrating = None;
        self.focus_tile(tile, cx);
        self.open_project_sheet(session, window, cx);
    }

    /// The "New project" sheet for `session`'s agent as its orchestrator; or that terminal's
    /// project, where it orchestrates one; or why not.
    fn open_project_sheet(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) =
            self.projects.mirror.of_orchestrator(session).map(|b| b.project.id.clone())
        {
            self.open_project(&project, cx);
            return;
        }
        if self.session_agent(session).is_none() {
            self.show_notice(NOT_AN_AGENT.to_owned(), cx);
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
        let agent = self
            .session_agent(session)
            .map_or_else(String::new, |a| agent_label(&AgentId(a.to_owned())));
        let machine = self.worker_name(worker_key(worker));
        let orchestrator = format!("Orchestrated by {agent} on {machine}, in this terminal");
        let filled = NewProject { title, repo, target, verifier: None, push: false };
        let theme = self.theme.clone();
        let view = cx.new(|cx| ProjectSheet::new(theme, filled, orchestrator, window, cx));
        let events = cx.subscribe(&view, |this, _sheet, event: &SheetEvent, cx| match event {
            SheetEvent::Create(new) => this.create_project(new.clone(), cx),
            SheetEvent::Cancel => this.close_project_sheet(cx),
        });
        let term = TermRef { worker, session };
        let scope = cx.focus_handle();
        let home = gpui::Focusable::focus_handle(view.read(cx), cx);
        crate::a11y::hold(&scope, &home, cx);
        self.projects.sheet = Some(Sheet { view, term, scope, _events: events });
        cx.notify();
    }

    /// Make the project the sheet holds, with its terminal as the orchestrator, and show its
    /// board once the server has it. A refusal is said and the sheet stays to be put right.
    fn create_project(&mut self, new: NewProject, cx: &mut Context<Self>) {
        let Some(term) = self.projects.sheet.as_ref().map(|s| s.term) else { return };
        if new.title.is_empty() {
            self.show_notice("Name the project".to_owned(), cx);
            return;
        }
        let mirror = &self.projects.mirror;
        let Some(project) = project_name(&new.title, |id| mirror.get(id).is_some()) else {
            self.show_notice(format!("No name is left for a project called {}", new.title), cx);
            return;
        };
        let target = if new.target.is_empty() { "main".to_owned() } else { new.target };
        let verb = Verb::ProjectCreate {
            project: project.clone(),
            title: new.title,
            repo: new.repo,
            target,
            verifier: new.verifier,
            push: new.push,
            orchestrator: Some(term),
            limits: LimitsChange::default(),
            metadata: None,
            members: Vec::new(),
        };
        self.send_to_server(
            verb,
            move |this, cx| {
                this.close_project_sheet(cx);
                this.open_when_orchestrated(project, term, cx);
            },
            cx,
        );
    }

    /// What the open "New project" sheet holds.
    #[cfg(test)]
    pub(super) fn project_sheet_typed(&self, cx: &gpui::App) -> Option<NewProject> {
        Some(self.projects.sheet.as_ref()?.view.read(cx).typed(cx))
    }

    /// Let the "New project" sheet go; the keyboard goes back to the terminal it came from.
    pub(super) fn close_project_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = self.projects.sheet.take() {
            if let Some(tile) = self.tile_of_session(sheet.term.session) {
                self.focus_tile(tile, cx);
            }
            cx.notify();
        }
    }

    /// The "New project" sheet over the workspace, while it is open.
    pub(super) fn render_project_sheet(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::{InteractiveElement as _, IntoElement as _, ParentElement as _};
        let sheet = self.projects.sheet.as_ref()?;
        let backdrop = crate::kit::backdrop(&self.theme, window).id("project-sheet-backdrop");
        Some(
            crate::a11y::trap(backdrop, &sheet.scope)
                .occlude()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _ev, _window, cx| {
                        this.close_project_sheet(cx);
                        cx.stop_propagation();
                    }),
                )
                .child(sheet.view.clone())
                .into_any_element(),
        )
    }

    /// How each agent of `board` is doing, as this client sees it.
    fn board_agents(&self, board: &Board) -> HashMap<SessionId, AgentSeen> {
        let sessions = std::iter::once(None)
            .chain(board.tasks.keys().copied().map(Some))
            .filter_map(|node| board.terminal(node).map(|(_, s)| s));
        sessions
            .filter_map(|session| {
                let agent = self.agent_state(session)?;
                let status = agent_mark_of(agent);
                let asks = agent_ask_text(agent);
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
pub(super) const fn lane_status(lane: Lane) -> Status {
    match lane {
        Lane::NeedsYou => Status::NeedsYou,
        Lane::Failed => Status::Failed,
        Lane::Working | Lane::Verifying => Status::Working,
        Lane::UpNext => Status::Idle,
        Lane::ReadyToMerge | Lane::Merged => Status::Done,
    }
}
