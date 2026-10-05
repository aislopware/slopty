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

use gpui::{
    FocusHandle, InteractiveElement, KeyDownEvent, Outline, StatefulInteractiveElement, Styled,
    WeakFocusHandle, Window, px,
};
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
        .focus_visible(move |style| style.outline_ring(ring(ring_color)))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
        .on_key_down(|event, window, cx| {
            if cycle(event, window, cx) {
                cx.stop_propagation();
            }
        })
}

/// Tab and ⇧Tab move the focus along the ring ([`step`]); `true` when the key was one of them.
pub fn cycle(event: &KeyDownEvent, window: &mut Window, cx: &mut gpui::App) -> bool {
    let stroke = &event.keystroke;
    if stroke.key != "tab" || stroke.modifiers.control || stroke.modifiers.platform {
        return false;
    }
    step(!stroke.modifiers.shift, window, cx);
    true
}

mod marker {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    gpui::actions!(
        a11y,
        [
            /// Never dispatched: a trap answers it, so the frame says whether a trap is in it.
            Trapped
        ]
    );
}
use marker::Trapped;

/// The traps registered so far, in the order they first drew: each a modal's scope and the
/// handle its keyboard lives at. One closed is not in the frame, so it holds nothing.
#[derive(Default)]
struct Traps(Vec<(WeakFocusHandle, WeakFocusHandle)>);

impl gpui::Global for Traps {}

/// Make `el` a modal's focus trap, tracking `scope`.
///
/// While it is drawn and [`hold`]s, Tab and ⇧Tab walk the stops inside it and wrap there,
/// never out to the tiles behind. A trap opened inside another is the one that holds.
///
/// After Ely's `FocusScope::trap` (`primitives/focus.rs`, Copyright (c) 2026 Ely GPUI
/// Component contributors, MIT OR Apache-2.0), registered rather than wrapped, since our Tab
/// is each stop's own key handler ([`tab_stop`]).
pub fn trap<E: InteractiveElement>(el: E, scope: &FocusHandle) -> E {
    el.track_focus(scope).on_action(|_: &Trapped, _window, _cx| {})
}

/// Register the trap `scope` ([`trap`]), whose keyboard lives at `home`.
///
/// A keyboard lost while it is drawn comes back there ([`reclaim`]). Held until either handle
/// is dropped; one that is not drawn holds nothing meanwhile.
pub fn hold(scope: &FocusHandle, home: &FocusHandle, cx: &mut gpui::App) {
    let traps = cx.default_global::<Traps>();
    traps.0.retain(|(scope, home)| scope.upgrade().is_some() && home.upgrade().is_some());
    let weak = scope.downgrade();
    match traps.0.iter_mut().find(|(s, _)| *s == weak) {
        Some(found) => found.1 = home.downgrade(),
        None => traps.0.push((weak, home.downgrade())),
    }
}

/// The innermost trap drawn in the last frame that `holds`, as (scope, home).
fn innermost(
    window: &Window,
    cx: &gpui::App,
    holds: impl Fn(&FocusHandle) -> bool,
) -> Option<(FocusHandle, FocusHandle)> {
    let traps = cx.try_global::<Traps>()?;
    let mut best: Option<(FocusHandle, FocusHandle)> = None;
    for (scope, home) in &traps.0 {
        let (Some(scope), Some(home)) = (scope.upgrade(), home.upgrade()) else { continue };
        if !holds(&scope) {
            continue;
        }
        if best.as_ref().is_none_or(|(outer, _)| outer.contains(&scope, window)) {
            best = Some((scope, home));
        }
    }
    best
}

/// Move the keyboard one stop along the ring, `forward` or back. Inside a trap it stays
/// there, wrapping at its ends; a trap with no stop keeps the focus where it is.
///
/// The walk is Ely's `step` (`primitives/focus.rs`, Copyright (c) 2026 Ely GPUI Component
/// contributors, MIT OR Apache-2.0).
pub fn step(forward: bool, window: &mut Window, cx: &mut gpui::App) {
    let advance = |window: &mut Window, cx: &mut gpui::App| {
        if forward {
            window.focus_next(cx);
        } else {
            window.focus_prev(cx);
        }
    };
    let Some((scope, _)) = innermost(window, cx, |scope| scope.contains_focused(window, cx)) else {
        advance(window, cx);
        return;
    };
    let origin = window.focused(cx);
    let mut first = None;
    loop {
        advance(window, cx);
        let focused = window.focused(cx);
        if scope.contains_focused(window, cx) && focused.as_ref() != Some(&scope) {
            return;
        }
        if focused.is_none() || focused == origin || (first.is_some() && focused == first) {
            break;
        }
        if first.is_none() {
            first = focused;
        }
    }
    if let Some(origin) = origin {
        window.focus(&origin, cx);
    }
}

/// The keyboard was lost (what held it left the frame): an open trap takes it back at its
/// home. `false` when no trap is open, for the owner to put it where it belongs.
pub fn reclaim(window: &mut Window, cx: &mut gpui::App) -> bool {
    let drawn = |scope: &FocusHandle| window.is_action_available_in(&Trapped, scope);
    let Some((_, home)) = innermost(window, cx, drawn) else {
        return false;
    };
    window.focus(&home, cx);
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

    /// Tab puts the ring round a stop: 2 pt clear of it, 2 pt wide, the focus tone (the chrome's
    /// text) at `alpha::STRONG`, drawn outside so nothing moves. A click shows no ring.
    #[gpui::test]
    fn the_keyboard_rings_a_stop_and_the_pointer_does_not(cx: &mut TestAppContext) {
        let theme = Theme::default();
        let color = hsla_alpha(theme.surfaces.focus, alpha::STRONG);
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

    /// A stage after Ely's overlay tests (`overlays/tests/mod.rs`, Copyright (c) 2026 Ely GPUI
    /// Component contributors, MIT OR Apache-2.0): a button before, a modal with two buttons and
    /// a home its keyboard lives at, and a button after.
    struct Stage {
        theme: Theme,
        scope: gpui::FocusHandle,
        home: gpui::FocusHandle,
        open: bool,
        /// The modal has no stop of its own.
        bare: bool,
    }

    impl Render for Stage {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            cx: &mut Context<Self>,
        ) -> impl IntoElement {
            use gpui::InteractiveElement as _;
            use gpui::prelude::FluentBuilder as _;
            let theme = &self.theme;
            let stop = |id: &'static str| button(theme, id, id, ButtonKind::Ghost);
            let modal = self.open.then(|| {
                crate::a11y::hold(&self.scope, &self.home, cx);
                let home = div().track_focus(&self.home).child("home");
                let modal = super::trap(div(), &self.scope).child(home);
                if self.bare { modal } else { modal.child(stop("one")).child(stop("two")) }
            });
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(stop("before"))
                .children(modal)
                .child(stop("after"))
                .when(false, |el| el)
        }
    }

    fn stage(
        cx: &mut TestAppContext,
        bare: bool,
    ) -> (gpui::Entity<Stage>, &mut gpui::VisualTestContext) {
        cx.add_window_view(|_window, cx| Stage {
            theme: Theme::default(),
            scope: cx.focus_handle(),
            home: cx.focus_handle(),
            open: false,
            bare,
        })
    }

    /// The label of the stop that has the keyboard, as a screen reader hears it.
    fn on(cx: &mut gpui::VisualTestContext) -> Option<String> {
        cx.update(|window, _| window.set_a11y_active(true));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let tree = cx.update(|window, _| super::tree(window));
        tree.into_iter().find(|n| n.focused).and_then(|n| n.label)
    }

    fn open(view: &gpui::Entity<Stage>, cx: &mut gpui::VisualTestContext) {
        view.update_in(cx, |stage, window, cx| {
            stage.open = true;
            window.focus(&stage.home, cx);
            cx.notify();
        });
        cx.run_until_parked();
    }

    /// Tab and ⇧Tab walk the open modal's stops and wrap there, never out to the buttons
    /// around it (Ely's `tab_walks_inside_an_open_popover`).
    #[gpui::test]
    fn tab_walks_inside_an_open_trap(cx: &mut TestAppContext) {
        let (view, cx) = stage(cx, false);
        open(&view, cx);
        // From its home, a field with no stop of its own, the ring's step enters the modal.
        cx.update(|window, cx| super::step(true, window, cx));
        assert_eq!(on(cx).as_deref(), Some("one"), "into the modal, not to the button before");
        let mut walked = Vec::new();
        for _ in 0..4 {
            cx.simulate_keystrokes("tab");
            walked.extend(on(cx));
        }
        assert_eq!(walked, ["two", "one", "two", "one"]);
        cx.simulate_keystrokes("shift-tab");
        assert_eq!(on(cx).as_deref(), Some("two"), "back, wrapping");
    }

    /// A modal with no stop keeps the keyboard where it is.
    #[gpui::test]
    fn a_trap_with_no_stop_keeps_the_keyboard(cx: &mut TestAppContext) {
        let (view, cx) = stage(cx, true);
        open(&view, cx);
        cx.simulate_keystrokes("tab");
        let home = view.read_with(cx, |stage, _| stage.home.clone());
        assert!(cx.update(|window, _| home.is_focused(window)), "still at home");
    }

    /// Closed, the trap holds nothing: Tab walks the whole window again.
    #[gpui::test]
    fn a_closed_trap_lets_tab_walk_the_window(cx: &mut TestAppContext) {
        let (view, cx) = stage(cx, false);
        open(&view, cx);
        view.update(cx, |stage, cx| {
            stage.open = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(gpui::Window::focus_next);
        let mut walked = Vec::new();
        for _ in 0..2 {
            cx.simulate_keystrokes("tab");
            walked.extend(on(cx));
        }
        assert_eq!(walked, ["after", "before"]);
    }

    /// A keyboard lost while the modal is open comes back to its home; with none open,
    /// [`super::reclaim`] leaves it to the owner.
    #[gpui::test]
    fn a_lost_keyboard_comes_back_to_the_open_trap(cx: &mut TestAppContext) {
        let (view, cx) = stage(cx, false);
        assert!(!cx.update(super::reclaim), "nothing open");
        open(&view, cx);
        cx.update(gpui::Window::blur);
        assert!(cx.update(super::reclaim));
        let home = view.read_with(cx, |stage, _| stage.home.clone());
        assert!(cx.update(|window, _| home.is_focused(window)));
    }
}
