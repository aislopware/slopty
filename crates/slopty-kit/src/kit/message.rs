//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame is the thread's one card (`docs/decisions/ui.md`, "The thread is one card
//! language"): boxed in the reading column it rests on the card's wash inside its ring at a
//! card's radius, `MonoCode`'s composer; in a narrow pane it runs full-bleed, edge to edge under
//! a sash line, as a pane's own bar does. The keyboard in it steps the ring to the keyboard's
//! tint and adds no fill: the caret says where the keyboard is. Its words are the
//! prose's: what is written to an agent is read at length. The send control is a 26 pt square
//! at a control's radius at the foot's trailing end, in the neutral solid, sending or
//! stopping. On a touch screen its side is the touch target's. It carries no key badge: its
//! key is in the palette.

use gpui::accesskit::Role;
use gpui::{
    Div, InteractiveElement as _, ParentElement as _, Stateful, StatefulInteractiveElement as _,
    Styled, div, px,
};
use slopty_theme::{Density, Theme};

use crate::colors::hsla;
use crate::icons::Symbol;

/// The send control's side at a pointer's density: Zed's and Warp's 26 pt square.
pub const SEND: f32 = 26.0;

/// How a message's frame stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Something stands on it as its head (a thread's tray), so only its foot's corners round.
    pub capped: bool,
    /// The keyboard is in it: its ring the keyboard's blue.
    pub focused: bool,
    /// The pane is narrow: edge to edge with no ring and no radius, the line along its top.
    pub bleeds: bool,
}

/// `el` as a message's frame, standing as `frame` says.
pub fn shell<E: Styled>(el: E, theme: &Theme, frame: Frame) -> E {
    let s = theme.surfaces;
    if frame.bleeds {
        return el.border_t(super::HAIR).border_color(hsla(s.stroke));
    }
    let el = super::inset(el, theme);
    let r = px(theme.radii.md);
    let el = if frame.capped { el.rounded_bl(r).rounded_br(r) } else { el.rounded(r) };
    let ring = if frame.focused { hsla(s.focus) } else { hsla(s.border) };
    el.border(super::HAIR).border_color(ring)
}

/// The send control under `id`, named `label` and drawn as `glyph`.
///
/// A [`SEND`] square at `radii.sm`, Zed's tinted accent, whether it sends work on or stops a
/// turn; the glyph says which. The caller says what a click does.
#[must_use]
pub fn send_control(
    theme: &Theme,
    id: &'static str,
    glyph: Symbol,
    label: &'static str,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let side = if theme.density == Density::TOUCH { theme.density.hit } else { SEND };
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(side))
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

    use super::{Frame, SEND, send_control, shell};
    use crate::colors::hsla;
    use crate::icons::Symbol;

    /// Boxed, the frame is the thread's one card: the card's wash inside the `border` ring at a
    /// card's radius, never `elevated`. Focused, its ring steps to the keyboard's tint and the
    /// fill stays, so focus is no second surface. In a narrow pane it bleeds, square under a
    /// sash.
    #[test]
    fn a_message_is_the_threads_card_and_focus_steps_its_ring() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut rest = shell(div(), &theme, Frame::default());
            let style = rest.style().clone();
            let r = Some(gpui::px(theme.radii.md).into());
            assert_eq!(style.corner_radii.top_left, r, "{variant:?}: a card's radius");
            assert_eq!(style.border_color, Some(hsla(s.border)), "{variant:?}: the ring");
            let well = Some(gpui::Fill::from(hsla(s.well)));
            assert_eq!(style.background, well, "{variant:?}: a well's ground");
            let focus = Frame { focused: true, ..Frame::default() };
            let mut held = shell(div(), &theme, focus);
            let held = held.style().clone();
            assert_eq!(
                held.border_color,
                Some(hsla(s.focus)),
                "{variant:?}: the keyboard's border"
            );
            assert_eq!(held.background, style.background, "{variant:?}: no second fill");
            let capped = Frame { capped: true, ..Frame::default() };
            let mut under = shell(div(), &theme, capped);
            let under = under.style();
            assert!(under.corner_radii.top_left.is_none(), "{variant:?}: its head rounds the top");
            assert_eq!(under.corner_radii.bottom_left, r, "{variant:?}: its foot rounds");
            let mut bled = shell(div(), &theme, Frame { bleeds: true, ..Frame::default() });
            let bled = bled.style();
            assert!(bled.corner_radii.top_left.is_none(), "{variant:?}: square");
            assert!(bled.border_widths.left.is_none(), "{variant:?}: edge to edge");
            assert!(bled.border_widths.top.is_some(), "{variant:?}: a line on top");
            assert_eq!(bled.border_color, Some(hsla(s.stroke)), "{variant:?}: the line");
            assert!(bled.background.is_none(), "{variant:?}: on the pane's own plane");
        }
    }

    /// Send is a 26 pt square at a control's radius in the neutral solid, stopping or sending.
    #[test]
    fn send_is_a_tinted_square() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            for glyph in [Symbol::ArrowUp, Symbol::StopFill] {
                let mut send = send_control(&theme, "s", glyph, "Send");
                let style = send.style();
                assert_eq!(style.size.width, Some(gpui::px(SEND).into()), "{variant:?}");
                let r = Some(gpui::px(theme.radii.sm).into());
                assert_eq!(style.corner_radii.top_left, r, "{variant:?}: square at 4");
                let tinted = Some(gpui::Fill::from(hsla(theme.surfaces.info_background)));
                assert_eq!(style.background, tinted, "{variant:?}: the tinted accent");
            }
        }
    }
}
