//! The board: one project drawn in its orchestrator's tile.
//!
//! A header names the project and where its work lands, over a bar of how much of it has
//! merged. Under it, the orchestrator while it waits on the person, then the lanes in their
//! order, each task in the one its state puts it in, what needs the person first; at its foot,
//! the message to the orchestrator, in the frame a thread's composer has. Every card whose
//! agent runs somewhere opens that agent's tile with a click or ↩.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, px, relative,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    NativeCounts, ProjectId, RunOn, StepKind, StepState, TaskCard, TaskId, TaskState, TaskStep,
    VerifierRun,
};
use slopty_theme::{Theme, Typography, alpha};

use super::model::{
    Board, Lane, Place, PlaceHow, RunOnPicker, Stage, StageKind, TaskAction, os_name, pull_words,
    queue_words, run_on_words, short_commit, state_word, verdict_detail, verdict_tail,
};
use super::recap::{Recap, RecapKind};
use super::{
    AddressComments, CancelTask, DeleteProject, EditChecks, FixCi, MergeTask, OpenNode, PushTask,
    ResolveConflicts, RetryTask, RunTaskOn, SelectNext, SelectPrevious, ShowTerminal,
    StopTaskAgent, TellOrchestrator, TogglePush,
};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconSize, Phase, Status, Symbol, icon, status_icon};
use crate::kit::progress::Progress;
use crate::palette::{Plate, age_label, dotted};

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
/// What the orchestrator's line under it says when its agent names no question.
const WAITING_ON_YOU: &str = "Waiting on you";
/// What the orchestrator's row is called.
pub(crate) const ORCHESTRATOR: &str = "Orchestrator";
/// The "Run on" picker's first choice.
pub(crate) const ANYWHERE: &str = "Anywhere";
/// What "Anywhere" means.
pub(crate) const ANYWHERE_LINE: &str = "A worker with room when it starts";
/// The "Run on" picker while the server reads the workers.
pub(crate) const RANKING: &str = "Reading the workers\u{2026}";
/// The "Run on" picker's close button.
pub(crate) const CLOSE_RUN_ON: &str = "Close the worker choice";

/// The least a lane is wide at zoom 1: the tile takes as many across as fit. Wide enough that
/// a card's title reads on two lines beside its mark rather than as an ellipsis.
const LANE_W: f32 = 280.0;
/// How far a card that arrives travels up into its place, at zoom 1.
const ARRIVE: f32 = 4.0;
/// The most facts a row's or a card's second line holds: two separators.
const META_PARTS: usize = 3;

pub use super::model::Node;

/// What a board tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectEvent {
    /// Open this node's agent in its tile.
    Open(Node),
    /// Show a terminal the server runs for a task, its verifier's, in its tile.
    Output(TermRef),
    /// Do this to a task ([`Board::actions`]).
    Act(TaskId, TaskAction),
    /// Push the target after each merge, or stop.
    SetPush(bool),
    /// Check each task's work by this command; an empty one is none.
    SetChecks {
        /// The verifier command.
        verifier: String,
    },
    /// Let the project go; the person pressed for it twice.
    Delete,
    /// Say this: what was asked of the board cannot be done now.
    Say(String),
    /// Run the task there, or wherever its placement chooses: the "Run on" picker's choice.
    Pin(TaskId, RunOn),
    /// Close the "Run on" picker.
    CloseRunOn,
    /// Tell the orchestrator this, as the person.
    Tell(String),
    /// The person read the recap: close it.
    CloseRecap,
}

/// What the message to the orchestrator says while it is empty.
const COMPOSE_PLACEHOLDER: &str = "Message the orchestrator\u{2026}";
/// What it is called to a screen reader.
const COMPOSE_LABEL: &str = "Message the orchestrator";
/// What its send control is called.
const SEND: &str = "Send";
/// The most lines the message grows to before it scrolls.
const COMPOSE_ROWS: usize = 6;

/// How long a first "Delete the project" waits for the second that does it.
const DELETE_CONFIRM: std::time::Duration = std::time::Duration::from_secs(5);

/// What a task's check shows on its card and its row: its verifier at work, or the last word
/// that still speaks.
#[derive(Clone, Debug, PartialEq)]
enum Check<'a> {
    /// The verifier at work, with its last line.
    Running { line: String, term: Option<TermRef> },
    /// What the verifier said, and the terminal a failed run is kept in.
    Verdict { run: &'a VerifierRun, term: Option<TermRef> },
}

impl Check<'_> {
    const fn term(&self) -> Option<TermRef> {
        match self {
            Self::Running { term, .. } | Self::Verdict { term, .. } => *term,
        }
    }

    /// Whether this block says what `step` would: the check it is, at work or with its word.
    fn speaks_for(&self, step: &TaskStep) -> bool {
        match self {
            Self::Running { .. } => true,
            Self::Verdict { .. } => step.kind == StepKind::Verify,
        }
    }
}

/// What a card says besides its title ([`ProjectView::card_facts`]).
struct CardFacts<'a> {
    /// Its check's block.
    check: Option<Check<'a>>,
    /// The stages of its way to the target that its check's block does not say.
    stages: Vec<Stage>,
    /// Its second line.
    meta: String,
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
    /// is pinned to, or takes a step on.
    pub workers: BTreeMap<WorkerId, WorkerSeen>,
    /// The agents this client sees, by session.
    pub agents: HashMap<SessionId, AgentSeen>,
    /// The server's clock now, near enough, for the recap's age and the time at work.
    pub now: WallMs,
    /// The "Run on" picker, while it is open on one of the project's tasks.
    pub run_on: Option<RunOnPicker>,
    /// What changed since this client last looked, from the moment the board opened until the
    /// person closes it or the board hides.
    pub recap: Option<Recap>,
}

/// How a project's work is checked, as the person sets it on the board: the verifier command.
struct Checks {
    verifier: Entity<InputState>,
    _enter: Subscription,
}

/// One project's board.
pub struct ProjectView {
    id: ProjectId,
    seen: Seen,
    /// The card the keyboard stands on.
    picked: Option<Node>,
    zoom: f32,
    width: f32,
    theme: Theme,
    /// The theme a hover hint draws by, shared by every hint the board makes.
    hint_theme: Rc<Theme>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    plate: Plate,
    /// When "Delete the project" was asked once, waiting for the second ask that does it.
    delete_asked: Option<std::time::Instant>,
    /// The message to the orchestrator, made with the first frame (it needs the window), and
    /// what watches it.
    composer: Option<(Entity<TextareaState>, [Subscription; 2])>,
    /// Words the server refused, to put back on the line once it is empty.
    refused: Option<String>,
    /// The project's checks being set, while that panel is open.
    checks: Option<Checks>,
    /// How many times it was drawn: the proof that an unchanged hand-over draws nothing.
    #[cfg(test)]
    renders: usize,
}

impl std::fmt::Debug for ProjectView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectView")
            .field("id", &self.id)
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
            picked: None,
            zoom: 1.0,
            width: 0.0,
            hint_theme: Rc::new(theme.clone()),
            theme,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            plate: Plate::default(),
            delete_asked: None,
            composer: None,
            refused: None,
            checks: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// Its project.
    #[must_use]
    pub const fn project(&self) -> &ProjectId {
        &self.id
    }

    /// The node the keyboard stands on, when it stands on one.
    #[must_use]
    pub const fn picked(&self) -> Option<Node> {
        self.picked
    }

    /// What it shows, as the workspace last handed it.
    #[must_use]
    pub const fn seen(&self) -> &Seen {
        &self.seen
    }

    /// Show the project as `seen` has it; drawn again only when what it shows changed. The
    /// clock alone draws nothing: the board keeps its own time.
    pub fn set_seen(&mut self, seen: Seen, cx: &mut Context<Self>) {
        let same_board = match (&self.seen.board, &seen.board) {
            (Some(was), Some(is)) => Arc::ptr_eq(was, is) || was == is,
            (was, is) => was.is_none() && is.is_none(),
        };
        let same = same_board
            && self.seen.workers == seen.workers
            && self.seen.agents == seen.agents
            && self.seen.run_on == seen.run_on
            && self.seen.recap == seen.recap;
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

    /// Give the board the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        // The checks panel keeps the keyboard it has: a click that opened it focused its tile.
        let typing = self
            .checks
            .as_ref()
            .is_some_and(|c| c.verifier.read(cx).focus_handle(cx).is_focused(window));
        if !typing {
            window.focus(&self.focus, cx);
        }
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
            cx.notify();
        }
    }

    /// Open the agent of the card the keyboard stands on.
    pub fn open_picked(&self, cx: &mut Context<Self>) {
        if let Some(node) = self.picked {
            cx.emit(ProjectEvent::Open(node));
        }
    }

    /// Do `action` to the task the keyboard stands on, or say why not.
    pub fn act_on_picked(&self, action: TaskAction, cx: &mut Context<Self>) {
        let Some(board) = &self.seen.board else { return };
        let Some(Some(task)) = self.picked() else {
            cx.emit(ProjectEvent::Say(format!("Stand on a task to {}", verb_of(action))));
            return;
        };
        if board.actions(task).contains(&action) || board.controls(task).contains(&action) {
            cx.emit(ProjectEvent::Act(task, action));
        } else {
            cx.emit(ProjectEvent::Say(format!("#{task} has nothing to {}", verb_of(action))));
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

    /// Every card the keyboard can stand on, in the order the lanes draw them.
    fn picks(&self) -> Vec<Node> {
        let Some(board) = &self.seen.board else { return Vec::new() };
        board.lanes().into_iter().flat_map(|(_, tasks)| tasks).map(Some).collect()
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
        }];
        lines.extend(place.worktree.as_ref().map(|w| format!("Worktree {w}")));
        lines.extend(place.branch.as_ref().map(|b| format!("Branch {b}")));
        lines.extend(place.why.as_ref().map(|why| format!("Why: {why}")));
        if node.is_some_and(|t| board.movable(t)) {
            lines.push(MOVE_HINT.to_owned());
        }
        Some((place, short, lines.join("\n")))
    }

    /// `node`'s place as a quiet chip: the worker and its system, in a stronger ink while its
    /// agent runs there. Its hint says the rest; a click moves a task not started yet ("Run
    /// on…"), and a chip that cannot move is only words.
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
            PlaceHow::Runs => (Symbol::ServerRack, s.text_secondary),
            PlaceHow::Ran => (Symbol::ServerRack, s.text_muted),
            PlaceHow::Pinned => (Symbol::Lock, s.text_muted),
        };
        let movable = node.filter(|t| board.movable(*t));
        let id = format!("{prefix}-{}-where", node_key(node));
        let selector = id.clone();
        let hint_theme = Rc::clone(&self.hint_theme);
        let label = hint.replace('\n', ". ");
        let el = div()
            .id(SharedString::from(id))
            .debug_selector(move || selector)
            .role(if movable.is_some() { Role::Button } else { Role::Label })
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
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(tone))
            .child(
                icon(theme, glyph, IconSize::Inline, hsla(tone))
                    .size(self.z(theme.typography.small())),
            )
            .child(
                div().min_w_0().overflow_hidden().text_ellipsis().child(SharedString::from(short)),
            )
            .map(crate::kit::hint_timing)
            .tooltip(move |_window, cx| {
                let theme = Rc::clone(&hint_theme);
                cx.new(|_| crate::kit::Hint::new(hint.clone(), "", theme)).into()
            });
        let Some(task) = movable else { return Some(el) };
        let el =
            el.cursor_pointer().hover(move |el| el.bg(hsla(s.selected)).text_color(hsla(s.text)));
        Some(tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _w, cx| {
            cx.stop_propagation();
            this.picked = Some(node);
            cx.emit(ProjectEvent::Act(task, TaskAction::RunOn));
            cx.notify();
        })))
    }

    /// The agent running `node` as this client sees it, when it sees it.
    fn agent(&self, board: &Board, node: Node) -> Option<&AgentSeen> {
        let (_, session) = board.terminal(node)?;
        self.seen.agents.get(&session)
    }

    /// Where a view of it is summed up in words: its lanes and their tasks, for the
    /// self-test's dump and a screen reader.
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
/// fit at [`LANE_W`], one at the least. They share the width equally and keep their order,
/// left to right and then down.
pub(super) fn lanes_across(width: f32, zoom: f32) -> u16 {
    let lanes = u16::try_from(Lane::ALL.len()).unwrap_or(u16::MAX);
    (2..=lanes).rev().find(|&n| width >= f32::from(n) * LANE_W * zoom).unwrap_or(1)
}

/// The push toggle's one name, said as pressed or not.
pub(crate) const PUSH: &str = "Push after each merge";
/// The recap's heading when the person looked a moment ago.
pub(crate) const RECAP: &str = "Since you last looked";
/// The recap's button that closes it.
pub(crate) const CLOSE_RECAP: &str = "Close the recap";
/// The recap's last line when it could not read back as far as the person's last look.
pub(crate) const RECAP_PARTIAL: &str = "And earlier changes the recap could not read";
/// What a click on a task's place does while it can still move.
pub(crate) const MOVE_HINT: &str = "Click to choose where it runs";
/// The header's way back to the orchestrator's terminal.
pub(crate) const SHOW_TERMINAL: &str = "Show the orchestrator's terminal";
/// The header's toggle for the panel that sets how the work is checked, and the panel's name.
pub(crate) const CHECKS: &str = "Verifier";
/// The verifier field's label.
pub(crate) const VERIFIER: &str = "Verifier";
/// What the verifier field says while empty.
pub(crate) const VERIFIER_HINT: &str = "A command that passes when a task's work is right";
/// Where a project's work lands and how it is checked, as one sentence: "slopty → main,
/// verified by cargo gate".
fn place_line(project: &slopty_proto::project::Project) -> String {
    let place = format!("{} \u{2192} {}", project.repo, project.target);
    match &project.verifier {
        Some(verifier) => format!("{place}, verified by {verifier}"),
        None => place,
    }
}

/// A switch at `zoom`: a track the solid fills while on, its knob at the far end.
pub(super) fn switch(
    theme: &Theme,
    zoom: f32,
    id: &'static str,
    label: &'static str,
    on: bool,
) -> Stateful<Div> {
    let z = |v: f32| px(v * zoom);
    let (s, sp) = (theme.surfaces, theme.spacing);
    let knob = 2.0_f32.mul_add(-sp.xxs, sp.lg);
    let width = sp.xl + sp.xs;
    let travel = 2.0_f32.mul_add(-sp.xxs, width - knob);
    let (track, ink) =
        if on { (hsla(s.solid), s.solid_ink) } else { (hsla(s.selected), s.text_secondary) };
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Switch)
        .aria_label(label)
        .aria_toggled(if on {
            gpui::accesskit::Toggled::True
        } else {
            gpui::accesskit::Toggled::False
        })
        .flex_none()
        .w(z(width))
        .h(z(sp.lg))
        .flex()
        .items_center()
        .px(z(sp.xxs))
        .rounded_full()
        .bg(track)
        .cursor_pointer()
        .child(
            div()
                .flex_none()
                .size(z(knob))
                .ml(z(if on { travel } else { 0.0 }))
                .rounded_full()
                .bg(hsla(ink)),
        );
    tab_stop(el, s.focus)
}

/// What an action does, as "nothing to …" and "stand on a task to …" say it.
const fn verb_of(action: TaskAction) -> &'static str {
    match action {
        TaskAction::Merge => "merge",
        TaskAction::Retry => "retry",
        TaskAction::RunOn => "choose where it runs",
        TaskAction::FixCi => "fix",
        TaskAction::AddressComments => "address",
        TaskAction::ResolveConflicts => "resolve",
        TaskAction::PushAgain => "push again",
        TaskAction::Cancel => "cancel",
        TaskAction::Stop => "stop",
    }
}

/// What a row says to a screen reader: its parts that say anything, comma by comma.
fn said(parts: &[&str]) -> SharedString {
    let parts: Vec<&str> = parts.iter().copied().filter(|p| !p.is_empty()).collect();
    SharedString::from(parts.join(", "))
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
    /// many of its agents run, and its controls: how the work is checked, pushing, and the way
    /// back to the orchestrator's terminal. The readouts are columns,
    /// so the line under the title is a sentence and not a string of facts.
    fn header(&self, board: &Board, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let project = &board.project;
        let (merged, total) = board.progress();
        let progress = (total > 0).then(|| format!("{merged} of {total} merged"));
        // What is live: the agents running for it, its orchestrator's among them.
        let live = match board.live() {
            1 => "1 agent running".to_owned(),
            n => format!("{n} agents running"),
        };
        let place = place_line(project);
        let readout = |id: &'static str, text: String| {
            crate::kit::tabular(div())
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex_none()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(SharedString::from(text))
        };
        let push = project.push;
        let push_toggle = crate::kit::icon_toggle(
            theme,
            "project-push",
            Symbol::ArrowUpToLine,
            PUSH,
            push,
            self.zoom,
        )
        .on_click(cx.listener(move |_this, _ev, _w, cx| {
            cx.emit(ProjectEvent::SetPush(!push));
        }));
        let terminal = crate::kit::icon_button_at(
            theme,
            "project-terminal",
            Symbol::Terminal,
            SHOW_TERMINAL,
            self.zoom,
        )
        .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::Open(None))));
        let checks_open = self.checks.is_some();
        let checks_toggle = crate::kit::icon_toggle(
            theme,
            "project-checks",
            Symbol::Checklist,
            CHECKS,
            checks_open,
            self.zoom,
        )
        .on_click(cx.listener(|this, _ev, window, cx| {
            if this.checks.is_some() {
                this.close_checks(window, cx);
            } else {
                this.open_checks(window, cx);
            }
        }));
        let title = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .min_w_0()
            .child(
                icon(theme, Symbol::RectangleSplit3x1, IconSize::Inline, hsla(s.text_secondary))
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
            .child(
                self.facts(
                    std::iter::once(readout("project-live", live))
                        .chain(progress.map(|p| readout("project-progress", p))),
                ),
            )
            .child(checks_toggle)
            .child(push_toggle)
            .child(terminal);
        let meta = div()
            .id("project-place")
            .debug_selector(|| "project-place".to_owned())
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(self.z(theme.typography.small()))
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

    /// Readouts side by side, parted by the quiet middle dot, so two counts never read as one
    /// run of words.
    fn facts(&self, readouts: impl Iterator<Item = Stateful<Div>>) -> Div {
        let theme = &self.theme;
        let mut row = div().flex_none().flex().items_center().gap(self.z(theme.spacing.xs));
        for (ix, readout) in readouts.enumerate() {
            if ix > 0 {
                row = row.child(
                    crate::kit::separator(theme).text_size(self.z(theme.typography.small())),
                );
            }
            row = row.child(readout);
        }
        row
    }

    /// A task's actions, as buttons on its row or card: what needs the person to move on, and
    /// on the task the board stands on its controls after them, quieter ([`Board::controls`]).
    /// On a task `held` up (a stage of its way holds, or its verifier failed) the first is the
    /// solid, the one move that frees it; the rest stay secondary, so a lane of cards ready to
    /// merge is not a column of solids.
    fn actions(
        &self,
        board: &Board,
        task: TaskId,
        prefix: &str,
        held: bool,
        cx: &Context<Self>,
    ) -> Option<Div> {
        // Choosing a worker is a control of the card stood on, so a lane of planned tasks is
        // not a column of the same button.
        let offered = board.actions(task);
        let mut actions: Vec<(TaskAction, bool)> = offered
            .iter()
            .copied()
            .filter(|a| *a != TaskAction::RunOn)
            .map(|a| (a, false))
            .collect();
        if self.picked() == Some(Some(task)) {
            if offered.contains(&TaskAction::RunOn) {
                actions.push((TaskAction::RunOn, true));
            }
            actions.extend(board.controls(task).into_iter().map(|a| (a, true)));
        }
        if actions.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let buttons = actions.into_iter().enumerate().map(|(ix, (action, control))| {
            let id = action.selector(prefix, task);
            let selector = id.clone();
            let el = div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{} #{task}", action.label())))
                // A secondary button at the small size: on a card's fill a ghost's words
                // read as plain text ("Merge" looked like a label), so it carries the
                // hairline that makes it a button there, at the least height a click needs.
                .flex_none()
                .flex()
                .items_center()
                .h(self.z(theme.density.hit))
                .px(self.z(sp.sm))
                .rounded(self.z(theme.radii.sm))
                .text_size(self.z(theme.typography.small()))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .child(action.label());
            let el = if control {
                el.text_color(hsla(s.text_secondary))
                    .hover(move |el| el.bg(hsla(s.selected)).text_color(hsla(s.text)))
            } else if held && ix == 0 {
                crate::kit::solid_pressable(el, theme)
            } else {
                crate::kit::secondary(el, theme)
            };
            tab_stop(el, s.focus).on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.stop_propagation();
                cx.emit(ProjectEvent::Act(task, action));
            }))
        });
        Some(div().flex_none().flex().items_center().gap(self.z(sp.xxs)).children(buttons))
    }

    /// How much of the work has merged: the merged tasks' share of them all, in the success
    /// tone on the quiet track, an empty track before any has. Only what is done fills it, so a
    /// full bar means the work is in; how the rest stands is the lanes' counts.
    fn bar(&self, board: &Board) -> Stateful<Div> {
        let theme = &self.theme;
        let (merged, total) = board.progress();
        #[expect(clippy::cast_precision_loss, reason = "task counts are small")]
        let share = if total == 0 { 0.0 } else { merged as f32 / total as f32 };
        let bar =
            crate::kit::progress::Bar::new(theme, "project-bar-share", Progress::Share(share))
                .label(SharedString::from(format!("{merged} of {total} merged")))
                .tone(theme.surfaces.success_fill)
                .height(self.z(theme.spacing.xs))
                .at_once();
        div()
            .id("project-bar")
            .debug_selector(|| "project-bar".to_owned())
            .mt(self.z(theme.spacing.xs))
            .child(bar)
    }

    /// The orchestrator while it waits on the person, over the lanes: never below a fold. Each
    /// task has its card in the *Needs you* lane; the orchestrator has none. Inset to the
    /// lanes' edges, so the band stands in the column as a card does.
    fn needs_you(&self, board: &Board, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let agent = self.agent(board, None).filter(|a| a.status == Status::NeedsYou)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let asks = agent.asks.clone().unwrap_or_else(|| WAITING_ON_YOU.to_owned());
        let key = "project-needs-orchestrator";
        let row = div()
            .id(key)
            .debug_selector(move || key.to_owned())
            .role(Role::Button)
            .aria_label(said(&[ORCHESTRATOR, &asks]))
            .flex()
            .items_start()
            .gap(self.z(sp.sm))
            .px(self.z(sp.sm))
            .py(self.z(sp.xs))
            .rounded(self.z(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.selected)))
            .child(
                div()
                    .flex_none()
                    .h(self.z(theme.density.row * 0.9))
                    .flex()
                    .items_center()
                    .child(self.mark(Some(Phase::NeedsYou))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(self.z(theme.density.row * 0.9))
                            .flex()
                            .items_center()
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .child(ORCHESTRATOR),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(self.z(theme.typography.small()))
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(asks)),
                    ),
            );
        let row = tab_stop(row, s.focus).on_click(cx.listener(|_this, _ev, _w, cx| {
            cx.emit(ProjectEvent::Open(None));
        }));
        Some(
            div()
                .id("project-needs-you")
                .debug_selector(|| "project-needs-you".to_owned())
                .role(Role::Group)
                .aria_label(NEEDS_YOU)
                .flex_none()
                .mx(self.z(sp.inset() - sp.xs))
                .mb(self.z(sp.xs))
                .p(self.z(sp.xxs))
                .rounded(self.z(theme.radii.md))
                .map(|el| crate::kit::raised(el, theme))
                .child(self.heading("project-needs-heading", NEEDS_YOU).px(self.z(sp.sm)))
                .child(row),
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
            Symbol::Xmark,
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
                .map(|el| crate::kit::raised(el, theme))
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
            None => (Symbol::Clock, s.text_muted),
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

    /// A quiet label over a group of rows, on the column their marks stand in.
    fn heading(&self, id: &'static str, text: &'static str) -> Stateful<Div> {
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
            .text_color(hsla(theme.surfaces.text_muted))
            .child(text)
    }

    /// A phase's glyph in its fixed slot at the board's zoom, named by the phase; with none,
    /// the agent's neutral mark.
    fn mark(&self, phase: Option<Phase>) -> Div {
        let theme = &self.theme;
        let slot = div()
            .flex_none()
            .size(self.z(theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_center();
        match phase {
            Some(phase) => slot.child(
                div()
                    .id(phase.label())
                    .role(Role::Image)
                    .aria_label(phase.label())
                    .child(phase.glyph(theme, self.z(theme.typography.icon()))),
            ),
            None => slot.child(
                icon(
                    theme,
                    crate::icons::AGENT,
                    IconSize::Inline,
                    hsla(theme.surfaces.text_secondary),
                )
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
        card: Option<&TaskCard>,
        check: Option<&Check<'_>>,
        piped: bool,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
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
            if check.is_none()
                && !piped
                && let Some(run) = board.verdict(card.id).filter(|r| r.passed)
            {
                parts.push(format!("Passed at {}", short_commit(&run.head)));
            }
            if let Some(words) =
                card.merge.as_ref().and_then(|m| super::model::merged_words(m, piped))
            {
                parts.push(words);
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

    /// What `card`'s check shows: the verifier at work, else its word while it still speaks
    /// to the task as it is now ([`Board::verdict`]).
    fn check<'a>(board: &'a Board, card: &'a TaskCard) -> Option<Check<'a>> {
        let running = card
            .step
            .as_ref()
            .filter(|s| s.running() && (s.kind == StepKind::Verify || s.term.is_some()));
        if let Some(step) = running
            && let StepState::Running { phase, .. } = &step.state
        {
            let line = crate::kit::first_line(phase).to_owned();
            return Some(Check::Running { line, term: step.term });
        }
        let term = card
            .step
            .as_ref()
            .filter(|s| s.kind == StepKind::Verify && matches!(s.state, StepState::Failed { .. }))
            .and_then(|s| s.term);
        let run = board.verdict(card.id)?;
        Some(Check::Verdict { run, term })
    }

    /// A verifier's run or verdict under a task: its mark and word, the commits it judged and
    /// how it ended, the way to its terminal, and for a failure the last lines it printed.
    fn check_block(&self, key: &str, check: &Check<'_>, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        // A verdict names the verifier: a task its failure sent back is Up next, and a bare
        // "Failed" there read as the Failed lane, which is a task given up.
        // The glyph wears its hue's mark step and the word stays neutral, a failure's excepted.
        let (glyph, ink, tone, word, detail, tail) = match check {
            Check::Running { line, .. } => (
                None,
                Status::Working.ink(theme),
                s.text_muted,
                "Verifying",
                line.clone(),
                Vec::new(),
            ),
            Check::Verdict { run, .. } if run.passed => (
                Some(Symbol::CheckmarkCircle),
                s.success_fill,
                s.text_secondary,
                "Verifier passed",
                verdict_detail(run),
                Vec::new(),
            ),
            Check::Verdict { run, .. } => (
                Some(Symbol::XmarkCircle),
                s.error_fill,
                s.error,
                "Verifier failed",
                verdict_detail(run),
                verdict_tail(run, TAIL_LINES),
            ),
        };
        let output = check.term().map(|term| {
            let id = format!("{key}-output");
            let selector = id.clone();
            let link = div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Link)
                .aria_label("Show the verifier's output")
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(sp.xxs))
                .px(self.z(sp.xxs))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |el| el.text_color(hsla(s.text)).bg(hsla(s.selected)))
                .child(
                    icon(theme, Symbol::Terminal, IconSize::Inline, hsla(s.text_muted))
                        .size(self.z(theme.typography.icon())),
                )
                .child("Output");
            tab_stop(link, s.focus).on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.stop_propagation();
                cx.emit(ProjectEvent::Output(term));
            }))
        });
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .min_w_0()
            .child(match glyph {
                Some(glyph) => icon(theme, glyph, IconSize::Inline, hsla(ink))
                    .size(self.z(theme.typography.icon()))
                    .into_any_element(),
                None => {
                    status_icon(theme, Status::Working, self.z(theme.typography.icon()), hsla(ink))
                }
            })
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
                // What the program printed, in the face a terminal and a tool's output use, at
                // the facts' size and a reading line, so why it failed is read, not squinted at.
                .font_family(theme.typography.mono_families.first().cloned().unwrap_or_default())
                .text_size(self.z(theme.typography.small()))
                .line_height(relative(theme.typography.markdown_line_height))
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
        let selector = format!("{key}-check");
        div()
            .debug_selector(move || selector)
            .flex()
            .flex_col()
            .gap(self.z(sp.xs))
            .pt(self.z(sp.xxs))
            .min_w_0()
            .text_size(self.z(theme.typography.small()))
            .child(head)
            .children(tail)
    }

    /// The lanes in their order, left to right and then down, as many across as the tile fits
    /// at [`LANE_W`] ([`lanes_across`]), sharing its width. A lane keeps its place however tall
    /// its neighbours grow, so the work reads in one direction; narrower than two lanes, they
    /// are sections down one column. The board never scrolls sideways: a sideways swipe moves
    /// the strip.
    fn board(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let lanes = board.lanes();
        if lanes.is_empty() {
            return vec![self.empty(NO_TASKS, NO_TASKS_HINT)];
        }
        let sp = self.theme.spacing;
        let across = usize::from(lanes_across(self.width, self.zoom)).clamp(1, lanes.len());
        let grid = div()
            .grid()
            .grid_cols(u16::try_from(across).unwrap_or(1))
            .items_start()
            .gap_x(self.z(sp.sm))
            .gap_y(self.z(sp.lg))
            .px(self.z(sp.inset() - sp.xs))
            .pt(self.z(sp.sm))
            .children(lanes.into_iter().map(|(lane, tasks)| self.lane(board, lane, tasks, cx)));
        vec![grid.into_any_element()]
    }

    /// What a card says besides its title: its check, the stages of its way to the target the
    /// check's own block does not say, and its second line.
    fn card_facts<'a>(&self, board: &'a Board, card: &'a TaskCard) -> CardFacts<'a> {
        let check = Self::check(board, card);
        let mut stages = board.pipeline(card.id);
        if check.is_some() {
            stages.retain(|stage| stage.kind != StageKind::Verifier);
        }
        let meta = self.node_meta(board, Some(card), check.as_ref(), !stages.is_empty());
        CardFacts { check, stages, meta }
    }

    /// One lane: its heading with its count, then its cards.
    fn lane(
        &self,
        board: &Board,
        lane: Lane,
        tasks: Vec<TaskId>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let sp = theme.spacing;
        let count = tasks.len();
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .px(self.z(sp.xs))
            .pb(self.z(sp.xs))
            .text_size(self.z(theme.typography.small()))
            .child(Phase::of(lane).glyph(theme, self.z(theme.typography.small())))
            .child(div().text_color(hsla(theme.surfaces.text_secondary)).child(lane.title()))
            .child(
                crate::kit::tabular(div())
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(SharedString::from(count.to_string())),
            );
        let cards = tasks.into_iter().filter_map(|task| {
            let card = board.tasks.get(&task)?;
            Some(self.card(board, card, cx))
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
            .p(self.z(sp.xs))
            .rounded(self.z(theme.radii.lg))
            .child(head)
            .children(cards)
            .into_any_element()
    }

    /// One task on the board: its number and its title on up to two lines, then, a step
    /// below, what moves it on, where it runs, its way to the target, its check and what the
    /// person can do. The title is the text ink at the task's size; the facts are the
    /// secondary ink, so what the task is reads before how it stands.
    fn card(&self, board: &Board, card: &TaskCard, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let node = Some(card.id);
        let picked = self.picked() == Some(node);
        let own = Lane::of(card.state);
        // The card leads with its lane's phase, or, while an agent works the task, with what
        // the agent says of itself; a task waiting on its own background work wears the dashed
        // ring.
        let lane_phase = match card.state {
            TaskState::Waiting => Phase::Waiting,
            state => Phase::of(Lane::of(state)),
        };
        let live = self.agent(board, node).map(|a| a.status);
        let phase = Some(match live {
            Some(status) if card.state.follows_the_agent() => lane_phase.with_agent(status),
            _ => lane_phase,
        });
        let key = format!("project-card-{}", card.id);
        let CardFacts { check, stages, meta } = self.card_facts(board, card);
        let held = stages.iter().any(|stage| stage.holds)
            || matches!(&check, Some(Check::Verdict { run, .. }) if !run.passed);
        let place = self.where_chip(board, node, "project-card", cx);
        let placed = board.place(node);
        let reason = placed.as_ref().and_then(|p| p.why.clone());
        let place_words =
            self.where_words(board, node).map_or_else(String::new, |(_, short, _)| short);
        let pipeline = self.pipeline_row(&key, &stages);
        let block = check.as_ref().map(|c| self.check_block(&key, c, cx));
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
                .text_size(self.z(theme.typography.small()))
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
        // The title's line: the mark and the number stand on its first.
        let roles = theme.roles();
        let line = self.z(roles.task_title.line);
        let first_line = |el: Div| el.flex_none().h(line).flex().items_center();
        let title = div()
            .flex()
            .items_start()
            .gap(self.z(sp.xs))
            .min_w_0()
            .child(first_line(div()).child(self.mark(phase)))
            .child(
                first_line(crate::kit::tabular(div()))
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!("#{}", card.id))),
            )
            .child(
                crate::kit::typed(div(), roles.task_title, self.zoom)
                    .flex_1()
                    .min_w_0()
                    .line_clamp(2)
                    .text_color(hsla(s.text))
                    .child(SharedString::from(card.title.clone())),
            );
        let meta = (!meta.is_empty()).then(|| {
            crate::kit::typed(div(), roles.metadata, self.zoom)
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_color(hsla(s.text_secondary))
                .child(dotted(theme, meta))
                .into_any_element()
        });
        let facts: Vec<AnyElement> = meta
            .into_iter()
            .chain(where_line.map(IntoElement::into_any_element))
            .chain(pipeline.map(IntoElement::into_any_element))
            .chain(block.map(IntoElement::into_any_element))
            .chain(
                self.run_on_block(card.id, "project-card", cx).map(IntoElement::into_any_element),
            )
            // Last, on a line of their own: a lane is too narrow for a title and its buttons.
            .chain(
                self.actions(board, card.id, "project-card", held, cx)
                    .map(IntoElement::into_any_element),
            )
            .collect();
        let facts = (!facts.is_empty())
            .then(|| div().flex().flex_col().gap(self.z(sp.xs)).min_w_0().children(facts));
        let el = crate::kit::card(theme)
            .id(SharedString::from(key))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(label)
            .flex()
            .flex_col()
            .gap(self.z(sp.sm))
            .p(self.z(sp.md))
            .rounded(self.z(theme.radii.md))
            .cursor_pointer()
            .when(picked, |el| crate::kit::selected(el, theme, true))
            .when(!picked, |el| el.hover(move |el| el.bg(hsla(s.selected))))
            .when(own == Lane::Merged, |el| el.opacity(alpha::STRONG))
            .child(title)
            .children(facts);
        let el = tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _w, cx| {
            this.picked = Some(node);
            cx.emit(ProjectEvent::Open(node));
            cx.notify();
        }));
        crate::kit::slide_fade(el, arrive, ARRIVE, crate::kit::Pace::Fade, cx)
    }

    /// Make the message to the orchestrator once there is a window, and put refused words back
    /// on it while it is empty. ↵ sends it and ⇧↵ starts a new line, as in a thread.
    fn composer_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_none() {
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder(COMPOSE_PLACEHOLDER)
                    .auto_grow(1, COMPOSE_ROWS)
                    .submit_on_enter(true)
            });
            let sending = cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
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

    /// Open the panel that sets how the project's work is checked, its fields holding what
    /// the project has now, the keyboard in the verifier's.
    fn open_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.seen.board.as_ref().map(|b| &b.project);
        let text = project.and_then(|p| p.verifier.clone()).unwrap_or_default();
        let verifier = cx.new(|cx| InputState::new(window, cx).placeholder(VERIFIER_HINT));
        verifier.update(cx, |input, cx| input.set_value(text, window, cx));
        let enter = cx.subscribe_in(&verifier, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_checks(window, cx);
            }
        });
        verifier.update(cx, |input, cx| input.focus(window, cx));
        self.checks = Some(Checks { verifier, _enter: enter });
        cx.notify();
    }

    /// What the open checks panel holds: the verifier.
    #[cfg(test)]
    pub(crate) fn checks_typed(&self, cx: &gpui::App) -> Option<String> {
        Some(self.checks.as_ref()?.verifier.read(cx).value().to_string())
    }

    /// Close the checks panel unsaved; the keyboard goes back to the board.
    fn close_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.checks.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Say the panel's checks to the server and close it.
    fn save_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(checks) = self.checks.as_ref() else { return };
        let verifier = checks.verifier.read(cx).value().trim().to_owned();
        cx.emit(ProjectEvent::SetChecks { verifier });
        self.close_checks(window, cx);
    }

    /// The panel under the header that sets how the work is checked, while it is open: the
    /// verifier command, and Save beside Cancel.
    fn checks_panel(&self, cx: &Context<Self>) -> Option<Stateful<Div>> {
        let checks = self.checks.as_ref()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let label = |text: &'static str| {
            div()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(text)
        };
        // A command: what is typed is code.
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let field =
            div().font_family(mono).child(Input::new(&checks.verifier).aria_label(VERIFIER));
        let button = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Button)
                .aria_label(text)
                .flex_none()
                .flex()
                .items_center()
                .h(self.z(theme.density.control))
                .px(self.z(sp.md))
                .rounded(self.z(theme.radii.sm))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .child(text)
        };
        let cancel = button("project-checks-cancel", "Cancel")
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        let cancel = tab_stop(cancel, s.focus)
            .on_click(cx.listener(|this, _ev, window, cx| this.close_checks(window, cx)));
        let save = crate::kit::solid_pressable(button("project-checks-save", "Save"), theme);
        let save = tab_stop(save, s.focus)
            .on_click(cx.listener(|this, _ev, window, cx| this.save_checks(window, cx)));
        let panel = div()
            .id("project-checks-panel")
            .debug_selector(|| "project-checks-panel".to_owned())
            .role(Role::Group)
            .aria_label(CHECKS)
            .flex_none()
            .flex()
            .flex_col()
            .gap(self.z(sp.xs))
            .mx(self.z(sp.inset()))
            .mb(self.z(sp.sm))
            .p(self.z(sp.md))
            .rounded(self.z(theme.radii.md))
            .bg(hsla(s.panel))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_checks(window, cx)))
            .child(label(VERIFIER))
            .child(field)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(self.z(sp.xs))
                    .pt(self.z(sp.xs))
                    .child(cancel)
                    .child(save),
            );
        Some(panel)
    }

    /// The message to the orchestrator at the board's foot, while it has one: a thread's
    /// composer's frame ([`crate::kit::message`]) with the field in the prose's size over its
    /// send control, the frame's edge in the accent while the keyboard is in it.
    fn composer_row(
        &self,
        board: &Board,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        board.project.orchestrator?;
        let (input, _) = self.composer.as_ref()?;
        let theme = &self.theme;
        let sp = theme.spacing;
        let focused = input.read(cx).focus_handle(cx).contains_focused(window, cx);
        let field = div().px(self.z(sp.md)).pt(self.z(sp.sm)).child(
            Textarea::new(input)
                .with_size(Size::XSmall)
                .appearance(false)
                .bordered(false)
                .text_size(self.z(theme.typography.prose()))
                .line_height(relative(theme.typography.prose_line_height))
                .aria_label(COMPOSE_LABEL),
        );
        let send = crate::kit::message::send_control(
            theme,
            self.zoom,
            "project-send",
            Symbol::ArrowUp,
            SEND,
        )
        .on_click(cx.listener(|this, _ev, window, cx| this.send_composed(window, cx)));
        let foot = div()
            .flex()
            .justify_end()
            .px(self.z(sp.sm))
            .pb(self.z(sp.sm))
            .child(tab_stop(send, theme.surfaces.focus));
        let frame = crate::kit::message::shell(
            div().flex().flex_col().gap(self.z(sp.xs)),
            theme,
            self.zoom,
            false,
            focused,
        )
        .child(field)
        .child(foot);
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
            .child(frame);
        Some(row)
    }

    /// A task's way to its target as one line of plain words parted by the quiet dot, wrapping
    /// when it must, a stage holding the merge back in the text ink: none of it is a control, so
    /// none of it wears a button's edge. Only a stage that failed is coloured, in the error ink
    /// with its mark, since that is what someone has to act on.
    fn pipeline_row(&self, key: &str, stages: &[Stage]) -> Option<Div> {
        if stages.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x(self.z(sp.xs))
            .min_w_0()
            .text_size(self.z(theme.typography.small()));
        for (ix, stage) in stages.iter().enumerate() {
            if ix > 0 {
                row = row.child(crate::kit::separator(theme));
            }
            let id = format!("{key}-{}", stage.kind.word());
            let selector = id.clone();
            let ink = if stage.failed {
                s.error
            } else if stage.holds {
                s.text
            } else {
                s.text_secondary
            };
            let mark = stage.failed.then(|| {
                icon(theme, Symbol::XmarkCircle, IconSize::Inline, hsla(ink))
                    .flex_none()
                    .size(self.z(theme.typography.icon()))
            });
            row = row.child(
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector)
                    .role(Role::Label)
                    .aria_label(SharedString::from(stage.words.clone()))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(self.z(sp.xxs))
                    .text_color(hsla(ink))
                    .children(mark)
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(stage.words.clone())),
                    ),
            );
        }
        Some(row)
    }

    /// The "Run on" picker under `task`'s row or card, while it is open there: "Anywhere",
    /// then every worker with its system and its agents, the one the task is pinned to marked.
    fn run_on_block(
        &self,
        task: TaskId,
        prefix: &str,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        let picker = self.seen.run_on.as_ref().filter(|p| p.task == task)?;
        let board = self.seen.board.as_ref()?;
        let pin = board.tasks.get(&task).and_then(|c| c.pin);
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
                    .hover(move |el| el.bg(hsla(s.selected)))
                    .child(div().flex_none().size(self.z(theme.typography.icon())).children(
                        on.then(|| {
                            icon(theme, Symbol::Checkmark, IconSize::Inline, hsla(s.text))
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
                tab_stop(el, s.focus).on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.stop_propagation();
                    cx.emit(ProjectEvent::Pin(task, choice));
                }))
            };
        let close_id = format!("{key}-close");
        let close =
            crate::kit::icon_button_at(theme, close_id, Symbol::Xmark, CLOSE_RUN_ON, self.zoom)
                .on_click(cx.listener(|_this, _ev, _w, cx| {
                    cx.stop_propagation();
                    cx.emit(ProjectEvent::CloseRunOn);
                }));
        let head = div()
            .flex()
            .items_center()
            .gap(self.z(sp.xs))
            .child(
                div()
                    .flex_1()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(format!("Run #{task} on"))),
            )
            .child(close);
        let mut options = vec![option(
            format!("{key}-anywhere"),
            ANYWHERE.to_owned(),
            ANYWHERE_LINE.to_owned(),
            pin.is_none(),
            true,
            RunOn::Anywhere,
        )];
        let ranking = match &picker.workers {
            None => Some(
                div()
                    .px(self.z(sp.xs))
                    .text_color(hsla(s.text_muted))
                    .child(RANKING)
                    .into_any_element(),
            ),
            Some(workers) => {
                options.extend(workers.iter().enumerate().map(|(i, w)| {
                    let (name, line, online) = run_on_words(w);
                    option(
                        format!("{key}-{i}"),
                        name,
                        line,
                        pin == Some(w.worker),
                        online,
                        RunOn::Worker(w.worker),
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
                .text_size(self.z(theme.typography.small()))
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
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(hint),
            )
            .into_any_element()
    }
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

/// The word a recap line's selector ends in.
const fn recap_word(kind: RecapKind) -> &'static str {
    match kind {
        RecapKind::VerifyFailed => "verify-failed",
        RecapKind::Conflicts => "conflicts",
        RecapKind::ChecksFailed => "checks-failed",
        RecapKind::StepFailed => "step-failed",
        RecapKind::AgentEnded => "ended",
        RecapKind::Merged => "merged",
        RecapKind::Verified => "verified",
        RecapKind::Started => "started",
        RecapKind::Created => "created",
    }
}

/// A recap line's mark: the one its timeline entries draw.
const fn recap_icon(kind: RecapKind) -> Symbol {
    match kind {
        RecapKind::VerifyFailed | RecapKind::StepFailed => Symbol::XmarkCircle,
        RecapKind::Conflicts => Symbol::ArrowTriangleBranch,
        RecapKind::ChecksFailed => Symbol::ArrowTrianglePull,
        RecapKind::AgentEnded => Symbol::Power,
        RecapKind::Merged => Symbol::ArrowTriangleMerge,
        RecapKind::Verified => Symbol::CheckmarkCircle,
        RecapKind::Started => Symbol::Terminal,
        RecapKind::Created => Symbol::Plus,
    }
}

impl Render for ProjectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
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
            .on_action(cx.listener(|this, _: &RunTaskOn, _w, cx| {
                this.act_on_picked(TaskAction::RunOn, cx);
            }))
            .on_action(cx.listener(|this, _: &MergeTask, _w, cx| {
                this.act_on_picked(TaskAction::Merge, cx);
            }))
            .on_action(cx.listener(|this, _: &RetryTask, _w, cx| {
                this.act_on_picked(TaskAction::Retry, cx);
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
            .on_action(cx.listener(|this, _: &PushTask, _w, cx| {
                this.act_on_picked(TaskAction::PushAgain, cx);
            }))
            .on_action(cx.listener(|this, _: &CancelTask, _w, cx| {
                this.act_on_picked(TaskAction::Cancel, cx);
            }))
            .on_action(cx.listener(|this, _: &StopTaskAgent, _w, cx| {
                this.act_on_picked(TaskAction::Stop, cx);
            }))
            .on_action(cx.listener(|this, _: &TogglePush, _w, cx| this.toggle_push(cx)))
            .on_action(cx.listener(|_this, _: &ShowTerminal, _w, cx| {
                cx.emit(ProjectEvent::Open(None));
            }))
            .on_action(cx.listener(|this, _: &DeleteProject, _w, cx| this.delete(cx)))
            .on_action(cx.listener(|this, _: &TellOrchestrator, window, cx| {
                this.compose(window, cx);
            }))
            .on_action(cx.listener(|this, _: &EditChecks, window, cx| {
                this.open_checks(window, cx);
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
        let body = self.board(&board, cx);
        let composer = self.composer_row(&board, window, cx);
        let keys =
            keys.children(self.recap(&board, cx)).children(self.needs_you(&board, cx)).child(
                self.scroll_fade(
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
                ),
            );
        // The header and the checks panel sit over the board's keys, as the line to the
        // orchestrator sits under them: a letter typed into a field there is a letter.
        root.child(self.header(&board, cx))
            .children(self.checks_panel(cx))
            .child(keys)
            .children(composer)
    }
}

impl ProjectView {
    /// `body` fading out at its edges while more lies past them, so a lane cut at the foot reads
    /// as more below, not as the end. The body fades per pixel; the tile's surface is outside
    /// the fade.
    fn scroll_fade(&self, body: impl IntoElement) -> gpui::EdgeFadeElement {
        let spacing = self.theme.spacing;
        let edges = gpui::Edges {
            top: self.z(spacing.md),
            bottom: self.z(spacing.xl),
            ..gpui::Edges::default()
        };
        gpui::edge_fade(body, gpui::EdgeFade::new(edges)).hidden_by_scroll(&self.scroll)
    }
}
