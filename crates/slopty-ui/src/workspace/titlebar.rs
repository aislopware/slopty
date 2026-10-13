//! The window's top line and a phone's bar.
//!
//! On a Mac and an iPad the window has no title bar of its own. Its top line is 34 pt: the
//! navigator's header while it is docked, then the tab bars of the panes that meet the window's
//! top ([`super::tile`]). The top-left tab bar starts with the window's lead and the top-right
//! one ends with the window's cluster ([`super::cluster`]), and every top tab bar's empty run is
//! the window's title bar: pressed and moved it drags the window, and a double-click is the
//! system's zoom ([`WorkspaceView::drags_window`]). Only while no pane meets the top (the
//! settings page, the empty workspace, no machine yet) does the window draw a line of its own
//! here, the lead and the cluster on the tab bar's plane with its drag run between them.
//!
//! On a phone the bar is a navigation bar: the focused tile's title (else the project's name),
//! which opens the tab's other panes and the project's tabs while there are some
//! ([`MenuKind::Switch`]), and "…", which leads with the new tiles a phone has no menu bar
//! for. It has no room for the readouts, and its notices hang under its middle.
//!
//! A menu fades in as it drops 4 pt from its button, at once under Reduce Motion. The bar is a
//! view of its own, drawn cached: an echo in a terminal does not draw it again.

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
use slopty_client::layout::{GroupKey, Tab, TabId, TileRef, WorkerKey};
use slopty_kit::colors::hsla;
use slopty_kit::draw::Draw;
use slopty_kit::icons::{Status, Symbol};
use slopty_kit::kit;
use slopty_proto::items::ItemKind;

use super::actions::{
    AddWindow, GoBack, GoForward, NewAgent, NewNote, NewTerminal, OpenCommands, ToggleNavigator,
};
use super::navigator::Mode;
use super::rollup::Rollup;
use super::{MenuEntry, MenuGroup, WorkspaceView};

/// The bar's height under `theme`'s density: a finger's target and a hairline's room round
/// it at least, so its buttons fit it on touch.
#[must_use]
pub const fn titlebar_height(theme: &slopty_theme::Theme) -> f32 {
    theme.density.title.max(2.0_f32.mul_add(theme.spacing.xxs, theme.density.hit))
}

/// Room for the traffic lights at the left of the bar, or of the navigator when it shows.
#[cfg(target_os = "macos")]
pub(super) const LEADING_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub(super) const LEADING_INSET: f32 = 12.0;

/// What the navigator's filter is called while it waits hidden.
pub(super) const SEARCH: &str = "Search";

/// What the navigator header's pencil is called: it starts "New agent…".
pub(super) const NEW_AGENT: &str = "New agent";

/// What the lead's arrows through the tabs visited are called.
pub(super) const BACK: &str = "Back";
pub(super) const FORWARD: &str = "Forward";

/// Keyboard hints where there is a keyboard with a ⌘ key.
pub(super) const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// What a menu row runs, on the workspace itself.
type MenuAction = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum MenuKind {
    /// The breadcrumb's project: every project.
    Projects,
    /// The breadcrumb's checkout: the same repository's other checkouts.
    Checkouts,
    /// "…": everything else, the app's entries included.
    More,
    /// A machine's "…" in the navigator: what it says of itself, and what can be done to it.
    Machine(WorkerKey),
    /// The server's readout while it is offline: try it now, or another server.
    Server,
    /// A tile's or a project's own menu, opened by a press on it (`context_menus`).
    Context,
    /// A phone's title: the tab's panes and the project's tabs, as the phone draws one pane.
    Switch,
}

/// What the top line's empty run asks of the window, as a native title bar does.
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

    /// Whether a round trip going from `was` to `now` is on screen: the navigator prints those
    /// slow enough to name. A quick link's samples then draw nothing.
    pub(super) fn rtt_shown(&self, was: Option<Duration>, now: Option<Duration>) -> bool {
        let slow =
            |rtt: Option<Duration>| rtt.is_some_and(|rtt| rtt >= super::navigator::RTT_SHOWN_FROM);
        self.nav.drawn.is_some() && (slow(was) || slow(now))
    }

    /// The name of the project on show: the one given, else its group's.
    #[must_use]
    pub fn project_name(&self) -> String {
        self.layout.shown_index().map(|ix| self.project_name_at(ix)).unwrap_or_default()
    }

    /// Project `ix`'s name: the one the person gave it, else its group's as the navigator says
    /// it, which is its machine's for work that has no project.
    pub(super) fn project_name_at(&self, ix: usize) -> String {
        let Some(project) = self.layout.projects().get(ix) else { return String::new() };
        if let Some(name) = project.name() {
            return name.to_owned();
        }
        self.home_name(project.home())
    }

    /// What the group `home` is called: its machine's name, else its group's.
    pub(super) fn home_name(&self, home: &GroupKey) -> String {
        if let Some(worker) = home.worker() {
            return self.worker_name(worker);
        }
        let projects = self.project_groups();
        projects.group(home).map_or_else(|| home.value().to_owned(), |g| self.group_name(g))
    }

    /// What project `ix`'s tiles add up to, and how many there are.
    pub(super) fn project_rollup(&self, ix: usize) -> (Rollup, usize) {
        let mut rollup = Rollup::default();
        let mut count = 0_usize;
        let Some(project) = self.layout.projects().get(ix) else { return (rollup, count) };
        for tile in project.tabs().iter().flat_map(Tab::tiles) {
            count = count.saturating_add(1);
            if let Some(item) = self.item(tile) {
                let (mark, unseen) = self.tile_marks(tile, item);
                rollup.add(mark, unseen);
            }
        }
        (rollup, count)
    }

    /// What a tab whose focused tile is `tile` is named: its title; a thread on its way
    /// as its tile's header names it; a tile whose item is not here, its machine's name, as the
    /// tile itself says while its worker is away.
    fn tab_title(&self, tile: TileRef) -> String {
        if let Some(item) = self.item(tile) {
            return self.tile_title(item);
        }
        match self.starting.get(tile.item) {
            Some(starting) => {
                format!(
                    "New {} thread",
                    crate::conversation::thread::view::agent_label(&starting.agent)
                )
            }
            None => self.worker_name(tile.worker),
        }
    }

    /// The tabs of the project on show, each named by its focused work: what "Other tabs…" and
    /// a phone's title menu list.
    pub(super) fn tab_lines(&self) -> Vec<TabLine> {
        let Some(project) = self.layout.shown_project() else { return Vec::new() };
        let shown = project.shown().map(Tab::id);
        project
            .tabs()
            .iter()
            .map(|tab| TabLine {
                id: tab.id(),
                title: tab.focused().map(|t| self.tab_title(t)).unwrap_or_default().into(),
                shown: shown == Some(tab.id()),
            })
            .collect()
    }

    /// Close tab `id` and everything in it, as "Close other tabs" does each other one. A shell
    /// whose command runs asks first, in its tab.
    pub(super) fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some((p, t)) = self.layout.tab_place(id) else { return };
        let tiles: Vec<TileRef> = self
            .layout
            .projects()
            .get(p)
            .and_then(|project| project.tabs().get(t))
            .map(|tab| tab.tiles().collect())
            .unwrap_or_default();
        for tile in tiles {
            self.close_tile(tile, window, cx);
        }
    }

    pub(super) fn toggle_menu(
        &mut self,
        which: MenuKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu == Some(which) {
            self.close_menu(window, cx);
        } else {
            self.menu = Some(which);
            self.menu_at = None;
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

    /// The navigator's toggle, at the far leading edge past the traffic lights wherever it
    /// shows: in the lead while the navigator is hidden, in the navigator's header while it is
    /// docked, so it never moves as the navigator opens or closes. Its glyph says which way it
    /// goes: Octicons' `sidebar-collapse` while docked, `sidebar-expand` while not.
    pub(super) fn navigator_toggle(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let docked = self.nav.drawn == Some(Mode::Docked);
        let glyph = if docked { Symbol::SidebarLeft } else { Symbol::SidebarExpand };
        self.leading_button("navigator-toggle", glyph, "Navigator", &ToggleNavigator).on_click(
            cx.listener(|this, _ev, window, cx| {
                this.toggle_navigator(&ToggleNavigator, window, cx);
            }),
        )
    }

    /// A button of the leading cluster's: `icon` named `label`, with the system's hint naming
    /// it and `action`'s keys on a Mac.
    fn leading_button(
        &self,
        id: &'static str,
        icon: Symbol,
        label: &'static str,
        action: &'static dyn gpui::Action,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let hint_theme = Rc::new(theme.clone());
        kit::icon_button(theme, id, icon, label).when(SHORTCUT_HINTS, |el| {
            kit::hint_timing(el).tooltip(move |_window, cx| {
                let keys = crate::palette::keys_for(action, &super::key_bindings(cx));
                cx.new(|_| kit::Hint::new(label, keys, Rc::clone(&hint_theme))).into()
            })
        })
    }

    /// Back and forward through the tabs visited (⌘[ ⌘]), a pair that always stands so the
    /// tabs after it never move; a way with nowhere to go is drawn faint and takes no press.
    pub(super) fn visit_arrows(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let ways: [(bool, &'static str, Symbol, &'static str, &'static dyn gpui::Action); 2] = [
            (false, "go-back", Symbol::ChevronLeft, BACK, &GoBack),
            (true, "go-forward", Symbol::ChevronRight, FORWARD, &GoForward),
        ];
        let buttons = ways.map(|(forward, id, icon, label, action)| {
            if self.layout.can_go_back(forward) {
                self.leading_button(id, icon, label, action)
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.layout_action(cx, |l| {
                            l.go_back(forward);
                        });
                    }))
                    .into_any_element()
            } else {
                kit::icon_button_inked(theme, id, icon, label, theme.surfaces.text_muted)
                    .into_any_element()
            }
        });
        div().flex_none().flex().items_center().children(buttons).into_any_element()
    }

    /// The window's own top line, drawn only while no pane meets the top: the lead, the drag
    /// run and the cluster ([`super::cluster`]). On a phone, its navigation bar.
    pub(super) fn render_titlebar(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        if self.phone {
            return self.render_phone_bar(window, cx);
        }
        let theme = &self.theme;
        let safe = window.insets().effective();
        let bar = div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .map(|el| super::tab_look::row(theme, el))
            .bg(hsla(theme.surfaces.tab_bar))
            .size_full()
            .pt(safe.top)
            .flex()
            .items_center()
            .font_family(theme.typography.ui_family.clone())
            .child(self.chrome.lead.clone())
            .child(div().flex_1().h_full())
            .child(self.chrome.cluster.clone());
        Self::drags_window(bar, cx).into_any_element()
    }

    /// `el`, a run of the top line, as the window's title bar: pressed and moved it drags the
    /// window, and a double-click is the system's zoom. A button's press stops before it gets
    /// here.
    pub(super) fn drags_window(
        el: gpui::Stateful<gpui::Div>,
        cx: &Draw<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.on_mouse_down(
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
    }

    /// A phone's navigation bar: the navigator's toggle, the focused tile's title, the bell
    /// and "…", its notices hanging under its middle. `safe.top` is the notch's inset.
    fn render_phone_bar(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let has_workers = !self.workers.is_empty();
        let docked = self.nav.drawn == Some(Mode::Docked);
        let toggle = (has_workers && !docked).then(|| self.navigator_toggle(cx));
        let title = has_workers.then(|| self.render_phone_switch(cx));
        let bell = (has_workers && !docked).then(|| self.bell_button(cx));
        let more = self.more_button(cx);
        let notices = self.render_notices(cx).map(|notices| {
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
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .map(|el| super::tab_look::row(theme, el))
            .bg(hsla(theme.surfaces.sidebar))
            .size_full()
            .pt(safe.top)
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .pl(px(LEADING_INSET + f32::from(safe.left)))
            .pr(px(spacing.md + f32::from(safe.right)))
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
                    .children(title),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(spacing.xxs))
                    .children(bell)
                    .child(more),
            )
            .children(notices)
            .into_any_element()
    }

    /// A phone's title, and while the tab has panes or the project tabs the phone does not
    /// draw, the menu that goes to them ([`MenuKind::Switch`]), its chevron after the title
    /// as the breadcrumb's segments have theirs.
    fn render_phone_switch(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let title = self.render_phone_title();
        let (panes, tabs) = self.switch_counts();
        if panes < 2 && tabs < 2 {
            return title;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let which = MenuKind::Switch;
        let anchors = Rc::clone(&self.anchors.at);
        let measure = canvas(
            move |bounds, _window, _cx| {
                anchors.borrow_mut().insert(which, bounds);
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
        let switch = div()
            .id("phone-switch")
            .debug_selector(|| "phone-switch".to_owned())
            .role(Role::Button)
            .aria_label(menu_name(which))
            .aria_expanded(self.menu == Some(which))
            .relative()
            .flex_shrink(1.0)
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .cursor_pointer()
            .child(measure)
            .child(title)
            .child(self.chevron())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(
                cx.listener(move |this, _ev, window, cx| this.toggle_menu(which, window, cx)),
            );
        slopty_kit::a11y::tab_stop(switch, s.focus).into_any_element()
    }

    /// A phone's title: the focused tile's, as an iOS navigation bar names its screen, its kind
    /// (or its agent's mark) before its name and how it is doing after, as an inline navigation
    /// title ([`phone_title_role`]). The tile has no header of its own on a phone, so its rows
    /// are the bar's "…". With no tile focused it names the project.
    fn render_phone_title(&self) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let focused = self.focused().and_then(|tile| self.item(tile).map(|item| (tile, item)));
        let Some((tile, item)) = focused else {
            let ix = self.layout.shown_index().unwrap_or_default();
            let (label, _rollup) = self.project_words(ix);
            return phone_heading(theme, label)
                .child(SharedString::from(self.project_name_at(ix)))
                .into_any_element();
        };
        let id = item.id;
        let title = self.tile_title(item);
        let state = self.tile_status(tile, item).filter(|st| *st != Status::Idle);
        // What its header's agent pill would say, whole, for a screen reader; else its state.
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session),
            ItemKind::Thread { thread } => self.thread_stand(thread),
            _ => None,
        };
        let said = agent
            .filter(|agent| {
                state.is_some() && super::agents::agent_mark_of(agent) != Status::Working
            })
            .map(super::agents::agent_status_text)
            .or_else(|| state.map(|st| st.label().to_owned()));
        // Named as its header would be on a wider screen: its agent or its kind, then its title.
        let kind = self.spoken_kind(item);
        let named = super::tile::spoken_heading(&kind, &title);
        let label = match said {
            Some(said) => format!("{named}, {said}"),
            None => named,
        };
        let lead = crate::palette::lead_slot(theme, self.kind_glyph(item), hsla(s.icon))
            .debug_selector(move || format!("phone-kind-{}", id.as_uuid()));
        // Renamed, the field takes the title's place, as it does in a header.
        if let Some(field) = self.rename_field(tile, id) {
            return kit::typed(div(), phone_title_role(theme))
                .id("phone-renaming")
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .child(lead)
                .child(field)
                .into_any_element();
        }
        phone_heading(theme, SharedString::from(label))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .child(lead)
            .child(
                div()
                    .debug_selector(move || format!("phone-title-{}", id.as_uuid()))
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(SharedString::from(title)),
            )
            .children(state.map(|st| slopty_kit::icons::status_mark(theme, Some(st))))
            .into_any_element()
    }

    /// What a project's name says to a screen reader: its name, how many tiles it holds, and
    /// what they add up to.
    pub(super) fn project_words(&self, ix: usize) -> (SharedString, Rollup) {
        let name = self.project_name_at(ix);
        let (rollup, count) = self.project_rollup(ix);
        let noun = if count == 1 { "tile" } else { "tiles" };
        let label = match rollup.words() {
            Some(words) => format!("{name}, {count} {noun}, {words}"),
            None => format!("{name}, {count} {noun}"),
        };
        (label.into(), rollup)
    }

    /// A phone's file facts (its language, indent and caret) heading its "…": its tile has no
    /// tab bar to end with them, and over the keyboard they stood mid-screen
    /// (`.research/design-polish-2026-10-12.md` §1.19).
    fn phone_file_facts(&self) -> Vec<gpui::AnyElement> {
        let Some(tile) = self.focused() else { return Vec::new() };
        let Some(facts) = self.file_facts(tile.item).editor else { return Vec::new() };
        let theme = &self.theme;
        vec![
            kit::sheet_row(theme, kit::Row::One)
                .debug_selector(|| "phone-file-facts".to_owned())
                .child(facts)
                .into_any_element(),
            kit::list_rule(theme),
        ]
    }

    /// A phone's new tiles, leading its "…" as `entry` makes a row: the palette's names for the
    /// same actions, which the rows run as the keys do.
    fn new_entries(
        entry: &impl Fn(MenuGroup, &'static str, Option<&dyn gpui::Action>, MenuAction) -> MenuEntry,
    ) -> Vec<MenuEntry> {
        vec![
            entry(MenuGroup::Tiles, "New terminal", Some(&NewTerminal), |this, w, cx| {
                this.new_terminal(&NewTerminal, w, cx);
            }),
            entry(
                MenuGroup::Tiles,
                super::actions::NEW_AGENT_ON,
                Some(&NewAgent),
                |this, w, cx| {
                    this.new_agent(&NewAgent, w, cx);
                },
            ),
            entry(MenuGroup::Tiles, "Add a window or display", Some(&AddWindow), |this, w, cx| {
                this.add_window(&AddWindow, w, cx);
            }),
            entry(MenuGroup::Tiles, "New note", Some(&NewNote), |this, w, cx| {
                this.new_note(&NewNote, w, cx);
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
        let bindings = super::key_bindings(cx);
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
            MenuKind::Projects => self.project_entries_menu(&entity),
            MenuKind::Server => self.server_entries.clone(),
            MenuKind::Context => {
                self.context_menu.as_ref().map(|m| m.entries.clone()).unwrap_or_default()
            }
            MenuKind::Checkouts => self.checkout_entries(&entity),
            MenuKind::Switch => self.switch_entries(&entity),
            MenuKind::More => {
                let phone = self.phone;
                // A phone's bar is the focused tile's: its own rows lead its "…".
                let mut entries: Vec<MenuEntry> = if phone {
                    self.phone_tile_entries(cx)
                        .into_iter()
                        .chain(Self::new_entries(&entry))
                        .collect()
                } else {
                    Vec::new()
                };
                // A Mac opens the palette by its keys and its menu bar; a phone or an iPad,
                // with neither, by this row.
                if phone || self.theme.density == slopty_theme::Density::TOUCH {
                    entries.push(action("Command palette", &OpenCommands, |this, w, cx| {
                        this.open_commands(&OpenCommands, w, cx);
                    }));
                }
                entries.extend(self.more_entries.iter().cloned());
                entries.sort_by_key(|entry| entry.group);
                entries
            }
        };
        let head = match which {
            MenuKind::Machine(key) => self.machine_facts_rows(key),
            MenuKind::More if self.phone => self.phone_file_facts(),
            _ => Vec::new(),
        };
        let mut menu = kit::Menu::new();
        let mut group = entries.first().map(|entry| entry.group);
        // Two rows of one name (two shells in one folder) are two rows: the second keyed apart.
        let mut keys = std::collections::HashSet::new();
        for (n, entry) in entries.into_iter().enumerate() {
            if group != Some(entry.group) {
                group = Some(entry.group);
                menu.separate();
            }
            let key = if keys.insert(entry.label.clone()) {
                entry.label.clone()
            } else {
                SharedString::from(format!("{}-{n}", entry.label))
            };
            menu.push(kit::MenuItem::with_run(key, entry.label, entry.run).detail(entry.detail));
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
                    cx.listener(|this, _ev, window, cx| this.close_menu(window, cx)),
                )
            })
            .child(match self.menu_at {
                // A menu a press opened hangs where it landed, kept inside the window.
                Some(at) => gpui::anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(spacing.sm))
                    .child(panel)
                    .into_any_element(),
                None => div()
                    .absolute()
                    // The breadcrumb's menus hang from their own left edges, as a menu
                    // bar's menus do; a machine's from its "…", under its row.
                    .map(|el| {
                        let at = self.anchors.at.borrow().get(&which).copied();
                        let under_bar = px(titlebar_height(theme) + gap) + safe.top;
                        let left = |el: gpui::Div| {
                            el.left(at.map_or_else(|| px(spacing.inset()), |b| b.origin.x))
                        };
                        match which {
                            MenuKind::Projects | MenuKind::Checkouts | MenuKind::Switch => {
                                left(el.top(under_bar))
                            }
                            // The server's readout sits among the trailing ones: its menu ends
                            // on the readout's right edge.
                            MenuKind::Server => el.top(under_bar).right(at.map_or_else(
                                || px(spacing.inset()) + safe.right,
                                |b| window.viewport_size().width - b.right(),
                            )),
                            MenuKind::Machine(_) => {
                                left(el.top(at.map_or(under_bar, |b| b.bottom() + px(gap))))
                            }
                            MenuKind::More | MenuKind::Context => {
                                el.top(under_bar).right(px(spacing.inset()) + safe.right)
                            }
                        }
                    })
                    .child(panel)
                    .into_any_element(),
            });
        let layer = slopty_kit::layer::Layer::Popover.priority();
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
        let panel = kit::MenuPanel::new(
            "menu",
            self.menu_name(which),
            Rc::new(menu),
            theme,
            move |w, cx| {
                // The menu closes first, so the row runs with the keyboard back where it was.
                let _gone = closing.update(cx, |this, cx| this.close_menu(w, cx));
            },
        )
        .on_dismiss(move |w, cx| {
            let _gone = dismissing.update(cx, |this, cx| this.close_menu(w, cx));
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

impl WorkspaceView {
    /// What a menu is called, as a screen reader names it: a pressed thing's own says what
    /// the thing is.
    fn menu_name(&self, which: MenuKind) -> &'static str {
        match which {
            MenuKind::Context => self.context_menu.as_ref().map_or("Menu", |m| m.name),
            _ => menu_name(which),
        }
    }
}

/// A phone's title's type: an iOS inline navigation title, the size of the task title's role
/// (17 pt on touch) in the strong weight, so it stands a step above the rows beside it by weight
/// and not by size. The panel title's 20 pt was a large title squeezed into a 44 pt bar.
pub(super) fn phone_title_role(theme: &slopty_theme::Theme) -> slopty_theme::TypeRole {
    slopty_theme::TypeRole {
        weight: slopty_theme::Typography::STRONG_WEIGHT,
        ..theme.roles().task_title
    }
}

/// A phone bar's heading: one line as an inline navigation title, giving way at its end.
fn phone_heading(theme: &slopty_theme::Theme, label: SharedString) -> gpui::Stateful<gpui::Div> {
    kit::typed(div(), phone_title_role(theme))
        .id("phone-title")
        .debug_selector(|| "phone-title".to_owned())
        .role(Role::Heading)
        .aria_label(label)
        .flex_shrink(1.0)
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(hsla(theme.surfaces.text))
}

/// What a bar's menu is called, as a screen reader names it.
const fn menu_name(which: MenuKind) -> &'static str {
    match which {
        MenuKind::More => "More",
        MenuKind::Machine(_) => "Machine",
        MenuKind::Projects => "Projects",
        MenuKind::Checkouts => "Checkouts",
        MenuKind::Server => "Server",
        MenuKind::Context => "Menu",
        MenuKind::Switch => "Panes and tabs",
    }
}

/// One tab of the project on show, as "Other tabs…" and a phone's title menu list it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TabLine {
    /// Which.
    pub id: TabId,
    /// The title of its focused work.
    pub title: SharedString,
    /// It is the tab on show.
    pub shown: bool,
}
