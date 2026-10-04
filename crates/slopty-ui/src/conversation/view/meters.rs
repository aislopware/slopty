//! The context popover under the header's ring: how full the context window is, what the
//! session cost, the account's five-hour and seven-day limits, and a way to compact.
//!
//! Compact types `/compact` as the composer types any command, and only while the agent is
//! idle: mid-turn the TUI would queue it behind the turn the person is watching.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, relative,
};
use slopty_proto::conversation::ThreadId;

use super::ConversationView;
use crate::colors::hsla;
use crate::conversation::figures;
use crate::kit::{self, ButtonKind};

/// The popover's width, in points at zoom 1.
const POPOVER_WIDTH: f32 = 260.0;

/// The context bar's height, in points at zoom 1.
const BAR_HEIGHT: f32 = 4.0;

impl ConversationView {
    /// Open or close the context popover.
    pub fn toggle_context(&mut self, cx: &mut Context<Self>) {
        self.context_open = !self.context_open;
        cx.notify();
    }

    /// Whether the context popover is open.
    #[must_use]
    pub const fn context_open(&self) -> bool {
        self.context_open
    }

    /// Type `/compact` into the agent's terminal, while it is idle, and close the popover.
    fn compact(&mut self, cx: &mut Context<Self>) {
        if self.turn_running() || self.approvals.prompt().is_some() {
            return;
        }
        self.context_open = false;
        self.send("/compact".to_owned(), Vec::new(), cx);
    }

    /// The figures the popover says, while the status line has said how full the window is.
    fn context_figures(&self) -> Option<figures::ContextFigures> {
        let last = self
            .model
            .thread(&ThreadId::Main)
            .and_then(|t| t.last_turn())
            .and_then(|t| t.context_tokens);
        figures::context_figures(self.model.meters()?, last)
    }

    /// The popover, over everything, under the ring: a press anywhere outside it closes it.
    pub(super) fn context_popover(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if !self.context_open {
            return None;
        }
        let figures = self.context_figures()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let tone = crate::conversation::thread::view::context_tone(theme, figures.used_pct);
        #[expect(clippy::cast_possible_truncation, reason = "a share on screen")]
        let share = (figures.used_pct / 100.0).clamp(0.0, 1.0) as f32;
        let bar = div()
            .w_full()
            .h(self.z(BAR_HEIGHT))
            .rounded(self.z(theme.radii.xs))
            .bg(hsla(s.border))
            .child({
                let (radius, fill) = (self.z(theme.radii.xs), hsla(tone));
                kit::Gliding::new("context-bar", share, move |share| {
                    div().h_full().w(relative(share)).rounded(radius).bg(fill).into_any_element()
                })
                .fill()
            });
        let line = |text: String, tone| {
            div().text_color(hsla(tone)).child(SharedString::from(text)).into_any_element()
        };
        let limits = figures
            .limits
            .iter()
            .map(|(text, warn)| line(text.clone(), if *warn { s.warn } else { s.text_secondary }));
        let idle = !self.turn_running() && self.approvals.prompt().is_none();
        let compact = kit::button(theme, "context-compact", "Compact", ButtonKind::Ghost)
            .when(!idle, |el| el.opacity(slopty_theme::alpha::PRESSED).cursor_default())
            .when(idle, |el| el.on_click(cx.listener(|this, _ev, _w, cx| this.compact(cx))));
        let panel = kit::tabular(kit::elevate(div(), theme))
            .id("context-popover")
            .debug_selector(|| "context-popover".to_owned())
            .role(Role::Dialog)
            .aria_label("Context")
            .occlude()
            .w(self.z(POPOVER_WIDTH))
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .p(self.z(theme.spacing.md))
            .rounded(self.z(theme.radii.lg))
            .font_family(theme.typography.ui_family.clone())
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xs))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(self.z(theme.typography.small()))
                                    .text_color(hsla(s.text))
                                    .font_weight(gpui::FontWeight(
                                        slopty_theme::Typography::MEDIUM_WEIGHT,
                                    ))
                                    .child("Context"),
                            )
                            .child(line(figures.used, s.text_muted)),
                    )
                    .child(bar),
            )
            .children(figures.cost.map(|cost| line(cost, s.text_secondary)))
            .children(limits)
            .child(div().flex().justify_end().child(compact));
        let chip = self.context_chip.get();
        let viewport = window.viewport_size();
        let right = (viewport.width - chip.right()).max(px(theme.spacing.sm));
        Some(
            gpui::deferred(
                gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(
                    div()
                        .id("context-away")
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _ev, _w, cx| {
                                this.context_open = false;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .absolute()
                                .top(chip.bottom() + self.z(theme.spacing.xs))
                                .right(right)
                                .child(kit::fade_in(panel, "context-fade", cx)),
                        ),
                ),
            )
            .with_priority(crate::palette::Layer::Popover.priority())
            .into_any_element(),
        )
    }
}
