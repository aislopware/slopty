//! The board: one project drawn in its orchestrator's tile.
//!
//! A header names the project and where its work lands, over a bar that is every task at once,
//! each a segment in its lane's tone. Under it, whatever waits on the person, then one of three
//! lenses: the tree of who split what from whom, down to the subagents running inside a session;
//! the board, each task in the lane its most urgent descendant is in; the timeline, newest
//! first. Every node that runs somewhere opens its agent's tile with a click or ↩.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Div, ElementId, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, relative,
};
use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Merge, Moment, NativeCounts, ProjectId, ReportKind, ReviewRun, StepKind, StepState, TaskCard,
    TaskId, TaskState, TaskStep, VerifierRun,
};
use slopty_theme::{Rgb, Theme, Typography, alpha};

use super::model::{
    Board, Lane, TreeRow, finding_place, queue_words, review_detail, short_commit, state_status,
    state_word, verdict_detail, verdict_tail,
};
use super::{Lens, OpenNode, SelectNext, SelectPrevious, ShowBoard, ShowTimeline, ShowTree};
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

/// The least a lane is wide at zoom 1: the tile takes as many across as fit.
const LANE_W: f32 = 232.0;
/// How far a level of the tree steps in, at zoom 1.
const INDENT: f32 = 16.0;
/// How far a row that arrives travels up into its place, at zoom 1: a new task, an entry.
const ARRIVE: f32 = 4.0;
/// How often the timeline's ages move on: they say minutes at the finest.
const AGE_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// The progress bar's height, at zoom 1.
const BAR_H: f32 = 3.0;

/// A node of the tree: a task, or the orchestrator for `None`.
pub type Node = Option<TaskId>;

/// What a board tells the workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectEvent {
    /// Open this node's agent in its tile.
    Open(Node),
    /// Show a terminal the server runs for a task, its verifier's, in its tile.
    Output(TermRef),
    /// Open the Claude Code session a task's reviewer reads its work in.
    Reviewer(TermRef),
}

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

/// What the workspace hands a board: the project and what the board names it by.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Seen {
    /// The project, or `None` once the server has let it go.
    pub board: Option<Arc<Board>>,
    /// Each worker's name.
    pub workers: BTreeMap<WorkerId, String>,
    /// The agents this client sees, by session.
    pub agents: HashMap<SessionId, AgentSeen>,
    /// The server's clock now, near enough, for the timeline's ages.
    pub now: WallMs,
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
    focus: FocusHandle,
    scroll: ScrollHandle,
    plate: Plate,
    /// Moves the timeline's ages on once a minute while the timeline shows.
    tick: Option<Task<()>>,
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
            theme,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            plate: Plate::default(),
            tick: None,
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
        let same =
            same_board && self.seen.workers == seen.workers && self.seen.agents == seen.agents;
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
            self.theme = theme;
            cx.notify();
        }
    }

    /// Show `lens`.
    pub fn show(&mut self, lens: Lens, cx: &mut Context<Self>) {
        if self.lens != lens {
            self.lens = lens;
            self.picked = self.picked().map(Pick::Node).filter(|p| self.picks().contains(p));
            self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    /// The timeline's ages move on once a minute while it shows, and nothing ticks otherwise.
    fn keep_time(&mut self, cx: &Context<Self>) {
        if self.lens != Lens::Timeline {
            self.tick = None;
            return;
        }
        if self.tick.is_some() {
            return;
        }
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AGE_TICK).await;
                let ticked = this.update(cx, |v, cx| {
                    v.seen.now = WallMs::now();
                    cx.notify();
                });
                if ticked.is_err() {
                    break;
                }
            }
        }));
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
        }
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
        self.seen.workers.get(&worker).cloned().unwrap_or_else(|| "a worker".to_owned())
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

/// A lane's tone: the mark its tasks wear.
const fn lane_tone(theme: &Theme, lane: Lane) -> Rgb {
    let s = &theme.surfaces;
    match lane {
        Lane::NeedsYou => s.warn,
        Lane::Failed => s.error,
        Lane::Working | Lane::Verifying => s.accent,
        Lane::UpNext => s.text_muted,
        Lane::ReadyToMerge | Lane::Merged => s.success,
    }
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

    /// The name, where the work lands, and how far along it is.
    fn header(&self, board: &Board) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let project = &board.project;
        let (merged, total) = board.progress();
        let progress = (total > 0).then(|| format!("{merged} of {total} merged"));
        let live = board.live();
        let limit = project.limits.live_per_project;
        let mut place =
            vec![project.id.to_string(), format!("{} \u{2192} {}", project.repo, project.target)];
        if let Some(verifier) = &project.verifier {
            place.push(format!("verified by {verifier}"));
        }
        if project.review.is_some() {
            place.push("reviewed before it merges".to_owned());
        }
        place.push(format!("{live} of {limit} agents live"));
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
            .children(progress.map(|p| {
                crate::kit::tabular(div())
                    .id("project-progress")
                    .debug_selector(|| "project-progress".to_owned())
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_secondary))
                    .child(SharedString::from(p))
            }));
        let meta = div()
            .id("project-place")
            .debug_selector(|| "project-place".to_owned())
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(s.text_muted))
            .child(dotted(theme, place.join(" \u{b7} ")));
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

    /// Every task at once: a segment each, in its own lane's tone, left to right as the lanes
    /// run. The bar is the board seen from across the room.
    fn bar(&self, board: &Board) -> Div {
        let theme = &self.theme;
        let total = board.tasks.len();
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
            .bg(hsla(theme.surfaces.border_subtle));
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
            let line = Line { asking: Some(second), depth: 0, prefix: "project-needs" };
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
    }

    /// One node on two lines: its mark, its number and title with its state at the right, then
    /// where it runs and what it is on. `depth` steps it in under its parent.
    fn node_row(&self, board: &Board, node: Node, line: Line, cx: &Context<Self>) -> AnyElement {
        let Line { asking, depth, prefix } = line;
        let mark = asking.is_some().then_some(Status::NeedsYou);
        let second = asking;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let sp = theme.spacing;
        let card = node.and_then(|t| board.tasks.get(&t));
        let status = mark.or_else(|| self.node_status(board, node));
        let title = card.map_or_else(|| ORCHESTRATOR.to_owned(), |c| c.title.clone());
        // A row under "Needs you" says no word its heading says.
        let word = card
            .filter(|_| mark.is_none())
            .map(|c| (state_word(c.state), state_status(c.state).tone(theme)));
        let settled = card.is_some_and(|c| c.state == TaskState::Merged);
        // The tree shows a verifier running or failed under its row; a pass is a word in it.
        let check = card
            .filter(|_| prefix == "project-row")
            .and_then(|c| Self::check(board, c))
            .filter(|c| !c.cleared());
        let meta = second.unwrap_or_else(|| self.node_meta(board, node, card, check.as_ref()));
        let key = format!("{prefix}-{}", node_key(node));
        let picked =
            prefix == "project-row" && self.picked() == Some(node) && self.lens == Lens::Tree;
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
            .children(word.map(|(word, tone)| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(tone))
                    .child(word)
            }));
        let second = div()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(self.z(theme.typography.meta()))
            .text_color(hsla(s.text_muted))
            .child(dotted(theme, meta.clone()));
        let label = SharedString::from(match word {
            Some((word, _)) => format!("{title}, {word}, {meta}"),
            None => format!("{title}, {meta}"),
        });
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
                    .child(second)
                    .children(check.as_ref().map(|c| self.check_block(&key, c, cx))),
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
                hsla(status.tone(theme)),
            )),
            None => slot.child(
                icon(theme, IconName::Bot, IconSize::Inline, hsla(theme.surfaces.text_secondary))
                    .size(self.z(theme.typography.icon())),
            ),
        }
    }

    /// A node's second line: where it runs, its branch and worktree, what it still waits on,
    /// what runs inside it, and what its agent says it is doing.
    fn node_meta(
        &self,
        board: &Board,
        node: Node,
        card: Option<&TaskCard>,
        check: Option<&Check<'_>>,
    ) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(worker) = board.worker(node) {
            parts.push(self.worker_name(worker));
        } else if card.is_some() {
            parts.push("Not placed".to_owned());
        }
        if let Some(card) = card {
            if let Some(branch) = &card.branch {
                parts.push(branch.clone());
            }
            if let Some(pr) = &card.pr {
                parts.push(format!("#{}", pr.number));
            }
            let waits = board.waiting_on(card.id);
            if !waits.is_empty() && matches!(card.state, TaskState::Planned) {
                let list: Vec<String> = waits.iter().map(|t| format!("#{t}")).collect();
                parts.push(format!("after {}", list.join(", ")));
            }
            if card.read_only {
                parts.push("reads only".to_owned());
            }
            // A step under way or failed says so on the row; one done is the timeline's, and a
            // check running, or a word with its own block, says it there.
            let shown = card
                .step
                .as_ref()
                .filter(|s| !matches!(s.state, StepState::Done { .. }))
                .filter(|s| !check.is_some_and(|c| c.speaks_for(s)));
            if let Some(step) = shown {
                parts.push(super::model::step_line(step, |w| self.worker_name(w)));
            }
            let merging =
                card.step.as_ref().is_some_and(|s| s.kind == StepKind::Merge && s.running());
            if let Some((place, _)) = board.queue_place(card.id).filter(|_| !merging) {
                parts.push(queue_words(place));
            }
            if check.is_none() {
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
        }
        let counts = card.map_or(board.orchestrator_natives, |c| c.natives);
        parts.extend(natives_line(counts));
        if let Some(status) = card.and_then(|c| c.status.as_deref()).filter(|s| !s.is_empty()) {
            parts.push(crate::kit::first_line(status).to_owned());
        }
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
        let (status, word, detail, tail) = match check {
            Check::Running { line, review: false, .. } => {
                (Status::Working, "Verifying", line.clone(), Vec::new())
            }
            Check::Running { line, review: true, .. } => {
                (Status::Working, "Reviewing", line.clone(), Vec::new())
            }
            Check::Verdict { run, .. } if run.passed => {
                (Status::Done, "Passed", verdict_detail(run), Vec::new())
            }
            Check::Verdict { run, .. } => {
                (Status::Failed, "Failed", verdict_detail(run), verdict_tail(run, TAIL_LINES))
            }
            Check::Review { run, .. } if run.verdict.approved => {
                (Status::Done, "Approved", review_detail(run), Vec::new())
            }
            Check::Review { run, .. } => {
                (Status::Failed, "Changes asked", review_detail(run), Vec::new())
            }
        };
        let review = check.is_review();
        let (link_word, link_label) = if review {
            ("Reviewer", "Open the reviewer's session")
        } else {
            ("Output", "Show the verifier's output")
        };
        let tone = status.tone(theme);
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
            .child(status_icon(theme, status, self.z(theme.typography.icon()), hsla(tone)))
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
            let tone = if f.blocking { s.error } else { s.text_muted };
            div()
                .flex()
                .items_baseline()
                .gap(self.z(sp.xs))
                .min_w_0()
                .child(div().flex_none().text_color(hsla(tone)).child(sentence_case(&f.severity)))
                .children(finding_place(f).map(|place| {
                    // The file's own name and line, as narrow as a card is: the path is in the
                    // session and in what the agent was told.
                    let short = place.rsplit('/').next().unwrap_or(&place).to_owned();
                    div()
                        .flex_none()
                        .max_w(relative(0.4))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .font_family(mono.clone())
                        .text_size(self.z(theme.typography.caption()))
                        .text_color(hsla(s.text_muted))
                        .child(short)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(hsla(s.text_secondary))
                        .child(crate::kit::first_line(&f.body).to_owned()),
                )
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
            let line = Line { asking: None, depth: row.depth, prefix: "project-row" };
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
        let meta = self.node_meta(board, node, Some(card), check.as_ref());
        let block =
            check.as_ref().map(|c| self.check_block(&format!("project-card-{}", card.id), c, cx));
        // A parent standing in a lane for a descendant says whose it is.
        let why = (own != lane).then(|| format!("{} in a subtask", lane.title()));
        let key = format!("project-card-{}", card.id);
        let arrive = ElementId::Name(format!("{key}-in").into());
        let selector = key.clone();
        let label =
            SharedString::from(format!("{}, {}, {meta}", card.title, state_word(card.state)));
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
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(dotted(theme, meta)),
            )
            .children(block)
            .children(why.map(|why| {
                div()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(lane_tone(theme, lane)))
                    .child(SharedString::from(why))
            }));
        let el = tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, _w, cx| {
            this.picked = Some(Pick::Node(node));
            cx.emit(ProjectEvent::Open(node));
            cx.notify();
        }));
        crate::kit::slide_fade(el, arrive, ARRIVE, crate::kit::Pace::Fade, cx)
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

/// How a node's row is drawn: in the tree at its depth, or in what needs the person with what
/// its agent asks.
struct Line {
    /// What the agent asks, for a row of what needs the person.
    asking: Option<String>,
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
        (Some(a), Some(t)) => Some(format!("{a} \u{b7} {t}")),
    }
}

/// A timeline entry's icon and tone.
fn moment_icon(theme: &Theme, what: &Moment) -> (IconName, Hsla) {
    let s = &theme.surfaces;
    let (glyph, tone) = match what {
        Moment::Created | Moment::TaskCreated { .. } => (IconName::Plus, s.text_muted),
        Moment::Orchestrator { .. } => (IconName::Bot, s.text_secondary),
        Moment::Limits { .. } => (IconName::ListFilter, s.text_muted),
        Moment::Claimed { .. } => (IconName::Lock, s.text_muted),
        Moment::Assigned { .. } => (IconName::SquareTerminal, s.accent),
        Moment::State { to, .. } => {
            let status = state_status(*to);
            (status.icon(), status.tone(theme))
        }
        Moment::Branch { pr: Some(_), .. } => (IconName::GitPullRequest, s.text_secondary),
        Moment::Branch { .. } => (IconName::GitBranch, s.text_secondary),
        Moment::Verified(run) if run.passed => (IconName::CircleCheck, s.success),
        Moment::Verified(_) => (IconName::CircleX, s.error),
        Moment::Reviewed(run) if run.verdict.approved => (IconName::CircleCheck, s.success),
        Moment::Reviewed(_) => (IconName::MessageSquareWarning, s.error),
        Moment::AgentGone { .. } => (IconName::Power, s.text_muted),
        Moment::Note { .. } => (IconName::MessageSquare, s.text_secondary),
        Moment::Reported { report } => match report.kind {
            ReportKind::Checkpoint => (IconName::Flag, s.text_secondary),
            ReportKind::NeedsInput => (IconName::MessageSquareWarning, s.warn),
            ReportKind::Stuck => (IconName::CircleAlert, s.error),
            ReportKind::Done => (IconName::CircleCheck, s.success),
        },
        Moment::Delivered { .. } => (IconName::Inbox, s.text_muted),
        Moment::Step(step) => match (step.kind, &step.state) {
            (_, StepState::Failed { .. }) => (IconName::CircleX, s.error),
            (StepKind::Clone, _) => (IconName::FolderGit2, s.text_secondary),
            (StepKind::Home, StepState::Done { .. }) => (IconName::GitBranch, s.success),
            (StepKind::Home, _) => (IconName::Download, s.text_secondary),
            (StepKind::Verify, _) => (IconName::ListChecks, s.text_secondary),
            (StepKind::Merge, StepState::Done { .. }) => (IconName::GitMerge, s.success),
            (StepKind::Merge, _) => (IconName::GitMerge, s.text_secondary),
            (StepKind::Review, _) => (IconName::Eye, s.text_secondary),
        },
    };
    (glyph, hsla(tone))
}

impl Render for ProjectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        self.keep_time(cx);
        let theme = &self.theme;
        let root = div()
            .id("project")
            .debug_selector(|| "project".to_owned())
            .key_context(CTX)
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label(SharedString::from(format!("Project {}", self.id)))
            .aria_value(SharedString::from(self.summary()))
            .on_action(cx.listener(|this, _: &SelectNext, _w, cx| this.select_by(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _w, cx| this.select_by(-1, cx)))
            .on_action(cx.listener(|this, _: &OpenNode, _w, cx| this.open_picked(cx)))
            .on_action(cx.listener(|this, _: &ShowTree, _w, cx| this.show(Lens::Tree, cx)))
            .on_action(cx.listener(|this, _: &ShowBoard, _w, cx| this.show(Lens::Board, cx)))
            .on_action(cx.listener(|this, _: &ShowTimeline, _w, cx| this.show(Lens::Timeline, cx)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.ui_size))
            .text_color(hsla(theme.surfaces.text));
        let Some(board) = self.seen.board.clone() else {
            return root
                .child(self.empty(PROJECT_GONE, "Its tasks and agents are as they were left."));
        };
        let body = match self.lens {
            Lens::Tree => self.tree(&board, cx),
            Lens::Board => self.board(&board, cx),
            Lens::Timeline => self.timeline(&board, cx),
        };
        root.child(self.header(&board))
            .children(self.needs_you(&board, cx))
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
            )
    }
}
