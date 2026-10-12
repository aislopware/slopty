//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame is Delta's composer: the next turn's row, unboxed. It runs its pane's width under
//! the lighter `stroke` line and lays nothing of its own on the pane's ground; the keyboard in
//! it is said by the caret, not by a ring. Its words are the prose's: what is written to an
//! agent is read at length. The send control is Delta's bordered square, an icon button's
//! side, tinted in the accent while a press would act, at the foot's trailing end, sending or
//! stopping. On a touch screen its side is the touch target's. It carries no key badge: its key is
//! in the palette.

use gpui::accesskit::Role;
use gpui::{
    Div, InteractiveElement as _, ParentElement as _, Stateful, StatefulInteractiveElement as _,
    Styled, div, px,
};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::Symbol;

/// `el` as a message's frame: the lighter line along its top, and no fill, ring or radius.
pub fn shell<E: Styled>(el: E, theme: &Theme) -> E {
    el.border_t(super::HAIR).border_color(hsla(theme.surfaces.stroke))
}

/// The send control under `id`, named `label` and drawn as `glyph`, `armed` while a press
/// would act (a draft to send, a turn to stop).
///
/// Delta's send, measured from the signed-in app (`.research/delta-signed-in-2026-10-12.md`
/// §2.6): an icon button's square (`density.button`, 22) at `radii.sm` inside the lighter
/// line on the element's ground, its glyph quiet; armed, it takes the accent's quiet ground,
/// the selection's line and the accent's glyph. The caller says what a click does.
#[must_use]
pub fn send_control(
    theme: &Theme,
    id: &'static str,
    glyph: Symbol,
    label: &'static str,
    armed: bool,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let (tinted_hover, tinted_pressed) = super::tinted_states(theme);
    let (ground, line, ink) = if armed {
        (hsla(s.info_background), hsla(s.selected_border), hsla(s.accent))
    } else {
        (hsla(s.element), hsla(s.stroke), hsla(s.icon_muted))
    };
    let (hovered, pressed) = if armed {
        (hsla(tinted_hover), hsla(tinted_pressed))
    } else {
        (hsla(s.hover), hsla(s.pressed))
    };
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(theme.density.button))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm))
        .border(super::HAIR)
        .border_color(line)
        .bg(ground)
        .cursor_pointer()
        .child(crate::icons::icon(theme, glyph, ink));
    super::eased(el).hover(move |el| el.bg(hovered)).active(move |el| el.bg(pressed))
}

#[cfg(test)]
mod tests {
    use gpui::{Styled as _, div};
    use slopty_theme::{Theme, Variant};

    use super::{send_control, shell};
    use crate::colors::hsla;
    use crate::icons::Symbol;

    /// The frame is Delta's unboxed composer: the lighter line along its top, square, with no
    /// ring and no fill of its own, so it lies on the pane's ground as the turns above it do.
    #[test]
    fn a_message_is_the_next_turn_unboxed() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let mut frame = shell(div(), &theme);
            let style = frame.style();
            assert!(style.border_widths.top.is_some(), "{variant:?}: a line on top");
            assert!(style.border_widths.left.is_none(), "{variant:?}: no ring");
            assert!(style.border_widths.bottom.is_none(), "{variant:?}: no ring");
            assert_eq!(style.border_color, Some(hsla(theme.surfaces.stroke)), "{variant:?}");
            assert!(style.corner_radii.top_left.is_none(), "{variant:?}: square");
            assert!(style.background.is_none(), "{variant:?}: on the pane's own ground");
        }
    }

    /// Send is Delta's 22 pt square on the element's ground inside the lighter line, quiet
    /// at rest; armed (a draft, a turn to stop) it takes the accent's ground and glyph.
    #[test]
    fn send_is_a_bordered_square_tinted_when_armed() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            for glyph in [Symbol::Send, Symbol::StopFill] {
                for armed in [false, true] {
                    let mut send = send_control(&theme, "s", glyph, "Send", armed);
                    let style = send.style();
                    let side = Some(gpui::px(theme.density.button).into());
                    assert_eq!(style.size.width, side, "{variant:?}");
                    assert_eq!(style.size.height, side, "{variant:?}: square");
                    let r = Some(gpui::px(theme.radii.sm).into());
                    assert_eq!(style.corner_radii.top_left, r, "{variant:?}: square at 4");
                    assert!(style.border_widths.left.is_some(), "{variant:?}: bordered");
                    let (line, ground) = if armed {
                        (hsla(s.selected_border), hsla(s.info_background))
                    } else {
                        (hsla(s.stroke), hsla(s.element))
                    };
                    assert_eq!(style.border_color, Some(line), "{variant:?} {armed}");
                    assert_eq!(style.background, Some(ground.into()), "{variant:?} {armed}");
                }
            }
        }
    }
}
