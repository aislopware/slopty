//! The one place a person writes to an agent, wherever it shows.
//!
//! A thread's composer and the board's message to its orchestrator share its frame
//! ([`shell`]) and its send control ([`send_control`]), so a message reads as a message, not
//! as a form's field, in both.
//!
//! The frame floats on the resting elevation inside the quiet hairline, its corners at the
//! radius of what floats. Its words are the prose's: what is written to an agent is read at
//! length. The send control is a disc a control's side across at the foot's trailing end, in
//! the green that sets work going ([`super::go()`]); while it stops a turn it is the neutral
//! solid. On a touch screen its side is the touch target's. It carries no key badge: its key
//! is in the palette.

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
/// hairline steps from the quiet one to the field's ordinary one: the caret says where the
/// keyboard is, and a near-black ring round the card was the loudest thing on the screen.
pub fn shell<E: Styled>(el: E, theme: &Theme, zoom: f32, capped: bool, focused: bool) -> E {
    let s = theme.surfaces;
    let r = px(theme.radii.lg * zoom);
    let el =
        if capped { el.rounded_bl(r).rounded_br(r) } else { el.rounded(px(theme.radii.lg * zoom)) };
    let edge = if focused { hsla(s.border) } else { hsla(s.stroke) };
    el.border(super::HAIR).border_color(edge).bg(hsla(s.elevated))
}

/// The send control at `zoom`, under `id`, named `label` and drawn as `glyph`.
///
/// A disc a control's side across, in the green with its ink while it sends work on (`go`), in
/// the neutral solid while it stops a turn. The caller says what a click does.
#[must_use]
pub fn send_control(
    theme: &Theme,
    zoom: f32,
    id: &'static str,
    glyph: Symbol,
    label: &'static str,
    go: bool,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let ink = if go { super::go_ink(theme) } else { s.solid_ink };
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
        .rounded(px(theme.radii.full))
        .cursor_pointer()
        .child(
            crate::icons::icon(theme, glyph, IconSize::Inline, hsla(ink))
                .size(px(theme.typography.icon() * zoom)),
        );
    if go { super::go_pressable(el, theme) } else { super::solid_pressable(el, theme) }
}
