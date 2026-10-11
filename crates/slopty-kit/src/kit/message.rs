//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame is Delta's composer: the next turn's row, unboxed. It runs its pane's width under
//! the lighter `stroke` line and lays nothing of its own on the pane's ground; the keyboard in
//! it is said by the caret, not by a ring. Its words are the prose's: what is written to an
//! agent is read at length. The send control is Zed's icon button in the tinted accent, a
//! button's side, at the foot's trailing end, sending or stopping. On a touch screen its side
//! is the touch target's. It carries no key badge: its key is in the palette.

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

/// The send control under `id`, named `label` and drawn as `glyph`.
///
/// An icon button's square ([`super::icon_button_side`]) at `radii.sm`, Zed's tinted accent,
/// whether it sends work on or stops a turn; the glyph says which. The caller says what a
/// click does.
#[must_use]
pub fn send_control(
    theme: &Theme,
    id: &'static str,
    glyph: Symbol,
    label: &'static str,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(super::icon_button_side(theme)))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .child(crate::icons::icon(theme, glyph, hsla(s.text)).size(px(theme.typography.icon())));
    super::tinted_pressable(el, theme)
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

    /// Send is an icon button's square in the tinted accent, stopping or sending.
    #[test]
    fn send_is_a_tinted_square() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            for glyph in [Symbol::Send, Symbol::StopFill] {
                let mut send = send_control(&theme, "s", glyph, "Send");
                let style = send.style();
                let side = Some(gpui::px(crate::kit::icon_button_side(&theme)).into());
                assert_eq!(style.size.width, side, "{variant:?}");
                let r = Some(gpui::px(theme.radii.sm).into());
                assert_eq!(style.corner_radii.top_left, r, "{variant:?}: square at 4");
                let tinted = Some(gpui::Fill::from(hsla(theme.surfaces.info_background)));
                assert_eq!(style.background, tinted, "{variant:?}: the tinted accent");
            }
        }
    }
}
