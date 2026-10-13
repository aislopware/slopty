// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! [`rounded_clip`]: a child's whole subtree clipped to its own bounds with rounded corners.
//!
//! Quads, text and images are cut per pixel along the arcs, with an antialiased edge
//! ([`Window::with_rounded_clip`]). It is built for content scrolling inside a rounded card over
//! glass, where no corner painted over it can hide what scrolls past the corners.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window, px,
};

/// `child` clipped to its own bounds with `radius` corners. It takes the child's layout as is,
/// so it can wrap a flex item without changing its sizing.
pub fn rounded_clip(radius: f32, child: impl IntoElement) -> RoundedClip {
    RoundedClip { radius, child: child.into_any_element() }
}

/// A child clipped to rounded corners ([`rounded_clip`]).
pub struct RoundedClip {
    radius: f32,
    child: AnyElement,
}

impl std::fmt::Debug for RoundedClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundedClip").field("radius", &self.radius).finish_non_exhaustive()
    }
}

impl Element for RoundedClip {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Retention notes the clip here: a view inside a resized clip, drawn again from last
        // frame, would otherwise keep the corners it was painted in. The clip narrows the
        // content mask too, so hitboxes past the bounds are clipped like the paint.
        window.with_rounded_clip(bounds, px(self.radius), |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_rounded_clip(bounds, px(self.radius), |window| self.child.paint(window, cx));
    }
}

impl IntoElement for RoundedClip {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
