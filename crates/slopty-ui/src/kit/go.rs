//! The go control: the one press that sends work on, in the brand's green.
//!
//! The composer's send and the plain allow of an agent's ask are the two places the person
//! sets work going. They wear the green fill with its ink, where every other primary keeps the
//! neutral solid ([`super::solid`]), so a surface holds at most one green control and it is
//! always the way on (`docs/decisions/ui.md`, "Premium pass from the mockups", "One green way
//! on"). The lint `the_accent_is_never_a_control` keeps every other green control out; this
//! file is where the one is drawn.

use gpui::{Div, InteractiveElement as _, Stateful, StatefulInteractiveElement as _, Styled};
use slopty_theme::{Rgb, Theme, Variant, alpha};

use crate::colors::hsla;

/// `el` filled with the green, its words or glyph in the green's ink.
pub fn go<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.accent_fill)).text_color(hsla(s.accent_ink))
}

/// The ink a glyph on the green is drawn in.
#[must_use]
pub const fn go_ink(theme: &Theme) -> Rgb {
    theme.surfaces.accent_ink
}

/// [`go`] that answers the pointer, flat as the solid is.
///
/// Under the pointer the green lightens a little and pressed more, so its near-black ink keeps
/// its contrast in both variants.
pub fn go_pressable(el: Stateful<Div>, theme: &Theme) -> Stateful<Div> {
    let (hovered, pressed) = go_states(theme);
    super::eased(go(el, theme))
        .hover(move |el| el.bg(hsla(hovered)))
        .active(move |el| el.bg(hsla(pressed)))
}

/// The green under the pointer and pressed: given [`alpha::FAINT`] and [`alpha::DIM`] toward
/// the light end of the ladder (white in light, the text in dark), away from its dark ink.
pub(super) fn go_states(theme: &Theme) -> (Rgb, Rgb) {
    let s = theme.surfaces;
    let light = if theme.variant() == Variant::Light { s.elevated } else { s.text };
    (s.accent_fill.mix(light, alpha::FAINT), s.accent_fill.mix(light, alpha::DIM))
}

#[cfg(test)]
mod tests {
    use slopty_theme::{Theme, Variant};

    use super::go_states;

    /// The go control's ink reads AA on its fill at rest, under the pointer and pressed, in
    /// both variants: its states move away from the ink, never toward it.
    #[test]
    fn the_go_control_reads_in_every_state() {
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let (hovered, pressed) = go_states(&theme);
            for (state, fill) in [("rest", s.accent_fill), ("hover", hovered), ("pressed", pressed)]
            {
                let ratio = s.accent_ink.contrast(fill);
                assert!(ratio >= 4.5, "{variant:?} {state}: the ink reads {ratio:.2}");
            }
        }
    }
}
