//! The board: one project drawn in its orchestrator's tile.
//!
//! A header names the project and where its work lands, over a bar that is every task at once,
//! each a segment in its lane's tone. Under it, whatever waits on the person, then one of three
//! lenses: the tree of who split what from whom, down to the subagents running inside a session;
//! the board, each task in the lane its most urgent descendant is in; the timeline, newest
//! first. Every node that runs somewhere opens its agent's tile with a click or ↩.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px, relative,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Merge, Moment, NativeCounts, ProjectId, ReportKind, ReviewRun, RunOn, StepKind, StepState,
    TaskCard, TaskId, TaskState, TaskStep, VerifierRun,
};
use slopty_theme::{Rgb, Theme, Typography, alpha};

use super::model::{
    Board, Lane, Machine, Place, PlaceHow, RunOnPicker, Stage, StageKind, TaskAction, TreeRow,
    finding_place, need_words, os_name, pull_words, queue_words, review_detail, short_commit,
    state_status, state_word, verdict_detail, verdict_tail,
};
use super::recap::{Recap, RecapKind};
use super::spend::{CONTEXT_WARN_BP, MetersBySession, NodeSpend, dollars, limit_line, worked};
use super::{
    AddressComments, ApproveTask, DeleteProject, FixCi, Lens, MergeTask, OpenNode,
    ResolveConflicts, RetryTask, RunTaskOn, SelectNext, SelectPrevious, ShowBoard, ShowMachines,
    ShowTerminal, ShowTimeline, ShowTree, StartProposed, StartTask, TellOrchestrator,
    ToggleAskToStart, TogglePush,
};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize, Status, icon, status_icon};
use crate::palette::{Plate, age_label, dotted, sentence_case};

/// The key context of a board; its keys are bound in it.
pub const CTX: &str = "ProjectBoard";

/// What a board with no tasks yet says.
pub(crate) const NO_TASKS: &str = "No tasks yet";
/// What it adds: where tasks come from.
pub(crate) const NO_TASKS_HINT: &str =
    "The orchestrator splits the goal into tasks; each shows here as it is made.";
/// What a board whose project the server no longer has says.
pub(crate) const PROJECT_GONE: &str = "This project is no longer on the server";
/// The heading over what waits on the person.
pub(crate) const NEEDS_YOU: &str = "Needs you";
/// What the root of the tree is called.
pub(crate) const ORCHESTRATOR: &str = "Orchestrator";
/// A timeline entry for a task made under the title it still has.
pub(crate) const CREATED: &str = "Created";
/// The machines lens with no worker to draw.
pub(crate) const NO_MACHINES: &str = "No workers yet";
/// What the machines lens offers then.
pub(crate) const NO_MACHINES_HINT: &str =
    "The workers this server reaches show here, with the agents on each.";
/// A worker the server cannot reach now.
pub(crate) const AWAY: &str = "Away";
/// The machines lens's heading over the tasks still to start.
pub(crate) const NOT_STARTED: &str = "Not started";
/// The machines lens's heading over what the project's work needs of its machines.
pub(crate) const NEEDS: &str = "What the work needs";
/// The "Run on" picker's first choice.
pub(crate) const ANYWHERE: &str = "Anywhere";
/// What "Anywhere" means.
pub(crate) const ANYWHERE_LINE: &str = "Wherever its placement chooses";
/// The "Run on" picker while the server ranks the workers.
pub(crate) const RANKING: &str = "Ranking the workers\u{2026}";
/// The "Run on" picker's close button.
pub(crate) const CLOSE_RUN_ON: &str = "Close the worker choice";

/// The least a lane is wide at zoom 1: the tile takes as many across as fit.
const LANE_W: f32 = 232.0;
/// How far a level of the tree steps in, at zoom 1.
const INDENT: f32 = 16.0;
/// How far a row that arrives travels up into its place, at zoom 1: a new task, an entry.
const ARRIVE: f32 = 4.0;
/// How often the timeline's ages move on: they say minutes at the finest.
const AGE_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// How often the tree and the board move their time at work on while agents work: often
/// enough that a minute's readout is never more than a moment late, sums of several stretches
/// included, which cross their minutes at no one stretch's.
const AT_WORK_TICK: std::time::Duration = std::time::Duration::from_secs(10);
/// The most facts a row's or a card's second line holds: two separators.
const META_PARTS: usize = 3;
/// The progress bar's height, at zoom 1.
const BAR_H: f32 = 3.0;

pub use super::model::Node;
/// How often the machines lens asks again how the workers are doing, while it shows.
const MACHINES_TICK: std::time::Duration = std::time::Duration::from_secs(5);

/// What a board tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectEvent {
    /// Open this node's agent in its tile.
    Open(Node),
    /// Show a terminal the server runs for a task, its verifier's, in its tile.
    Output(TermRef),
    /// Open the Claude Code session a task's reviewer reads its work in.
    Reviewer(TermRef),
    /// Do this to a task ([`Board::actions`]).
    Act(TaskId, TaskAction),
    /// Push the target after each merge, or stop.
    SetPush(bool),
    /// Let the project go; the person pressed for it twice.
    Delete,
    /// Say this: what was asked of the board cannot be done now.
    Say(String),
    /// The machines lens shows: ask the server how the workers are doing.
    Machines,
    /// Run the task there, or wherever its placement chooses: the "Run on" picker's choice.
    Pin(TaskId, RunOn),
    /// Close the "Run on" picker.
    CloseRunOn,
    /// Tell the orchestrator this, as the person.
    Tell(String),
    /// Start every task whose start is proposed.
    StartAll,
    /// Hold each task's start for the person, or let the orchestrator start them.
    SetAsk(bool),
    /// The person read the recap: close it.
    CloseRecap,
}

/// What the line to the orchestrator says while it is empty.
const COMPOSE_PLACEHOLDER: &str = "Tell the orchestrator what to do next";
/// What it is called to a screen reader.
const COMPOSE_LABEL: &str = "Tell the orchestrator";

/// How long a first "Delete the project" waits for the second that does it.
const DELETE_CONFIRM: std::time::Duration = std::time::Duration::from_secs(5);

/// What a task's checks show on its card and its row: its verifier or its reviewer at work,
/// or the last word that still speaks.
#[derive(Clone, Debug, PartialEq)]
enum Check<'a> {
    /// A verifier or a reviewer at work, with its last line.
    Running { line: String, term: Option<TermRef>, review: bool },
    /// What the verifier said, and the terminal a failed run is kept in.
    Verdict { run: &'a VerifierRun, term: Option<TermRef> },
    /// What the reviewer said, and its session, kept when it asked for changes.
    Review { run: &'a ReviewRun, term: Option<TermRef> },
}

impl Check<'_> {
    const fn term(&self) -> Option<TermRef> {
        match self {
            Self::Running { term, .. } | Self::Verdict { term, .. } | Self::Review { term, .. } => {
                *term
            }
        }
    }

    /// A reviewer's, whose terminal is a Claude Code session to open rather than output.
    const fn is_review(&self) -> bool {
        matches!(self, Self::Review { .. } | Self::Running { review: true, .. })
    }

    /// Whether this block says what `step` would: the check it is, at work or with its word.
    fn speaks_for(&self, step: &TaskStep) -> bool {
        match self {
            Self::Running { .. } => true,
            Self::Verdict { .. } => step.kind == StepKind::Verify,
            Self::Review { .. } => step.kind == StepKind::Review,
        }
    }

    /// A word that lets the work go on: a pass, an approval.
    const fn cleared(&self) -> bool {
        match self {
            Self::Verdict { run, .. } => run.passed,
            Self::Review { run, .. } => run.verdict.approved,
            Self::Running { .. } => false,
        }
    }
}

/// How many of a failed verifier's last lines its card and row show.
const TAIL_LINES: usize = 4;

/// How an agent this client follows is doing, beside its task's state on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSeen {
    /// Its mark.
    pub status: Status,
    /// What it asks the person, while it waits on them.
    pub asks: Option<String>,
}

/// A worker as the board names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerSeen {
    /// Its name.
    pub name: String,
    /// Its system, once it has said.
    pub os: Option<slopty_proto::server::Os>,
}

/// What the workspace hands a board: the project and what the board names it by.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Seen {
    /// The project, or `None` once the server has let it go.
    pub board: Option<Arc<Board>>,
    /// The workers the board names: the orchestrator's, and every one a task runs on, ran on,
    /// is pinned or proposed to, or takes a step on.
    pub workers: BTreeMap<WorkerId, WorkerSeen>,
    /// The agents this client sees, by session.
    pub agents: HashMap<SessionId, AgentSeen>,
    /// The server's clock now, near enough, for the timeline's ages.
    pub now: WallMs,
    /// The workers as the server last said they are doing, for the machines lens.
    pub machines: Vec<Machine>,
    /// The "Run on" picker, while it is open on one of the project's tasks.
    pub run_on: Option<RunOnPicker>,
    /// What changed since this client last looked, from the moment the board opened until the
    /// person closes it or the board hides.
    pub recap: Option<Recap>,
    /// The meters of the project's agents' threads, as this client heard them, by session.
    pub meters: MetersBySession,
}

/// A row the keyboard can stand on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Pick {
    Node(Node),
    Entry(u64),
}

/// One project's board.
pub struct ProjectView {
    id: ProjectId,
    seen: Seen,
    lens: Lens,
    picked: Option<Pick>,
    zoom: f32,
    width: f32,
    theme: Theme,
    /// The theme a hover hint draws by, shared by every hint the board makes.
    hint_theme: Rc<Theme>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    plate: Plate,
    /// Moves the timeline's ages on once a minute while the timeline shows, and asks for the
    /// workers' news while the machines lens does; for the lens it was started for.
    tick: Option<(Lens, Task<()>)>,
    /// When "Delete the project" was asked once, waiting for the second ask that does it.
    delete_asked: Option<std::time::Instant>,
    /// The line to the orchestrator, made with the first frame (it needs the window), and
    /// what watches it.
    composer: Option<(Entity<InputState>, [Subscription; 2])>,
    /// Words the server refused, to put back on the line once it is empty.
    refused: Option<String>,
    /// How many times it was drawn: the proof that an unchanged hand-over draws nothing.
    #[cfg(test)]
    renders: usize,
}

impl std::fmt::Debug for ProjectView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectView")
            .field("id", &self.id)
            .field("lens", &self.lens)
            .field("picked", &self.picked)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ProjectEvent> for ProjectView {}

impl Focusable for ProjectView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProjectView {
    /// A board for `id`, empty until the workspace hands it the project.
    pub fn new(id: ProjectId, theme: Theme, cx: &Context<Self>) -> Self {
        Self {
            id,
            seen: Seen::default(),
            lens: Lens::Tree,
            picked: None,
            zoom: 1.0,
            width: 0.0,
            hint_theme: Rc::new(theme.clone()),
            theme,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            plate: Plate::default(),
            tick: None,
            delete_asked: None,
            composer: None,
            refused: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// Its project.
    #[must_use]
    pub const fn project(&self) -> &ProjectId {
        &self.id
    }

    /// The lens it shows.
    #[must_use]
    pub const fn lens(&self) -> Lens {
        self.lens
    }

    /// The node the keyboard stands on, when it stands on one.
    #[must_use]
    pub fn picked(&self) -> Option<Node> {
        match self.picked? {
            Pick::Node(node) => Some(node),
            Pick::Entry(seq) => self.entry_node(seq),
        }
    }

    /// What it shows, as the workspace last handed it.
    #[must_use]
    pub const fn seen(&self) -> &Seen {
        &self.seen
    }

    /// Show the project as `seen` has it; drawn again only when what it shows changed. The
    /// clock alone draws nothing: the timeline keeps its own time.
    pub fn set_seen(&mut self, seen: Seen, cx: &mut Context<Self>) {
        let same_board = match (&self.seen.board, &seen.board) {
            (Some(was), Some(is)) => Arc::ptr_eq(was, is) || was == is,
            (was, is) => was.is_none() && is.is_none(),
        };
        let same = same_board
            && self.seen.workers == seen.workers
            && self.seen.agents == seen.agents
            && self.seen.machines == seen.machines
            && self.seen.run_on == seen.run_on
            && self.seen.recap == seen.recap
            && self.seen.meters == seen.meters;
        if !same {
            self.seen = seen;
            if self.picked.is_some_and(|p| !self.picks().contains(&p)) {
                self.picked = None;
            }
            cx.notify();
        }
    }

    /// The zoom it is drawn at and its tile's width at rest, in points.
    pub fn set_layout(&mut self, zoom: f32, width: f32, cx: &mut Context<Self>) {
        let across = lanes_across;
        if (self.zoom - zoom).abs() > f32::EPSILON
            || across(self.width, zoom) != across(width, zoom)
        {
            self.zoom = zoom;
            self.width = width;
            cx.notify();
        } else {
            self.width = width;
        }
    }

    /// Draw by another theme.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.hint_theme = Rc::new(theme.clone());
            self.theme = theme;
            cx.notify();
        }
    }

    /// Show `lens`. The machines lens asks the server how the workers are doing.
    pub fn show(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            if lens == Lens::Machines {
                cx.emit(ProjectEvent::Machines);
            }
            self.picked = self.picked().map(Pick::Node).filter(|p| self.picks().contains(p));
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    /// The timeline's ages move on once a minute while it shows, the machines lens asks for
    /// the workers' news every few seconds while it does, the clocks of agents at work move on
    /// once a minute, and nothing ticks otherwise.
    fn keep_time(&mut self, cx: &Context<Self>) {
        let at_work = self.seen.board.as_ref().is_some_and(|b| b.at_work());
        let every = match self.lens {
            Lens::Timeline => AGE_TICK,
            Lens::Machines => MACHINES_TICK,
            Lens::Tree | Lens::Board if at_work => AT_WORK_TICK,
            Lens::Tree | Lens::Board => {
                self.tick = None;
                return;
            }
        };
        if self.tick.as_ref().is_some_and(|(lens, _)| *lens == self.lens) {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let ticked = this.update(cx, |v, cx| {
                    if v.lens == Lens::Machines {
                        cx.emit(ProjectEvent::Machines);
                        if v.seen.board.as_ref().is_some_and(|b| b.at_work()) {
                            v.seen.now = WallMs::now();
                            cx.notify();
                        }
                    } else {
                        v.seen.now = WallMs::now();
                        cx.notify();
                    }
                });
                if ticked.is_err() {
                    break;
                }
            }
        });
        self.tick = Some((self.lens, task));
    }

    /// Give the board the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// Move the keyboard's row by `delta`, stopping at the ends; the first press lands on the
    /// first row.
    pub fn select_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let picks = self.picks();
        if picks.is_empty() {
            return;
        }
        let last = picks.len().saturating_sub(1);
        let at = match self.picked.and_then(|p| picks.iter().position(|q| *q == p)) {
            None => 0,
            Some(at) => at.saturating_add_signed(delta).min(last),
        };
        if let Some(pick) = picks.get(at).copied()
            && self.picked != Some(pick)
        {
            self.picked = Some(pick);
            if let Some(child) = self.child_of(pick) {
                self.scroll.scroll_to_item(child);
            }
            cx.notify();
        }
    }

    /// Open the node the keyboard stands on. A timeline entry about the project itself
    /// opens nothing: the orchestrator's own row is the way back to its terminal, and an entry
    /// is not.
    pub fn open_picked(&self, cx: &mut Context<Self>) {
        let node = match self.picked {
            Some(Pick::Node(node)) => node,
            Some(Pick::Entry(seq)) => match self.entry_node(seq) {
                Some(Some(task)) => Some(task),
                _ => return,
            },
            None => return,
        };
        cx.emit(ProjectEvent::Open(node));
    }

    /// Do `action` to the task the keyboard stands on, or say why not.
    pub fn act_on_picked(&self, action: TaskAction, cx: &mut Context<Self>) {
        let Some(board) = &self.seen.board else { return };
        let Some(Some(task)) = self.picked() else {
            cx.emit(ProjectEvent::Say(format!("Stand on a task to {}", verb_of(action))));
            return;
        };
        if board.actions(task).contains(&action) {
            cx.emit(ProjectEvent::Act(task, action));
        } else {
            cx.emit(ProjectEvent::Say(format!("#{task} has nothing to {}", verb_of(action))));
        }
    }

    /// Start every proposed task, or say there is none.
    pub fn start_all(&self, cx: &mut Context<Self>) {
        let Some(board) = &self.seen.board else { return };
        if board.proposed().is_empty() {
            cx.emit(ProjectEvent::Say(NOTHING_PROPOSED.to_owned()));
        } else {
            cx.emit(ProjectEvent::StartAll);
        }
    }

    /// Hold each task's start for the person, or stop, as the project does not now.
    pub fn toggle_ask(&self, cx: &mut Context<Self>) {
        if let Some(board) = &self.seen.board {
            cx.emit(ProjectEvent::SetAsk(!board.project.ask_to_start));
        }
    }

    /// Push the target after each merge, or stop, as the project does not now.
    pub fn toggle_push(&self, cx: &mut Context<Self>) {
        if let Some(board) = &self.seen.board {
            cx.emit(ProjectEvent::SetPush(!board.project.push));
        }
    }

    /// The first ask says what a second does; a second within five seconds lets the project
    /// go.
    pub fn delete(&mut self, cx: &mut Context<Self>) {
        let Some(board) = &self.seen.board else { return };
        let now = std::time::Instant::now();
        if self.delete_asked.is_some_and(|at| now.duration_since(at) < DELETE_CONFIRM) {
            self.delete_asked = None;
            cx.emit(ProjectEvent::Delete);
            return;
        }
        self.delete_asked = Some(now);
        let title = &board.project.title;
        cx.emit(ProjectEvent::Say(format!(
            "Delete again to let {title} go. Its terminals stay; its tasks and timeline do not"
        )));
    }

    /// Everything the keyboard can stand on, in the order the lens draws it.
    fn picks(&self) -> Vec<Pick> {
        let Some(board) = &self.seen.board else { return Vec::new() };
        match self.lens {
            Lens::Tree => board.tree().into_iter().map(|row| Pick::Node(row.task)).collect(),
            Lens::Board => board
                .lanes()
                .into_iter()
                .flat_map(|(_, tasks)| tasks)
                .map(|task| Pick::Node(Some(task)))
                .collect(),
            Lens::Timeline => board.timeline.iter().rev().map(|e| Pick::Entry(e.seq)).collect(),
            Lens::Machines => self
                .machine_groups(board)
                .into_iter()
                .flat_map(|g| g.nodes)
                .chain(board.waiting_to_start().into_iter().map(Some))
                .map(Pick::Node)
                .collect(),
        }
    }

    /// The machines lens's groups, in the order it draws them: the workers running this
    /// project's agents, by name, then the rest online, then those away. A worker the server
    /// said nothing of yet is drawn from its name alone.
    fn machine_groups(&self, board: &Board) -> Vec<MachineGroup> {
        let mut groups: Vec<MachineGroup> = self
            .seen
            .machines
            .iter()
            .map(|m| MachineGroup {
                worker: m.worker,
                name: m.name.clone(),
                machine: Some(m.clone()),
                nodes: board.on_worker(m.worker),
            })
            .collect();
        let mut running: Vec<WorkerId> = std::iter::once(None)
            .chain(board.tasks.keys().copied().map(Some))
            .filter_map(|node| board.terminal(node).map(|(w, _)| w))
            .collect();
        running.sort_unstable();
        running.dedup();
        for worker in running {
            if !groups.iter().any(|g| g.worker == worker) {
                groups.push(MachineGroup {
                    worker,
                    name: self.worker_name(worker),
                    machine: None,
                    nodes: board.on_worker(worker),
                });
            }
        }
        groups.sort_by(|a, b| {
            let rank = |g: &MachineGroup| {
                let online = g.machine.as_ref().is_none_or(|m| m.online);
                (g.nodes.is_empty(), !online)
            };
            rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
        });
        groups
    }

    /// Which child of the scrolled list draws `pick`: a tree row comes after the running
    /// subagents of the rows above it. The board's lanes are one child.
    fn child_of(&self, pick: Pick) -> Option<usize> {
        let board = self.seen.board.as_ref()?;
        match (self.lens, pick) {
            (Lens::Tree, Pick::Node(node)) => {
                let mut child = 0_usize;
                for row in board.tree() {
                    if row.task == node {
                        return Some(child);
                    }
                    let running = board
                        .natives
                        .get(&row.task)
                        .map_or(0, |n| n.agents.iter().filter(|a| a.stopped_ms.is_none()).count());
                    child = child.saturating_add(1).saturating_add(running);
                }
                None
            }
            (Lens::Timeline, Pick::Entry(seq)) => {
                board.timeline.iter().rev().position(|e| e.seq == seq)
            }
            _ => None,
        }
    }

    fn entry_node(&self, seq: u64) -> Option<Node> {
        let board = self.seen.board.as_ref()?;
        board.timeline.iter().find(|e| e.seq == seq).map(|e| e.task)
    }

    fn worker_name(&self, worker: WorkerId) -> String {
        self.seen.workers.get(&worker).map_or_else(|| "a worker".to_owned(), |w| w.name.clone())
    }

    /// Where `node` is, as its chip says it ("studio · macOS") and as its hint and its label
    /// say it in full: how it is there, its worktree and branch, why it went there, and what a
    /// click does.
    fn where_words(&self, board: &Board, node: Node) -> Option<(Place, String, String)> {
        let place = board.place(node)?;
        let name = self.worker_name(place.worker);
        let os = self.seen.workers.get(&place.worker).and_then(|w| w.os).map(os_name);
        let short = os.map_or_else(|| name.clone(), |os| format!("{name} \u{b7} {os}"));
        let on = os.map_or_else(|| name.clone(), |os| format!("{name}, {os}"));
        let mut lines = vec![match place.how {
            PlaceHow::Runs => format!("Runs on {on}"),
            PlaceHow::Ran => format!("Ran on {on}"),
            PlaceHow::Pinned => format!("Pinned to {on}"),
            PlaceHow::Proposed => format!("Would start on {on}"),
        }];
        lines.extend(place.worktree.as_ref().map(|w| format!("Worktree {w}")));
        lines.extend(place.branch.as_ref().map(|b| format!("Branch {b}")));
        lines.extend(place.why.as_ref().map(|why| format!("Why: {why}")));
        let task = node.filter(|t| board.movable(*t));
        lines.push(if task.is_some() { MOVE_HINT } else { MACHINES_HINT }.to_owned());
        Some((place, short, lines.join("\n")))
    }

    /// `node`'s place as a quiet chip: the worker and its system, in a stronger ink while its
    /// agent runs there. Its hint says the rest; a click moves a task not started yet ("Run
    /// on…") and shows the machines lens otherwise.
    fn where_chip(
        &self,
        board: &Board,
        node: Node,
        prefix: &str,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (place, short, hint) = self.where_words(board, node)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let (glyph, tone) = match place.how {
            PlaceHow::Runs => (IconName::Server, s.text_secondary),
            PlaceHow::Ran => (IconName::Server, s.text_muted),
            PlaceHow::Pinned => (IconName::Lock, s.text_muted),
            PlaceHow::Proposed => (IconName::MoveRight, s.text_muted),
        };
        let movable = node.filter(|t| board.movable(*t));
        let id = format!("{prefix}-{}-where", node_key(node));
        let selector = id.clone();
        let hint_theme = Rc::clone(&self.hint_theme);
        let label = hint.replace('\n', ". ");
        let el = div()
            .id(SharedString::from(id))
            .debug_selector(move || selector)
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .flex_none()
            .max_w_full()
            .flex()
            .items_center()
            .gap(self.z(sp.xxs))
            .px(self.z(sp.xxs))
            .rounded(self.z(theme.radii.sm))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(tone))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.overlay)).text_color(hsla(s.text)))
            .child(
                icon(theme, glyph, IconSize::Inline, hsla(tone))
                    .size(self.z(theme.typography.meta())),
            )
            .child(
                div().min_w_0().overflow_hidden().text_ellipsis().child(SharedString::from(short)),
            )
            .tooltip(move |_window, cx| {
                let theme = Rc::clone(&hint_theme);
                cx.new(|_| crate::kit::Hint::new(hint.clone(), "", theme)).into()
            });
        Some(tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
            cx.stop_propagation();
            this.picked = Some(Pick::Node(node));
            match movable {
                Some(task) => cx.emit(ProjectEvent::Act(task, TaskAction::RunOn)),
                None => this.show(Lens::Machines, cx),
            }
            cx.notify();
        })))
    }

    /// The agent running `node` as this client sees it, when it sees it.
    fn agent(&self, board: &Board, node: Node) -> Option<&AgentSeen> {
        let (_, session) = board.terminal(node)?;
        self.seen.agents.get(&session)
    }

    /// The mark a node draws: its agent's while the task follows it, else its state's.
    fn node_status(&self, board: &Board, node: Node) -> Option<Status> {
        let live = self.agent(board, node).map(|a| a.status);
        match node {
            None => live,
            Some(task) => {
                let state = board.tasks.get(&task)?.state;
                let own = state_status(state);
                Some(if state.follows_the_agent() { live.unwrap_or(own) } else { own })
            }
        }
    }

    /// Where a view of it is summed up in words: its lens and its tasks, for the self-test's
    /// dump and a screen reader.
    #[must_use]
    pub fn summary(&self) -> String {
        let Some(board) = &self.seen.board else { return PROJECT_GONE.to_owned() };
        let lanes: Vec<String> = board
            .lanes()
            .into_iter()
            .map(|(lane, tasks)| format!("{} {}", lane.title(), tasks.len()))
            .collect();
        match lanes.as_slice() {
            [] => NO_TASKS.to_owned(),
            lanes => lanes.join(", "),
        }
    }

    /// How many times it has drawn.
    #[cfg(test)]
    #[must_use]
    pub const fn renders(&self) -> usize {
        self.renders
    }
}

// ----- drawing ---------------------------------------------------------------------------------

/// How many lanes a tile `width` points wide, drawn at `zoom`, sets side by side: as many as
/// fit, one at the least. They share the width equally and wrap past it, so a lane that comes
/// or goes moves no card sideways.
pub(super) fn lanes_across(width: f32, zoom: f32) -> u16 {
    let lanes = u16::try_from(Lane::ALL.len()).unwrap_or(u16::MAX);
    (2..=lanes).rev().find(|&n| width >= f32::from(n) * LANE_W * zoom).unwrap_or(1)
}

/// A lane's tone: colour for the two lanes that need the person, and for the rest the ink
/// of how far along they are, the finished a step brighter than what is still to do.
const fn lane_tone(theme: &Theme, lane: Lane) -> Rgb {
    let s = &theme.surfaces;
    match lane {
        Lane::NeedsYou => s.warn,
        Lane::Failed => s.error,
        Lane::Working | Lane::Verifying | Lane::UpNext => s.text_muted,
        Lane::ReadyToMerge | Lane::Merged => s.text_secondary,
    }
}

/// The push toggle's one name, said as pressed or not.
pub(crate) const PUSH: &str = "Push after each merge";
/// The header's toggle for holding each task's start for the person.
pub(crate) const ASK_TO_START: &str = "Ask before each task starts";
/// The plan band's heading.
pub(crate) const PROPOSED: &str = "Proposed";
/// The plan band's button.
pub(crate) const START_ALL: &str = "Start all";
/// A proposal's word in its row, for a task its orchestrator would start.
pub(crate) const PROPOSED_WORD: &str = "Proposed";
/// The plan band with no finished task to estimate from.
pub(crate) const NO_ESTIMATE: &str = "No finished task to estimate from yet";
/// "Start all" with nothing proposed.
pub(crate) const NOTHING_PROPOSED: &str = "No task waits for you to start it";
/// The recap's heading when the person looked a moment ago.
pub(crate) const RECAP: &str = "Since you last looked";
/// The recap's button that closes it.
pub(crate) const CLOSE_RECAP: &str = "Close the recap";
/// The recap's last line when it could not read back as far as the person's last look.
pub(crate) const RECAP_PARTIAL: &str = "And earlier changes the recap could not read";
/// What a click on a task's place does while it can still move.
pub(crate) const MOVE_HINT: &str = "Click to choose where it runs";
/// What a click on a place does once its agent has started.
pub(crate) const MACHINES_HINT: &str = "Click to see the machines";
/// The header's way back to the orchestrator's terminal.
pub(crate) const SHOW_TERMINAL: &str = "Show the orchestrator's terminal";

/// Where a project's work lands and how it is checked, as one sentence: "slopty → main,
/// verified by cargo gate and reviewed".
fn place_line(project: &slopty_proto::project::Project) -> String {
    let place = format!("{} \u{2192} {}", project.repo, project.target);
    match (&project.verifier, project.review.is_some()) {
        (Some(verifier), true) => format!("{place}, verified by {verifier} and reviewed"),
        (Some(verifier), false) => format!("{place}, verified by {verifier}"),
        (None, true) => format!("{place}, reviewed"),
        (None, false) => place,
    }
}

/// What an action does, as "nothing to …" and "stand on a task to …" say it.
const fn verb_of(action: TaskAction) -> &'static str {
    match action {
        TaskAction::Merge => "merge",
        TaskAction::Retry => "retry",
        TaskAction::Approve => "approve",
        TaskAction::RunOn => "choose where it runs",
        TaskAction::Start => "start",
        TaskAction::FixCi => "fix",
        TaskAction::AddressComments => "address",
        TaskAction::ResolveConflicts => "resolve",
    }
}

/// A status's tone on the board: colour only for what needs the person, warn for *Needs
/// you* and error for *Failed*; every other state is the muted ink, and recedes.
const fn board_tone(theme: &Theme, status: Status) -> Rgb {
    let s = &theme.surfaces;
    match status {
        Status::NeedsYou => s.warn,
        Status::Failed => s.error,
        Status::Idle | Status::Working | Status::Running | Status::Done | Status::Away => {
            s.text_muted
        }
    }
}

/// What a row says to a screen reader: its parts that say anything, comma by comma.
fn said(parts: &[&str]) -> SharedString {
    let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    SharedString::from(parts.join(", "))
}

/// An element id for the `n`th of a kind.
fn numbered(kind: &'static str, n: u64) -> ElementId {
    ElementId::NamedInteger(kind.into(), n)
}

/// A node's element id and selector: `project-node-orchestrator`, `project-node-3`.
fn node_key(node: Node) -> String {
    node.map_or_else(|| "project-node-orchestrator".to_owned(), |t| format!("project-node-{t}"))
}

impl ProjectView {
    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }

    /// The name, where the work lands and how it is checked, how far along it is and how
    /// many of its agents run, and its two controls: pushing, and the way back to the
    /// orchestrator's terminal. The readouts are columns, so the line under the title is a
    /// sentence and not a string of facts.
    fn header(&self, board: &Board, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let project = &board.project;
        let (merged, total) = board.progress();
        let progress = (total > 0).then(|| format!("{merged} of {total} merged"));
        let live = format!("{} of {} live", board.live(), project.limits.live_per_project);
        let place = place_line(project);
        let readout = |id: &'static str, text: String| {
            crate::kit::tabular(div())
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex_none()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_secondary))
                .child(SharedString::from(text))
        };
        let push = project.push;
        let push_toggle =
            crate::kit::icon_toggle(theme, "project-push", IconName::Upload, PUSH, push, self.zoom)
                .on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(ProjectEvent::SetPush(!push));
                }));
        let terminal = crate::kit::icon_button_at(
            theme,
            "project-terminal",
            IconName::SquareTerminal,
            SHOW_TERMINAL,
            self.zoom,
        )
        .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::Open(None))));
        let ask = project.ask_to_start;
        let ask_toggle = crate::kit::icon_toggle(
            theme,
            "project-ask",
            IconName::Hand,
            ASK_TO_START,
            ask,
            self.zoom,
        )
        .on_click(cx.listener(move |_this, _ev, _w, cx| {
            cx.emit(ProjectEvent::SetAsk(!ask));
        }));
        let title = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .min_w_0()
            .child(
                icon(theme, IconName::Workflow, IconSize::Inline, hsla(s.text_secondary))
                    .size(self.z(theme.typography.icon())),
            )
            .child(
                div()
                    .id("project-title")
                    .debug_selector(|| "project-title".to_owned())
                    .role(Role::Heading)
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.title()))
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(project.title.clone())),
            )
            .child(readout("project-live", live))
            .children(progress.map(|p| readout("project-progress", p)))
            .children(self.spent_readouts(board))
            .child(ask_toggle)
            .child(push_toggle)
            .child(terminal);
        let meta = div()
            .id("project-place")
            .debug_selector(|| "project-place".to_owned())
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(place));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(self.z(sp.xxs))
            .px(self.z(sp.inset()))
            .pt(self.z(sp.md))
            .pb(self.z(sp.sm))
            .child(title)
            .child(meta)
            .child(self.bar(board))
    }

    /// What the project spent, in the header: its time at work, then its cost and the plan's
    /// rate windows once the agents' threads say them. Each says on hover how the
    /// orchestrator's share and its tasks' make it up.
    fn spent_readouts(&self, board: &Board) -> Vec<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spend = board.project_spend(self.seen.now, &self.seen.meters);
        let readout = |id: &'static str, text: String, hint: Option<String>, tone: Rgb| {
            let hint_theme = Rc::clone(&self.hint_theme);
            let label = hint.clone().unwrap_or_else(|| text.clone());
            crate::kit::tabular(div())
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Label)
                .aria_label(SharedString::from(label))
                .flex_none()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(tone))
                .child(SharedString::from(text))
                .when_some(hint, |el, hint| {
                    el.tooltip(move |_window, cx| {
                        let theme = Rc::clone(&hint_theme);
                        cx.new(|_| crate::kit::Hint::new(hint.clone(), "", theme)).into()
                    })
                })
        };
        let mut out = Vec::new();
        if spend.total_ms() >= SHOWN_FROM_MS {
            let hint = format!(
                "{} of work: tasks {}, orchestrator {}",
                worked(spend.total_ms()),
                worked(spend.tasks_ms),
                worked(spend.orchestrator_ms)
            );
            out.push(readout(
                "project-spent",
                worked(spend.total_ms()),
                Some(hint),
                s.text_secondary,
            ));
        }
        if let Some(cost) = spend.total_cost() {
            let part = |c: Option<u64>| c.map_or_else(|| "not heard".to_owned(), dollars);
            let hint = format!(
                "{} spent: tasks {}, orchestrator {}",
                dollars(cost),
                part(spend.tasks_cost),
                part(spend.orchestrator_cost)
            );
            out.push(readout("project-cost", dollars(cost), Some(hint), s.text_secondary));
        }
        let ids = ["project-limit-0", "project-limit-1", "project-limit-2"];
        for (limit, id) in spend.limits.iter().zip(ids) {
            let tone = if limit.used_bp >= CONTEXT_WARN_BP { s.warn } else { s.text_secondary };
            out.push(readout(id, limit_line(limit), None, tone));
        }
        out
    }

    /// A task's actions, as buttons on its row or card: what needs the person to move on.
    fn actions(
        &self,
        board: &Board,
        task: TaskId,
        prefix: &str,
        cx: &Context<Self>,
    ) -> Option<Div> {
        // Choosing a worker is the machines lens's: elsewhere it is the palette's, so a tree
        // of planned tasks is not a column of the same button.
        let actions: Vec<TaskAction> = board
            .actions(task)
            .into_iter()
            .filter(|a| {
                *a != TaskAction::RunOn || matches!(prefix, "project-machine" | "project-plan")
            })
            .collect();
        if actions.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let buttons = actions.into_iter().map(|action| {
            let id = action.selector(prefix, task);
            let selector = id.clone();
            let el = div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{} #{task}", action.label())))
                // A ghost, as a tile header's actions are: its words in the text's tone and a
                // fill only under the pointer. Boxed, a row's Retry and Approve read as two
                // more chips beside its state.
                .flex_none()
                .px(self.z(sp.xs))
                .rounded(self.z(theme.radii.sm))
                .text_size(self.z(theme.typography.meta()))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .text_color(hsla(s.text))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(action.label());
            tab_stop(el, s.accent).on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.stop_propagation();
                cx.emit(ProjectEvent::Act(task, action));
            }))
        });
        Some(div().flex_none().flex().items_center().gap(self.z(sp.xxs)).children(buttons))
    }

    /// Every task at once: a segment each, in its own lane's tone, left to right as the lanes
    /// run. The bar is the board seen from across the room.
    fn bar(&self, board: &Board) -> Div {
        let theme = &self.theme;
        let total = board.tasks.len();
        // The bar's legend: what each segment counts, in the lanes' own words.
        let legend = self.summary();
        let hint_theme = Rc::clone(&self.hint_theme);
        let track = div()
            .id("project-bar")
            .debug_selector(|| "project-bar".to_owned())
            .role(Role::ProgressIndicator)
            .aria_label(SharedString::from(self.summary()))
            .mt(self.z(theme.spacing.xs))
            .h(self.z(BAR_H))
            .w_full()
            .flex()
            .gap(self.z(1.0))
            .rounded(self.z(BAR_H))
            .overflow_hidden()
            .bg(hsla(theme.surfaces.border_subtle))
            .tooltip(move |_window, cx| {
                let theme = Rc::clone(&hint_theme);
                let legend = legend.clone();
                cx.new(|_| crate::kit::Hint::new(legend, "", theme)).into()
            });
        if total == 0 {
            return div().child(track);
        }
        let mut by_lane: BTreeMap<Lane, usize> = BTreeMap::new();
        for card in board.tasks.values() {
            let n = by_lane.entry(Lane::of(card.state)).or_default();
            *n = n.saturating_add(1);
        }
        #[expect(clippy::cast_precision_loss, reason = "task counts are small")]
        let share = |n: usize| n as f32 / total as f32;
        let segments = by_lane.into_iter().map(|(lane, n)| {
            let tone = lane_tone(theme, lane);
            let fill =
                if lane == Lane::Merged { hsla(tone) } else { hsla_alpha(tone, alpha::STRONG) };
            div().h_full().w(relative(share(n))).bg(fill)
        });
        div().child(track.children(segments))
    }

    /// The tasks whose agents wait on the person, over the lens: never below a fold. The board
    /// leads with its own *Needs you* lane, so there the band holds only the orchestrator,
    /// which has no card.
    fn needs_you(&self, board: &Board, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let mut nodes: Vec<Node> = Vec::new();
        if self.agent(board, None).is_some_and(|a| a.status == Status::NeedsYou) {
            nodes.push(None);
        }
        if self.lens != Lens::Board {
            nodes.extend(board.needs_you().into_iter().map(Some));
        }
        if nodes.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let rows = nodes.into_iter().map(|node| {
            let asks = self.agent(board, node).and_then(|a| a.asks.clone());
            let status = node.and_then(|t| board.tasks.get(&t)).and_then(|c| c.status.clone());
            let second = asks.or(status).unwrap_or_else(|| "Waiting on you".to_owned());
            let line = Line { asking: Some(second), said: None, depth: 0, prefix: "project-needs" };
            self.node_row(board, node, line, cx)
        });
        Some(
            div()
                .id("project-needs-you")
                .debug_selector(|| "project-needs-you".to_owned())
                .role(Role::Group)
                .aria_label(NEEDS_YOU)
                .flex_none()
                .mb(self.z(theme.spacing.xs))
                .pb(self.z(theme.spacing.xxs))
                .bg(hsla(s.raised))
                .child(self.heading("project-needs-heading", NEEDS_YOU, Some(s.warn)))
                .children(rows),
        )
    }

    /// What changed since this client last looked, over what needs the person: a line per
    /// kind of change, what needs them first, and a button that closes it.
    fn recap(&self, board: &Board, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let recap = self.seen.recap.as_ref()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let away = std::time::Duration::from_millis(
            self.seen.now.as_millis().saturating_sub(recap.since.at_ms.as_millis()),
        );
        let heading = match age_label(away).as_str() {
            "now" => RECAP.to_owned(),
            age => format!("Since you looked, {age} ago"),
        };
        let close = crate::kit::icon_button_at(
            theme,
            "project-recap-close",
            IconName::X,
            CLOSE_RECAP,
            self.zoom,
        )
        .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::CloseRecap)));
        let head = div()
            .flex()
            .items_center()
            .pr(self.z(sp.xs))
            .child(
                div()
                    .id("project-recap-heading")
                    .debug_selector(|| "project-recap-heading".to_owned())
                    .role(Role::Heading)
                    .aria_label(SharedString::from(heading.clone()))
                    .flex_1()
                    .min_w_0()
                    .px(self.z(sp.inset()))
                    .pt(self.z(sp.xs))
                    .pb(self.z(sp.xxs))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(heading)),
            )
            .child(close);
        let partial = recap
            .partial
            .then(|| self.recap_line("project-recap-partial", None, RECAP_PARTIAL.to_owned()));
        let lines = recap.lines.iter().map(|line| {
            let id = format!("project-recap-{}", recap_word(line.kind));
            self.recap_line(&id, Some(line.kind), line.text(board))
        });
        Some(
            div()
                .id("project-recap")
                .debug_selector(|| "project-recap".to_owned())
                .role(Role::Group)
                .aria_label(RECAP)
                .flex_none()
                .mb(self.z(sp.xs))
                .pb(self.z(sp.xs))
                .bg(hsla(s.raised))
                .child(head)
                .children(lines)
                .children(partial),
        )
    }

    /// One line of the recap: its kind's mark, in the warning tone when it needs the person,
    /// and its words.
    fn recap_line(&self, id: &str, kind: Option<RecapKind>, text: String) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (glyph, tone) = match kind {
            Some(kind) if kind.needs_you() => (recap_icon(kind), s.warn),
            Some(kind) => (recap_icon(kind), s.text_muted),
            None => (IconName::Clock, s.text_muted),
        };
        let key = id.to_owned();
        div()
            .id(SharedString::from(id.to_owned()))
            .debug_selector(move || key)
            .role(Role::ListItem)
            .aria_label(SharedString::from(text.clone()))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.inset()))
            .py(self.z(theme.spacing.xxs))
            .min_w_0()
            .child(
                icon(theme, glyph, IconSize::Inline, hsla(tone))
                    .flex_none()
                    .size(self.z(theme.typography.icon())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(text)),
            )
    }

    /// The plan before it fans out, over the lens: the tasks the orchestrator proposed, each
    /// with where it would start and why, how long they take as the project's finished tasks
    /// say, and a button to start them all.
    fn plan(&self, board: &Board, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let proposed = board.proposed();
        if proposed.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let kind = proposed
            .first()
            .and_then(|t| board.tasks.get(t))
            .map_or(String::new(), |c| c.kind.clone());
        let tasks = if proposed.len() == 1 {
            "1 task".to_owned()
        } else {
            format!("{} tasks", proposed.len())
        };
        let estimate = board
            .estimate(&kind)
            .map_or_else(|| NO_ESTIMATE.to_owned(), |e| sentence_case(&e.line()));
        let summary = format!("{tasks}. {estimate}.");
        let start_all = div()
            .id("project-plan-start-all")
            .debug_selector(|| "project-plan-start-all".to_owned())
            .role(Role::Button)
            .aria_label(START_ALL)
            .flex_none()
            .px(self.z(sp.sm))
            .rounded(self.z(theme.radii.sm))
            .border_1()
            .border_color(hsla(s.border))
            .text_size(self.z(theme.typography.meta()))
            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.overlay)))
            .child(START_ALL);
        let start_all = tab_stop(start_all, s.accent)
            .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::StartAll)));
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .pr(self.z(sp.inset()))
            .child(self.heading("project-plan-heading", PROPOSED, None).flex_none())
            .child(
                div()
                    .id("project-plan-summary")
                    .debug_selector(|| "project-plan-summary".to_owned())
                    .flex_1()
                    .min_w_0()
                    .pt(self.z(sp.xs))
                    .pb(self.z(sp.xxs))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(summary)),
            )
            .child(start_all);
        let rows = proposed.into_iter().map(|task| {
            let said =
                board.tasks.get(&task).and_then(|c| c.proposed.as_ref()).map(|p| match p.on {
                    Some(worker) if p.why.is_empty() => {
                        format!("Would start on {}", self.worker_name(worker))
                    }
                    Some(worker) => {
                        format!("Would start on {}: {}", self.worker_name(worker), p.why)
                    }
                    None => sentence_case(&p.why),
                });
            let line = Line { asking: None, said, depth: 0, prefix: "project-plan" };
            self.node_row(board, Some(task), line, cx)
        });
        Some(
            div()
                .id("project-plan")
                .debug_selector(|| "project-plan".to_owned())
                .role(Role::Group)
                .aria_label(PROPOSED)
                .flex_none()
                .mb(self.z(sp.xs))
                .pb(self.z(sp.xxs))
                .bg(hsla(s.raised))
                .child(head)
                .children(rows),
        )
    }

    /// A quiet label over a group of rows, on the column their marks stand in.
    fn heading(&self, id: &'static str, text: &'static str, tone: Option<Rgb>) -> Stateful<Div> {
        let theme = &self.theme;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .role(Role::Heading)
            .aria_label(text)
            .px(self.z(theme.spacing.inset()))
            .pt(self.z(theme.spacing.xs))
            .pb(self.z(theme.spacing.xxs))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(tone.unwrap_or(theme.surfaces.text_muted)))
            .child(text)
    }

    /// The three lenses, as tabs on a plate of their own, and what the board holds.
    fn lenses(&self, cx: &Context<Self>) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let tab = |lens: Lens| {
            let on = self.lens == lens;
            let id = lens.selector();
            let el = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Tab)
                .aria_label(lens.title())
                .aria_selected(on)
                .flex()
                .items_center()
                .gap(self.z(sp.xs))
                .px(self.z(sp.sm))
                .py(self.z(sp.xxs))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .text_size(self.z(theme.typography.small()))
                .when(on, |el| el.bg(hsla(s.overlay)).text_color(hsla(s.text)))
                .when(!on, |el| {
                    el.text_color(hsla(s.text_muted))
                        .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
                })
                .child(
                    icon(
                        theme,
                        lens.icon(),
                        IconSize::Inline,
                        hsla(if on { s.text_secondary } else { s.text_muted }),
                    )
                    .size(self.z(theme.typography.icon())),
                )
                .child(lens.title());
            tab_stop(el, s.accent)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.show(lens, cx)))
        };
        div()
            .id("project-lenses")
            .debug_selector(|| "project-lenses".to_owned())
            .role(Role::TabList)
            .aria_label("Lens")
            .flex_none()
            .flex()
            .items_center()
            .gap(self.z(sp.xxs))
            .px(self.z(sp.inset() - sp.sm))
            .pb(self.z(sp.xs))
            .border_b_1()
            .border_color(hsla(s.border_subtle))
            .child(tab(Lens::Tree))
            .child(tab(Lens::Board))
            .child(tab(Lens::Timeline))
            .child(tab(Lens::Machines))
    }

    /// One node on two lines: its mark, its number and title with its state at the right, then
    /// where it runs and what it is on. `depth` steps it in under its parent.
    fn node_row(&self, board: &Board, node: Node, line: Line, cx: &Context<Self>) -> AnyElement {
        let Line { asking, said: own_line, depth, prefix } = line;
        let mark = asking.is_some().then_some(Status::NeedsYou);
        let second = asking.or(own_line);
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let card = node.and_then(|t| board.tasks.get(&t));
        let status = mark.or_else(|| self.node_status(board, node));
        let title = card.map_or_else(|| ORCHESTRATOR.to_owned(), |c| c.title.clone());
        // A row under "Needs you" says no word its heading says.
        let word = card.filter(|_| mark.is_none()).map(|c| {
            let word = if c.proposed.is_some() { PROPOSED_WORD } else { state_word(c.state) };
            (word, board_tone(theme, state_status(c.state)))
        });
        let actions = node.and_then(|task| self.actions(board, task, prefix, cx));
        let run_on = node.and_then(|task| self.run_on_block(task, prefix, cx));
        let spend = board.spend(node, self.seen.now, &self.seen.meters);
        let spent_words = spent_words(&spend);
        let settled = card.is_some_and(|c| c.state == TaskState::Merged);
        // The tree shows a verifier running or failed under its row; a pass is a word in it.
        let check = card
            .filter(|_| prefix == "project-row")
            .and_then(|c| Self::check(board, c))
            .filter(|c| !c.cleared());
        // A row that says its own line, and the machines lens's, which groups by worker, show
        // no place.
        let placed = second.is_none() && prefix != "project-machine";
        let meta =
            second.unwrap_or_else(|| self.node_meta(board, node, card, check.as_ref(), false));
        // The place sits at the end of a line it shares, so the rows' places read as one
        // column; alone, it stands where the line's words would.
        let place = placed
            .then(|| self.where_chip(board, node, prefix, cx))
            .flatten()
            .map(|chip| chip.when(!meta.is_empty(), gpui::Styled::ml_auto));
        let place_words = placed
            .then(|| self.where_words(board, node))
            .flatten()
            .map_or_else(String::new, |(_, short, _)| short);
        let key = format!("{prefix}-{}", node_key(node));
        let spent = self.spent_readout(&key, &spend);
        let picked = self.picked() == Some(node)
            && match self.lens {
                Lens::Tree => prefix == "project-row",
                Lens::Machines => prefix == "project-machine",
                Lens::Board | Lens::Timeline => false,
            };
        let number = node.map(|t| {
            crate::kit::tabular(div())
                .flex_none()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(format!("#{t}")))
        });
        let first = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .min_w_0()
            .h(self.z(theme.density.row * 0.9))
            .children(number)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(hsla(if settled { s.text_secondary } else { s.text }))
                    .when(node.is_none(), |el| {
                        el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    })
                    .child(SharedString::from(title.clone())),
            )
            .children(spent)
            .children(word.map(|(word, tone)| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(tone))
                    .child(word)
            }))
            .children(actions);
        let text = (!meta.is_empty()).then(|| {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(dotted(theme, meta.clone()))
        });
        let second = (place.is_some() || text.is_some()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(self.z(sp.xs))
                .min_w_0()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .children(text)
                .children(place)
        });
        let label =
            said(&[&title, word.map_or("", |(word, _)| word), &meta, &place_words, &spent_words]);
        let selector = key.clone();
        let row = div()
            .id(SharedString::from(key.clone()))
            .debug_selector(move || selector)
            .role(Role::TreeItem)
            .aria_label(label)
            .relative()
            .flex()
            .items_start()
            .gap(self.z(sp.sm))
            .mx(self.z(sp.xs))
            .pl(self.z(INDENT.mul_add(depth_f(depth), sp.inset() - sp.xs)))
            .pr(self.z(sp.inset() - sp.xs))
            .py(self.z(sp.xs))
            .rounded(self.z(theme.radii.sm))
            .cursor_pointer()
            .when(settled, |el| el.opacity(alpha::STRONG))
            .hover(move |el| el.bg(hsla(if mark.is_some() { s.overlay } else { s.raised })))
            .children((depth > 0).then(|| self.guides(depth)))
            .child(
                div()
                    .flex_none()
                    .h(self.z(theme.density.row * 0.9))
                    .flex()
                    .items_center()
                    .child(self.mark(status)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(first)
                    .children(second)
                    .children(check.as_ref().map(|c| self.check_block(&key, c, cx)))
                    .children(run_on),
            );
        let arrive = ElementId::Name(format!("{key}-in").into());
        let row = if picked { self.plate.mark(row, key) } else { row };
        let row = tab_stop(row, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
            this.picked = Some(Pick::Node(node));
            cx.emit(ProjectEvent::Open(node));
            cx.notify();
        }));
        crate::kit::slide_fade(row, arrive, ARRIVE, crate::kit::Pace::Fade, cx)
    }

    /// A node's time at work beside its state, with its subtree's when it split work off, and
    /// its context once full enough to matter.
    fn spent_readout(&self, key: &str, spend: &NodeSpend) -> Option<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let ms = if spend.has_subtree() { spend.subtree_ms } else { spend.own_ms };
        let ms = Some(ms).filter(|ms| *ms >= SHOWN_FROM_MS);
        let context = spend.context_shown();
        if ms.is_none() && context.is_none() {
            return None;
        }
        let time = ms.map(|ms| {
            let id = format!("{key}-spent");
            crate::kit::tabular(div())
                .debug_selector(move || id)
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(worked(ms)))
        });
        let context = context.map(|(bp, warns)| {
            let id = format!("{key}-context");
            crate::kit::tabular(div())
                .debug_selector(move || id)
                .text_color(hsla(if warns { s.warn } else { s.text_muted }))
                .child(SharedString::from(format!("{}%", bp / 100)))
        });
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.meta()))
                .children(context)
                .children(time),
        )
    }

    /// The hairlines that tie a row to its parent's column: one per level above it.
    fn guides(&self, depth: usize) -> Div {
        let theme = &self.theme;
        let line = hsla(theme.surfaces.border_subtle);
        let first =
            theme.typography.icon_large().mul_add(0.5, theme.spacing.inset() - theme.spacing.xs);
        div().absolute().top_0().bottom_0().left_0().children((1..=depth).map(|level| {
            let x = INDENT.mul_add(depth_f(level.saturating_sub(1)), first);
            div().absolute().top_0().bottom_0().left(self.z(x)).w(px(1.0)).bg(line)
        }))
    }

    /// A status mark in its fixed slot at the board's zoom.
    fn mark(&self, status: Option<Status>) -> Div {
        let theme = &self.theme;
        let slot = div()
            .flex_none()
            .size(self.z(theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_center();
        match status {
            Some(status) => slot.child(status_icon(
                theme,
                status,
                self.z(theme.typography.icon()),
                hsla(board_tone(theme, status)),
            )),
            None => slot.child(
                icon(theme, IconName::Bot, IconSize::Inline, hsla(theme.surfaces.text_secondary))
                    .size(self.z(theme.typography.icon())),
            ),
        }
    }

    /// A node's second line, the three facts that matter most of what moves it on (what it
    /// waits on, the step it is at, its place in the queue, its last pass, where it merged),
    /// what its agent says it is doing, where it runs, its branch and pull request, that it
    /// only reads, and what runs inside it. Two separators at the most: the rest is on its
    /// card's other lines, the timeline, and its agent's tile. `piped` leaves out what a
    /// pipeline row under it says already.
    fn node_meta(
        &self,
        board: &Board,
        node: Node,
        card: Option<&TaskCard>,
        check: Option<&Check<'_>>,
        piped: bool,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        let proposed = board.proposed().len();
        if node.is_none() && proposed > 0 {
            let tasks =
                if proposed == 1 { "1 task".to_owned() } else { format!("{proposed} tasks") };
            parts.push(format!("Waits on you to start {tasks}"));
        }
        if let Some(card) = card {
            let waits = board.waiting_on(card.id);
            if !waits.is_empty() && matches!(card.state, TaskState::Planned) {
                let list: Vec<String> = waits.iter().map(|t| format!("#{t}")).collect();
                parts.push(format!("after {}", list.join(", ")));
            }
            // A step under way or failed says so on the row; one done is the timeline's, and a
            // check running, or a word with its own block, says it there.
            let shown = card
                .step
                .as_ref()
                .filter(|s| !matches!(s.state, StepState::Done { .. }))
                .filter(|s| !check.is_some_and(|c| c.speaks_for(s)))
                .filter(|s| !(piped && s.running() && s.kind == StepKind::Merge));
            if let Some(step) = shown {
                parts.push(super::model::step_line(step, |w| self.worker_name(w)));
            }
            let merging =
                card.step.as_ref().is_some_and(|s| s.kind == StepKind::Merge && s.running());
            if let Some((place, _)) = board.queue_place(card.id).filter(|_| !merging && !piped) {
                parts.push(queue_words(place));
            }
            if check.is_none() && !piped {
                if let Some(run) = board.review(card.id).filter(|r| r.verdict.approved) {
                    parts.push(format!("Approved at {}", short_commit(&run.head)));
                } else if let Some(run) = board.verdict(card.id).filter(|r| r.passed) {
                    parts.push(format!("Passed at {}", short_commit(&run.head)));
                }
            }
            if let Some(Merge::Merged { target, head, pushed, .. }) = &card.merge {
                let pushed = if *pushed { ", pushed" } else { "" };
                parts.push(format!("into {target} at {}{pushed}", short_commit(head)));
            }
            if let Some(status) = card.status.as_deref().filter(|s| !s.is_empty()) {
                parts.push(crate::kit::first_line(status).to_owned());
            }
        }
        if let Some(card) = card {
            if let Some(branch) = card.branch.as_ref().filter(|_| !piped) {
                parts.push(branch.clone());
            }
            if let Some((words, _)) = pull_words(card).filter(|_| !piped) {
                parts.push(words);
            }
            if card.read_only {
                parts.push("reads only".to_owned());
            }
        }
        let counts = card.map_or(board.orchestrator_natives, |c| c.natives);
        parts.extend(natives_line(counts));
        parts.truncate(META_PARTS);
        parts.join(" \u{b7} ")
    }

    /// What `card`'s checks show: a verifier or a reviewer at work, else the reviewer's word
    /// and then the verifier's while it still speaks to the task as it is now
    /// ([`Board::review`], [`Board::verdict`]). An approval stands for the pass it followed.
    fn check<'a>(board: &'a Board, card: &'a TaskCard) -> Option<Check<'a>> {
        let checks = |s: &TaskStep| matches!(s.kind, StepKind::Verify | StepKind::Review);
        let running = card.step.as_ref().filter(|s| s.running() && (checks(s) || s.term.is_some()));
        if let Some(step) = running
            && let StepState::Running { phase, .. } = &step.state
        {
            let line = crate::kit::first_line(phase).to_owned();
            let review = step.kind == StepKind::Review;
            return Some(Check::Running { line, term: step.term, review });
        }
        let kept = |kind: StepKind| {
            card.step
                .as_ref()
                .filter(|s| s.kind == kind && matches!(s.state, StepState::Failed { .. }))
                .and_then(|s| s.term)
        };
        if let Some(run) = board.review(card.id) {
            return Some(Check::Review { run, term: kept(StepKind::Review) });
        }
        let run = board.verdict(card.id)?;
        Some(Check::Verdict { run, term: kept(StepKind::Verify) })
    }

    /// A verifier's run or verdict under a task: its mark and word, the commits it judged and
    /// how it ended, the way to its terminal, and for a failure the last lines it printed.
    fn check_block(&self, key: &str, check: &Check<'_>, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        // Red is for a run that failed; a reviewer asking for changes is a word, not an alarm.
        let (glyph, tone, word, detail, tail) = match check {
            Check::Running { line, review: false, .. } => {
                let status = Status::Working;
                (status.icon(), board_tone(theme, status), "Verifying", line.clone(), Vec::new())
            }
            Check::Running { line, review: true, .. } => {
                let status = Status::Working;
                (status.icon(), board_tone(theme, status), "Reviewing", line.clone(), Vec::new())
            }
            Check::Verdict { run, .. } if run.passed => {
                (IconName::CircleCheck, s.text_secondary, "Passed", verdict_detail(run), Vec::new())
            }
            Check::Verdict { run, .. } => (
                IconName::CircleX,
                s.error,
                "Failed",
                verdict_detail(run),
                verdict_tail(run, TAIL_LINES),
            ),
            Check::Review { run, .. } if run.verdict.approved => (
                IconName::CircleCheck,
                s.text_secondary,
                "Approved",
                review_detail(run),
                Vec::new(),
            ),
            Check::Review { run, .. } => (
                IconName::MessageSquareWarning,
                s.text_secondary,
                "Changes asked",
                review_detail(run),
                Vec::new(),
            ),
        };
        let review = check.is_review();
        let (link_word, link_label) = if review {
            ("Reviewer", "Open the reviewer's session")
        } else {
            ("Output", "Show the verifier's output")
        };
        let output = check.term().map(|term| {
            let id = format!("{key}-output");
            let selector = id.clone();
            let link = div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Link)
                .aria_label(link_label)
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(sp.xxs))
                .px(self.z(sp.xxs))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |el| el.text_color(hsla(s.text)).bg(hsla(s.overlay)))
                .child(
                    icon(theme, IconName::SquareTerminal, IconSize::Inline, hsla(s.text_muted))
                        .size(self.z(theme.typography.icon())),
                )
                .child(link_word);
            tab_stop(link, s.accent).on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.stop_propagation();
                cx.emit(if review {
                    ProjectEvent::Reviewer(term)
                } else {
                    ProjectEvent::Output(term)
                });
            }))
        });
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .min_w_0()
            .child(
                icon(theme, glyph, IconSize::Inline, hsla(tone))
                    .size(self.z(theme.typography.icon())),
            )
            .child(div().flex_none().text_color(hsla(tone)).child(word))
            .child(
                crate::kit::tabular(div())
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(hsla(s.text_muted))
                    .child(dotted(theme, detail)),
            )
            .children(output);
        let tail = (!tail.is_empty()).then(|| {
            let id = format!("{key}-tail");
            let selector = id.clone();
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Log)
                .flex()
                .flex_col()
                .px(self.z(sp.sm))
                .py(self.z(sp.xs))
                .rounded(self.z(theme.radii.sm))
                .bg(hsla(s.panel))
                // What the program printed, in the face a terminal and a tool's output use; it
                // reads a size larger than the chrome's at the same points.
                .font_family(theme.typography.mono_families.first().cloned().unwrap_or_default())
                .text_size(self.z(theme.typography.caption()))
                .text_color(hsla(s.text_secondary))
                .children(tail.into_iter().map(|line| {
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(SharedString::from(line.to_owned()))
                }))
        });
        let findings = match check {
            Check::Review { run, .. } => self.findings(key, run),
            _ => None,
        };
        let selector = format!("{key}-check");
        div()
            .debug_selector(move || selector)
            .flex()
            .flex_col()
            .gap(self.z(sp.xs))
            .pt(self.z(sp.xxs))
            .min_w_0()
            .text_size(self.z(theme.typography.meta()))
            .child(head)
            .children(tail)
            .children(findings)
    }

    /// What a reviewer found, a line each, what blocks first as the server keeps them: its
    /// severity, where it points in the face a path is read in, and what it says from its
    /// first line, with a count of the rest. The session holds the review whole.
    fn findings(&self, key: &str, run: &ReviewRun) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        if run.verdict.findings.is_empty() {
            return None;
        }
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let shown = run.verdict.findings.iter().take(TAIL_LINES);
        let rest = run
            .verdict
            .findings
            .len()
            .saturating_sub(TAIL_LINES)
            .saturating_add(usize::from(run.more));
        let rows = shown.map(|f| {
            // What blocks reads at the default ink with its ✕, the rest muted: no red, which
            // says a run failed.
            let tone = if f.blocking { s.text } else { s.text_muted };
            let severity = div()
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(sp.xxs))
                .text_color(hsla(tone))
                .when(f.blocking, |el| {
                    el.child(
                        icon(theme, IconName::X, IconSize::Inline, hsla(tone))
                            .size(self.z(theme.typography.icon())),
                    )
                })
                .child(sentence_case(&f.severity));
            let place = finding_place(f).map(|place| {
                // The file's own name and line, as narrow as a card is: the path is in the
                // session and in what the agent was told.
                let short = place.rsplit('/').next().unwrap_or(&place).to_owned();
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .font_family(mono.clone())
                    .text_size(self.z(theme.typography.caption()))
                    .text_color(hsla(s.text_muted))
                    .child(short)
            });
            // Its words under its severity and place, the lane's width and two lines of it:
            // the reviewer's session holds the rest.
            let body = div()
                .min_w_0()
                .overflow_hidden()
                .line_clamp(2)
                .text_ellipsis()
                .text_color(hsla(s.text_secondary))
                .child(crate::kit::first_line(&f.body).to_owned());
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(self.z(sp.xs))
                        .min_w_0()
                        .child(severity)
                        .children(place),
                )
                .child(body)
        });
        let more = (rest > 0).then(|| {
            let word = if rest == 1 { "finding" } else { "findings" };
            div().text_color(hsla(s.text_muted)).child(format!("{rest} more {word}"))
        });
        let id = format!("{key}-findings");
        let selector = id.clone();
        Some(
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::List)
                .flex()
                .flex_col()
                .gap(self.z(sp.xxs))
                .px(self.z(sp.sm))
                .py(self.z(sp.xs))
                .rounded(self.z(theme.radii.sm))
                .bg(hsla(s.panel))
                .children(rows)
                .children(more),
        )
    }

    /// Claude Code's own subagents running under `node`, as leaves of the tree.
    fn native_rows(&self, board: &Board, row: TreeRow) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let Some(natives) = board.natives.get(&row.task) else { return Vec::new() };
        natives
            .agents
            .iter()
            .filter(|a| a.stopped_ms.is_none())
            .map(|agent| {
                let id = format!("{}-native-{}", node_key(row.task), agent.id);
                let selector = id.clone();
                let depth = row.depth.saturating_add(1);
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector)
                    .role(Role::TreeItem)
                    .aria_label(SharedString::from(format!("Subagent {}", agent.kind)))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(self.z(sp.sm))
                    .mx(self.z(sp.xs))
                    .pl(self.z(INDENT.mul_add(depth_f(depth), sp.inset() - sp.xs)))
                    .h(self.z(theme.density.row))
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_secondary))
                    .child(self.guides(depth))
                    .child(self.mark(Some(Status::Working)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(agent.kind.clone())),
                    )
                    .into_any_element()
            })
            .collect()
    }

    fn tree(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        for row in board.tree() {
            let line = Line { asking: None, said: None, depth: row.depth, prefix: "project-row" };
            out.push(self.node_row(board, row.task, line, cx));
            out.extend(self.native_rows(board, row));
        }
        if board.tasks.is_empty() {
            out.push(self.empty(NO_TASKS, NO_TASKS_HINT));
        }
        out
    }

    /// The lanes, as many across as the tile fits ([`lanes_across`]), sharing its width.
    fn board(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let lanes = board.lanes();
        if lanes.is_empty() {
            return vec![self.empty(NO_TASKS, NO_TASKS_HINT)];
        }
        let theme = &self.theme;
        let sp = theme.spacing;
        let columns = lanes.into_iter().map(|(lane, tasks)| {
            let tone = lane_tone(theme, lane);
            let count = tasks.len();
            let head = div()
                .flex()
                .items_center()
                .gap(self.z(sp.xs))
                .px(self.z(sp.xs))
                .pb(self.z(sp.xs))
                .text_size(self.z(theme.typography.small()))
                .child(div().flex_none().size(self.z(6.0)).rounded_full().bg(hsla(tone)))
                .child(div().text_color(hsla(theme.surfaces.text_secondary)).child(lane.title()))
                .child(
                    crate::kit::tabular(div())
                        .text_color(hsla(theme.surfaces.text_muted))
                        .child(SharedString::from(count.to_string())),
                );
            let cards = tasks.into_iter().filter_map(|task| {
                let card = board.tasks.get(&task)?;
                Some(self.card(board, card, lane, cx))
            });
            let selector = format!("project-lane-{}", lane.selector());
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .role(Role::List)
                .aria_label(lane.title())
                .min_w_0()
                .flex()
                .flex_col()
                .gap(self.z(sp.xs))
                .child(head)
                .children(cards)
                .into_any_element()
        });
        let grid = div()
            .grid()
            .grid_cols(lanes_across(self.width, self.zoom))
            .items_start()
            .gap_x(self.z(sp.sm))
            .gap_y(self.z(sp.md))
            .px(self.z(sp.inset() - sp.xs))
            .pt(self.z(sp.sm))
            .children(columns);
        vec![grid.into_any_element()]
    }

    /// One task on the board: its number and title, where it runs, and what it waits on.
    fn card(&self, board: &Board, card: &TaskCard, lane: Lane, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let node = Some(card.id);
        let picked = self.picked() == Some(node);
        let own = Lane::of(card.state);
        let status = self.node_status(board, node);
        let check = Self::check(board, card);
        let key = format!("project-card-{}", card.id);
        // The check's own block says its stage, with its detail.
        let mut stages = board.pipeline(card.id);
        if let Some(check) = &check {
            let spoken = if check.is_review() { StageKind::Reviewer } else { StageKind::Verifier };
            stages.retain(|stage| stage.kind != spoken);
        }
        let piped = !stages.is_empty();
        let meta = self.node_meta(board, node, Some(card), check.as_ref(), piped);
        let place = self.where_chip(board, node, "project-card", cx);
        // A reason that says only that the worker had room tells the card nothing.
        let placed = board.place(node);
        let reason = placed
            .as_ref()
            .and_then(|p| p.why.clone())
            .filter(|why| why != slopty_proto::project::Suggestion::UNDECIDED);
        let place_words =
            self.where_words(board, node).map_or_else(String::new, |(_, short, _)| short);
        let pipeline = self.pipeline_row(&key, &stages);
        let block = check.as_ref().map(|c| self.check_block(&key, c, cx));
        // A parent standing in a lane for a descendant says whose it is.
        let why = (own != lane).then(|| format!("{} in a subtask", lane.title()));
        let arrive = ElementId::Name(format!("{key}-in").into());
        let selector = key.clone();
        let along: Vec<&str> = stages.iter().map(|stage| stage.words.as_str()).collect();
        let label = said(&[
            &card.title,
            state_word(card.state),
            &place_words,
            reason.as_deref().unwrap_or(""),
            &meta,
            &along.join(", "),
        ]);
        // Where it runs and why it went there, on a line of its own: what the fleet map is
        // made of, card by card.
        let where_line = place.map(|chip| {
            div()
                .flex()
                .items_center()
                .gap(self.z(sp.xs))
                .min_w_0()
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child(chip)
                .children(reason.map(|why| {
                    let id = format!("{key}-why");
                    div()
                        .debug_selector(move || id)
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(SharedString::from(why))
                }))
        });
        let el = div()
            .id(SharedString::from(key))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(label)
            .flex()
            .flex_col()
            .gap(self.z(sp.xxs))
            .p(self.z(sp.sm))
            .rounded(self.z(theme.radii.md))
            .bg(hsla(if picked { s.overlay } else { s.raised }))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.overlay)))
            .when(own == Lane::Merged, |el| el.opacity(alpha::STRONG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(sp.xs))
                    .min_w_0()
                    .child(self.mark(status))
                    .child(
                        crate::kit::tabular(div())
                            .flex_none()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(format!("#{}", card.id))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(card.title.clone())),
                    ),
            )
            .children((!meta.is_empty()).then(|| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(dotted(theme, meta))
            }))
            .children(where_line)
            .children(pipeline)
            .children(block)
            .children(self.run_on_block(card.id, "project-card", cx))
            .children(why.map(|why| {
                div()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(lane_tone(theme, lane)))
                    .child(SharedString::from(why))
            }))
            // Last, on a line of their own: a lane is too narrow for a title and its buttons.
            .children(
                self.actions(board, card.id, "project-card", cx)
                    .map(|actions| actions.pt(self.z(sp.xxs))),
            );
        let el = tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
            this.picked = Some(Pick::Node(node));
            cx.emit(ProjectEvent::Open(node));
            cx.notify();
        }));
        crate::kit::slide_fade(el, arrive, ARRIVE, crate::kit::Pace::Fade, cx)
    }

    /// Make the line to the orchestrator once there is a window, and put refused words back
    /// on it while it is empty.
    fn composer_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(COMPOSE_PLACEHOLDER));
            let sending = cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.send_composed(window, cx);
                }
            });
            let watching = cx.observe(&input, |_, _, cx| cx.notify());
            self.composer = Some((input, [sending, watching]));
        }
        if let Some(text) = self.refused.take()
            && let Some((input, _)) = &self.composer
        {
            input.update(cx, |input, cx| {
                if input.value().trim().is_empty() {
                    input.set_value(text, window, cx);
                }
            });
        }
    }

    /// The keyboard goes to the line to the orchestrator.
    fn compose(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((input, _)) = &self.composer {
            input.update(cx, |input, cx| input.focus(window, cx));
        }
    }

    /// Send what is on the line to the orchestrator, and clear it: the timeline shows it once
    /// the server has it.
    fn send_composed(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((input, _)) = &self.composer else { return };
        let text = input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        input.update(cx, |input, cx| input.set_value("", window, cx));
        cx.emit(ProjectEvent::Tell(text));
    }

    /// The server refused `text`: it goes back on the line, unless the person has started
    /// another.
    pub fn refused(&mut self, text: String, cx: &mut Context<Self>) {
        self.refused = Some(text);
        cx.notify();
    }

    /// What is on the line to the orchestrator now.
    #[must_use]
    pub fn composing(&self, cx: &gpui::App) -> Option<String> {
        self.composer.as_ref().map(|(input, _)| input.read(cx).value().to_string())
    }

    /// The line at the board's foot that talks to the orchestrator, while it has one.
    fn composer_row(&self, board: &Board, cx: &Context<Self>) -> Option<Stateful<Div>> {
        board.project.orchestrator?;
        let (input, _) = self.composer.as_ref()?;
        let sp = self.theme.spacing;
        let row = div()
            .id("project-composer")
            .debug_selector(|| "project-composer".to_owned())
            .flex_none()
            .px(self.z(sp.inset() - sp.xs))
            .pt(self.z(sp.xs))
            .pb(self.z(sp.sm))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .child(Input::new(input).aria_label(COMPOSE_LABEL));
        Some(row)
    }

    /// A task's way to its target on one row of quiet chips, a stage holding the merge back in
    /// a stronger ink: no colour, since none of it is an alarm until someone has to act.
    fn pipeline_row(&self, key: &str, stages: &[Stage]) -> Option<Div> {
        if stages.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let chips = stages.iter().map(|stage| {
            let id = format!("{key}-{}", stage.kind.word());
            let selector = id.clone();
            div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Label)
                .aria_label(SharedString::from(stage.words.clone()))
                .flex_none()
                .max_w_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .px(self.z(sp.xs))
                .rounded(self.z(theme.radii.sm))
                .border_1()
                .border_color(hsla(s.border_subtle))
                .text_size(self.z(theme.typography.meta()))
                .text_color(hsla(if stage.holds { s.text } else { s.text_muted }))
                .child(SharedString::from(stage.words.clone()))
        });
        Some(div().flex().flex_wrap().gap(self.z(sp.xxs)).pt(self.z(sp.xxs)).children(chips))
    }

    /// The timeline, newest first: when, which task, and what happened.
    fn timeline(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        if board.timeline.is_empty() {
            return vec![self.empty(
                "Nothing has happened yet",
                "Tasks starting, waiting on you, passing their verifier and merging show here.",
            )];
        }
        let now = self.seen.now;
        // A run of entries of one age says it once, on the newest, as a heading would.
        let mut above: Option<String> = None;
        board
            .timeline
            .iter()
            .rev()
            .map(|entry| {
                let seq = entry.seq;
                let task = entry.task.and_then(|t| board.tasks.get(&t));
                // The row names its task already; a title the task was made under says only
                // that it was made, unless it has been renamed since.
                let line = match (&entry.what, task) {
                    (Moment::TaskCreated { title }, Some(card)) if card.title == *title => {
                        CREATED.to_owned()
                    }
                    _ => super::model::moment_line(
                        entry,
                        |w| self.worker_name(w),
                        |t| board.agent_at(t),
                    ),
                };
                let (glyph, tone) = moment_icon(theme, &entry.what);
                let age = age_label(now.since(entry.at_ms));
                let shown = (above.as_ref() != Some(&age)).then(|| age.clone());
                above = Some(age.clone());
                let key = format!("project-entry-{seq}");
                let selector = key.clone();
                let picked = self.picked == Some(Pick::Entry(seq));
                let label = SharedString::from(match task {
                    Some(card) => format!("#{} {}: {line}, {age}", card.id, card.title),
                    None => format!("{line}, {age}"),
                });
                let row = div()
                    .id(numbered("project-entry", seq))
                    .debug_selector(move || selector)
                    .role(Role::ListItem)
                    .aria_label(label)
                    .relative()
                    .flex()
                    .items_center()
                    .gap(self.z(sp.sm))
                    .mx(self.z(sp.xs))
                    .px(self.z(sp.inset() - sp.xs))
                    .h(self.z(theme.density.row + sp.xs))
                    .rounded(self.z(theme.radii.sm))
                    .when(entry.task.is_some(), |el| {
                        el.cursor_pointer().hover(move |el| el.bg(hsla(s.raised)))
                    })
                    .child(
                        div()
                            .flex_none()
                            .size(self.z(theme.typography.icon_large()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icon(theme, glyph, IconSize::Inline, tone)
                                    .size(self.z(theme.typography.icon())),
                            ),
                    )
                    .children(task.map(|card| {
                        crate::kit::tabular(div())
                            .flex_none()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(format!("#{}", card.id)))
                    }))
                    .children(task.map(|card| {
                        div()
                            .min_w_0()
                            .max_w(relative(0.4))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text_secondary))
                            .child(SharedString::from(card.title.clone()))
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(line)),
                    )
                    .children(shown.map(|age| {
                        let selector = format!("project-entry-{seq}-age");
                        crate::kit::tabular(div())
                            .debug_selector(move || selector)
                            .flex_none()
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(age))
                    }));
                let arrive = ElementId::Name(format!("{key}-in").into());
                let row = if picked { self.plate.mark(row, key) } else { row };
                let node = entry.task;
                let row =
                    tab_stop(row, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.picked = Some(Pick::Entry(seq));
                        if node.is_some() {
                            cx.emit(ProjectEvent::Open(node));
                        }
                        cx.notify();
                    }));
                crate::kit::slide_fade(row, arrive, ARRIVE, crate::kit::Pace::Fade, cx)
            })
            .collect()
    }

    /// A lens with nothing in it: one line, and what will land there.
    /// The machines lens: each worker with how it is doing and the project's agents on it,
    /// each with why the server put it there, then the tasks still to start, each with a way
    /// to choose where it runs.
    fn machines(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let groups = self.machine_groups(board);
        let waiting = board.waiting_to_start();
        if groups.is_empty() && waiting.is_empty() {
            return vec![self.empty(NO_MACHINES, NO_MACHINES_HINT)];
        }
        let per_worker = board.project.limits.live_per_worker;
        let mut out = Vec::new();
        if !board.project.needs.is_empty() {
            out.push(self.heading("project-machines-needs", NEEDS, None).into_any_element());
            for (at, need) in board.project.needs.iter().enumerate() {
                out.push(self.need_row(at, need).into_any_element());
            }
        }
        for group in groups {
            let key = format!("project-host-{}", group.worker);
            let online = group.machine.as_ref().is_none_or(|m| m.online);
            let live = format!("{} of {per_worker} live", board.live_on(group.worker));
            let load = group.machine.as_ref().and_then(|m| m.load).map(|l| format!("load {l:.1}"));
            let kind = group.machine.as_ref().map(Machine::kind_line).filter(|k| !k.is_empty());
            let away = (!online).then_some(AWAY);
            let readout = |text: String| {
                crate::kit::tabular(div())
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(text))
            };
            let label = said(&[
                &group.name,
                away.unwrap_or(""),
                kind.as_deref().unwrap_or(""),
                load.as_deref().unwrap_or(""),
                &live,
            ]);
            let selector = key.clone();
            let head = div()
                .id(SharedString::from(key.clone()))
                .debug_selector(move || selector)
                .role(Role::Heading)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(self.z(sp.xs))
                .px(self.z(sp.inset()))
                .pt(self.z(sp.sm))
                .pb(self.z(sp.xxs))
                .when(!online, |el| el.opacity(alpha::STRONG))
                .child(
                    icon(theme, IconName::Server, IconSize::Inline, hsla(s.text_secondary))
                        .size(self.z(theme.typography.icon())),
                )
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .text_color(hsla(s.text))
                        .child(SharedString::from(group.name.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(
                            [away.map(str::to_owned), kind]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join(", "),
                        )),
                )
                .children(load.map(readout))
                .child(readout(live));
            out.push(head.into_any_element());
            for node in group.nodes {
                let said = node
                    .and_then(|t| board.tasks.get(&t))
                    .and_then(|c| c.assignment.as_ref()?.placed.as_ref())
                    .map(|p| p.why.clone());
                let line = Line { asking: None, said, depth: 1, prefix: "project-machine" };
                out.push(self.node_row(board, node, line, cx));
            }
        }
        if !waiting.is_empty() {
            out.push(
                self.heading("project-machines-waiting", NOT_STARTED, None).into_any_element(),
            );
            for task in waiting {
                let said = board.tasks.get(&task).map(|c| match c.pin {
                    Some(worker) => format!("To run on {}", self.worker_name(worker)),
                    None => ANYWHERE_LINE.to_owned(),
                });
                let line = Line { asking: None, said, depth: 0, prefix: "project-machine" };
                out.push(self.node_row(board, Some(task), line, cx));
            }
        }
        out
    }

    /// One of the project's needs: its name, then the paths it covers and what it asks of a
    /// worker, cut to the line; the label says it all.
    fn need_row(&self, at: usize, need: &slopty_proto::project::Need) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let (covers, asks) = need_words(need);
        let key = format!("project-need-{at}");
        let selector = key.clone();
        div()
            .id(SharedString::from(key))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(said(&[&need.name, &covers, &asks]))
            .flex()
            .items_baseline()
            .gap(self.z(sp.xs))
            .px(self.z(sp.inset()))
            .py(self.z(sp.xxs))
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(need.name.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(dotted(theme, format!("{covers} \u{b7} {asks}"))),
            )
    }

    /// The "Run on" picker under `task`'s row or card, while it is open there: "Anywhere",
    /// then every worker as the server ranks them, each with what decides it, the one the
    /// task is pinned to marked.
    fn run_on_block(
        &self,
        task: TaskId,
        prefix: &str,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        let picker = self.seen.run_on.as_ref().filter(|p| p.task == task)?;
        let board = self.seen.board.as_ref()?;
        let pin = board.tasks.get(&task).and_then(|c| c.pin);
        let proposing = board.tasks.get(&task).is_some_and(|c| c.proposed.is_some());
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let key = format!("{prefix}-picker-{task}");
        let option =
            |id: String, name: String, why: String, on: bool, fits: bool, choice: RunOn| {
                let selector = id.clone();
                let el = div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector)
                    .role(Role::RadioButton)
                    .aria_label(said(&[&name, &why]))
                    .aria_toggled(if on {
                        gpui::accesskit::Toggled::True
                    } else {
                        gpui::accesskit::Toggled::False
                    })
                    .flex()
                    .items_baseline()
                    .gap(self.z(sp.xs))
                    .min_w_0()
                    .px(self.z(sp.xs))
                    .py(self.z(sp.xxs))
                    .rounded(self.z(theme.radii.sm))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.overlay)))
                    .child(div().flex_none().size(self.z(theme.typography.icon())).children(
                        on.then(|| {
                            icon(theme, IconName::Check, IconSize::Inline, hsla(s.text))
                                .size(self.z(theme.typography.icon()))
                        }),
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_color(hsla(if fits { s.text } else { s.text_muted }))
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(why)),
                    );
                tab_stop(el, s.accent).on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.stop_propagation();
                    cx.emit(ProjectEvent::Pin(task, choice));
                }))
            };
        let close_id = format!("{key}-close");
        let close =
            crate::kit::icon_button_at(theme, close_id, IconName::X, CLOSE_RUN_ON, self.zoom)
                .on_click(cx.listener(|_this, _ev, _w, cx| {
                    cx.stop_propagation();
                    cx.emit(ProjectEvent::CloseRunOn);
                }));
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .child(div().flex_1().text_color(hsla(s.text_secondary)).child(SharedString::from(
                if proposing { format!("Start #{task} on") } else { format!("Run #{task} on") },
            )))
            .child(close);
        let mut options = vec![option(
            format!("{key}-anywhere"),
            ANYWHERE.to_owned(),
            ANYWHERE_LINE.to_owned(),
            pin.is_none(),
            true,
            RunOn::Anywhere,
        )];
        let ranking = match &picker.ranked {
            None => Some(
                div()
                    .px(self.z(sp.xs))
                    .text_color(hsla(s.text_muted))
                    .child(RANKING)
                    .into_any_element(),
            ),
            Some(ranked) => {
                options.extend(ranked.iter().enumerate().map(|(i, r)| {
                    option(
                        format!("{key}-{i}"),
                        r.name.clone(),
                        r.why(),
                        pin == Some(r.worker),
                        r.fits,
                        RunOn::Worker(r.worker),
                    )
                }));
                None
            }
        };
        let selector = key.clone();
        Some(
            div()
                .id(SharedString::from(key))
                .debug_selector(move || selector)
                .role(Role::RadioGroup)
                .aria_label(SharedString::from(format!("Run #{task} on")))
                .flex()
                .flex_col()
                .gap(self.z(sp.xxs))
                .mt(self.z(sp.xs))
                .px(self.z(sp.sm))
                .py(self.z(sp.xs))
                .rounded(self.z(theme.radii.sm))
                .bg(hsla(s.panel))
                .text_size(self.z(theme.typography.meta()))
                .child(head)
                .children(options)
                .children(ranking),
        )
    }

    fn empty(&self, line: &'static str, hint: &'static str) -> AnyElement {
        let theme = &self.theme;
        div()
            .id("project-empty")
            .debug_selector(|| "project-empty".to_owned())
            .role(Role::Status)
            .aria_label(line)
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xxs))
            .px(self.z(theme.spacing.inset()))
            .py(self.z(theme.spacing.md))
            .child(div().text_color(hsla(theme.surfaces.text_secondary)).child(line))
            .child(
                div()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(hint),
            )
            .into_any_element()
    }
}

/// One worker in the machines lens, and the project's nodes running on it.
struct MachineGroup {
    worker: WorkerId,
    name: String,
    /// What the server last said of it.
    machine: Option<Machine>,
    nodes: Vec<Node>,
}

/// How a node's row is drawn: in the tree at its depth, or in what needs the person with what
/// its agent asks.
struct Line {
    /// What the agent asks, for a row of what needs the person.
    asking: Option<String>,
    /// A second line of its own, over the node's facts: why it runs where it does.
    said: Option<String>,
    /// How far in the tree it steps.
    depth: usize,
    /// Its element name's start.
    prefix: &'static str,
}

/// `n` as a float, for a step of the tree's indent.
#[expect(clippy::cast_precision_loss, reason = "a tree's depth is small")]
const fn depth_f(n: usize) -> f32 {
    n as f32
}

/// What runs inside a node, in words: its subagents and its to-dos.
fn natives_line(counts: NativeCounts) -> Option<String> {
    let agents = match (counts.agents, counts.running) {
        (0, _) => None,
        (n, 0) => Some(if n == 1 { "1 subagent".to_owned() } else { format!("{n} subagents") }),
        (n, running) => Some(format!("{running} of {n} subagents running")),
    };
    let todos =
        (counts.todos > 0).then(|| format!("{} of {} to-dos done", counts.done, counts.todos));
    match (agents, todos) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(t)) => Some(t),
        (Some(a), Some(t)) => Some(format!("{a}, {t}")),
    }
}

/// A timeline entry's icon and tone.
/// Time at work is shown from a minute: less than that on every row of a board just started
/// would be noise.
const SHOWN_FROM_MS: u64 = 60_000;

/// What a node spent, as its row says it to assistive technology: "worked 40m, 12m itself,
/// $1.20, context 82%".
fn spent_words(spend: &NodeSpend) -> String {
    let mut parts: Vec<String> = Vec::new();
    if spend.has_subtree() && spend.subtree_ms >= SHOWN_FROM_MS {
        parts.push(format!("worked {}", worked(spend.subtree_ms)));
        parts.push(format!("{} itself", worked(spend.own_ms)));
    } else if spend.own_ms >= SHOWN_FROM_MS {
        parts.push(format!("worked {}", worked(spend.own_ms)));
    }
    let cost = if spend.has_subtree() { spend.subtree_cost } else { spend.own_cost };
    parts.extend(cost.map(dollars));
    if let Some((bp, _)) = spend.context_shown() {
        parts.push(format!("context {}%", bp / 100));
    }
    parts.join(", ")
}

/// The word a recap line's selector ends in.
const fn recap_word(kind: RecapKind) -> &'static str {
    match kind {
        RecapKind::ChangesAsked => "changes",
        RecapKind::VerifyFailed => "verify-failed",
        RecapKind::Conflicts => "conflicts",
        RecapKind::ChecksFailed => "checks-failed",
        RecapKind::StepFailed => "step-failed",
        RecapKind::Stuck => "stuck",
        RecapKind::AgentEnded => "ended",
        RecapKind::Proposed => "proposed",
        RecapKind::Merged => "merged",
        RecapKind::Verified => "verified",
        RecapKind::Started => "started",
        RecapKind::Created => "created",
    }
}

/// A recap line's mark: the one its timeline entries draw.
const fn recap_icon(kind: RecapKind) -> IconName {
    match kind {
        RecapKind::ChangesAsked => IconName::MessageSquareWarning,
        RecapKind::VerifyFailed | RecapKind::StepFailed => IconName::CircleX,
        RecapKind::Conflicts => IconName::GitBranch,
        RecapKind::ChecksFailed => IconName::GitPullRequest,
        RecapKind::Stuck => IconName::CircleAlert,
        RecapKind::AgentEnded => IconName::Power,
        RecapKind::Proposed => IconName::Hand,
        RecapKind::Merged => IconName::GitMerge,
        RecapKind::Verified => IconName::CircleCheck,
        RecapKind::Started => IconName::SquareTerminal,
        RecapKind::Created => IconName::Plus,
    }
}

fn moment_icon(theme: &Theme, what: &Moment) -> (IconName, Hsla) {
    let s = &theme.surfaces;
    let (glyph, tone) = match what {
        Moment::Created | Moment::TaskCreated { .. } => (IconName::Plus, s.text_muted),
        Moment::Orchestrator { .. } => (IconName::Bot, s.text_secondary),
        Moment::Limits { .. } | Moment::Needs { .. } => (IconName::ListFilter, s.text_muted),
        Moment::Claimed { .. } => (IconName::Lock, s.text_muted),
        Moment::Assigned { .. } => (IconName::SquareTerminal, s.text_secondary),
        Moment::Proposed { .. } => (IconName::Hand, s.text_secondary),
        Moment::State { to, .. } => {
            let status = state_status(*to);
            (status.icon(), board_tone(theme, status))
        }
        Moment::Branch { pr: Some(_), .. } => (IconName::GitPullRequest, s.text_secondary),
        Moment::Branch { .. } => (IconName::GitBranch, s.text_secondary),
        Moment::Verified(run) if run.passed => (IconName::CircleCheck, s.text_secondary),
        Moment::Verified(_) => (IconName::CircleX, s.error),
        Moment::Checks(checks) => match checks.state {
            slopty_proto::project::ChecksState::Failing => (IconName::CircleX, s.error),
            slopty_proto::project::ChecksState::Passing => {
                (IconName::CircleCheck, s.text_secondary)
            }
            _ => (IconName::GitPullRequest, s.text_secondary),
        },
        Moment::Reviewed(run) if run.verdict.approved => (IconName::CircleCheck, s.text_secondary),
        Moment::Reviewed(_) => (IconName::MessageSquareWarning, s.text_secondary),
        Moment::AgentGone { .. } => (IconName::Power, s.text_muted),
        Moment::Note { .. } | Moment::Told { .. } => (IconName::MessageSquare, s.text_secondary),
        Moment::Reported { report } => match report.kind {
            ReportKind::Checkpoint => (IconName::Flag, s.text_secondary),
            ReportKind::NeedsInput => (IconName::MessageSquareWarning, s.warn),
            ReportKind::Stuck => (IconName::CircleAlert, s.error),
            ReportKind::Done => (IconName::CircleCheck, s.text_secondary),
        },
        Moment::Delivered { .. } => (IconName::Inbox, s.text_muted),
        Moment::Step(step) => match (step.kind, &step.state) {
            (_, StepState::Failed { .. }) => (IconName::CircleX, s.error),
            (StepKind::Clone, _) => (IconName::FolderGit2, s.text_secondary),
            (StepKind::Home, StepState::Done { .. }) | (StepKind::Rebase, _) => {
                (IconName::GitBranch, s.text_secondary)
            }
            (StepKind::Home, _) => (IconName::Download, s.text_secondary),
            (StepKind::Verify, _) => (IconName::ListChecks, s.text_secondary),
            (StepKind::Merge, _) => (IconName::GitMerge, s.text_secondary),
            (StepKind::Review, _) => (IconName::Eye, s.text_secondary),
        },
    };
    (glyph, hsla(tone))
}

impl Render for ProjectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        self.keep_time(cx);
        self.composer_in(window, cx);
        let theme = &self.theme;
        // The board's bare keys hold only while the board has the keyboard: the line to the
        // orchestrator is beside them, so a letter typed there is a letter.
        let keys = div()
            .id("project-keys")
            .key_context(CTX)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _w, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _w, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &OpenNode, _w, cx| this.open_picked(cx)))
            .on_action(cx.listener(|this, _: &ShowTree, _w, cx| this.show(Lens::Tree, cx)))
            .on_action(cx.listener(|this, _: &ShowBoard, _w, cx| this.show(Lens::Board, cx)))
            .on_action(cx.listener(|this, _: &ShowTimeline, _w, cx| this.show(Lens::Timeline, cx)))
            .on_action(cx.listener(|this, _: &ShowMachines, _w, cx| this.show(Lens::Machines, cx)))
            .on_action(cx.listener(|this, _: &RunTaskOn, _w, cx| {
                this.act_on_picked(TaskAction::RunOn, cx);
            }))
            .on_action(cx.listener(|this, _: &StartTask, _w, cx| {
                this.act_on_picked(TaskAction::Start, cx);
            }))
            .on_action(cx.listener(|this, _: &StartProposed, _w, cx| this.start_all(cx)))
            .on_action(cx.listener(|this, _: &ToggleAskToStart, _w, cx| this.toggle_ask(cx)))
            .on_action(cx.listener(|this, _: &MergeTask, _w, cx| {
                this.act_on_picked(TaskAction::Merge, cx);
            }))
            .on_action(cx.listener(|this, _: &RetryTask, _w, cx| {
                this.act_on_picked(TaskAction::Retry, cx);
            }))
            .on_action(cx.listener(|this, _: &ApproveTask, _w, cx| {
                this.act_on_picked(TaskAction::Approve, cx);
            }))
            .on_action(cx.listener(|this, _: &FixCi, _w, cx| {
                this.act_on_picked(TaskAction::FixCi, cx);
            }))
            .on_action(cx.listener(|this, _: &AddressComments, _w, cx| {
                this.act_on_picked(TaskAction::AddressComments, cx);
            }))
            .on_action(cx.listener(|this, _: &ResolveConflicts, _w, cx| {
                this.act_on_picked(TaskAction::ResolveConflicts, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePush, _w, cx| this.toggle_push(cx)))
            .on_action(cx.listener(|_this, _: &ShowTerminal, _w, cx| {
                cx.emit(ProjectEvent::Open(None));
            }))
            .on_action(cx.listener(|this, _: &DeleteProject, _w, cx| this.delete(cx)))
            .on_action(cx.listener(|this, _: &TellOrchestrator, window, cx| {
                this.compose(window, cx);
            }))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden();
        let root = div()
            .id("project")
            .debug_selector(|| "project".to_owned())
            .role(Role::Group)
            .aria_label(SharedString::from(format!("Project {}", self.id)))
            .aria_value(SharedString::from(self.summary()))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.ui_size))
            .text_color(hsla(theme.surfaces.text));
        let Some(board) = self.seen.board.clone() else {
            return root.child(
                keys.child(self.empty(PROJECT_GONE, "Its tasks and agents are as they were left.")),
            );
        };
        let body = match self.lens {
            Lens::Tree => self.tree(&board, cx),
            Lens::Board => self.board(&board, cx),
            Lens::Timeline => self.timeline(&board, cx),
            Lens::Machines => self.machines(&board, cx),
        };
        let composer = self.composer_row(&board, cx);
        let keys = keys
            .child(self.header(&board, cx))
            .children(self.recap(&board, cx))
            .children(self.needs_you(&board, cx))
            .children(self.plan(&board, cx))
            .child(self.lenses(cx))
            .child(
                div()
                    .id("project-body")
                    .debug_selector(|| "project-body".to_owned())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.plate.under(theme))
                    .child(
                        div()
                            .id("project-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .pt(self.z(theme.spacing.xs))
                            .pb(self.z(theme.spacing.md))
                            .children(body),
                    ),
            );
        root.child(keys).children(composer)
    }
}
