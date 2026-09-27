//! Accessibility: the keyboard ring and, for tests, a trimmed copy of the tree.
//!
//! Every button-like element goes through [`tab_stop`]: it becomes focusable in reading
//! order (render order, all at tab index 0), Tab and ⇧Tab move along the ring, Enter and
//! Space click it (GPUI's keyboard click), and while it holds the keyboard focus that came
//! from the keyboard it wears the focus ring: a 2 pt gap, then a 2 pt ring of the accent at
//! [`alpha::STRONG`], its corners the element's grown by the gap and the ring. Pointer focus
//! shows nothing, and a mouse press does not move the focus to it: the terminal keeps the
//! keyboard, as macOS buttons behave.
//!
//! The screen reader side is GPUI's accesskit tree: roles and labels sit on the elements
//! (`.role`, `.aria_label`, `.aria_value`), macOS and iOS read the same tree (the iOS bridge
//! lives in `gpui_ios`). `tree` (under `cfg(test)` or the `e2e` feature) is what a test reads
//! instead of a screen reader.

use gpui::{KeyDownEvent, Outline, StatefulInteractiveElement, Styled, Window, px};
use slopty_theme::{Rgb, alpha};

use crate::colors::hsla_alpha;

/// The focus ring's gap from its element and its own width, in points.
///
/// Geist's `0 0 0 2px background, 0 0 0 4px blue`. The gap is left clear, so the ring stands
/// off any element on any surface, and a ring drawn outside it moves no layout.
pub const RING: f32 = 2.0;

/// The ring round a stop that holds the keyboard focus.
#[must_use]
pub fn ring(color: Rgb) -> Outline {
    Outline { color: hsla_alpha(color, alpha::STRONG), width: px(RING), offset: px(RING) }
}

/// Make `el` one stop of the keyboard ring (see the module docs).
pub fn tab_stop<E: StatefulInteractiveElement + Styled>(el: E, ring_color: Rgb) -> E {
    el.tab_index(0)
        .focus_visible(move |style| style.outline(ring(ring_color)))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
        .on_key_down(|event, window, cx| {
            if cycle(event, window, cx) {
                cx.stop_propagation();
            }
        })
}

/// Tab and ⇧Tab move the focus along the ring; `true` when the key was one of them.
pub fn cycle(event: &KeyDownEvent, window: &mut Window, cx: &mut gpui::App) -> bool {
    let stroke = &event.keystroke;
    if stroke.key != "tab" || stroke.modifiers.control || stroke.modifiers.platform {
        return false;
    }
    if stroke.modifiers.shift {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    true
}

/// One node of the tree as a test sees it.
#[cfg(any(test, feature = "e2e"))]
#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    /// The accesskit role, as `Debug` prints it (`Button`, `Terminal`, …).
    pub role: String,
    /// The label, if any.
    pub label: Option<String>,
    /// The value, if any (a terminal's cursor row, a text field's text).
    pub value: Option<String>,
    /// What the node does, beyond its label, if said.
    pub description: Option<String>,
    /// The node holds the keyboard focus.
    pub focused: bool,
    /// Window rect in points: x, y, w, h.
    pub bounds: [f32; 4],
}

/// The tree GPUI built for the last frame, depth first in reading order, trimmed to what a
/// screen reader reads; empty until [`Window::set_a11y_active`] and a frame.
#[cfg(any(test, feature = "e2e"))]
#[must_use]
pub fn tree(window: &Window) -> Vec<Node> {
    use std::collections::HashMap;

    use gpui::accesskit::{Node as AkNode, NodeId};

    let Some(update) = window.a11y_tree() else { return Vec::new() };
    let nodes: HashMap<NodeId, &AkNode> = update.nodes.iter().map(|(id, n)| (*id, n)).collect();
    let Some(root) = update.tree.as_ref().map(|t| t.root) else { return Vec::new() };
    let scale = window.scale_factor().max(0.01);
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(node) = nodes.get(&id) else { continue };
        let bounds = node.bounds().map_or([0.0; 4], |r| {
            #[expect(clippy::cast_possible_truncation, reason = "window points")]
            let p = |v: f64| (v as f32) / scale;
            [p(r.x0), p(r.y0), p(r.x1 - r.x0), p(r.y1 - r.y0)]
        });
        out.push(Node {
            role: format!("{:?}", node.role()),
            label: node.label().map(str::to_owned),
            value: node.value().map(str::to_owned),
            description: node.description().map(str::to_owned),
            focused: id == update.focus,
            bounds,
        });
        stack.extend(node.children().iter().rev().copied());
    }
    out
}

#[cfg(any(test, feature = "e2e"))]
impl Node {
    /// `true` when the node has `role` and, if given, `label`.
    #[must_use]
    pub fn is(&self, role: &str, label: Option<&str>) -> bool {
        self.role == role && label.is_none_or(|l| self.label.as_deref() == Some(l))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, div, px,
    };
    use slopty_theme::{Theme, alpha};

    use crate::colors::hsla_alpha;
    use crate::kit::{ButtonKind, button};

    struct Buttons(Theme);

    impl Render for Buttons {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            let theme = &self.0;
            div().size_full().p(px(theme.spacing.xl)).child(button(
                theme,
                "save",
                "Save",
                ButtonKind::Ghost,
            ))
        }
    }

    /// Tab puts the ring round a stop: 2 pt clear of it, 2 pt wide, the accent at
    /// `alpha::STRONG`, drawn outside so nothing moves. A click shows no ring.
    #[gpui::test]
    fn the_keyboard_rings_a_stop_and_the_pointer_does_not(cx: &mut TestAppContext) {
        let theme = Theme::default();
        let color = hsla_alpha(theme.surfaces.accent, alpha::STRONG);
        let (_view, cx) = cx.add_window_view(|_window, _cx| Buttons(theme.clone()));
        let rings = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            let (scale, quads) =
                cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
            quads
                .into_iter()
                .filter(|q| q.border_color == color)
                .map(|q| (q.bounds.size.width.0 / scale, q.border_widths.top.0 / scale))
                // A border-only quad is painted as four strips of itself, one per edge.
                .fold(Vec::new(), |mut out, ring| {
                    if !out.contains(&ring) {
                        out.push(ring);
                    }
                    out
                })
        };
        let save = cx.debug_bounds("save").expect("the button");
        assert!(rings(cx).is_empty(), "no ring at rest");
        // The window's first stop takes the focus, and a Tab (the one stop cycles to itself)
        // makes the keyboard the last input.
        cx.update(gpui::Window::focus_next);
        assert!(rings(cx).is_empty(), "focus alone, from no keystroke, shows no ring");
        cx.simulate_keystrokes("tab");
        let ring = super::RING;
        let expected = 4.0_f32.mul_add(ring, f32::from(save.size.width));
        assert!(
            matches!(rings(cx).as_slice(), [(w, b)] if (w - expected).abs() < 0.01 && (b - ring).abs() < 0.01),
            "{:?}",
            rings(cx)
        );
        cx.simulate_click(save.center(), gpui::Modifiers::default());
        assert!(rings(cx).is_empty(), "a click shows no ring");
    }
}
