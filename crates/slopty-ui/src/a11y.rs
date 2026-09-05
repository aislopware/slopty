//! Accessibility: the keyboard ring and, for tests, a trimmed copy of the tree.
//!
//! Every button-like element goes through [`tab_stop`]: it becomes focusable in reading
//! order (render order, all at tab index 0), Tab and ⇧Tab move along the ring, Enter and
//! Space click it (GPUI's keyboard click), and while it holds the keyboard focus that came
//! from the keyboard it wears the theme's accent as a hairline ring. A mouse press does not
//! move the focus to it: the terminal keeps the keyboard, as macOS buttons behave.
//!
//! The screen reader side is GPUI's accesskit tree: roles and labels sit on the elements
//! (`.role`, `.aria_label`, `.aria_value`), macOS and iOS read the same tree (the iOS bridge
//! lives in `gpui_ios`). `tree` (under `cfg(test)` or the `e2e` feature) is what a test reads
//! instead of a screen reader.

use gpui::{KeyDownEvent, StatefulInteractiveElement, Styled, Window};
use slopty_theme::Rgb;

use crate::colors::hsla;

/// Make `el` one stop of the keyboard ring (see the module docs).
pub fn tab_stop<E: StatefulInteractiveElement + Styled>(el: E, ring: Rgb) -> E {
    el.tab_index(0)
        .focus_visible(move |style| style.border_1().border_color(hsla(ring)))
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

/// A key-bar key's name for a screen reader.
#[must_use]
pub fn key_name(label: &str) -> &'static str {
    match label {
        "esc" => "Escape",
        "tab" => "Tab",
        "⌃" => "Control",
        "⌘" => "Command",
        "←" => "Left arrow",
        "↑" => "Up arrow",
        "↓" => "Down arrow",
        "→" => "Right arrow",
        "-" => "Minus",
        "/" => "Slash",
        "|" => "Pipe",
        "~" => "Tilde",
        "copy" => "Copy",
        "paste" => "Paste",
        "select" => "Select",
        _ => "Key",
    }
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
