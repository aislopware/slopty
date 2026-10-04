//! Dot, Slopty's own companion, where no agent is: beside the mark on the empty workspace,
//! placed over the layout, never in it, so nothing moves with companions on or off.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, ClickEvent, InteractiveElement as _, IntoElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_theme::Theme;

use super::{Kind, Pose, companion, shown};

/// What Dot is called to a screen reader.
pub(crate) const DOT: &str = "Dot";

/// Dot standing at the foot of the empty workspace's mark, on its right: its eye the mark's
/// cursor, unlit as it blinks (`lit`), one hop each time it is clicked (`pets` so far, counted
/// by `pet`). None with companions off. The mark it stands beside must be `relative`.
pub(crate) fn beside_mark(
    theme: &Theme,
    lit: bool,
    pets: u32,
    pet: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Option<AnyElement> {
    if !shown(theme) {
        return None;
    }
    let dot = companion(theme, Kind::Dot, Pose::Idle)
        .large()
        .side(px(theme.typography.icon_large()))
        .eyes_shut(!lit)
        .petted("dot-companion", pets);
    Some(
        div()
            .id("dot")
            .debug_selector(|| "dot".to_owned())
            .role(Role::Button)
            .aria_label(DOT)
            .absolute()
            .left_full()
            .bottom_0()
            .ml(px(theme.spacing.md))
            .cursor_pointer()
            .on_click(pet)
            .child(dot)
            .into_any_element(),
    )
}
