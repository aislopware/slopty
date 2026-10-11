//! A disclosure chevron that turns: one chevron, pointing right when closed and down when
//! open, turned a quarter as the state changes rather than swapped for another glyph, so the
//! eye follows what opened.
//!
//! After Ely GPUI Components (`src/primitives/disclosure.rs`), Copyright (c) 2026 Ely GPUI
//! Component contributors, MIT OR Apache-2.0, on Slopty's [`super::on_change`] and motion
//! tokens: still on the first paint and under Reduce Motion.

use std::f32::consts::FRAC_PI_2;

use gpui::{
    AnimationExt as _, App, ElementId, Hsla, IntoElement, ParentElement as _, Pixels, RenderOnce,
    SharedString, Styled as _, Window, div,
};
use slopty_theme::Theme;

use super::Pace;
use crate::icons::{Drawn, Symbol};

/// A chevron for a row that opens, under `id`, pointing down while `open`.
#[derive(IntoElement)]
pub struct Disclosure {
    id: SharedString,
    open: bool,
    color: Hsla,
    side: Pixels,
    theme: Theme,
}

impl std::fmt::Debug for Disclosure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Disclosure")
            .field("id", &self.id)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl Disclosure {
    /// The chevron under `id`, `side` points square (already zoomed), in `color`.
    #[must_use]
    pub fn new(
        id: impl Into<SharedString>,
        open: bool,
        theme: &Theme,
        side: Pixels,
        color: Hsla,
    ) -> Self {
        Self { id: id.into(), open, color, side, theme: theme.clone() }
    }
}

impl RenderOnce for Disclosure {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, open, color, side, theme } = self;
        let turns = super::on_change(
            ElementId::Name(SharedString::from(format!("{id}-turns"))),
            &open,
            window,
            cx,
        );
        // At rest the chevron is the system's own, right or down, at exact pixels; only while
        // it turns is the right one turned, a quarter over the fade.
        let rest = if open { Symbol::ChevronDown } else { Symbol::ChevronRight };
        if turns == 0 || !super::motion(cx) {
            return Drawn::disclosure(&theme, rest).slot(side, color).into_any_element();
        }
        let key = ElementId::Name(SharedString::from(format!("{id}-turn-{turns}")));
        div()
            .flex_none()
            .size(side)
            .with_animation(key, Pace::Fade.animation(), move |el, t| {
                let share = if open { t } else { 1.0 - t };
                let drawn = if t >= 1.0 {
                    Drawn::disclosure(&theme, rest)
                } else {
                    Drawn::disclosure(&theme, Symbol::ChevronRight).turned(share * FRAC_PI_2)
                };
                el.child(drawn.slot(side, color))
            })
            .into_any_element()
    }
}
