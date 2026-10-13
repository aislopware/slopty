// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! [`edge_faded`]: a child painted fading toward its own edges, per pixel, over any background,
//! glass included, where no overlay could fade it.
//!
//! Zeron's builder on gpui-fast's [`EdgeFade`]: each edge's band is its width, an edge that is
//! off has none, and the region is the wrapper's bounds moved by [`EdgeFaded::inset_top`] and
//! [`EdgeFaded::outset_bottom`] (content past an edge of the region fades on to nothing, so
//! content under the titlebar vanishes before it can meet the titlebar's text). gpui-fast nests
//! fades edge by edge, so a label's fade keeps its list's.
//!
//! The gates read the scroll handles at prepaint, before the child lays out, and the same fade
//! paints: a fade follows a scroll a frame behind at most.

use gpui::{
    AnyElement, App, Bounds, EdgeFade, Edges, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollHandle, Window, px,
};

/// `child` fading at its own edges: `top` and `bottom` enable those edges, `band` is the ramp's
/// height. The horizontal edges are [`EdgeFaded::fade_left`] and [`EdgeFaded::fade_right`].
pub fn edge_faded(band: f32, top: bool, bottom: bool, child: impl IntoElement) -> EdgeFaded {
    EdgeFaded {
        band,
        band_top: None,
        band_bottom: None,
        inset_top: 0.0,
        outset_bottom: 0.0,
        top,
        bottom,
        left: false,
        right: false,
        scroll_y: None,
        overflow_y: None,
        scroll_x: None,
        smooth_overflow_x: false,
        grow_x: false,
        child: child.into_any_element(),
    }
}

/// Whether content lies past the top and the bottom.
type Overflow = dyn Fn(&App) -> (bool, bool);

/// A child fading at its edges ([`edge_faded`]).
pub struct EdgeFaded {
    band: f32,
    band_top: Option<f32>,
    band_bottom: Option<f32>,
    inset_top: f32,
    outset_bottom: f32,
    top: bool,
    bottom: bool,
    left: bool,
    right: bool,
    scroll_y: Option<ScrollHandle>,
    overflow_y: Option<Box<Overflow>>,
    scroll_x: Option<ScrollHandle>,
    smooth_overflow_x: bool,
    grow_x: bool,
    child: AnyElement,
}

impl std::fmt::Debug for EdgeFaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeFaded")
            .field("band", &self.band)
            .field("top", &self.top)
            .field("bottom", &self.bottom)
            .field("left", &self.left)
            .field("right", &self.right)
            .finish_non_exhaustive()
    }
}

impl EdgeFaded {
    /// Fade the left edge.
    #[must_use]
    pub const fn fade_left(mut self, on: bool) -> Self {
        self.left = on;
        self
    }

    /// Fade the right edge.
    #[must_use]
    pub const fn fade_right(mut self, on: bool) -> Self {
        self.right = on;
        self
    }

    /// The top edge's own band: content fades across chrome of different heights, a short
    /// titlebar above and a tall composer below.
    #[must_use]
    pub const fn band_top(mut self, px: f32) -> Self {
        self.band_top = Some(px);
        self
    }

    /// The bottom edge's own band.
    #[must_use]
    pub const fn band_bottom(mut self, px: f32) -> Self {
        self.band_bottom = Some(px);
        self
    }

    /// Gate the vertical edges on `handle`'s overflow: an edge fades only while content lies
    /// past it.
    #[must_use]
    pub fn fade_overflow_y(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_y = Some(handle.clone());
        self
    }

    /// Gate the vertical edges on a scrollable element's own overflow, `(top, bottom)`.
    #[must_use]
    pub fn fade_overflow_y_with(
        mut self,
        overflow: impl Fn(&App) -> (bool, bool) + 'static,
    ) -> Self {
        self.overflow_y = Some(Box::new(overflow));
        self
    }

    /// Gate the horizontal edges on `handle`'s overflow.
    #[must_use]
    pub fn fade_overflow_x(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_x = Some(handle.clone());
        self
    }

    /// A scrolling strip: each side's ramp as wide as the content hidden past it, up to the
    /// band, so a strip scrolled 2 px dims 2 px.
    #[must_use]
    pub fn fade_scroll_x(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_x = Some(handle.clone());
        self.grow_x = true;
        self
    }

    /// A label's right fade, easing in as it overflows from nothing to one band.
    #[must_use]
    pub fn fade_label_overflow(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_x = Some(handle.clone());
        self.smooth_overflow_x = true;
        self
    }

    /// Pull the region's top edge `px` inside the wrapper: what lies above it paints fully
    /// transparent.
    #[must_use]
    pub const fn inset_top(mut self, px: f32) -> Self {
        self.inset_top = px;
        self
    }

    /// Push the region's bottom edge `px` past the wrapper.
    #[must_use]
    pub const fn outset_bottom(mut self, px: f32) -> Self {
        self.outset_bottom = px;
        self
    }

    /// The region and the fade, from the gates as they stand.
    fn resolve(&self, bounds: Bounds<Pixels>, cx: &App) -> Option<(Bounds<Pixels>, EdgeFade)> {
        let (mut top, mut bottom) = (self.top, self.bottom);
        if let Some(scroll) = &self.scroll_y {
            let scrolled = -f32::from(scroll.offset().y);
            let max_scroll = f32::from(scroll.max_offset().y);
            top &= scrolled > 1.0;
            bottom &= scrolled < max_scroll - 1.0;
        }
        if let Some(overflow) = &self.overflow_y {
            let (overflow_top, overflow_bottom) = overflow(cx);
            top &= overflow_top;
            bottom &= overflow_bottom;
        }
        let (mut left, mut right) = (self.left, self.right);
        let mut outset_right = 0.0;
        let (mut band_left, mut band_right) = (self.band, self.band);
        if let Some(scroll) = &self.scroll_x {
            let scrolled = -f32::from(scroll.offset().x);
            let max_scroll = f32::from(scroll.max_offset().x);
            if self.grow_x {
                let hidden_left = scrolled.max(0.0);
                let hidden_right = (max_scroll - hidden_left).max(0.0);
                left &= hidden_left > 0.5;
                right &= hidden_right > 0.5;
                band_left = hidden_left.min(self.band);
                band_right = hidden_right.min(self.band);
            } else {
                left &= scrolled > 1.0;
                if self.smooth_overflow_x {
                    let overflow = (max_scroll - scrolled).max(0.0);
                    right &= overflow > 0.0;
                    outset_right = label_fade_outset(overflow, self.band);
                } else {
                    right &= scrolled < max_scroll - 1.0;
                }
            }
        }
        if !(top || bottom || left || right) {
            return None;
        }
        let width = |on: bool, band: f32| if on { px(band) } else { Pixels::ZERO };
        let fade = EdgeFade::new(Edges {
            top: width(top, self.band_top.unwrap_or(self.band)),
            right: width(right, band_right),
            bottom: width(bottom, self.band_bottom.unwrap_or(self.band)),
            left: width(left, band_left),
        });
        let mut region = bounds;
        region.size.width += px(outset_right);
        let inset = px(self.inset_top).min(region.size.height);
        region.origin.y += inset;
        region.size.height =
            (region.size.height - inset + px(self.outset_bottom)).max(Pixels::ZERO);
        Some((region, fade))
    }
}

/// How far a label's region reaches past it while it overflows by less than a band, so its
/// last characters dim gradually rather than all at once.
fn label_fade_outset(overflow: f32, band: f32) -> f32 {
    let band = band.max(1.0);
    let progress = (overflow / band).clamp(0.0, 1.0);
    let eased = progress * progress * 2.0_f32.mul_add(-progress, 3.0);
    band * (1.0 - eased)
}

impl Element for EdgeFaded {
    type PrepaintState = Option<(Bounds<Pixels>, EdgeFade)>;
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
    ) -> Self::PrepaintState {
        let fade = self.resolve(bounds, cx);
        match fade {
            Some((region, edges)) => {
                window.with_edge_fade(region, edges, |window| self.child.prepaint(window, cx));
            }
            None => {
                self.child.prepaint(window, cx);
            }
        }
        fade
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        match *prepaint {
            Some((region, edges)) => {
                window.with_edge_fade(region, edges, |window| self.child.paint(window, cx));
            }
            None => self.child.paint(window, cx),
        }
    }
}

impl IntoElement for EdgeFaded {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_fade_eases_in_with_its_overflow() {
        assert!((label_fade_outset(0.0, 20.0) - 20.0).abs() < 1e-4, "none yet: all outside");
        assert!(label_fade_outset(20.0, 20.0).abs() < 1e-4, "a whole band: all inside");
        let half = label_fade_outset(10.0, 20.0);
        assert!(half > 0.0 && half < 20.0, "between: {half}");
    }
}
