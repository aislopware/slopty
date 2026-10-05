//! Words at work: a lit band sweeping across them on the working mark's step clock.
//!
//! gpui-kit's `ShimmerText` glides on every display refresh. A thread at work already draws
//! twelve frames a second for its mark, so the band steps with it instead and adds none of its
//! own (`docs/MEASUREMENTS.md`, "companions on the step clock").

use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, HighlightStyle,
    InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, StyledText, Window,
};
use slopty_theme::Rgb;

use crate::colors::hsla;
use crate::icons::{SPIN_STEP, SPIN_STEPS};

/// One sweep of the band: two turns of the working mark.
pub(crate) const SWEEP: Duration = SPIN_STEP.saturating_mul(2 * SPIN_STEPS);

/// The band's half-width as a share of the words, never under a character and a half.
const SPREAD: f32 = 0.3;

/// How lit, 0 to 1, character `ix` of `len` is `since` the spin clock started: a bell around
/// the band's centre, which enters before the first character and leaves past the last.
#[must_use]
pub(crate) fn glow(ix: usize, len: usize, since: Duration) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a short label's character count")]
    let (ix, len) = (ix as f32 + 0.5, len as f32);
    let half = (len * SPREAD).max(1.5);
    let period = SWEEP.as_nanos().max(1);
    #[expect(clippy::cast_precision_loss, reason = "a phase within one sweep")]
    let phase = since.as_nanos().checked_rem(period).unwrap_or(0) as f32 / period as f32;
    let centre = phase.mul_add(2.0_f32.mul_add(half, len), -half);
    let off = (ix - centre).abs() / half;
    if off >= 1.0 { 0.0 } else { f32::midpoint(1.0, (off * std::f32::consts::PI).cos()) }
}

/// `text` in `base` with a band of `peak` sweeping across it on the spin clock; plain and
/// still under Reduce Motion. Laid out as the text it is, so it wraps and inherits as text.
#[must_use]
pub fn shimmer(text: impl Into<SharedString>, base: Rgb, peak: Rgb) -> Shimmer {
    Shimmer { text: text.into(), base, peak, inner: None }
}

/// See [`shimmer`].
pub struct Shimmer {
    text: SharedString,
    base: Rgb,
    peak: Rgb,
    inner: Option<AnyElement>,
}

impl std::fmt::Debug for Shimmer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shimmer").field("text", &self.text).finish_non_exhaustive()
    }
}

impl IntoElement for Shimmer {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Shimmer {
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
        let text = StyledText::new(self.text.clone());
        let text = if cx.reduce_motion() {
            text
        } else {
            let since = crate::icons::steps_shown(cx);
            let len = self.text.chars().count();
            let lit = self.text.char_indices().enumerate().filter_map(|(ix, (at, ch))| {
                let glow = glow(ix, len, since);
                (glow > 0.0).then(|| {
                    let color = Some(hsla(self.base.mix(self.peak, glow)));
                    (
                        at..at.saturating_add(ch.len_utf8()),
                        HighlightStyle { color, ..HighlightStyle::default() },
                    )
                })
            });
            text.with_highlights(lit)
        };
        let mut inner = text.into_any_element();
        let layout = inner.request_layout(window, cx);
        self.inner = Some(inner);
        (layout, ())
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
        if let Some(inner) = &mut self.inner {
            inner.prepaint(window, cx);
        }
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
        if let Some(inner) = &mut self.inner {
            inner.paint(window, cx);
        }
        if !cx.reduce_motion() {
            crate::icons::wake_at_next_step(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_crosses_the_words_once_a_sweep_and_rests_between() {
        let lit = |since: Duration| (0..7).map(|ix| glow(ix, 7, since)).collect::<Vec<_>>();
        // It enters from before the first character: nothing lit as a sweep begins.
        assert!(lit(Duration::ZERO).iter().all(|g| *g < 0.01), "{:?}", lit(Duration::ZERO));
        // Half way, the middle is the brightest and the ends the dimmest.
        let mid = lit(SWEEP / 2);
        assert!(mid[3] > 0.9 && mid[0] < mid[3] && mid[6] < mid[3], "{mid:?}");
        // It moves left to right, one step at a time.
        let peak = |since| {
            let g = lit(since);
            (0..7).max_by(|a, b| g[*a].total_cmp(&g[*b]))
        };
        assert!(peak(SWEEP / 3) < peak(SWEEP * 2 / 3));
        // A sweep later it is where it was.
        assert_eq!(lit(SWEEP / 3 + SWEEP), lit(SWEEP / 3));
        // Every value is a share.
        assert!((0..48).all(|s| lit(SPIN_STEP * s).iter().all(|g| (0.0..=1.0).contains(g))));
    }
}
