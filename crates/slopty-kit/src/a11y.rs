//! Accessibility, for tests: a trimmed copy of the tree a screen reader reads.
//!
//! The screen reader side is GPUI's accesskit tree: roles and labels sit on the elements
//! (`.role`, `.aria_label`, `.aria_value`), macOS and iOS read the same tree (the iOS bridge
//! lives in `gpui_ios`). `tree` (under `cfg(test)` or the `e2e` feature) is what a test reads
//! instead of a screen reader. The keyboard ring and its focus rings are the shell's
//! (`focus_visible`, `slopty_shell::live::chrome`).

#[cfg(any(test, feature = "e2e"))]
use gpui::Window;

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
    /// How a live region announces a change to its value (`Polite`, `Assertive`); `None` off
    /// every live region.
    pub live: Option<String>,
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
            live: node.live().map(|live| format!("{live:?}")),
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
