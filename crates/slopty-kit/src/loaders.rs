// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Loaders: the mark loader, the 3×3 working spinner, the 2×3 mini spinner, a breathing glow,
//! the upload ring and the boot splash.
//!
//! Cells share one self-parking pulse clock ([`motion::pulse_delta`]), so every loader stays in
//! phase; each animates inside a fixed slot, its opacity and size paint-local, so nothing
//! around it moves. Under Reduce Motion they hold still. Slopty's mark is the 3×3 dot family
//! (`docs/decisions/brand.md`), so zeron's pixel-bird loader is the same grid here.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EntityId, Hsla, PathBuilder, Render, SharedString, Window,
    canvas, div, point, px, rgb,
};

use crate::motion::{self, GRADIENT_SPIN, PULSE_STAGGER, SPLASH_OUT, ZERON_PULSE};
pub use crate::motion::{GSPIN_DIM, GSPIN_ROW_TINTS, MATRIX_SIDE};
use crate::theme::{GlyphPalette, Theme};

/// Slopty's mark as a loader: the 3×3 grid in the text tone, each dot resting dim (0.08, 0.9 of
/// its slot) and flaring as a wave crosses the grid from its bottom-left corner. `side` is the
/// mark's side.
pub fn mark_loader(theme: &Theme, side: f32, view: EntityId, cx: &mut App) -> impl IntoElement {
    let color = theme.text;
    let slot = side / 5.0;
    let delta = motion::pulse_delta(&ZERON_PULSE, view, cx);
    div().flex().flex_col().gap(px(slot)).children((0..MATRIX_SIDE).map(move |row| {
        div().flex().flex_row().gap(px(slot)).children((0..MATRIX_SIDE).map(move |col| {
            let index = MATRIX_SIDE.saturating_sub(1).saturating_sub(row).saturating_add(col);
            let phase = motion::staggered_phase(delta, index, PULSE_STAGGER * 2.0);
            div().size(px(slot)).flex().items_center().justify_center().child(
                div()
                    .rounded_full()
                    .bg(color)
                    .opacity(motion::pulse_opacity(phase))
                    .size(px(slot * motion::pulse_scale(phase))),
            )
        }))
    }))
}

/// The row loader: five cells pulsing from 0.08 to full over 2.4 s, 0.15 s apart.
pub fn row_loader(theme: &Theme, cell_px: f32, view: EntityId, cx: &mut App) -> impl IntoElement {
    const CELLS: usize = 5;
    let color = theme.text;
    let delta = motion::pulse_delta(&ZERON_PULSE, view, cx);
    div().flex().flex_row().items_center().gap(px(cell_px / 2.0)).children((0..CELLS).map(
        move |i| {
            let phase = motion::staggered_phase(delta, i, PULSE_STAGGER);
            div().size(px(cell_px)).flex().items_center().justify_center().child(
                div()
                    .rounded(px(cell_px / 4.0))
                    .bg(color)
                    .opacity(motion::pulse_opacity(phase))
                    .size(px(cell_px * motion::pulse_scale(phase))),
            )
        },
    ))
}

/// The working spinner: a 3×3 grid of round cells tinted by row from the sunrise gradient,
/// each pulsing once every 750 ms, the wave rising from the bottom edge to the top centre.
pub fn gradient_spinner(cell_px: f32, view: EntityId, cx: &mut App) -> impl IntoElement {
    let pulse = motion::activity_pulse_slow(view, cx);
    div().flex().flex_col().gap(px(cell_px / 2.0)).children(
        GSPIN_ROW_TINTS.into_iter().enumerate().map(move |(row, tint)| {
            let tint: Hsla = rgb(tint).into();
            div().flex().flex_row().gap(px(cell_px / 2.0)).children((0..MATRIX_SIDE).map(
                move |col| {
                    div()
                        .size(px(cell_px))
                        .rounded(px(cell_px / 2.0))
                        .bg(tint)
                        .opacity(pulse.opacity(motion::gspin_cell_phase(row, col), GSPIN_DIM))
                },
            ))
        }),
    )
}

/// A 2×3 activity glyph for compact status slots, in the accent's glyph rows, brightness
/// chasing around its ring.
pub fn mini_glyph_spinner(
    key: impl Into<SharedString>,
    cell_px: f32,
    palette: GlyphPalette,
) -> impl IntoElement {
    MiniSpinner { key: key.into(), cell_px, row_tints: palette.rows() }
}

/// [`mini_glyph_spinner`] in one tone, where an accent would pull focus.
pub fn mini_mono_spinner(
    key: impl Into<SharedString>,
    cell_px: f32,
    tint: Hsla,
) -> impl IntoElement {
    MiniSpinner { key: key.into(), cell_px, row_tints: [tint; 3] }
}

/// The mini spinner, drawn from a cached view of its own so the rows around it stay still while
/// its cells animate.
#[derive(IntoElement)]
struct MiniSpinner {
    key: SharedString,
    cell_px: f32,
    row_tints: [Hsla; 3],
}

impl RenderOnce for MiniSpinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (cell_px, row_tints) = (self.cell_px, self.row_tints);
        let view = window.with_global_id(self.key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<Entity<MiniSpinnerView>>, _| {
                let view =
                    previous.unwrap_or_else(|| cx.new(|_| MiniSpinnerView { cell_px, row_tints }));
                view.update(cx, |view, cx| {
                    if (view.cell_px - cell_px).abs() > f32::EPSILON || view.row_tints != row_tints
                    {
                        view.cell_px = cell_px;
                        view.row_tints = row_tints;
                        cx.notify();
                    }
                });
                (view.clone(), view)
            })
        });
        view.cached(gpui::StyleRefinement::default().w(px(cell_px * 2.5)).h(px(cell_px * 4.0)))
    }
}

struct MiniSpinnerView {
    cell_px: f32,
    row_tints: [Hsla; 3],
}

impl Render for MiniSpinnerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cell_px = self.cell_px;
        let pulse = motion::activity_pulse(cx.entity_id(), cx);
        div().flex().flex_col().gap(px(cell_px / 2.0)).children(
            self.row_tints.into_iter().zip(motion::MINI_RING).map(move |(tint, ring)| {
                div().flex().flex_row().gap(px(cell_px / 2.0)).children(ring.into_iter().map(
                    move |at| {
                        #[expect(clippy::cast_precision_loss, reason = "a ring position, 0 to 5")]
                        let phase = at as f32 / motion::MINI_RING_LEN;
                        div()
                            .size(px(cell_px))
                            .rounded(px(cell_px / 2.0))
                            .bg(tint)
                            .opacity(pulse.opacity(phase, GSPIN_DIM))
                    },
                ))
            }),
        )
    }
}

/// A rounded fill breathing between `floor` and `peak` opacity on the pulse clock, filling its
/// positioned box; it repaints from a cached view of its own.
pub fn pulse_glow(
    key: impl Into<SharedString>,
    radius: f32,
    tint: Hsla,
    floor: f32,
    peak: f32,
) -> impl IntoElement {
    PulseGlow { key: key.into(), style: GlowStyle { radius, tint, floor, peak } }
}

#[derive(Clone, Copy, PartialEq)]
struct GlowStyle {
    radius: f32,
    tint: Hsla,
    floor: f32,
    peak: f32,
}

#[derive(IntoElement)]
struct PulseGlow {
    key: SharedString,
    style: GlowStyle,
}

impl RenderOnce for PulseGlow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.style;
        let view = window.with_global_id(self.key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<Entity<PulseGlowView>>, _| {
                let view = previous.unwrap_or_else(|| cx.new(|_| PulseGlowView { style }));
                view.update(cx, |view, cx| {
                    if view.style != style {
                        view.style = style;
                        cx.notify();
                    }
                });
                (view.clone(), view)
            })
        });
        view.cached(gpui::StyleRefinement::default().size_full())
    }
}

struct PulseGlowView {
    style: GlowStyle,
}

impl Render for PulseGlowView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let GlowStyle { radius, tint, floor, peak } = self.style;
        let delta = motion::pulse_delta_slow(&ZERON_PULSE, cx.entity_id(), cx);
        let wave = (motion::pulse_opacity(delta) - motion::PULSE_MIN_OPACITY)
            / (1.0 - motion::PULSE_MIN_OPACITY);
        div().size_full().rounded(px(radius)).bg(tint).opacity(motion::lerp(floor, peak, wave))
    }
}

/// The upload ring's stroke.
const RING_STROKE: f32 = 2.5;
/// Segments of a whole circle.
const RING_SEGMENTS: f32 = 64.0;

/// The upload ring with its percent in the middle, over a sending attachment's thumbnail: a
/// faint track and an arc growing clockwise from twelve, white on the dimmed image.
pub fn upload_progress_ring(percent: u8, diameter: f32) -> AnyElement {
    let ring = progress_arc(
        percent,
        diameter,
        gpui::hsla(0.0, 0.0, 1.0, 0.22),
        gpui::hsla(0.0, 0.0, 1.0, 0.95),
    );
    div()
        .relative()
        .size(px(diameter))
        .flex()
        .items_center()
        .justify_center()
        .child(ring)
        .child(
            div()
                .text_size(px(9.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(gpui::hsla(0.0, 0.0, 1.0, 0.95))
                .child(SharedString::from(format!("{percent}%"))),
        )
        .into_any_element()
}

/// The ring without its percent, in the caller's colours, for a spot too small to label.
pub fn upload_progress_arc(percent: u8, diameter: f32, track: Hsla, arc: Hsla) -> AnyElement {
    div()
        .relative()
        .size(px(diameter))
        .child(progress_arc(percent, diameter, track, arc))
        .into_any_element()
}

fn progress_arc(percent: u8, diameter: f32, track: Hsla, arc: Hsla) -> gpui::Canvas<()> {
    let frac = f32::from(percent.min(100)) / 100.0;
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let center = bounds.center();
            let radius = diameter / 2.0 - RING_STROKE;
            let mut paint_arc = |sweep: f32, color: Hsla| {
                if sweep <= 0.0 {
                    return;
                }
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≤ 64")]
                let steps = ((RING_SEGMENTS * sweep).ceil() as u16).max(2);
                let at = |i: u16| {
                    let theta = (std::f32::consts::TAU * sweep)
                        .mul_add(f32::from(i) / f32::from(steps), -std::f32::consts::FRAC_PI_2);
                    point(center.x + px(radius * theta.cos()), center.y + px(radius * theta.sin()))
                };
                let mut builder = PathBuilder::stroke(px(RING_STROKE));
                builder.move_to(at(0));
                for i in 1..=steps {
                    builder.line_to(at(i));
                }
                if let Ok(path) = builder.build() {
                    window.paint_path(path, color);
                }
            };
            paint_arc(1.0, track);
            paint_arc(frac, arc);
        },
    )
    .absolute()
    .inset_0()
}

/// The boot splash: the working spinner over the window plane with a quiet line. While `fading`
/// it plays `splash-out`; the shell drops it once [`SPLASH_OUT`] has run.
pub fn splash_overlay(
    theme: &Theme,
    label: impl Into<SharedString>,
    fading: bool,
    view: EntityId,
    cx: &mut App,
) -> AnyElement {
    let content = div()
        .absolute()
        .inset_0()
        .bg(theme.glass())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        .child(gradient_spinner(2.5, view, cx))
        .child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted.opacity(0.7))
                .child(label.into()),
        );
    if fading {
        motion::splash_out("boot-splash-out", content).into_any_element()
    } else {
        content.into_any_element()
    }
}

const _: () = {
    assert!(SPLASH_OUT.delay_ms == 150, "the splash holds 150 ms");
    assert!(ZERON_PULSE.duration_ms == 2400, "the pulse is 2.4 s");
    assert!(GRADIENT_SPIN.duration_ms == 750, "the spinner is 750 ms");
};
