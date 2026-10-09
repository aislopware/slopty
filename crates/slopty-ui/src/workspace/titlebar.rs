//! The bar across the top, from the navigator's right edge to the window's (from past the
//! traffic lights when the navigator is hidden): the navigator's toggle, back and forward
//! through the tabs visited (`MonoCode`'s `TabVisitNav`), the breadcrumb of
//! where the focused work is (`project ▾ / checkout ▾ / branch`, `breadcrumb.rs`, whose
//! project menu is how the bar goes between projects), the project's tabs (`title_tabs.rs`)
//! and "+" after them (a menu of what to open: a terminal, an agent, a window or a note); the
//! bell and "…" on the right. The bell counts what needs the person and the agents' turns left
//! to review, and opens the navigator at them. Between them, only while there is something to say:
//! the notices that are about no one tile's work (`toast`), and before the bell the readouts
//! (`readouts`): the server while it does not answer, a plan far used, the ports forwarded, the
//! transfers, a newer Slopty, the frame time with the stats. Every other action is a key, the
//! palette, or a tile's own header. There is no bar along the bottom.
//!
//! It takes the content's tone ([`slopty_theme::Theme::content`]) with no rule under it, so the
//! content runs up to the window's top edge and the navigator is the one panel beside it, as
//! macOS 26 draws a sidebar beside edge-to-edge content and Linear and the Codex app draw a grey
//! sidebar beside a white main area. It used to take the navigator's tone, and with the bar
//! along the bottom the chrome read as a grey frame round the content, an older Electron
//! window's look. A
//! menu fades in as it drops 4 pt from its button, at once under Reduce Motion.
//!
//! On a phone the bar is a navigation bar: the focused tile's title (else the project's name),
//! which opens the tab's other panes and the project's tabs while there are some
//! ([`MenuKind::Switch`]), and what "+" opens folded into "…". It has no room for the
//! readouts, and its notices hang under its middle.
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
use slopty_client::layout::{GroupKey, Tab, TabId, TileRef, WorkerKey};
use slopty_proto::items::ItemKind;

use super::actions::{
    AddWindow, FilterNavigator, GoBack, GoForward, NewAgent, NewNote, NewTerminal, OpenCommands,
    OpenPalette, ToggleNavigator, ToggleStats,
};
use super::navigator::Mode;
use super::rollup::Rollup;
use super::title_tabs::TitleTab;
use super::{MenuEntry, MenuGroup, WorkspaceView};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Status, Symbol};
use crate::kit;

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

/// The bell's hover group, which its badge's cut-out ring follows.
const BELL: &str = "bell";

/// What "+" is called: it opens a menu of things to open.
pub(super) const NEW: &str = "New";

/// What the magnifier beside the navigator's toggle is called.
pub(super) const SEARCH: &str = "Search";

/// What the pencil after it is called: it starts "New agent…".
pub(super) const NEW_AGENT: &str = "New agent";

/// What the bar's arrows through the tabs visited are called.
pub(super) const BACK: &str = "Back";
pub(super) const FORWARD: &str = "Forward";

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// What a menu row runs, on the workspace itself.
type MenuAction = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum MenuKind {
    /// The breadcrumb's project: every project.
    Projects,
    /// The breadcrumb's checkout: the same repository's other checkouts.
    Checkouts,
    /// "+": what to open.
    New,
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

    /// The tabs of the project on show, as the bar draws them: each named by its focused
    /// work, with a mark for each agent in it that works or has finished.
    pub(super) fn title_tabs(&self) -> Vec<TitleTab> {
        let Some(project) = self.layout.shown_project() else { return Vec::new() };
        let shown = project.shown().map(Tab::id);
        project
            .tabs()
            .iter()
            .map(|tab| {
                let title = tab
                    .focused()
                    .and_then(|t| self.item(t))
                    .map(|item| self.tile_title(item))
                    .unwrap_or_default();
                let marks = tab
                    .tiles()
                    .filter_map(|t| {
                        let item = self.item(t)?;
                        let (mark, _) = self.tile_marks(t, item);
                        mark.filter(|m| {
                            matches!(
                                m,
                                Status::Working | Status::NeedsYou | Status::Done | Status::Failed
                            )
                        })
                    })
                    .collect();
                TitleTab {
                    id: tab.id(),
                    title: title.into(),
                    marks,
                    shown: shown == Some(tab.id()),
                }
            })
            .collect()
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
        self.leading_button("navigator-toggle", Symbol::SidebarLeft, "Navigator", &ToggleNavigator)
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.toggle_navigator(&ToggleNavigator, window, cx);
            }))
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
                let keys = crate::palette::keys_for(action, &super::key_bindings());
                cx.new(|_| kit::Hint::new(label, keys, Rc::clone(&hint_theme))).into()
            })
        })
    }

    /// Back and forward through the tabs visited (⌘[ ⌘]), a pair that always stands so the
    /// tabs after it never move; a way with nowhere to go is drawn faint and takes no press.
    fn visit_arrows(&self, cx: &Draw<'_, Self>) -> gpui::AnyElement {
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

    /// "Search", beside the navigator's toggle: in the navigator's top row it shows the
    /// navigator's filter and gives it the keyboard; in the bar, with the navigator hidden, it
    /// opens the palette, the one search left on screen.
    pub(super) fn search_button(
        &self,
        in_navigator: bool,
        cx: &Draw<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (id, action): (&'static str, &'static dyn gpui::Action) = if in_navigator {
            ("nav-search", &FilterNavigator)
        } else {
            ("bar-search", &OpenPalette)
        };
        self.leading_button(id, Symbol::Magnifyingglass, SEARCH, action).on_click(cx.listener(
            move |this, _ev, window, cx| {
                if in_navigator {
                    this.reveal_navigator_filter(cx);
                } else {
                    this.open_palette(&OpenPalette, window, cx);
                }
            },
        ))
    }

    /// "New agent", after "Search": "New agent…", the one way to start an agent.
    pub(super) fn new_agent_button(
        &self,
        id: &'static str,
        cx: &Draw<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.leading_button(id, Symbol::SquareAndPencil, NEW_AGENT, &NewAgent)
            .on_click(cx.listener(|this, _ev, window, cx| this.new_agent(&NewAgent, window, cx)))
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
        let phone = self.phone;
        let phone_title = (has_workers && phone).then(|| self.render_phone_switch(cx));
        let theme = &self.theme;
        let s = &theme.surfaces;

        // Left: the navigator's toggle (a docked navigator holds it in its own top row, at the
        // same place beside the lights), "Search" and "New agent" while the navigator is
        // hidden (a navigator holds them at its top row's end), then where the focused work is.
        let toggle = (has_workers && !docked).then(|| self.navigator_toggle(cx));
        let hidden = has_workers && !phone && self.nav.drawn.is_none();
        let search = hidden.then(|| self.search_button(false, cx));
        let new_agent = hidden.then(|| self.new_agent_button("bar-new-agent", cx));
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

        // Back and forward through the tabs visited, then where the focused work is, then
        // the project's tabs, "+" after the last.
        let visits = (has_workers && !phone).then(|| self.visit_arrows(cx));
        let where_ = (has_workers && !phone).then(|| self.render_breadcrumb(cx));
        let tabs =
            (has_workers && !phone).then(|| self.chrome.title_tabs.clone().into_any_element());
        if tabs.is_none() {
            self.drop_spots.no_strip();
        }

        // Right: the bell and "…". Who needs you is counted once, on the bell, with the turns
        // left to review; it opens the navigator at them.
        let total = self.drawn_waiting.len().saturating_add(self.drawn_thread_waits.len());
        let unread = total.saturating_add(self.to_review().len());
        let bell = has_workers.then(|| {
            // No count disc, the one web badge the app had: the glyph itself says it, as the
            // Mac's own monochrome `bell.badge` does, in its words' tier, and in the warn fill
            // only while something needs the person. The count is said, not drawn.
            let symbol = if unread > 0 { Symbol::BellBadge } else { Symbol::Bell };
            let label = super::navigator::NEEDS_YOU;
            let button = if total > 0 {
                kit::icon_button_inked(theme, "bell", symbol, label, s.warn_fill)
            } else {
                kit::icon_button(theme, "bell", symbol, label)
            };
            let count = (unread > 0).then(|| {
                div()
                    .id("bell-count")
                    .debug_selector(|| "bell-count".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("{unread} new")))
                    .absolute()
                    .size_0()
            });
            button
                .group(BELL)
                .relative()
                .children(count)
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
            .map(|el| super::tab_look::row(theme, el))
            .size_full()
            .pt(safe.top)
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .pl(px(leading))
            .pr(px(trailing))
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
                    .children(search)
                    .children(new_agent)
                    .children(phone_title)
                    .children(visits)
                    .children(where_)
                    .children(tabs)
                    .children(new),
            )
            .children(lane)
            .children(readouts)
            .child(buttons)
            .children(hanging)
            .children(self.render_approval_cards(window, cx))
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
        crate::a11y::tab_stop(switch, s.focus).into_any_element()
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
        let lead = crate::palette::lead_slot(theme, self.kind_glyph(item), hsla(s.text))
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
            .children(state.map(|st| crate::icons::status_mark(theme, Some(st))))
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
            MenuKind::Projects => self.project_entries_menu(&entity),
            MenuKind::Server => self.server_entries.clone(),
            MenuKind::Context => {
                self.context_menu.as_ref().map(|m| m.entries.clone()).unwrap_or_default()
            }
            MenuKind::Checkouts => self.checkout_entries(&entity),
            MenuKind::Switch => self.switch_entries(&entity),
            // The palette's names for the same actions, which the rows run as the keys do.
            MenuKind::New => {
                self.target_entries(&entity).into_iter().chain(Self::new_entries(&entry)).collect()
            }
            MenuKind::More => {
                // A phone's bar has no "+": what it opens leads its "…".
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
                entries.extend([
                    action("Command palette", &OpenCommands, |this, w, cx| {
                        this.open_commands(&OpenCommands, w, cx);
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
                    cx.listener(|this, _ev, window, cx| this.dismiss_menu(window, cx)),
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
                    // "+" and the breadcrumb hang their menus from their own left edges, as a
                    // menu bar's menus do; a machine's from its "…", under its row.
                    .map(|el| {
                        let at = self.anchors.at.borrow().get(&which).copied();
                        let under_bar = px(titlebar_height(theme) + gap) + safe.top;
                        let left = |el: gpui::Div| {
                            el.left(at.map_or_else(|| px(spacing.inset()), |b| b.origin.x))
                        };
                        match which {
                            MenuKind::New
                            | MenuKind::Projects
                            | MenuKind::Checkouts
                            | MenuKind::Switch => left(el.top(under_bar)),
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
/// and not by size. The panel title's 20 pt was a large title squeezed into a 44 pt bar. The
/// drawer's title takes it too.
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
        MenuKind::New => NEW,
        MenuKind::More => "More",
        MenuKind::Machine(_) => "Machine",
        MenuKind::Projects => "Projects",
        MenuKind::Checkouts => "Checkouts",
        MenuKind::Server => "Server",
        MenuKind::Context => "Menu",
        MenuKind::Switch => "Panes and tabs",
    }
}

impl super::title_tabs::TitleTabsHost for WorkspaceView {
    fn show_title_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.layout_action(cx, |l| l.show_tab(id));
    }

    fn carry_title_tab(&mut self, id: TabId, ev: &gpui::MouseDownEvent) {
        self.begin_carry(super::area::Carried::Tab(id), ev);
    }

    fn close_title_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
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

    fn title_tab_menu(
        &mut self,
        id: TabId,
        at: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_title_tab_menu(id, at, window, cx);
    }
}
