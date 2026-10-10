//! The foot bar: a bar the status height (28 pt) along the window's foot, under the panes, on
//! the window's ground with the one line along its top (`MonoCode`'s `UsageFooter`, audit row
//! 3): its words at the caption role in the secondary tone, its chips 20 pt tall.
//!
//! It holds what is always worth a glance and never a warning. On the left, the plan's usage on
//! the focused tile's machine and agent (`Plan 5h 23% · 7d 41%`, in `warn` once a window is 80 %
//! used), which lists every machine's readings when clicked. The focused agent's state is its
//! tile header's alone, so the foot does not say it again. On the right, the ports forwarded here,
//! the transfers in flight, a chip for each shell of the project on show whose command runs out of
//! sight (in another tab, behind a pane's other tab, or in the tab's terminal put away; a click
//! goes to it), and the toggle of the tab's terminal, a chip that says "Terminal" (⌘⌥T, which the
//! palette names), in the accent while the terminal shows. A shell on show
//! says its command in its own header, so it has no chip. The title bar keeps only the readouts
//! that warn: the server out of reach, a link on a relay, a newer build. The frame time is the
//! stream stats overlay's (⌘⇧I), not a readout.
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
use crate::icons::{Drawn, IconSize, Symbol};
use crate::kit;

/// The toggle's name while the tab's terminal is put away, or the tab has none.
pub(super) const SHOW_TERMINAL: &str = "Show the tab's terminal";

/// The toggle's name while the tab's terminal shows.
pub(super) const HIDE_TERMINAL: &str = "Hide the tab's terminal";

/// The widest a shell's chip grows before its command is cut: a long command line must not
/// push the rest of the bar away.
const CHIP_MAX: f32 = 160.0;

/// The word on the toggle of the tab's terminal.
const TERMINAL_WORD: &str = "Terminal";

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
    /// with the panes, is not up. It lies on the ground, as the band the app lays under the
    /// workspace over the home indicator and a soft keyboard does, so the bar pads for neither.
    pub(super) fn foot_drawn(&self) -> bool {
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
        let running = self.running_shells();
        *self.foot.running.borrow_mut() = running.iter().map(|(_, s)| *s).collect();

        let plan = self.plan_button(cx).map(gpui::IntoElement::into_any_element);
        let ports = self.ports_button(cx).map(gpui::IntoElement::into_any_element);
        let transfers = self.transfers_button(cx).map(gpui::IntoElement::into_any_element);
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
        let gap = spacing.xs + spacing.xxs;
        let leading = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(gap))
            .children(plan);
        let trailing = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(gap))
            .children(ports)
            .children(transfers)
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
            .gap(px(gap))
            .px(px(spacing.md - spacing.xs - spacing.xxs))
            .bg(hsla(s.ground))
            .border_t(kit::HAIR)
            .border_color(hsla(s.stroke))
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
        kit::typed(bar, theme.roles().caption)
            .text_color(hsla(s.text_secondary))
            .children(plans)
            .children(transfer_list)
            .into_any_element()
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
        let mark = Drawn::disclosure(theme, Symbol::Terminal)
            .slot(px(IconSize::Inline.slot(theme)), hsla(s.text_muted));
        let el = div()
            .id(SharedString::from(format!("foot-shell-{session}")))
            .debug_selector(move || format!("foot-shell-{session}"))
            .role(Role::Button)
            .aria_label(SharedString::from(format!("{words}, running")))
            .flex_none()
            .max_w(px(CHIP_MAX))
            .h(px(theme.density.chip))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .px(px(theme.spacing.xs + theme.spacing.xxs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(s.text_secondary))
            .map(kit::eased)
            .hover(move |el| el.bg(hsla(s.hover_strong)).text_color(hsla(s.text)))
            .child(mark)
            .child(div().min_w_0().truncate().child(SharedString::from(words)));
        tab_stop(el, s.focus).on_click(cx.listener(move |this, _ev, _window, cx| {
            this.focus_tile(tile, cx);
        }))
    }

    /// The toggle of the tab's terminal, `MonoCode`'s footer "Terminal" button: a chip with the
    /// terminal's glyph and the word, quiet at rest and in the accent while the terminal shows.
    fn terminal_toggle(&self, cx: &Draw<'_, Self>) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let shows = self.layout.shown_tab().is_some_and(|tab| {
            tab.terminal().and_then(|id| tab.pane(id)).is_some_and(|pane| !pane.hidden())
        });
        let label = if shows { HIDE_TERMINAL } else { SHOW_TERMINAL };
        let ink = if shows { s.accent } else { s.text_muted };
        let glyph = IconSize::Inline;
        let el = div()
            .id("foot-terminal")
            .debug_selector(|| "foot-terminal".to_owned())
            .role(Role::Button)
            .aria_label(label)
            .aria_toggled(if shows { Toggled::True } else { Toggled::False })
            .flex_none()
            .h(px(theme.density.chip))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs + theme.spacing.xxs))
            .px(px(theme.spacing.xs + theme.spacing.xxs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_color(hsla(ink))
            .map(kit::eased)
            .hover(move |el| {
                let words = if shows { s.accent } else { s.text };
                el.bg(hsla(s.hover_strong)).text_color(hsla(words))
            })
            .child(
                Drawn::new(theme, Symbol::Terminal, glyph).slot(px(glyph.slot(theme)), hsla(ink)),
            )
            .child(TERMINAL_WORD);
        tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, window, cx| {
            this.tab_terminal(&TabTerminal, window, cx);
        }))
    }
}
