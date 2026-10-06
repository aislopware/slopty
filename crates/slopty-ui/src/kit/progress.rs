//! Progress, in one language wherever it shows: an upload, a file going up from the composer, a
//! transfer, an install, a program's report in its terminal.
//!
//! A bar ([`Bar`]) is a capsule `spacing.xs` tall: a quiet track (`border_subtle`) and a fill with
//! round ends, at least as wide as it is tall, so 1 % is a dot and never a sliver. It never sits on
//! an edge or a hairline: a line along an edge reads as a stray rule, as the focus line did. A ring
//! ([`ring`]) is the same language in a round slot, for a chip or a pill where a bar does not fit.
//!
//! - **A known share** glides to each new value over `Motion.settle` on the ease-out curve
//!   ([`super::Gliding`]), turning back from where it is drawn; it never steps.
//! - **An unknown one** breathes: the whole fill's opacity runs from [`BREATH_LOW`] to
//!   `alpha::STRONG` and back over `Motion.breath`, stepped on the working mark's spin clock
//!   (twelve steps a second), so it costs no frame the working mark does not draw already. A
//!   stepped opacity reads as smooth where a stepped position jumps.
//! - **Paused** turns the fill muted and keeps its figure; **failed** turns it the error tone.
//!   Neither is ever a coloured line across a tile.
//! - **Reduce Motion:** a known share lands at once, and an unknown one stands still at
//!   `alpha::STRONG`.
//! - **Nothing for the first [`SHOW_AFTER`]:** work that ends sooner never flashes a bar.
//! - **A screen reader** hears it as a progress indicator with its value ("42%"), named by the
//!   words the caller gives.

use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, PathBuilder, Pixels,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, canvas, div,
    point, px, relative,
};
use slopty_theme::{Motion, Rgb, Theme, alpha};

use super::Gliding;
use crate::colors::{hsla, hsla_alpha};

/// How far a thing has got.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Progress {
    /// This share of it, 0 to 1.
    Share(f32),
    /// Under way, how far unknown.
    Busy,
    /// Held, at this share when one is known.
    Paused(Option<f32>),
    /// Stopped by a failure, at this share when one is known.
    Failed(Option<f32>),
}

impl Progress {
    /// The share it shows, when one is known.
    #[must_use]
    pub fn share(self) -> Option<f32> {
        match self {
            Self::Share(share) => Some(share),
            Self::Busy => None,
            Self::Paused(share) | Self::Failed(share) => share,
        }
        .map(|share| share.clamp(0.0, 1.0))
    }

    /// What a screen reader hears of it, and the figure beside it: "42%", else nothing.
    #[must_use]
    pub fn figure(self) -> Option<String> {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 100")]
        self.share().map(|share| format!("{}%", (share * 100.0).round() as u32))
    }

    /// The fill's tone: the accent's fill while it goes, muted while held, the error once it
    /// failed.
    #[must_use]
    pub const fn tone(self, theme: &Theme) -> Rgb {
        let s = &theme.surfaces;
        match self {
            Self::Share(_) | Self::Busy => s.accent_fill,
            Self::Paused(_) => s.text_muted,
            Self::Failed(_) => s.error,
        }
    }
}

/// How long work goes before its progress shows: work that ends sooner never shows a bar.
pub const SHOW_AFTER: Duration = Duration::from_millis(400);

/// The least opacity of an unknown share's breath.
pub const BREATH_LOW: f32 = 0.25;

/// The quiet track every bar is drawn on: `border_subtle`.
#[must_use]
pub fn track_tone(theme: &Theme) -> Hsla {
    hsla(theme.surfaces.border_subtle)
}

/// The opacity an unknown share's fill shows `since` the spin clock started: from
/// [`BREATH_LOW`] up to `alpha::STRONG` and back over `Motion.breath`.
#[must_use]
pub fn breath_at(since: Duration) -> f32 {
    let period = Motion::DEFAULT.breath.as_nanos().max(1);
    #[expect(clippy::cast_precision_loss, reason = "a phase within one breath")]
    let phase = since.as_nanos().checked_rem(period).unwrap_or(0) as f32 / period as f32;
    let wave = (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0;
    (alpha::STRONG - BREATH_LOW).mul_add(wave, BREATH_LOW)
}

/// A progress bar: [`Progress`] under `id`, one per place it shows, taking the width it is
/// given. Its fill answers to the debug selector `<id>-fill`.
#[derive(IntoElement)]
pub struct Bar {
    id: SharedString,
    progress: Progress,
    height: Pixels,
    track: Hsla,
    tone: Rgb,
    label: Option<SharedString>,
    at_once: bool,
    still: bool,
}

impl std::fmt::Debug for Bar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bar")
            .field("id", &self.id)
            .field("progress", &self.progress)
            .finish_non_exhaustive()
    }
}

impl Bar {
    /// `progress` under `id`, `spacing.xs` tall.
    #[must_use]
    pub fn new(theme: &Theme, id: impl Into<SharedString>, progress: Progress) -> Self {
        Self {
            id: id.into(),
            progress,
            height: px(theme.spacing.xs),
            track: track_tone(theme),
            tone: progress.tone(theme),
            label: None,
            at_once: false,
            still: false,
        }
    }

    /// What a screen reader calls it ("Uploading capture.mov"), its value being the figure.
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Its fill in `tone` rather than its state's: a budget's meter in the tone its share calls
    /// for.
    #[must_use]
    pub const fn tone(mut self, tone: Rgb) -> Self {
        self.tone = tone;
        self
    }

    /// `height` tall, for a bar drawn at a zoom of its own.
    #[must_use]
    pub const fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Hold it still where nothing may move on its own (the navigator): an unknown share stands
    /// at `alpha::STRONG`, as it does under Reduce Motion.
    #[must_use]
    pub const fn still(mut self) -> Self {
        self.still = true;
        self
    }

    /// Show it from its first frame, for progress that has been under way a while already (a
    /// transfer the person opened a list of).
    #[must_use]
    pub const fn at_once(mut self) -> Self {
        self.at_once = true;
        self
    }
}

/// When a bar under one id first drew, and whether the frame that shows it is on its way.
struct Shown {
    since: Instant,
    armed: bool,
}

impl RenderOnce for Bar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, progress, height, track, tone, label, at_once, still } = self;
        let now = cx.background_executor().now();
        let key = ElementId::Name(format!("{id}-shown").into());
        let state = window.use_keyed_state(key, cx, move |_, _| Shown { since: now, armed: false });
        let waited = now.saturating_duration_since(state.read(cx).since);
        let shown = at_once || waited >= SHOW_AFTER;
        if !shown && !state.read(cx).armed {
            // The view that draws it draws again once the wait is over.
            state.update(cx, |s, _| s.armed = true);
            let view = window.current_view();
            let wait = SHOW_AFTER.saturating_sub(waited);
            window
                .spawn(cx, async move |cx| {
                    cx.background_executor().timer(wait).await;
                    let _drawn = cx.update(|_, cx| cx.notify(view));
                })
                .detach();
        }
        let selector = format!("{id}-fill");
        let capsule = move || {
            let selector = selector.clone();
            div().debug_selector(move || selector).absolute().top_0().bottom_0().rounded_full()
        };
        let fill: AnyElement = match progress.share() {
            Some(share) => Gliding::new(ElementId::Name(id.clone()), share, move |share| {
                // A sliver at least as wide as it is tall, so its ends stay round.
                capsule()
                    .left_0()
                    .when(share > 0.0, |el| el.min_w(height))
                    .w(relative(share))
                    .bg(hsla(tone))
                    .into_any_element()
            })
            .fill()
            .into_any_element(),
            None if still => {
                capsule().left_0().right_0().bg(hsla_alpha(tone, alpha::STRONG)).into_any_element()
            }
            None => Breathing { fill: Some(capsule().left_0().right_0()), tone, inner: None }
                .into_any_element(),
        };
        let figure = progress.figure();
        div()
            .id(ElementId::Name(id))
            .role(Role::ProgressIndicator)
            .when_some(label, gpui::StatefulInteractiveElement::aria_label)
            .when_some(progress.share(), |el, share| {
                el.aria_numeric_value(f64::from(share) * 100.0)
                    .aria_min_numeric_value(0.0)
                    .aria_max_numeric_value(100.0)
            })
            .when_some(figure, gpui::StatefulInteractiveElement::aria_value)
            .relative()
            .w_full()
            .h(height)
            .flex_none()
            .rounded_full()
            .overflow_hidden()
            .when(!shown, gpui::Styled::invisible)
            .bg(track)
            .child(fill)
    }
}

/// An unknown share's fill: the whole track breathing on the spin clock, still at
/// `alpha::STRONG` under Reduce Motion. Toned to the clock's step as it is laid out.
struct Breathing {
    fill: Option<gpui::Div>,
    tone: Rgb,
    inner: Option<AnyElement>,
}

impl IntoElement for Breathing {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Breathing {
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
        let opacity = if cx.reduce_motion() {
            alpha::STRONG
        } else {
            breath_at(crate::icons::steps_shown(cx))
        };
        let fill = self.fill.take().unwrap_or_else(div);
        let mut inner = fill.bg(hsla_alpha(self.tone, opacity)).into_any_element();
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

/// The ring's track: `border`, so a part-full ring reads as a gauge and not a spinner.
#[must_use]
pub fn ring_track(theme: &Theme) -> Hsla {
    hsla(theme.surfaces.border)
}

/// A ring `side` square showing `share` in `tone`, under `id`: the bar wound round, the used
/// arc over its track with round ends, gliding to each new share.
#[must_use]
pub fn ring(
    theme: &Theme,
    id: impl Into<ElementId>,
    share: f32,
    tone: Rgb,
    side: Pixels,
) -> AnyElement {
    let (track, arc) = (ring_track(theme), hsla(tone));
    let share = share.clamp(0.0, 1.0);
    Gliding::new(id, share, move |share| ring_at(share, side, track, arc)).into_any_element()
}

/// The ring at `share`: its track, and the arc with round ends.
fn ring_at(share: f32, side: Pixels, track: Hsla, arc: Hsla) -> AnyElement {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds: Bounds<Pixels>, (), window, _cx| {
            let width = (bounds.size.width.min(bounds.size.height) * 0.16).max(px(1.5));
            let r = (bounds.size.width.min(bounds.size.height) - width) / 2.0;
            let c = bounds.center();
            let at = |t: f32| {
                let a = t.mul_add(std::f32::consts::TAU, -std::f32::consts::FRAC_PI_2);
                point(c.x + r * a.cos(), c.y + r * a.sin())
            };
            // Arcs of at most half a turn, so each is the short way round between its ends.
            let stroke = |from: f32, to: f32| {
                let mut path = PathBuilder::stroke(width);
                path.move_to(at(from));
                let mid = (from + 0.5).min(to);
                path.arc_to(point(r, r), px(0.0), false, true, at(mid));
                if mid < to {
                    path.arc_to(point(r, r), px(0.0), false, true, at(to));
                }
                path.build().ok()
            };
            // A round end: a disc as wide as the stroke.
            let cap = |t: f32| {
                let (p, rc) = (at(t), width / 2.0);
                let mut path = PathBuilder::fill();
                path.move_to(point(p.x + rc, p.y));
                path.arc_to(point(rc, rc), px(0.0), false, true, point(p.x - rc, p.y));
                path.arc_to(point(rc, rc), px(0.0), false, true, point(p.x + rc, p.y));
                path.close();
                path.build().ok()
            };
            if let Some(path) = stroke(0.0, 1.0) {
                window.paint_path(path, track);
            }
            if share > 0.0 {
                for path in [stroke(0.0, share), cap(0.0), cap(share)].into_iter().flatten() {
                    window.paint_path(path, arc);
                }
            }
        },
    )
    .size(side)
    .flex_none()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_theme::{Motion, Theme, alpha};

    use super::{BREATH_LOW, Progress, breath_at};

    /// A known share is its figure and a screen reader's value; an unknown one has none, and
    /// each state wears its tone.
    #[test]
    fn a_share_is_its_figure_in_its_state_s_tone() {
        let theme = Theme::default();
        let s = theme.surfaces;
        for (share, figure) in [(0.0, "0%"), (0.01, "1%"), (0.5, "50%"), (1.0, "100%")] {
            assert_eq!(Progress::Share(share).figure().as_deref(), Some(figure));
        }
        assert_eq!(Progress::Share(2.5).share(), Some(1.0), "never past the end");
        assert_eq!(Progress::Busy.figure(), None);
        assert_eq!(Progress::Paused(Some(0.4)).figure().as_deref(), Some("40%"), "kept");
        assert_eq!(Progress::Share(0.4).tone(&theme), s.accent_fill);
        assert_eq!(Progress::Busy.tone(&theme), s.accent_fill);
        assert_eq!(Progress::Paused(None).tone(&theme), s.text_muted);
        assert_eq!(Progress::Failed(Some(0.8)).tone(&theme), s.error);
    }

    /// An unknown share breathes between its floor and `alpha::STRONG` over one breath, and
    /// comes back where it began.
    #[test]
    fn an_unknown_share_breathes() {
        let breath = Motion::DEFAULT.breath;
        assert!((breath_at(Duration::ZERO) - BREATH_LOW).abs() < 1e-4, "starts low");
        assert!((breath_at(breath / 2) - alpha::STRONG).abs() < 1e-3, "peaks half way");
        assert!((breath_at(breath) - BREATH_LOW).abs() < 1e-4, "and comes back");
        let samples: Vec<f32> = (0..48).map(|i| breath_at(breath * i / 48)).collect();
        assert!(samples.iter().all(|a| (BREATH_LOW - 1e-4..=alpha::STRONG + 1e-4).contains(a)));
    }
}
