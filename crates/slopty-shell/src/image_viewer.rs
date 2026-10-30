// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Shared image geometry and native pointer gestures. Scaling reuses the texture.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Image, MouseButton, Pixels, Point, ScrollDelta, Size, TouchPhase,
    Window, div, point, px, size,
};

/// One picture's viewer: its fit, zoom and pan, and the drag under way, shared by every
/// handler its element sets up.
#[derive(Clone, Default)]
pub struct ImageView(Rc<RefCell<ViewState>>);

impl std::fmt::Debug for ImageView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.0.borrow();
        f.debug_struct("ImageView")
            .field("geometry", &state.geometry)
            .field("dragged", &state.dragged)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct ViewState {
    owner: Option<gpui::EntityId>,
    geometry: Geometry,
    bounds: Bounds<Pixels>,
    drag: Option<(Point<f32>, Point<f32>)>,
    dragged: bool,
    pinch: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
struct Geometry {
    natural: Size<f32>,
    viewport: Size<f32>,
    scale: f32,
    pan: Point<f32>,
    fitted: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            natural: size(1.0, 1.0),
            viewport: size(1.0, 1.0),
            scale: 1.0,
            pan: point(0.0, 0.0),
            fitted: true,
        }
    }
}

impl Geometry {
    fn fit_scale(&self) -> f32 {
        (self.viewport.width / self.natural.width)
            .min(self.viewport.height / self.natural.height)
            .min(1.0)
    }

    fn resize(&mut self, natural: Size<f32>, viewport: Size<f32>) {
        if ![natural.width, natural.height, viewport.width, viewport.height]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
        {
            return;
        }
        self.natural = natural;
        self.viewport = viewport;
        if self.fitted {
            self.fit();
        } else {
            self.clamp_pan();
        }
    }

    fn fit(&mut self) {
        self.scale = self.fit_scale();
        self.pan = point(0.0, 0.0);
        self.fitted = true;
    }

    fn clamp_pan(&mut self) {
        let limit = point(
            (self.natural.width.mul_add(self.scale, -self.viewport.width) / 2.0).max(0.0),
            (self.natural.height.mul_add(self.scale, -self.viewport.height) / 2.0).max(0.0),
        );
        self.pan.x = self.pan.x.clamp(-limit.x, limit.x);
        self.pan.y = self.pan.y.clamp(-limit.y, limit.y);
    }

    fn zoom(&mut self, scale: f32, anchor: Point<f32>) {
        if !scale.is_finite() || scale <= 0.0 || !anchor.x.is_finite() || !anchor.y.is_finite() {
            return;
        }
        // Bound layout coordinates even for SVGs with enormous logical dimensions.
        let maximum = (131_072.0 / self.natural.width.max(self.natural.height))
            .min(32.0)
            .max(self.fit_scale());
        let scale = scale.clamp(self.fit_scale().min(0.01), maximum);
        let ratio = scale / self.scale;
        let anchor = anchor - point(self.viewport.width / 2.0, self.viewport.height / 2.0);
        self.pan = point(
            (anchor.x - self.pan.x).mul_add(-ratio, anchor.x),
            (anchor.y - self.pan.y).mul_add(-ratio, anchor.y),
        );
        self.scale = scale;
        self.fitted = false;
        self.clamp_pan();
    }

    fn pan_by(&mut self, delta: Point<f32>) -> bool {
        if !delta.x.is_finite() || !delta.y.is_finite() {
            return false;
        }
        let before = self.pan;
        self.pan += delta;
        self.clamp_pan();
        self.pan != before
    }

    fn image_origin(&self) -> Point<f32> {
        point(
            self.natural.width.mul_add(-self.scale, self.viewport.width) / 2.0 + self.pan.x,
            self.natural.height.mul_add(-self.scale, self.viewport.height) / 2.0 + self.pan.y,
        )
    }
}

fn scroll_pixels(delta: ScrollDelta) -> Point<f32> {
    match delta {
        ScrollDelta::Pixels(p) => point(f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(p) => point(p.x * 40.0, p.y * 40.0),
    }
}

impl ViewState {
    fn local(&self, position: Point<Pixels>) -> Point<f32> {
        point(
            f32::from(position.x - self.bounds.origin.x),
            f32::from(position.y - self.bounds.origin.y),
        )
    }

    fn pinch(&mut self, event: &gpui::PinchEvent) {
        if event.phase == TouchPhase::Ended {
            self.pinch = None;
            return;
        }
        if !event.delta.is_finite() {
            return;
        }
        if event.phase == TouchPhase::Started {
            self.pinch = Some((self.geometry.scale, 1.0));
        }
        let (start, factor) = self.pinch.get_or_insert((self.geometry.scale, 1.0));
        *factor = (*factor + event.delta).max(0.001);
        let scale = *start * *factor;
        let local = self.local(event.position);
        self.geometry.zoom(scale, local);
    }

    fn wheel(&mut self, event: &gpui::ScrollEvent) -> bool {
        let delta = scroll_pixels(event.delta);
        if event.modifiers.control {
            let scale = self.geometry.scale * (delta.y * 0.0025).clamp(-2.0, 2.0).exp();
            self.geometry.zoom(scale, self.local(event.position));
            true
        } else {
            self.geometry.pan_by(delta)
        }
    }

    fn pointer_down(&mut self, position: Point<Pixels>) {
        self.dragged = false;
        self.drag = Some((self.local(position), self.geometry.pan));
    }

    fn pointer_move(&mut self, event: &gpui::MouseMoveEvent) -> bool {
        if !event.dragging() {
            self.drag = None;
            return false;
        }
        let Some((start, pan)) = self.drag else {
            return false;
        };
        let delta = self.local(event.position) - start;
        if delta.x.hypot(delta.y) >= 4.0 {
            self.dragged = true;
        }
        if !self.dragged {
            return false;
        }
        self.geometry.pan = pan;
        self.geometry.pan_by(delta);
        true
    }
}

/// What a click on the picture itself (not the room around it) does.
pub type ImageClick = Rc<dyn Fn(&mut Window, &mut App)>;

impl ImageView {
    /// A press begins: what follows is a click until it drags.
    pub fn begin_click(&self) {
        let mut state = self.0.borrow_mut();
        state.drag = None;
        state.dragged = false;
    }

    /// Whether the press under way dragged the picture, so its release is no click.
    #[must_use]
    pub fn dragged(&self) -> bool {
        self.0.borrow().dragged
    }

    /// Back to the picture fitted, nothing under way: a viewer opened afresh.
    pub fn reset(&self) {
        *self.0.borrow_mut() = ViewState::default();
    }

    /// `image` at `natural` size, fitted to the room it is given until zoomed (a pinch, or the
    /// wheel with Control) and dragged; `plate` fills behind a transparent one.
    pub fn render(
        &self,
        image: Arc<Image>,
        natural: Size<Pixels>,
        on_image_click: Option<ImageClick>,
        plate: Option<gpui::Hsla>,
    ) -> AnyElement {
        let natural = size(f32::from(natural.width), f32::from(natural.height));
        {
            let mut state = self.0.borrow_mut();
            let bounds = state.bounds;
            state
                .geometry
                .resize(natural, size(f32::from(bounds.size.width), f32::from(bounds.size.height)));
        }
        let geometry = self.0.borrow().geometry;
        let origin = geometry.image_origin();
        let measure = self.clone();
        let wheel = self.clone();
        let pinch = self.clone();
        let down = self.clone();
        let movement = self.clone();
        let up = self.clone();
        let up_out = self.clone();
        let click = self.clone();
        let viewport = div()
            .id("image-viewport")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .w_full()
            .relative()
            .overflow_hidden()
            .on_scroll(move |event, window, cx| {
                if wheel.0.borrow_mut().wheel(event) {
                    cx.stop_propagation();
                    window.prevent_default();
                    if let Some(owner) = wheel.0.borrow().owner {
                        cx.notify(owner);
                    }
                }
            })
            .on_pinch(move |event, window, cx| {
                pinch.0.borrow_mut().pinch(event);
                cx.stop_propagation();
                window.prevent_default();
                if let Some(owner) = pinch.0.borrow().owner {
                    cx.notify(owner);
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, _| {
                down.0.borrow_mut().pointer_down(event.position);
                window.prevent_default();
            })
            .on_mouse_up(MouseButton::Left, move |_, _, _| {
                up.0.borrow_mut().drag = None;
            })
            .on_mouse_up_out(MouseButton::Left, move |_, _, _| {
                up_out.0.borrow_mut().drag = None;
            })
            .on_click(move |event, window, cx| {
                let state = click.0.borrow();
                if state.dragged {
                    cx.stop_propagation();
                    return;
                }
                let local = state.local(event.position());
                let geometry = state.geometry;
                let origin = geometry.image_origin();
                let inside = local.x >= origin.x
                    && local.y >= origin.y
                    && local.x <= geometry.natural.width.mul_add(geometry.scale, origin.x)
                    && local.y <= geometry.natural.height.mul_add(geometry.scale, origin.y);
                drop(state);
                if inside && let Some(on_click) = &on_image_click {
                    cx.stop_propagation();
                    on_click(window, cx);
                }
            })
            .when_some(plate, |viewport, plate| {
                viewport.child(
                    div()
                        .absolute()
                        .left(px(origin.x))
                        .top(px(origin.y))
                        .w(px(natural.width * geometry.scale))
                        .h(px(natural.height * geometry.scale))
                        .rounded(px(10.0))
                        .bg(plate),
                )
            })
            .child(
                gpui::img(image)
                    .absolute()
                    .left(px(origin.x))
                    .top(px(origin.y))
                    .w(px(natural.width * geometry.scale))
                    .h(px(natural.height * geometry.scale))
                    .object_fit(gpui::ObjectFit::Contain),
            )
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let mut state = measure.0.borrow_mut();
                        // Capture during prepaint, when GPUI knows the owning view.
                        let owner = window.current_view();
                        state.owner = Some(owner);
                        if state.bounds != bounds {
                            state.bounds = bounds;
                            state.geometry.resize(
                                natural,
                                size(f32::from(bounds.size.width), f32::from(bounds.size.height)),
                            );
                            window.defer(cx, move |_, cx| cx.notify(owner));
                        }
                    },
                    move |_, (), window, _| {
                        // Continue an image drag beyond its viewport without taking over
                        // other pointers. Release is handled inside and outside the hitbox.
                        let movement = movement;
                        window.on_mouse_event(
                            move |event: &gpui::MouseMoveEvent, phase, _window, cx| {
                                if phase == gpui::DispatchPhase::Bubble
                                    && movement.0.borrow_mut().pointer_move(event)
                                {
                                    cx.stop_propagation();
                                    if let Some(owner) = movement.0.borrow().owner {
                                        cx.notify(owner);
                                    }
                                }
                            },
                        );
                    },
                )
                .absolute()
                .inset_0(),
            );
        div().size_full().min_w_0().min_h_0().flex().flex_col().child(viewport).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn geometry() -> Geometry {
        let mut g = Geometry::default();
        g.resize(size(1000.0, 500.0), size(500.0, 300.0));
        g
    }
    #[test]
    fn fit_preserves_aspect_ratio_and_never_upscales() {
        let mut g = geometry();
        assert_eq!(g.scale, 0.5);
        assert_eq!(g.image_origin(), point(0.0, 25.0));
        g.resize(size(100.0, 50.0), size(500.0, 300.0));
        assert_eq!(g.scale, 1.0);
        assert_eq!(g.image_origin(), point(200.0, 125.0));
    }
    #[test]
    fn zoom_keeps_the_cursor_over_the_same_image_point() {
        let mut g = geometry();
        let anchor = point(100.0, 150.0);
        let before = (anchor - g.image_origin()) / g.scale;
        g.zoom(2.0, anchor);
        let after = (anchor - g.image_origin()) / g.scale;
        assert!((before.x - after.x).abs() < 0.001);
        assert!((before.y - after.y).abs() < 0.001);
    }
    #[test]
    fn pan_zoom_and_resize_stay_bounded() {
        let mut g = geometry();
        g.zoom(1e9, point(250.0, 150.0));
        assert_eq!(g.scale, 32.0);
        g.pan_by(point(1e9, -1e9));
        assert_eq!(g.pan, point(15750.0, -7850.0));
        g.zoom(1e-9, point(250.0, 150.0));
        assert_eq!(g.scale, 0.01);
        assert_eq!(g.pan, point(0.0, 0.0));
        g.zoom(f32::NAN, point(0.0, 0.0));
        assert_eq!(g.scale, 0.01);
        g.fit();
        g.resize(size(1000.0, 500.0), size(250.0, 200.0));
        assert_eq!(g.scale, 0.25);
        g.zoom(1.0, point(125.0, 100.0));
        g.resize(size(1000.0, 500.0), size(300.0, 200.0));
        assert_eq!(g.scale, 1.0);
    }
    #[test]
    fn wheel_requires_control_and_normalizes_lines() {
        let mut state = ViewState { geometry: geometry(), ..Default::default() };
        let mut event = gpui::ScrollEvent {
            position: point(px(250.0), px(150.0)),
            delta: ScrollDelta::Lines(point(0.0, 1.0)),
            ..Default::default()
        };
        assert!(!state.wheel(&event));
        assert_eq!(state.geometry.scale, 0.5);
        event.modifiers.control = true;
        assert!(state.wheel(&event));
        let scale = state.geometry.scale;
        assert!(scale > 0.5, "GPUI positive Y (wheel up) must zoom in");
        state.geometry.fit();
        event.delta = ScrollDelta::Pixels(point(px(0.0), px(40.0)));
        state.wheel(&event);
        assert_eq!(state.geometry.scale, scale);
        event.delta = ScrollDelta::Lines(point(0.0, -1.0));
        state.wheel(&event);
        assert!((state.geometry.scale - 0.5).abs() < 0.0001, "wheel down reverses zoom");
    }
    #[test]
    fn pinch_accumulates_native_deltas_and_a_drag_does_not_click() {
        let mut state = ViewState { geometry: geometry(), ..Default::default() };
        let mut event = gpui::PinchEvent {
            position: point(px(250.0), px(150.0)),
            phase: TouchPhase::Started,
            ..Default::default()
        };
        state.pinch(&event);
        event.phase = TouchPhase::Moved;
        event.delta = 0.2;
        state.pinch(&event);
        state.pinch(&event);
        assert!((state.geometry.scale - 0.7).abs() < 0.001);
        event.phase = TouchPhase::Ended;
        state.pinch(&event);
        assert!(state.pinch.is_none());
        state.pointer_down(point(px(200.0), px(150.0)));
        assert!(!state.pointer_move(&gpui::MouseMoveEvent {
            position: point(px(202.0), px(150.0)),
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        }));
        assert!(state.pointer_move(&gpui::MouseMoveEvent {
            position: point(px(220.0), px(150.0)),
            pressed_button: Some(MouseButton::Left),
            ..Default::default()
        }));
        assert!(state.dragged);
        state.pointer_down(point(px(200.0), px(150.0)));
        assert!(!state.dragged);
    }
}
