//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, the breadcrumb of
//! where the focused work is (`workspace ▾ / checkout ▾ / branch`, `breadcrumb.rs`, whose
//! workspace menu is how the bar goes between workspaces) and "+" after it (a menu of what to
//! open: a terminal, an agent, a window, a note, or a workspace); the bell and "…" on the
//! right. The bell counts what needs the person and the agents' turns left to review, and opens
//! the navigator at them. Where the view is along the strip is the strip's own thumb (`marks`), not
//! the bar's. Between them, only while there is something to say: the notices that are about no
//! one tile's work (`toast`), and before the bell the readouts (`readouts`): the server while
//! it does not answer, a plan far used, the ports forwarded, the transfers, a newer Slopty, the
//! frame time with the stats. Every other action is a key, the palette, or a tile's own
//! header. There is no bar along the bottom.
//!
//! It takes the content's tone ([`slopty_theme::Theme::content`]) with no rule under it, so the
//! content runs up to the window's top edge and the navigator is the one panel beside it, as
//! macOS 26 draws a sidebar beside edge-to-edge content and Linear and the Codex app draw a grey
//! sidebar beside a white main area. It used to take the navigator's tone, and with the bar
//! along the bottom the chrome read as a grey frame round the content, an older Electron
//! window's look. A
//! menu fades in as it drops 4 pt from its button, at once under Reduce Motion.
//!
//! On a phone the bar is a navigation bar: the workspace's name alone, as the breadcrumb's
//! first segment says it (the body's size, the medium weight), and what "+" opens folded into
//! "…". It has no room for the readouts, and its notices hang under its middle.
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
use slopty_client::layout::{Column, Tile, TileRef, WorkerKey};
use slopty_theme::Typography;

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::Rollup;
use super::strip::NEW_WORKSPACE;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Status, Symbol};
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
    /// A machine's "…" in the navigator: what it says of itself, and what can be done to it.
    Machine(WorkerKey),
}

/// What the title bar's empty span asks of the window, as a native title bar does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum WindowAsk {
    /// Pressed and moved: the system drags the window after the pointer.
    Move,
    /// Double-clicked: the system's title-bar action, zoom or minimise as the person set it.
    TitleBarDoubleClick,
}

/// Where the buttons that hang a menu were last laid out, in the window, by the menu each
/// opens.
#[derive(Default, Debug)]
pub(super) struct Anchors {
    pub at: Rc<RefCell<HashMap<MenuKind, gpui::Bounds<gpui::Pixels>>>>,
}

impl WorkspaceView {
    /// Ask the window for `ask`; a test keeps it instead, as the test platform drags nothing.
    #[cfg(test)]
    fn window_ask(&mut self, ask: WindowAsk, _window: &Window) {
        self.window_asks.push(ask);
    }

    /// Ask the window for `ask`.
    #[expect(
        clippy::cfg_not_test,
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the test platform panics on a window move, so the test build's twin keeps the \
                  ask on the view instead, with this signature"
    )]
    #[cfg(not(test))]
    fn window_ask(&mut self, ask: WindowAsk, window: &Window) {
        match ask {
            WindowAsk::Move => window.start_window_move(),
            WindowAsk::TitleBarDoubleClick => window.titlebar_double_click(),
        }
    }

    /// What the title bar asked of the window since the last call.
    #[cfg(test)]
    pub(super) fn take_window_asks(&mut self) -> Vec<WindowAsk> {
        std::mem::take(&mut self.window_asks)
    }

    /// Whether chrome moves now: not under Reduce Motion, nor under the self-test, where a
    /// frame is a step and a dump must see where things land.
    pub(super) fn chrome_moves(&self, cx: &gpui::App) -> bool {
        self.animate && kit::motion(cx)
    }

    /// Whether the bar is a phone's navigation bar.
    fn phone_bar(&self, window: &Window) -> bool {
        self.width(window) < self.layout.config().phone_below
    }

    /// Whether a round trip going from `was` to `now` is on screen: the navigator prints those
    /// slow enough to name. A quick link's samples then draw nothing.
    pub(super) fn rtt_shown(&self, was: Option<Duration>, now: Option<Duration>) -> bool {
        let slow =
            |rtt: Option<Duration>| rtt.is_some_and(|rtt| rtt >= super::navigator::RTT_SHOWN_FROM);
        self.nav.drawn.is_some() && (slow(was) || slow(now))
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

    /// What the overview says beside workspace `ix`'s name, in one meta line: the machines its
    /// tiles are on, unless that is its name already, then the next thing in it that needs the
    /// person ("Claude Code · Needs approval"), the first in reading order. Empty when there is
    /// nothing to add.
    pub(super) fn workspace_glance(&self, ix: usize) -> String {
        let name = self.workspace_name_at(ix);
        let Some(ws) = self.layout.workspaces().get(ix) else { return String::new() };
        let tiles: Vec<TileRef> =
            ws.columns().iter().flat_map(Column::tiles).map(Tile::tile).collect();
        let mut machines: Vec<String> = Vec::new();
        for tile in &tiles {
            let machine = self.worker_name(tile.worker);
            if !machines.contains(&machine) {
                machines.push(machine);
            }
        }
        let machines = machines.join(", ");
        let machines = (machines != name).then_some(machines);
        let need = tiles.iter().find_map(|&tile| {
            let item = self.item(tile)?;
            let (mark, _) = self.tile_marks(tile, item);
            let word = self.tile_word(item, mark.filter(|m| *m == Status::NeedsYou))?;
            Some(format!("{}{}{word}", self.tile_title(item), super::rollup::META_SEPARATOR))
        });
        super::rollup::meta_line([machines.as_deref(), need.as_deref()])
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
            cx.notify();
        }
    }

    /// Close the bar's menu and hand the keyboard back to the focused tile at once.
    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(which) = self.menu.take()
            && self.chrome_moves(cx)
        {
            self.keep_leaving(which, kit::Pace::Exit.duration(), |this| &mut this.menu_leaving, cx);
        }
        self.return_keyboard(window, cx);
        cx.notify();
    }

    /// Close the bar's menu with nothing chosen from it: a machine "+" was pointed at goes
    /// with it, so the next ⌘T is not sent there unasked.
    pub(super) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_on = None;
        self.close_menu(window, cx);
    }

    /// The navigator's toggle, at the far leading edge past the traffic lights wherever it
    /// shows: in the bar while the navigator is hidden, in the navigator's top row while it is
    /// docked, so it never moves as the navigator opens or closes.
    pub(super) fn navigator_toggle(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let hint_theme = Rc::new(theme.clone());
        kit::icon_button(theme, "navigator-toggle", Symbol::SidebarLeft, "Navigator")
            .when(SHORTCUT_HINTS, |el| {
                kit::hint_timing(el).tooltip(move |_window, cx| {
                    let keys = crate::palette::keys_for(&ToggleNavigator, &super::key_bindings());
                    cx.new(|_| kit::Hint::new("Navigator", keys, Rc::clone(&hint_theme))).into()
                })
            })
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.toggle_navigator(&ToggleNavigator, window, cx);
            }))
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

        // Left: the navigator's toggle (a docked navigator holds it in its own top row, at the
        // same place beside the lights), then where the focused work is.
        let toggle = (has_workers && !docked).then(|| self.navigator_toggle(cx));
        // A phone's "+" is a row of "…": the bar keeps the name, the bell and the menu.
        let new = (has_workers && !phone).then(|| {
            let at = Rc::clone(&self.anchors.at);
            let measure = canvas(
                move |bounds, _window, _cx| {
                    at.borrow_mut().insert(MenuKind::New, bounds);
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0();
            kit::icon_button(theme, "new-menu", Symbol::Plus, NEW)
                .relative()
                .aria_expanded(self.menu == Some(MenuKind::New))
                .child(measure)
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_menu(MenuKind::New, window, cx);
                }))
        });

        // Right: the bell and "…". Who needs you is counted once, on the bell, with the turns
        // left to review; it opens the navigator at them.
        let total = self.drawn_waiting.len().saturating_add(self.drawn_thread_waits.len());
        let unread = total.saturating_add(self.to_review().len());
        let bell = has_workers.then(|| {
            // Each fill with its own ink: a near-black on the green and on a state's fill.
            let (fill, ink) =
                if total > 0 { (s.warn_fill, s.fill_fg) } else { (s.accent_fill, s.accent_ink) };
            let badge = (unread > 0).then(|| {
                let count = SharedString::from(unread.to_string());
                let side = theme.typography.caption() + spacing.xs + spacing.xxs;
                let hovered = hsla(s.hover.over(theme.content()));
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
                    .border(px(slopty_theme::stroke::EDGE))
                    .border_color(hsla(theme.content()))
                    .group_hover(BELL, move |el| el.border_color(hovered))
                    .bg(hsla(fill))
                    .text_color(hsla(ink))
                    .text_size(px(theme.typography.caption()))
                    .child(count)
            });
            kit::icon_button(theme, "bell", Symbol::Bell, super::navigator::NEEDS_YOU)
                .group(BELL)
                .relative()
                .children(badge)
                .on_click(cx.listener(|this, _ev, window, cx| this.needs_you_shown(window, cx)))
        });
        let more = kit::icon_button(theme, "more", Symbol::Ellipsis, "More")
            .aria_expanded(self.menu == Some(MenuKind::More))
            .on_click(
                cx.listener(|this, _ev, window, cx| this.toggle_menu(MenuKind::More, window, cx)),
            );
        let buttons =
            div().flex_none().flex().items_center().gap(px(spacing.xxs)).children(bell).child(more);
        let readouts = self.render_readouts(phone, window, cx);
        // The notices about no one tile's work: in the lane before the readouts, or hanging
        // under a phone's bar, whose middle is its title.
        let notices = self.render_notices(cx);
        let (lane, hanging) = if phone { (None, notices) } else { (notices, None) };
        let hanging = hanging.map(|notices| {
            div()
                .absolute()
                .top(px(titlebar_height(theme)) + safe.top + px(spacing.xs))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .px(px(spacing.sm))
                .child(notices)
        });
        // Its empty span is a native title bar: pressed and moved it drags the window, and a
        // double-click is the system's zoom. A button's press stops before it gets here.
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, window, _cx| {
                    this.title_press = ev.click_count < 2;
                    if ev.click_count == 2 {
                        this.window_ask(WindowAsk::TitleBarDoubleClick, window);
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, _cx| {
                if this.title_press && ev.pressed_button == Some(MouseButton::Left) {
                    this.title_press = false;
                    this.window_ask(WindowAsk::Move, window);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, _cx| this.title_press = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _ev, _window, _cx| this.title_press = false),
            )
            .relative()
            .size_full()
            .pt(safe.top)
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .pl(px(leading))
            .pr(px(trailing))
            .bg(hsla(theme.content()))
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
            .children(lane)
            .children(readouts)
            .child(buttons)
            .children(hanging)
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
        let (which, leaving) = match (self.menu, self.menu_leaving) {
            (Some(open), _) => (open, false),
            (None, Some(gone)) => (gone, true),
            (None, None) => return None,
        };
        let theme = &self.theme;
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
            MenuKind::Machine(key) => self.machine_entries(key, &entity, cx),
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
                        this.overview_flipped(cx);
                        cx.notify();
                    }),
                    action("Stream stats", &ToggleStats, |this, w, cx| {
                        this.toggle_stats(&ToggleStats, w, cx);
                    }),
                ]);
                entries.extend(self.more_entries.iter().cloned());
                entries.sort_by_key(|entry| entry.group);
                entries
            }
        };
        let head = match which {
            MenuKind::Machine(key) => self.machine_facts_rows(key),
            _ => Vec::new(),
        };
        let mut menu = kit::Menu::new();
        let mut group = entries.first().map(|entry| entry.group);
        for entry in entries {
            if group != Some(entry.group) {
                group = Some(entry.group);
                menu.separate();
            }
            menu.push(
                kit::MenuItem::with_run(entry.label.clone(), entry.label, entry.run)
                    .detail(entry.detail),
            );
        }
        // A base unit below the bar, the right edge on the window's inset, where the tiles'
        // headers end: a popover lined up with what it covers rather than hung off its button a
        // few points in.
        let gap = spacing.xs;
        let panel = self.menu_panel(menu, head, which, leaving, cx);
        // A click anywhere else closes it and goes no further, so a press on the button that
        // opened it closes it rather than opening it again. A popover, it paints over the frame
        // and the navigator laid over it, under a dialog. Leaving, it lets the window have the
        // pointer back at once: only its own fading panel holds a press.
        let away = div()
            .id("menu-away")
            .absolute()
            .inset_0()
            .when(!leaving, |el| {
                el.occlude().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, window, cx| this.dismiss_menu(window, cx)),
                )
            })
            .child(
                div()
                    .absolute()
                    // "+" and the breadcrumb hang their menus from their own left edges, as a
                    // menu bar's menus do; a machine's from its "…", under its row.
                    .map(|el| {
                        let at = self.anchors.at.borrow().get(&which).copied();
                        let under_bar = px(titlebar_height(theme) + gap) + safe.top;
                        let left = |el: gpui::Div| {
                            el.left(at.map_or_else(|| px(spacing.inset()), |b| b.origin.x))
                        };
                        match which {
                            MenuKind::New | MenuKind::Workspaces | MenuKind::Checkouts => {
                                left(el.top(under_bar))
                            }
                            MenuKind::Machine(_) => {
                                left(el.top(at.map_or(under_bar, |b| b.bottom() + px(gap))))
                            }
                            MenuKind::More => {
                                el.top(under_bar).right(px(spacing.inset()) + safe.right)
                            }
                        }
                    })
                    .child(panel),
            );
        let layer = crate::palette::Layer::Popover.priority();
        Some(gpui::deferred(away).with_priority(layer).into_any_element())
    }

    /// A menu's panel around its rows, fading in as it drops a base unit from its button, and
    /// back up and out once `leaving`, turning back if it is opened again on its way out
    /// ([`kit::Presence`]).
    fn menu_panel(
        &self,
        menu: kit::Menu,
        head: Vec<gpui::AnyElement>,
        which: MenuKind,
        leaving: bool,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let (closing, dismissing) = (cx.entity().downgrade(), cx.entity().downgrade());
        let panel =
            kit::MenuPanel::new("menu", menu_name(which), Rc::new(menu), theme, move |w, cx| {
                // The menu closes first, so the row runs with the keyboard back where it was.
                let _gone = closing.update(cx, |this, cx| this.close_menu(w, cx));
            })
            .on_dismiss(move |w, cx| {
                let _gone = dismissing.update(cx, |this, cx| this.dismiss_menu(w, cx));
            })
            .head(head)
            .keyed(self.menu_keyed)
            .inert(leaving);
        let panel = div().child(panel);
        let panel =
            if leaving { panel.debug_selector(|| "menu-leaving".to_owned()) } else { panel };
        let id = SharedString::from(format!("menu-presence-{which:?}"));
        kit::presence(panel, id, !leaving)
            .arrives_whole(self.menu_keyed || !self.chrome_moves(cx))
            .travel(-spacing.xs)
            .into_any_element()
    }
}

/// What a bar's menu is called, as a screen reader names it.
const fn menu_name(which: MenuKind) -> &'static str {
    match which {
        MenuKind::New => NEW,
        MenuKind::More => "More",
        MenuKind::Machine(_) => "Machine",
        MenuKind::Workspaces => "Workspaces",
        MenuKind::Checkouts => "Checkouts",
    }
}
