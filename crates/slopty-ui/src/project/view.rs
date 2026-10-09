//! The board: one project drawn in its orchestrator's tile.
//!
//! A header names the project and where its work lands, over a bar of how much of it has
//! merged. Under it, one grouped list, as Linear's issues are: the lanes in their order, what
//! needs the person first (the orchestrator leading it while it waits on them), each a head
//! over a row per task in it; at its foot, the message to the orchestrator, in the frame a
//! thread's composer has. Every row whose agent runs somewhere opens that agent's tile with a
//! click or ↩.

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
use slopty_proto::thread::AgentId;
use slopty_theme::{Theme, Typography, alpha};

use super::model::{
    Board, Lane, Place, PlaceHow, RunOnPicker, Stage, StageKind, TaskAction, os_name, pull_words,
    queue_words, run_on_words, short_commit, state_word, verdict_detail, verdict_tail,
};
use super::recap::{Recap, RecapKind};
use super::{
    AddressComments, CancelTask, DeleteProject, EditChecks, FixCi, GiveTaskToAgent, MergeTask,
    OpenNode, PushTask, ResolveConflicts, RetryTask, ReviewTask, RunTaskOn, SelectNext,
    SelectPrevious, ShowTerminal, StartTask, StartTaskFresh, StopTaskAgent, TellOrchestrator,
    TogglePush,
};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{GitGlyph, IconSize, Mark, Phase, Status, Symbol, icon, status_icon};
use crate::kit::Priority;
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

/// How far a card that arrives travels up into its place.
const ARRIVE: f32 = 4.0;
/// The least the board's name narrows to in its header, and a row's title in its row, in ems
/// of the chrome's text, before its progress, or the row's facts, leave.
const TITLE_FLOOR_EM: f32 = 8.0;

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
    /// Hand the task's work to this agent: the "Give to another agent" picker's choice.
    GiveTo(TaskId, AgentId),
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
    /// What machine it is, once it has said: the glyph its place wears.
    pub form: Option<slopty_proto::server::Form>,
    /// The agents it can start, while it is linked: what "Give to another agent" offers for a
    /// task that ran there.
    pub agents: Vec<AgentId>,
    /// It is out of reach now: a task running there is not heard from until it is back.
    pub away: bool,
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

/// A task the person asked to start again, until the board shows its new agent or the server
/// refuses: what its card says meanwhile.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Handing {
    /// "Starting #3 fresh…", "Handing #3 to Codex…".
    words: String,
    /// The agent's terminal or seat it had when asked; a new one says the start came.
    from: Option<TermRef>,
}

/// One project's board.
pub struct ProjectView {
    id: ProjectId,
    seen: Seen,
    /// The card the keyboard stands on.
    picked: Option<Node>,
    /// The person opened *Merged*, which otherwise folds to its head.
    merged_open: bool,
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
    /// The task whose "Give to another agent" picker is open.
    giving: Option<TaskId>,
    /// The tasks asked to start again whose new agent the board does not show yet.
    handing: BTreeMap<TaskId, Handing>,
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
            merged_open: false,
            hint_theme: Rc::new(theme.clone()),
            theme,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            plate: Plate::default(),
            delete_asked: None,
            composer: None,
            refused: None,
            checks: None,
            giving: None,
            handing: BTreeMap::new(),
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
            self.settle_handing();
            cx.notify();
        }
    }

    /// Say at once on `task`'s card that it starts again, before the server answers.
    fn hand(&mut self, task: TaskId, words: String) {
        let card = self.seen.board.as_ref().and_then(|b| b.tasks.get(&task));
        let from = card.and_then(|c| c.assignment.as_ref()).map(|a| a.term);
        self.handing.insert(task, Handing { words, from });
    }

    /// The server refused to start `task` again: its card says what it did before.
    pub fn handing_refused(&mut self, task: TaskId, cx: &mut Context<Self>) {
        if self.handing.remove(&task).is_some() {
            cx.notify();
        }
    }

    /// Forget the asks the board has answered: a task with a new agent, merged, or gone.
    fn settle_handing(&mut self) {
        let Some(board) = &self.seen.board else {
            self.handing.clear();
            return;
        };
        self.handing.retain(|task, handing| {
            board.tasks.get(task).is_some_and(|card| {
                card.state != TaskState::Merged
                    && card.assignment.as_ref().is_none_or(|a| Some(a.term) == handing.from)
            })
        });
    }

    /// Do `action` to `task`: the board's own pickers open here, and a start again shows at
    /// once on its card.
    fn act(&mut self, task: TaskId, action: TaskAction, cx: &mut Context<Self>) {
        match action {
            TaskAction::GiveTo => return self.toggle_giving(task, cx),
            TaskAction::StartFresh => {
                self.giving = None;
                self.hand(task, format!("Starting #{task} fresh\u{2026}"));
                cx.notify();
            }
            TaskAction::Start => {
                self.hand(task, format!("Starting #{task}\u{2026}"));
                cx.notify();
            }
            _ => {}
        }
        cx.emit(ProjectEvent::Act(task, action));
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
    pub fn act_on_picked(&mut self, action: TaskAction, cx: &mut Context<Self>) {
        let Some(board) = &self.seen.board else { return };
        let Some(Some(task)) = self.picked() else {
            cx.emit(ProjectEvent::Say(format!("Stand on a task to {}", verb_of(action))));
            return;
        };
        let handing = self.handing.contains_key(&task)
            && matches!(action, TaskAction::Start | TaskAction::StartFresh | TaskAction::GiveTo);
        if handing {
            let again = if action == TaskAction::Start { "" } else { " again" };
            cx.emit(ProjectEvent::Say(format!("#{task} is starting{again}")));
        } else if board.actions(task).contains(&action) || board.controls(task).contains(&action) {
            self.act(task, action, cx);
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

    /// Every row the keyboard can stand on, in the order the lanes draw them; a folded
    /// lane's are not drawn.
    fn picks(&self) -> Vec<Node> {
        let Some(board) = &self.seen.board else { return Vec::new() };
        board
            .lanes()
            .into_iter()
            .filter(|(lane, tasks)| !self.folded(board, *lane, tasks))
            .flat_map(|(_, tasks)| tasks)
            .map(Some)
            .collect()
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
        let away = self.runs_away(&place);
        let short = match os {
            _ if away => format!("{name} \u{b7} away"),
            Some(os) => format!("{name} \u{b7} {os}"),
            None => name.clone(),
        };
        let on = os.map_or_else(|| name.clone(), |os| format!("{name}, {os}"));
        let mut lines = vec![match place.how {
            PlaceHow::Runs => format!("Runs on {on}"),
            PlaceHow::Ran => format!("Ran on {on}"),
            PlaceHow::Pinned => format!("Pinned to {on}"),
        }];
        if away {
            lines.push(format!("{name} is away: its agent is not heard from until it is back"));
        }
        lines.extend(place.worktree.as_ref().map(|w| format!("Worktree {w}")));
        lines.extend(place.branch.as_ref().map(|b| format!("Branch {b}")));
        lines.extend(place.why.as_ref().map(|why| format!("Why: {why}")));
        if node.is_some_and(|t| board.movable(t)) {
            lines.push(MOVE_HINT.to_owned());
        }
        Some((place, short, lines.join("\n")))
    }

    /// Whether `place` is where an agent runs on a machine out of reach now.
    fn runs_away(&self, place: &Place) -> bool {
        place.how == PlaceHow::Runs && self.seen.workers.get(&place.worker).is_some_and(|w| w.away)
    }

    /// `node`'s place as a quiet chip: the worker and its system in words, in a stronger ink
    /// while its agent runs there, with no machine glyph: a row carries one mark, its state's,
    /// and a machine's colour is the navigator's alone. A pin keeps its lock, which the words
    /// do not say. Its hint says the rest; a click moves a task not started yet ("Run on…"),
    /// and a chip that cannot move is only words.
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
        let (pinned, tone) = match place.how {
            PlaceHow::Runs => (false, s.text_secondary),
            PlaceHow::Ran => (false, s.text_muted),
            PlaceHow::Pinned => (true, s.text_muted),
        };
        let movable = node.filter(|t| board.movable(*t));
        // Its machine out of reach, the chip wears the away mark: the row's own mark still says
        // the task's state, which nothing has changed yet.
        let id = format!("{prefix}-{}-where", node_key(node));
        let away = self.runs_away(&place).then(|| {
            let side = px(theme.typography.icon());
            let selector = format!("{id}-away");
            div().debug_selector(move || selector).flex_none().child(status_icon(
                theme,
                Status::Away,
                side,
                hsla(s.text_muted),
            ))
        });
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
            .gap(px(sp.xxs))
            .px(px(sp.xxs))
            .rounded(px(theme.radii.sm))
            .overflow_hidden()
            .text_size(px(theme.roles().metadata.size))
            .text_color(hsla(tone))
            .when(pinned, |el| {
                el.child(
                    icon(theme, Symbol::Lock, IconSize::Inline, hsla(tone))
                        .size(px(theme.typography.icon())),
                )
            })
            .children(away)
            .child(div().min_w_0().truncate().child(SharedString::from(short)))
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

/// A switch: a track the solid fills while on, its knob at the far end.
pub(super) fn switch(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    on: bool,
) -> Stateful<Div> {
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
        .w(px(width))
        .h(px(sp.lg))
        .flex()
        .items_center()
        .px(px(sp.xxs))
        .rounded_full()
        .bg(track)
        .cursor_pointer()
        .child(
            div()
                .flex_none()
                .size(px(knob))
                .ml(px(if on { travel } else { 0.0 }))
                .rounded_full()
                .bg(hsla(ink)),
        );
    tab_stop(el, s.focus)
}

/// What an action does, as "nothing to …" and "stand on a task to …" say it.
const fn verb_of(action: TaskAction) -> &'static str {
    match action {
        TaskAction::Review => "review",
        TaskAction::Merge => "merge",
        TaskAction::Retry => "retry",
        TaskAction::Start => "start",
        TaskAction::RunOn => "choose where it runs",
        TaskAction::FixCi => "fix",
        TaskAction::AddressComments => "address",
        TaskAction::ResolveConflicts => "resolve",
        TaskAction::PushAgain => "push again",
        TaskAction::Cancel => "cancel",
        TaskAction::Stop => "stop",
        TaskAction::StartFresh => "start fresh",
        TaskAction::GiveTo => "give to another agent",
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
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(SharedString::from(text))
        };
        let push = project.push;
        let push_toggle =
            crate::kit::icon_toggle(theme, "project-push", Symbol::ArrowUpToLine, PUSH, push)
                .on_click(cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(ProjectEvent::SetPush(!push));
                }));
        let terminal =
            crate::kit::icon_button(theme, "project-terminal", Symbol::Terminal, SHOW_TERMINAL)
                .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::Open(None))));
        let checks_open = self.checks.is_some();
        let checks_toggle = crate::kit::icon_toggle(
            theme,
            "project-checks",
            Symbol::Checklist,
            CHECKS,
            checks_open,
        )
        .on_click(cx.listener(|this, _ev, window, cx| {
            if this.checks.is_some() {
                this.close_checks(window, cx);
            } else {
                this.open_checks(window, cx);
            }
        }));
        let name = div()
            .id("project-title")
            .debug_selector(|| "project-title".to_owned())
            .role(Role::Heading)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .map(|el| crate::kit::typed(el, theme.roles().panel_title))
            .text_color(hsla(s.text))
            .child(SharedString::from(project.title.clone()));
        // The name, then how far along it is and what runs, then its controls. Where the board
        // is narrow the running count leaves first, then the name narrows to its floor, and
        // only then does the progress leave: the bar under the name says it too. The dot
        // between the two readouts goes with the count, so it never parts nothing.
        let parted = progress.is_some();
        let live = readout("project-live", live);
        let live = if parted {
            div()
                .flex()
                .items_center()
                .gap(px(sp.xs))
                .child(crate::kit::separator(theme).text_size(px(theme.typography.small())))
                .child(live)
                .into_any_element()
        } else {
            live.into_any_element()
        };
        let mut title = crate::kit::priority_row("project-header")
            .h(px(crate::kit::icon_button_side(theme)))
            .gap(px(sp.xs))
            .title(name, px(theme.typography.ui_size * TITLE_FLOOR_EM))
            .end();
        if let Some(progress) = progress {
            title = title.item("progress", Priority::MEDIUM, readout("project-progress", progress));
        }
        let title = title
            .item("live", Priority::LOW, live)
            .item("checks", Priority::ESSENTIAL, checks_toggle)
            .item("push", Priority::ESSENTIAL, push_toggle)
            .item("terminal", Priority::ESSENTIAL, terminal);
        let meta = div()
            .id("project-place")
            .debug_selector(|| "project-place".to_owned())
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(place));
        div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(sp.xxs))
            .px(px(sp.inset()))
            .pt(px(sp.md))
            .pb(px(sp.sm))
            .child(title)
            .child(meta)
            .child(self.bar(board))
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
        let stood_on = [TaskAction::Start, TaskAction::RunOn];
        let mut actions: Vec<(TaskAction, bool)> =
            offered.iter().copied().filter(|a| !stood_on.contains(a)).map(|a| (a, false)).collect();
        if self.picked() == Some(Some(task)) {
            // A task asked to start again is not asked twice while it does.
            let handing = self.handing.contains_key(&task);
            let starts = stood_on.into_iter().filter(|a| offered.contains(a));
            actions.extend(
                starts.filter(|a| !(handing && *a == TaskAction::Start)).map(|a| (a, true)),
            );
            let controls = board
                .controls(task)
                .into_iter()
                .filter(|a| !(handing && matches!(a, TaskAction::StartFresh | TaskAction::GiveTo)));
            actions.extend(controls.map(|a| (a, true)));
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
                .h(px(theme.density.hit))
                .px(px(sp.sm))
                .rounded(px(theme.radii.sm))
                .text_size(px(theme.typography.small()))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .child(action.label());
            let el = if control {
                el.text_color(hsla(s.text_secondary))
                    .hover(move |el| el.bg(hsla(s.selected)).text_color(hsla(s.text)))
            } else if held && ix == 0 {
                crate::kit::solid_pressable(el, theme)
            } else if ix == 0 {
                crate::kit::secondary(el, theme)
            } else {
                // One box to a row: what follows the first reads as a control beside it.
                el.text_color(hsla(s.text_secondary))
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            };
            tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                this.act(task, action, cx);
            }))
        });
        Some(div().flex_none().flex().items_center().gap(px(sp.xxs)).children(buttons))
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
                .height(px(theme.spacing.xs))
                .at_once();
        div()
            .id("project-bar")
            .debug_selector(|| "project-bar".to_owned())
            .mt(px(theme.spacing.xs))
            .child(bar)
    }

    /// The orchestrator while it waits on the person: the first row of *Needs you*, what it
    /// asks on the line under it. Each task waiting on the person has its own row there.
    fn orchestrator_row(&self, board: &Board, cx: &Context<Self>) -> Option<AnyElement> {
        let agent = self.agent(board, None).filter(|a| a.status == Status::NeedsYou)?;
        let s = &self.theme.surfaces;
        let asks = agent.asks.clone().unwrap_or_else(|| WAITING_ON_YOU.to_owned());
        let key = "project-needs-orchestrator";
        let line = crate::kit::priority_row(SharedString::from(format!("{key}-line")))
            .h(self.row_height())
            .gap(px(self.theme.spacing.sm))
            .item("mark", Priority::ESSENTIAL, self.mark(Some(Phase::NeedsYou)))
            .title(self.row_title(ORCHESTRATOR.to_owned()), self.title_floor());
        let el = self
            .row_frame(key.to_owned(), said(&[ORCHESTRATOR, &asks]), false)
            .role(Role::Button)
            .child(line)
            .children(self.detail(vec![self.asks_line(key, asks)]));
        let el = tab_stop(el, s.focus).on_click(cx.listener(|_this, _ev, _w, cx| {
            cx.emit(ProjectEvent::Open(None));
        }));
        Some(el.into_any_element())
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
        let close =
            crate::kit::icon_button(theme, "project-recap-close", Symbol::Xmark, CLOSE_RECAP)
                .on_click(cx.listener(|_this, _ev, _w, cx| cx.emit(ProjectEvent::CloseRecap)));
        let head = div()
            .flex()
            .items_center()
            .pr(px(sp.xs))
            .child(
                div()
                    .id("project-recap-heading")
                    .debug_selector(|| "project-recap-heading".to_owned())
                    .role(Role::Heading)
                    .aria_label(SharedString::from(heading.clone()))
                    .flex_1()
                    .min_w_0()
                    .px(px(sp.inset()))
                    .pt(px(sp.xs))
                    .pb(px(sp.xxs))
                    .text_size(px(theme.typography.small()))
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
                .mb(px(sp.xs))
                .pb(px(sp.xs))
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
            Some(kind) => (recap_icon(kind), s.text_secondary),
            None => (Mark::Symbol(Symbol::Clock), s.text_muted),
        };
        let key = id.to_owned();
        div()
            .id(SharedString::from(id.to_owned()))
            .debug_selector(move || key)
            .role(Role::ListItem)
            .aria_label(SharedString::from(text.clone()))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.inset()))
            .py(px(theme.spacing.xxs))
            .min_w_0()
            .child(
                icon(theme, glyph, IconSize::Inline, hsla(tone))
                    .flex_none()
                    .size(px(theme.typography.icon())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(text)),
            )
    }

    /// A phase's glyph in its fixed slot, named by the phase; with none,
    /// the agent's neutral mark.
    fn mark(&self, phase: Option<Phase>) -> Div {
        let theme = &self.theme;
        let slot = div()
            .flex_none()
            .size(px(theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_center();
        match phase {
            Some(phase) => slot.child(
                div()
                    .id(phase.label())
                    .role(Role::Image)
                    .aria_label(phase.label())
                    .child(phase.glyph(theme, px(theme.typography.icon()))),
            ),
            None => slot.child(
                icon(
                    theme,
                    crate::icons::AGENT,
                    IconSize::Inline,
                    hsla(theme.surfaces.text_secondary),
                )
                .size(px(theme.typography.icon())),
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
        asked: bool,
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
            if let Some(status) = card.status.as_deref().filter(|s| !asked && !s.is_empty()) {
                parts.push(crate::kit::first_line(status).to_owned());
            }
        }
        if let Some(card) = card {
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

    /// A failed verdict under its task's row: its mark and word, the commits it judged, the
    /// way to its terminal, and in an inset the last lines it printed.
    fn check_block(&self, key: &str, check: &Check<'_>, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        // A verdict names the verifier: a task its failure sent back is Up next, and a bare
        // "Failed" there read as the Failed lane, which is a task given up.
        let (detail, tail) = match check {
            Check::Verdict { run, .. } => (verdict_detail(run), verdict_tail(run, TAIL_LINES)),
            Check::Running { line, .. } => (line.clone(), Vec::new()),
        };
        let output = check.term().map(|term| self.output_link(key, term, cx));
        let head = div()
            .flex()
            .items_center()
            .gap(px(sp.xs))
            .min_w_0()
            .child(
                icon(theme, Symbol::XmarkCircle, IconSize::Inline, hsla(s.error_fill))
                    .size(px(theme.typography.icon())),
            )
            .child(div().flex_none().text_color(hsla(s.error)).child("Verifier failed"))
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
                .px(px(sp.sm))
                .py(px(sp.xs))
                .rounded(px(theme.radii.sm))
                .map(|el| crate::kit::inset(el, theme))
                // What the program printed, in the face a terminal and a tool's output use, at
                // the facts' size and a reading line, so why it failed is read, not squinted at.
                .font_family(theme.typography.mono_families.first().cloned().unwrap_or_default())
                .text_size(px(theme.typography.small()))
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
            .gap(px(sp.xs))
            .pt(px(sp.xxs))
            .min_w_0()
            .text_size(px(theme.typography.small()))
            .child(head)
            .children(tail)
    }

    /// The lanes as one grouped list, in their order, the most urgent first, as Linear's
    /// grouped issues are: a lane's head (its glyph, its name, its count) over its rows, the
    /// groups parted by space alone. *Merged* folds to its head while nothing in it needs the
    /// person. The board never scrolls sideways.
    fn board(&self, board: &Board, cx: &Context<Self>) -> Vec<AnyElement> {
        let lanes = board.lanes();
        let mut orchestrator = self.orchestrator_row(board, cx);
        let sp = self.theme.spacing;
        let mut groups = Vec::new();
        if orchestrator.is_some() && !lanes.iter().any(|(lane, _)| *lane == Lane::NeedsYou) {
            groups.push(self.lane(board, Lane::NeedsYou, Vec::new(), orchestrator.take(), cx));
        }
        let empty = lanes.is_empty();
        for (lane, tasks) in lanes {
            let lead = if lane == Lane::NeedsYou { orchestrator.take() } else { None };
            groups.push(self.lane(board, lane, tasks, lead, cx));
        }
        if empty {
            groups.push(self.empty(NO_TASKS, NO_TASKS_HINT));
        }
        let list = div()
            .flex()
            .flex_col()
            .gap(px(sp.lg))
            .px(px(sp.inset() - sp.xs))
            .pt(px(sp.xs))
            .children(groups);
        vec![list.into_any_element()]
    }

    /// Whether `lane` folds to its head: *Merged*, while the person has not opened it and
    /// none of its tasks has anything for them to do (a push that failed).
    fn folded(&self, board: &Board, lane: Lane, tasks: &[TaskId]) -> bool {
        lane == Lane::Merged
            && !self.merged_open
            && tasks.iter().all(|task| board.actions(*task).is_empty())
    }

    /// What a row says besides its title: its check, the stages of its way to the target the
    /// check does not say, and its facts.
    fn card_facts<'a>(&self, board: &'a Board, card: &'a TaskCard, asked: bool) -> CardFacts<'a> {
        let check = Self::check(board, card);
        let mut stages = board.pipeline(card.id);
        if check.is_some() {
            stages.retain(|stage| stage.kind != StageKind::Verifier);
        }
        let meta = self.node_meta(board, Some(card), check.as_ref(), !stages.is_empty(), asked);
        CardFacts { check, stages, meta }
    }

    /// One lane: its head (its state's glyph, its name, and its count at the trailing edge),
    /// then its rows in one raised group, `lead` first (the orchestrator waiting on the
    /// person), a quiet hairline between two rows, as the references' boards and Apple's
    /// grouped lists set them. A folding lane's head is the button that opens it.
    fn lane(
        &self,
        board: &Board,
        lane: Lane,
        tasks: Vec<TaskId>,
        lead: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let roles = theme.roles();
        let count = tasks.len().saturating_add(usize::from(lead.is_some()));
        let folds = lane == Lane::Merged && tasks.iter().all(|t| board.actions(*t).is_empty());
        let folded = self.folded(board, lane, &tasks);
        let head_id = format!("project-lane-{}-head", lane.selector());
        let selector = head_id.clone();
        let glyph = Phase::of(lane).glyph(theme, px(IconSize::beside_slot(theme, roles.metadata)));
        let head = div()
            .id(SharedString::from(head_id.clone()))
            .debug_selector(move || selector)
            .flex_none()
            .h(px(theme.density.row))
            .flex()
            .items_center()
            .gap(px(sp.sm))
            .px(px(sp.xs))
            .rounded(px(theme.radii.sm))
            .child(self.slot().child(glyph))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(sp.xs))
                    .child(
                        crate::kit::label(theme, lane.title()).text_size(px(roles.metadata.size)),
                    )
                    .when(folds, |el| {
                        el.child(crate::kit::Disclosure::new(
                            format!("{head_id}-fold"),
                            !folded,
                            theme,
                            px(theme.typography.icon()),
                            hsla(s.text_muted),
                        ))
                    }),
            )
            .child(
                crate::kit::tabular(div())
                    .debug_selector({
                        let id = format!("{head_id}-count");
                        move || id
                    })
                    .ml_auto()
                    .text_size(px(roles.metadata.size))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(count.to_string())),
            );
        let head = if folds {
            let what = if folded { "Show" } else { "Hide" };
            let head = head
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{what} {} {count}", lane.title())))
                .aria_expanded(!folded)
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.hover)));
            tab_stop(head, s.focus)
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.merged_open = !this.merged_open;
                    if !this.merged_open
                        && let Some(Some(task)) = this.picked
                        && this.seen.board.as_ref().and_then(|b| b.lane(task)) == Some(Lane::Merged)
                    {
                        this.picked = None;
                    }
                    cx.notify();
                }))
                .into_any_element()
        } else {
            head.role(Role::Heading).aria_label(lane.title()).into_any_element()
        };
        let rows: Vec<AnyElement> = if folded {
            Vec::new()
        } else {
            let tasks = tasks.into_iter().filter_map(|task| {
                let card = board.tasks.get(&task)?;
                Some(self.row(board, card, cx))
            });
            lead.into_iter().chain(tasks).collect()
        };
        let group = (!rows.is_empty()).then(|| {
            let group = div()
                .debug_selector({
                    let id = format!("project-lane-{}-group", lane.selector());
                    move || id
                })
                .min_w_0()
                .flex()
                .flex_col()
                .p(px(sp.xxs))
                .rounded(px(theme.radii.md));
            crate::kit::raised(group, theme).children(rows.into_iter().enumerate().map(
                |(ix, row)| {
                    div()
                        .min_w_0()
                        .when(ix > 0, |el| {
                            el.border_t(crate::kit::HAIR).border_color(hsla(s.stroke))
                        })
                        .child(row)
                },
            ))
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
            .gap(px(sp.xxs))
            .child(head)
            .children(group)
            .into_any_element()
    }

    /// A row's height: Linear's 32 on the Mac, a finger's row on a touch screen.
    const fn row_height(&self) -> gpui::Pixels {
        let theme = &self.theme;
        px(theme.spacing.xxl.max(theme.density.row))
    }

    /// The least a row's title narrows to before its facts leave.
    fn title_floor(&self) -> gpui::Pixels {
        px(self.theme.typography.ui_size * TITLE_FLOOR_EM)
    }

    /// The column a row's mark and a lane's glyph stand in.
    fn slot(&self) -> Div {
        div()
            .flex_none()
            .size(px(self.theme.typography.icon_large()))
            .flex()
            .items_center()
            .justify_center()
    }

    /// A row's title: the action role (13, medium) in the text ink, on one line, ending in an
    /// ellipsis when its facts leave it less than its width.
    fn row_title(&self, text: String) -> Div {
        crate::kit::typed(div(), self.theme.roles().action)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_color(hsla(self.theme.surfaces.text))
            .child(SharedString::from(text))
    }

    /// A row's frame, `key` its name: no edge and no fill at rest, the hover's wash under the
    /// pointer, the selection while the keyboard stands on it.
    fn row_frame(&self, key: String, label: SharedString, picked: bool) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let selector = key.clone();
        div()
            .id(SharedString::from(key))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(label)
            .min_w_0()
            .flex()
            .flex_col()
            .px(px(theme.spacing.xs))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(picked, |el| crate::kit::selected(el, theme, true))
            .when(!picked, |el| el.hover(move |el| el.bg(hsla(s.hover))))
    }

    /// What stands under a row's line, from its number's edge: what it asks, what holds it,
    /// a failed check's last lines, the "Run on" picker. Nothing, most of the time.
    fn detail(&self, parts: Vec<AnyElement>) -> Option<Div> {
        let theme = &self.theme;
        let sp = theme.spacing;
        (!parts.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap(px(sp.xs))
                .min_w_0()
                .pl(px(theme.typography.icon_large() + sp.sm))
                .pb(px(sp.sm))
                .children(parts)
        })
    }

    /// What an agent asks the person, as a row's second line in the secondary ink.
    fn asks_line(&self, key: &str, asks: String) -> AnyElement {
        let id = format!("{key}-asks");
        crate::kit::typed(div(), self.theme.roles().metadata)
            .debug_selector(move || id)
            .min_w_0()
            .line_clamp(2)
            .text_color(hsla(self.theme.surfaces.text_secondary))
            .child(SharedString::from(asks))
            .into_any_element()
    }

    /// A start again the person asked for, while the board does not show its new agent: in the
    /// secondary ink, and told to a screen reader as it changes.
    fn handing_line(&self, key: &str, words: String) -> AnyElement {
        let id = format!("{key}-handing");
        let selector = id.clone();
        crate::kit::typed(div(), self.theme.roles().metadata)
            .id(SharedString::from(id))
            .debug_selector(move || selector)
            .role(Role::Status)
            .aria_label(SharedString::from(words.clone()))
            .min_w_0()
            .text_color(hsla(self.theme.surfaces.text_secondary))
            .child(SharedString::from(words))
            .into_any_element()
    }

    /// One of a row's facts at its trailing end: words in the facts' size and the muted ink,
    /// after a glyph when it has one, on one line.
    fn fact(&self, id: String, glyph: Option<Mark>, words: String) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let roles = theme.roles();
        let selector = id.clone();
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector)
            .role(Role::Label)
            .aria_label(SharedString::from(words.clone()))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .whitespace_nowrap()
            .text_size(px(roles.metadata.size))
            .text_color(hsla(s.text_muted))
            .children(glyph.map(|glyph| {
                crate::icons::beside(theme, glyph, roles.metadata, hsla(s.text_secondary))
                    .size(px(IconSize::beside_slot(theme, roles.metadata)))
            }))
            .child(SharedString::from(words))
    }

    /// A check that is not a failure, as a fact: the verifier at work, or its pass, with the
    /// way to its terminal. A failure stands under the row with its last lines.
    fn check_fact(&self, key: &str, check: &Check<'_>, cx: &Context<Self>) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let side = px(theme.typography.icon());
        // A pass is its word alone: the row's own done glyph already marks it.
        let (glyph, word, label) = match check {
            Check::Running { line, .. } => (
                Some(status_icon(theme, Status::Working, side, hsla(Status::Working.ink(theme)))),
                "Verifying",
                format!("Verifying, {line}"),
            ),
            Check::Verdict { run, .. } => {
                (None, "Verified", format!("Verifier passed {}", verdict_detail(run)))
            }
        };
        let output = check.term().map(|term| self.output_link(key, term, cx));
        let selector = format!("{key}-check");
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .role(Role::Group)
            .aria_label(SharedString::from(label))
            .flex()
            .items_center()
            .gap(px(sp.xxs))
            .whitespace_nowrap()
            .text_size(px(theme.roles().metadata.size))
            .text_color(hsla(s.text_muted))
            .children(glyph)
            .child(word)
            .children(output)
    }

    /// The way to a verifier's terminal.
    fn output_link(&self, key: &str, term: TermRef, cx: &Context<Self>) -> Stateful<Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
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
            .gap(px(theme.spacing.xxs))
            .px(px(theme.spacing.xxs))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .text_color(hsla(s.text_muted))
            .hover(move |el| el.text_color(hsla(s.text)).bg(hsla(s.selected)))
            .child(
                icon(theme, Symbol::Terminal, IconSize::Inline, hsla(s.text_secondary))
                    .size(px(theme.typography.icon())),
            )
            .child("Output");
        tab_stop(link, s.focus).on_click(cx.listener(move |_this, _ev, _w, cx| {
            cx.stop_propagation();
            cx.emit(ProjectEvent::Output(term));
        }))
    }

    /// One task on the board, a row: its mark, its number and its title, then at the trailing
    /// end its facts (what moves it on, its check, its way to the target, its branch, where it
    /// runs) and what the person can do. What needs reading goes under it: the question its
    /// agent asks, a stage holding it, a failed check's last lines.
    fn row(&self, board: &Board, card: &TaskCard, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let node = Some(card.id);
        let picked = self.picked() == Some(node);
        let own = Lane::of(card.state);
        // The row leads with its lane's phase, or, while an agent works the task, with what
        // the agent says of itself; a task waiting on its own background work wears the dashed
        // ring.
        let lane_phase = match card.state {
            TaskState::Waiting => Phase::Waiting,
            state => Phase::of(Lane::of(state)),
        };
        let agent = self.agent(board, node);
        let live = agent.map(|a| a.status);
        let phase = Some(match live {
            Some(status) if card.state.follows_the_agent() => lane_phase.with_agent(status),
            _ => lane_phase,
        });
        let key = format!("project-card-{}", card.id);
        // What its agent asks, while the task waits on the person: its own word, else the
        // status the task was left with.
        let asks = (own == Lane::NeedsYou || live == Some(Status::NeedsYou))
            .then(|| {
                agent
                    .and_then(|a| a.asks.clone())
                    .or_else(|| card.status.as_deref().filter(|s| !s.is_empty()).map(str::to_owned))
            })
            .flatten();
        let handing = self.handing.get(&card.id).map(|h| h.words.clone());
        let CardFacts { check, stages, meta } = self.card_facts(board, card, asks.is_some());
        let failed = matches!(&check, Some(Check::Verdict { run, .. }) if !run.passed);
        let held = stages.iter().any(|stage| stage.holds) || failed;
        let place_words =
            self.where_words(board, node).map_or_else(String::new, |(_, short, _)| short);
        let reason = board.place(node).and_then(|p| p.why);
        let along: Vec<&str> = stages.iter().map(|stage| stage.words.as_str()).collect();
        let label = said(&[
            &card.title,
            state_word(card.state),
            asks.as_deref().unwrap_or(""),
            handing.as_deref().unwrap_or(""),
            &place_words,
            reason.as_deref().unwrap_or(""),
            &meta,
            &along.join(", "),
        ]);
        let (loud, quiet): (Vec<Stage>, Vec<Stage>) = stages
            .into_iter()
            .filter(|stage| stage.kind != StageKind::Branch)
            .partition(|stage| stage.holds || stage.failed);

        let number = crate::kit::tabular(div())
            .flex_none()
            .whitespace_nowrap()
            .text_size(px(theme.roles().metadata.size))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(format!("#{}", card.id)));
        let mut line = crate::kit::priority_row(SharedString::from(format!("{key}-line")))
            .h(self.row_height())
            .gap(px(sp.sm))
            .item("mark", Priority::ESSENTIAL, self.mark(phase))
            .item("number", Priority::ESSENTIAL, number)
            .title(self.row_title(card.title.clone()), self.title_floor())
            .end();
        if !meta.is_empty() {
            let meta = div()
                .whitespace_nowrap()
                .text_size(px(theme.roles().metadata.size))
                .text_color(hsla(s.text_muted))
                .child(dotted(theme, meta));
            line = line.item("meta", Priority::LOW, meta);
        }
        if let Some(check) = check.as_ref().filter(|_| !failed) {
            line = line.item("check", Priority::HIGH, self.check_fact(&key, check, cx));
        }
        for stage in quiet {
            let rank = match stage.kind {
                StageKind::Queue => Priority(160),
                StageKind::ToDos => Priority::LOW,
                _ => Priority::MEDIUM,
            };
            let id = format!("{key}-{}", stage.kind.word());
            line = line.item(stage.kind.word(), rank, self.fact(id, None, stage.words));
        }
        if let Some(branch) = &card.branch {
            let fact =
                self.fact(format!("{key}-branch"), Some(GitGlyph::Branch.into()), branch.clone());
            line = line.item("branch", Priority(96), fact);
        }
        if let Some(why) = reason {
            line = line.item("why", Priority::LOW, self.fact(format!("{key}-why"), None, why));
        }
        if let Some(chip) = self.where_chip(board, node, "project-card", cx) {
            line = line.item("where", Priority(144), chip);
        }
        if let Some(actions) = self.actions(board, card.id, "project-card", held, cx) {
            line = line.item("actions", Priority::ESSENTIAL, actions);
        }

        let detail: Vec<AnyElement> = handing
            .map(|words| self.handing_line(&key, words))
            .into_iter()
            .chain(asks.map(|asks| self.asks_line(&key, asks)))
            .chain(self.pipeline_row(&key, &loud).map(IntoElement::into_any_element))
            .chain(
                check
                    .as_ref()
                    .filter(|_| failed)
                    .map(|c| self.check_block(&key, c, cx).into_any_element()),
            )
            .chain(
                self.run_on_block(card.id, "project-card", cx).map(IntoElement::into_any_element),
            )
            .chain(self.give_block(card, "project-card", cx).map(IntoElement::into_any_element))
            .collect();
        let arrive = ElementId::Name(format!("{key}-in").into());
        let el = self
            .row_frame(key, label, picked)
            .when(own == Lane::Merged, |el| el.opacity(alpha::STRONG))
            .child(line)
            .children(self.detail(detail));
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
            div().text_size(px(theme.typography.small())).text_color(hsla(s.text_muted)).child(text)
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
                .h(px(theme.density.control))
                .px(px(sp.md))
                .rounded(px(theme.radii.sm))
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
            .gap(px(sp.xs))
            .mx(px(sp.inset()))
            .mb(px(sp.sm))
            .p(px(sp.md))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.ground))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.close_checks(window, cx)))
            .child(label(VERIFIER))
            .child(field)
            .child(
                div().flex().justify_end().gap(px(sp.xs)).pt(px(sp.xs)).child(cancel).child(save),
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
        let field = div().px(px(sp.md)).pt(px(sp.sm)).child(
            Textarea::new(input)
                .with_size(Size::XSmall)
                .appearance(false)
                .bordered(false)
                .text_size(px(theme.typography.prose()))
                .line_height(relative(theme.typography.prose_line_height))
                .aria_label(COMPOSE_LABEL),
        );
        let send = crate::kit::message::send_control(theme, "project-send", Symbol::ArrowUp, SEND)
            .on_click(cx.listener(|this, _ev, window, cx| this.send_composed(window, cx)));
        let foot = div()
            .flex()
            .justify_end()
            .px(px(sp.sm))
            .pb(px(sp.sm))
            .child(tab_stop(send, theme.surfaces.focus));
        let frame = crate::kit::message::shell(
            div().flex().flex_col().gap(px(sp.xs)),
            theme,
            crate::kit::message::Frame { focused, ..Default::default() },
        )
        .child(field)
        .child(foot);
        let row = div()
            .id("project-composer")
            .debug_selector(|| "project-composer".to_owned())
            .flex_none()
            .px(px(sp.inset() - sp.xs))
            .pt(px(sp.xs))
            .pb(px(sp.sm))
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
    fn pipeline_row(&self, key: &str, stages: &[Stage]) -> Option<crate::kit::FactsRow> {
        if stages.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        // Wrapped between stages, never with a dot left at either end of a line.
        let mut row = crate::kit::facts_row(SharedString::from(format!("{key}-stages")), theme)
            .gap_x(px(sp.xs))
            .gap_y(px(sp.xxs))
            .text_size(px(theme.typography.small()));
        for stage in stages {
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
                    .size(px(theme.typography.icon()))
            });
            row = row.fact(
                div()
                    .id(SharedString::from(id))
                    .debug_selector(move || selector)
                    .role(Role::Label)
                    .aria_label(SharedString::from(stage.words.clone()))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(sp.xxs))
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

    /// Open the "Give to another agent" picker under `task`, or shut it there.
    fn toggle_giving(&mut self, task: TaskId, cx: &mut Context<Self>) {
        self.giving = if self.giving == Some(task) { None } else { Some(task) };
        cx.notify();
    }

    /// The "Give to another agent" picker under `card`, while it is open there: the agents the
    /// machine it ran on can start. Its work stays where it is, so the next agent goes there.
    fn give_block(
        &self,
        card: &TaskCard,
        prefix: &str,
        cx: &Context<Self>,
    ) -> Option<Stateful<Div>> {
        let task = card.id;
        if self.giving != Some(task) {
            return None;
        }
        let worker = card.assignment.as_ref().map(|a| a.term.worker)?;
        let seen = self.seen.workers.get(&worker);
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let key = format!("{prefix}-give-{task}");
        let close = crate::kit::icon_button(theme, format!("{key}-close"), Symbol::Xmark, "Close")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                cx.stop_propagation();
                this.giving = None;
                cx.notify();
            }));
        let machine = seen.map_or_else(|| "its machine".to_owned(), |w| w.name.clone());
        let head = div()
            .flex()
            .items_center()
            .gap(px(sp.xs))
            .child(
                div()
                    .flex_1()
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(format!("Give #{task} to an agent on {machine}"))),
            )
            .child(close);
        let agents = seen.map(|w| w.agents.as_slice()).unwrap_or_default();
        let options = agents.iter().enumerate().map(|(i, agent)| {
            let id = format!("{key}-{i}");
            let selector = id.clone();
            let name = crate::conversation::thread::view::agent_label(agent);
            let pick = agent.clone();
            let handing = format!("Handing #{task} to {name}\u{2026}");
            let el = div()
                .id(SharedString::from(id))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Give #{task} to {name}")))
                .px(px(sp.xs))
                .py(px(sp.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text))
                .hover(move |el| el.bg(hsla(s.selected)))
                .child(SharedString::from(name));
            tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                this.giving = None;
                this.hand(task, handing.clone());
                cx.emit(ProjectEvent::GiveTo(task, pick.clone()));
                cx.notify();
            }))
        });
        let options: Vec<_> = options.collect();
        let none = agents.is_empty().then(|| {
            div()
                .px(px(sp.xs))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(format!("{machine} is not linked here")))
        });
        let selector = key.clone();
        Some(
            div()
                .id(SharedString::from(key))
                .debug_selector(move || selector)
                .role(Role::Group)
                .aria_label(SharedString::from(format!("Give #{task} to another agent")))
                .flex()
                .flex_col()
                .gap(px(sp.xxs))
                .mt(px(sp.xs))
                .px(px(sp.sm))
                .py(px(sp.xs))
                .rounded(px(theme.radii.sm))
                .bg(hsla(s.ground))
                .text_size(px(theme.typography.small()))
                .child(head)
                .children(options)
                .children(none),
        )
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
                    .gap(px(sp.xs))
                    .min_w_0()
                    .px(px(sp.xs))
                    .py(px(sp.xxs))
                    .rounded(px(theme.radii.sm))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.selected)))
                    .child(div().flex_none().size(px(theme.typography.icon())).children(on.then(
                        || {
                            icon(theme, Symbol::Checkmark, IconSize::Inline, hsla(s.text))
                                .size(px(theme.typography.icon()))
                        },
                    )))
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
        let close = crate::kit::icon_button(theme, close_id, Symbol::Xmark, CLOSE_RUN_ON).on_click(
            cx.listener(|_this, _ev, _w, cx| {
                cx.stop_propagation();
                cx.emit(ProjectEvent::CloseRunOn);
            }),
        );
        let head = div()
            .flex()
            .items_center()
            .gap(px(sp.xs))
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
                    .px(px(sp.xs))
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
                .gap(px(sp.xxs))
                .mt(px(sp.xs))
                .px(px(sp.sm))
                .py(px(sp.xs))
                .rounded(px(theme.radii.sm))
                .bg(hsla(s.ground))
                .text_size(px(theme.typography.small()))
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
            .gap(px(theme.spacing.xxs))
            .px(px(theme.spacing.inset()))
            .py(px(theme.spacing.md))
            .child(div().text_color(hsla(theme.surfaces.text_secondary)).child(line))
            .child(
                div()
                    .text_size(px(theme.typography.small()))
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
const fn recap_icon(kind: RecapKind) -> Mark {
    match kind {
        RecapKind::VerifyFailed | RecapKind::StepFailed => Mark::Symbol(Symbol::XmarkCircle),
        RecapKind::Conflicts => Mark::Git(GitGlyph::Branch),
        RecapKind::ChecksFailed => Mark::Git(GitGlyph::PullRequest),
        RecapKind::AgentEnded => Mark::Symbol(Symbol::Power),
        RecapKind::Merged => Mark::Git(GitGlyph::Merge),
        RecapKind::Verified => Mark::Symbol(Symbol::CheckmarkCircle),
        RecapKind::Started => Mark::Symbol(Symbol::Terminal),
        RecapKind::Created => Mark::Symbol(Symbol::Plus),
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
            .on_action(cx.listener(|this, _: &ReviewTask, _w, cx| {
                this.act_on_picked(TaskAction::Review, cx);
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
            .on_action(cx.listener(|this, _: &StartTask, _w, cx| {
                this.act_on_picked(TaskAction::Start, cx);
            }))
            .on_action(cx.listener(|this, _: &StartTaskFresh, _w, cx| {
                this.act_on_picked(TaskAction::StartFresh, cx);
            }))
            .on_action(cx.listener(|this, _: &GiveTaskToAgent, _w, cx| {
                this.act_on_picked(TaskAction::GiveTo, cx);
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
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(theme.surfaces.text));
        let Some(board) = self.seen.board.clone() else {
            return root.child(
                keys.child(self.empty(PROJECT_GONE, "Its tasks and agents are as they were left.")),
            );
        };
        let body = self.board(&board, cx);
        let composer = self.composer_row(&board, window, cx);
        let keys = keys.children(self.recap(&board, cx)).child(
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
                            .pt(px(theme.spacing.xs))
                            .pb(px(theme.spacing.md))
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
        let edges =
            gpui::Edges { top: px(spacing.md), bottom: px(spacing.xl), ..gpui::Edges::default() };
        gpui::edge_fade(body, gpui::EdgeFade::new(edges)).hidden_by_scroll(&self.scroll)
    }
}
