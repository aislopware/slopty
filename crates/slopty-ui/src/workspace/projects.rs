//! Projects in the workspace: the server's projects mirrored, and each board shown in its
//! orchestrator's tile.
//!
//! The app hands over the server's snapshot and its changes ([`WorkspaceView::projects_part`],
//! [`WorkspaceView::project_update`]). The board is a side panel of the orchestrator's tile, on
//! its right beside the thread or the TUI, so the plan and the conversation show together; a
//! tile too narrow for both (a phone's, a thin pane) shows the board over them instead. The
//! header's button, ⌘⇧J and the palette's line for the project show it or put it away; its rows
//! open the tiles of the agents they name. Everything a board draws is handed to it in the frame
//! after it changed, compared first, so a board is drawn again only when what it shows moved.

use std::collections::{BTreeMap, HashMap, HashSet};

use gpui::{AppContext as _, Context, Entity, Window};
use slopty_client::layout::WorkerKey;
use slopty_client::server::ServerCaller;
use slopty_core::{ItemId, SessionId, WallMs, WorkerId};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::orchestration::{Outcome, TermRef, Verb};
use slopty_proto::project::{
    Autonomy, LimitsChange, ProjectId, ProjectUpdate, ProjectsPart, TaskChange, TaskId, TaskLaunch,
    TaskState,
};
use slopty_proto::thread::AgentId;

use super::WorkspaceView;
use super::agents::{agent_ask_text, agent_mark_of};
use super::rollup::META_SEPARATOR;
use crate::icons::Status;
use crate::project::create::{Filled, GoalSheet, NewGoal, SheetEvent, Starter};
use crate::project::model::{Board, Lane, Projects, TaskAction};
use crate::project::recap::{Looked, Recap};
use crate::project::{AgentSeen, Asked, Node, ProjectEvent, ProjectView, Seen, WorkerSeen};

/// What a board's action says when no server is linked to take it.
pub(crate) const NOT_SENT: &str = "Not sent: the server is away";
/// The palette's line that opens the "New goal" sheet.
pub(crate) const NEW_GOAL: &str = "New goal\u{2026}";
/// What "New goal…" says when no machine can start an agent that runs in a terminal.
pub(crate) const NO_ORCHESTRATOR: &str =
    "No machine has an agent that runs in a terminal, and only one can take a goal";
/// What the sheet says when its goal is empty.
pub(crate) const WRITE_THE_GOAL: &str = "Write the goal first";
/// The most a project's name drawn from its goal runs to, in characters.
const NAME_FROM_GOAL: usize = 48;
/// How many pages of the timeline a recap reads back from the server, past what the board
/// holds: far enough for a night away from a busy project.
const RECAP_PAGES: usize = 8;
/// What a project's orchestrator's terminal is called.
pub(crate) const ORCHESTRATOR: &str = "Orchestrator";
/// What the timeline says of a task the person cancelled from the board.
pub(crate) const CANCELLED: &str = "Cancelled by the person";
/// The open "New goal" sheet.
pub(super) struct Sheet {
    view: Entity<GoalSheet>,
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
    /// The orchestrators whose tiles show their board beside them (or over them, too narrow).
    pub shown: HashSet<SessionId>,
    /// The tiles that show a board on their own, by the project each shows
    /// ([`super::board_tiles`]).
    pub tiles: HashMap<ItemId, ProjectId>,
    /// Boards that take the keyboard on the next frame.
    pub focus: HashSet<ProjectId>,
    /// Something a board shows changed since the boards were last handed what they show.
    pub dirty: bool,
    /// How the boards' actions reach the server, while the app is linked to one.
    pub caller: Option<ServerCaller>,
    /// A project just started or given an orchestrator here, whose board shows once the
    /// mirror has that terminal as its orchestrator.
    pub opening: Option<(ProjectId, TermRef)>,
    /// The "New goal" sheet, open over the workspace.
    pub sheet: Option<Sheet>,
    /// The tile of the agent "New goal…" started, and the goal it takes, whose project is
    /// made once that tile is its terminal's ([`WorkspaceView::orchestrator_started`]).
    pub orchestrating: Option<(ItemId, NewGoal)>,
    /// How far this client read each project's timeline, as its board last hid.
    pub looked: HashMap<ProjectId, Looked>,
    /// The boards on show at the last hand-over: one not among them opened since.
    pub open: HashSet<ProjectId>,
    /// What changed since the last look, for each board that opened onto news.
    pub recaps: HashMap<ProjectId, Recap>,
    /// Tiles that came from elsewhere this run, each alone in a background tab, not yet known
    /// as a task's agent: one that turns out to be leaves the tiling ([`super::seating`]).
    pub arrived: HashSet<slopty_client::layout::TileRef>,
    /// The task's agent last opened as the helper preview, which the next one opened takes
    /// over while it is on show ([`WorkspaceView::open_helper`]).
    pub helper: Option<slopty_client::layout::TileRef>,
}

/// The palette's line that shows or puts away the focused orchestrator's board.
pub(crate) const TOGGLE_BOARD: &str = "Show or hide the board";

/// The narrowest orchestrator's tile, in points, that its board stands beside: a thread at its
/// least and the panel. Narrower, the board covers the tile while it shows.
pub(super) const BOARD_BESIDE_MIN: f32 = 760.0;

/// The board's panel beside its orchestrator, in points: two fifths of a tile `tile_w` wide,
/// within a card's least and a reading column's most.
pub(super) fn board_panel_w(tile_w: f32) -> f32 {
    (tile_w * 0.4).clamp(320.0, 460.0)
}

/// The header button's and the menu row's word for the board, `shown` or not.
pub(super) const fn board_word(shown: bool) -> &'static str {
    if shown { "Hide board" } else { "Show board" }
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
        autonomy: None,
    }
}

/// The person's change to how far `project`'s agents go before they ask.
fn set_autonomy(project: &ProjectId, autonomy: Autonomy) -> Verb {
    Verb::ProjectSet {
        project: project.clone(),
        orchestrator: None,
        verifier: None,
        push: None,
        limits: LimitsChange::default(),
        metadata: None,
        autonomy: Some(autonomy),
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
        autonomy: None,
    }
}

/// The worker id behind `key`: [`worker_key`] the other way. A UUID's simple form is its
/// 128 bits in hex, which is all a key holds.
pub(super) fn worker_id(key: WorkerKey) -> Option<WorkerId> {
    format!("{:032x}", key.value()).parse().ok()
}

/// A project name made from `name` (a directory's), as [`ProjectId`] takes it, and not one
/// of `taken`: lowercase, every run of anything else a dash, and `-2`, `-3`… when it is.
/// A project's name drawn from its goal: the goal's first line, cut at a word within
/// [`NAME_FROM_GOAL`] characters, with no stop at its end.
pub(super) fn name_from_goal(goal: &str) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut name = String::new();
    for word in line.split_whitespace() {
        let wider = name
            .chars()
            .count()
            .saturating_add(word.chars().count())
            .saturating_add(usize::from(!name.is_empty()));
        if wider > NAME_FROM_GOAL {
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
    if name.is_empty() {
        name = line.chars().take(NAME_FROM_GOAL).collect();
    }
    name.trim_end_matches(['.', ',', ';', ':', '!', '?']).to_owned()
}

pub(super) fn project_name(name: &str, taken: impl Fn(&ProjectId) -> bool) -> Option<ProjectId> {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Room for the widest suffix the loop below can add, cut back to a whole word.
    let room = ProjectId::MAX_LEN - 4;
    if slug.len() > room {
        let whole = slug.as_bytes().get(room) == Some(&b'-');
        slug.truncate(room);
        if !whole && let Some(at) = slug.rfind('-') {
            slug.truncate(at);
        }
    }
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

    /// The tasks of every project whose work waits to be merged, project by project, each in
    /// its merge queue's order.
    #[must_use]
    pub(super) fn ready_to_merge(&self) -> Vec<(ProjectId, TaskId)> {
        self.projects
            .mirror
            .boards()
            .flat_map(|board| {
                let ready = board
                    .lanes()
                    .into_iter()
                    .filter(|(lane, _)| *lane == Lane::ReadyToMerge)
                    .flat_map(|(_, tasks)| tasks);
                ready.map(|task| (board.project.id.clone(), task)).collect::<Vec<_>>()
            })
            .collect()
    }

    /// Whether what `about` names is a project task's agent: its terminal on a task, or its
    /// thread a task's seat.
    pub(super) fn task_agent(&self, about: super::attention::About) -> bool {
        use super::attention::About;
        match about {
            About::Session(session) => self.projects.mirror.of_agent(session).is_some(),
            About::Thread(thread) => self.projects.mirror.boards().any(|board| {
                board
                    .tasks
                    .values()
                    .any(|card| card.assignment.as_ref().is_some_and(|a| a.thread == Some(thread)))
            }),
        }
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

    /// Whether `session`'s board shows over its tile rather than beside it: its tile was drawn
    /// narrower than [`BOARD_BESIDE_MIN`].
    #[must_use]
    pub(super) fn board_covers(&self, session: SessionId) -> bool {
        self.board_shown(session)
            && self
                .tile_of_session(session)
                .and_then(|tile| self.tile_bounds(tile))
                .is_some_and(|b| f32::from(b.size.width) < BOARD_BESIDE_MIN)
    }

    /// ⌘⇧J: the focused orchestrator's board, shown beside it with the keyboard, or put away.
    pub(super) fn toggle_board(
        &mut self,
        _: &super::actions::ToggleBoard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.focused_session() else { return };
        let shown = self.board_shown(session);
        self.show_board(session, !shown, cx);
    }

    /// The board beside `session`'s tile was pressed: it takes the keyboard, in place of the
    /// thread or the terminal the tile's own press would give it to.
    pub(super) fn board_pressed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(project) =
            self.projects.mirror.of_orchestrator(session).map(|b| b.project.id.clone())
        else {
            return;
        };
        self.projects.focus.insert(project);
        self.projects.dirty = true;
        cx.notify();
    }

    /// Show `session`'s board beside its tile, with the keyboard, or put it away and give the
    /// keyboard back to the thread or the terminal.
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
    /// has no tile here, one is opened for it on its worker. With no orchestrator, one whose
    /// agent ended, or one on a machine away, the board opens in a tile of its own
    /// (`board_tiles`).
    pub fn open_project(&mut self, project: &ProjectId, cx: &mut Context<Self>) {
        let Some(board) = self.projects.mirror.get(project) else { return };
        let Some(term) = board.project.orchestrator else {
            self.open_board_tile(project, cx);
            return;
        };
        let session = term.session;
        let worker = worker_key(term.worker);
        let linked = self.workers.get(&worker).is_some_and(|w| w.link.is_some());
        // An orchestrator whose agent ended, or whose machine is away, has no tile to show it.
        if self.tile_of_session(session).is_none() && (!linked || self.summary(session).is_none()) {
            self.open_board_tile(project, cx);
            return;
        }
        if self.tile_of_session(session).is_none() {
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::Terminal { session },
                name: None,
                facts: BTreeMap::new(),
            };
            self.propose(worker, ItemOp::Add(item), cx);
        }
        let Some(tile) = self.tile_of_session(session) else {
            self.open_board_tile(project, cx);
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
        // A task agent that is itself an orchestrator's session opens on its terminal.
        self.projects.shown.remove(&session);
        self.reveal_session(session, cx);
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
        self.task_reviews_moved(cx);
        self.unseat_helpers(cx);
        // Whether a thread is a project's to brief moves with the boards.
        self.faces_dirty = true;
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
        let tiles: Vec<_> = self.layout.tiles().collect();
        let tiled: HashSet<SessionId> = tiles
            .iter()
            .filter_map(|t| match self.item(*t)?.kind {
                ItemKind::Terminal { session } => Some(session),
                _ => None,
            })
            .collect();
        let mirror = &self.projects.mirror;
        self.projects.shown.retain(|s| tiled.contains(s) && mirror.of_orchestrator(*s).is_some());
        // A board tile closed some other way than ⌘W (its tab, its pane) is let go here.
        let items: HashSet<ItemId> = tiles.iter().map(|t| t.item).collect();
        self.projects.tiles.retain(|item, _| items.contains(item));
        self.look_at_boards(cx);
        let wanted: Vec<ProjectId> = self.boards_on_show().into_iter().collect();
        if wanted.is_empty() {
            self.projects.focus.clear();
            return;
        }
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
                let caps = worker.caps.as_ref();
                let (os, form) = (caps.map(|c| c.os), caps.map(|c| c.form));
                let agents = self.startable_on(worker_key(id));
                let away = worker.link.is_none();
                Some((id, WorkerSeen { name: worker.name.clone(), os, form, agents, away }))
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
            let recap = self.projects.recaps.get(&project).cloned();
            let seen = Seen { board, workers: names.clone(), agents, now, recap };
            view.update(cx, |v, cx| v.set_seen(seen, cx));
        }
        for project in std::mem::take(&mut self.projects.focus) {
            if let Some(view) = self.projects.views.get(&project).cloned() {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
        }
    }

    /// The projects whose boards are on show: in their orchestrators' tiles, or in tiles of
    /// their own.
    fn boards_on_show(&self) -> HashSet<ProjectId> {
        let mirror = &self.projects.mirror;
        let in_orchestrators = self
            .projects
            .shown
            .iter()
            .filter_map(|s| Some(mirror.of_orchestrator(*s)?.project.id.clone()));
        let own = self.projects.tiles.values().filter(|p| mirror.get(p).is_some()).cloned();
        in_orchestrators.chain(own).collect()
    }

    /// The boards on show now against those at the last hand-over: a board that hid read its
    /// timeline to the end, and one that opened is handed what changed since this client last
    /// looked. A first look has nothing to compare with, and no recap.
    fn look_at_boards(&mut self, cx: &Context<Self>) {
        let now = self.boards_on_show();
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
                ProjectEvent::SetAutonomy(level) => {
                    this.send_to_server(set_autonomy(&asked, *level), |_, _| (), cx);
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
                ProjectEvent::Answer { session, ask, pressed } => {
                    this.answer_session(*session, ask, pressed, cx);
                }
                ProjectEvent::CloseRecap => {
                    if this.projects.recaps.remove(&asked).is_some() {
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
    /// the task up with the person's word on the timeline.
    fn act_on_task(
        &mut self,
        project: &ProjectId,
        task: TaskId,
        action: TaskAction,
        cx: &mut Context<Self>,
    ) {
        let project = project.clone();
        let verb = match action {
            TaskAction::Review => return self.review_task(&project, task, cx),
            TaskAction::Merge | TaskAction::Retry => Verb::TaskMerge { project, task },
            TaskAction::Push => Verb::TaskPush { project, task },
            TaskAction::Start => return self.start_task(&project, task, cx),
            TaskAction::Cancel => {
                let change = TaskChange {
                    state: Some(TaskState::Failed),
                    note: Some(CANCELLED.to_owned()),
                    ..TaskChange::default()
                };
                Verb::TaskUpdate { project, task, change: Box::new(change) }
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

    /// What `thread`'s review is to its project, when it is a task's: Merge while the task's
    /// work waits to be merged.
    pub(super) fn task_door(
        &self,
        thread: slopty_proto::thread::ThreadId,
    ) -> Option<crate::review::TaskDoor> {
        let (project, task) = self.reviews.task_of(thread)?;
        let lane = self.projects.mirror.get(project)?.lane(*task)?;
        Some(crate::review::TaskDoor { merge: lane == Lane::ReadyToMerge })
    }

    /// A task review's comments, `text`, back to the task's agent as the person's word, through
    /// the project; `done` hears whether they went. A refusal is said.
    pub(super) fn send_back(
        &self,
        thread: slopty_proto::thread::ThreadId,
        text: String,
        cx: &mut Context<Self>,
        done: impl FnOnce(bool, &mut gpui::App) + 'static,
    ) {
        let (Some((project, task)), Some(caller)) =
            (self.reviews.task_of(thread).cloned(), self.projects.caller.clone())
        else {
            Self::not_sent(cx);
            done(false, cx);
            return;
        };
        let verb = Verb::TaskTell { project, task: Some(task), text };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            let sent = !matches!(outcome, Outcome::Error { .. });
            let _gone = this.update(cx, |this, cx| {
                if let Outcome::Error { message, .. } = outcome {
                    this.show_notice(message, cx);
                }
                let said = format!("Sent back to #{task}'s agent");
                if sent {
                    this.show_notice(said, cx);
                }
                done(sent, cx);
            });
        })
        .detach();
    }

    /// The task reviews open: what their foot offers follows their tasks.
    fn task_reviews_moved(&self, cx: &mut Context<Self>) {
        let doors: Vec<_> = self
            .reviews
            .task_threads()
            .filter_map(|thread| Some((self.review_of(thread)?.clone(), self.task_door(thread))))
            .collect();
        for (view, door) in doors {
            view.update(cx, |v, cx| v.set_task(door, cx));
        }
    }

    /// Merge `task`, ready, on the person's word from outside its board: its row under *Ready
    /// to merge*, or its review's foot.
    pub(super) fn merge_task(&mut self, project: &ProjectId, task: TaskId, cx: &mut Context<Self>) {
        self.act_on_task(project, task, TaskAction::Merge, cx);
    }

    /// "Review" on a finished task: its thread's review over its whole branch, which is what
    /// its merge brings, in a tile of its own on its machine. Its comments go back to the
    /// task's agent as the person's word, and its foot merges it once its work passed. A task
    /// whose agent has no thread here any more has its worktree's changes read instead, with
    /// no way back to an agent.
    pub(super) fn review_task(
        &mut self,
        project: &ProjectId,
        task: TaskId,
        cx: &mut Context<Self>,
    ) {
        let board = self.projects.mirror.get(project);
        let seat = board.and_then(|b| b.tasks.get(&task)).and_then(|c| c.assignment.as_ref());
        let thread = seat.and_then(|a| a.thread.or_else(|| self.session_thread(a.term.session)));
        if let (Some(thread), Some(seat)) = (thread, seat) {
            let key = worker_key(seat.term.worker);
            if self.workers.contains_key(&key) {
                self.reviews.make_task(thread, project.clone(), task);
                if let Some(view) = self.review_of(thread).cloned() {
                    let door = self.task_door(thread);
                    view.update(cx, |v, cx| v.set_task(door, cx));
                }
                self.faces_dirty = true;
                self.ask_review(key, thread, Some(crate::review::Scope::WholeBranch));
                cx.notify();
                return;
            }
        }
        let board = self.projects.mirror.get(project);
        let Some(((worker, path), target)) =
            board.and_then(|b| Some((b.worktree(task)?, b.project.target.clone())))
        else {
            return self.show_notice(format!("#{task} has no worktree to review"), cx);
        };
        let key = worker_key(worker);
        if !self.workers.contains_key(&key) {
            return self.show_notice(format!("#{task}'s machine is not linked here"), cx);
        }
        self.open_changes(key, path, Some(target), cx);
    }

    /// The person's words from a board's line to its orchestrator. The server keeps them on
    /// the timeline and hands them to the orchestrator through its hooks; words it refuses go
    /// back on the line.
    fn tell_orchestrator(&self, project: &ProjectId, text: String, cx: &mut Context<Self>) {
        let Some(caller) = self.projects.caller.clone() else {
            self.give_back_words(project, text, cx);
            Self::not_sent(cx);
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

    /// "Start" on a task not started yet: the task spawned on its pin when it has one, by the
    /// agent its orchestrator is (Claude Code when this client cannot tell), its brief its first
    /// prompt. The server places it, as it would for the orchestrator.
    fn start_task(&mut self, project: &ProjectId, task: TaskId, cx: &mut Context<Self>) {
        if self.projects.caller.is_none() {
            return self.start_refused(project, task, NOT_SENT.to_owned(), cx);
        }
        let board = self.projects.mirror.get(project);
        let pin = board.and_then(|b| b.tasks.get(&task)).and_then(|c| c.pin);
        let orchestrator = board.and_then(|b| b.project.orchestrator).map(|t| t.session);
        let agent = orchestrator
            .and_then(|s| self.session_agent(s))
            .map_or_else(|| AgentId::named(AgentId::CLAUDE_CODE), AgentId::named);
        let asked = project.clone();
        let launch = TaskLaunch { pin, agent };
        let verb = Verb::TaskSpawn { project: project.clone(), task, launch };
        self.ask_server(verb, cx, move |this, outcome, cx| {
            if let Outcome::Error { message, .. } = outcome {
                this.start_refused(&asked, task, message, cx);
            }
        });
    }

    /// The server would not start `task`: say why, and let its card say what it did.
    fn start_refused(
        &mut self,
        project: &ProjectId,
        task: TaskId,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(board) = self.projects.views.get(project) {
            board.update(cx, |board, cx| board.handing_refused(task, cx));
        }
        self.show_failure(message, cx);
    }

    /// A board's action pressed with no server linked says so ([`NOT_SENT`]) rather than
    /// doing nothing.
    fn not_sent(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            this.update(cx, |this, cx| this.show_failure(NOT_SENT.to_owned(), cx))
        })
        .detach();
    }

    /// Send `verb` and hand whatever the server answers to `then`.
    fn ask_server(
        &self,
        verb: Verb,
        cx: &Context<Self>,
        then: impl FnOnce(&mut Self, Outcome, &mut Context<Self>) + 'static,
    ) {
        let Some(caller) = self.projects.caller.clone() else {
            Self::not_sent(cx);
            return;
        };
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
        let Some(caller) = self.projects.caller.clone() else {
            Self::not_sent(cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = caller.call(verb).await;
            this.update(cx, |this, cx| match outcome {
                Outcome::Error { message, .. } => this.show_failure(message, cx),
                _ => then(this, cx),
            })
        })
        .detach();
    }

    /// "New goal…": the one sheet, its folder filled from the focus (else the last start), the
    /// agent and machine from the last start, and the verifier guessed from the repository's
    /// own scripts where the machine has read them.
    pub(super) fn new_goal(
        &mut self,
        _: &super::actions::NewGoal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.projects.sheet.is_some() {
            return;
        }
        let Some(filled) = self.goal_filled(cx) else {
            self.show_notice(NO_ORCHESTRATOR.to_owned(), cx);
            return;
        };
        let theme = self.theme.clone();
        let view = cx.new(|cx| GoalSheet::new(theme, filled, window, cx));
        let events = cx.subscribe(&view, |this, _sheet, event: &SheetEvent, cx| match event {
            SheetEvent::Create(goal) => this.take_goal(goal.clone(), cx),
            SheetEvent::Cancel => this.close_project_sheet(cx),
        });
        let scope = cx.focus_handle();
        let home = gpui::Focusable::focus_handle(view.read(cx), cx);
        crate::a11y::hold(&scope, &home, cx);
        self.projects.sheet = Some(Sheet { view, scope, _events: events });
        cx.notify();
    }

    /// What a goal starts with when the person says no more than the goal: the folder from the
    /// focus, else the last start, else the home; the agents and machines that can take it; the
    /// verifier guessed from the folder's repository. `None` while no machine can start one.
    pub(super) fn goal_filled(&self, cx: &gpui::App) -> Option<Filled> {
        let starters = self.goal_starters();
        let worker = starters.first().and_then(|s| s.machines.first()).map(|(k, _)| *k)?;
        let here = self.focused().filter(|t| t.worker == worker).and_then(|_| self.active_cwd());
        let last = self.starts.last().filter(|l| l.worker == worker).map(|l| l.cwd.clone());
        let folder = here.or(last).unwrap_or_else(|| "~".to_owned());
        let repo = self.repo_at(worker, &folder, cx);
        let verifier = repo.as_deref().and_then(|repo| self.guessed_verifier(worker, repo, cx));
        Some(Filled { folder, verifier, starters })
    }

    /// The goal written on the empty workspace's composer: handed to an orchestrator as the
    /// sheet's Create hands it, with what the sheet would open holding (its first agent on its
    /// first machine, a branch of the project's own, the default autonomy). Whether it went.
    pub(super) fn goal_from_page(&mut self, words: &str, cx: &mut Context<Self>) -> bool {
        let words = words.trim();
        if words.is_empty() {
            return false;
        }
        let Some(filled) = self.goal_filled(cx) else {
            self.show_notice(NO_ORCHESTRATOR.to_owned(), cx);
            return false;
        };
        let Some((starter, worker)) =
            filled.starters.first().and_then(|s| Some((s, s.machines.first()?.0)))
        else {
            return false;
        };
        let goal = NewGoal {
            goal: words.to_owned(),
            folder: filled.folder,
            worker,
            agent: starter.agent.clone(),
            target: String::new(),
            verifier: filled.verifier,
            autonomy: Autonomy::default(),
        };
        self.take_goal(goal, cx);
        true
    }

    /// Where the empty workspace's composer sends a goal, in words: the machine, the folder and
    /// the agent, as its foot shows them. `None` while no machine can start one.
    pub(super) fn page_goal_place(&self, cx: &gpui::App) -> Option<String> {
        let filled = self.goal_filled(cx)?;
        let starter = filled.starters.first()?;
        let (_, machine) = starter.machines.first()?;
        Some(
            [machine.as_str(), filled.folder.as_str(), starter.label.as_str()].join(META_SEPARATOR),
        )
    }

    /// The empty workspace's composer's foot, pressed: "New goal…"'s sheet, holding what was
    /// typed, to say where it goes. The words move into the sheet.
    pub(super) fn goal_sheet_from_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.empty_goal.as_ref().map(|(g, _)| g.clone()) else { return };
        let words = page.read(cx).value().to_string();
        self.new_goal(&super::actions::NewGoal, window, cx);
        let Some(sheet) = self.projects.sheet.as_ref().map(|s| s.view.clone()) else { return };
        sheet.update(cx, |sheet, cx| sheet.set_goal(&words, window, cx));
        page.update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// Once a frame: the empty workspace's composer is made the first time the page shows with
    /// a machine to begin on, and takes the keyboard where nothing else holds it.
    pub(super) fn sync_empty_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.empty_goal.is_some() || !self.bare() || self.workers.is_empty() {
            return;
        }
        let goal = cx.new(|cx| {
            gpui_kit::component::input::TextareaState::new(window, cx)
                .placeholder(super::area::EMPTY_GOAL_HINT)
                .auto_grow(2, crate::project::create::GOAL_ROWS)
                .submit_on_enter(true)
        });
        let entered = cx.subscribe_in(&goal, window, |this, goal, event, window, cx| {
            if let gpui_kit::component::input::InputEvent::PressEnter { shift: false, .. } = event {
                let words = goal.read(cx).value().to_string();
                if this.goal_from_page(&words, cx) {
                    goal.update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
        });
        if self.focus.is_focused(window) {
            goal.update(cx, |input, cx| input.focus(window, cx));
        }
        self.empty_goal = Some((goal, entered));
    }

    /// The agents that can take a goal, those that run in a terminal, the last started first,
    /// each with the machines that can start it: the last start's first, then the focus's,
    /// then the rest by name. An agent no machine can start is left out.
    fn goal_starters(&self) -> Vec<Starter> {
        let mut agents: Vec<AgentId> = self
            .startable_agents()
            .into_iter()
            .filter(super::agent_start::runs_in_terminal)
            .collect();
        let last = self.starts.last();
        if let Some(at) = last.and_then(|l| agents.iter().position(|a| *a == l.agent)) {
            let agent = agents.remove(at);
            agents.insert(0, agent);
        }
        agents
            .into_iter()
            .filter_map(|agent| {
                let mut machines: Vec<WorkerKey> = self
                    .workers
                    .keys()
                    .copied()
                    .filter(|k| self.startable_on(*k).contains(&agent))
                    .collect();
                machines.sort_by_key(|k| self.worker_name(*k).to_lowercase());
                for first in [self.context_worker(), last.map(|l| l.worker)] {
                    if let Some(at) = first.and_then(|k| machines.iter().position(|m| *m == k)) {
                        let key = machines.remove(at);
                        machines.insert(0, key);
                    }
                }
                let machines: Vec<(WorkerKey, String)> =
                    machines.into_iter().map(|k| (k, self.worker_name(k))).collect();
                (!machines.is_empty()).then(|| Starter {
                    label: agent_label(&agent),
                    agent,
                    machines,
                })
            })
            .collect()
    }

    /// A verifier guessed from `repo`'s run scripts on `worker`, as the machine last read them:
    /// the first whose name reads as a check (`gate`, `check`, `verify`, `test`, `ci`).
    fn guessed_verifier(&self, worker: WorkerKey, repo: &str, cx: &gpui::App) -> Option<String> {
        const CHECKS: [&str; 5] = ["gate", "check", "verify", "test", "ci"];
        let hub = self.held_hub(worker)?;
        let scripts = hub.read(cx).git().repo(repo)?.scripts.clone()?;
        CHECKS.iter().find_map(|check| {
            scripts.list.iter().find(|s| s.name.eq_ignore_ascii_case(check)).map(|s| s.line.clone())
        })
    }

    /// The sheet's Create: its goal's agent starts at once in a tile of its own, with no first
    /// message, and the project is made around it once that tile is its terminal's
    /// ([`Self::orchestrator_started`]).
    fn take_goal(&mut self, goal: NewGoal, cx: &mut Context<Self>) {
        if goal.goal.is_empty() {
            self.show_notice(WRITE_THE_GOAL.to_owned(), cx);
            return;
        }
        let at = crate::clock::now(cx);
        let went = slopty_client::starts::LastStart {
            agent: goal.agent.clone(),
            worker: goal.worker,
            cwd: goal.folder.clone(),
            worktree: false,
            at,
        };
        self.start_went(went, None, cx);
        let item = ItemId::new();
        let starting = super::starting::Starting::new(
            goal.worker,
            goal.agent.clone(),
            goal.folder.clone(),
            None,
        );
        self.open_starting(item, starting, cx);
        self.send_start(item, None, cx);
        self.projects.orchestrating = Some((item, goal));
        self.close_project_sheet(cx);
    }

    /// Once a frame: the agent "New goal…" started is in its terminal's tile, so its project is
    /// made around it. While it starts, or its tile waits on its worker's word, nothing yet; a
    /// start that failed or a tile closed lets the goal go.
    fn orchestrator_started(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.projects.orchestrating.as_ref().map(|(item, _)| *item) else {
            return;
        };
        if self.starting.has(item) {
            return;
        }
        let Some(tile) = self.layout.tiles().find(|t| t.item == item) else {
            self.projects.orchestrating = None;
            return;
        };
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            return;
        };
        let Some(worker) = worker_id(tile.worker) else { return };
        let Some((_, goal)) = self.projects.orchestrating.take() else { return };
        self.focus_tile(tile, cx);
        self.create_project(goal, TermRef { worker, session }, cx);
    }

    /// Make the project `goal` names around its orchestrator `term`, which the server hands the
    /// goal to as the person's first message, and show its board once the server has it. A
    /// refusal is said.
    fn create_project(&mut self, goal: NewGoal, term: TermRef, cx: &mut Context<Self>) {
        let title = name_from_goal(&goal.goal);
        let mirror = &self.projects.mirror;
        let Some(project) = project_name(&title, |id| mirror.get(id).is_some()) else {
            self.show_notice(format!("No name is left for a project called {title}"), cx);
            return;
        };
        let key = worker_key(term.worker);
        let summary = self.summary(term.session);
        let repo = summary
            .and_then(|s| s.repo.clone())
            .or_else(|| self.repo_at(key, &goal.folder, cx))
            .unwrap_or_else(|| goal.folder.clone());
        let verb = Verb::ProjectCreate {
            project: project.clone(),
            title,
            goal: Some(goal.goal.clone()),
            autonomy: goal.autonomy,
            repo,
            // Blank asks the server for a branch of the project's own off the checkout.
            target: goal.target,
            verifier: goal.verifier,
            // Pushing is the board head's one setting, off until the person turns it on.
            push: false,
            orchestrator: Some(term),
            limits: LimitsChange::default(),
            metadata: None,
        };
        // The server hands the goal to the orchestrator itself, as the person's first word.
        self.send_to_server(
            verb,
            move |this, cx| this.open_when_orchestrated(project, term, cx),
            cx,
        );
    }

    /// What the open "New goal" sheet holds.
    #[cfg(test)]
    pub(super) fn goal_sheet_typed(&self, cx: &gpui::App) -> Option<NewGoal> {
        self.projects.sheet.as_ref()?.view.read(cx).typed(cx)
    }

    /// Let the "New goal" sheet go; the keyboard goes back to the focused tile.
    pub(super) fn close_project_sheet(&mut self, cx: &mut Context<Self>) {
        if self.projects.sheet.take().is_some() {
            self.pending_return = true;
            cx.notify();
        }
    }

    /// The "New goal" sheet over the workspace, while it is open.
    pub(super) fn render_project_sheet(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        use gpui::{InteractiveElement as _, IntoElement as _, ParentElement as _};
        let sheet = self.projects.sheet.as_ref()?;
        let backdrop = crate::kit::backdrop(&self.theme, window).id("goal-sheet-backdrop");
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
                let asked = (status == Status::NeedsYou).then(|| self.asked(session)).flatten();
                Some((session, AgentSeen { status, asks, asked }))
            })
            .collect()
    }

    /// What `session`'s agent can be answered with from its board row: its thread's open
    /// request, while it takes a press and was not answered here already.
    fn asked(&self, session: SessionId) -> Option<Asked> {
        let thread = self.session_thread(session)?;
        let card = self.thread_request(thread)?;
        if self.thread_answered_here(thread) == Some(&card.id) {
            return None;
        }
        let yes_no = card.answerable();
        let picks = if yes_no {
            Vec::new()
        } else {
            card.buttons.iter().map(|b| (b.choice.clone(), b.label.clone())).collect()
        };
        (yes_no || !picks.is_empty()).then(|| Asked { ask: card.id.clone(), yes_no, picks })
    }

    /// Counts of what the projects keep, for the leak checks.
    #[cfg(test)]
    pub(super) fn project_sizes(&self) -> [(&'static str, usize); 5] {
        [
            ("projects.views", self.projects.views.len()),
            ("projects.subscriptions", self.projects.subscriptions.len()),
            ("projects.shown", self.projects.shown.len()),
            ("projects.focus", self.projects.focus.len()),
            ("projects.tiles", self.projects.tiles.len()),
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
