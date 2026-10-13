//! A remote window follows the room it is shown in: made wider when its pane is, as a local
//! window would be.
//!
//! The view's body comes to rest at a size; when it rests at another, the window on the worker
//! is asked to change by the same proportion, in its native pixels, so a pane made half again as
//! wide asks for a window half again as wide ([`ScreenRequest::Resize`]). The first size the body
//! rests at is the window's own: opening a window never resizes it. A size is a rest once it has
//! held for [`SETTLE`], so a pane sliding open or a window being dragged larger asks once, at the
//! end. A display cannot be resized here; it letterboxes, or follows its own way when it is one
//! made for this device.

use std::time::Duration;

use gpui::Context;
use slopty_proto::screen::{CaptureTarget, ScreenRequest};

use super::{ScreenView, ScreenViewEvent};

/// How long the body holds a size before it counts as a rest.
pub const SETTLE: Duration = Duration::from_millis(400);

/// A change smaller than this, in points, is no change.
const STEP: f32 = 1.0;

/// Where the body rests and where it is going.
#[derive(Default)]
pub(super) struct Fit {
    /// The size the body last rested at, in points; `None` until it first rests.
    rest: Option<(f32, f32)>,
    /// The timer that makes the latest size a rest.
    settling: Option<gpui::Task<()>>,
}

/// The native size a window at `native` takes when its body goes from `from` to `to` points.
#[must_use]
pub(super) fn follow(native: (f32, f32), from: (f32, f32), to: (f32, f32)) -> (u32, u32) {
    let side = |native: f32, from: f32, to: f32| {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 1, small")]
        let out = (native * to / from.max(1.0)).round().max(1.0) as u32;
        out
    };
    (side(native.0, from.0, to.0), side(native.1, from.1, to.1))
}

impl ScreenView {
    /// The body's size in device pixels and the screen's backing scale, once it has been drawn.
    #[must_use]
    pub fn body_pixels(&self) -> Option<((u32, u32), f32)> {
        let scale = self.scale_factor;
        let side = |points: gpui::Pixels| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "≥ 1, small"
            )]
            let out = (f32::from(points) * scale).round() as u32;
            out
        };
        let pixels = (side(self.bounds.size.width), side(self.bounds.size.height));
        (pixels.0 > 0 && pixels.1 > 0 && scale > 0.0).then_some((pixels, scale))
    }

    /// The body was drawn at a new size: once it holds it for [`SETTLE`], a window takes it.
    pub(super) fn body_moved(&mut self, cx: &Context<Self>) {
        self.fit.settling = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _gone = this.update(cx, Self::body_rests);
        }));
    }

    /// The body rests at its size: the first rest is the window's own, a later one resizes it.
    /// The workspace hears it too, for a display made for this device.
    fn body_rests(&mut self, cx: &mut Context<Self>) {
        self.fit.settling = None;
        let now = (f32::from(self.bounds.size.width), f32::from(self.bounds.size.height));
        if now.0 < STEP || now.1 < STEP {
            return;
        }
        cx.emit(ScreenViewEvent::Rested);
        let Some(was) = self.fit.rest.replace(now) else { return };
        if (now.0 - was.0).abs() < STEP && (now.1 - was.1).abs() < STEP {
            return;
        }
        if !matches!(self.target, CaptureTarget::Window(_)) || self.shape.is_none() {
            return;
        }
        let (width, height) = follow(self.native, was, now);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 0, small")]
        let native = (self.native.0.round() as u32, self.native.1.round() as u32);
        if (width, height) == native {
            return;
        }
        self.send(ScreenRequest::Resize { stream: self.stream, width, height, scale: None });
    }
}

#[cfg(test)]
mod tests {
    use super::follow;

    /// A body half again as wide asks for a window half again as wide, and as tall stays as
    /// tall; a body that shrinks asks for less, never for nothing.
    #[test]
    fn a_window_takes_its_body_s_proportion() {
        assert_eq!(follow((1200.0, 800.0), (600.0, 400.0), (900.0, 400.0)), (1800, 800));
        assert_eq!(follow((1200.0, 800.0), (600.0, 400.0), (300.0, 200.0)), (600, 400));
        assert_eq!(follow((1200.0, 800.0), (600.0, 400.0), (0.1, 0.1)), (1, 1));
    }
}
