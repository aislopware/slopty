//! The thread view: one agent's thread drawn from this client's mirror of it, for any agent.
//!
//! A header names the thread and says where it is; under it runs one reading column, 680 pt at
//! most, prose at 15/1.6, each settled turn folded to one line over its answer and the live
//! turn whole; under the column sit the activity bar and the composer.
//!
//! Everything the person does goes through the hub's outbox and shows in the frame they did
//! it: a message as a bubble on its way, an answer flipping its card, a stop as "Stopping".
//! The list is gpui's `ListState` in tail-follow, its rows spliced in by key, so a row that
//! kept its key keeps its place and its measured height.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FollowMode, FontWeight, InteractiveElement as _, IntoElement, ListAlignment,
    ListState, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, list, px, relative,
};
use gpui_kit::component::input::{self, InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use slopty_client::threads::{Mirror, Sent};
use slopty_core::WallMs;
use slopty_proto::thread::detail::ExecStatus;
use slopty_proto::thread::wire::{Expanded, Intent};
use slopty_proto::thread::{
    AgentId, Clipped, Delivery, Effect, IntentId, Item, ItemBody, ItemId, Phase, Request, ThreadId,
    ThreadState, ToolCall, ToolDetail, ToolState, TurnId, TurnState, kind,
};
use slopty_theme::{Rgb, Theme, Typography, alpha};

use self::composing::Composing;
use super::activity::{Activity, Asked, Edit, STEP_DONE};
use super::hub::{HubEvent, ThreadHub};
use super::rows::{self, Fold, Input, Row};
use crate::colors::hsla;
use crate::conversation::composer::Attach;
use crate::conversation::diff::{self, Block};
use crate::conversation::lines::{self, Ink};
use crate::conversation::{CTX, CycleDensity, Interrupt};
use crate::icons::{IconName, IconSize, Status};
use crate::kit::{self, ButtonKind};

/// The widest the reading column grows, in points at zoom 1 (`design.md` §3).
pub const COLUMN: f32 = 680.0;

/// How far past the viewport the list lays rows out, as Zed's thread does.
const OVERDRAW: f32 = 2048.0;

/// The composer grows with its text up to this many rows, then scrolls.
const COMPOSER_ROWS: usize = 8;

/// The context ring shows only once this share of the window is in use (`design.md` §4.3).
const RING_FROM: f64 = 20.0;

/// A call's line: its least height and the square its mark sits in.
const TOOL_ROW: f32 = 24.0;

/// Lines of a call's output or a diff shown before "Show all".
const PEEK_LINES: usize = 12;

/// Markdown headings at the prose base: h1, h2, then the rest at the prose size.
const HEADINGS: [f32; 2] = [18.0, 16.0];

/// The gap between an answer's paragraphs, in points at the prose size.
const PARAGRAPH: f32 = 10.0;

/// The widest a message's bubble grows, as a share of the column.
const BUBBLE: f32 = 0.85;

/// A message longer than this many lines or characters shows its start first.
const BUBBLE_LINES: usize = 8;
const BUBBLE_CHARS: usize = 480;

mod composing;
mod pictures;
mod trail;

/// Diffs coloured once, by call.
type Coloured = HashMap<ItemId, Rc<[Block]>>;

/// What a thread view asks of the workspace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ThreadViewEvent {
    /// Show the agent's own terminal in the tile instead.
    ShowTerminal,
    /// Open the review of what the thread changed.
    Review {
        /// The thread.
        thread: ThreadId,
    },
    /// Upload this for the draft; its chip is attachment `id`, which the workspace reports on
    /// ([`ThreadView::attachment_progress`], [`ThreadView::attachment_landed`],
    /// [`ThreadView::attachment_ended`]).
    Attach {
        /// The chip.
        id: u64,
        /// What goes up.
        what: Attach,
    },
    /// The person took attachment `id` off the draft: its upload stops. Its chip is gone.
    Detach {
        /// The chip.
        id: u64,
    },
    /// Show the system's picker; the files picked are attached as a drop on the tile is.
    PickFiles,
    /// Ask the worker for the paths under `root` an `@` query matches; the answer comes to
    /// [`ThreadView::files_found`].
    FindFiles {
        /// The agent's directory.
        root: String,
        /// What follows the `@`.
        query: String,
    },
}

/// One thread, drawn.
pub struct ThreadView {
    hub: Entity<ThreadHub>,
    thread: ThreadId,
    theme: Theme,
    /// The theme, shared with what outlives a frame (a code block's corner).
    shared: Arc<Theme>,
    zoom: f32,
    /// The tile's width at rest, in points: whether a diff splits.
    width: f32,
    /// The view draws its own header; off where the tile's says the same.
    header: bool,
    rows: Rc<[Row]>,
    /// Each row's key and revision, as the list holds them.
    keys: Vec<(u64, u64)>,
    /// Where each row's items are in the thread's.
    spans: Vec<std::ops::Range<usize>>,
    list: ListState,
    composer: Entity<TextareaState>,
    /// Settled turns the reader opened.
    open: HashSet<TurnId>,
    /// Calls and reasoning the reader opened.
    items_open: HashSet<ItemId>,
    /// Clipped texts the reader asked to see whole.
    whole: HashSet<ItemId>,
    /// Which waiting request the bar shows, by its place among them.
    asked_at: usize,
    plan_open: bool,
    /// Diffs coloured once, by call.
    diffs: RefCell<Coloured>,
    /// Ticks once a second while the agent works (the elapsed time).
    clock: Option<Task<()>>,
    /// The composer's menus, attachments and the waiting message being changed.
    composing: Composing,
    /// The threads above a subagent's on show, the tile's own first.
    trail: Vec<trail::Above>,
    /// Pictures decoded once, by digest.
    pictures: RefCell<HashMap<String, Arc<gpui::Image>>>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ThreadView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadView")
            .field("thread", &self.thread)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ThreadViewEvent> for ThreadView {}

impl Focusable for ThreadView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl ThreadView {
    /// A view of `thread` from `hub`: drawn from the cache in its first frame when the cache
    /// kept it, and followed while it is open.
    pub fn new(
        hub: Entity<ThreadHub>,
        thread: ThreadId,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = hub
            .read(cx)
            .threads()
            .mirror(thread)
            .and_then(Mirror::state)
            .map_or_else(|| "Message the agent".to_owned(), |s| placeholder(&s.meta.agent));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(placeholder)
                .auto_grow(1, COMPOSER_ROWS)
                .submit_on_enter(true)
        });
        let composing =
            cx.subscribe_in(&composer, window, |this, _input, event, window, cx| match event {
                InputEvent::PressEnter { secondary, shift: false } => {
                    let delivery = if *secondary { Delivery::Queue } else { Delivery::Steer };
                    this.submit(delivery, window, cx);
                }
                InputEvent::Change => this.composer_changed(cx),
                _ => {}
            });
        let hearing = cx.subscribe(&hub, |this, _hub, event, cx| match event {
            HubEvent::Thread(t) if *t == this.thread => this.rebuild(cx),
            HubEvent::Table | HubEvent::Expanded(_) => cx.notify(),
            _ => {}
        });
        let watching = cx.observe(&composer, |_, _, cx| cx.notify());
        let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
        list.set_follow_mode(FollowMode::Tail);
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |event, _window, cx| {
            if event.visible_range.start == 0 && event.is_scrolled {
                let _gone = weak.update(cx, |this, cx| this.older(cx));
            }
        });
        cx.on_release(move |this, cx| {
            let shown: Vec<ThreadId> =
                this.trail.iter().map(trail::Above::thread).chain([this.thread]).collect();
            this.hub.update(cx, |hub, cx| {
                for thread in shown {
                    hub.close(thread, cx);
                }
            });
        })
        .detach();
        let mut view = Self {
            shared: Arc::new(theme.clone()),
            theme,
            hub,
            thread,
            zoom: 1.0,
            width: 0.0,
            header: true,
            rows: Rc::from([]),
            keys: Vec::new(),
            spans: Vec::new(),
            list,
            composer,
            open: HashSet::new(),
            items_open: HashSet::new(),
            whole: HashSet::new(),
            asked_at: 0,
            plan_open: false,
            diffs: RefCell::default(),
            clock: None,
            composing: Composing::default(),
            trail: Vec::new(),
            pictures: RefCell::default(),
            focus: cx.focus_handle(),
            _subscriptions: vec![composing, hearing, watching],
        };
        view.hub.update(cx, |hub, cx| hub.open(thread, cx));
        view.rebuild(cx);
        view
    }

    // ----- reading -----------------------------------------------------------------------

    /// The tile's own thread, whichever of its subagents' is on show.
    #[must_use]
    pub fn thread(&self) -> ThreadId {
        self.trail.first().map_or(self.thread, trail::Above::thread)
    }

    /// The thread on show: the tile's own, or a subagent's opened from it.
    #[must_use]
    pub const fn shown(&self) -> ThreadId {
        self.thread
    }

    /// The rows, as last built.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Each row's key and revision, as the list holds them.
    #[cfg(test)]
    pub(super) fn keys(&self) -> &[(u64, u64)] {
        &self.keys
    }

    /// Whether the list follows the newest row.
    #[must_use]
    pub fn following(&self) -> bool {
        self.list.is_following_tail()
    }

    /// What the composer holds.
    #[must_use]
    pub fn draft(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    /// Draw the header, or leave it to the tile.
    pub fn set_header(&mut self, header: bool, cx: &mut Context<Self>) {
        self.header = header;
        cx.notify();
    }

    /// Draw at the chrome's zoom `zoom`, in a tile `width` points wide at rest.
    pub fn set_layout(&mut self, zoom: f32, width: f32, cx: &mut Context<Self>) {
        let zoomed = (self.zoom - zoom).abs() > f32::EPSILON;
        let resized = (self.width - width).abs() > f32::EPSILON;
        self.zoom = zoom;
        self.width = width;
        if zoomed || resized {
            self.list.remeasure();
            cx.notify();
        }
    }

    /// The theme it draws in.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Draw in `theme`.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.shared = Arc::new(theme.clone());
        self.theme = theme;
        self.diffs.borrow_mut().clear();
        self.list.remeasure();
        cx.notify();
    }

    /// Give the composer the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    fn state<'a>(&self, cx: &'a App) -> Option<&'a ThreadState> {
        self.hub.read(cx).threads().mirror(self.thread).and_then(Mirror::state)
    }

    /// The item `id`, which row `ix` draws: found where the rows were built, else looked for.
    fn item<'a>(&self, ix: usize, id: &ItemId, cx: &'a App) -> Option<&'a Item> {
        let state = self.state(cx)?;
        let at = self.spans.get(ix).map(|span| span.start);
        at.and_then(|at| state.items.get(at))
            .filter(|item| item.id == *id)
            .or_else(|| state.item(id))
    }

    /// The items of turn `turn`, which row `ix` folds.
    fn turn_items<'a>(&self, ix: usize, turn: TurnId, cx: &'a App) -> &'a [Item] {
        let Some(state) = self.state(cx) else { return &[] };
        let span = self.spans.get(ix).cloned().unwrap_or_default();
        match state.items.get(span) {
            Some(items) if items.iter().all(|i| i.turn == turn) && !items.is_empty() => items,
            _ => {
                let start = state.items.iter().position(|i| i.turn == turn).unwrap_or(0);
                let len = state
                    .items
                    .get(start..)
                    .map_or(0, |rest| rest.iter().take_while(|i| i.turn == turn).count());
                state.items.get(start..start.saturating_add(len)).unwrap_or_default()
            }
        }
    }

    fn working(&self, cx: &App) -> bool {
        self.state(cx).and_then(rows::under_way).is_some()
    }

    // ----- rows ------------------------------------------------------------------------

    /// Build the rows from the mirror again and splice them into the list by key.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let hub = self.hub.read(cx);
        let threads = hub.threads();
        let mirror = threads.mirror(self.thread);
        let (built, keys) = match mirror.and_then(|m| m.state().map(|s| (m, s))) {
            Some((mirror, state)) => {
                let unshown: Vec<&Sent> = threads.unshown(self.thread).collect();
                let built = rows::build_spans(Input { state, unshown: &unshown, open: &self.open });
                let keys = built
                    .rows
                    .iter()
                    .map(|row| (row.key(), self.rev(row, mirror, state, &unshown)))
                    .collect();
                (built, keys)
            }
            None => (rows::Built::default(), Vec::new()),
        };
        splice(&self.list, &self.keys, &keys);
        self.rows = built.rows.into();
        self.spans = built.spans;
        self.keys = keys;
        self.run_clock(cx);
        cx.notify();
    }

    /// What says a row must be measured again: its items' revisions and what the reader
    /// opened of it.
    fn rev(&self, row: &Row, mirror: &Mirror, state: &ThreadState, unshown: &[&Sent]) -> u64 {
        let flags = |item: &ItemId| {
            u64::from(self.items_open.contains(item)) | (u64::from(self.whole.contains(item)) << 1)
        };
        match row {
            Row::User { item }
            | Row::Text { item }
            | Row::Reasoning { item }
            | Row::Tool { item }
            | Row::Note { item } => mirror.rev(item).wrapping_mul(4) | flags(item),
            Row::Fold { turn, open } => {
                let ended = state.turn(*turn).and_then(|t| t.ended_ms).map_or(0, WallMs::as_millis);
                ended.wrapping_mul(2) | u64::from(*open)
            }
            Row::Working { .. } => 0,
            Row::Sending { intent } => unshown
                .iter()
                .find(|s| s.id == *intent)
                .map_or(0, |s| u64::from(s.outcome.is_some()) | (u64::from(s.failed()) << 1)),
        }
    }

    fn run_clock(&mut self, cx: &Context<Self>) {
        if !self.working(cx) {
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
                        cx.notify();
                        this.working(cx)
                    })
                    .unwrap_or(false);
                if !going {
                    let _gone = this.update(cx, |this, _cx| this.clock = None);
                    return;
                }
            }
        }));
    }

    fn older(&self, cx: &mut Context<Self>) {
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| hub.page(thread, cx));
    }

    // ----- what the person does --------------------------------------------------------

    fn intent(&self, intent: Intent, cx: &mut Context<Self>) -> IntentId {
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| hub.intent(thread, intent, cx))
    }

    /// Send the draft, led by what was attached: now, into the turn under way (↵), or once it
    /// ends (⌘↵). While a waiting message is being changed, either sends the change.
    fn submit(&mut self, delivery: Delivery, window: &mut Window, cx: &mut Context<Self>) {
        if self.composing.editing() {
            self.save_edit(window, cx);
            return;
        }
        let Some(text) = self.take_message(cx) else { return };
        let _id = self.intent(Intent::Send { text, delivery }, cx);
        self.composer.update(cx, |c, cx| c.clean(window, cx));
        self.list.scroll_to_end();
    }

    /// A key the composer's field would take: `take` has it first, while the field has the
    /// keyboard and no input method composes, and the field never sees it once taken.
    fn menu_key(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        take: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) -> bool,
    ) {
        if !self.composing(cx) && self.composer_focused(window, cx) && take(self, window, cx) {
            cx.stop_propagation();
        }
    }

    /// Esc in the composer with nothing else to close: the turn under way stops. Whether one
    /// was under way.
    fn stop_by_key(&self, cx: &mut Context<Self>) -> bool {
        let working = self.working(cx);
        self.interrupt(cx);
        working
    }

    /// Stop the turn under way.
    fn interrupt(&self, cx: &mut Context<Self>) {
        let stopping = self.hub.read(cx).threads().stopping(self.thread);
        if self.working(cx) && !stopping {
            let _id = self.intent(Intent::Interrupt, cx);
        }
    }

    fn answer(&self, ask: slopty_proto::thread::AskId, choice: String, cx: &mut Context<Self>) {
        let _id = self.intent(Intent::Answer { ask, choice, message: None }, cx);
    }

    /// ⌃O: every settled turn opens and every step with it, the turn under way's too; all
    /// open, they fold again.
    fn every_step(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state(cx) else { return };
        let settled: Vec<TurnId> = state
            .turns
            .iter()
            .filter(|t| !matches!(t.state, TurnState::Active))
            .map(|t| t.id)
            .collect();
        let steps: Vec<ItemId> = state
            .items
            .iter()
            .filter(|i| matches!(i.body, ItemBody::Reasoning(_) | ItemBody::Tool(_)))
            .map(|i| i.id.clone())
            .collect();
        let all_open = settled.iter().all(|t| self.open.contains(t))
            && steps.iter().all(|i| self.items_open.contains(i));
        if all_open {
            self.open.clear();
            self.items_open.clear();
        } else {
            self.open.extend(settled);
            self.items_open.extend(steps);
        }
        self.rebuild(cx);
    }

    fn toggle_turn(&mut self, turn: TurnId, cx: &mut Context<Self>) {
        if !self.open.remove(&turn) {
            self.open.insert(turn);
        }
        self.rebuild(cx);
    }

    fn toggle_item(&mut self, item: ItemId, cx: &mut Context<Self>) {
        if !self.items_open.remove(&item) {
            self.items_open.insert(item);
        }
        self.rebuild(cx);
    }

    fn show_whole(&mut self, item: ItemId, cx: &mut Context<Self>) {
        self.whole.insert(item);
        self.rebuild(cx);
    }

    fn dismiss(&self, id: IntentId, cx: &mut Context<Self>) {
        self.hub.update(cx, |hub, cx| hub.dismiss(id, cx));
    }

    fn retry(&self, sent: &Sent, cx: &mut Context<Self>) {
        let (id, intent) = (sent.id, sent.intent.clone());
        self.dismiss(id, cx);
        let _id = self.intent(intent, cx);
    }

    fn step_asked(&mut self, by: isize, waiting: usize, cx: &mut Context<Self>) {
        if waiting > 0 {
            let at = self.asked_at.min(waiting.saturating_sub(1));
            self.asked_at = at.saturating_add_signed(by).min(waiting.saturating_sub(1));
            cx.notify();
        }
    }

    // ----- drawing: pieces -------------------------------------------------------------

    fn icon(&self, name: IconName, tone: Rgb) -> AnyElement {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(self.z(self.theme.typography.icon()))
            .into_any_element()
    }

    /// The square every mark sits in.
    fn slot(&self) -> Div {
        div().flex_none().size(self.z(TOOL_ROW)).flex().items_center().justify_center()
    }

    fn spinner(&self, calm: bool) -> AnyElement {
        let status = if calm { Status::Running } else { Status::Working };
        crate::icons::status_icon(
            &self.theme,
            status,
            self.z(self.theme.typography.icon()),
            hsla(self.theme.surfaces.text_muted),
        )
    }

    /// A text button at the chrome's zoom, for words that come from the agent.
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        kind: ButtonKind,
    ) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (id, label) = (id.into(), label.into());
        let selector = id.to_string();
        let el = div()
            .id(ElementId::Name(id))
            .debug_selector(move || selector)
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.xs))
            .border_1()
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
            .cursor_pointer()
            .child(label);
        let el = match kind {
            ButtonKind::Primary => el
                .border_color(hsla(s.accent_fill))
                .bg(hsla(s.accent_fill))
                .text_color(hsla(s.accent_ink)),
            ButtonKind::Secondary => el
                .border_color(hsla(s.border))
                .bg(hsla(s.elevated))
                .text_color(hsla(s.text))
                .hover(move |el| el.bg(hsla(s.raised))),
            ButtonKind::Ghost | ButtonKind::Link => el
                .border_color(gpui::transparent_black())
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text))),
        };
        crate::a11y::tab_stop(el, s.accent)
    }

    /// A bare icon button: its glyph brightens under the pointer.
    fn icon_button(
        &self,
        id: impl Into<SharedString>,
        icon: IconName,
        label: &'static str,
    ) -> gpui::Stateful<Div> {
        let s = self.theme.surfaces;
        let id = id.into();
        let selector = id.to_string();
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(id))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .size(self.z(kit::icon_button_side(&self.theme)))
                .flex()
                .items_center()
                .justify_center()
                .rounded(self.z(self.theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |el| el.text_color(hsla(s.text)))
                .child(
                    crate::icons::icon(&self.theme, icon, IconSize::Inline, hsla(s.text_muted))
                        .size(self.z(self.theme.typography.icon())),
                ),
            s.accent,
        )
    }

    /// The prose look: the theme's Markdown at the prose size, headings 18, 16, then the
    /// prose size at the strong weight, paragraphs 10 apart, code in the mono face.
    fn prose_style(&self) -> TextViewStyle {
        let theme = &self.theme;
        let z = self.zoom;
        let base = theme.typography.prose();
        let mono = self.mono();
        let mut style = crate::markdown::style(theme, &mono, z);
        style.paragraph_gap = gpui::rems(PARAGRAPH / base);
        style.heading_base_font_size = px(base * z);
        style.heading_font_size = Some(Arc::new(move |level: u8, _base| {
            let size = match level {
                1 => HEADINGS[0],
                2 => HEADINGS[1],
                _ => base,
            };
            px(size * z)
        }));
        style.code_block = gpui::StyleRefinement::default()
            .font_family(mono.to_string())
            .text_size(px(theme.typography.small() * z))
            .bg(hsla(theme.surfaces.raised))
            .rounded(px(theme.radii.md * z))
            .px(px(theme.spacing.md * z))
            .py(px(theme.spacing.sm * z));
        style
    }

    fn markdown(&self, id: String, text: &str) -> AnyElement {
        let theme = Arc::clone(&self.shared);
        let zoom = self.zoom;
        TextView::markdown(ElementId::Name(id.into()), SharedString::from(text.to_owned()))
            .style(self.prose_style())
            .selectable(true)
            .code_block_actions(move |block, _window, _cx| {
                crate::conversation::view::code_actions(&theme, zoom, block)
            })
            .into_any_element()
    }

    /// The text of `clipped` to show: all of it once it came whole and the reader asked.
    fn text_of(&self, item: &ItemId, clipped: &Clipped, cx: &mut Context<Self>) -> (String, bool) {
        let Some(full) = clipped.full.clone() else { return (clipped.text.clone(), false) };
        if !self.whole.contains(item) {
            return (clipped.text.clone(), true);
        }
        let thread = self.thread;
        let came = self.hub.update(cx, |hub, cx| hub.expanded(thread, &full, cx));
        match came.as_deref() {
            Some(Expanded::Text(text)) => (text.clone(), false),
            _ => (clipped.text.clone(), false),
        }
    }

    /// "Show all 240 lines", under a clipped text.
    fn show_all(&self, item: &ItemId, clipped: &Clipped, cx: &Context<Self>) -> AnyElement {
        let id = item.clone();
        let s = self.theme.surfaces;
        div()
            .id(ElementId::Name(format!("whole-{}", item.0).into()))
            .role(Role::Button)
            .text_size(self.z(self.theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .hover(move |el| el.text_color(hsla(s.text)))
            .child(SharedString::from(format!("Show all {} lines", clipped.lines)))
            .on_click(cx.listener(move |this, _ev, _w, cx| this.show_whole(id.clone(), cx)))
            .into_any_element()
    }

    /// The column every row sits in: centred, 680 pt at most.
    fn column(&self, child: impl IntoElement) -> Div {
        let spacing = self.theme.spacing;
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(div().w_full().max_w(self.z(COLUMN)).px(self.z(spacing.lg)).child(child))
    }

    // ----- drawing: rows ---------------------------------------------------------------

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).cloned() else { return div().into_any_element() };
        let spacing = self.theme.spacing;
        let first = ix == 0;
        let (inner, gap) = match &row {
            Row::User { item } => (self.user_row(ix, item, cx), spacing.lg),
            Row::Text { item } => (self.text_row(ix, item, cx), spacing.md),
            Row::Reasoning { item } => (self.reasoning_row(ix, item, cx), spacing.xs),
            Row::Tool { item } => (self.tool_row(ix, item, cx), spacing.xxs),
            Row::Note { item } => (self.note_row(ix, item, cx), spacing.xs),
            Row::Fold { turn, open } => (self.fold_row(ix, *turn, *open, cx), spacing.sm),
            Row::Working { turn } => (self.working_row(*turn, cx), spacing.sm),
            Row::Sending { intent } => (self.sending_row(*intent, cx), spacing.lg),
        };
        self.column(inner).pt(self.z(if first { spacing.lg } else { gap })).into_any_element()
    }

    fn user_row(&self, ix: usize, id: &ItemId, cx: &mut Context<Self>) -> AnyElement {
        let Some(Item { body: ItemBody::User(message), .. }) = self.item(ix, id, cx) else {
            return div().into_any_element();
        };
        let words = match &message.command {
            Some(command) if message.text.text.trim().is_empty() => format!("/{command}"),
            _ => message.text.text.clone(),
        };
        let images = message.images.clone();
        let pictures = self.pictures_row(&images, true, cx);
        // A long message shows its start until the reader asks for the rest.
        let cut = (!self.whole.contains(id)).then(|| clamp(&words)).flatten();
        let more = cut.is_some().then(|| {
            let item = id.clone();
            let s = self.theme.surfaces;
            div()
                .id(ElementId::Name(format!("more-{}", id.0).into()))
                .role(Role::Button)
                .aria_label("Show more")
                .text_size(self.z(self.theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .cursor_pointer()
                .hover(move |el| el.text_color(hsla(s.text)))
                .child("Show more")
                .on_click(cx.listener(move |this, _ev, _w, cx| this.show_whole(item.clone(), cx)))
                .into_any_element()
        });
        let shown = cut.unwrap_or(words);
        self.bubble(format!("item-{}", id.0), shown, pictures, more, false)
    }

    /// What the person sent, on the raised surface at the column's right, its pictures over it.
    fn bubble(
        &self,
        id: String,
        words: String,
        pictures: Option<AnyElement>,
        under: Option<AnyElement>,
        faded: bool,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let label = SharedString::from(format!("You: {}", kit::first_line(&words)));
        div()
            .id(ElementId::Name(id.into()))
            .role(Role::Article)
            .aria_label(label)
            .w_full()
            .flex()
            .flex_col()
            .items_end()
            .gap(self.z(theme.spacing.xxs))
            .children(pictures)
            .child(
                div()
                    .max_w(relative(BUBBLE))
                    .px(self.z(theme.spacing.md))
                    .py(self.z(theme.spacing.sm))
                    .rounded(self.z(theme.radii.lg))
                    .bg(hsla(s.raised))
                    .text_size(self.z(theme.typography.prose()))
                    .line_height(relative(theme.typography.prose_line_height))
                    .text_color(hsla(s.text))
                    .whitespace_normal()
                    .when(faded, |el| el.opacity(alpha::STRONG))
                    .child(SharedString::from(words)),
            )
            .children(under)
            .into_any_element()
    }

    fn text_row(&self, ix: usize, id: &ItemId, cx: &mut Context<Self>) -> AnyElement {
        let Some(clipped) = self.item(ix, id, cx).and_then(|i| match &i.body {
            ItemBody::Text(text) => Some(text.clone()),
            _ => None,
        }) else {
            return div().into_any_element();
        };
        let (text, clipped_more) = self.text_of(id, &clipped, cx);
        let theme = &self.theme;
        let label = SharedString::from(kit::first_line(&text).to_owned());
        div()
            .id(ElementId::Name(format!("item-{}", id.0).into()))
            .role(Role::Article)
            .aria_label(label)
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.prose()))
            .line_height(relative(theme.typography.prose_line_height))
            .text_color(hsla(theme.surfaces.text))
            .child(self.markdown(format!("text-{}", id.0), &text))
            .when(clipped_more, |el| el.child(self.show_all(id, &clipped, cx)))
            .into_any_element()
    }

    fn reasoning_row(&self, ix: usize, id: &ItemId, cx: &Context<Self>) -> AnyElement {
        let Some(Item { body: ItemBody::Reasoning(text), .. }) = self.item(ix, id, cx) else {
            return div().into_any_element();
        };
        let open = self.items_open.contains(id);
        let s = self.theme.surfaces;
        let line = kit::first_line(&text.text).to_owned();
        let toggle = id.clone();
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id(ElementId::Name(format!("reasoning-{}", id.0).into()))
                    .role(Role::Button)
                    .aria_label("Thought")
                    .aria_expanded(open)
                    .flex()
                    .items_center()
                    .gap(self.z(self.theme.spacing.xs))
                    .min_h(self.z(TOOL_ROW))
                    .text_size(self.z(self.theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text_secondary)))
                    .child(self.slot().child(self.icon(IconName::Brain, s.text_muted)))
                    .child(div().flex_none().child("Thought"))
                    .when(!open, |el| {
                        el.child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(SharedString::from(line)),
                        )
                    })
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.toggle_item(toggle.clone(), cx);
                    })),
            )
            .when(open, |el| {
                el.child(
                    div()
                        .pl(self.z(TOOL_ROW + self.theme.spacing.xs))
                        .text_size(self.z(self.theme.typography.small()))
                        .text_color(hsla(s.text_secondary))
                        .whitespace_normal()
                        .child(SharedString::from(text.text.clone())),
                )
            })
            .into_any_element()
    }

    fn tool_row(&self, ix: usize, id: &ItemId, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.item(ix, id, cx).cloned() else { return div().into_any_element() };
        let ItemBody::Tool(call) = &item.body else { return div().into_any_element() };
        // A subagent's call opens its thread, once the table has it.
        let child = call.child.filter(|c| self.hub.read(cx).threads().rows().rows.contains_key(c));
        let theme = &self.theme;
        let s = theme.surfaces;
        let open = self.items_open.contains(id);
        let mark = match call.state {
            ToolState::Streaming | ToolState::Running => self.spinner(false),
            ToolState::Pending { .. } => self.spinner(true),
            _ => self.icon(tool_icon(&call.kind), s.text_muted),
        };
        let title = if call.title.is_empty() { call.name.clone() } else { call.title.clone() };
        let changes =
            patch_of(call).and_then(|p| kit::changes_at(theme, p.added, p.removed, self.zoom));
        let failed = matches!(call.state, ToolState::Failed | ToolState::Rejected);
        let took = call
            .ended_ms
            .filter(|end| *end > item.at_ms && !item.at_ms.is_zero())
            .map(|end| kit::duration(Duration::from_millis(end.millis_since(item.at_ms))));
        let waiting = matches!(call.state, ToolState::Pending { .. });
        let called = title.clone();
        let label = match (&call.state, child) {
            (ToolState::Streaming, _) => format!("Preparing {}", call.name),
            (_, Some(_)) => format!("Subagent {title}"),
            _ => title.clone(),
        };
        let toggle = id.clone();
        let line = div()
            .id(ElementId::Name(format!("tool-{}", id.0).into()))
            .debug_selector({
                let id = id.0.clone();
                move || format!("tool-{id}")
            })
            .role(Role::Button)
            .aria_label(SharedString::from(label))
            .when(child.is_none(), |el| el.aria_expanded(open))
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .text_size(self.z(theme.typography.small()))
            .cursor_pointer()
            .child(self.slot().child(mark))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text_secondary))
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(SharedString::from(title)),
            )
            .children(changes)
            .when(failed, |el| el.child(self.icon(IconName::X, s.text_muted)))
            .when(waiting, |el| {
                el.child(div().flex_none().text_color(hsla(s.text_muted)).child("Waiting for you"))
            })
            .child(div().flex_1())
            .children(took.map(|t| {
                kit::tabular(div())
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(t))
            }))
            .when_some(child, |el, _| el.child(self.icon(IconName::ChevronRight, s.text_muted)))
            .on_click(cx.listener(move |this, _ev, window, cx| match child {
                Some(child) => this.open_subagent(child, called.clone(), window, cx),
                None => this.toggle_item(toggle.clone(), cx),
            }));
        let body = open.then(|| self.tool_body(id, call)).flatten();
        let pictures = if open { self.pictures_row(&call.images, false, cx) } else { None };
        let indent = self.z(TOOL_ROW + self.theme.spacing.xs);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(self.theme.spacing.xxs))
            .child(line)
            .children(body)
            .children(pictures.map(|p| div().pl(indent).child(p)))
            .into_any_element()
    }

    /// What an opened call shows under its line: its diff, or its command and output.
    fn tool_body(&self, id: &ItemId, call: &ToolCall) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let indent = self.z(TOOL_ROW + theme.spacing.xs);
        if let Some((path, patch)) = path_patch(call) {
            // A call's patch is whole once it shows: the call is over before it opens.
            let blocks = Rc::clone(
                self.diffs
                    .borrow_mut()
                    .entry(id.clone())
                    .or_insert_with(|| diff::thread_blocks(path, patch)),
            );
            let ink = Ink { theme, zoom: self.zoom, digits: lines::digits(&blocks) };
            let mut shown = 0_usize;
            let mut children: Vec<AnyElement> = Vec::new();
            for (ix, block) in blocks.iter().enumerate() {
                if shown >= PEEK_LINES {
                    break;
                }
                if ix > 0 {
                    children.push(ink.hunk_head(block).into_any_element());
                }
                for line in block.lines.iter().take(PEEK_LINES.saturating_sub(shown)) {
                    children.push(ink.unified(line).into_any_element());
                    shown = shown.saturating_add(1);
                }
            }
            return Some(div().pl(indent).child(ink.frame().children(children)).into_any_element());
        }
        let command = match &call.detail {
            Some(ToolDetail::Exec(exec)) => Some(exec.command.text.clone()),
            _ => None,
        };
        let output = call.output.as_ref().map(|o| tail(&o.text, PEEK_LINES));
        if command.is_none() && output.is_none() {
            return None;
        }
        Some(
            div()
                .pl(indent)
                .child(
                    div()
                        .w_full()
                        .rounded(self.z(theme.radii.md))
                        .bg(hsla(s.raised))
                        .px(self.z(theme.spacing.md))
                        .py(self.z(theme.spacing.sm))
                        .font_family(self.mono())
                        .text_size(self.z(theme.typography.small()))
                        .flex()
                        .flex_col()
                        .gap(self.z(theme.spacing.xs))
                        .children(command.map(|c| {
                            div()
                                .text_color(hsla(s.text))
                                .child(SharedString::from(format!("$ {c}")))
                        }))
                        .children(output.map(|o| {
                            div()
                                .text_color(hsla(s.text_secondary))
                                .whitespace_normal()
                                .child(SharedString::from(o))
                        })),
                )
                .into_any_element(),
        )
    }

    fn note_row(&self, ix: usize, id: &ItemId, cx: &Context<Self>) -> AnyElement {
        let Some(item) = self.item(ix, id, cx) else { return div().into_any_element() };
        let s = self.theme.surfaces;
        let (icon, words) = match &item.body {
            ItemBody::Compaction(c) => (
                IconName::Scissors,
                match (c.before_tokens, c.after_tokens) {
                    (Some(b), Some(a)) => format!(
                        "Compacted the conversation from {} to {} tokens",
                        tokens(b),
                        tokens(a)
                    ),
                    _ => "Compacted the conversation".to_owned(),
                },
            ),
            ItemBody::Notice(n) => (IconName::Info, kit::first_line(&n.text.text).to_owned()),
            ItemBody::Review { .. } => (IconName::ListChecks, "Reviewed the changes".to_owned()),
            ItemBody::Extra { kind, .. } => (IconName::Info, sentence(kind)),
            _ => return div().into_any_element(),
        };
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(self.theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .text_size(self.z(self.theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.icon(icon, s.text_muted)))
            .child(div().min_w_0().whitespace_normal().child(SharedString::from(words)))
            .into_any_element()
    }

    fn fold_row(&self, ix: usize, turn: TurnId, open: bool, cx: &Context<Self>) -> AnyElement {
        let Some(state) = self.state(cx) else { return div().into_any_element() };
        let Some(figures) = state.turn(turn) else { return div().into_any_element() };
        let fold = Fold::of(figures, self.turn_items(ix, turn, cx));
        let theme = &self.theme;
        let s = theme.surfaces;
        let changes = kit::changes_at(theme, fold.added, fold.removed, self.zoom);
        let label = fold.line();
        div()
            .id(ElementId::Name(format!("fold-{}", turn.0).into()))
            .debug_selector(move || format!("fold-{}", turn.0))
            .role(Role::Button)
            .aria_label(SharedString::from(label.clone()))
            .aria_expanded(open)
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .hover(move |el| el.text_color(hsla(s.text_secondary)))
            .child(self.slot().child(self.icon(
                if open { IconName::ChevronDown } else { IconName::ChevronRight },
                s.text_muted,
            )))
            .child(
                kit::tabular(div())
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(label)),
            )
            .children(changes)
            .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_turn(turn, cx)))
            .into_any_element()
    }

    fn working_row(&self, turn: TurnId, cx: &Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let stopping = hub.threads().stopping(self.thread);
        let since = self.state(cx).and_then(|s| s.turn(turn)).map(|t| t.started_ms);
        let elapsed = since
            .filter(|t| !t.is_zero())
            .map(|t| kit::duration(Duration::from_secs(WallMs::now().millis_since(t) / 1_000)));
        let s = self.theme.surfaces;
        let words = if stopping { "Stopping" } else { "Working" };
        div()
            .id("thread-working")
            .debug_selector(|| "thread-working".to_owned())
            .role(Role::Status)
            .aria_label(words)
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(self.theme.spacing.xs))
            .min_h(self.z(TOOL_ROW))
            .text_size(self.z(self.theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.spinner(stopping)))
            .child(div().child(words))
            .children(elapsed.map(|e| kit::tabular(div()).child(SharedString::from(e))))
            .into_any_element()
    }

    fn sending_row(&self, intent: IntentId, cx: &Context<Self>) -> AnyElement {
        let hub = self.hub.read(cx);
        let Some(sent) = hub.threads().outbox().all().iter().find(|s| s.id == intent).cloned()
        else {
            return div().into_any_element();
        };
        let Intent::Send { text, .. } = &sent.intent else { return div().into_any_element() };
        let s = self.theme.surfaces;
        let under = match sent.failure() {
            Some(why) => {
                let (dismissed, retried) = (sent.id, sent.clone());
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(self.theme.spacing.sm))
                    .text_size(self.z(self.theme.typography.small()))
                    .child(div().text_color(hsla(s.error)).child(SharedString::from(why)))
                    .child(
                        self.button(format!("retry-{intent}"), "Try again", ButtonKind::Ghost)
                            .on_click(
                                cx.listener(move |this, _ev, _w, cx| this.retry(&retried, cx)),
                            ),
                    )
                    .child(
                        self.button(format!("dismiss-{intent}"), "Dismiss", ButtonKind::Ghost)
                            .on_click(
                                cx.listener(move |this, _ev, _w, cx| this.dismiss(dismissed, cx)),
                            ),
                    )
                    .into_any_element()
            }
            None => div()
                .text_size(self.z(self.theme.typography.meta()))
                .text_color(hsla(s.text_muted))
                .child(if hub.threads().linked() {
                    "Sending"
                } else {
                    "Sends when the worker is back"
                })
                .into_any_element(),
        };
        div()
            .debug_selector(move || format!("sending-{intent}"))
            .child(self.bubble(
                format!("sending-bubble-{}", sent.id),
                text.clone(),
                None,
                Some(under),
                sent.failure().is_none(),
            ))
            .into_any_element()
    }

    // ----- drawing: header, bar, composer ----------------------------------------------

    fn header_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.header {
            return None;
        }
        let hub = self.hub.read(cx);
        let theme = &self.theme;
        let s = theme.surfaces;
        let state = self.state(cx);
        let row = hub.threads().rows().rows.get(&self.thread);
        let title = state
            .map(|st| st.meta.title.clone())
            .or_else(|| row.map(|r| r.title.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "New thread".to_owned());
        let agent = state.map(|st| &st.meta.agent).or_else(|| row.map(|r| &r.agent));
        let phase = state.map(|st| &st.status).or_else(|| row.map(|r| &r.status));
        let status = if hub.threads().linked() {
            phase.and_then(|p| status_of(p.phase))
        } else {
            Some(Status::Away)
        };
        let wait = phase
            .and_then(|p| p.wait.as_ref())
            .map(|w| w.text.clone())
            .filter(|_| phase.is_some_and(|p| p.phase == Phase::NeedsYou));
        let used = state.and_then(|st| context_used(&st.meters)).filter(|u| *u >= RING_FROM);
        let k = self.zoom;
        Some(
            div()
                .id("thread-header")
                .debug_selector(|| "thread-header".to_owned())
                .role(Role::Banner)
                .aria_label(SharedString::from(title.clone()))
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.lg))
                .min_h(self.z(kit::Row::Two.height(theme)))
                .border_b_1()
                .border_color(hsla(s.border_subtle))
                .child(self.icon(agent_icon(agent), s.text_secondary))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(self.z(theme.typography.small()))
                                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                                .text_color(hsla(s.text))
                                .child(SharedString::from(title)),
                        )
                        .children(wait.map(|w| {
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(w))
                        })),
                )
                .children(status.map(|st| {
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.xxs))
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(st.tone(theme)))
                        .child(crate::icons::status_mark(theme, Some(st), k))
                        .child(st.label())
                }))
                .child(
                    kit::pill(theme, s.text_secondary, k)
                        .child(SharedString::from(hub.worker().to_owned())),
                )
                .children(used.map(|u| {
                    kit::tabular(div())
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(self.z(theme.spacing.xxs))
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(crate::conversation::view::context_ring(
                            theme,
                            u,
                            theme.typography.small() * k,
                        ))
                        .child(SharedString::from(format!("{u:.0}%")))
                }))
                .into_any_element(),
        )
    }

    fn activity_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let hub = self.hub.read(cx);
        let state = self.state(cx)?;
        let bar = Activity::of(hub.threads(), self.thread, state);
        if bar.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let mut sections: Vec<AnyElement> = Vec::new();
        for asked in bar.asked.iter().filter(|a| a.answered.is_some()) {
            sections.push(self.answered_line(asked));
        }
        let waiting: Vec<&Asked<'_>> = bar.waiting().collect();
        if let Some(current) = waiting.get(self.asked_at.min(waiting.len().saturating_sub(1))) {
            sections.push(self.request_card(
                current.request,
                self.asked_at.min(waiting.len().saturating_sub(1)),
                waiting.len(),
                cx,
            ));
        }
        if let Some(plan) = bar.plan {
            sections.push(self.plan_section(plan, cx));
        }
        if !bar.edited.is_empty() {
            sections.push(self.edited_section(&bar.edited, cx));
        }
        for queued in &bar.queue {
            sections.push(self.queued_line(queued, bar.can_withdraw, cx));
        }
        if !bar.background.is_empty() {
            sections.push(self.background_section(&bar.background));
        }
        for task in &bar.tasks {
            sections.push(self.task_line(task, bar.can_stop, cx));
        }
        let count = sections.len();
        Some(
            div()
                .id("thread-activity")
                .debug_selector(|| "thread-activity".to_owned())
                .role(Role::Group)
                .aria_label("Activity")
                .w_full()
                .flex()
                .flex_col()
                .rounded(self.z(theme.radii.lg))
                .border_1()
                .border_color(hsla(s.border_subtle))
                .bg(hsla(s.panel))
                .overflow_hidden()
                .children(sections.into_iter().enumerate().map(|(ix, section)| {
                    div()
                        .w_full()
                        .when(ix > 0 && ix < count, |el| {
                            el.border_t_1().border_color(hsla(s.border_subtle))
                        })
                        .child(section)
                }))
                .into_any_element(),
        )
    }

    fn section(&self) -> Div {
        let spacing = self.theme.spacing;
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(spacing.xs))
            .px(self.z(spacing.sm))
            .min_h(self.z(kit::Row::One.height(&self.theme)))
            .text_size(self.z(self.theme.typography.small()))
    }

    fn answered_line(&self, asked: &Asked<'_>) -> AnyElement {
        let s = self.theme.surfaces;
        let choice = asked.answered.and_then(|sent| match &sent.intent {
            Intent::Answer { choice, .. } => Some(choice.clone()),
            Intent::Release { .. } => Some("Answer in the terminal".to_owned()),
            _ => None,
        });
        let label = choice
            .and_then(|c| {
                asked
                    .request
                    .options
                    .iter()
                    .find(|o| o.id == c)
                    .map(|o| o.label.clone())
                    .or(Some(c))
            })
            .unwrap_or_default();
        let id = asked.request.id.0.clone();
        self.section()
            .debug_selector(move || format!("answered-{id}"))
            .text_color(hsla(s.text_muted))
            .child(self.slot().child(self.icon(IconName::Check, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(asked.request.title.clone())),
            )
            .child(div().flex_1())
            .child(div().flex_none().child(SharedString::from(label)))
            .into_any_element()
    }

    /// The request on show: what it asks, its answers, and "2 of 5" with the way to the
    /// others. Only a press answers it: no key does, so a stray one cannot.
    fn request_card(
        &self,
        request: &Request,
        at: usize,
        of: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let ask = request.id.clone();
        let mut answers: Vec<AnyElement> = Vec::new();
        let mut primary = true;
        for choice in &request.options {
            let kind = match choice.effect {
                Effect::Allow if primary => {
                    primary = false;
                    ButtonKind::Primary
                }
                _ => ButtonKind::Secondary,
            };
            let (ask, id) = (ask.clone(), choice.id.clone());
            answers.push(
                self.button(format!("answer-{}-{}", ask.0, choice.id), choice.label.clone(), kind)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.answer(ask.clone(), id.clone(), cx);
                    }))
                    .into_any_element(),
            );
        }
        if let Some(question) = request.questions.first().filter(|_| request.options.is_empty()) {
            for offered in &question.options {
                let (ask, label) = (ask.clone(), offered.label.clone());
                answers.push(
                    self.button(
                        format!("answer-{}-{}", ask.0, offered.label),
                        offered.label.clone(),
                        ButtonKind::Secondary,
                    )
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.answer(ask.clone(), label.clone(), cx);
                    }))
                    .into_any_element(),
                );
            }
        }
        let release = ask.clone();
        answers.push(
            self.button(format!("release-{}", ask.0), "Answer in the terminal", ButtonKind::Ghost)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    let _id = this.intent(Intent::Release { ask: release.clone() }, cx);
                }))
                .into_any_element(),
        );
        let text = request
            .text
            .as_ref()
            .map(|t| t.text.clone())
            .or_else(|| request.questions.first().map(|q| q.text.clone()));
        let stepper = (of > 1).then(|| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xxs))
                .child(
                    self.icon_button("asked-prev", IconName::ChevronUp, "Previous request")
                        .on_click(
                            cx.listener(move |this, _ev, _w, cx| this.step_asked(-1, of, cx)),
                        ),
                )
                .child(
                    kit::tabular(div())
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{} of {of}", at.saturating_add(1)))),
                )
                .child(
                    self.icon_button("asked-next", IconName::ChevronDown, "Next request")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.step_asked(1, of, cx))),
                )
        });
        let id = request.id.0.clone();
        div()
            .id(ElementId::Name(format!("request-{id}").into()))
            .debug_selector(move || format!("request-{id}"))
            .role(Role::Dialog)
            .aria_label(SharedString::from(request.title.clone()))
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .p(self.z(theme.spacing.md))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .child(self.icon(IconName::CircleAlert, s.warn))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .text_size(self.z(theme.typography.small()))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(s.text))
                            .child(SharedString::from(request.title.clone())),
                    )
                    .children(stepper),
            )
            .children(text.map(|t| {
                div()
                    .w_full()
                    .rounded(self.z(theme.radii.md))
                    .bg(hsla(s.raised))
                    .px(self.z(theme.spacing.sm))
                    .py(self.z(theme.spacing.xs))
                    .font_family(self.mono())
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text))
                    .whitespace_normal()
                    .child(SharedString::from(tail(&t, PEEK_LINES)))
            }))
            .child(div().flex().flex_wrap().gap(self.z(theme.spacing.xs)).children(answers))
            .into_any_element()
    }

    fn plan_section(&self, plan: &slopty_proto::thread::Plan, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let done = plan.steps.iter().filter(|st| st.status == STEP_DONE).count();
        let total = plan.steps.len();
        let open = self.plan_open;
        let head =
            self.section()
                .id("thread-plan")
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Plan, {done} of {total} done")))
                .aria_expanded(open)
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .child(self.slot().child(self.icon(IconName::ListTodo, s.text_muted)))
                .child(div().flex_none().child("Plan"))
                .child(
                    kit::tabular(div())
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(format!("{done} of {total} done"))),
                )
                .child(div().flex_1())
                .child(self.icon(
                    if open { IconName::ChevronDown } else { IconName::ChevronUp },
                    s.text_muted,
                ))
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.plan_open = !this.plan_open;
                    cx.notify();
                }));
        let steps = open.then(|| {
            div().w_full().flex().flex_col().pb(self.z(theme.spacing.xs)).children(
                plan.steps.iter().map(|step| {
                    let (icon, tone) = match step.status.as_str() {
                        STEP_DONE => (IconName::CircleCheck, s.text_muted),
                        "in_progress" => (IconName::CircleDot, s.text),
                        _ => (IconName::Circle, s.text_muted),
                    };
                    self.section()
                        .text_color(hsla(tone))
                        .child(self.slot().child(self.icon(icon, tone)))
                        .child(
                            div()
                                .min_w_0()
                                .whitespace_normal()
                                .child(SharedString::from(step.text.clone())),
                        )
                }),
            )
        });
        div().w_full().flex().flex_col().child(head).children(steps).into_any_element()
    }

    fn edited_section(&self, edited: &[super::activity::Edited], cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (added, removed) = edited.iter().fold((0_u32, 0_u32), |(a, r), e| {
            (a.saturating_add(e.added), r.saturating_add(e.removed))
        });
        let words = match edited {
            [one] => format!("Edited {}", one.path.rsplit('/').next().unwrap_or(&one.path)),
            many => format!("Edited {} files", many.len()),
        };
        let thread = self.thread;
        self.section()
            .id("thread-edited")
            .debug_selector(|| "thread-edited".to_owned())
            .role(Role::Group)
            .aria_label(SharedString::from(words.clone()))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(IconName::FilePen, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(words)),
            )
            .children(kit::changes_at(theme, added, removed, self.zoom))
            .child(div().flex_1())
            .child(self.button("thread-review", "Review", ButtonKind::Ghost).on_click(cx.listener(
                move |_this, _ev, _w, cx| {
                    cx.emit(ThreadViewEvent::Review { thread });
                },
            )))
            .into_any_element()
    }

    /// A waiting message: its first line, where it is, and the ways to change it or take it
    /// back while the worker holds it.
    fn queued_line(
        &self,
        queued: &super::activity::Queued,
        can_change: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let pending = queued.intent;
        let refused = match &queued.edit {
            Some(Edit::Refused { intent, text, reason }) => Some((*intent, text.clone(), reason)),
            Some(Edit::Sending) | None => None,
        };
        let state = match (&queued.held, queued.on_worker, queued.withdrawing, &refused) {
            (_, _, true, _) => Some("Taking back".to_owned()),
            (.., Some((_, _, reason))) => Some(format!("Not changed: {reason}")),
            (Some(why), ..) => Some(why.clone()),
            (None, false, ..) => Some("Sending".to_owned()),
            (None, true, false, None) => None,
        };
        let open = can_change && queued.on_worker && !queued.withdrawing;
        let editable = open && !queued.going && !self.composing.editing();
        // The words the composer takes: a refused change's, so they are not lost.
        let words = refused.as_ref().map_or_else(|| queued.text.clone(), |(_, t, _)| t.clone());
        self.section()
            .debug_selector(move || format!("queued-{pending}"))
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.icon(IconName::Clock, s.text_muted)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(kit::first_line(&queued.text).to_owned())),
            )
            .children(state.map(|st| {
                div()
                    .flex_none()
                    .max_w(relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(self.theme.typography.meta()))
                    .text_color(hsla(if refused.is_some() { s.warn } else { s.text_muted }))
                    .child(SharedString::from(st))
            }))
            .when(editable, |el| {
                el.child(
                    self.icon_button(format!("edit-{pending}"), IconName::Pencil, "Edit").on_click(
                        cx.listener(move |this, _ev, window, cx| {
                            this.start_edit(pending, &words, window, cx);
                        }),
                    ),
                )
            })
            .when_some(refused.map(|(intent, ..)| intent), |el, intent| {
                el.child(
                    self.icon_button(format!("edit-dismiss-{pending}"), IconName::X, "Dismiss")
                        .on_click(cx.listener(move |this, _ev, _w, cx| this.dismiss(intent, cx))),
                )
            })
            .when(open && queued.edit.is_none(), |el| {
                el.child(
                    self.icon_button(format!("withdraw-{pending}"), IconName::X, "Take back")
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::Withdraw { pending }, cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// The commands run in the background, each with how it stands and its last line.
    fn background_section(&self, background: &[super::activity::Background<'_>]) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let lines = background.iter().map(|bg| {
            let state = match bg.detail.status {
                ExecStatus::Running => "Running".to_owned(),
                ExecStatus::Done => bg.detail.duration_ms.map_or_else(
                    || "Done".to_owned(),
                    |ms| format!("Done \u{b7} {}", kit::duration(Duration::from_millis(ms))),
                ),
                ExecStatus::Failed => "Failed".to_owned(),
                ExecStatus::Interrupted => "Stopped".to_owned(),
            };
            let running = matches!(bg.detail.status, ExecStatus::Running);
            let last = bg.output.and_then(|o| o.lines().rev().find(|l| !l.trim().is_empty()));
            let label = SharedString::from(format!("{}: {state}", bg.title));
            self.section()
                .id(ElementId::Name(format!("background-{}", bg.item.0).into()))
                .debug_selector({
                    let id = bg.item.0.clone();
                    move || format!("background-{id}")
                })
                .role(Role::Status)
                .aria_label(label)
                .text_color(hsla(s.text_secondary))
                .child(self.slot().child(if running {
                    self.spinner(true)
                } else {
                    self.icon(IconName::Terminal, s.text_muted)
                }))
                .child(div().flex_none().child(SharedString::from(bg.title.to_owned())))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(self.mono())
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .children(last.map(|l| SharedString::from(l.trim().to_owned()))),
                )
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .text_size(self.z(theme.typography.meta()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(state)),
                )
        });
        div()
            .id("thread-background")
            .role(Role::List)
            .aria_label("In the background")
            .w_full()
            .flex()
            .flex_col()
            .children(lines)
            .into_any_element()
    }

    fn task_line(
        &self,
        task: &slopty_proto::thread::BackgroundTask,
        can_stop: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let s = self.theme.surfaces;
        let id = task.id.clone();
        let stopping = self.hub.read(cx).threads().unshown(self.thread).any(|sent| {
            matches!(&sent.intent, Intent::StopTask { task: t } if *t == id) && !sent.failed()
        });
        self.section()
            .text_color(hsla(s.text_secondary))
            .child(self.slot().child(self.spinner(true)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(task.title.clone())),
            )
            .when(stopping, |el| el.child(div().text_color(hsla(s.text_muted)).child("Stopping")))
            .when(can_stop && !stopping, |el| {
                el.child(
                    self.button(format!("stop-task-{id}"), "Stop", ButtonKind::Ghost).on_click(
                        cx.listener(move |this, _ev, _w, cx| {
                            let _id = this.intent(Intent::StopTask { task: id.clone() }, cx);
                        }),
                    ),
                )
            })
            .into_any_element()
    }

    /// The field, over it the menu, the chips of what is attached or the line saying a waiting
    /// message is being changed, and under it the way to attach, the model, and the way to
    /// stop the turn.
    fn composer_box(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let working = self.working(cx);
        let stopping = self.hub.read(cx).threads().stopping(self.thread);
        let model = self.state(cx).and_then(|st| st.meters.model.clone());
        let editing = self.composing.editing();
        let view = cx.weak_entity();
        div()
            .id("thread-composer")
            .debug_selector(|| "thread-composer".to_owned())
            .w_full()
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.lg))
            .border_1()
            .border_color(hsla(s.border))
            .bg(hsla(s.elevated))
            .text_size(self.z(theme.typography.prose()))
            .children(self.menu_section(cx))
            .children(self.editing_strip(cx))
            .children(self.attachment_chips(cx))
            .child(
                Textarea::new(&self.composer)
                    .appearance(false)
                    .bordered(false)
                    .aria_label("Message")
                    .on_paste(move |item, _window, cx| {
                        view.update(cx, |v, cx| v.paste_attachment(item, cx)).unwrap_or(false)
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.xs))
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .when(!editing, |el| {
                        el.child(
                            self.icon_button("thread-attach", IconName::Paperclip, "Attach files")
                                .on_click(cx.listener(|_this, _ev, _w, cx| {
                                    cx.emit(ThreadViewEvent::PickFiles);
                                })),
                        )
                    })
                    .children(model.map(|m| div().child(SharedString::from(m))))
                    .child(div().flex_1())
                    .when(working && !stopping, |el| {
                        el.child(
                            self.icon_button("thread-stop", IconName::Square, "Stop")
                                .on_click(cx.listener(|this, _ev, _w, cx| this.interrupt(cx))),
                        )
                    }),
            )
            .into_any_element()
    }

    fn list_region(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let region = div().relative().flex_1().min_h_0().w_full();
        if self.rows.is_empty() {
            let linked = self.hub.read(cx).threads().linked();
            let words = if self.state(cx).is_some() {
                "Nothing here yet"
            } else if linked {
                "Reading the thread…"
            } else {
                "The worker is out of reach"
            };
            return region
                .flex()
                .items_center()
                .justify_center()
                .id("thread-empty")
                .debug_selector(|| "thread-empty".to_owned())
                .role(Role::Status)
                .aria_label(words)
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(words)
                .into_any_element();
        }
        region
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix: usize, _window, cx| this.render_row(ix, cx)),
                )
                .size_full(),
            )
            .into_any_element()
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settle_edit(window, cx);
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let header = self.header_bar(cx);
        let trail = self.trail_bar(cx);
        let rows = self.list_region(cx);
        let bar = self.activity_bar(cx);
        // A subagent takes no messages: its thread is read, and answered from the bar.
        let composer = (!self.in_subagent()).then(|| self.composer_box(cx));
        div()
            .id("thread")
            .debug_selector(|| "thread".to_owned())
            .key_context(CTX)
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label("Thread")
            .on_action(cx.listener(|this, _: &Interrupt, window, cx| {
                if !this.leave_subagent(window, cx) {
                    this.interrupt(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CycleDensity, _w, cx| this.every_step(cx)))
            // The composer's menu and a change to a waiting message take the arrows, ↵, ⇥ and
            // Esc before the field does; Esc otherwise stops the turn under way. While an input
            // method composes, these keys are all its own.
            .capture_action(cx.listener(|this, _: &input::MoveUp, window, cx| {
                this.menu_key(window, cx, |this, _w, cx| this.menu_step(-1, cx));
            }))
            .capture_action(cx.listener(|this, _: &input::MoveDown, window, cx| {
                this.menu_key(window, cx, |this, _w, cx| this.menu_step(1, cx));
            }))
            .capture_action(cx.listener(|this, enter: &input::Enter, window, cx| {
                if !enter.shift && !enter.secondary {
                    this.menu_key(window, cx, Self::menu_enter);
                }
            }))
            .capture_action(cx.listener(|this, _: &input::IndentInline, window, cx| {
                this.menu_key(window, cx, Self::menu_enter);
            }))
            .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                this.menu_key(window, cx, |this, window, cx| {
                    this.menu_close(cx)
                        || this.cancel_edit(window, cx)
                        || this.leave_subagent(window, cx)
                        || this.stop_by_key(cx)
                });
            }))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(hsla(theme.content()))
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.ui_size))
            .text_color(hsla(s.text))
            .children(header)
            .children(trail)
            .child(rows)
            .child(
                self.column(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(self.z(theme.spacing.xs))
                        .pb(self.z(theme.spacing.md))
                        .children(bar)
                        .children(composer),
                ),
            )
    }
}

/// Splice `keys` into `list` over `old`: the rows before the first key that moved and after
/// the last stay put; rows that kept their key but not their revision are measured again.
fn splice(list: &ListState, old: &[(u64, u64)], keys: &[(u64, u64)]) {
    let prefix = old.iter().zip(keys).take_while(|(a, b)| a.0 == b.0).count();
    let room = old.len().min(keys.len()).saturating_sub(prefix);
    let suffix =
        old.iter().rev().zip(keys.iter().rev()).take(room).take_while(|(a, b)| a.0 == b.0).count();
    let old_mid = prefix..old.len().saturating_sub(suffix);
    let new_mid = keys.len().saturating_sub(suffix).saturating_sub(prefix);
    if !old_mid.is_empty() || new_mid > 0 {
        list.splice(old_mid, new_mid);
    }
    let kept = (0..prefix).chain(keys.len().saturating_sub(suffix)..keys.len());
    for ix in kept {
        let old_ix =
            if ix < prefix { ix } else { ix.saturating_add(old.len()).saturating_sub(keys.len()) };
        if old.get(old_ix).map(|k| k.1) != keys.get(ix).map(|k| k.1) {
            list.remeasure_items(ix..ix.saturating_add(1));
        }
    }
}

/// What the composer says before anything is typed, by the agent.
fn placeholder(agent: &AgentId) -> String {
    let name = match agent.0.as_str() {
        AgentId::CLAUDE_CODE => "Claude",
        AgentId::CODEX => "Codex",
        AgentId::PI => "pi",
        _ => "the agent",
    };
    format!("Message {name}")
}

/// The start of a message too long to show whole at first, cut at a word, with an ellipsis;
/// `None` for one short enough.
fn clamp(words: &str) -> Option<String> {
    let lines: Vec<&str> = words.lines().collect();
    let by_lines = lines.len() > BUBBLE_LINES;
    let by_chars = words.chars().count() > BUBBLE_CHARS;
    if !by_lines && !by_chars {
        return None;
    }
    let head = if by_lines {
        lines.get(..BUBBLE_LINES).unwrap_or_default().join("\n")
    } else {
        words.to_owned()
    };
    let head: String = head.chars().take(BUBBLE_CHARS).collect();
    let cut = head
        .rfind(char::is_whitespace)
        .filter(|_| by_chars)
        .map_or(head.as_str(), |at| head.get(..at).unwrap_or(&head));
    Some(format!("{}\u{2026}", cut.trim_end()))
}

/// The glyph of an agent's kind.
fn agent_icon(agent: Option<&AgentId>) -> IconName {
    match agent.map(|a| a.0.as_str()) {
        Some(AgentId::CLAUDE_CODE) => IconName::Asterisk,
        _ => IconName::Bot,
    }
}

/// The one vocabulary's word for a phase; none for a thread at rest.
const fn status_of(phase: Phase) -> Option<Status> {
    match phase {
        Phase::Working => Some(Status::Working),
        Phase::Waiting => Some(Status::Running),
        Phase::NeedsYou => Some(Status::NeedsYou),
        Phase::Failed => Some(Status::Failed),
        Phase::Idle | Phase::Done | Phase::Stopped => None,
    }
}

/// The share of the context window in use, in percent.
fn context_used(meters: &slopty_proto::thread::Meters) -> Option<f64> {
    let (used, window) = (meters.context_tokens?, meters.context_window?);
    #[expect(clippy::cast_precision_loss, reason = "a share on screen")]
    let share = used as f64 / window.max(1) as f64;
    Some(share * 100.0)
}

/// The icon of a call's kind.
fn tool_icon(kind: &str) -> IconName {
    match kind {
        kind::READ => IconName::FileText,
        kind::EDIT => IconName::FilePen,
        kind::WRITE => IconName::FilePlus,
        kind::EXEC => IconName::SquareTerminal,
        kind::SEARCH => IconName::Search,
        kind::FETCH | kind::WEB_SEARCH => IconName::Globe,
        kind::MCP => IconName::Plug,
        kind::AGENT => IconName::Bot,
        kind::QUESTION => IconName::MessageSquare,
        kind::PLAN => IconName::Map,
        kind::TASKS => IconName::ListTodo,
        _ => IconName::Wrench,
    }
}

fn path_patch(call: &ToolCall) -> Option<(&str, &slopty_proto::thread::Patch)> {
    match &call.detail {
        Some(ToolDetail::Edit(d)) => Some((&d.path, &d.patch)),
        Some(ToolDetail::Write(d)) => Some((&d.path, &d.patch)),
        _ => None,
    }
}

fn patch_of(call: &ToolCall) -> Option<&slopty_proto::thread::Patch> {
    path_patch(call).map(|(_, p)| p)
}

/// The last `lines` lines of `text`.
fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all.get(all.len().saturating_sub(lines)..).unwrap_or_default().join("\n")
}

/// A token count as a person reads it: "48k", "1.2M".
fn tokens(n: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a label, not arithmetic")]
    let f = n as f64;
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.0}k", f / 1_000.0),
        _ => format!("{:.1}M", f / 1_000_000.0),
    }
}

/// An open name ("skill-loaded") in sentence case ("Skill loaded").
fn sentence(name: &str) -> String {
    let words = name.replace(['-', '_'], " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}
