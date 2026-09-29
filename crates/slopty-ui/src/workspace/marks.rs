//! Where the view is along the strip: a thin overlay thumb on the strip's bottom edge, shown
//! only while the strip scrolls or the pointer comes near that edge, as niri shows a column's
//! position only while it moves.
//!
//! The title bar held segments for this through four forms, and each read as a progress bar,
//! the loudest thing in the bar saying the least. Nothing here is a control: the navigator and
//! ⌘⌥← / → are the ways to reach a column out of view.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Styled as _, WeakEntity, div, px, relative,
};
use slopty_client::layout::Strip;
use slopty_theme::alpha;

use super::WorkspaceView;
use crate::colors::{hsla, hsla_alpha};
use crate::draw::Draw;
use crate::kit;

/// How long the thumb stays once the strip is still and the pointer has left the edge.
pub(super) const MARKS_HOLD: Duration = Duration::from_millis(800);

/// How near the strip's bottom edge the pointer brings the thumb, in points.
const NEAR_EDGE: f32 = 24.0;

/// The thumb's thickness, in points.
const THUMB_H: f32 = 3.0;

/// Whether every column of `strip` is in its view, so there is nowhere to show.
pub(super) fn all_in_view(strip: &Strip) -> bool {
    let (view_x, view_w) = strip.view;
    strip.columns.iter().all(|(x, w)| *x >= view_x - 1.0 && x + w <= view_x + view_w + 1.0)
}

/// Where the view sits along `strip`, as a share of the strip's whole length: where the thumb
/// starts and how long it is. `None` for a strip of one column or with every column in view.
#[must_use]
pub(super) fn thumb(strip: &Strip) -> Option<(f32, f32)> {
    if strip.columns.len() < 2 || all_in_view(strip) {
        return None;
    }
    let (view_x, view_w) = strip.view;
    let start = strip.columns.iter().map(|(x, _)| *x).fold(view_x, f32::min);
    let end = strip.columns.iter().map(|(x, w)| x + w).fold(view_x + view_w, f32::max);
    let length = end - start;
    (length > 0.0).then(|| ((view_x - start) / length, view_w / length))
}

/// Whether the thumb shows, and how it goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) enum Marks {
    /// Not drawn.
    #[default]
    Hidden,
    /// Drawn: the strip moves or the pointer is near its edge.
    Shown,
    /// Fading out, the fade named by its generation.
    Fading(u64),
}

impl WorkspaceView {
    /// Whether the thumb is drawn now.
    #[must_use]
    pub(super) fn marks_shown(&self) -> bool {
        self.drawn.marks.get() != Marks::Hidden
    }

    /// The strip moved under the view, in the frame being drawn: the thumb shows in it, and
    /// goes a while after the strip stops.
    pub(super) fn strip_scrolled(&self, cx: &Draw<'_, Self>) {
        self.drawn.marks.set(Marks::Shown);
        if !self.drawn.marks_near.get() {
            self.hold_marks(cx.weak_entity(), cx);
        }
    }

    /// The pointer moved over the strip to `y` points from its top, of `height`: near the
    /// bottom edge the thumb shows and stays; leaving the edge, it goes a while after.
    pub(super) fn pointer_over_strip(&self, y: f32, height: f32, cx: &mut Context<Self>) {
        let near = height - y <= NEAR_EDGE && thumb(&self.drawn.strip.borrow()).is_some();
        if near == self.drawn.marks_near.get() {
            return;
        }
        self.drawn.marks_near.set(near);
        if near {
            self.drawn.marks_timer.replace(None);
            self.show_marks(cx);
        } else if self.marks_shown() {
            self.hold_marks(cx.weak_entity(), cx);
        }
    }

    fn show_marks(&self, cx: &mut App) {
        if self.drawn.marks.get() != Marks::Shown {
            self.drawn.marks.set(Marks::Shown);
            self.redraw_strip(cx);
        }
    }

    /// Draw the strip again, and only the strip: the thumb is no news for the chrome.
    fn redraw_strip(&self, cx: &mut App) {
        cx.notify(self.strip_host.entity_id());
    }

    /// After [`MARKS_HOLD`] still, fade the thumb out; under Reduce Motion it goes at once.
    /// The thumb's state is the strip's, so the workspace is read, never updated, on the way.
    fn hold_marks(&self, this: WeakEntity<Self>, cx: &App) {
        let timer = cx.spawn(async move |cx| {
            cx.background_executor().timer(MARKS_HOLD).await;
            let fading = cx.update(|cx| {
                let this = this.upgrade()?;
                let view = this.read(cx);
                if view.chrome_moves(cx) {
                    let generation = view.drawn.marks_gen.get().wrapping_add(1);
                    view.drawn.marks_gen.set(generation);
                    view.drawn.marks.set(Marks::Fading(generation));
                } else {
                    view.drawn.marks.set(Marks::Hidden);
                }
                let fading = view.drawn.marks.get() != Marks::Hidden;
                let host = view.strip_host.entity_id();
                cx.notify(host);
                Some(fading)
            });
            if fading != Some(true) {
                return;
            }
            cx.background_executor().timer(kit::Pace::Fade.duration()).await;
            cx.update(|cx| {
                let Some(this) = this.upgrade() else { return };
                let view = this.read(cx);
                view.drawn.marks.set(Marks::Hidden);
                let host = view.strip_host.entity_id();
                cx.notify(host);
            });
        });
        self.drawn.marks_timer.replace(Some(timer));
    }

    /// The thumb over the strip's bottom edge, while it shows: a faint track across the strip
    /// and the view's share of it in the muted tone.
    pub(super) fn render_marks(&self) -> Option<gpui::AnyElement> {
        if !self.marks_shown() {
            return None;
        }
        let (start, length) = thumb(&self.drawn.strip.borrow())?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let radius = px(theme.radii.xs);
        let track = hsla_alpha(s.border, alpha::FAINT);
        let track = div()
            .id("strip-marks")
            .debug_selector(|| "strip-marks".to_owned())
            .absolute()
            .left(px(theme.spacing.inset()))
            .right(px(theme.spacing.inset()))
            .bottom(px(theme.spacing.sm))
            .h(px(THUMB_H))
            .rounded(radius)
            .bg(track)
            .child(
                div()
                    .debug_selector(|| "strip-thumb".to_owned())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(start.clamp(0.0, 1.0)))
                    .w(relative(length.clamp(0.0, 1.0)))
                    .rounded(radius)
                    .bg(hsla(s.text_muted)),
            );
        Some(match self.drawn.marks.get() {
            Marks::Fading(generation) => track
                .with_animation(
                    ("strip-marks-fade", generation),
                    Animation::new(kit::Pace::Fade.duration()).with_easing(kit::ease_out()),
                    |el, t| el.opacity(1.0 - t),
                )
                .into_any_element(),
            Marks::Shown | Marks::Hidden => track.into_any_element(),
        })
    }
}
