//! The window's lead and cluster: the two ends of the top line
//! (`.research/design-polish-2026-10-12.md` §4.1, §4.2 and §4.4).
//!
//! The window has no title bar and no status bar. Its top line is the navigator's header and
//! the top panes' tab bars, and what the bars across the window held stands at its two ends,
//! each a view of its own drawn where the top line has room for it: in the top-left and the
//! top-right pane's tab bars, or in the window's own line while no pane meets the top
//! ([`super::titlebar`]).
//!
//! The lead starts the top line: while the navigator is not docked, the room for the traffic
//! lights and the navigator's toggle; then back and forward through the tabs visited.
//!
//! The cluster ends it, 6 in from the window's right, controls only and never a sentence: the
//! toggle of the tab's terminal (on while it shows), a 1 pt divider, then the project and
//! branch as Zed's title-bar buttons while the navigator is hidden (docked, it names the
//! project), the readouts that warn (the server out of reach, a link on a relay, a newer
//! Slopty, the plan once a window of it is 80 % used), the transfers in flight as a glyph and
//! a count, the bell with a count badge of what waits on the person, and "…". The notices that
//! are about no one tile's work hang under it, and the popovers it opens drop from it.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};
use slopty_kit::colors::hsla;
use slopty_kit::draw::Draw;
use slopty_kit::icons::Symbol;
use slopty_kit::kit;

use super::WorkspaceView;
use super::actions::TabTerminal;
use super::navigator::Mode;
use super::titlebar::{LEADING_INSET, MenuKind};

/// The toggle's name while the tab's terminal is put away, or the tab has none.
pub(super) const SHOW_TERMINAL: &str = "Show the tab's terminal";

/// The toggle's name while the tab's terminal shows.
pub(super) const HIDE_TERMINAL: &str = "Hide the tab's terminal";

/// The widest the project and branch buttons grow in the cluster, past which the branch gives
/// way first: the tabs before the cluster keep their room.
const CRUMBS_MAX: f32 = 480.0;

impl WorkspaceView {
    /// The lead: while the navigator is not docked, the room for the traffic lights (or the
    /// safe area) and the navigator's toggle; then back and forward through the tabs visited.
    /// Docked, the navigator's header holds the lights and the toggle.
    pub(super) fn render_lead(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let has_workers = !self.workers.is_empty();
        let docked = self.nav.drawn == Some(Mode::Docked);
        let pad = if docked { spacing.xs } else { LEADING_INSET + f32::from(safe.left) };
        let toggle = (has_workers && !docked).then(|| self.navigator_toggle(cx));
        let arrows = has_workers.then(|| self.visit_arrows(cx));
        div()
            .id("lead")
            .debug_selector(|| "lead".to_owned())
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .pl(px(pad))
            .pr(px(spacing.xs))
            .children(toggle)
            .children(arrows)
            .into_any_element()
    }

    /// The cluster at the top line's trailing end, with the notices that hang under it and the
    /// popovers it opened.
    pub(super) fn render_cluster(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let has_workers = !self.workers.is_empty();
        let docked = self.nav.drawn == Some(Mode::Docked);
        let tabbed = has_workers && self.layout.shown_tab().is_some() && self.settings.is_none();
        let toggle = tabbed.then(|| self.terminal_toggle(cx));
        let divider = tabbed.then(|| {
            div().flex_none().w(kit::HAIR).h(px(theme.typography.ui_size)).bg(hsla(s.stroke))
        });
        let crumbs = (has_workers && !docked).then(|| {
            div().flex_initial().min_w_0().max_w(px(CRUMBS_MAX)).child(self.render_breadcrumb(cx))
        });
        let readouts = self.render_readouts(window, cx);
        let plan = self.plan_button(cx);
        let transfers = self.transfers_button(cx);
        let bell = has_workers.then(|| self.bell_button(cx));
        let more = self.more_button(cx);
        let at = self.readouts.at();
        let measure = canvas(move |bounds, _window, _cx| at.set(Some(bounds)), |_, (), _, _| {})
            .absolute()
            .inset_0();
        // The notices about no one tile's work hang under the cluster, their right edge on the
        // window's inset, in a lane as wide as the window: past that the older go behind a
        // count. The row floats itself over the panes.
        let notices = self.render_notices(cx).map(|notices| {
            let lane = window.viewport_size().width - px(2.0 * spacing.sm) - safe.left - safe.right;
            div()
                .absolute()
                .top_full()
                .right(px(spacing.sm) + safe.right)
                .pt(px(spacing.xs))
                .w(lane.max(px(0.0)))
                .flex()
                .justify_end()
                .child(notices)
        });
        let plans = self
            .readouts
            .shown(super::readouts::Popover::Plans)
            .then(|| self.render_plans(window, cx));
        let transfer_list = (self.readouts.shown(super::readouts::Popover::Transfers)
            && self.transfers_in_flight())
        .then(|| self.render_transfers(window, cx));
        div()
            .id("cluster")
            .debug_selector(|| "cluster".to_owned())
            .role(Role::Group)
            .aria_label("Window")
            .relative()
            .flex_none()
            .h_full()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .pl(px(spacing.xs))
            .pr(px(spacing.inset_trailing()) + safe.right)
            .font_family(theme.typography.ui_family.clone())
            .child(measure)
            .children(toggle)
            .children(divider)
            .children(crumbs)
            .children(readouts)
            .children(plan)
            .children(transfers)
            .children(bell)
            .child(more)
            .children(notices)
            .children(plans)
            .children(transfer_list)
            .children(self.render_approval_cards(window, cx))
            .into_any_element()
    }

    /// The bell: what waits on the person, counted once beside its glyph in tabular figures,
    /// in the warn ink while an agent needs them, quiet while only turns wait to be reviewed.
    /// A click opens the navigator at them.
    pub(super) fn bell_button(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let needs = self.drawn_waiting.len().saturating_add(self.drawn_thread_waits.len());
        let count = self.bell_count();
        // The count stands beside the bell on the button's own line, in the figures' face at
        // the chrome's small size: a badge over the glyph was cut by the bar and hid both.
        let badge = (count > 0).then(|| {
            let ink = if needs > 0 { s.warn_fill } else { s.text_secondary };
            kit::tabular(div())
                .id("bell-count")
                .debug_selector(|| "bell-count".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(format!("{count} new")))
                .flex_none()
                .text_color(hsla(ink))
                .text_size(px(theme.typography.small()))
                .child(SharedString::from(count.to_string()))
        });
        let counted = badge.is_some();
        kit::icon_button(theme, "bell", Symbol::Bell, super::navigator::NEEDS_YOU)
            .when(counted, |el| {
                el.w_auto()
                    .min_w(px(kit::icon_button_side(theme)))
                    .px(px(theme.spacing.xs))
                    .gap(px(theme.spacing.xxs))
            })
            .children(badge)
            .on_click(cx.listener(|this, _ev, window, cx| this.needs_you_shown(window, cx)))
    }

    /// "…": settings, the server, adding a machine, and what a phone has no menu bar for.
    pub(super) fn more_button(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        kit::icon_button(&self.theme, "more", Symbol::Ellipsis, "More")
            .aria_expanded(self.menu == Some(MenuKind::More))
            .on_click(
                cx.listener(|this, _ev, window, cx| this.toggle_menu(MenuKind::More, window, cx)),
            )
    }

    /// The toggle of the tab's terminal: Zed's dock toggle, an icon button with the terminal's
    /// glyph, on (the selected wash, the text's ink) while the terminal shows; its hint names
    /// it and its keys (⌘⌥T).
    fn terminal_toggle(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let shows = self.layout.shown_tab().is_some_and(|tab| {
            tab.terminal().and_then(|id| tab.pane(id)).is_some_and(|pane| !pane.hidden())
        });
        let label = if shows { HIDE_TERMINAL } else { SHOW_TERMINAL };
        let hint_theme = Rc::new(theme.clone());
        let el = kit::icon_toggle(theme, "tab-terminal", Symbol::Terminal, label, shows);
        kit::hint_timing(el)
            .tooltip(move |_window, cx| {
                let keys = crate::palette::keys_for(&TabTerminal, &super::key_bindings(cx));
                cx.new(|_| kit::Hint::new(label, keys, Rc::clone(&hint_theme))).into()
            })
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.tab_terminal(&TabTerminal, window, cx);
            }))
    }
}
