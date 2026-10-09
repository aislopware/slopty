//! The foot bar: a bar the status height (28 pt) along the window's foot, under the panes, on
//! the chrome (`MonoCode`'s `UsageFooter`, audit row 3).
//!
//! It holds what is always worth a glance and never a warning. On the left, the plan's usage on
//! the focused tile's machine and agent (`Plan 5h 23% · 7d 41%`, in `warn` once a window is 80 %
//! used), which lists every machine's readings when clicked; then the focused agent: its mark,
//! its name and how it is doing. On the right, the ports forwarded here, the transfers in
//! flight, the frame time while the stats show, a chip for each shell of the project on show
//! whose command runs out of sight (in another tab, behind a pane's other tab, or in the tab's
//! terminal put away; a click goes to it), and the toggle of the tab's terminal (⌘⌥T, which the
//! palette names). A shell on show says its command in its own header, so it has no chip. The title
//! bar keeps only the readouts that warn: the server out of reach, a link on a relay, a newer
//! build.
//!
//! A phone has no room for it: its bar along the foot is the key bar. The popovers it opens
//! rise from it, at its ends.

use std::cell::Cell;

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, InteractiveElement as _, IntoElement as _, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_client::layout::TileRef;
use slopty_core::SessionId;
use slopty_proto::items::ItemKind;

use super::WorkspaceView;
use super::actions::TabTerminal;
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Drawn, IconSize, Status, Symbol};
use crate::kit::{self, meta};

/// The toggle's name while the tab's terminal is put away, or the tab has none.
pub(super) const SHOW_TERMINAL: &str = "Show the tab's terminal";

/// The toggle's name while the tab's terminal shows.
pub(super) const HIDE_TERMINAL: &str = "Hide the tab's terminal";

/// The widest a shell's chip grows before its command is cut: a long command line must not
/// push the rest of the bar away.
const CHIP_MAX: f32 = 160.0;

/// The foot bar's own state: where it was laid out, which its popovers rise from, and the
/// shells its chips name as last drawn.
#[derive(Debug, Default)]
pub(super) struct Foot {
    /// The bar's bounds in the window, as its last frame laid it out.
    pub at: std::rc::Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The shells whose chips it drew last: a clock's tick draws it again only when they change.
    pub running: std::cell::RefCell<Vec<SessionId>>,
}

impl WorkspaceView {
    /// The foot bar's height: the status step, a finger's on touch.
    pub(super) const fn foot_height(&self) -> f32 {
        self.theme.density.status
    }

    /// Whether the window has a foot bar: not on a phone, nor before a machine is known.
    pub(super) fn foot_shown(&self) -> bool {
        !self.phone && !self.workers.is_empty()
    }

    /// Whether the foot bar is drawn now: the window has one and the settings page, which goes
    /// with the panes, is not up. The band the app lays under the workspace over the home
    /// indicator and a soft keyboard continues it, so the bar itself pads for neither.
    #[must_use]
    pub fn foot_drawn(&self) -> bool {
        self.foot_shown() && self.settings.is_none()
    }

    /// The shells of the project on show whose command runs past the running time out of
    /// sight, in the layout's order: what the foot bar's chips name.
    pub(super) fn running_shells(&self) -> Vec<(TileRef, SessionId)> {
        let shown = self.layout.shown_index();
        self.layout
            .tiles()
            .filter(|t| self.layout.position(*t).is_some_and(|p| Some(p.project) == shown))
            .filter_map(|tile| {
                let item = self.item(tile)?;
                let ItemKind::Terminal { session } = item.kind else { return None };
                let shell = self.item_agent(item).is_none();
                let away = !self.layout.on_show(tile);
                (shell && away && self.running_for(session).is_some()).then_some((tile, session))
            })
            .collect()
    }

    /// The foot bar, with the popover it opened.
    pub(super) fn render_foot(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let stats = self.show_stats && !self.phone;
        self.keep_clock(stats, cx);
        let running = self.running_shells();
        *self.foot.running.borrow_mut() = running.iter().map(|(_, s)| *s).collect();

        let plan = self.plan_button(cx).map(gpui::IntoElement::into_any_element);
        let agent = self.foot_agent().map(gpui::IntoElement::into_any_element);
        let ports = self.ports_button(cx).map(gpui::IntoElement::into_any_element);
        let transfers = self.transfers_button(cx).map(gpui::IntoElement::into_any_element);
        let frame = self.frame_readout_el(stats, cx).map(gpui::IntoElement::into_any_element);
        let chips: Vec<gpui::AnyElement> = running
            .into_iter()
            .map(|(tile, session)| self.shell_chip(tile, session, cx).into_any_element())
            .collect();
        let toggle = self.terminal_toggle(cx);

        let at = std::rc::Rc::clone(&self.foot.at);
        let measure =
            gpui::canvas(move |bounds, _window, _cx| at.set(Some(bounds)), |_, (), _, _| {})
                .absolute()
                .inset_0();
        let leading = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .children(plan)
            .children(agent);
        let trailing = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .children(ports)
            .children(transfers)
            .children(frame)
            .children(chips)
            .child(toggle);
        let bar = div()
            .id("foot")
            .debug_selector(|| "foot".to_owned())
            .role(Role::Group)
            .aria_label("Status")
            .relative()
            .size_full()
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .pl(px(spacing.xs))
            .pr(px(spacing.xxs))
            .bg(hsla(s.chrome))
            .border_t(kit::HAIR)
            .border_color(hsla(s.sash))
            .font_family(theme.typography.ui_family.clone())
            .child(measure)
            .child(leading)
            .child(trailing);
        let plans = self
            .readouts
            .shown(super::readouts::Popover::Plans)
            .then(|| self.render_plans(window, cx));
        let transfer_list = (self.readouts.shown(super::readouts::Popover::Transfers)
            && self.transfers_in_flight())
        .then(|| self.render_transfers(window, cx));
        meta(bar, theme).children(plans).children(transfer_list).into_any_element()
    }

    /// The focused tile's agent: its mark, its name, and how it is doing unless at rest.
    fn foot_agent(&self) -> Option<gpui::Stateful<gpui::Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = self.focused()?;
        let item = self.item(tile)?;
        let agent = self.item_agent(item)?;
        let name = super::projects::agent_label(&slopty_proto::thread::AgentId(agent.to_owned()));
        let status = self.tile_status(tile, item).filter(|st| *st != Status::Idle);
        let role = theme.roles().metadata;
        let mark = Drawn::beside(theme, self.kind_glyph(item), role)
            .slot(px(IconSize::beside_slot(theme, role)), hsla(s.text_muted));
        let label = match status {
            Some(st) => format!("{name}, {}", st.label()),
            None => name.clone(),
        };
        let state = status.map(|st| {
            div()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs))
                .child(crate::icons::status_mark(theme, Some(st)))
                .child(div().text_color(hsla(st.word(theme))).child(st.label()))
        });
        Some(
            div()
                .id("foot-agent")
                .debug_selector(|| "foot-agent".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(label))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.xs))
                .whitespace_nowrap()
                .child(mark)
                .child(div().text_color(hsla(s.text_secondary)).child(SharedString::from(name)))
                .children(state),
        )
    }

    /// A shell whose command runs: its terminal mark and its command; a click shows it.
    fn shell_chip(
        &self,
        tile: TileRef,
        session: SessionId,
        cx: &Draw<'_, Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let words = self
            .shell(session)
            .and_then(|sh| sh.running.clone())
            .or_else(|| self.item(tile).map(|i| self.tile_title(i)))
            .unwrap_or_default();
        let role = theme.roles().metadata;
        let mark = Drawn::beside(theme, Symbol::Terminal, role)
            .slot(px(IconSize::beside_slot(theme, role)), hsla(s.text_muted));
        let el = div()
            .id(SharedString::from(format!("foot-shell-{session}")))
            .debug_selector(move || format!("foot-shell-{session}"))
            .role(Role::Button)
            .aria_label(SharedString::from(format!("{words}, running")))
            .flex_none()
            .max_w(px(CHIP_MAX))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .px(px(theme.spacing.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_secondary))
            .map(kit::eased)
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .child(mark)
            .child(div().min_w_0().truncate().child(SharedString::from(words)));
        tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _window, cx| {
            this.focus_tile(tile, cx);
        }))
    }

    /// The toggle of the tab's terminal, lit while it shows.
    fn terminal_toggle(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let shows = self.layout.shown_tab().is_some_and(|tab| {
            tab.terminal().and_then(|id| tab.pane(id)).is_some_and(|pane| !pane.hidden())
        });
        let label = if shows { HIDE_TERMINAL } else { SHOW_TERMINAL };
        kit::icon_button(theme, "foot-terminal", Symbol::Terminal, label)
            .aria_toggled(if shows { Toggled::True } else { Toggled::False })
            .when(shows, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .on_click(cx.listener(|this, _ev, window, cx| {
                this.tab_terminal(&TabTerminal, window, cx);
            }))
    }
}
