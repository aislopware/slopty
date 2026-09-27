//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, the workspaces as
//! tabs with "+" after them (a menu of what to open: a terminal, an agent, a window, a note, or
//! a workspace), and the active workspace's column dots while some column is out of view; the
//! inbox's bell and "…" on the right. Nothing else: every other action is a key, the
//! palette, or a tile's own header, and the readouts (the server's state among them) live in
//! the status bar.
//!
//! A tab is as wide as its name, from 64 to 180 pt. The active one stands on the content's
//! colour with the bar's edge broken under it, so it joins what it shows, as an editor's tab
//! does; the rest are quiet words. Where the bar has room each tab grows to two lines, its name
//! over what it holds. A single workspace is no tab at all, only its name in the strong weight.
//! Each ends in what its tiles add up to (the rollup), a dot at most. A tab that closes folds
//! its width away so its neighbours slide into its place, at once under Reduce Motion.
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
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, ease_out_quint, px,
};
use slopty_client::layout::{Column, Strip, Tile, WorkerKey};
use slopty_theme::{Typography, alpha};

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::{Rollup, rollup_slot};
use super::strip::NEW_WORKSPACE;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
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

/// The least width of a tab of two lines: below it the tab keeps one.
const TAB_WIDE_W: f32 = 176.0;

/// How long a closing tab takes to fold its width away.
const TAB_SETTLE: Duration = Duration::from_millis(160);

/// What "+" is called: it opens a menu of things to open.
pub(super) const NEW: &str = "New";

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// The column marks: a dot's diameter and the room it takes, at most this many dots at full
/// size before they shrink to fit.
const DOT: f32 = 6.0;
const DOTS_AT_FULL_SIZE: usize = 12;

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

/// The tabs as last drawn, so a tab that goes can fold away where it was.
#[derive(Default)]
pub(super) struct Tabs {
    /// Each tab drawn, by its workspace's id: its name.
    drawn: Vec<(u64, String)>,
    /// Each tab's width as laid out, by its workspace's id.
    widths: Rc<RefCell<HashMap<u64, f32>>>,
    /// Tabs folding away: the workspace's id, its name and width, where it stood, since when.
    closing: Vec<(u64, String, f32, usize, Instant)>,
    /// The left edge of "+" in the window as last laid out, where its menu hangs from.
    new_at: Rc<Cell<f32>>,
}

impl std::fmt::Debug for Tabs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tabs").field("drawn", &self.drawn.len()).finish_non_exhaustive()
    }
}

/// Whether every column of `strip` is in its view, so the dots would say nothing.
pub(super) fn all_in_view(strip: &Strip) -> bool {
    let (view_x, view_w) = strip.view;
    strip.columns.iter().all(|(x, w)| *x >= view_x - 1.0 && x + w <= view_x + view_w + 1.0)
}

impl WorkspaceView {
    /// Whether `key`'s round trip, going from `was` to `now`, is on screen: the status bar
    /// shows the focused tile's worker's, the hosts popover every worker's, and the navigator
    /// those slow enough to name.
    pub(super) fn rtt_shown(
        &self,
        key: WorkerKey,
        was: Option<Duration>,
        now: Option<Duration>,
    ) -> bool {
        let slow =
            |rtt: Option<Duration>| rtt.is_some_and(|rtt| rtt >= super::navigator::RTT_SHOWN_FROM);
        self.status_worker() == Some(key)
            || self.hosts_open()
            || (self.nav.drawn.is_some() && (slow(was) || slow(now)))
    }

    /// The active workspace's name: the one given, else its place.
    #[must_use]
    pub fn workspace_name(&self) -> String {
        self.workspace_name_at(self.layout.active_workspace())
    }

    /// Workspace `ix`'s name: the one given, else where its first shell is (the repository,
    /// else the directory), else its number. A number says nothing a tab's place does not.
    pub(super) fn workspace_name_at(&self, ix: usize) -> String {
        let ws = self.layout.workspaces().get(ix);
        ws.and_then(|ws| ws.name().map(str::to_owned))
            .or_else(|| ws.and_then(|ws| self.workspace_place(ws)))
            .unwrap_or_else(|| format!("Workspace {}", ix.saturating_add(1)))
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

    /// What workspace `ix`'s tiles add up to, how many there are, and on how many workers.
    pub(super) fn workspace_rollup(&self, ix: usize, cx: &gpui::App) -> (Rollup, usize, usize) {
        let mut rollup = Rollup::default();
        let mut count = 0_usize;
        let mut workers: Vec<WorkerKey> = Vec::new();
        let Some(ws) = self.layout.workspaces().get(ix) else { return (rollup, count, 0) };
        for tile in ws.columns().iter().flat_map(Column::tiles).map(Tile::tile) {
            count = count.saturating_add(1);
            if !workers.contains(&tile.worker) {
                workers.push(tile.worker);
            }
            if let Some(item) = self.item(tile) {
                let (mark, unseen) = self.tile_marks(tile, item, cx);
                rollup.add(mark, unseen);
            }
        }
        (rollup, count, workers.len())
    }

    fn toggle_menu(&mut self, which: MenuKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu == Some(which) {
            self.close_menu(window, cx);
        } else {
            self.menu = Some(which);
            cx.notify();
        }
    }

    /// Close the bar's menu and hand the keyboard back to the focused tile at once.
    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.return_keyboard(window, cx);
        cx.notify();
    }

    /// The bar. `safe_top` is the notch's inset on a phone, zero on a Mac.
    pub(super) fn render_titlebar(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
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
        let bar_w = self.width(window)
            - if docked { self.navigator_width() } else { 0.0 }
            - leading
            - trailing;
        let tabs = has_workers.then(|| self.render_workspace_tabs(bar_w, cx));
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
        let new = has_workers.then(|| {
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
        let dots = self.render_indicator(cx);

        // Right: the inbox and "…". Who needs you is counted once, on the bell; its rows go to
        // them.
        let total = self.drawn_waiting.len();
        let unread = total.saturating_add(self.finished.len());
        let bell = has_workers.then(|| {
            let fill = if total > 0 { s.warn_fill } else { s.accent_fill };
            let badge = (unread > 0).then(|| {
                let count = SharedString::from(unread.to_string());
                let side = theme.typography.caption() + spacing.xs;
                kit::tabular(div())
                    .id("bell-count")
                    .debug_selector(|| "bell-count".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("{unread} new")))
                    .absolute()
                    .top(px(-spacing.xxs))
                    .right(px(-spacing.xxs))
                    .h(px(side))
                    .min_w(px(side))
                    .px(px(spacing.xxs))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(hsla(fill))
                    .text_color(hsla(s.fill_fg))
                    .text_size(px(theme.typography.caption()))
                    .child(count)
            });
            kit::icon_button(theme, "bell", IconName::Bell, "Inbox")
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
                    .children(new)
                    .children(dots),
            )
            .child(buttons)
            .into_any_element()
    }

    /// The workspaces: a tab each where there are several, the name alone where there is one.
    fn render_workspace_tabs(&mut self, bar_w: f32, cx: &Context<Self>) -> gpui::AnyElement {
        let active = self.layout.active_workspace();
        let tabbed = self.tabbed_workspaces();
        let ids: Vec<(u64, String)> = tabbed
            .iter()
            .filter_map(|ix| {
                let ws = self.layout.workspaces().get(*ix)?;
                Some((ws.id(), self.workspace_name_at(*ix)))
            })
            .collect();
        self.fold_closed_tabs(&ids, cx);
        // Room for two lines when each tab would get the wide width, past the toggle, "+",
        // the dots and the buttons on the right.
        let reserve = kit::icon_button_side(&self.theme).mul_add(5.0, self.theme.spacing.xl * 2.0);
        #[expect(clippy::cast_precision_loss, reason = "a handful of tabs")]
        let wide = bar_w - reserve >= TAB_WIDE_W * tabbed.len() as f32;
        if let [only] = tabbed.as_slice() {
            return self.render_lone_workspace(*only, wide, cx);
        }
        let mut tabs: Vec<gpui::AnyElement> =
            tabbed.iter().map(|ix| self.render_tab(*ix, *ix == active, wide, cx)).collect();
        for (id, name, width, at, _) in &self.tabs.closing {
            let ghost = self.render_closing_tab(*id, name, *width);
            tabs.insert((*at).min(tabs.len()), ghost);
        }
        div()
            .id("ws-tabs")
            .debug_selector(|| "ws-tabs".to_owned())
            .role(Role::Group)
            .aria_label("Workspaces")
            .flex_shrink(1.0)
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .flex()
            .items_end()
            .gap(px(self.theme.spacing.xxs))
            .children(tabs)
            .into_any_element()
    }

    /// Note which tabs went since the last drawing: each folds away where it stood, unless
    /// motion is reduced (or moves are off, as under the self-test). Those done folding, or
    /// back, are forgotten.
    fn fold_closed_tabs(&mut self, ids: &[(u64, String)], cx: &gpui::App) {
        // The fold runs on the wall clock, as GPUI's animations do.
        let now = Instant::now();
        self.tabs.closing.retain(|(id, .., since)| {
            now.saturating_duration_since(*since) < TAB_SETTLE
                && !ids.iter().any(|(kept, _)| kept == id)
        });
        if self.animate && kit::motion(cx) {
            let widths = self.tabs.widths.borrow();
            for (at, (id, name)) in self.tabs.drawn.iter().enumerate() {
                if ids.iter().any(|(kept, _)| kept == id) {
                    continue;
                }
                let width = widths.get(id).copied().unwrap_or(TAB_MIN_W);
                self.tabs.closing.push((*id, name.clone(), width, at, now));
            }
        }
        self.tabs.drawn = ids.to_vec();
        let closing = &self.tabs.closing;
        self.tabs.widths.borrow_mut().retain(|id, _| {
            ids.iter().any(|(kept, _)| kept == id) || closing.iter().any(|(gone, ..)| gone == id)
        });
    }

    /// A tab that went, folding its width away over [`TAB_SETTLE`].
    fn render_closing_tab(&self, id: u64, name: &str, width: f32) -> gpui::AnyElement {
        let theme = &self.theme;
        div()
            .id(("ws-tab-closing", id))
            .debug_selector(move || format!("ws-tab-closing-{id}"))
            .flex_none()
            .h_full()
            .w(px(width))
            .overflow_hidden()
            .flex()
            .items_center()
            .px(px(theme.spacing.sm))
            .whitespace_nowrap()
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(theme.surfaces.text_secondary))
            .child(SharedString::from(name.to_owned()))
            .with_animation(
                ("ws-tab-closing", id),
                Animation::new(TAB_SETTLE).with_easing(ease_out_quint()),
                move |el, delta| el.w(px(width * (1.0 - delta))).opacity(1.0 - delta),
            )
            .into_any_element()
    }

    /// What a workspace's tab says to a screen reader, and its meta line: how many tiles it
    /// holds and on how many workers.
    fn tab_words(&self, ix: usize, cx: &gpui::App) -> (SharedString, SharedString, Rollup) {
        let name = self.workspace_name_at(ix);
        let (rollup, count, workers) = self.workspace_rollup(ix, cx);
        let noun = if count == 1 { "tile" } else { "tiles" };
        let label = match rollup.words() {
            Some(words) => format!("{name}, {count} {noun}, {words}"),
            None => format!("{name}, {count} {noun}"),
        };
        let meta = match workers {
            // An empty workspace says so in its strip; "0 tiles" here would say it twice.
            0 => String::new(),
            1 => format!("{count} {noun}"),
            n => format!("{count} {noun} \u{b7} {n} workers"),
        };
        (label.into(), meta.into(), rollup)
    }

    /// One workspace's tab: its name (over its meta line when `wide`) and its rollup.
    fn render_tab(
        &self,
        ix: usize,
        selected: bool,
        wide: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        let name = SharedString::from(self.workspace_name_at(ix));
        let (label, meta, rollup) = self.tab_words(ix, cx);
        let id = self.layout.workspaces().get(ix).map_or(0, slopty_client::layout::Workspace::id);
        let widths = Rc::clone(&self.tabs.widths);
        let measure = canvas(
            move |bounds, _window, _cx| {
                widths.borrow_mut().insert(id, f32::from(bounds.size.width));
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let size = if wide { theme.typography.small() } else { theme.typography.ui_size };
        let words = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(size))
                    .text_color(hsla(if selected { s.text } else { s.text_secondary }))
                    .child(name),
            )
            .when(wide, |words| {
                words.child(
                    kit::meta(div(), theme)
                        .debug_selector(move || format!("ws-tab-meta-{ix}"))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(meta),
                )
            });
        let tab = div()
            .id(("ws-tab", ix))
            .debug_selector(move || format!("ws-tab-{ix}"))
            .role(Role::Tab)
            .aria_label(label)
            .relative()
            .flex_shrink(1.0)
            .min_w(px(if wide { TAB_WIDE_W } else { TAB_MIN_W }))
            .max_w(px(TAB_MAX_W))
            // Down over the bar's edge, every tab the same box so switching moves none; the
            // active one fills it and breaks the edge: it joins the content.
            .h(px(titlebar_height(theme) - spacing.xs + 1.0))
            .mb(px(-1.0))
            .pl(px(spacing.sm))
            .pr(px(spacing.xs))
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .rounded_t(px(theme.radii.sm))
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .cursor_pointer()
            .when(selected, |el| el.bg(hsla(theme.content())).border_color(hsla(s.border)))
            .when(!selected, |el| {
                el.border_color(gpui::transparent_black()).hover(move |el| el.bg(hsla(s.raised)))
            })
            .child(measure)
            .child(words)
            .child(rollup_slot(theme, format!("ws-rollup-{ix}"), rollup, true))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_workspace(ix, cx)));
        tab_stop(tab, s.accent).into_any_element()
    }

    /// The one workspace there is: its name in the strong weight (what it holds after it, in
    /// meta, where there is room); nothing to switch between, so no tab. Nor a rollup: with one
    /// workspace it says only what the bell's badge already counts.
    fn render_lone_workspace(&self, ix: usize, wide: bool, cx: &gpui::App) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (label, meta, _rollup) = self.tab_words(ix, cx);
        div()
            .id(("ws-tab", ix))
            .debug_selector(move || format!("ws-tab-{ix}"))
            .role(Role::Heading)
            .aria_label(label)
            // The name is held to a tab's widest, not the name and what it holds together: the
            // meta beside it must not squeeze it.
            .flex_shrink(1.0)
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .child(
                div()
                    .debug_selector(move || format!("ws-name-{ix}"))
                    .min_w_0()
                    .max_w(px(TAB_MAX_W))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(theme.typography.ui_size))
                    .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(self.workspace_name_at(ix))),
            )
            .when(wide && !meta.is_empty(), |el| el.child(kit::meta(div(), theme).flex_none().child(meta)))
            .into_any_element()
    }

    /// One dot per column of the active workspace: the focused column's in the text colour,
    /// those in view a step quieter, the rest faint; a click on one goes to that column.
    /// Nothing where there is nowhere to go: a workspace of one column, every column in view,
    /// or the navigator laid over the bar.
    ///
    /// Dots, not a scaled map of the strip: a track with the view bracketed and the active
    /// column filled read as a progress bar, the loudest thing in the bar saying the least.
    fn render_indicator(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let strip = &self.drawn_strip;
        let count = strip.columns.len();
        let covered = matches!(self.nav.drawn, Some(Mode::Overlay | Mode::Drawer));
        if count < 2 || covered || all_in_view(strip) {
            return None;
        }
        let (view_x, view_w) = strip.view;
        let size = if count > DOTS_AT_FULL_SIZE { DOT - theme.spacing.xxs } else { DOT };
        let marks: Vec<gpui::AnyElement> = strip
            .columns
            .iter()
            .enumerate()
            .map(|(i, (x, w))| {
                let active = strip.active == Some(i);
                let in_view = x + w > view_x + 1.0 && *x < view_x + view_w - 1.0;
                let ink = if active {
                    hsla(s.text)
                } else if in_view {
                    hsla(s.text_muted)
                } else {
                    hsla_alpha(s.text_muted, alpha::PRESSED)
                };
                div()
                    .id(("column", i))
                    .debug_selector(move || format!("column-{i}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("Column {}", i.saturating_add(1))))
                    .flex_none()
                    .py(px(theme.spacing.sm))
                    .px(px(theme.spacing.xxs))
                    .cursor_pointer()
                    .child(div().size(px(size)).rounded_full().bg(ink))
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.tick();
                        this.layout.focus_column(i);
                        this.after_focus_moved(cx);
                        this.layout_touched(cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        let row = div()
            .id("indicator")
            .debug_selector(|| "indicator".to_owned())
            .role(Role::Group)
            .aria_label("Columns")
            .flex_none()
            .flex()
            .items_center()
            .children(marks)
            .into_any_element();
        Some(row)
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

    /// The open menu, anchored under its button at the right of the bar.
    pub(super) fn render_menu(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Rows run on the workspace itself: with nothing focused, a dispatched action would
        // never reach its handlers.
        type Run = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);
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
            MenuKind::New => self
                .target_entries(&entity)
                .into_iter()
                .chain([
                    entry(MenuGroup::Tiles, "New terminal", Some(&NewTerminal), |this, w, cx| {
                        this.new_terminal(&NewTerminal, w, cx);
                    }),
                    entry(MenuGroup::Tiles, "New agent", Some(&NewAgent), |this, w, cx| {
                        this.new_agent(&NewAgent, w, cx);
                    }),
                    entry(
                        MenuGroup::Tiles,
                        "Add a window or display",
                        Some(&AddWindow),
                        |this, w, cx| {
                            this.add_window(&AddWindow, w, cx);
                        },
                    ),
                    entry(MenuGroup::Tiles, "New note", Some(&NewNote), |this, w, cx| {
                        this.new_note(&NewNote, w, cx);
                    }),
                    entry(MenuGroup::Workspaces, NEW_WORKSPACE, None, |this, _w, cx| {
                        // The layout always keeps an empty workspace last.
                        let last = this.layout.workspaces().len().saturating_sub(1);
                        this.go_to_workspace(last, cx);
                    }),
                ])
                .collect(),
            MenuKind::More => {
                let mut entries = vec![
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
                ];
                entries.extend(self.more_entries.iter().cloned());
                // The hosts popover, which the status bar's count opens only while a worker is
                // down. A phone's bar has no room for the popover, nor this row.
                let phone = self.width(window) < self.layout.config().phone_below;
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
            self.menu_panel(rows, cx)
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

    /// A menu's panel around its rows.
    fn menu_panel(&self, rows: Vec<gpui::AnyElement>, _cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        kit::elevate(div(), theme)
            .id("menu")
            .debug_selector(|| "menu".to_owned())
            .role(Role::Menu)
            .occlude()
            .w(px(260.0))
            .flex()
            .flex_col()
            .py(px(spacing.xs))
            .rounded(px(theme.radii.md))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .children(rows)
            .into_any_element()
    }
}
