//! Slopty's mark, live, over the empty workspace.
//!
//! The mark is a prompt on the aislopware grid: nine circles of one size, the chevron `>` lit
//! at (column, row) (0,0), (1,1) and (0,2), the cursor at (2,2), the rest unlit (set back to
//! [`Theme::brand_unlit`]), all in [`slopty_theme::BRAND`] (`docs/decisions/brand.md`). In the
//! app the cursor is the live element: it blinks at the terminal caret's cadence
//! ([`Motion::blink`]) while a worker is reachable, holds steady under Reduce Motion, and sits
//! at the unlit level while no worker is. It is a view of its own, so a blink draws the mark
//! and nothing round it; its clock runs only while it is drawn.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Task,
    Window, div, px,
};
use slopty_theme::{Motion, Theme};

use super::WorkspaceView;
use crate::colors::{hsla, hsla_alpha};
use crate::kit;

/// The lit dots of the chevron, as (column, row).
const CHEVRON: [(usize, usize); 3] = [(0, 0), (1, 1), (0, 2)];
/// The cursor's dot, on the baseline after the chevron.
const CURSOR: (usize, usize) = (2, 2);

/// A dot's side and the gap between two, from the spacing scale: a dot twice its gap, as the
/// mark's art has them.
const fn dot_and_gap(theme: &Theme) -> (f32, f32) {
    (theme.spacing.sm, theme.spacing.xs)
}

/// The mark as a view: nine dots and a cursor with a clock.
pub struct Mark {
    theme: Theme,
    /// The prefix of its debug selectors: `<name>-mark`, `<name>-cursor`.
    name: &'static str,
    /// A worker is reachable: the cursor is lit (and blinks).
    lit: bool,
    /// The blink's phase: the cursor shows.
    on: bool,
    /// Drawn since the clock last ticked: a mark no longer drawn stops its clock.
    drawn: bool,
    blink: Option<Task<()>>,
    /// How often Dot, beside the page's mark, was clicked: each is one hop.
    pets: u32,
}

impl Mark {
    /// A mark, lit while `lit`.
    #[must_use]
    pub const fn new(theme: Theme, name: &'static str, lit: bool) -> Self {
        Self { theme, name, lit, on: true, drawn: false, blink: None, pets: 0 }
    }

    /// Whether a worker is reachable.
    pub fn set_lit(&mut self, lit: bool, cx: &mut Context<Self>) {
        if self.lit != lit {
            self.lit = lit;
            self.on = true;
            cx.notify();
        }
    }

    /// The theme changed: the unlit level follows the content.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Whether the cursor shows lit now.
    #[must_use]
    pub const fn cursor_lit(&self) -> bool {
        self.lit && self.on
    }

    /// Whether its clock runs.
    #[cfg(test)]
    #[must_use]
    pub const fn blinking(&self) -> bool {
        self.blink.is_some()
    }

    /// Start the clock, when the cursor is lit, the system lets things move, and it is not
    /// running.
    fn blink_if_due(&mut self, cx: &Context<Self>) {
        if !self.lit || self.blink.is_some() || !kit::motion(cx) {
            return;
        }
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Motion::DEFAULT.blink).await;
                if !this.update(cx, Self::tick).unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Half a blink passed: flip the phase. False ends the clock, the cursor steady: the mark
    /// was not drawn since, no worker is reachable, or motion is off.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.drawn || !self.lit || !kit::motion(cx) {
            self.blink = None;
            if !self.on {
                self.on = true;
                cx.notify();
            }
            return false;
        }
        self.drawn = false;
        self.on = !self.on;
        cx.notify();
        true
    }
}

impl Render for Mark {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = true;
        self.blink_if_due(cx);
        let brand = self.theme.surfaces.brand;
        let (lit, unlit) = (hsla(brand), hsla_alpha(brand, self.theme.brand_unlit()));
        let (side, gap) = dot_and_gap(&self.theme);
        let cursor = self.cursor_lit();
        let name = self.name;
        let rows = (0..3).map(|row| {
            div().flex().gap(px(gap)).children((0..3).map(move |column| {
                let at = (column, row);
                let on = if at == CURSOR { cursor } else { CHEVRON.contains(&at) };
                div()
                    .flex_none()
                    .size(px(side))
                    .rounded_full()
                    .bg(if on { lit } else { unlit })
                    .when(at == CURSOR, |el| el.debug_selector(move || format!("{name}-cursor")))
            }))
        });
        let pet = cx.listener(|this: &mut Self, _ev: &gpui::ClickEvent, _w, cx| {
            this.pets = this.pets.wrapping_add(1);
            cx.notify();
        });
        let dot = crate::companions::beside_mark(&self.theme, cursor, self.pets, pet);
        div()
            .debug_selector(move || format!("{name}-mark"))
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(gap))
            .children(rows)
            .children(dot)
    }
}

impl WorkspaceView {
    /// A worker's link is up: the mark's cursor is lit.
    pub(super) fn reachable(&self) -> bool {
        self.workers.values().any(|w| w.link.is_some())
    }

    /// The mark follows whether a worker is reachable.
    pub(super) fn light_marks(&self, cx: &mut Context<Self>) {
        let lit = self.reachable();
        if self.empty_mark.read(cx).lit != lit {
            self.empty_mark.update(cx, |m, cx| m.set_lit(lit, cx));
        }
    }

    /// The mark takes a new theme.
    pub(super) fn theme_marks(&self, cx: &mut Context<Self>) {
        let theme = self.theme.clone();
        self.empty_mark.update(cx, |m, cx| m.set_theme(theme, cx));
    }
}
