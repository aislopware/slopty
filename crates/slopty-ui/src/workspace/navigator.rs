//! The navigator: what is there and where it runs, down the whole left of the window.
//!
//! It runs from the window's top edge to its bottom, and its top row is the title bar's
//! height: the traffic lights sit in it on a Mac, then a field that filters every row below.
//! Two sections follow. *Needs you* appears only while an agent waits on the human.
//! *Workers* has a header per worker, disclosing its tiles. A header names the worker in the
//! strong weight after a server icon (crossed out while the worker is away), then on its right
//! edge how many tiles it has and its round trip, or a word for what is wrong, led by what its
//! tiles add up to while it is folded. The pointer brings out the chevron and "+" (a new shell
//! on that worker) in the readouts' place. Its tiles come in order of attention: what needs the
//! human, then what finished unseen, then what is working, then the rest, each class in
//! reading order. Each tile is two lines: a fixed leading slot (its kind at rest, its status
//! mark otherwise) and its title, then its directory, what its agent says or its last command,
//! its branch and its age, muted. A row flies the camera to what it names. Workspaces are the
//! title bar's tabs, not a section here.
//!
//! On a window wide enough it docks beside the rest of the frame, 248 pt by default, dragged
//! from 200 to 400 by the handle on its right edge; ⌘B shows or hides it, and both are kept
//! with the device's layout. On an iPad it opens over the frame, and on a phone it slides in
//! as a drawer over a scrim; either closes once a row is chosen.

use std::collections::HashSet;
use std::time::SystemTime;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Div, ElementId, Entity, InteractiveElement as _, IntoElement as _,
    MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, canvas, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use slopty_client::layout::{Navigator, TileRef, WorkerKey};
use slopty_proto::agent::AgentStatus;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::{Theme, Typography, alpha};

use super::actions::ToggleNavigator;
use super::agents::{Waiting, agent_status_text};
use super::rollup::{Rollup, age_at, meta_line, rollup_slot};
use super::tile::{cwd_tail, kind_icon};
use super::titlebar::{LEADING_INSET, TITLEBAR_H};
use super::{WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize, Status, icon, status_mark};

/// A one-line row's height, in points: a worker's header, a row of *Needs you*.
pub(super) const ROW_H: f32 = 28.0;

/// A tile's row: its title over its muted second line.
const TILE_ROW_H: f32 = 40.0;

/// The two lines' height, as a multiple of their type size: tighter than a paragraph's, so
/// the pair reads as one row.
const TILE_LINE: f32 = 1.3;

/// The handle's width, along the inside of the navigator's right edge.
const HANDLE_W: f32 = 6.0;

/// How the navigator sits in the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// Beside the frame, which narrows to make room.
    Docked,
    /// Over the frame, which keeps its width: an iPad, or a window too narrow to give the
    /// room without turning the strip into a phone's.
    Overlay,
    /// Over the frame and a scrim, from the left edge: a phone.
    Drawer,
}

/// How the navigator sits in a window `window_w` wide, `width` its own width. A window that
/// is a phone's gets the drawer; a touch screen (an iPad) or a window that would leave the
/// strip a phone's width gets the overlay; anything else docks it.
pub(super) fn mode(window_w: f32, width: f32, phone_below: f32, touch: bool) -> Mode {
    if window_w < phone_below {
        Mode::Drawer
    } else if touch || window_w - width < phone_below {
        Mode::Overlay
    } else {
        Mode::Docked
    }
}

/// What the navigator keeps for this run only; whether it is docked and its width are the
/// layout's, and saved with it.
#[derive(Default)]
pub(super) struct NavState {
    /// Open over the strip, where it does not dock.
    pub open: bool,
    /// Workers whose tiles are folded away.
    pub folded: HashSet<WorkerKey>,
    /// The handle is being dragged: where the pointer and the width were when it was pressed.
    pub resize: Option<(f32, f32)>,
    /// How it sat in the last frame drawn, and whether it showed.
    pub drawn: Option<Mode>,
    /// The filter over its rows.
    pub filter: Filter,
}

/// The navigator's filter: its field, made on the first frame that shows the navigator (it
/// needs the window), what the field holds, and the field's events, held while it is.
#[derive(Default)]
pub(super) struct Filter {
    input: Option<Entity<InputState>>,
    query: String,
    events: Option<Subscription>,
}

impl std::fmt::Debug for NavState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavState")
            .field("open", &self.open)
            .field("folded", &self.folded)
            .field("drawn", &self.drawn)
            .field("query", &self.filter.query)
            .finish_non_exhaustive()
    }
}

/// A round trip as the navigator and the status bar print it: tenths under 10 ms, whole
/// milliseconds above, so a jittery link does not repaint the chrome on every sample.
pub(super) fn rtt_label(rtt: std::time::Duration) -> String {
    let ms = rtt.as_secs_f64() * 1e3;
    if ms < 10.0 { format!("{ms:.1} ms") } else { format!("{ms:.0} ms") }
}

/// A worker's link where it is not simply up: a mark and a short word. A link that is up
/// shows neither, so a list of workers that are fine is a list of names; the word says what
/// is wrong only when something is.
pub(super) const fn worker_health(status: &WorkerStatus) -> Option<(Status, &'static str)> {
    match status {
        WorkerStatus::Connected => None,
        WorkerStatus::Connecting => Some((Status::Working, "connecting")),
        WorkerStatus::Silent(_) => Some((Status::Away, "silent")),
        WorkerStatus::Reconnecting(_) => Some((Status::Away, "reconnecting")),
        WorkerStatus::Unreachable => Some((Status::Away, "unreachable")),
        WorkerStatus::Gone => Some((Status::Away, "gone")),
    }
}

/// Where a tile's row stands among its worker's: what needs the human first, then what
/// finished or has news not yet seen, then what is working, then the rest.
pub(super) const fn attention(status: Option<Status>, unseen: bool) -> u8 {
    match status {
        Some(Status::NeedsYou) => 0,
        _ if unseen => 1,
        Some(Status::Done | Status::Failed) => 1,
        Some(Status::Working) => 2,
        Some(Status::Idle | Status::Away) | None => 3,
    }
}

/// Whether `query` (already lowercase) is in any of `hay`; an empty query is in everything.
pub(super) fn matches(query: &str, hay: &[&str]) -> bool {
    query.is_empty() || hay.iter().any(|h| h.to_lowercase().contains(query))
}

/// The unseen dot: something ended there while the human was elsewhere. It sits centred in a
/// fixed slot at the end of a row's first line, so a title keeps its length with or without it.
pub(super) fn unseen_dot(theme: &Theme, selector: String, shown: bool) -> Div {
    let s = &theme.surfaces;
    let dot = theme.spacing.xs + theme.spacing.xxs;
    div().flex_none().w(px(theme.spacing.sm)).flex().items_center().justify_center().when(
        shown,
        |slot| {
            slot.child(
                div()
                    .debug_selector(move || selector)
                    .size(px(dot))
                    .rounded_full()
                    .bg(hsla(s.accent)),
            )
        },
    )
}

/// One row of a list in the frame: the navigator's and the inbox's. A tab stop named `label`;
/// the pointer washes it `raised`, and the selected row sits on `overlay`.
pub(super) fn row(
    theme: &Theme,
    id: impl Into<ElementId>,
    selector: String,
    label: SharedString,
    selected: bool,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let spacing = theme.spacing;
    let el = div()
        .id(id)
        .debug_selector(move || selector)
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .h(px(ROW_H))
        .mx(px(spacing.xs))
        .px(px(spacing.xs))
        .flex()
        .items_center()
        .gap(px(spacing.xs))
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .text_size(px(theme.typography.ui_size))
        .when(selected, |el| el.bg(hsla(s.overlay)))
        .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.raised))));
    tab_stop(el, s.accent)
}

/// A row's title, cut with an ellipsis.
pub(super) fn title(text: impl Into<SharedString>, color: gpui::Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(color)
        .child(text.into())
}

/// Muted caption type on the right of a row, in tabular figures: a count or a round trip that
/// changes does not move what is beside it.
pub(super) fn caption(theme: &Theme, text: impl Into<SharedString>) -> Div {
    crate::kit::tabular(div())
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(theme.typography.caption()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text.into())
}

/// A section's heading, as every list in the frame heads its sections.
fn heading(theme: &Theme, selector: &'static str, text: &'static str) -> Stateful<Div> {
    crate::palette::section_heading(theme, selector.into(), text)
        // On the rows' leading edge, where their icons start.
        .px(px(theme.spacing.sm))
        .debug_selector(move || selector.to_owned())
        .flex_none()
}

/// A square of the leading slot's side around `child`, so every row's title starts on one edge.
fn lead_slot(theme: &Theme, child: impl gpui::IntoElement) -> Div {
    div()
        .flex_none()
        .size(px(theme.typography.icon_large()))
        .flex()
        .items_center()
        .justify_center()
        .child(child)
}

/// A tile as its row shows it.
struct NavTile {
    tile: TileRef,
    kind: IconName,
    mark: Option<Status>,
    unseen: bool,
    title: String,
    meta: String,
    age: Option<String>,
}

/// A worker's block as the navigator lists it.
struct NavWorker {
    key: WorkerKey,
    /// Every tile it has in the layout, whatever the filter shows.
    count: usize,
    rollup: Rollup,
    tiles: Vec<NavTile>,
}

impl WorkspaceView {
    /// ⌘B: dock or undock the navigator where it docks (kept with the layout), else open or
    /// close it over the strip.
    pub fn toggle_navigator(
        &mut self,
        _: &ToggleNavigator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.navigator_mode(window) {
            Mode::Docked => {
                let nav = self.layout.navigator();
                self.layout.set_navigator(Navigator { shown: !nav.shown, ..nav });
                self.nav.open = false;
                self.layout_touched(cx);
            }
            Mode::Overlay | Mode::Drawer => self.nav.open = !self.nav.open,
        }
        cx.notify();
    }

    /// How the navigator sits in `window`.
    pub(super) fn navigator_mode(&self, window: &Window) -> Mode {
        let window_w = self.width(window);
        let width = self.layout.navigator().width;
        mode(window_w, width, self.layout.config().phone_below, cfg!(target_os = "ios"))
    }

    /// Whether the navigator is drawn in `mode`.
    pub(super) fn navigator_visible(&self, mode: Mode) -> bool {
        !self.workers.is_empty()
            && match mode {
                Mode::Docked => self.layout.navigator().shown,
                Mode::Overlay | Mode::Drawer => self.nav.open,
            }
    }

    /// The navigator's width, in points.
    #[must_use]
    pub const fn navigator_width(&self) -> f32 {
        self.layout.navigator().width
    }

    /// What the navigator's filter holds.
    #[must_use]
    pub fn navigator_filter(&self) -> &str {
        &self.nav.filter.query
    }

    /// A row was chosen: the inbox closes, and over the strip the navigator gets out of the
    /// way of what it chose.
    fn navigated(&mut self) {
        self.menu = None;
        self.nav.open = false;
        if self.layout.overview_open() {
            self.layout.set_overview(false);
        }
    }

    /// Fly to `tile` and focus it.
    pub(super) fn go_to_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.focus_tile(tile, cx);
    }

    /// Go to an agent waiting on the human: its tile, or, with none here, a new one on its
    /// worker (the same as ⌘⇧A does for it).
    pub(super) fn go_to_waiting(&mut self, waiting: Waiting, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        if waiting.tile.is_some() {
            self.reveal_session(waiting.session, cx);
            return;
        }
        self.show_untiled(waiting.worker, waiting.session, cx);
    }

    /// Switch to workspace `ix`.
    pub(super) fn go_to_workspace(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.layout.focus_workspace(ix);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// The handle moved to `x` (window points): the width follows, within its clamps.
    fn resize_navigator_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((grab, from)) = self.nav.resize else { return };
        let nav = self.layout.navigator();
        let width = Navigator::clamp_width(from + x - grab);
        if (width - nav.width).abs() > f32::EPSILON {
            self.layout.set_navigator(Navigator { width, ..nav });
            cx.notify();
        }
    }

    fn end_navigator_resize(&mut self, cx: &mut Context<Self>) {
        if self.nav.resize.take().is_some() {
            self.layout_touched(cx);
            cx.notify();
        }
    }

    /// Whether a coding agent runs in `item`'s terminal.
    pub(super) fn runs_agent(&self, item: &Item) -> bool {
        let ItemKind::Terminal { session } = item.kind else { return false };
        self.agent_state(session).is_some_and(|a| a.status != AgentStatus::None)
    }

    /// A tile's status mark, and whether it holds news the human has not looked at: a long
    /// command that finished unwatched, while the tile is not busy or waiting.
    pub(super) fn tile_marks(
        &self,
        tile: TileRef,
        item: &Item,
        cx: &gpui::App,
    ) -> (Option<Status>, bool) {
        let mark = self.tile_status(tile, item, cx);
        let unwatched = match item.kind {
            ItemKind::Terminal { session } => self.finished.contains_key(&session),
            _ => false,
        };
        (mark, unwatched && !matches!(mark, Some(Status::Working | Status::NeedsYou)))
    }

    /// What a tile's second line says, and its age: a shell's directory, its agent's words
    /// or its last command, its branch; a page's address; a file's directory; else its kind.
    fn tile_meta(&self, item: &Item, now: SystemTime, cx: &gpui::App) -> (String, Option<String>) {
        match &item.kind {
            ItemKind::Terminal { session } => {
                let summary = self.summary(*session);
                let place = summary.and_then(|s| s.cwd.as_deref()).map(cwd_tail);
                let agent = self
                    .agent_state(*session)
                    .filter(|a| a.status != AgentStatus::None)
                    .map(agent_status_text);
                let command = agent.is_none().then(|| self.last_command(*session, cx)).flatten();
                let branch = summary.and_then(|s| s.branch.as_deref());
                let meta =
                    meta_line([place.as_deref(), agent.as_deref().or(command.as_deref()), branch]);
                let age =
                    summary.and_then(|s| age_at(s.started_ms, now)).map(crate::palette::age_label);
                (meta, age)
            }
            ItemKind::Browser { url } => (crate::browser::short_url(url).to_owned(), None),
            ItemKind::File { .. } => {
                (self.cwd_of(item).map(|dir| cwd_tail(&dir)).unwrap_or_default(), None)
            }
            ItemKind::Window { .. } => ("Window".to_owned(), None),
            ItemKind::Display { .. } => ("Display".to_owned(), None),
            ItemKind::Note { .. } => ("Note".to_owned(), None),
        }
    }

    /// The command a shell runs now, else the last one it ran, else the one that finished
    /// unwatched: its first line.
    fn last_command(&self, session: slopty_core::SessionId, cx: &gpui::App) -> Option<String> {
        let state = self.terminals.get(&session).map(|v| v.read(cx).state());
        let typed = state.and_then(|state| {
            state.running_command().map(str::to_owned).or_else(|| state.last_command())
        });
        let typed = typed.or_else(|| self.finished.get(&session).map(|f| f.command.clone()))?;
        typed.lines().next().map(str::to_owned).filter(|l| !l.trim().is_empty())
    }

    /// Every worker's block as the filter leaves it: a worker whose name matches keeps every
    /// tile; otherwise only its matching tiles, and without one it is not listed. Tiles come
    /// in order of attention.
    fn nav_listing(&self, cx: &gpui::App) -> Vec<NavWorker> {
        let query = self.nav.filter.query.trim().to_lowercase();
        let order = self.reading_order();
        let now = SystemTime::now();
        let mut out = Vec::new();
        for (key, w) in &self.workers {
            let key = *key;
            let named = matches(&query, &[&w.name]);
            let mut rollup = Rollup::default();
            let mut count = 0_usize;
            let mut tiles: Vec<(u8, NavTile)> = Vec::new();
            for &tile in order.iter().filter(|t| t.worker == key) {
                let Some(item) = w.doc.get(tile.item) else { continue };
                count = count.saturating_add(1);
                let (mark, unseen) = self.tile_marks(tile, item, cx);
                rollup.add(mark, unseen);
                let title = self.card_title(tile, item, cx);
                let (meta, age) = self.tile_meta(item, now, cx);
                if !named && !matches(&query, &[&title, &meta]) {
                    continue;
                }
                let kind = kind_icon(item, self.runs_agent(item));
                let row = NavTile { tile, kind, mark, unseen, title, meta, age };
                tiles.push((attention(mark, unseen), row));
            }
            if !named && tiles.is_empty() {
                continue;
            }
            // Stable: within a class the tiles keep their reading order.
            tiles.sort_by_key(|(class, _)| *class);
            let tiles = tiles.into_iter().map(|(_, t)| t).collect();
            out.push(NavWorker { key, count, rollup, tiles });
        }
        out
    }

    /// The filter's field, made once there is a window to make it in. ↩ in it goes to the
    /// first tile listed.
    pub(super) fn ensure_navigator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav.filter.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        self.nav.filter.events = Some(cx.subscribe(&input, |this, input, event, cx| match event {
            InputEvent::Change => {
                this.nav.filter.query = input.read(cx).value().to_string();
                cx.notify();
            }
            InputEvent::PressEnter { .. } => {
                let first = this.nav_listing(cx).into_iter().flat_map(|w| w.tiles).next();
                if let Some(first) = first {
                    this.go_to_tile(first.tile, cx);
                }
            }
            InputEvent::Focus | InputEvent::Blur => {}
        }));
        self.nav.filter.input = Some(input);
    }

    /// Empty the filter, and give the keyboard back to the workspace.
    fn clear_navigator_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.nav.filter.input.clone() {
            input.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        }
        self.nav.filter.query.clear();
        self.pending_focus_self = true;
        cx.notify();
    }

    /// The navigator's panel as `mode` places it; `None` while it is hidden.
    pub(super) fn render_navigator(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<(Mode, gpui::AnyElement)> {
        let mode = self.navigator_mode(window);
        let visible = self.navigator_visible(mode);
        self.nav.drawn = visible.then_some(mode);
        if !visible {
            self.nav.resize = None;
            return None;
        }
        Some((mode, self.navigator_panel(mode, window, cx)))
    }

    /// The top row, the title bar's height: room for the traffic lights on a Mac, then the
    /// filter.
    fn navigator_header(&self, window: &Window, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let safe = window.insets().effective();
        let leading = if cfg!(target_os = "macos") { LEADING_INSET } else { theme.spacing.sm };
        let field = self.nav.filter.input.as_ref().map(|input| {
            div()
                .debug_selector(|| "nav-filter".to_owned())
                .flex_1()
                .min_w_0()
                .text_size(px(theme.typography.ui_size))
                .child(Input::new(input).appearance(false).px_0().aria_label("Filter"))
        });
        let clear = (!self.nav.filter.query.is_empty()).then(|| {
            crate::kit::icon_button(theme, "nav-filter-clear", IconName::X, "Clear filter")
                .on_click(
                    cx.listener(|this, _ev, window, cx| this.clear_navigator_filter(window, cx)),
                )
        });
        div()
            .flex_none()
            .h(px(TITLEBAR_H) + safe.top)
            .pt(safe.top)
            .pl(px(leading))
            .pr(px(theme.spacing.xs))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .border_b_1()
            .border_color(hsla(s.border))
            .child(lead_slot(
                theme,
                icon(theme, IconName::Search, IconSize::Inline, hsla(s.text_muted)),
            ))
            .children(field)
            .children(clear)
    }

    /// The panel itself, docked or laid over the frame.
    fn navigator_panel(&self, mode: Mode, window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let safe = window.insets().effective();
        let width = match mode {
            Mode::Drawer => {
                let room = theme.spacing.xl.mul_add(-2.0, self.width(window));
                self.navigator_width().min(room)
            }
            Mode::Docked | Mode::Overlay => self.navigator_width(),
        };
        let query = self.nav.filter.query.trim().to_lowercase();
        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        let waiting: Vec<Waiting> = self
            .drawn_waiting
            .iter()
            .filter(|w| {
                query.is_empty()
                    || matches(&query, &[&self.waiting_what(**w, cx), &self.worker_name(w.worker)])
            })
            .copied()
            .collect();
        if !waiting.is_empty() {
            sections.push(heading(theme, "nav-needs-you", "Needs you").into_any_element());
            sections.extend(waiting.into_iter().map(|w| self.waiting_row("nav", w, cx)));
        }
        let listing = self.nav_listing(cx);
        if !listing.is_empty() {
            sections.push(heading(theme, "nav-workers", "Workers").into_any_element());
        }
        let focused = self.focused();
        for worker in listing {
            sections.extend(self.worker_block(worker, !query.is_empty(), focused, cx));
        }
        if sections.is_empty() {
            sections.push(
                div()
                    .debug_selector(|| "nav-nothing".to_owned())
                    .px(px(theme.spacing.md))
                    .py(px(theme.spacing.md))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(crate::picker::NOTHING_MATCHES)
                    .into_any_element(),
            );
        }
        let list = div()
            .id("navigator-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(px(theme.spacing.md))
            .children(sections);
        let handle = (mode != Mode::Drawer).then(|| Self::render_handle(cx));
        let panel = div()
            .id("navigator")
            .debug_selector(|| "navigator".to_owned())
            .role(Role::Navigation)
            .aria_label("Navigator")
            .occlude()
            .relative()
            .flex_none()
            .h_full()
            .w(px(width) + if mode == Mode::Docked { px(0.0) } else { safe.left })
            .pl(safe.left)
            .flex()
            .flex_col()
            .bg(hsla(s.panel))
            .border_r_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            .when(mode != Mode::Docked, gpui::Styled::shadow_sm)
            // Esc in the filter empties it and hands the keyboard back.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                if !this.nav.filter.query.is_empty() {
                    this.clear_navigator_filter(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.navigator_header(window, cx))
            .child(list)
            .children(handle);
        match mode {
            Mode::Docked => panel.into_any_element(),
            Mode::Overlay | Mode::Drawer => {
                let scrim = if mode == Mode::Drawer {
                    hsla_alpha(s.canvas, alpha::SCRIM)
                } else {
                    gpui::transparent_black()
                };
                div()
                    .id("navigator-away")
                    .debug_selector(|| "navigator-away".to_owned())
                    .absolute()
                    .inset_0()
                    .bg(scrim)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _w, cx| {
                            this.nav.open = false;
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                            .child(panel),
                    )
                    .into_any_element()
            }
        }
    }

    /// The strip of the right edge the pointer drags, and the listeners that follow the
    /// pointer wherever it goes once the handle is pressed. They are there in every frame the
    /// handle is, so the first move after the press is followed without waiting for a frame.
    fn render_handle(cx: &Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let follow = canvas(
            |_bounds, _window, _cx| (),
            move |_bounds, (), window, _cx| {
                let moved = entity.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    let resizing =
                        moved.read_with(cx, |this, _| this.nav.resize.is_some()).unwrap_or(false);
                    if phase != gpui::DispatchPhase::Capture || !resizing {
                        return;
                    }
                    let _gone = moved.update(cx, |this, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            this.resize_navigator_to(f32::from(ev.position.x), cx);
                        } else {
                            this.end_navigator_resize(cx);
                        }
                    });
                });
                let released = entity;
                window.on_mouse_event(move |_ev: &MouseUpEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _gone = released.update(cx, Self::end_navigator_resize);
                    }
                });
            },
        )
        .absolute()
        .size_0();
        div()
            .id("navigator-handle")
            .debug_selector(|| "navigator-handle".to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(HANDLE_W))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, _w, cx| {
                    this.nav.resize = Some((f32::from(ev.position.x), this.navigator_width()));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(follow)
            .into_any_element()
    }

    /// `key`'s name, or nothing for a worker no longer known.
    fn worker_name(&self, key: WorkerKey) -> String {
        self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default()
    }

    /// What waits, in a row of *Needs you*: its tile's title, else its agent's words.
    fn waiting_what(&self, waiting: Waiting, cx: &gpui::App) -> String {
        waiting
            .tile
            .and_then(|t| Some(self.card_title(t, self.item(t)?, cx)))
            .or_else(|| self.agent_state(waiting.session).map(agent_status_text))
            .unwrap_or_else(|| "Agent".to_owned())
    }

    /// A row of *Needs you*: the mark, what waits, and on which worker.
    pub(super) fn waiting_row(
        &self,
        prefix: &str,
        waiting: Waiting,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let what = self.waiting_what(waiting, cx);
        let worker = self.worker_name(waiting.worker);
        let session = waiting.session;
        let label = SharedString::from(format!("{what}, needs you on {worker}"));
        row(
            theme,
            ElementId::Name(format!("{prefix}-waiting-{session}").into()),
            format!("{prefix}-waiting-{session}"),
            label,
            false,
        )
        .child(status_mark(theme, Some(Status::NeedsYou), 1.0))
        .child(title(what, hsla(s.text)))
        .child(caption(theme, worker))
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_waiting(waiting, cx)))
        .into_any_element()
    }

    /// A worker's header, and its tiles' rows beneath it unless it is folded. While the filter
    /// holds something, a fold hides nothing.
    fn worker_block(
        &self,
        worker: NavWorker,
        filtering: bool,
        focused: Option<TileRef>,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let folded = !filtering && self.nav.folded.contains(&worker.key);
        let mut rows = vec![self.worker_header(&worker, folded, cx)];
        if !folded {
            rows.extend(worker.tiles.into_iter().map(|t| self.tile_row(t, focused, cx)));
        }
        rows
    }

    /// A worker's header: the server icon (crossed out, in warn, while it is away), the name,
    /// then on the right edge its count of tiles and its round trip or what is wrong, led by
    /// what a folded worker's tiles add up to. Under the pointer the chevron and "+" take the
    /// readouts' place; nothing moves when either shows.
    fn worker_header(
        &self,
        worker: &NavWorker,
        folded: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let key = worker.key;
        let Some(w) = self.workers.get(&key) else { return div().into_any_element() };
        let health = worker_health(&w.status);
        let rtt = w.rtt.filter(|_| health.is_none()).map(rtt_label);
        let label = SharedString::from(format!(
            "{}{}{}",
            w.name,
            health.map(|(_, word)| format!(", {word}")).unwrap_or_default(),
            if folded { ", folded" } else { "" }
        ));
        let lead = match health {
            None => lead_slot(
                theme,
                icon(theme, IconName::Server, IconSize::Inline, hsla(s.text_muted)),
            ),
            Some((Status::Away, _)) => lead_slot(
                theme,
                div().id("away").role(Role::Image).aria_label(Status::Away.label()).child(icon(
                    theme,
                    IconName::ServerOff,
                    IconSize::Inline,
                    hsla(s.warn),
                )),
            ),
            Some((mark, _)) => lead_slot(theme, status_mark(theme, Some(mark), 1.0)),
        };
        let name = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
            .text_color(hsla(s.text))
            .child(w.name.clone());
        let count = (worker.count > 0).then(|| {
            caption(theme, worker.count.to_string())
                .debug_selector(move || format!("nav-count-{key}"))
        });
        let group = SharedString::from(format!("nav-worker-group-{key}"));
        // At rest: the readouts on the row's right edge, where the tiles' ages end, and before
        // them what a folded worker's tiles add up to. They grow leftwards, so a rollup coming
        // or going moves neither the count nor the round trip.
        let rollup = folded.then_some(worker.rollup).filter(|r| r.shown().is_some());
        let rest = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(theme.spacing.xs))
            .group_hover(group.clone(), gpui::Styled::invisible)
            .children(rollup.map(|r| rollup_slot(theme, format!("nav-rollup-{key}"), r)))
            .children(count)
            .children(health.map(|(_, word)| caption(theme, word)))
            .children(
                rtt.map(|rtt| caption(theme, rtt).debug_selector(move || format!("nav-rtt-{key}"))),
            );
        let chevron = if folded { IconName::ChevronRight } else { IconName::ChevronDown };
        let side = theme.typography.icon_large();
        let add = w.link.is_some().then(|| {
            let el = div()
                .id(SharedString::from(format!("nav-new-shell-{key}")))
                .debug_selector(move || format!("nav-new-shell-{key}"))
                .role(Role::Button)
                .aria_label(SharedString::from(format!("New shell on {}", w.name)))
                .flex_none()
                .size(px(side))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.overlay)))
                .child(icon(theme, IconName::Plus, IconSize::Inline, hsla(s.text_secondary)))
                .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    cx.stop_propagation();
                    this.open_session_on(key, None, Vec::new(), None, cx);
                }));
            tab_stop(el, s.accent)
        });
        // Under the pointer: the chevron and "+" in their fixed places over the readouts.
        let hover = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(theme.spacing.xxs))
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            .child(lead_slot(theme, icon(theme, chevron, IconSize::Inline, hsla(s.text_muted))))
            .children(add);
        let trailing = div()
            .debug_selector(move || format!("nav-worker-slot-{key}"))
            .relative()
            .flex_none()
            .min_w(px(header_actions_width(theme)))
            .h(px(side))
            .flex()
            .items_center()
            .justify_end()
            .child(rest)
            .child(hover);
        row(
            theme,
            ElementId::Name(format!("nav-worker-{key}").into()),
            format!("nav-worker-{key}"),
            label,
            false,
        )
        .group(group)
        .child(lead)
        .child(name)
        .child(trailing)
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            if !this.nav.folded.remove(&key) {
                this.nav.folded.insert(key);
            }
            cx.notify();
        }))
        .into_any_element()
    }

    /// A tile's row: the leading slot (its kind at rest, its status otherwise), the title and
    /// the unseen dot's slot, then the muted second line and the age.
    fn tile_row(
        &self,
        t: NavTile,
        focused: Option<TileRef>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let typo = &theme.typography;
        let selected = focused == Some(t.tile);
        let label =
            [Some(t.title.as_str()), t.mark.map(Status::label), t.unseen.then_some("unseen")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
        let ink = if selected { s.text } else { s.text_secondary };
        let id = t.tile.item.as_uuid();
        // An idle tile is at rest: it shows what it is, not that nothing is happening.
        let mark = t.mark.filter(|m| *m != Status::Idle);
        let lead = crate::palette::status_slot(theme, t.kind, mark, hsla(s.text_muted), 1.0);
        let first = typo.ui_size * TILE_LINE;
        let second = typo.small() * TILE_LINE;
        let line1 = div()
            .h(px(first))
            .line_height(px(first))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(title(t.title, hsla(ink)))
            .child(unseen_dot(theme, format!("nav-unseen-{id}"), t.unseen));
        let line2 = div()
            .h(px(second))
            .line_height(px(second))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .text_size(px(typo.small()))
            .text_color(hsla(s.text_muted))
            .child(
                div()
                    .debug_selector(move || format!("nav-meta-{id}"))
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(t.meta),
            )
            .children(t.age.map(|age| {
                crate::kit::tabular(div())
                    .debug_selector(move || format!("nav-age-{id}"))
                    .flex_none()
                    .whitespace_nowrap()
                    .child(age)
            }));
        let tile = t.tile;
        row(
            theme,
            ElementId::Name(format!("nav-tile-{id}").into()),
            format!("nav-tile-{id}"),
            label.into(),
            selected,
        )
        .h(px(TILE_ROW_H))
        .items_start()
        .pt(px(theme.spacing.xs))
        .pl(px(theme.spacing.xl))
        .child(div().h(px(first)).flex().items_center().child(lead))
        .child(div().flex_1().min_w_0().flex().flex_col().child(line1).child(line2))
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_tile(tile, cx)))
        .into_any_element()
    }
}

/// The least the worker header's trailing part takes: the chevron and "+" side by side.
fn header_actions_width(theme: &Theme) -> f32 {
    theme.typography.icon_large().mul_add(2.0, theme.spacing.xxs)
}

#[cfg(test)]
impl WorkspaceView {
    /// Every tile row the navigator lists, as drawn: its title, its second line and its age.
    pub(super) fn navigator_lines(&self, cx: &gpui::App) -> Vec<(String, String, Option<String>)> {
        self.nav_listing(cx)
            .into_iter()
            .flat_map(|w| w.tiles)
            .map(|t| (t.title, t.meta, t.age))
            .collect()
    }
}
