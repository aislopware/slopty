//! The face: one virtualized column of the conversation, the prompt rail beside it, the task
//! card, and the composer (or the permission card that takes its place) under it.
//!
//! The list is gpui's `ListState` in tail-follow, as Zed's agent panel keeps its thread: new
//! rows keep the reader at the bottom while they are there, and the strict band gpui keeps
//! (the view within a point of the end) is the only thing that re-arms the follow once they
//! scrolled away, so a row growing never yanks them back. Rows are rebuilt from the model on
//! every change and spliced into the list by key: rows that kept their key keep their place
//! and their measured height, and only rows whose content moved are measured again.

mod blocks;
mod entries;
mod parts;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, EventEmitter, FocusHandle, Focusable, FollowMode,
    InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, list, px,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use slopty_core::{ClientId, SessionId};
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::conversation::{
    AgentRun, Body, ConversationEvent, PermissionEvent, TextRef, ThreadId, ToolDetail, Verdict,
};
use slopty_theme::Theme;

use super::approval::Approvals;
use super::diff::Block;
use super::model::Model;
use super::rows::{self, Density, Input, Row};
use super::{CTX, CycleDensity, Interrupt, MESSAGE_PLACEHOLDER};
use crate::colors::hsla;

/// Diffs coloured once, by entry id and revision.
type Coloured = HashMap<(String, u64), Rc<[Block]>>;

/// How far past the viewport the list lays rows out, as Zed's thread does: a fling shows
/// rows already measured.
const OVERDRAW: f32 = 2048.0;

/// The composer grows with its text up to this many rows, then scrolls.
const COMPOSER_ROWS: usize = 8;

/// How long a message the transcript never records (a local command) shows as pending.
const PENDING_FOR: Duration = Duration::from_secs(15);

/// How wide a tile has to be, at rest, for an edit's diff to show its sides beside each other.
pub const SPLIT_FROM: f32 = 960.0;

/// What the face asks the workspace to do: everything that reaches the worker goes through
/// the workspace, which knows the link.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FaceEvent {
    /// Type this message into the agent's terminal.
    Submit(String),
    /// Stop the agent's turn (Esc in its terminal).
    Interrupt,
    /// Answer the held permission prompt `ask`.
    Answer {
        /// The prompt.
        ask: u64,
        /// The answer.
        verdict: Verdict,
    },
    /// Ask for the whole of a clipped text.
    Expand {
        /// Its thread.
        thread: ThreadId,
        /// Where it is.
        reference: TextRef,
    },
    /// Show the TUI in the tile instead.
    ShowTerminal,
}

/// A session's conversation face.
pub struct ConversationView {
    session: SessionId,
    theme: Theme,
    /// The chrome's zoom (the overview's).
    zoom: f32,
    /// The tile's width at rest, in points: whether a diff splits.
    width: f32,
    model: Model,
    /// The worker's word on the agent: whether its turn is live, and since when.
    agent: Option<AgentEvent>,
    density: Density,
    /// Folds, groups and calls the reader opened or closed, by row key.
    toggled: HashSet<String>,
    /// The thread on show: the session's own, or a subagent's.
    thread: ThreadId,
    rows: Rc<[Row]>,
    /// Each row's key and revision, as the list holds them.
    keys: Vec<(String, u64)>,
    list: ListState,
    composer: gpui::Entity<TextareaState>,
    /// Why a denial is given, typed on the permission card.
    deny: gpui::Entity<InputState>,
    /// The deny field is open on the card.
    deny_open: bool,
    approvals: Approvals,
    /// This client's id on the worker, for telling its own answers from another's.
    me: Option<ClientId>,
    /// Diffs coloured once, by entry id and revision.
    diffs: std::cell::RefCell<Coloured>,
    /// The task card shows every task.
    tasks_open: bool,
    /// The face is in the tile: the list and its clock run only then.
    shown: bool,
    /// Ticks once a second while the agent works and the face shows (the elapsed time).
    clock: Option<Task<()>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ConversationView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationView")
            .field("session", &self.session)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<FaceEvent> for ConversationView {}

impl Focusable for ConversationView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

/// Now, in ms since the Unix epoch by this client's clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl ConversationView {
    /// An empty face for `session`, waiting for the worker's conversation.
    pub fn new(
        session: SessionId,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(MESSAGE_PLACEHOLDER)
                .auto_grow(1, COMPOSER_ROWS)
                .submit_on_enter(true)
        });
        let deny =
            cx.new(|cx| InputState::new(window, cx).placeholder("Tell Claude what to do instead"));
        let composing = cx.subscribe_in(&composer, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.submit(window, cx);
            }
        });
        let denying = cx.subscribe_in(&deny, window, |this, _input, event, _window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.deny(cx);
            }
        });
        let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            session,
            theme,
            zoom: 1.0,
            width: 0.0,
            model: Model::default(),
            agent: None,
            density: Density::default(),
            toggled: HashSet::new(),
            thread: ThreadId::Main,
            rows: Rc::from([]),
            keys: Vec::new(),
            list,
            composer,
            deny,
            deny_open: false,
            approvals: Approvals::default(),
            me: None,
            diffs: std::cell::RefCell::default(),
            tasks_open: false,
            shown: false,
            clock: None,
            focus: cx.focus_handle(),
            _subscriptions: vec![composing, denying],
        }
    }

    // ----- reading -----------------------------------------------------------------------

    /// The session this face is of.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// The conversation as received.
    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    /// The rows on show.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// How much of the work shows.
    #[must_use]
    pub const fn density(&self) -> Density {
        self.density
    }

    /// The thread on show.
    #[must_use]
    pub const fn thread(&self) -> &ThreadId {
        &self.thread
    }

    /// The list follows the tail.
    #[must_use]
    pub fn following(&self) -> bool {
        self.list.is_following_tail()
    }

    /// The row at the top of the list and how far into it the view is: the scroll anchor.
    #[must_use]
    pub fn anchor(&self) -> ListOffset {
        self.list.logical_scroll_top()
    }

    /// What the composer holds.
    #[must_use]
    pub fn draft(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// The held permission prompts.
    #[must_use]
    pub const fn approvals(&self) -> &Approvals {
        &self.approvals
    }

    /// The agent is on a turn: working, running a tool, or blocked on the person.
    #[must_use]
    pub fn working(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| match &a.status {
            AgentStatus::Working | AgentStatus::Tool { .. } => true,
            AgentStatus::Blocked(why) => *why != BlockReason::IdlePrompt,
            AgentStatus::None | AgentStatus::Idle | AgentStatus::Done => false,
        })
    }

    /// One line of what the agent is doing or last did, for the navigator and the overview:
    /// the call it is on while it works, else the first line of its last answer.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let main = self.model.thread(&ThreadId::Main)?;
        let tasks = main.tasks();
        if self.working() {
            let call = main
                .entries()
                .iter()
                .rev()
                .take_while(|e| !matches!(e.body, Body::Prompt(_)))
                .find_map(|e| match &e.body {
                    Body::Tool(call) => Some(call),
                    _ => None,
                });
            if let Some(call) = call {
                let title = super::tools::title(call, tasks);
                return Some(match title.subject {
                    Some(subject) => format!("{} {subject}", title.verb),
                    None => title.verb,
                });
            }
            return None;
        }
        main.entries().iter().rev().find_map(|e| match &e.body {
            Body::Text(text) => text.text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| {
                l.trim_start_matches(['#', '*', '>', '-', ' ']).trim_end_matches('*').to_owned()
            }),
            _ => None,
        })
    }

    // ----- what the workspace tells it -------------------------------------------------

    /// The worker's conversation stream.
    pub fn apply(&mut self, event: ConversationEvent, cx: &mut Context<Self>) {
        let expanded = matches!(event, ConversationEvent::Expanded { .. });
        let applied = self.model.apply(event);
        if expanded {
            self.list.remeasure();
        }
        if applied.threads || applied.live {
            if self.model.thread(&self.thread).is_none()
                && self.thread != ThreadId::Main
                && applied.current
            {
                self.thread = ThreadId::Main;
            }
            self.rebuild(cx);
        }
        if applied.meters || applied.current {
            cx.notify();
        }
    }

    /// A permission prompt for this session was asked or settled.
    pub fn permission(
        &mut self,
        event: PermissionEvent,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        match event {
            PermissionEvent::Asked(prompt) => {
                self.approvals.asked(*prompt);
                self.deny_open = false;
            }
            PermissionEvent::Settled { ask, outcome, .. } => {
                self.approvals.settled(ask, outcome, self.me);
                self.deny_open = false;
                if let Some(window) = window {
                    self.composer.update(cx, |c, cx| c.focus(window, cx));
                }
            }
        }
        cx.notify();
    }

    /// Following ended (the face hid, the link went): prompts held here are the worker's to
    /// release, and nothing streams until the next follow.
    pub fn unfollowed(&mut self, cx: &mut Context<Self>) {
        self.approvals.reset();
        self.deny_open = false;
        cx.notify();
    }

    /// This client's id on the session's worker.
    pub const fn set_me(&mut self, me: Option<ClientId>) {
        self.me = me;
    }

    /// The worker's word on the agent in the session.
    pub fn set_agent(&mut self, agent: Option<AgentEvent>, cx: &mut Context<Self>) {
        let was = self.working();
        let status_changed =
            self.agent.as_ref().map(|a| &a.status) != agent.as_ref().map(|a| &a.status);
        self.agent = agent;
        if status_changed {
            // A settled prompt's line stays until the agent moves on.
            if self.approvals.last().is_some()
                && !matches!(self.agent.as_ref().map(|a| &a.status), Some(AgentStatus::Blocked(_)))
                && self.approvals.last().is_some_and(|(_, o)| o.in_terminal())
            {
                self.approvals.clear_last();
            }
            if was != self.working() {
                self.rebuild(cx);
            }
            self.run_clock(cx);
            cx.notify();
        }
    }

    /// The theme changed.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            self.diffs.borrow_mut().clear();
            self.list.remeasure();
            cx.notify();
        }
    }

    /// Where the tile lays the face out: the chrome's zoom and the tile's width at rest.
    pub fn set_layout(&mut self, zoom: f32, width: f32, cx: &mut Context<Self>) {
        let split = |w: f32| w >= SPLIT_FROM;
        if (self.zoom - zoom).abs() > f32::EPSILON || split(self.width) != split(width) {
            self.zoom = zoom;
            self.width = width;
            self.list.remeasure();
            cx.notify();
        } else {
            self.width = width;
        }
    }

    /// The face came into its tile or left it.
    pub fn set_shown(&mut self, shown: bool, cx: &Context<Self>) {
        if self.shown != shown {
            self.shown = shown;
            self.run_clock(cx);
        }
    }

    /// Whether the face is in its tile.
    #[must_use]
    pub const fn shown(&self) -> bool {
        self.shown
    }

    /// Give the composer the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    /// The elapsed time of a turn ticks once a second, only while it is on screen and the
    /// agent works; pending messages that never showed up go with the same tick.
    fn run_clock(&mut self, cx: &Context<Self>) {
        if !(self.shown && (self.working() || !self.model.pending().is_empty())) {
            self.clock = None;
            return;
        }
        if self.clock.is_some() {
            return;
        }
        self.clock = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this
                    .update(cx, |this, cx| {
                        let bound = now_ms().saturating_sub(
                            u64::try_from(PENDING_FOR.as_millis()).unwrap_or(u64::MAX),
                        );
                        if this.model.expire_pending(bound) {
                            this.rebuild(cx);
                        }
                        cx.notify();
                        this.shown && (this.working() || !this.model.pending().is_empty())
                    })
                    .unwrap_or(false);
                if !going {
                    let _gone = this.update(cx, |this, _cx| this.clock = None);
                    return;
                }
            }
        }));
    }

    // ----- rows ------------------------------------------------------------------------

    /// Whether the thread on show has a turn in progress.
    fn live_turn(&self) -> bool {
        match &self.thread {
            ThreadId::Main => self.working() || self.model.live(&ThreadId::Main).next().is_some(),
            ThreadId::Agent(id) => self.subagent_running(id),
        }
    }

    /// Whether the subagent `id` still runs, as its call in the main thread says.
    fn subagent_running(&self, id: &str) -> bool {
        self.model.threads().flat_map(|(_, t)| t.entries()).any(|e| match &e.body {
            Body::Tool(call) => matches!(
                &call.detail,
                ToolDetail::Agent(a) if a.agent_id.as_deref() == Some(id) && a.status == AgentRun::Running
            ),
            _ => false,
        })
    }

    /// Build the rows again and splice what changed into the list.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let empty = super::model::Thread::default();
        let thread = self.model.thread(&self.thread).unwrap_or(&empty);
        let live: Vec<_> = self.model.live(&self.thread).collect();
        let pending: &[super::model::Pending] =
            if self.thread == ThreadId::Main { self.model.pending() } else { &[] };
        let rows = rows::build(Input {
            thread,
            id: &self.thread,
            live_turn: self.live_turn(),
            waiting: self.agent.as_ref().is_some_and(|a| {
                matches!(&a.status, AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt)
            }),
            density: self.density,
            toggled: &self.toggled,
            live: &live,
            pending,
        });
        let keys: Vec<(String, u64)> = rows.iter().map(|row| (row.key(), self.rev(row))).collect();
        self.splice(&keys);
        self.keys = keys;
        self.rows = Rc::from(rows);
        cx.notify();
    }

    /// A row's revision: it changes whenever what the row draws changes.
    fn rev(&self, row: &Row) -> u64 {
        let thread = self.model.thread(&self.thread);
        let entry_rev = |id: &str| thread.map_or(0, |t| t.rev(id));
        match row {
            Row::Prompt { id } => entry_rev(id),
            Row::Entry { id, level } => entry_rev(id).wrapping_mul(4).wrapping_add(*level as u64),
            Row::Fold { fold, open, .. } => u64::from(fold.steps)
                .wrapping_mul(31)
                .wrapping_add(fold.took_ms.unwrap_or(0))
                .wrapping_mul(2)
                .wrapping_add(u64::from(*open)),
            Row::Group { ids, open, .. } => ids
                .iter()
                .fold(u64::from(*open), |acc, id| acc.wrapping_mul(31).wrapping_add(entry_rev(id))),
            Row::Live { id } => self.model.live_block_at(id).map_or(0, |b| b.text.len() as u64),
            Row::Working | Row::Pending { .. } => 0,
        }
    }

    /// Splice `keys` into the list over what it holds: the rows before the first key that
    /// moved and after the last stay put (and so does the reader among them); rows that kept
    /// their key but not their revision are measured again.
    fn splice(&self, keys: &[(String, u64)]) {
        let old = &self.keys;
        let prefix = old.iter().zip(keys).take_while(|(a, b)| a.0 == b.0).count();
        let room = old.len().min(keys.len()).saturating_sub(prefix);
        let suffix = old
            .iter()
            .rev()
            .zip(keys.iter().rev())
            .take(room)
            .take_while(|(a, b)| a.0 == b.0)
            .count();
        let old_mid = prefix..old.len().saturating_sub(suffix);
        let new_mid = keys.len().saturating_sub(suffix).saturating_sub(prefix);
        if !old_mid.is_empty() || new_mid > 0 {
            self.list.splice(old_mid, new_mid);
        }
        let kept = (0..prefix).chain(keys.len().saturating_sub(suffix)..keys.len());
        for ix in kept {
            let old_ix = if ix < prefix {
                ix
            } else {
                ix.saturating_add(old.len()).saturating_sub(keys.len())
            };
            if old.get(old_ix).map(|k| k.1) != keys.get(ix).map(|k| k.1) {
                self.list.remeasure_items(ix..ix.saturating_add(1));
            }
        }
    }

    // ----- what the reader does -------------------------------------------------------

    /// Open or close the fold, group or call `key`.
    fn toggle(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
        self.rebuild(cx);
    }

    /// Show another thread: a subagent's, or back to the session's own.
    pub fn open_thread(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if self.thread != thread {
            self.thread = thread;
            self.keys.clear();
            self.list.reset(0);
            self.list.set_follow_mode(FollowMode::Tail);
            self.rebuild(cx);
        }
    }

    /// The next density.
    pub fn cycle_density(&mut self, cx: &mut Context<Self>) {
        self.density = self.density.next();
        self.toggled.clear();
        self.rebuild(cx);
    }

    fn on_cycle_density(&mut self, _: &CycleDensity, _window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_density(cx);
    }

    fn on_interrupt(&mut self, _: &Interrupt, _window: &mut Window, cx: &mut Context<Self>) {
        self.interrupt(cx);
    }

    /// Stop the agent's turn: only while it has one, since Esc at an idle prompt is Claude
    /// Code's way into its rewind menu.
    pub fn interrupt(&self, cx: &mut Context<Self>) {
        if self.working() {
            cx.emit(FaceEvent::Interrupt);
        }
    }

    /// Send the composer's text.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft(cx);
        if text.trim().is_empty() || self.approvals.prompt().is_some() {
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        self.model.sent(text.trim().to_owned(), self.working(), now_ms());
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
        self.rebuild(cx);
        self.run_clock(cx);
        cx.emit(FaceEvent::Submit(text));
    }

    /// Answer the held prompt.
    pub fn answer(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        if let Some((ask, verdict)) = self.approvals.answer(verdict) {
            cx.emit(FaceEvent::Answer { ask, verdict });
            cx.notify();
        }
    }

    /// Deny the held prompt with what the deny field says.
    fn deny(&mut self, cx: &mut Context<Self>) {
        let message = self.deny.read(cx).value().trim().to_owned();
        self.answer(Verdict::Deny { message, interrupt: false }, cx);
    }

    /// Ask the worker for a clipped text whole.
    fn expand(&self, reference: TextRef, cx: &mut Context<Self>) {
        cx.emit(FaceEvent::Expand { thread: self.thread.clone(), reference });
    }

    /// Scroll to the prompt before the top of the view (`-1`) or after it (`1`).
    pub fn step_prompt(&self, delta: i8, cx: &mut Context<Self>) {
        let top = self.list.logical_scroll_top();
        let prompts: Vec<usize> = rows::prompts(&self.rows).map(|(ix, _)| ix).collect();
        let at = top.item_ix;
        let target = if delta < 0 {
            let before_or_at = if top.offset_in_item > px(0.0) { at.saturating_add(1) } else { at };
            prompts.iter().rev().copied().find(|&ix| ix < before_or_at)
        } else {
            prompts.iter().copied().find(|&ix| ix > at)
        };
        if let Some(ix) = target {
            self.scroll_to_row(ix, cx);
        }
    }

    /// Put row `ix` at the top of the view.
    fn scroll_to_row(&self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        cx.notify();
    }

    /// Back to the newest row, following again.
    fn to_latest(&self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(FollowMode::Tail);
        cx.notify();
    }

    /// A diff coloured once per entry revision.
    fn diff_blocks(
        &self,
        id: &str,
        path: &str,
        patch: &slopty_proto::conversation::Patch,
    ) -> Rc<[Block]> {
        let rev = self.model.thread(&self.thread).map_or(0, |t| t.rev(id));
        let key = (id.to_owned(), rev);
        if let Some(blocks) = self.diffs.borrow().get(&key) {
            return Rc::clone(blocks);
        }
        let blocks = super::diff::blocks(path, patch);
        self.diffs.borrow_mut().insert(key, Rc::clone(&blocks));
        blocks
    }

    /// `v` points at the face's zoom.
    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }

    /// The mono family code is set in.
    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }
}

impl Render for ConversationView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let rows = self.list_region(window, cx);
        let bar = self.thread_bar(cx);
        let foot = self.foot(window, cx);
        div()
            .id("conversation")
            .debug_selector(|| "conversation".to_owned())
            .key_context(CTX)
            .track_focus(&self.focus)
            .role(gpui::accesskit::Role::Group)
            .aria_label("Conversation")
            .on_action(cx.listener(Self::on_cycle_density))
            .on_action(cx.listener(Self::on_interrupt))
            .on_action(cx.listener(|this, _: &crate::terminal::PrevPrompt, _w, cx| this.step_prompt(-1, cx)))
            .on_action(cx.listener(|this, _: &crate::terminal::NextPrompt, _w, cx| this.step_prompt(1, cx)))
            // Esc in the composer stops the agent's turn while it has one; otherwise it is the
            // field's own.
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, _w, cx| {
                if this.working() && this.approvals.prompt().is_none() {
                    this.interrupt(cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(hsla(theme.content()))
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.ui_size))
            .text_color(hsla(s.text))
            .children(bar)
            .child(rows)
            .child(foot)
    }
}

impl ConversationView {
    /// The list, with the prompt rail beside it and the way back to the newest row over it.
    fn list_region(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let empty = self.rows.is_empty();
        let region = div().relative().flex_1().min_h_0().w_full();
        if empty {
            let text = if self.model.is_current() { "Nothing here yet" } else { "" };
            let shown = self.model.is_current()
                || crate::screen::past_grace(
                    SharedString::from(format!("conversation-{}", self.session)),
                    window,
                    cx,
                );
            let text = if shown && !self.model.is_current() {
                "Reading the conversation…"
            } else {
                text
            };
            return region
                .flex()
                .items_center()
                .justify_center()
                .id("conversation-empty")
                .when(!text.is_empty(), |el| {
                    el.debug_selector(|| "conversation-empty".to_owned())
                        .role(gpui::accesskit::Role::Status)
                        .aria_label(text)
                        .text_size(self.z(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(text)
                })
                .into_any_element();
        }
        let items = list(
            self.list.clone(),
            cx.processor(|this, ix: usize, window, cx| this.render_row(ix, window, cx)),
        )
        .size_full();
        let rail = self.rail(cx);
        let latest = (!self.list.is_following_tail()).then(|| self.latest_pill(cx));
        region.child(items).children(rail).children(latest).into_any_element()
    }
}

#[cfg(test)]
mod tests;
