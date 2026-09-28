//! The face: one virtualized column of the conversation and the composer under it.
//!
//! Beside the column runs the prompt rail; over the composer sit the task card and the
//! background work, and the permission prompt takes the composer's shell. A picture opens
//! large over the face.
//!
//! The same list shows the session's changes on the header chip's click: every file the
//! session changed, over the diffs that changed it.
//!
//! The list is gpui's `ListState` in tail-follow, as Zed's agent panel keeps its thread: new
//! rows keep the reader at the bottom while they are there, and the strict band gpui keeps
//! (the view within a point of the end) is the only thing that re-arms the follow once they
//! scrolled away, so a row growing never yanks them back. Rows are rebuilt from the model on
//! every change and spliced into the list by key: rows that kept their key keep their place
//! and their measured height, and only rows whose content moved are measured again.

mod blocks;
mod entries;
mod media;
mod parts;
mod rail;
mod work;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    FollowMode, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, list, px,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use slopty_core::{ClientId, SessionId, WallMs};
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::conversation::{
    AgentRun, Body, ConversationEvent, LiveKind, PermissionEvent, TextRef, ThreadId, ToolDetail,
    Verdict,
};
use slopty_theme::Theme;

use super::approval::Approvals;
use super::diff::Block;
use super::figures::{self, FileChange};
use super::model::Model;
use super::rows::{self, Density, Input, Row, RowKey};
use super::{CTX, CycleDensity, Interrupt, MESSAGE_PLACEHOLDER, composer};
use crate::colors::hsla;

/// Diffs coloured once, by thread, entry id and revision.
type Coloured = HashMap<(ThreadId, String, u64), Rc<[Block]>>;

/// How far past the viewport the list lays rows out, as Zed's thread does: a fling shows
/// rows already measured.
const OVERDRAW: f32 = 2048.0;

/// The composer grows with its text up to this many rows, then scrolls.
const COMPOSER_ROWS: usize = 8;

/// How long a message the transcript never records (a local command) shows as pending.
const PENDING_FOR: Duration = Duration::from_secs(15);

/// How wide a tile has to be, at rest, for an edit's diff to show its sides beside each other.
pub const SPLIT_FROM: f32 = 960.0;

/// How long a copy button says it copied.
const COPIED_FOR: Duration = Duration::from_millis(1_500);

/// What the list shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pane {
    /// The thread on show.
    #[default]
    Conversation,
    /// Every file the session changed, over the edits that changed it.
    Changes,
}

/// The find bar over the list: its field, the entries that match, and the one on show.
struct Finder {
    field: gpui::Entity<InputState>,
    hits: Vec<String>,
    at: usize,
    _typing: [Subscription; 2],
}

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
    /// Upload this to the worker for the prompt; its chip is attachment `id`, which the
    /// workspace reports on ([`ConversationView::attachment_progress`],
    /// [`ConversationView::attachment_landed`], [`ConversationView::attachment_ended`]).
    Attach {
        /// The chip.
        id: u64,
        /// What goes up.
        what: Attach,
    },
    /// The human took attachment `id` off the draft: its upload stops. Its chip is already
    /// gone.
    Detach {
        /// The chip.
        id: u64,
    },
}

/// What an attachment is before it goes up.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Attach {
    /// A picture pasted into the composer: its bytes and the name it lands under.
    Picture {
        /// The name, [`composer::picture_name`].
        name: String,
        /// The encoded picture.
        bytes: Vec<u8>,
    },
    /// Files copied here, pasted into the composer.
    Files(Vec<std::path::PathBuf>),
}

/// The extension of a picture Claude Code reads, for a pasted picture in `format`; `None` for
/// one it does not.
const fn picture_extension(format: gpui::ImageFormat) -> Option<&'static str> {
    match format {
        gpui::ImageFormat::Png => Some("png"),
        gpui::ImageFormat::Jpeg => Some("jpg"),
        gpui::ImageFormat::Gif => Some("gif"),
        gpui::ImageFormat::Webp => Some("webp"),
        _ => None,
    }
}

/// A session's conversation face.
pub struct ConversationView {
    session: SessionId,
    theme: Theme,
    /// The theme, shared with what outlives a frame (a code block's corner, a hint), so a
    /// frame never copies it.
    shared: std::sync::Arc<Theme>,
    hint_theme: Rc<Theme>,
    /// The chrome's zoom (the overview's).
    zoom: f32,
    /// The tile's width at rest, in points: whether a diff splits.
    width: f32,
    model: Model,
    /// The worker's word on the agent: whether its turn is live, and since when.
    agent: Option<AgentEvent>,
    density: Density,
    /// Folds, groups and calls the reader opened or closed, by row key.
    toggled: HashSet<RowKey>,
    /// The thread on show: the session's own, or a subagent's.
    thread: ThreadId,
    rows: Rc<[Row]>,
    /// Each row's key and revision, as the list holds them.
    keys: Vec<(RowKey, u64)>,
    /// The prompt rail, and the prompts it was last given, by row and revision.
    rail: gpui::Entity<rail::Rail>,
    rail_drawn: Vec<(usize, u64)>,
    list: ListState,
    composer: gpui::Entity<TextareaState>,
    /// What was attached to the draft and is still uploading, a chip each.
    attachments: composer::Attachments,
    /// The window the face is in, to type a landed attachment's path into the composer.
    window: gpui::AnyWindowHandle,
    /// Why a denial is given, typed on the permission card.
    deny: gpui::Entity<InputState>,
    /// The deny field is open on the card.
    deny_open: bool,
    approvals: Approvals,
    /// This client's id on the worker, for telling its own answers from another's.
    me: Option<ClientId>,
    /// Diffs coloured once, by thread, entry id and revision.
    diffs: std::cell::RefCell<Coloured>,
    /// What the list shows.
    pane: Pane,
    /// The find bar, while it is open.
    find: Option<Finder>,
    /// The copy button that just copied, by key, and the timer that clears it.
    copied: Option<String>,
    copied_clear: Option<Task<()>>,
    /// The permission prompt's command shows whole.
    ask_all: bool,
    /// The task card shows every task.
    tasks_open: bool,
    /// The face is in the tile: the list and its clock run only then.
    shown: bool,
    /// The picture open large over the face.
    viewing: Option<slopty_proto::conversation::Image>,
    /// Pictures made ready to draw, by digest: made once, decoded by GPUI off the frame.
    pictures: std::cell::RefCell<HashMap<String, std::sync::Arc<gpui::Image>>>,
    /// Background work opened in the tray to its last lines, by call.
    tray_open: HashSet<String>,
    /// The tray lists all of its work, not only the first few.
    tray_all: bool,
    /// When the face last swapped the composer for a permission prompt or back, and which way:
    /// the shell cross-fades its contents.
    morph: Option<(u64, bool)>,
    /// Rows a fold just opened, by key, with the generation that names their reveal: they
    /// settle in, and leave this once they have.
    settling: HashMap<RowKey, u64>,
    settled_clear: Option<Task<()>>,
    /// Counts reveals and morphs, so each one animates from its start.
    generation: u64,
    /// How many times the rows were built again, and how many times only the live ones grew:
    /// the proof that a streamed word builds nothing.
    #[cfg(test)]
    rebuilt: usize,
    #[cfg(test)]
    grown: usize,
    /// The permission prompt on show and when, by this client's clock, it came: its fallback
    /// counts down from here, whatever the worker's clock says.
    held: Option<(u64, WallMs)>,
    /// Ticks once a second while the agent works and the face shows (the elapsed time).
    clock: Option<Task<()>>,
    focus: FocusHandle,
    /// Times this view was rendered rather than replayed from the view cache (tests).
    #[cfg(test)]
    renders: u32,
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

/// What an attachment's chip calls it: the picture's name, the one file's, or how many.
fn attachment_name(what: &Attach) -> String {
    match what {
        Attach::Picture { name, .. } => name.clone(),
        Attach::Files(paths) => match paths.as_slice() {
            [one] => one.file_name().map_or_else(
                || one.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            many => format!("{} files", many.len()),
        },
    }
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
        // The face is drawn from a cached view: whatever changes one of its fields (a key, a
        // caret blink, a selection) draws the face afresh.
        let watching = [
            cx.observe(&composer, |_, _, cx| cx.notify()),
            cx.observe(&deny, |_, _, cx| cx.notify()),
        ];
        let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let hint_theme = Rc::new(theme.clone());
        let face = cx.weak_entity();
        let rail = cx.new(|_| rail::Rail::new(face, Rc::clone(&hint_theme)));
        Self {
            session,
            shared: std::sync::Arc::new(theme.clone()),
            hint_theme,
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
            rail,
            rail_drawn: Vec::new(),
            list,
            composer,
            attachments: composer::Attachments::default(),
            window: window.window_handle(),
            deny,
            deny_open: false,
            approvals: Approvals::default(),
            me: None,
            diffs: std::cell::RefCell::default(),
            pane: Pane::Conversation,
            find: None,
            copied: None,
            copied_clear: None,
            ask_all: false,
            tasks_open: false,
            shown: false,
            viewing: None,
            pictures: std::cell::RefCell::default(),
            tray_open: HashSet::new(),
            tray_all: false,
            morph: None,
            settling: HashMap::new(),
            settled_clear: None,
            #[cfg(test)]
            rebuilt: 0,
            #[cfg(test)]
            grown: 0,
            generation: 0,
            held: None,
            clock: None,
            focus: cx.focus_handle(),
            #[cfg(test)]
            renders: 0,
            _subscriptions: [composing, denying].into_iter().chain(watching).collect(),
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

    /// What the list shows.
    #[must_use]
    pub const fn pane(&self) -> &Pane {
        &self.pane
    }

    /// The find bar's field, while it is open.
    fn find_field(&self) -> Option<&gpui::Entity<InputState>> {
        self.find.as_ref().map(|f| &f.field)
    }

    /// The find bar's matches and the one on show, while it is open.
    #[must_use]
    pub fn found(&self) -> Option<(usize, usize)> {
        self.find.as_ref().map(|f| (f.at, f.hits.len()))
    }

    /// Every file the session changed, in the order each was first changed, across its
    /// threads.
    #[must_use]
    pub fn session_files(&self) -> Vec<FileChange> {
        let mut entries: Vec<(&ThreadId, &slopty_proto::conversation::Entry)> = self
            .model
            .threads()
            .flat_map(|(id, t)| t.entries().iter().map(move |e| (id, e)))
            .collect();
        entries.sort_by_key(|(_, e)| e.at_ms);
        figures::files(entries)
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

    /// Times this view was rendered rather than replayed from the view cache.
    #[cfg(test)]
    pub const fn renders(&self) -> u32 {
        self.renders
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

    /// The transcript shows the agent mid-turn, whatever the hooks say: a block still streaming,
    /// or a tool call of the last turn with no result yet (and no interruption after it).
    #[must_use]
    pub fn mid_turn(&self) -> bool {
        if self.model.live(&ThreadId::Main).next().is_some() {
            return true;
        }
        let Some(main) = self.model.thread(&ThreadId::Main) else { return false };
        for entry in main.entries().iter().rev() {
            match &entry.body {
                Body::Prompt(_) | Body::Interrupted { .. } => return false,
                Body::Tool(call) if call.result.is_none() => return true,
                _ => {}
            }
        }
        false
    }

    /// What the session is about, for a tile the agent has not titled: its first prompt's
    /// first line, cut short.
    #[must_use]
    pub fn first_prompt(&self) -> Option<String> {
        figures::first_prompt(self.model.thread(&ThreadId::Main)?.entries())
    }

    /// One line of what the agent is doing or last did, for the navigator and the overview:
    /// the call it is on while it works (by the hooks or by the transcript, whichever knows),
    /// else the first line of its last answer. Mid-turn with no call yet, nothing: the last
    /// answer is not what it is doing.
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        if self.working() || self.mid_turn() {
            // A call still streaming its input is the newest thing it does and has no entry
            // yet: name it as its live row does.
            let live = self.model.live(&ThreadId::Main).find_map(|(_, block)| match &block.kind {
                LiveKind::Tool { name, .. } => {
                    let input = super::tools::preparing(&block.text);
                    Some(if input.is_empty() { name.clone() } else { format!("{name} {input}") })
                }
                LiveKind::Text | LiveKind::Thinking => None,
            });
            if live.is_some() {
                return live;
            }
            let main = self.model.thread(&ThreadId::Main)?;
            let call = main
                .entries()
                .iter()
                .rev()
                .take_while(|e| !matches!(e.body, Body::Prompt(_)))
                .find_map(|e| match &e.body {
                    Body::Tool(call) => Some(call),
                    _ => None,
                })?;
            let title = super::tools::title(call, main.tasks());
            return Some(match title.subject {
                Some(subject) => format!("{} {subject}", title.verb),
                None => title.verb,
            });
        }
        let main = self.model.thread(&ThreadId::Main)?;
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
        } else if applied.grew && self.pane == Pane::Conversation {
            self.grow_live(cx);
        }
        if applied.media {
            // A tail grew or a picture came: the rows that show them are measured again.
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
        let asked = self.approvals.prompt().is_some();
        match event {
            PermissionEvent::Asked(prompt) => {
                self.held = Some((prompt.ask, WallMs::now()));
                self.approvals.asked(*prompt);
                self.deny_open = false;
                self.ask_all = false;
            }
            PermissionEvent::Settled { ask, outcome, .. } => {
                self.approvals.settled(ask, outcome, self.me);
                self.deny_open = false;
                if let Some(window) = window {
                    self.composer.update(cx, |c, cx| c.focus(window, cx));
                }
            }
        }
        let asks = self.approvals.prompt().is_some();
        if asks != asked {
            self.generation = self.generation.wrapping_add(1);
            self.morph = Some((self.generation, asks));
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
            self.shared = std::sync::Arc::new(theme.clone());
            self.hint_theme = Rc::new(theme.clone());
            self.theme = theme;
            self.diffs.borrow_mut().clear();
            self.list.remeasure();
            self.list.remeasure();
            self.sync_rail(cx);
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
            self.sync_rail(cx);
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
    /// Whether something on show counts time: the turn's elapsed time, a pending message,
    /// background work's, a prompt's fallback.
    fn ticking(&self) -> bool {
        self.shown
            && (self.working()
                || !self.model.pending().is_empty()
                || self.background_running()
                || self.approvals.prompt().is_some())
    }

    fn run_clock(&mut self, cx: &Context<Self>) {
        if !self.ticking() {
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
                        let bound = WallMs::from_millis(WallMs::now().as_millis().saturating_sub(
                            u64::try_from(PENDING_FOR.as_millis()).unwrap_or(u64::MAX),
                        ));
                        if this.model.expire_pending(bound) {
                            this.rebuild(cx);
                        }
                        cx.notify();
                        this.ticking()
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
        #[cfg(test)]
        {
            self.rebuilt = self.rebuilt.saturating_add(1);
        }
        if self.pane == Pane::Changes {
            let rows = rows::changes(&self.session_files());
            let keys: Vec<(RowKey, u64)> =
                rows.iter().map(|row| (row.key(), self.rev(row))).collect();
            self.splice(&keys);
            self.keys = keys;
            self.rows = Rc::from(rows);
            self.sync_rail(cx);
            cx.notify();
            return;
        }
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
        let keys: Vec<(RowKey, u64)> = rows.iter().map(|row| (row.key(), self.rev(row))).collect();
        self.splice(&keys);
        self.keys = keys;
        self.rows = Rc::from(rows);
        self.sync_rail(cx);
        cx.notify();
    }

    /// Live blocks grew and none started or went: the rows are the ones the list holds, and only
    /// the live rows (at the foot, before what waits to be recorded) are measured again.
    fn grow_live(&mut self, cx: &mut Context<Self>) {
        let rows = Rc::clone(&self.rows);
        let foot = rows.iter().enumerate().rev().take_while(|(_, row)| {
            matches!(row, Row::Live { .. } | Row::Working | Row::Pending { .. })
        });
        let mut grew = false;
        for (ix, row) in foot {
            let Row::Live { .. } = row else { continue };
            let rev = self.rev(row);
            if let Some((_, drawn)) = self.keys.get_mut(ix)
                && *drawn != rev
            {
                *drawn = rev;
                self.list.remeasure_items(ix..ix.saturating_add(1));
                grew = true;
            }
        }
        if grew {
            #[cfg(test)]
            {
                self.grown = self.grown.saturating_add(1);
            }
            cx.notify();
        }
    }

    /// Give the rail the prompts in the rows, their words worked out again only when a prompt
    /// came, went, moved or changed; and the rows' count, the theme and the zoom it places them
    /// by. The rail draws again only when one of those changed.
    fn sync_rail(&mut self, cx: &mut Context<Self>) {
        let drawn: Vec<(usize, u64)> = rows::prompts(&self.rows)
            .map(|(ix, _)| (ix, self.keys.get(ix).map_or(0, |(_, rev)| *rev)))
            .collect();
        let ticks = (drawn != self.rail_drawn).then(|| {
            let thread = self.model.thread(&self.thread);
            rows::prompts(&self.rows)
                .map(|(ix, id)| {
                    let words = thread
                        .and_then(|t| t.entry(id))
                        .and_then(|e| match &e.body {
                            Body::Prompt(p) => Some(crate::kit::first_line(&p.text.text)),
                            _ => None,
                        })
                        .unwrap_or_default();
                    let hint = SharedString::from(words.chars().take(80).collect::<String>());
                    rail::Tick { ix, hint }
                })
                .collect::<Rc<[_]>>()
        });
        self.rail_drawn = drawn;
        let (total, theme, zoom) = (self.rows.len(), Rc::clone(&self.hint_theme), self.zoom);
        self.rail.update(cx, |rail, cx| rail.set(ticks, total, &theme, zoom, cx));
    }

    /// A row's revision: it changes whenever what the row draws changes.
    fn rev(&self, row: &Row) -> u64 {
        let thread = self.model.thread(&self.thread);
        let entry_rev = |id: &str| thread.map_or(0, |t| t.rev(id));
        match row {
            Row::Prompt { id } => entry_rev(id),
            Row::Answer { id, end } => entry_rev(id).wrapping_mul(2).wrapping_add(u64::from(*end)),
            Row::Entry { id, level } => {
                // A background command's row grows with what it prints.
                let printed = self.model.output(&self.thread, id).map_or(0, |o| o.bytes);
                entry_rev(id)
                    .wrapping_mul(4)
                    .wrapping_add(*level as u64)
                    .wrapping_mul(31)
                    .wrapping_add(printed)
            }
            Row::Fold { id, fold, open } => {
                let figures = thread.and_then(|t| t.turn(id)).map_or(0, |t| {
                    t.usage.output.wrapping_mul(31).wrapping_add(u64::from(t.requests))
                });
                u64::from(fold.steps)
                    .wrapping_mul(31)
                    .wrapping_add(fold.took_ms.unwrap_or(0))
                    .wrapping_mul(31)
                    .wrapping_add(figures)
                    .wrapping_mul(2)
                    .wrapping_add(u64::from(*open))
            }
            Row::Edit { thread, id } => self.model.thread(thread).map_or(0, |t| t.rev(id)),
            Row::Group { ids, open, .. } => ids
                .iter()
                .fold(u64::from(*open), |acc, id| acc.wrapping_mul(31).wrapping_add(entry_rev(id))),
            Row::Live { id } => {
                let written = self.model.live_block_at(id).map_or(0, |b| b.text.len() as u64);
                let open = self.toggled.contains(&rows::live_key(id));
                written.wrapping_mul(2).wrapping_add(u64::from(open))
            }
            // What a settled turn changed stays as it was once the turn settled.
            Row::Working | Row::Pending { .. } | Row::Changes { .. } | Row::File { .. } => 0,
        }
    }

    /// Splice `keys` into the list over what it holds: the rows before the first key that
    /// moved and after the last stay put (and so does the reader among them); rows that kept
    /// their key but not their revision are measured again.
    fn splice(&self, keys: &[(RowKey, u64)]) {
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

    /// Open or close the fold, group or call `key`. What a fold opens settles in.
    fn toggle(&mut self, key: RowKey, cx: &mut Context<Self>) {
        let opens_fold = key.is_fold() && !self.toggled.contains(&key);
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
        let before: HashSet<RowKey> = self.keys.iter().map(|(k, _)| *k).collect();
        self.rebuild(cx);
        if !opens_fold || !crate::kit::motion(cx) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.settling.extend(
            self.keys.iter().filter(|(k, _)| !before.contains(k)).map(|(k, _)| (*k, generation)),
        );
        let settle = crate::kit::Pace::Settle.duration();
        self.settled_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(settle).await;
            let _gone = this.update(cx, |this, _cx| this.settling.clear());
        }));
    }

    /// Open `image` large over the face, or close it.
    pub fn view_picture(
        &mut self,
        image: Option<slopty_proto::conversation::Image>,
        cx: &mut Context<Self>,
    ) {
        self.viewing = image;
        cx.notify();
    }

    /// The picture open large, if one is.
    #[must_use]
    pub const fn viewing(&self) -> Option<&slopty_proto::conversation::Image> {
        self.viewing.as_ref()
    }

    /// Ask the worker for the pictures row `ix` shows that this client has not got: a row's
    /// pictures are fetched when it is laid out (in view, or about to be), not with the
    /// conversation.
    fn want_pictures(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix) else { return };
        let (Row::Prompt { id } | Row::Entry { id, .. }) = row else { return };
        let images =
            match self.model.thread(&self.thread).and_then(|t| t.entry(id)).map(|e| &e.body) {
                Some(Body::Prompt(prompt)) => prompt.images.as_slice(),
                Some(Body::Tool(call)) => {
                    call.result.as_ref().map_or(&[][..], |r| r.images.as_slice())
                }
                _ => return,
            };
        // Drawn every frame the row is in view: a row with nothing to fetch costs a lookup.
        if images.iter().all(|image| self.model.picture(&image.digest).is_some()) {
            return;
        }
        let (thread, images) = (self.thread.clone(), images.to_vec());
        for image in images {
            if self.model.want(&thread, &image) {
                cx.emit(FaceEvent::Expand { thread: thread.clone(), reference: image.at });
            }
        }
    }

    /// Show another thread: a subagent's, or back to the session's own.
    pub fn open_thread(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if self.thread != thread || self.pane != Pane::Conversation {
            self.thread = thread;
            self.pane = Pane::Conversation;
            self.find = None;
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

    /// A paste into the composer: a picture with no text on the clipboard, or files copied here,
    /// are attached rather than pasted as text. Whether the paste was taken.
    pub fn paste_attachment(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        let mut files = Vec::new();
        let mut picture = None;
        for entry in item.entries() {
            match entry {
                gpui::ClipboardEntry::ExternalPaths(paths) => {
                    files.extend_from_slice(paths.paths());
                }
                gpui::ClipboardEntry::Image(image) => {
                    picture = picture.or_else(|| {
                        picture_extension(image.format).map(|ext| (ext, image.bytes.clone()))
                    });
                }
                gpui::ClipboardEntry::String(_) => {}
            }
        }
        if !files.is_empty() {
            self.attach(Attach::Files(files), cx);
            return true;
        }
        let texted = item.text().is_some_and(|t| !t.is_empty());
        match picture {
            Some((ext, bytes)) if !texted => {
                self.attach(Attach::Picture { name: composer::picture_name(ext), bytes }, cx);
                true
            }
            _ => false,
        }
    }

    /// Show `what`'s chip and ask the workspace to send it up.
    pub fn attach(&mut self, what: Attach, cx: &mut Context<Self>) {
        let id = self.attachments.add(attachment_name(&what));
        cx.emit(FaceEvent::Attach { id, what });
        cx.notify();
    }

    /// Show the chip of `what`, which the workspace sends up itself (a drop on the face), and
    /// say which it is.
    pub fn start_attachment(&mut self, what: &Attach) -> u64 {
        self.attachments.add(attachment_name(what))
    }

    /// The chips of what is still uploading.
    #[must_use]
    pub fn attachments(&self) -> &[composer::Attachment] {
        self.attachments.chips()
    }

    /// Attachment `id` is `fraction` of the way up.
    pub fn attachment_progress(&mut self, id: u64, fraction: f32, cx: &mut Context<Self>) {
        if self.attachments.progress(id, fraction) {
            cx.notify();
        }
    }

    /// Attachment `id` landed at `paths` on the worker: its chip goes, and the paths are typed
    /// at the composer's cursor, where Claude Code reads them as attached files.
    pub fn attachment_landed(&mut self, id: u64, paths: &[String], cx: &mut Context<Self>) {
        if !self.attachments.end(id) || paths.is_empty() {
            return;
        }
        cx.notify();
        let (composer, window, paths) = (self.composer.clone(), self.window, paths.to_vec());
        // After whatever update delivered this: typing into the field takes its window.
        cx.defer(move |cx| {
            let _gone = window.update(cx, |_root, window, cx| {
                composer.update(cx, |c, cx| {
                    let value = c.value();
                    let before = value.get(..c.cursor()).and_then(|b| b.chars().next_back());
                    c.insert(composer::typed_paths(before, &paths), window, cx);
                });
            });
        });
    }

    /// The human takes attachment `id` off the draft: its chip goes at once, and the workspace
    /// stops its upload, so nothing is typed into the composer when it would have landed.
    pub fn detach(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.attachments.end(id) {
            cx.emit(FaceEvent::Detach { id });
            cx.notify();
        }
    }

    /// Attachment `id` will not land (a failure, a cancel, the link gone): its chip goes.
    pub fn attachment_ended(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.attachments.end(id) {
            cx.notify();
        }
    }

    /// Send the composer's text.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.draft(cx);
        if text.trim().is_empty() || self.approvals.prompt().is_some() {
            return;
        }
        self.composer.update(cx, |c, cx| c.set_value("", window, cx));
        self.model.sent(text.trim().to_owned(), self.working(), WallMs::now());
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

    /// Ask the worker for a clipped text of `thread` whole.
    fn expand_in(thread: ThreadId, reference: TextRef, cx: &mut Context<Self>) {
        cx.emit(FaceEvent::Expand { thread, reference });
    }

    /// Show the session's changes, `at` a file's first edit when given; the conversation
    /// again when they show and no file is asked for.
    pub fn open_changes(&mut self, at: Option<&str>, cx: &mut Context<Self>) {
        if self.pane == Pane::Changes && at.is_none() {
            self.close_changes(cx);
            return;
        }
        if self.pane != Pane::Changes {
            self.pane = Pane::Changes;
            self.find = None;
            self.keys.clear();
            self.list.reset(0);
            self.list.set_follow_mode(FollowMode::Normal);
            self.rebuild(cx);
        }
        let row = at.and_then(|path| {
            self.rows.iter().position(|r| matches!(r, Row::File { path: p } if p == path))
        });
        self.scroll_to_row(row.unwrap_or(0), cx);
    }

    /// Back from the session's changes to the thread that showed before them.
    pub fn close_changes(&mut self, cx: &mut Context<Self>) {
        if self.pane == Pane::Changes {
            self.pane = Pane::Conversation;
            self.keys.clear();
            self.list.reset(0);
            self.list.set_follow_mode(FollowMode::Tail);
            self.rebuild(cx);
        }
    }

    /// Put `text` on the clipboard, and have the button `key` say so for a moment.
    fn copy(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key);
        self.copied_clear = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _gone = this.update(cx, |this, cx| {
                this.copied = None;
                this.copied_clear = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    // ----- finding ---------------------------------------------------------------------

    /// Open the find bar, or give it the keyboard again.
    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane != Pane::Conversation {
            return;
        }
        if let Some(find) = &self.find {
            find.field.update(cx, |f, cx| f.focus(window, cx));
            return;
        }
        let field =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find in the conversation"));
        let typing =
            cx.subscribe_in(&field, window, |this, _field, event, _window, cx| match event {
                InputEvent::Change => this.search(cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_find(if *shift { -1 } else { 1 }, cx);
                }
                _ => {}
            });
        let watching = cx.observe(&field, |_, _, cx| cx.notify());
        field.update(cx, |f, cx| f.focus(window, cx));
        self.find = Some(Finder { field, hits: Vec::new(), at: 0, _typing: [typing, watching] });
        cx.notify();
    }

    /// Close the find bar and give the composer the keyboard.
    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
            cx.notify();
        }
    }

    /// Match the find bar's words again, and go to the first match.
    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else { return };
        let query = find.field.read(cx).value().to_string();
        let thinking = self.density != Density::Normal;
        let hits = self
            .model
            .thread(&self.thread)
            .map(|t| super::find::matches(t, &query, thinking))
            .unwrap_or_default();
        if let Some(find) = &mut self.find {
            find.hits = hits;
            find.at = 0;
        }
        self.reveal(cx);
    }

    /// The next match (`1`) or the one before (`-1`), round.
    fn step_find(&mut self, delta: i8, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let count = find.hits.len();
        if count == 0 {
            return;
        }
        let next = find.at.saturating_add(1);
        find.at = if delta < 0 {
            find.at.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1))
        } else if next < count {
            next
        } else {
            0
        };
        self.reveal(cx);
    }

    /// Bring the match on show into view, opening the fold or group that hides it.
    fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.find.as_ref().and_then(|f| f.hits.get(f.at)).cloned() else {
            cx.notify();
            return;
        };
        if super::find::row_of(&self.rows, &id).is_none()
            && let Some(key) =
                self.model.thread(&self.thread).and_then(|t| super::find::fold_over(t, &id))
        {
            self.toggled.insert(key);
            self.rebuild(cx);
        }
        if let Some(ix) = super::find::row_of(&self.rows, &id) {
            self.list.set_follow_mode(FollowMode::Normal);
            self.scroll_to_row(ix, cx);
        }
        cx.notify();
    }

    /// The row the find bar is on.
    fn find_row(&self) -> Option<usize> {
        let find = self.find.as_ref()?;
        let id = find.hits.get(find.at)?;
        super::find::row_of(&self.rows, id)
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
        thread: &ThreadId,
        id: &str,
        path: &str,
        patch: &slopty_proto::conversation::Patch,
    ) -> Rc<[Block]> {
        let rev = self.model.thread(thread).map_or(0, |t| t.rev(id));
        let key = (thread.clone(), id.to_owned(), rev);
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
        #[cfg(test)]
        {
            self.renders = self.renders.saturating_add(1);
        }
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let rows = self.list_region(window, cx);
        let bar = self.thread_bar(cx);
        let foot = self.foot(cx);
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
            .on_action(cx.listener(|this, _: &crate::terminal::Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &crate::terminal::FindNext, _w, cx| this.step_find(1, cx)))
            .on_action(cx.listener(|this, _: &crate::terminal::FindPrev, _w, cx| this.step_find(-1, cx)))
            // Esc in the composer stops the agent's turn while it has one; otherwise it is the
            // field's own.
            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::Escape, window, cx| {
                if this.viewing.is_some() {
                    this.view_picture(None, cx);
                    cx.stop_propagation();
                } else if this.find.as_ref().is_some_and(|f| f.field.focus_handle(cx).is_focused(window)) {
                    this.close_find(window, cx);
                    cx.stop_propagation();
                } else if this.working() && this.approvals.prompt().is_none() {
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
            .relative()
            .children(bar)
            .child(rows)
            .children(foot)
            .children(self.picture_viewer(window, cx))
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
            cx.processor(|this, ix: usize, window, cx| {
                this.want_pictures(ix, cx);
                this.render_row(ix, window, cx)
            }),
        )
        .size_full();
        let conversation = self.pane == Pane::Conversation;
        let rail = (conversation && self.rail.read(cx).shown()).then(|| {
            let z = |v: f32| px(v * self.zoom);
            let spacing = theme.spacing;
            self.rail.clone().cached(
                gpui::StyleRefinement::default()
                    .absolute()
                    .top(z(spacing.md))
                    .bottom(z(spacing.md))
                    .right(z(spacing.xxs))
                    .w(z(spacing.md)),
            )
        });
        let latest = (conversation && !self.list.is_following_tail()).then(|| self.latest_pill(cx));
        let find = self.find_bar(cx);
        region
            .child(items)
            .children(self.list_fade())
            .children(rail)
            .children(latest)
            .children(find)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests;
