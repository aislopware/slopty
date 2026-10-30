//! The element a session's panes are drawn in: it lays the [`Panes`] tree out in the bounds it is
//! given, in the frame it is given them (so a pane never trails the window by a frame), draws each
//! pane at its rectangle, and draws and drags the dividers between them (Ghostty's craft:
//! `docs/decisions/layout.md`, "One session per view, splits on demand").

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, CursorStyle, DispatchPhase, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Style, Window, px, relative, size,
};

use super::panes::{Axis, DIVIDER, Divider, Grid, Laid, Pane, Panes};

/// A divider moved to give its split's first child this share of the room.
type OnRatio = Rc<dyn Fn(&[bool], f32, &mut Window, &mut App)>;
/// A divider double-clicked: every split back to even shares.
type OnEqualize = Rc<dyn Fn(&mut Window, &mut App)>;
/// How much narrower than its rectangle a pane fits its grid this frame, told before it lays out.
type OnHold = Rc<dyn Fn(Pane, Pixels, &mut App)>;

/// The panes of one view, drawn: see the module.
pub struct PaneArea {
    id: ElementId,
    panes: Panes,
    grid: Option<Grid>,
    children: Vec<(Pane, AnyElement)>,
    /// The dividers' colour: the terminal's ground darkened (Ghostty's).
    divider: Hsla,
    /// The panes each wearing a thin ring of the hue, over its content.
    rings: Vec<(Pane, Hsla)>,
    /// Where the area was laid out last, for walking focus by direction between frames.
    laid_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    on_ratio: OnRatio,
    on_equalize: OnEqualize,
    /// How much narrower the area is at the narrower end of a glide around it, and who hears
    /// each pane's share of that hold.
    glide: Option<(f32, OnHold)>,
}

/// The ring's width over an unfocused pane that needs the person.
const RING: f32 = 1.5;

/// A divider being dragged: its split's place in the tree, its axis and the split's rectangle.
type Dragging = Rc<RefCell<Option<(Vec<bool>, Axis, Bounds<Pixels>)>>>;

pub struct Prepainted {
    laid: Laid,
    children: Vec<(Bounds<Pixels>, AnyElement)>,
    grabs: Vec<(Divider, Hitbox)>,
    dragging: Dragging,
}

impl PaneArea {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        panes: Panes,
        grid: Option<Grid>,
        laid_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    ) -> Self {
        Self {
            id: id.into(),
            panes,
            grid,
            children: Vec::new(),
            divider: gpui::transparent_black(),
            rings: Vec::new(),
            laid_at,
            on_ratio: Rc::new(|_, _, _, _| {}),
            on_equalize: Rc::new(|_, _| {}),
            glide: None,
        }
    }

    /// Draw `pane` as `element`.
    pub(crate) fn pane(mut self, pane: Pane, element: impl IntoElement) -> Self {
        self.children.push((pane, element.into_any_element()));
        self
    }

    pub(crate) const fn divider(mut self, color: Hsla) -> Self {
        self.divider = color;
        self
    }

    pub(crate) fn ring(mut self, pane: Pane, hue: Hsla) -> Self {
        self.rings.push((pane, hue));
        self
    }

    pub(crate) fn on_ratio(
        mut self,
        f: impl Fn(&[bool], f32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_ratio = Rc::new(f);
        self
    }

    pub(crate) fn on_equalize(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_equalize = Rc::new(f);
        self
    }

    /// The area is `by` wider now than at the narrower end of a glide around it (zero at rest):
    /// before each pane lays out, `f` hears how much narrower than its rectangle it fits its
    /// grid, its rectangle now less its rectangle in the narrower area, so a terminal in it
    /// reflows once for the glide whatever its share of the room (`docs/decisions/ui.md`, "A
    /// glide reflows the TUI once").
    pub(crate) fn glide(mut self, by: f32, f: impl Fn(Pane, Pixels, &mut App) + 'static) -> Self {
        self.glide = Some((by, Rc::new(f)));
        self
    }
}

impl IntoElement for PaneArea {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The share of `span`'s room the pointer at `at` gives its split's first child.
fn ratio_at(axis: Axis, span: Bounds<Pixels>, at: gpui::Point<Pixels>) -> f32 {
    let (from, extent, at) = match axis {
        Axis::Row => (span.origin.x, span.size.width, at.x),
        Axis::Column => (span.origin.y, span.size.height, at.y),
    };
    let room = (f32::from(extent) - DIVIDER).max(1.0);
    f32::from(at - from) / room
}

impl Element for PaneArea {
    type PrepaintState = Prepainted;
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: relative(0.).into(),
            size: size(relative(1.).into(), relative(1.).into()),
            min_size: size(px(0.).into(), px(0.).into()),
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepainted {
        // The area always names itself (`id`); without a name a drag would last one frame.
        let dragging = id.map_or_else(Dragging::default, |id| {
            window.with_element_state::<Dragging, _>(id, |state, _| {
                let state = state.unwrap_or_default();
                (Rc::clone(&state), state)
            })
        });
        self.laid_at.set(Some(bounds));
        let laid = self.panes.layout(bounds, self.grid);
        // The panes as the narrower end of a glide lays them out, snapped to the same cells.
        let narrower = self.glide.as_ref().filter(|(by, _)| *by > 0.0).map(|(by, _)| {
            let mut at = bounds;
            at.size.width = (at.size.width - px(*by)).max(Pixels::ZERO);
            self.panes.layout(at, self.grid)
        });
        let mut children = Vec::new();
        for (pane, mut element) in std::mem::take(&mut self.children) {
            // A pane the layout leaves out (one zoomed over the rest) is not drawn.
            let Some(at) = laid.of(pane) else { continue };
            if let Some((_, on_hold)) = &self.glide {
                let then = narrower.as_ref().and_then(|narrower| narrower.of(pane));
                let held = then.map_or(Pixels::ZERO, |then| {
                    (at.size.width - then.size.width).max(Pixels::ZERO)
                });
                on_hold(pane, held, cx);
            }
            element.layout_as_root(at.size.into(), window, cx);
            element.prepaint_at(at.origin, window, cx);
            children.push((at, element));
        }
        // The grabs go in over the panes, so a press on a divider's edge drags it rather than
        // reaching the terminal under it; a scroll still reaches the terminal.
        let grabs = laid
            .dividers
            .iter()
            .map(|divider| {
                (
                    divider.clone(),
                    window.insert_hitbox(divider.grab(), HitboxBehavior::BlockMouseExceptScroll),
                )
            })
            .collect();
        Prepainted { laid, children, grabs, dragging }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        prepaint: &mut Prepainted,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (_, element) in &mut prepaint.children {
            element.paint(window, cx);
        }
        for (pane, hue) in &self.rings {
            if let Some(at) = prepaint.laid.of(*pane) {
                window.paint_quad(gpui::quad(
                    at,
                    0.0,
                    gpui::transparent_black(),
                    px(RING),
                    *hue,
                    gpui::BorderStyle::Solid,
                ));
            }
        }
        let dragged = prepaint.dragging.borrow().as_ref().map(|(path, ..)| path.clone());
        for (divider, grab) in &prepaint.grabs {
            let line = match divider.axis {
                Axis::Row => Bounds::new(
                    gpui::point(divider.at, divider.span.origin.y),
                    size(px(DIVIDER), divider.span.size.height),
                ),
                Axis::Column => Bounds::new(
                    gpui::point(divider.span.origin.x, divider.at),
                    size(divider.span.size.width, px(DIVIDER)),
                ),
            };
            window.paint_quad(gpui::fill(line, self.divider));
            let cursor = match divider.axis {
                Axis::Row => CursorStyle::ResizeLeftRight,
                Axis::Column => CursorStyle::ResizeUpDown,
            };
            if dragged.as_ref() == Some(&divider.path) {
                window.set_window_cursor_style(cursor);
            } else {
                window.set_cursor_style(cursor, grab);
            }
            let (grab, divider, dragging, equalize) = (
                grab.clone(),
                divider.clone(),
                Rc::clone(&prepaint.dragging),
                Rc::clone(&self.on_equalize),
            );
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !grab.is_hovered(window) {
                    return;
                }
                if event.click_count >= 2 {
                    dragging.replace(None);
                    equalize(window, cx);
                } else {
                    dragging.replace(Some((divider.path.clone(), divider.axis, divider.span)));
                }
                cx.stop_propagation();
            });
        }
        let (dragging, on_ratio) = (Rc::clone(&prepaint.dragging), Rc::clone(&self.on_ratio));
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let Some((path, axis, span)) = dragging.borrow().clone() else { return };
            if !event.dragging() {
                dragging.replace(None);
                return;
            }
            on_ratio(&path, ratio_at(axis, span, event.position), window, cx);
            cx.stop_propagation();
        });
        let dragging = Rc::clone(&prepaint.dragging);
        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble && dragging.borrow_mut().take().is_some() {
                window.refresh();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::point;

    use super::*;

    /// The pointer's share of a split's room, from the split's leading edge, along its axis.
    #[test]
    fn a_drag_gives_the_pointers_share_of_the_room() {
        let span = Bounds::new(point(px(100.0), px(50.0)), size(px(401.0), px(201.0)));
        assert_eq!(ratio_at(Axis::Row, span, point(px(200.0), px(0.0))), 0.25);
        assert_eq!(ratio_at(Axis::Column, span, point(px(0.0), px(150.0))), 0.5);
    }
}
