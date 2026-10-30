//! A copy control's glyph that answers a copy with a check drawn in, not swapped in
//! (HextaUI's button state; `.research/design-inspiration-2026-10-19.md` item 12). The copy glyph
//! fades over [`COPY_FADE`] while the check waits, then the check is drawn stroke by stroke over
//! 180 ms, 180 ms in, in the success green; the copy glyph comes back once the control's "Copied"
//! span ([`motion::COPIED_FOR`], 1.5 s) ends. Under reduced motion the check is drawn at once.
//! The control keeps its words and its size (only its tooltip says "Copied"), so nothing beside
//! it moves; HextaUI's word roll is left out for that.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Hsla, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window,
    canvas, div, point, px,
};

use crate::motion::{self, EASE, EASE_OUT, MotionSpec};
use crate::theme::Theme;

/// The copy glyph fading as a copy lands.
pub const COPY_FADE: MotionSpec = MotionSpec::new(180, EASE);
/// The check drawn in once the copy glyph has gone.
pub const CHECK_DRAW: MotionSpec = MotionSpec::new(180, EASE_OUT).with_delay(180);

/// The check's polyline in a 24-unit box (the icon set's `check`).
const CHECK: [(f32, f32); 3] = [(4.0, 12.0), (9.0, 17.0), (20.0, 6.0)];
/// The icon set's stroke in its 24-unit box.
const STROKE: f32 = 1.75;

/// The glyph of the copy control whose "Copied" span is `key` ([`motion::copy_feedback`]):
/// the copy glyph in `rest` at `size`, or the check being drawn.
#[derive(IntoElement)]
pub struct CopyGlyph {
    key: SharedString,
    size: f32,
    rest: Hsla,
}

impl CopyGlyph {
    pub fn new(key: impl Into<SharedString>, size: f32, rest: Hsla) -> Self {
        Self { key: key.into(), size, rest }
    }
}

/// Where a copy glyph stands `elapsed` after its copy: the copy glyph's opacity and how much of
/// the check is drawn. `still` (reduced motion) draws the check at once.
pub(crate) fn copy_frame(elapsed: Duration, still: bool) -> (f32, f32) {
    if still {
        return (0.0, 1.0);
    }
    let at = |spec: MotionSpec| {
        let total = spec.total().as_secs_f32() * motion::speed_scale();
        spec.progress(elapsed.as_secs_f32() / total)
    };
    (1.0 - at(COPY_FADE), at(CHECK_DRAW))
}

/// When a copy glyph's copy landed, as first drawn copied.
struct Landed(Option<Instant>);

impl RenderOnce for CopyGlyph {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let copied = motion::copied(&self.key, cx);
        let now = crate::clock::now(cx);
        let landed = window.use_keyed_state(
            gpui::ElementId::Name(format!("copy-glyph-{}", self.key).into()),
            cx,
            |_, _: &mut gpui::Context<Landed>| Landed(None),
        );
        let since = landed.update(cx, |landed, _| {
            if copied {
                Some(*landed.0.get_or_insert(now))
            } else {
                landed.0 = None;
                None
            }
        });
        let size = self.size;
        let icon =
            |path: &'static str| crate::icons::icon(path).size(px(size)).text_color(self.rest);
        let Some(since) = since else {
            return div().size(px(size)).flex_none().child(icon(crate::icons::COPY));
        };
        let elapsed = now.saturating_duration_since(since);
        let (copy, drawn) = copy_frame(elapsed, motion::reduced_motion(cx));
        if drawn < 1.0 {
            window.request_animation_frame();
        }
        let green = Theme::of(cx).spinner_ink(Theme::of(cx).success);
        div()
            .size(px(size))
            .flex_none()
            .relative()
            .when(copy > 0.0, |el| {
                el.child(div().absolute().inset_0().opacity(copy).child(icon(crate::icons::COPY)))
            })
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, _| {
                        let unit = f32::from(bounds.size.width) / 24.0;
                        let at = |(x, y): (f32, f32)| {
                            point(bounds.origin.x + px(x * unit), bounds.origin.y + px(y * unit))
                        };
                        let points = CHECK.map(at);
                        super::status_mark::stroke_part(
                            &points,
                            drawn,
                            STROKE * unit,
                            green,
                            window,
                        );
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The copy glyph fades first and the check is drawn after it, 180 ms in, over 180 ms; under
    /// reduced motion the check is whole at once.
    #[test]
    fn the_check_is_drawn_after_the_copy_glyph_fades() {
        let ms = Duration::from_millis;
        assert_eq!(copy_frame(ms(0), false), (1.0, 0.0));
        let (copy, drawn) = copy_frame(ms(120), false);
        assert!(copy < 0.5 && drawn == 0.0, "{copy} {drawn}");
        let (copy, drawn) = copy_frame(ms(270), false);
        assert!(copy < 1e-3 && drawn > 0.3 && drawn < 1.0, "{copy} {drawn}");
        assert!(copy_frame(ms(360), false).1 > 0.999);
        assert_eq!(copy_frame(ms(0), true), (0.0, 1.0));
    }
}
