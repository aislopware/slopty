//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, the workspaces as
//! tabs with "+" after them, the active workspace's column dots, and, quietly, any worker that
//! is down on the left; the inbox's bell, "+" and "…" on the right. Nothing else: every other
//! action is a key, the palette, or a tile's own header, and the readouts live in the status
//! bar.
//!
//! A tab is a fixed width, giving way evenly only when the bar runs out of room, so switching
//! workspaces, renaming one or a mark appearing moves nothing. Each ends in a fixed slot for
//! what its tiles add up to (the rollup): the warn mark and a count, the working mark, or the
//! unseen dot. The dots follow the tabs, in one place whichever tab is active.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Div, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use slopty_client::layout::{Column, Strip, Tile, WorkerKey};
use slopty_theme::{Typography, alpha};

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenFile, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::{Rollup, rollup_slot};
use super::strip::NEW_WORKSPACE;
use super::{MenuEntry, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::IconName;
use crate::kit;

/// The bar's height, under the safe area.
pub const TITLEBAR_H: f32 = 38.0;

/// Room for the traffic lights at the left of the bar, or of the navigator when it shows.
#[cfg(target_os = "macos")]
pub(super) const LEADING_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub(super) const LEADING_INSET: f32 = 12.0;

/// A workspace tab's width, and the least it gives way to when the bar runs out of room.
const TAB_W: f32 = 148.0;
const TAB_MIN_W: f32 = 64.0;

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// The column marks: a dot's diameter and the room it takes, at most this many dots at full
/// size before they shrink to fit.
const DOT: f32 = 6.0;
const DOTS_AT_FULL_SIZE: usize = 12;

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    /// "+": what can be opened.
    Add,
    /// "…": everything else, the app's entries included.
    More,
    /// The bell: what needs the human and what finished.
    Inbox,
}

impl WorkspaceView {
    /// Whether `key`'s round trip is on screen: the status bar shows the focused tile's
    /// worker's, and the navigator every worker's.
    pub(super) fn rtt_shown(&self, key: WorkerKey) -> bool {
        self.nav.drawn.is_some() || self.status_worker() == Some(key)
    }

    /// The active workspace's name: the one given, else its place.
    #[must_use]
    pub fn workspace_name(&self) -> String {
        self.workspace_name_at(self.layout.active_workspace())
    }

    /// Workspace `ix`'s name: the one given, else its place.
    pub(super) fn workspace_name_at(&self, ix: usize) -> String {
        self.layout
            .workspaces()
            .get(ix)
            .and_then(|ws| ws.name().map(str::to_owned))
            .unwrap_or_else(|| format!("Workspace {}", ix.saturating_add(1)))
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
    pub(super) fn workspace_rollup(&self, ix: usize, cx: &gpui::App) -> (Rollup, usize) {
        let mut rollup = Rollup::default();
        let mut count = 0_usize;
        let Some(ws) = self.layout.workspaces().get(ix) else { return (rollup, count) };
        for tile in ws.columns().iter().flat_map(Column::tiles).map(Tile::tile) {
            count = count.saturating_add(1);
            if let Some(item) = self.item(tile) {
                let (mark, unseen) = self.tile_marks(tile, item, cx);
                rollup.add(mark, unseen);
            }
        }
        (rollup, count)
    }

    fn toggle_menu(&mut self, which: MenuKind, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(which) { None } else { Some(which) };
        cx.notify();
    }

    /// The bar. `safe_top` is the notch's inset on a phone, zero on a Mac.
    pub(super) fn render_titlebar(
        &self,
        strip: &Strip,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let small = theme.typography.small();
        // Nothing to open, no column to mark and no one to point at before the first worker:
        // the bar keeps only "…", where settings and the ways to add one live.
        let has_workers = !self.workers.is_empty();
        // A docked navigator holds the traffic lights and the safe area's left edge; the bar
        // starts at its right edge.
        let docked = self.nav.drawn == Some(Mode::Docked);
        let leading = if docked { spacing.sm } else { LEADING_INSET + f32::from(safe.left) };
        let trailing = spacing.md + f32::from(safe.right);

        // Left: the navigator's toggle, the workspaces, the columns of the one in view, and
        // which workers are not there with the human.
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
        let tabs = has_workers.then(|| self.render_workspace_tabs(cx));
        let new_workspace = has_workers.then(|| {
            kit::icon_button(theme, "new-workspace", IconName::Plus, NEW_WORKSPACE).on_click(
                cx.listener(|this, _ev, _w, cx| {
                    // The layout always keeps an empty workspace last.
                    let last = this.layout.workspaces().len().saturating_sub(1);
                    this.go_to_workspace(last, cx);
                }),
            )
        });
        let dots = self.render_indicator(strip, cx);
        let down: Vec<gpui::AnyElement> = self
            .workers
            .iter()
            .filter(|(_, w)| !w.status.is_up())
            .map(|(key, w)| {
                let key = *key;
                div()
                    .id(SharedString::from(format!("down-{key}")))
                    .debug_selector(move || format!("down-{key}"))
                    .role(Role::Status)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .text_size(px(small))
                    .text_color(hsla(s.text_muted))
                    .aria_label(SharedString::from(format!("{}, {}", w.name, w.status.text())))
                    .child(div().size(px(spacing.xs)).rounded_full().bg(hsla(s.warn)))
                    .child(SharedString::from(format!("{} {}", w.name, w.status.text())))
                    .into_any_element()
            })
            .collect();
        let server = self.server_status.clone().map(|text| {
            div()
                .id("server-status")
                .debug_selector(|| "server-status".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_size(px(small))
                .text_color(hsla(s.text_muted))
                .child(div().size(px(spacing.xs)).rounded_full().bg(hsla(s.warn)))
                .child(text)
        });

        // Right: the inbox, "+" and "…". Who needs you is counted on the bell and named in the
        // status bar, which also goes to them.
        let total = self.drawn_waiting.len();
        let unread = total.saturating_add(self.finished.len());
        let bell = has_workers.then(|| {
            let tone = if total > 0 { s.warn } else { s.accent };
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
                    .bg(hsla(tone))
                    .text_color(hsla(s.accent_fg))
                    .text_size(px(theme.typography.caption()))
                    .child(count)
            });
            kit::icon_button(theme, "bell", IconName::Bell, "Inbox")
                .relative()
                .children(badge)
                .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::Inbox, cx)))
        });
        let add = has_workers.then(|| {
            kit::icon_button(theme, "add", IconName::Plus, "Open")
                .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::Add, cx)))
        });
        let more = kit::icon_button(theme, "more", IconName::Ellipsis, "More")
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::More, cx)));
        let buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .children(bell)
            .children(add)
            .child(more);
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .relative()
            .h(px(TITLEBAR_H) + safe.top)
            .pt(safe.top)
            .w_full()
            .flex_none()
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
                    .flex()
                    .items_center()
                    .gap(px(spacing.sm))
                    .children(toggle)
                    .children(tabs)
                    .children(new_workspace)
                    .children(dots)
                    .when_some(server, gpui::ParentElement::child)
                    .children(down),
            )
            .child(buttons)
            .into_any_element()
    }

    /// A tab per workspace worth one: its name and, in a fixed slot at its end, what its tiles
    /// add up to. The active tab sits on `overlay` in the strong weight.
    fn render_workspace_tabs(&self, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        let active = self.layout.active_workspace();
        let height = 2.0_f32.mul_add(spacing.xs, theme.typography.icon_large());
        let tabs: Vec<gpui::AnyElement> = self
            .tabbed_workspaces()
            .into_iter()
            .map(|ix| {
                let name = self.workspace_name_at(ix);
                let (rollup, count) = self.workspace_rollup(ix, cx);
                let noun = if count == 1 { "tile" } else { "tiles" };
                let label = match rollup.words() {
                    Some(words) => format!("{name}, {count} {noun}, {words}"),
                    None => format!("{name}, {count} {noun}"),
                };
                let selected = ix == active;
                let weight =
                    if selected { Typography::STRONG_WEIGHT } else { gpui::FontWeight::NORMAL.0 };
                let tab = div()
                    .id(("ws-tab", ix))
                    .debug_selector(move || format!("ws-tab-{ix}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(label))
                    .flex_shrink(1.0)
                    .w(px(TAB_W))
                    .min_w(px(TAB_MIN_W))
                    .h(px(height))
                    .pl(px(spacing.sm))
                    .pr(px(spacing.xs))
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .rounded(px(theme.radii.sm))
                    .cursor_pointer()
                    .when(selected, |el| el.bg(hsla(s.overlay)))
                    .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.raised))))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(theme.typography.ui_size))
                            .font_weight(gpui::FontWeight(weight))
                            .text_color(hsla(if selected { s.text } else { s.text_secondary }))
                            .child(SharedString::from(name)),
                    )
                    .child(rollup_slot(theme, format!("ws-rollup-{ix}"), rollup))
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_workspace(ix, cx)));
                tab_stop(tab, s.accent).into_any_element()
            })
            .collect();
        div()
            .id("ws-tabs")
            .debug_selector(|| "ws-tabs".to_owned())
            .role(Role::Group)
            .aria_label("Workspaces")
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .children(tabs)
    }

    /// One dot per column of the active workspace: the focused column's in the text colour,
    /// those in view a step quieter, the rest faint; a click on one goes to that column.
    /// Nothing for a workspace of one column, where there is nowhere to go.
    ///
    /// Dots, not a scaled map of the strip: a track with the view bracketed and the active
    /// column filled read as a progress bar, the loudest thing in the bar saying the least.
    fn render_indicator(&self, strip: &Strip, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let count = strip.columns.len();
        if count < 2 {
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
        let action = |label: &'static str, bound: &dyn gpui::Action, run: Run| {
            let entity = entity.clone();
            let detail = if SHORTCUT_HINTS {
                crate::palette::keys_for(bound, &bindings)
            } else {
                String::new()
            };
            MenuEntry {
                label: label.into(),
                detail: detail.into(),
                run: Rc::new(move |window, cx| {
                    let _gone = entity.update(cx, |this, cx| run(this, window, cx));
                }),
            }
        };
        let entries: Vec<MenuEntry> = match which {
            MenuKind::Inbox => Vec::new(),
            MenuKind::Add => vec![
                action("Shell", &NewTerminal, |this, w, cx| this.new_terminal(&NewTerminal, w, cx)),
                action("Agent", &NewAgent, |this, w, cx| this.new_agent(&NewAgent, w, cx)),
                action("Note", &NewNote, |this, w, cx| this.new_note(&NewNote, w, cx)),
                action("Window", &AddWindow, |this, w, cx| this.add_window(&AddWindow, w, cx)),
                action("File", &OpenFile, |this, w, cx| this.open_file_palette(&OpenFile, w, cx)),
            ],
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
                entries
            }
        };
        let rows: Vec<gpui::AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let run = Rc::clone(&entry.run);
                let entity = entity.clone();
                let row = div()
                    .id(("menu-row", i))
                    .debug_selector({
                        let label = entry.label.clone();
                        move || format!("menu-{label}")
                    })
                    .role(Role::MenuItem)
                    .aria_label(entry.label.clone())
                    .flex()
                    .items_center()
                    .gap(px(spacing.md))
                    .px(px(spacing.md))
                    .py(px(spacing.xs))
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
                        let _closed = entity.update(cx, |this, cx| {
                            this.menu = None;
                            cx.notify();
                        });
                        run(window, cx);
                    })
                    .into_any_element()
            })
            .collect();
        let panel = if which == MenuKind::Inbox {
            self.render_inbox(cx)
        } else {
            self.menu_panel(rows, cx)
        };
        // A click anywhere else closes it.
        Some(
            div()
                .id("menu-away")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _w, cx| {
                        this.menu = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(TITLEBAR_H) + safe.top)
                        .right(px(spacing.md) + safe.right)
                        .child(panel),
                )
                .into_any_element(),
        )
    }

    /// The "+" or "…" menu's panel around its rows.
    fn menu_panel(&self, rows: Vec<gpui::AnyElement>, _cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        div()
            .id("menu")
            .debug_selector(|| "menu".to_owned())
            .role(Role::Menu)
            .occlude()
            .w(px(260.0))
            .flex()
            .flex_col()
            .py(px(spacing.xs))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .children(rows)
            .into_any_element()
    }
}
