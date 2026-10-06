//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame is Zed's and Warp's squared down (`docs/decisions/ui.md`, "The design system
//! starts from `MonoCode`'s"): boxed in the reading column it rests on the raised step inside
//! the control's ring at a card's radius, 6 pt; in a narrow pane it runs full-bleed, edge to
//! edge under a sash line, as a pane's own bar does. The keyboard in it adds the card's 3 %
//! wash and leaves the ring alone: the caret says where the keyboard is. Its words are the
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
use crate::icons::{IconSize, Symbol};

/// The send control's side at a pointer's density: Zed's and Warp's 26 pt square.
pub const SEND: f32 = 26.0;

/// How a message's frame stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Something stands on it as its head (a thread's tray), so only its foot's corners round.
    pub capped: bool,
    /// The keyboard is in it: the card's wash over the raised step.
    pub focused: bool,
    /// The pane is narrow: edge to edge with no ring and no radius, a sash line along its top.
    pub bleeds: bool,
}

/// `el` as a message's frame, standing as `frame` says.
pub fn shell<E: Styled>(el: E, theme: &Theme, frame: Frame) -> E {
    let s = theme.surfaces;
    let fill = if frame.focused { s.card.over(s.elevated) } else { s.elevated };
    let el = el.bg(hsla(fill));
    if frame.bleeds {
        return el.border_t(super::HAIR).border_color(hsla(s.sash));
    }
    let r = px(theme.radii.md);
    let el = if frame.capped { el.rounded_bl(r).rounded_br(r) } else { el.rounded(r) };
    el.border(super::HAIR).border_color(hsla(s.border))
}

/// The send control under `id`, named `label` and drawn as `glyph`.
///
/// A [`SEND`] square at `radii.sm` in the neutral solid with its ink, whether it sends work on
/// or stops a turn; the glyph says which. The caller says what a click does.
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
        .child(
            crate::icons::icon(theme, glyph, IconSize::Inline, hsla(s.solid_ink))
                .size(px(theme.typography.icon())),
        );
    super::solid_pressable(el, theme)
}

#[cfg(test)]
mod tests {
    use gpui::{Styled as _, div};
    use slopty_theme::{Theme, Variant};

    use super::{Frame, SEND, send_control, shell};
    use crate::colors::hsla;
    use crate::icons::Symbol;

    /// Boxed, the frame is a card's radius inside the control's ring; focused, it takes the
    /// card's wash and keeps the same ring; in a narrow pane it bleeds, square under a sash.
    #[test]
    fn a_message_is_boxed_or_bleeds_and_focus_is_a_wash() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut rest = shell(div(), &theme, Frame::default());
            let style = rest.style().clone();
            let r = Some(gpui::px(theme.radii.md).into());
            assert_eq!(style.corner_radii.top_left, r, "{variant:?}: a card's radius");
            assert_eq!(style.border_color, Some(hsla(s.border)), "{variant:?}: the ring");
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.elevated))));
            let focus = Frame { focused: true, ..Frame::default() };
            let mut held = shell(div(), &theme, focus);
            let held = held.style().clone();
            assert_eq!(held.border_color, style.border_color, "{variant:?}: the ring stays");
            let washed = gpui::Fill::from(hsla(s.card.over(s.elevated)));
            assert_eq!(held.background, Some(washed), "{variant:?}: the wash");
            assert_ne!(held.background, style.background, "{variant:?}: focus shows");
            let mut bled = shell(div(), &theme, Frame { bleeds: true, ..Frame::default() });
            let bled = bled.style();
            assert!(bled.corner_radii.top_left.is_none(), "{variant:?}: square");
            assert!(bled.border_widths.left.is_none(), "{variant:?}: edge to edge");
            assert!(bled.border_widths.top.is_some(), "{variant:?}: a line on top");
            assert_eq!(bled.border_color, Some(hsla(s.sash)), "{variant:?}: the sash");
        }
    }

    /// Send is a 26 pt square at a control's radius in the neutral solid, stopping or sending.
    #[test]
    fn send_is_a_neutral_square() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            for glyph in [Symbol::ArrowUp, Symbol::StopFill] {
                let mut send = send_control(&theme, "s", glyph, "Send");
                let style = send.style();
                assert_eq!(style.size.width, Some(gpui::px(SEND).into()), "{variant:?}");
                let r = Some(gpui::px(theme.radii.sm).into());
                assert_eq!(style.corner_radii.top_left, r, "{variant:?}: square at 4");
                let solid = Some(gpui::Fill::from(hsla(theme.surfaces.solid)));
                assert_eq!(style.background, solid, "{variant:?}: the neutral solid");
            }
        }
    }
}
