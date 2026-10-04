//! A disclosure chevron that turns: one chevron, pointing right when closed and down when
//! open, turned a quarter as the state changes rather than swapped for another glyph, so the
//! eye follows what opened.
//!
//! After Ely GPUI Components (`src/primitives/disclosure.rs`), Copyright (c) 2026 Ely GPUI
//! Component contributors, MIT OR Apache-2.0, on Slopty's [`super::on_change`] and motion
//! tokens: still on the first paint and under Reduce Motion.

use std::f32::consts::FRAC_PI_2;

use gpui::{
    AnimationExt as _, App, ElementId, Hsla, IntoElement, Pixels, RenderOnce, SharedString,
    Styled as _, Transformation, Window, radians,
};
use slopty_theme::Theme;

use super::Pace;
use crate::icons::{IconName, IconSize};

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

/// How far the chevron has turned from pointing right, at `share` of its quarter turn.
fn turned(share: f32) -> Transformation {
    Transformation::rotate(radians(share * FRAC_PI_2))
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
        let chevron =
            crate::icons::icon(&theme, IconName::ChevronRight, IconSize::Inline, color).size(side);
        if turns == 0 || !super::motion(cx) {
            return chevron
                .with_transformation(turned(if open { 1.0 } else { 0.0 }))
                .into_any_element();
        }
        let key = ElementId::Name(SharedString::from(format!("{id}-turn-{turns}")));
        chevron
            .with_animation(key, Pace::Fade.animation(), move |chevron, t| {
                chevron.with_transformation(turned(if open { t } else { 1.0 - t }))
            })
            .into_any_element()
    }
}
