// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The right pane's surface toolbars and their controls: a 38 px bar with 24 px controls, a
//! field for search and addresses, and the neutral tab chip.

use gpui::prelude::*;
use gpui::{Div, ElementId, Stateful, div, px};
use slopty_theme::layout;

use crate::theme::{Theme, ink, wash};

/// The toolbar's height.
pub const HEADER_HEIGHT: f32 = layout::TOOLBAR_HEIGHT;
/// A toolbar control's side.
pub const CONTROL_SIZE: f32 = 24.0;
/// A toolbar control's radius.
pub const CONTROL_RADIUS: f32 = layout::radius::CONTROL;
/// A toolbar icon's side.
pub const ICON_SIZE: f32 = 14.0;
/// Between toolbar controls.
pub const CONTROL_GAP: f32 = layout::space::XS;
/// The toolbar's edge inset.
pub const EDGE_INSET: f32 = layout::space::SM;

/// The field the file search and the browser's address share.
#[must_use]
pub fn input() -> Div {
    div()
        .h(px(CONTROL_SIZE))
        .min_w_0()
        .flex_1()
        .px(px(layout::space::SM))
        .rounded(px(CONTROL_RADIUS))
        .bg(ink(0.035))
        .flex()
        .items_center()
        .gap(px(layout::space::ROW_Y))
        .text_size(px(11.5))
}

/// A surface toolbar: hairlines above and below, the shell's tone.
#[must_use]
pub fn toolbar(theme: &Theme) -> Div {
    div()
        .h(px(HEADER_HEIGHT))
        .w_full()
        .flex_none()
        .px(px(EDGE_INSET))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .border_t_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(if theme.is_glass() { theme.surface.opacity(0.26) } else { theme.surface })
}

/// The neutral tab chip of the right pane's strip, without its hover.
#[must_use]
pub fn tab_frame(id: impl Into<ElementId>, selected: bool, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(CONTROL_SIZE))
        .flex_none()
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_selected(selected)
        .tab_index(0)
        .text_size(px(12.0))
        .text_color(if selected { theme.text } else { theme.text_muted })
        .focus_visible(|style| style.border_2().border_color(theme.accent))
        .when(selected, |el| el.bg(wash(0.10)))
}

/// The neutral tab chip with its hover wash.
#[must_use]
pub fn tab(id: impl Into<ElementId>, selected: bool, theme: &Theme) -> Stateful<Div> {
    tab_frame(id, selected, theme).when(!selected, |el| el.hover(|style| style.bg(wash(0.06))))
}
