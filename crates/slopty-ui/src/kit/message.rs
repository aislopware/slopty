//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame floats on the resting elevation inside one hairline, its corners at the radius of
//! what floats. Its words are the prose's: what is written to an agent is read at length. The
//! send control is the solid, a control's side, at the foot's trailing end; on a touch screen
//! its side is the touch target's. It carries no key badge: its key is in the palette.

use gpui::accesskit::Role;
use gpui::{
    Div, InteractiveElement as _, ParentElement as _, Stateful, StatefulInteractiveElement as _,
    Styled, div, px,
};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};

/// `el` as a message's frame at `zoom`.
///
/// `capped`: something stands on it as its head (a thread's tray), so only its foot's corners
/// round and it casts the stack's contact alone. `focused`: the keyboard is in it, and its
/// hairline takes the quiet focus tone of a field that has the keyboard
/// ([`slopty_theme::Theme::field_focus`]).
pub fn shell<E: Styled>(el: E, theme: &Theme, zoom: f32, capped: bool, focused: bool) -> E {
    let s = theme.surfaces;
    let r = px(theme.radii.lg * zoom);
    let el =
        if capped { el.rounded_bl(r).rounded_br(r) } else { el.rounded(px(theme.radii.lg * zoom)) };
    let edge = if focused { hsla(theme.field_focus()) } else { hsla(s.border) };
    let el = el.border(super::HAIR).border_color(edge).bg(hsla(s.elevated));
    super::rests(el, theme, !capped, true)
}

/// The send control at `zoom`, under `id`, named `label` and drawn as `glyph`: the solid, a
/// control's side square, the glyph in the solid's ink. The caller says what a click does.
#[must_use]
pub fn send_control(
    theme: &Theme,
    zoom: f32,
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
        .size(px(theme.density.control * zoom))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm * zoom))
        .cursor_pointer()
        .child(
            crate::icons::icon(theme, glyph, IconSize::Inline, hsla(s.solid_ink))
                .size(px(theme.typography.icon() * zoom)),
        );
    super::solid_pressable(el, theme)
}
