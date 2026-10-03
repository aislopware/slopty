//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, the breadcrumb of
//! where the focused work is (`workspace ▾ / checkout ▾ / branch`, `breadcrumb.rs`, whose
//! workspace menu is how the bar goes between workspaces) and "+" after it (a menu of what to
//! open: a terminal, an agent, a window, a note, or a workspace); the inbox's bell and "…" on
//! the right. Where the view is along the strip is the strip's own thumb (`marks`), not the
//! bar's. Nothing else: every other action is a key, the palette, or a tile's own header, and
//! the readouts (the server's state among them) live in the status bar.
//!
//! It takes the navigator's tone with no rule under it, so the two read as one frame round the
//! content, as `MonoCode`'s and Zed's do. A menu fades in as it drops 4 pt from its button, at
//! once under Reduce Motion.
//!
//! On a phone the bar is a navigation bar: the workspace's name alone, as the breadcrumb's
//! first segment says it (the body's size, the medium weight), and what "+" opens folded into
//! "…".
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas,
    div, px,
};
use slopty_client::groups::Group;
use slopty_client::layout::{Column, Tile, WorkerKey};
use slopty_theme::Typography;

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::Rollup;
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

/// The bell's hover group, which its badge's cut-out ring follows.
const BELL: &str = "bell";

/// What "+" is called: it opens a menu of things to open.
pub(super) const NEW: &str = "New";

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// What a menu row runs, on the workspace itself.
type MenuAction = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum MenuKind {
    /// The breadcrumb's workspace: every workspace, and a new one.
    Workspaces,
    /// The breadcrumb's checkout: the same repository's other checkouts.
    Checkouts,
    /// "+": what to open.
    New,
    /// "…": everything else, the app's entries included.
    More,
    /// The bell: what needs the human and what finished.
    Inbox,
}

/// Where the bar's buttons that hang a menu were last laid out: each one's left edge in the
/// window, by the menu it opens.
#[derive(Default, Debug)]
pub(super) struct Anchors {
    pub at: Rc<RefCell<HashMap<MenuKind, f32>>>,
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

    /// Workspace `ix`'s name: the one given, else the project most of its tiles are in (a tie
    /// going to the one first in the strip, so the name holds as the focus moves), which is the
    /// worker's name where that is all its tiles have in common. Never a number, nor a tile's
    /// title, which follows every command run and every page loaded. One with nothing on it is
    /// new.
    pub(super) fn workspace_name_at(&self, ix: usize) -> String {
        let Some(ws) = self.layout.workspaces().get(ix) else { return NEW_WORKSPACE.to_owned() };
        if let Some(name) = ws.name() {
            return name.to_owned();
        }
        let projects = self.project_groups();
        // The project of the most tiles; a project before a machine, which only says where.
        let mut counts: Vec<(&Group, usize)> = Vec::new();
        for tile in ws.columns().iter().flat_map(Column::tiles).map(Tile::tile) {
            let Some(group) = projects.group_of(tile) else { continue };
            match counts.iter_mut().find(|(g, _)| g.key == group.key) {
                Some((_, n)) => *n = n.saturating_add(1),
                None => counts.push((group, 1)),
            }
        }
        let best = counts.iter().enumerate().max_by_key(|(order, (g, n))| {
            (g.key.worker().is_none(), *n, std::cmp::Reverse(*order))
        });
        best.map(|(_, (group, _))| *group)
            .map_or_else(|| NEW_WORKSPACE.to_owned(), |group| self.group_name(group))
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

    pub(super) fn toggle_menu(
        &mut self,
        which: MenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu == Some(which) {
            self.dismiss_menu(window, cx);
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

    /// Close the bar's menu with nothing chosen from it: a machine "+" was pointed at goes
    /// with it, so the next ⌘T is not sent there unasked.
    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_on = None;
        self.close_menu(window, cx);
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
        let place = has_workers
            .then(|| if phone { self.render_phone_title() } else { self.render_breadcrumb(cx) });
        let theme = &self.theme;
        let s = &theme.surfaces;

        // Left: the navigator's toggle, then where the focused work is.
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
            let at = Rc::clone(&self.anchors.at);
            let measure = canvas(
                move |bounds, _window, _cx| {
                    at.borrow_mut().insert(MenuKind::New, f32::from(bounds.origin.x));
                },
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
        let total = self.drawn_waiting.len().saturating_add(self.drawn_thread_waits.len());
        let unread = total.saturating_add(self.unread_finishes());
        let bell = has_workers.then(|| {
            // Each fill with its own ink: a near-black on the green and on a state's fill.
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
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .children(toggle)
                    .children(place)
                    .children(new),
            )
            .child(buttons)
            .into_any_element()
    }

    /// A phone's title: the active workspace's name, as an iOS navigation bar names its screen,
    /// at the size and weight the breadcrumb names it in on a wider window: the chrome has no
    /// display type. The navigator and a swipe go between workspaces; a breadcrumb has no room
    /// at this width.
    fn render_phone_title(&self) -> gpui::AnyElement {
        let ix = self.layout.active_workspace();
        let theme = &self.theme;
        let (label, _rollup) = self.workspace_words(ix);
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
            .text_size(px(theme.typography.ui_size))
            .font_weight(gpui::FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(theme.surfaces.text))
            .child(SharedString::from(self.workspace_name_at(ix)))
            .into_any_element()
    }

    /// What a workspace's name says to a screen reader: its name, how many tiles it holds, and
    /// what they add up to.
    fn workspace_words(&self, ix: usize) -> (SharedString, Rollup) {
        let name = self.workspace_name_at(ix);
        let (rollup, count) = self.workspace_rollup(ix);
        let noun = if count == 1 { "tile" } else { "tiles" };
        let label = match rollup.words() {
            Some(words) => format!("{name}, {count} {noun}, {words}"),
            None => format!("{name}, {count} {noun}"),
        };
        (label.into(), rollup)
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
            entry(MenuGroup::Tiles, "New agent\u{2026}", Some(&NewAgent), |this, w, cx| {
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
            MenuKind::Workspaces => self.workspace_entries(&entity),
            MenuKind::Checkouts => self.checkout_entries(&entity),
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
                        label: "Machines".into(),
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
                let row = kit::sheet_row(theme, kit::Row::One)
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
            MenuKind::New | MenuKind::More | MenuKind::Workspaces | MenuKind::Checkouts => {
                spacing.xs
            }
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
                cx.listener(|this, _ev, window, cx| this.dismiss_menu(window, cx)),
            )
            .child(
                div()
                    .absolute()
                    .top(px(titlebar_height(theme) + gap) + safe.top)
                    // "+" and the breadcrumb hang their menus from their own left edges, as a
                    // menu bar's menus do.
                    .map(|el| match which {
                        MenuKind::New | MenuKind::Workspaces | MenuKind::Checkouts => {
                            let at = self.anchors.at.borrow().get(&which).copied();
                            el.left(px(at.unwrap_or_else(|| spacing.inset())))
                        }
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
            .p(px(kit::sheet_pad(theme)))
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
