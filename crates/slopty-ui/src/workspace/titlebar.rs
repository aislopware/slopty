//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, the workspaces as
//! tabs with "+" after them (a menu of what to open: a terminal, an agent, a window, a note, or
//! a workspace); the inbox's bell and "…" on the right. Where the view is along the strip is
//! the strip's own thumb (`marks`), not the bar's. Nothing else: every other action is a key, the
//! palette, or a tile's own header, and the readouts (the server's state among them) live in
//! the status bar.
//!
//! A tab is as wide as its name, from 64 to 180 pt, its words on the bar's midline with the
//! buttons and the traffic lights. The active one is the medium weight on the content's colour,
//! reaching down through the bar's edge so it joins what it shows, as an editor's tab does; the
//! rest are quiet words that take a row's fill under the pointer. A single workspace is no tab
//! at all, only its name in the medium weight, and no count beside it: the navigator and the
//! overview count tiles. A tab ends in what its tiles add up to (the rollup), a dot at most. A
//! tab that opens grows from nothing, its words fading in once there is room for them; one that
//! closes folds its width away so its neighbours slide into its place; and the active tab's
//! fill slides from the old tab to the new one while the weight of the words swaps at once.
//! A menu fades in as it drops 4 pt from its button. All of it lands at once under Reduce
//! Motion.
//!
//! On a phone the bar is a navigation bar: the workspace's name alone at the navigation title's
//! size, what "+" opens folded into "…", and the column marks after the name.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AppContext as _, Context, InteractiveElement as _,
    IntoElement as _, MouseButton, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};
use slopty_client::layout::{Column, Tile, WorkerKey};
use slopty_theme::{Motion, Typography};

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::{Rollup, rollup_slot};
use super::strip::NEW_WORKSPACE;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::IconName;
use crate::kit;

/// The bar's height under the safe area, with a pointer.
pub const TITLEBAR_H: f32 = 38.0;

/// The bar's height under `theme`'s density: a finger's target and a hairline's room round
/// it at least, so its buttons fit it on touch.
#[must_use]
pub const fn titlebar_height(theme: &slopty_theme::Theme) -> f32 {
    TITLEBAR_H.max(2.0_f32.mul_add(theme.spacing.xxs, theme.density.hit))
}

/// Room for the traffic lights at the left of the bar, or of the navigator when it shows.
#[cfg(target_os = "macos")]
pub(super) const LEADING_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub(super) const LEADING_INSET: f32 = 12.0;

/// A workspace tab's width: as wide as its name, within these.
const TAB_MIN_W: f32 = 64.0;
const TAB_MAX_W: f32 = 180.0;

/// How long a tab takes to grow in, fold away, or hand its fill to another.
const TAB_SETTLE: Duration = Motion::DEFAULT.settle;

/// The bar's hairline, which a tab's box reaches through and draws round the active one.
const TAB_HAIRLINE: f32 = 1.0;

/// The bell's hover group, which its badge's cut-out ring follows.
const BELL: &str = "bell";

/// What "+" is called: it opens a menu of things to open.
pub(super) const NEW: &str = "New";

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// How much bigger than the body the phone's workspace name is: the iOS navigation title's
/// 17 over the body's 15.
const PHONE_TITLE_STEP: f32 = 2.0;

/// The share of a tab's opening before its words start to fade in: they wait for the room.
const LABEL_WAITS: f32 = 0.4;

/// The band a tab's words and hover fill take, centred on the bar's midline: a row's height,
/// so a finger gets a row's target on touch.
const fn tab_band(theme: &slopty_theme::Theme) -> f32 {
    theme.density.row
}

/// What a menu row runs, on the workspace itself.
type MenuAction = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    /// "+": what to open.
    New,
    /// "…": everything else, the app's entries included.
    More,
    /// The bell: what needs the human and what finished.
    Inbox,
}

/// A tab folding away where it stood.
struct ClosingTab {
    /// Its workspace's id.
    id: u64,
    /// Its name, as drawn.
    name: String,
    /// Its width as laid out.
    width: f32,
    /// Its place in the row.
    at: usize,
    /// Since when it folds.
    since: Instant,
}

/// The tabs as last drawn, so a tab that goes can fold away where it was.
#[derive(Default)]
pub(super) struct Tabs {
    /// Each tab drawn, by its workspace's id: its name.
    drawn: RefCell<Vec<(u64, String)>>,
    /// Each tab's left edge in the window and its width as laid out, by its workspace's id.
    widths: Rc<RefCell<HashMap<u64, (f32, f32)>>>,
    /// The tab row's left edge in the window as last laid out.
    row_at: Rc<Cell<f32>>,
    /// Tabs folding away.
    closing: RefCell<Vec<ClosingTab>>,
    /// Tabs growing in: the workspace's id, since when.
    opening: RefCell<Vec<(u64, Instant)>>,
    /// The active tab as last drawn, by its workspace's id.
    active: Cell<Option<u64>>,
    /// The active fill on its way between two tabs: where it left from (left edge in the row,
    /// width), the tab it goes to, how many slides there have been, since when.
    slide: Cell<Option<Slide>>,
    /// Slides so far: each one's animation is its own.
    slides: Cell<u64>,
    /// The left edge of "+" in the window as last laid out, where its menu hangs from.
    new_at: Rc<Cell<f32>>,
}

/// The active tab's fill moving from one tab to another.
#[derive(Clone, Copy, Debug)]
struct Slide {
    from: (f32, f32),
    to: u64,
    seq: u64,
    since: Instant,
}

impl std::fmt::Debug for Tabs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tabs").field("drawn", &self.drawn.borrow().len()).finish_non_exhaustive()
    }
}

impl WorkspaceView {
    /// Whether chrome moves now: not under Reduce Motion, nor under the self-test, where a
    /// frame is a step and a dump must see where things land.
    pub(super) fn chrome_moves(&self, cx: &gpui::App) -> bool {
        self.animate && kit::motion(cx)
    }

    /// Whether the bar is a phone's navigation bar.
    fn phone_bar(&self, window: &Window) -> bool {
        self.width(window) < self.layout.config().phone_below
    }

    /// Whether `key`'s round trip, going from `was` to `now`, is on screen: the status bar
    /// prints the focused tile's worker's while it is slow or under the pointer, the hosts
    /// popover every worker's, and the navigator those slow enough to name. A quick link's
    /// samples then draw nothing.
    pub(super) fn rtt_shown(
        &self,
        key: WorkerKey,
        was: Option<Duration>,
        now: Option<Duration>,
    ) -> bool {
        let slow =
            |rtt: Option<Duration>| rtt.is_some_and(|rtt| rtt >= super::navigator::RTT_SHOWN_FROM);
        self.status_prints_rtt(key, was, now)
            || self.hosts_open()
            || (self.nav.drawn.is_some() && (slow(was) || slow(now)))
    }

    /// The active workspace's name: the one given, else its place.
    #[must_use]
    pub fn workspace_name(&self) -> String {
        self.workspace_name_at(self.layout.active_workspace())
    }

    /// Workspace `ix`'s name: the one given, else where its first shell is (the repository,
    /// else the directory), else the worker its first tile is on. Never a number, nor a tile's
    /// title, which follows every command run and every page loaded. One with nothing on it is
    /// new.
    pub(super) fn workspace_name_at(&self, ix: usize) -> String {
        let Some(ws) = self.layout.workspaces().get(ix) else { return NEW_WORKSPACE.to_owned() };
        if let Some(name) = ws.name().map(str::to_owned).or_else(|| self.workspace_place(ws)) {
            return name;
        }
        ws.columns()
            .iter()
            .flat_map(Column::tiles)
            .map(Tile::tile)
            .next()
            .map_or_else(|| NEW_WORKSPACE.to_owned(), |first| self.worker_name(first.worker))
    }

    /// Where the first shell of `ws` that has said so is: its repository's name, else its
    /// directory's, unless that is the home directory.
    fn workspace_place(&self, ws: &slopty_client::layout::Workspace) -> Option<String> {
        ws.columns().iter().flat_map(Column::tiles).map(Tile::tile).find_map(|tile| {
            let slopty_proto::items::ItemKind::Terminal { session } = self.item(tile)?.kind else {
                return None;
            };
            let (home, summary) = self.session_on(session)?;
            super::tile::place_name(summary.cwd.as_deref()?, summary.repo.as_deref(), home)
        })
    }

    /// The workspaces the bar has a tab for: those holding something or named, and the
    /// active one even when empty.
    pub(super) fn tabbed_workspaces(&self) -> Vec<usize> {
        let active = self.layout.active_workspace();
        self.layout
            .workspaces()
            .iter()
            .enumerate()
            .filter(|(ix, ws)| *ix == active || !ws.columns().is_empty() || ws.name().is_some())
            .map(|(ix, _)| ix)
            .collect()
    }

    /// What workspace `ix`'s tiles add up to, and how many there are.
    pub(super) fn workspace_rollup(&self, ix: usize) -> (Rollup, usize) {
        let mut rollup = Rollup::default();
        let mut count = 0_usize;
        let Some(ws) = self.layout.workspaces().get(ix) else { return (rollup, count) };
        for tile in ws.columns().iter().flat_map(Column::tiles).map(Tile::tile) {
            count = count.saturating_add(1);
            if let Some(item) = self.item(tile) {
                let (mark, unseen) = self.tile_marks(tile, item);
                rollup.add(mark, unseen);
            }
        }
        (rollup, count)
    }

    fn toggle_menu(&mut self, which: MenuKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu == Some(which) {
            self.close_menu(window, cx);
        } else {
            self.menu = Some(which);
            self.menu_keyed = window.last_input_was_keyboard();
            // The inbox takes the keyboard, so its rows are worked by key at once.
            if which == MenuKind::Inbox {
                let focus = self.inbox_focus(cx);
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    /// ⌘⇧U: the bell's inbox, opened or closed by key.
    pub(super) fn toggle_inbox(
        &mut self,
        _: &super::actions::ToggleInbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_menu(MenuKind::Inbox, window, cx);
    }

    /// Close the bar's menu and hand the keyboard back to the focused tile at once.
    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.return_keyboard(window, cx);
        cx.notify();
    }

    /// The bar. `safe_top` is the notch's inset on a phone, zero on a Mac.
    pub(super) fn render_titlebar(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let spacing = self.theme.spacing;
        let safe = window.insets().effective();
        // Nothing to open, no column to mark and no one to point at before the first worker:
        // the bar keeps only "…", where settings and the ways to add one live.
        let has_workers = !self.workers.is_empty();
        // A docked navigator holds the traffic lights and the safe area's left edge; the bar
        // starts at its right edge.
        let docked = self.nav.drawn == Some(Mode::Docked);
        let leading = if docked { spacing.sm } else { LEADING_INSET + f32::from(safe.left) };
        let trailing = spacing.md + f32::from(safe.right);
        let phone = self.phone_bar(window);
        let tabs = has_workers.then(|| {
            if phone { self.render_phone_title(cx) } else { self.render_workspace_tabs(window, cx) }
        });
        let theme = &self.theme;
        let s = &theme.surfaces;

        // Left: the navigator's toggle, the workspaces and the columns of the one in view.
        let toggle = has_workers.then(|| {
            let hint_theme = Rc::new(theme.clone());
            kit::icon_button(theme, "navigator-toggle", IconName::PanelLeft, "Navigator")
                .when(SHORTCUT_HINTS, |el| {
                    el.tooltip(move |_window, cx| {
                        let keys =
                            crate::palette::keys_for(&ToggleNavigator, &super::key_bindings());
                        cx.new(|_| kit::Hint::new("Navigator", keys, Rc::clone(&hint_theme))).into()
                    })
                })
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_navigator(&ToggleNavigator, window, cx);
                }))
        });
        // A phone's "+" is a row of "…": the bar keeps the name, the bell and the menu.
        let new = (has_workers && !phone).then(|| {
            let at = Rc::clone(&self.tabs.new_at);
            let measure = canvas(
                move |bounds, _window, _cx| at.set(f32::from(bounds.origin.x)),
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0();
            kit::icon_button(theme, "new-menu", IconName::Plus, NEW)
                .relative()
                .aria_expanded(self.menu == Some(MenuKind::New))
                .child(measure)
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_menu(MenuKind::New, window, cx);
                }))
        });

        // Right: the inbox and "…". Who needs you is counted once, on the bell; its rows go to
        // them.
        let total = self.drawn_waiting.len();
        let unread = total.saturating_add(self.unread_finishes());
        let bell = has_workers.then(|| {
            // Each fill with its own ink: the accent's is white, a state fill's near-black.
            let (fill, ink) =
                if total > 0 { (s.warn_fill, s.fill_fg) } else { (s.accent_fill, s.accent_ink) };
            let badge = (unread > 0).then(|| {
                let count = SharedString::from(unread.to_string());
                let side = theme.typography.caption() + spacing.xs + spacing.xxs;
                let raised = hsla(s.raised);
                // A ring of the bar's colour cuts the disc out of the bell's stroke, as a
                // badge on a Mac's dock is cut out of its icon; under the pointer it takes
                // the button's hover fill.
                kit::tabular(div())
                    .id("bell-count")
                    .debug_selector(|| "bell-count".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("{unread} new")))
                    .absolute()
                    .top(px(-spacing.xs))
                    .right(px(-spacing.xs))
                    .h(px(side))
                    .min_w(px(side))
                    .px(px(spacing.xxs))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(hsla(s.canvas))
                    .group_hover(BELL, move |el| el.border_color(raised))
                    .bg(hsla(fill))
                    .text_color(hsla(ink))
                    .text_size(px(theme.typography.caption()))
                    .child(count)
            });
            kit::icon_button(theme, "bell", IconName::Bell, "Inbox")
                .group(BELL)
                .relative()
                .children(badge)
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_menu(MenuKind::Inbox, window, cx);
                }))
        });
        let more = kit::icon_button(theme, "more", IconName::Ellipsis, "More")
            .aria_expanded(self.menu == Some(MenuKind::More))
            .on_click(
                cx.listener(|this, _ev, window, cx| this.toggle_menu(MenuKind::More, window, cx)),
            );
        let buttons =
            div().flex_none().flex().items_center().gap(px(spacing.xxs)).children(bell).child(more);
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .relative()
            .size_full()
            .pt(safe.top)
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .pl(px(leading))
            .pr(px(trailing))
            .bg(hsla(s.canvas))
            .border_b_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
                    .children(toggle)
                    .children(tabs)
                    .children(new),
            )
            .child(buttons)
            .into_any_element()
    }

    /// A phone's title: the active workspace's name at the navigation title's size and the
    /// strong weight, as an iOS navigation bar names its screen. The navigator and a swipe go
    /// between workspaces; a row of tabs has no room at this width.
    fn render_phone_title(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let ids = self.tab_ids();
        self.fold_closed_tabs(&ids, cx);
        let ix = self.layout.active_workspace();
        let theme = &self.theme;
        let (label, _rollup) = self.tab_words(ix);
        div()
            .id(("ws-tab", ix))
            .debug_selector(move || format!("ws-tab-{ix}"))
            .role(Role::Heading)
            .aria_label(label)
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(px(theme.typography.title() + PHONE_TITLE_STEP))
            .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
            .text_color(hsla(theme.surfaces.text))
            .child(SharedString::from(self.workspace_name_at(ix)))
            .into_any_element()
    }

    /// Each tabbed workspace's id and name.
    fn tab_ids(&self) -> Vec<(u64, String)> {
        self.tabbed_workspaces()
            .iter()
            .filter_map(|ix| {
                let ws = self.layout.workspaces().get(*ix)?;
                Some((ws.id(), self.workspace_name_at(*ix)))
            })
            .collect()
    }

    /// The workspaces: a tab each where there are several, the name alone where there is one.
    fn render_workspace_tabs(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let active = self.layout.active_workspace();
        let tabbed = self.tabbed_workspaces();
        let ids = self.tab_ids();
        self.fold_closed_tabs(&ids, cx);
        let active_id =
            self.layout.workspaces().get(active).map(slopty_client::layout::Workspace::id);
        self.follow_active_tab(active_id, tabbed.len() > 1, cx);
        if let [only] = tabbed.as_slice() {
            return self.render_lone_workspace(*only);
        }
        let sliding = self.tabs.slide.get().map(|slide| slide.to);
        let mut tabs: Vec<gpui::AnyElement> = tabbed
            .iter()
            .map(|ix| {
                let id = self
                    .layout
                    .workspaces()
                    .get(*ix)
                    .map_or(0, slopty_client::layout::Workspace::id);
                let opening = self.tabs.opening.borrow().iter().any(|(o, _)| *o == id);
                let tab = self.render_tab(*ix, *ix == active, sliding == Some(id), cx);
                if opening { self.open_tab(tab, id, &ids, window) } else { tab }
            })
            .collect();
        for ClosingTab { id, name, width, at, .. } in self.tabs.closing.borrow().iter() {
            let ghost = self.render_closing_tab(*id, name, *width);
            tabs.insert((*at).min(tabs.len()), ghost);
        }
        let row_at = Rc::clone(&self.tabs.row_at);
        let measure = canvas(
            move |bounds, _window, _cx| row_at.set(f32::from(bounds.origin.x)),
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let fill = self.render_sliding_fill();
        div()
            .id("ws-tabs")
            .debug_selector(|| "ws-tabs".to_owned())
            .role(Role::Group)
            .aria_label("Workspaces")
            .relative()
            .flex_shrink(1.0)
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .flex()
            .items_end()
            .gap(px(self.theme.spacing.xxs))
            .child(measure)
            .children(fill)
            .children(tabs)
            .into_any_element()
    }

    /// Note a change of the active tab: its fill slides from where the old one's was, when
    /// both were laid out and chrome moves; a slide done is forgotten.
    fn follow_active_tab(&self, active: Option<u64>, tabbed: bool, cx: &gpui::App) {
        let moves = self.chrome_moves(cx);
        if !moves || self.tabs.slide.get().is_some_and(|slide| slide.since.elapsed() >= TAB_SETTLE)
        {
            self.tabs.slide.set(None);
        }
        let was = self.tabs.active.replace(active);
        if was == active || !tabbed || !moves {
            return;
        }
        let widths = self.tabs.widths.borrow();
        let row = self.tabs.row_at.get();
        let from = was.and_then(|id| widths.get(&id)).map(|(x, w)| (x - row, *w));
        let known = active.is_some_and(|id| widths.contains_key(&id));
        if let (Some(from), Some(to), true) = (from, active, known) {
            let seq = self.tabs.slides.get().wrapping_add(1);
            self.tabs.slides.set(seq);
            self.tabs.slide.set(Some(Slide { from, to, seq, since: Instant::now() }));
        }
    }

    /// The active tab's fill while it slides: one plate in the tab's box whose left edge and
    /// width move from the old tab's to the new one's, drawn under the words. The new tab
    /// draws its own fill once it lands.
    fn render_sliding_fill(&self) -> Option<gpui::AnyElement> {
        let slide = self.tabs.slide.get()?;
        let (to_x, to_w) = self
            .tabs
            .widths
            .borrow()
            .get(&slide.to)
            .map(|(x, w)| (x - self.tabs.row_at.get(), *w))?;
        let theme = &self.theme;
        let (from_x, from_w) = slide.from;
        let curve = kit::Pace::Settle.curve();
        let plate = self
            .tab_box()
            .debug_selector(|| "ws-tab-fill".to_owned())
            .absolute()
            .bottom_0()
            .rounded_t(px(theme.radii.sm))
            .bg(hsla(theme.content()))
            .border_color(hsla(theme.surfaces.border));
        Some(
            plate
                .with_animation(
                    ("ws-tab-fill", slide.seq),
                    Animation::new(TAB_SETTLE),
                    move |el, t| {
                        let e = curve.at(t);
                        el.left(px(e.mul_add(to_x - from_x, from_x)))
                            .w(px(e.mul_add(to_w - from_w, from_w)))
                    },
                )
                .into_any_element(),
        )
    }

    /// A tab that just appeared, growing from nothing to the width its name takes over a
    /// settle while its neighbours slide aside; its words fade in over the last part, once
    /// there is room for them.
    fn open_tab(
        &self,
        tab: gpui::AnyElement,
        id: u64,
        ids: &[(u64, String)],
        window: &Window,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let name = ids.iter().find(|(o, _)| *o == id).map_or("", |(_, n)| n.as_str());
        let mut style = window.text_style();
        style.font_weight = gpui::FontWeight(Typography::MEDIUM_WEIGHT);
        let run = style.to_run(name.len());
        let words = window
            .text_system()
            .shape_line(
                SharedString::from(name.to_owned()),
                px(theme.typography.ui_size),
                &[run],
                None,
            )
            .width;
        let full = (f32::from(words) + 2.0_f32.mul_add(theme.spacing.md, 2.0 * TAB_HAIRLINE))
            .clamp(TAB_MIN_W, TAB_MAX_W);
        let curve = kit::Pace::Settle.curve();
        div()
            .debug_selector(move || format!("ws-tab-opening-{id}"))
            .flex_none()
            .h_full()
            .flex()
            .items_end()
            .overflow_hidden()
            .child(tab)
            .with_animation(("ws-tab-opening", id), Animation::new(TAB_SETTLE), move |el, t| {
                let words = ((t - LABEL_WAITS) / (1.0 - LABEL_WAITS)).clamp(0.0, 1.0);
                el.w(px(full * curve.at(t))).opacity(words)
            })
            .into_any_element()
    }

    /// Note which tabs went since the last drawing, and which came: each that went folds away
    /// where it stood and each that came grows in, unless motion is reduced (or moves are
    /// off, as under the self-test). Those done folding or growing, or back, are forgotten.
    fn fold_closed_tabs(&self, ids: &[(u64, String)], cx: &gpui::App) {
        // The fold runs on the wall clock, as GPUI's animations do.
        let now = Instant::now();
        let (mut closing, mut opening) =
            (self.tabs.closing.borrow_mut(), self.tabs.opening.borrow_mut());
        closing.retain(|gone| {
            now.saturating_duration_since(gone.since) < TAB_SETTLE
                && !ids.iter().any(|(kept, _)| *kept == gone.id)
        });
        opening.retain(|(_, since)| now.saturating_duration_since(*since) < TAB_SETTLE);
        let moves = self.chrome_moves(cx);
        if !moves {
            closing.clear();
            opening.clear();
        }
        let mut drawn = self.tabs.drawn.borrow_mut();
        // The first drawing opens nothing: the bar was not there to grow in.
        if moves && !drawn.is_empty() {
            let widths = self.tabs.widths.borrow();
            for (at, (id, name)) in drawn.iter().enumerate() {
                if ids.iter().any(|(kept, _)| kept == id) {
                    continue;
                }
                let width = widths.get(id).map_or(TAB_MIN_W, |(_, w)| *w);
                closing.push(ClosingTab { id: *id, name: name.clone(), width, at, since: now });
            }
            for (id, _) in ids {
                if !drawn.iter().any(|(was, _)| was == id) {
                    opening.push((*id, now));
                }
            }
        }
        *drawn = ids.to_vec();
        self.tabs.widths.borrow_mut().retain(|id, _| {
            ids.iter().any(|(kept, _)| kept == id) || closing.iter().any(|gone| gone.id == *id)
        });
    }

    /// A tab's box: from its band's top, on the bar's midline, down through the bar's edge,
    /// so the active one joins the content under it. Every tab takes the same box, so
    /// switching moves none.
    fn tab_box(&self) -> gpui::Div {
        let theme = &self.theme;
        let bar = titlebar_height(theme);
        // The bar's content stands on its hairline, so its midline is half a point above
        // the bar's own; the box's own top edge (a hairline, drawn on the active tab) sits
        // above the band.
        let above = (bar - TAB_HAIRLINE - tab_band(theme)) / 2.0 - TAB_HAIRLINE;
        div()
            .relative()
            .flex_none()
            .h(px(bar - above))
            .mb(px(-TAB_HAIRLINE))
            .flex()
            .flex_col()
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(gpui::transparent_black())
    }

    /// A tab that went, folding its width away over [`TAB_SETTLE`].
    fn render_closing_tab(&self, id: u64, name: &str, width: f32) -> gpui::AnyElement {
        let theme = &self.theme;
        self.tab_box()
            .id(("ws-tab-closing", id))
            .debug_selector(move || format!("ws-tab-closing-{id}"))
            .w(px(width))
            .overflow_hidden()
            .child(
                div()
                    .h(px(tab_band(theme)))
                    .flex()
                    .items_center()
                    .px(px(theme.spacing.md))
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.ui_size))
                    .text_color(hsla(theme.surfaces.text_secondary))
                    .child(SharedString::from(name.to_owned())),
            )
            .with_animation(
                ("ws-tab-closing", id),
                Animation::new(TAB_SETTLE).with_easing(kit::ease_out()),
                move |el, delta| el.w(px(width * (1.0 - delta))).opacity(1.0 - delta),
            )
            .into_any_element()
    }

    /// What a workspace's tab says to a screen reader: its name, how many tiles it holds, and
    /// what they add up to.
    fn tab_words(&self, ix: usize) -> (SharedString, Rollup) {
        let name = self.workspace_name_at(ix);
        let (rollup, count) = self.workspace_rollup(ix);
        let noun = if count == 1 { "tile" } else { "tiles" };
        let label = match rollup.words() {
            Some(words) => format!("{name}, {count} {noun}, {words}"),
            None => format!("{name}, {count} {noun}"),
        };
        (label.into(), rollup)
    }

    /// One workspace's tab: its name and its rollup. The active one is the medium weight in
    /// the text colour on the content's; the rest are the regular weight a step quieter,
    /// taking a row's hover fill in their band. While the active fill slides to it
    /// (`sliding`), the tab leaves its fill to the sliding plate.
    fn render_tab(
        &self,
        ix: usize,
        selected: bool,
        sliding: bool,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        let name = SharedString::from(self.workspace_name_at(ix));
        let (label, rollup) = self.tab_words(ix);
        let id = self.layout.workspaces().get(ix).map_or(0, slopty_client::layout::Workspace::id);
        let widths = Rc::clone(&self.tabs.widths);
        let measure = canvas(
            move |bounds, _window, _cx| {
                let at = (f32::from(bounds.origin.x), f32::from(bounds.size.width));
                widths.borrow_mut().insert(id, at);
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let group = SharedString::from(format!("ws-tab-{ix}"));
        let medium = gpui::FontWeight(Typography::MEDIUM_WEIGHT);
        let weight = if selected { medium } else { gpui::FontWeight::NORMAL };
        let band = div()
            .debug_selector(move || format!("ws-tab-band-{ix}"))
            .flex_none()
            .h(px(tab_band(theme)))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.md))
            .rounded(px(theme.radii.sm))
            .when(!selected, |el| el.group_hover(group.clone(), move |el| el.bg(hsla(s.raised))))
            .child(
                // Laid out in the medium weight whichever it is drawn in, so a tab is as wide
                // active as not and switching moves no neighbour.
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(theme.typography.ui_size))
                    .child(one_line(div().invisible().font_weight(medium)).child(name.clone()))
                    .child(
                        one_line(div().absolute().inset_0())
                            .font_weight(weight)
                            .text_color(hsla(if selected { s.text } else { s.text_secondary }))
                            .child(name),
                    ),
            )
            .when(rollup.shown().is_some(), |el| {
                el.child(rollup_slot(theme, format!("ws-rollup-{ix}"), rollup, true))
            });
        let tab = self
            .tab_box()
            .id(("ws-tab", ix))
            .debug_selector(move || format!("ws-tab-{ix}"))
            .group(group)
            .role(Role::Tab)
            .aria_label(label)
            .aria_selected(selected)
            .flex_shrink(1.0)
            .min_w(px(TAB_MIN_W))
            .max_w(px(TAB_MAX_W))
            .rounded_t(px(theme.radii.sm))
            .cursor_pointer()
            .when(selected && !sliding, |el| {
                el.bg(hsla(theme.content())).border_color(hsla(s.border))
            })
            .child(measure)
            .child(band)
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_workspace(ix, cx)));
        tab_stop(tab, s.accent).into_any_element()
    }

    /// The one workspace there is: its name in the medium weight, nothing to switch between,
    /// so no tab. Nor a count or a rollup: the navigator and the overview count its tiles, and
    /// with one workspace a mark says only what the bell's badge already counts.
    fn render_lone_workspace(&self, ix: usize) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (label, _rollup) = self.tab_words(ix);
        div()
            .id(("ws-tab", ix))
            .debug_selector(move || format!("ws-tab-{ix}"))
            .role(Role::Heading)
            .aria_label(label)
            .flex_shrink(1.0)
            .min_w_0()
            .flex()
            .items_center()
            .child(
                div()
                    .debug_selector(move || format!("ws-name-{ix}"))
                    .min_w_0()
                    .max_w(px(TAB_MAX_W))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(theme.typography.ui_size))
                    .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(self.workspace_name_at(ix))),
            )
            .into_any_element()
    }

    /// "+"'s choice of worker, where there are several: a row each, the one a new tile goes to
    /// checked. Choosing one keeps the menu open on the kinds of tile.
    fn target_entries(&self, entity: &gpui::WeakEntity<Self>) -> Vec<MenuEntry> {
        if self.workers.len() < 2 {
            return Vec::new();
        }
        let target = self.new_on.or_else(|| self.context_worker());
        self.workers
            .iter()
            .map(|(key, w)| {
                let key = *key;
                let entity = entity.clone();
                MenuEntry {
                    group: MenuGroup::Target,
                    label: w.name.clone().into(),
                    detail: if target == Some(key) {
                        "\u{2713}".into()
                    } else {
                        SharedString::default()
                    },
                    run: Rc::new(move |_window, cx| {
                        let _gone = entity.update(cx, |this, cx| {
                            this.new_on = Some(key);
                            this.menu = Some(MenuKind::New);
                            cx.notify();
                        });
                    }),
                }
            })
            .collect()
    }

    /// What "+" opens, as `entry` makes a row: the palette's names for the same actions,
    /// which the rows run as the keys do.
    fn new_entries(
        entry: &impl Fn(MenuGroup, &'static str, Option<&dyn gpui::Action>, MenuAction) -> MenuEntry,
    ) -> Vec<MenuEntry> {
        vec![
            entry(MenuGroup::Tiles, "New terminal", Some(&NewTerminal), |this, w, cx| {
                this.new_terminal(&NewTerminal, w, cx);
            }),
            entry(MenuGroup::Tiles, "New agent", Some(&NewAgent), |this, w, cx| {
                this.new_agent(&NewAgent, w, cx);
            }),
            entry(MenuGroup::Tiles, "Add a window or display", Some(&AddWindow), |this, w, cx| {
                this.add_window(&AddWindow, w, cx);
            }),
            entry(MenuGroup::Tiles, "New note", Some(&NewNote), |this, w, cx| {
                this.new_note(&NewNote, w, cx);
            }),
            entry(MenuGroup::Workspaces, NEW_WORKSPACE, None, |this, _w, cx| {
                // The layout always keeps an empty workspace last.
                let last = this.layout.workspaces().len().saturating_sub(1);
                this.go_to_workspace(last, cx);
            }),
        ]
    }

    /// The open menu, anchored under its button at the right of the bar.
    pub(super) fn render_menu(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Rows run on the workspace itself: with nothing focused, a dispatched action would
        // never reach its handlers.
        type Run = MenuAction;
        let which = self.menu?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let entity = cx.entity().downgrade();
        let bindings = super::key_bindings();
        // The row's keys come from the binding table, spelled as the palette spells them; a
        // phone's menu shows none, as its bar shows no hints.
        let entry =
            |group: MenuGroup, label: &'static str, bound: Option<&dyn gpui::Action>, run: Run| {
                let entity = entity.clone();
                let detail = match bound {
                    Some(bound) if SHORTCUT_HINTS => crate::palette::keys_for(bound, &bindings),
                    _ => String::new(),
                };
                MenuEntry {
                    group,
                    label: label.into(),
                    detail: detail.into(),
                    run: Rc::new(move |window, cx| {
                        let _gone = entity.update(cx, |this, cx| run(this, window, cx));
                    }),
                }
            };
        let action = |label: &'static str, bound: &dyn gpui::Action, run: Run| {
            entry(MenuGroup::Navigation, label, Some(bound), run)
        };
        let entries: Vec<MenuEntry> = match which {
            MenuKind::Inbox => Vec::new(),
            // The palette's names for the same actions, which the rows run as the keys do.
            MenuKind::New => {
                self.target_entries(&entity).into_iter().chain(Self::new_entries(&entry)).collect()
            }
            MenuKind::More => {
                // A phone's bar has no "+": what it opens leads its "…".
                let phone = self.phone_bar(window);
                let mut entries: Vec<MenuEntry> =
                    if phone { Self::new_entries(&entry) } else { Vec::new() };
                entries.extend([
                    action("Command palette", &OpenPalette, |this, w, cx| {
                        this.open_palette(&OpenPalette, w, cx);
                    }),
                    action("Overview", &super::actions::ToggleOverview, |this, _w, cx| {
                        this.tick();
                        this.layout.toggle_overview();
                        cx.notify();
                    }),
                    action("Stream stats", &ToggleStats, |this, w, cx| {
                        this.toggle_stats(&ToggleStats, w, cx);
                    }),
                ]);
                entries.extend(self.more_entries.iter().cloned());
                // The hosts popover, which the status bar's count opens only while a worker is
                // down. A phone's bar has no room for the popover, nor this row.
                if !self.workers.is_empty() && !phone {
                    let entity = entity.clone();
                    entries.push(MenuEntry {
                        group: MenuGroup::Connections,
                        label: "Workers".into(),
                        detail: SharedString::default(),
                        run: Rc::new(move |_window, cx| {
                            let _gone = entity.update(cx, Self::toggle_hosts);
                        }),
                    });
                }
                entries.sort_by_key(|entry| entry.group);
                entries
            }
        };
        let mut rows: Vec<gpui::AnyElement> = Vec::with_capacity(entries.len());
        let mut group = entries.first().map(|entry| entry.group);
        for (i, entry) in entries.into_iter().enumerate() {
            if group != Some(entry.group) {
                group = Some(entry.group);
                rows.push(
                    div()
                        .debug_selector(move || format!("menu-separator-{i}"))
                        .flex_none()
                        .my(px(spacing.xs))
                        .h(px(1.0))
                        .bg(hsla(s.border_subtle))
                        .into_any_element(),
                );
            }
            rows.push({
                let run = Rc::clone(&entry.run);
                let entity = entity.clone();
                let row = kit::row(theme, kit::Row::One)
                    .id(("menu-row", i))
                    .debug_selector({
                        let label = entry.label.clone();
                        move || format!("menu-{label}")
                    })
                    .role(Role::MenuItem)
                    .aria_label(entry.label.clone())
                    .gap(px(spacing.md))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(theme.typography.ui_size))
                            .text_color(hsla(s.text))
                            .child(entry.label.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(theme.typography.small()))
                            .text_color(hsla(s.text_muted))
                            .child(entry.detail.clone()),
                    );
                tab_stop(row, s.accent)
                    .on_click(move |_ev, window, cx| {
                        // The menu closes first, so the entry runs with the focus back where
                        // it was.
                        let _closed = entity.update(cx, |this, cx| this.close_menu(window, cx));
                        run(window, cx);
                    })
                    .into_any_element()
            });
        }
        // A base unit below the bar (the inbox keeps that gap itself), the right edge on the
        // window's inset, where the tiles' headers end: a popover lined up with what it covers
        // rather than hung off its button a few points in.
        let gap = match which {
            MenuKind::Inbox => 0.0,
            MenuKind::New | MenuKind::More => spacing.xs,
        };
        let panel = if which == MenuKind::Inbox {
            self.render_inbox(cx)
        } else {
            self.menu_panel(rows, which, cx)
        };
        // A click anywhere else closes it and goes no further, so a press on the button that
        // opened it closes it rather than opening it again. A popover, it paints over the frame
        // and the navigator laid over it, under a dialog.
        let away = div()
            .id("menu-away")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| this.close_menu(window, cx)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(titlebar_height(theme) + gap) + safe.top)
                    // "+" hangs its menu from its own left edge, as a menu bar's menus do.
                    .map(|el| match which {
                        MenuKind::New => el.left(px(self.tabs.new_at.get())),
                        MenuKind::Inbox | MenuKind::More => {
                            el.right(px(spacing.inset()) + safe.right)
                        }
                    })
                    .child(panel),
            );
        let layer = crate::palette::Layer::Popover.priority();
        Some(gpui::deferred(away).with_priority(layer).into_any_element())
    }

    /// A menu's panel around its rows, fading in as it drops a base unit from its button.
    fn menu_panel(
        &self,
        rows: Vec<gpui::AnyElement>,
        which: MenuKind,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let panel = kit::elevate(div(), theme)
            .id("menu")
            .debug_selector(|| "menu".to_owned())
            .role(Role::Menu)
            .occlude()
            .w(px(260.0))
            .flex()
            .flex_col()
            .py(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .children(rows);
        if self.menu_keyed || !self.chrome_moves(cx) {
            return panel.into_any_element();
        }
        let id = SharedString::from(format!("menu-in-{which:?}"));
        kit::slide_fade(panel, id, -spacing.xs, kit::Pace::Fade, cx)
    }
}

/// `el` as one line of words, cut with an ellipsis where it runs out of room.
fn one_line(el: gpui::Div) -> gpui::Div {
    el.overflow_hidden().whitespace_nowrap().text_ellipsis()
}
