//! The next turn's slot after the last turn, Delta's: what waits and the composer's field stand
//! right under the thread's rows while those are shorter than their room, and over the
//! composer's foot once they fill it. Nothing is left between the work and the field
//! (`.research/warp-composition-2026-10-12.md` §2.5.1).
//!
//! The slot is laid out under the rows' region, which takes the room left over, and drawn up
//! by the room the rows leave under their last row. That room is read as the slot prepaints,
//! after the rows' list has laid itself out in the same frame, so the slot never trails the
//! rows by a frame. The list's offset is read only while the rows fit, when nothing scrolls:
//! a thread that scrolls notes no read of its offset, and its scroll stays the list's alone.

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ListState, Pixels, Window, point, px,
};

/// `child`, laid out under the rows of `list`, drawn up against their last row while they
/// leave room under it.
pub(super) fn follows(list: &ListState, child: AnyElement) -> Follows {
    Follows { list: list.clone(), child }
}

/// See [`follows`].
pub(super) struct Follows {
    list: ListState,
    child: AnyElement,
}

impl IntoElement for Follows {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Follows {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let rise = room_under(&self.list);
        window.with_element_offset(point(px(0.0), -rise), |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// The room `list` leaves under its last row in its region: none while its rows fill the
/// region (it can scroll), and none before its last row is laid out.
fn room_under(list: &ListState) -> Pixels {
    if list.max_offset_for_scrollbar().y > px(0.0) {
        return px(0.0);
    }
    let Some(last) = list.item_count().checked_sub(1).and_then(|ix| list.bounds_for_item(ix))
    else {
        return px(0.0);
    };
    let region = list.viewport_bounds();
    (region.bottom() - last.bottom()).max(px(0.0)).min(region.size.height)
}
