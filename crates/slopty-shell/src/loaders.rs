// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Loaders: the slopty pulse loader, the gradient matrix spinner, and the boot
//! splash content. All motion routes through `crate::motion` pure helpers, so
//! the math is unit-tested and these elements are testable-by-compile.
//!
//! Rendering pattern: cells share a self-parking pulse clock; per-cell offsets
//! come from [`motion::staggered_phase`], so all cells stay phase-locked.
//! Cells animate inside fixed-size slots — opacity and inner size
//! are paint-local and never move surrounding layout. Reduced motion snaps every
//! cell to its rest state automatically (gpui `reduce_motion`). Activity grids
//! retain a gentle brightness pulse with system reduced motion; explicit On
//! and background pause keep them still.

use gpui::{
    AnyElement, App, AppContext, Context, Entity, EntityId, InteractiveElement as _, IntoElement,
    ParentElement, PathBuilder, Render, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled, Window, canvas, div, point, px,
};

pub mod status_mark; // the one mark a run's state morphs through
pub mod copy_glyph; // a copy control's glyph, its check drawn as the copy lands

use crate::motion::{self, GRADIENT_SPIN, PULSE_STAGGER, SPLASH_OUT, SLOPTY_PULSE};
use crate::theme::{GlyphPalette, Theme};

// Shared with the terminal viewport (`slopty_view_proto::motion`) so both animate the
// same loaders from the same numbers.
pub use slopty_view_proto::motion::{
    MARK_CELLS, MARK_SPREAD, MATRIX_SIDE, SLOPTY_CELLS, mark_cell_stagger,
};

/// The animated Slopty mark: the full logo pixel grid with a light wave sweeping tail→head. Each
/// cell rests dim (opacity 0.08, scale 0.9) and flares to full as the crest passes; per-cell
/// stagger follows the flight axis. `height_px` sets the mark's height (width follows the 820:940
/// canvas).
pub fn slopty_mark_loader(
    _id: &'static str,
    theme: &Theme,
    height_px: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    let color = theme.text;
    let floor = theme.spinner_floor(PULSE_FLOOR); // a light floor that keeps its shape
    let scale = height_px / 940.0;
    let cell = 100.0 * scale;
    let delta = motion::pulse_delta(&SLOPTY_PULSE, view, cx);
    div()
        .relative()
        .w(px(820.0 * scale))
        .h(px(height_px))
        .children(MARK_CELLS.iter().map(move |&(x, y)| {
            let stagger = mark_cell_stagger(x, y);
            // Fixed slot; the animated cell breathes inside it (paint-local).
            div()
                .absolute()
                .left(px(x * scale))
                .top(px(y * scale))
                .size(px(cell))
                .flex()
                .items_center()
                .justify_center()
                .child({
                    // The stagger adds phase, so each cell starts mid-cycle.
                    let phase = (delta + stagger).rem_euclid(1.0);
                    div()
                        .rounded(px(16.0 * scale))
                        .bg(color)
                        .opacity(lift_floor(motion::pulse_opacity(phase), floor))
                        .size(px(cell * motion::pulse_scale(phase)))
                })
        }))
}

/// The slopty wave loader: a row of cells pulsing opacity 0.08→1 / scale 0.9→1
/// over 2.4s with a 0.15s stagger per cell.
///
/// `id` scopes the per-cell animation state — give each loader instance a
/// distinct id.
pub fn slopty_loader(
    _id: &'static str,
    theme: &Theme,
    cell_px: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    let color = theme.text;
    let floor = theme.spinner_floor(PULSE_FLOOR); // a light floor that keeps its shape
    let slot = cell_px;
    let delta = motion::pulse_delta(&SLOPTY_PULSE, view, cx);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(slot / 2.0))
        .children((0..SLOPTY_CELLS).map(move |i| {
            // Fixed slot; the animated cell breathes inside it.
            div()
                .size(px(slot))
                .flex()
                .items_center()
                .justify_center()
                .child({
                    let phase = motion::staggered_phase(delta, i, PULSE_STAGGER);
                    div()
                        .rounded(px(slot / 4.0))
                        .bg(color)
                        .opacity(lift_floor(motion::pulse_opacity(phase), floor))
                        .size(px(slot * motion::pulse_scale(phase)))
                })
        }))
}

pub use slopty_view_proto::motion::{GSPIN_DIM, GSPIN_ROW_TINTS};

/// The gradient matrix spinner (WorkingIndicator): a 3×3 grid of round cells tinted per row from
/// the sunrise gradient. Each cell pulses opacity once per 750ms period; the per-cell phase
/// follows the "arrow-up" pattern (the pulse enters at the bottom edge and converges toward the
/// top-center cell), so the wave reads as travelling upward.
pub fn gradient_spinner(
    _id: &'static str,
    theme: &Theme,
    cell_px: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    let center = (MATRIX_SIDE as f32 - 1.0) / 2.0;
    let max = MATRIX_SIDE as f32 - 1.0 + center;
    let pulse = motion::activity_pulse_slow(view, cx);
    // On light, each sunrise row takes the deeper tone of its hue that reads on white,
    // and the cells rest higher (`Theme::spinner_ink`, `Theme::spinner_floor`).
    let tints = gradient_tints(theme);
    let floor = theme.spinner_floor(GSPIN_DIM);
    div()
        .flex()
        .flex_col()
        .gap(px(cell_px / 2.0))
        .children((0..MATRIX_SIDE).map(move |row| {
            let tint = tints[row];
            div()
                .flex()
                .flex_row()
                .gap(px(cell_px / 2.0))
                .children((0..MATRIX_SIDE).map(move |col| {
                    // Distance of this cell from the wave origin, normalized
                    // into a phase offset (gradient-spin's `--gspin-phase`).
                    let d = MATRIX_SIDE as f32 - 1.0 - row as f32 + (col as f32 - center).abs();
                    let phase = if max == 0.0 { 0.0 } else { d / (max + 1.0) };
                    div()
                        .size(px(cell_px))
                        .rounded(px(cell_px / 2.0))
                        .bg(tint)
                        .opacity(pulse.opacity(phase, floor))
                }))
        }))
}

/// The resting opacity of the slopty wave and mark loaders (`motion::pulse_opacity`'s low end).
const PULSE_FLOOR: f32 = 0.08;

/// `opacity` from [`motion::pulse_opacity`] (0.08..1) rescaled to rest at `floor` instead.
fn lift_floor(opacity: f32, floor: f32) -> f32 {
    floor + (opacity - PULSE_FLOOR) / (1.0 - PULSE_FLOOR) * (1.0 - floor)
}

/// The gradient spinner's three row tints as `theme` draws them.
pub(crate) fn gradient_tints(theme: &Theme) -> [gpui::Hsla; MATRIX_SIDE] {
    GSPIN_ROW_TINTS.map(|rgb| theme.spinner_ink(gpui::rgb(rgb).into()))
}

/// A 2×3 activity glyph sized for compact status slots. Its color is an
/// explicit accent-preset role supplied by the caller, while brightness snakes
/// around the grid's perimeter as a tiny radial chase.
pub fn mini_glyph_spinner(
    key: impl Into<SharedString>,
    cell_px: f32,
    palette: GlyphPalette,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    mini_spinner_tinted(key, cell_px, palette.rows(), view, cx)
}

/// Grayscale variant for surfaces where an accent would pull focus (the
/// sidebar connection line): same grid, snake, and timing, color left to the
/// caller.
pub fn mini_mono_spinner(
    key: impl Into<SharedString>,
    cell_px: f32,
    tint: gpui::Hsla,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    mini_spinner_tinted(key, cell_px, [tint; 3], view, cx)
}

fn mini_spinner_tinted(
    key: impl Into<SharedString>,
    cell_px: f32,
    row_tints: [gpui::Hsla; 3],
    _view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    // Legible on light (`Theme::spinner_ink`), its cells resting higher there; as given
    // where no theme is installed.
    let theme = cx.try_global::<Theme>();
    MiniSpinner {
        key: key.into(),
        cell_px,
        row_tints: row_tints.map(|tint| theme.map_or(tint, |theme| theme.spinner_ink(tint))),
        floor: theme.map_or(GSPIN_DIM, |theme| theme.spinner_floor(GSPIN_DIM)),
    }
}

#[derive(IntoElement)]
struct MiniSpinner {
    key: SharedString,
    cell_px: f32,
    row_tints: [gpui::Hsla; 3],
    floor: f32,
}

impl RenderOnce for MiniSpinner {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // Keep pulse invalidation separate from container state changes so
        // cached sibling rows can be reused while these six cells animate.
        let view = window.with_global_id(self.key.into(), |id, window| {
            window.with_element_state(id, |previous: Option<Entity<MiniSpinnerView>>, _| {
                let view = previous.unwrap_or_else(|| {
                    cx.new(|_| MiniSpinnerView {
                        cell_px: self.cell_px,
                        row_tints: self.row_tints,
                        floor: self.floor,
                    })
                });
                view.update(cx, |view, cx| {
                    if view.cell_px != self.cell_px
                        || view.row_tints != self.row_tints
                        || view.floor != self.floor
                    {
                        view.cell_px = self.cell_px;
                        view.row_tints = self.row_tints;
                        view.floor = self.floor;
                        cx.notify();
                    }
                });
                (view.clone(), view)
            })
        });
        view.cached(
            gpui::StyleRefinement::default()
                .w(px(self.cell_px * 2.5))
                .h(px(self.cell_px * 4.0)),
        )
    }
}

struct MiniSpinnerView {
    cell_px: f32,
    row_tints: [gpui::Hsla; 3],
    floor: f32,
}

impl Render for MiniSpinnerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        mini_spinner_cells(self.cell_px, self.row_tints, self.floor, cx.entity_id(), cx)
    }
}

fn mini_spinner_cells(
    cell_px: f32,
    row_tints: [gpui::Hsla; 3],
    floor: f32,
    view: EntityId,
    cx: &mut App,
) -> impl IntoElement {
    const COLS: usize = 2;
    const ROWS: usize = 3;
    /// Clockwise ring position of each `(row, col)` cell, top-left first:
    /// (0,0) → (0,1) → (1,1) → (2,1) → (2,0) → (1,0).
    const RING: [[usize; COLS]; ROWS] = [[0, 1], [5, 2], [4, 3]];
    const RING_LEN: f32 = (COLS * ROWS) as f32;
    let pulse = motion::activity_pulse(view, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(cell_px / 2.0))
        .children((0..ROWS).map(move |row| {
            let tint = row_tints[row];
            div()
                .flex()
                .flex_row()
                .gap(px(cell_px / 2.0))
                .children((0..COLS).map(move |col| {
                    let phase = RING[row][col] as f32 / RING_LEN;
                    div()
                        .size(px(cell_px))
                        .rounded(px(cell_px / 2.0))
                        .bg(tint)
                        .opacity(pulse.opacity(phase, floor))
                }))
        }))
}

/// A rounded fill that breathes on the slopty pulse clock between `floor` and
/// `peak` opacity, filling whatever positioned box it is placed in. Like [`pulse_dot`] it repaints from its own cached
/// view, so the surrounding chrome stays still.
pub fn pulse_glow(
    key: impl Into<SharedString>,
    radius: f32,
    tint: gpui::Hsla,
    floor: f32,
    peak: f32,
) -> impl IntoElement {
    PulseGlow {
        key: key.into(),
        style: GlowStyle {
            radius,
            tint,
            floor,
            peak,
        },
    }
}

#[derive(Clone, Copy, PartialEq)]
struct GlowStyle {
    radius: f32,
    tint: gpui::Hsla,
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
        // Legible on light, as every spinner (`Theme::spinner_ink`).
        let style = match cx.try_global::<Theme>() {
            Some(theme) => GlowStyle {
                tint: theme.spinner_ink(self.style.tint),
                floor: theme.spinner_floor(self.style.floor),
                ..self.style
            },
            None => self.style,
        };
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
        let GlowStyle {
            radius,
            tint,
            floor,
            peak,
        } = self.style;
        let delta = motion::pulse_delta_slow(&SLOPTY_PULSE, cx.entity_id(), cx);
        // pulse_opacity spans 0.08..1; renormalise to 0..1 before mapping.
        let wave = (motion::pulse_opacity(delta) - 0.08) / 0.92;
        div()
            .size_full()
            .rounded(px(radius))
            .bg(tint)
            .opacity(motion::lerp(floor, peak, wave))
    }
}

/// Stroke width of [`upload_progress_ring`].
const RING_STROKE: f32 = 2.5;
/// Polyline segments for a full circle — plenty for a ≤40px ring.
const RING_SEGMENTS: f32 = 64.0;

/// A faint full track and a bright arc growing clockwise from 12 o'clock,
/// in the caller's colors, for a spot too small to label.
pub fn upload_progress_arc(
    percent: u8,
    diameter: f32,
    track: gpui::Hsla,
    arc: gpui::Hsla,
) -> AnyElement {
    div()
        .relative()
        .size(px(diameter))
        .child(progress_arc(percent, diameter, track, arc))
        .into_any_element()
}

/// A faint full track plus an arc growing clockwise from 12 o'clock, filling
/// its parent.
fn progress_arc(
    percent: u8,
    diameter: f32,
    track: gpui::Hsla,
    arc: gpui::Hsla,
) -> gpui::Canvas<()> {
    let frac = f32::from(percent.min(100)) / 100.0;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = diameter / 2.0 - RING_STROKE;
            let mut paint_arc = |sweep: f32, color: gpui::Hsla| {
                if sweep <= 0.0 {
                    return;
                }
                let steps = ((RING_SEGMENTS * sweep).ceil() as usize).max(2);
                let at = |i: usize| {
                    // Clockwise from 12 o'clock.
                    let theta = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * sweep * (i as f32 / steps as f32);
                    point(
                        center.x + px(radius * theta.cos()),
                        center.y + px(radius * theta.sin()),
                    )
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

/// Full-window boot splash: the app's dot loader (the same [`gradient_spinner`]
/// the session list and the reconnecting line pulse — user request, replacing
/// the hero ascii) over the app background with a quiet status line. While
/// `fading` it plays `splash-out` (150ms hold, then 0.5s fade + 6px lift); the
/// shell removes it once [`SPLASH_OUT`] has run its course.
pub fn splash_overlay(theme: &Theme, fading: bool, view: EntityId, cx: &mut App) -> AnyElement {
    let content = div()
        .absolute()
        .inset_0()
        // Frosted glass, not the opaque page tone (user request): the boot
        // overlay reads like the rest of the chrome — the frost tint over
        // the blurred window background (opaque platforms get the surface
        // tone, since `glass()` collapses to it there).
        .bg(theme.glass())
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.0))
        // Cell 2.5 — the size every other surface runs this spinner at (the
        // "Sending…" strip, the transcript working trailer).
        .child(gradient_spinner(
            "boot-splash-spinner",
            theme,
            2.5,
            view,
            cx,
        ))
        .child(
            div()
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.text_muted.opacity(0.7))
                .child(SharedString::from("Setting up Slopty environment")),
        );
    if fading {
        motion::splash_out("boot-splash-out", content).into_any_element()
    } else {
        content.into_any_element()
    }
}

// Compile-time proof the specs referenced here stay wired to the catalog.
const _: () = {
    assert!(SPLASH_OUT.delay_ms == 150);
    assert!(SLOPTY_PULSE.duration_ms == 2400);
    assert!(GRADIENT_SPIN.duration_ms == 750);
};

// One loading voice for every surface (`.research/micro-details-2026-10-16.md` M30).

/// How long a load runs before it says so: a fast one never flashes "Loading…" (HextaUI's
/// `useDelayedLoading`, 150 ms; design-inspiration item 10).
pub const PENDING_GRACE: std::time::Duration = std::time::Duration::from_millis(150);
/// How long a loader, once shown, stays: it never blinks on and straight off.
pub const PENDING_HOLD: std::time::Duration = std::time::Duration::from_millis(400);

/// "Loading file…" and its like: the mono spinner and muted words, shown only once the load has
/// run [`PENDING_GRACE`]. `key` names the load, so a new one waits its own grace.
pub fn pending(key: impl Into<SharedString>, text: impl Into<SharedString>) -> Pending {
    Pending {
        key: key.into(),
        text: text.into(),
        wait: true,
    }
}

#[derive(IntoElement)]
pub struct Pending {
    key: SharedString,
    text: SharedString,
    wait: bool,
}

impl Pending {
    /// Shown at once: [`delayed`] has waited already.
    #[must_use]
    pub fn at_once(mut self) -> Self {
        self.wait = false;
        self
    }
}

// A loader that never flashes (HextaUI's `useDelayedLoading`; design-inspiration item 10):
// it waits [`PENDING_GRACE`] before it shows, and once shown stays [`PENDING_HOLD`] even if the
// load lands meanwhile, so it never blinks on and off.
/// Where a load stands for its place: [`delayed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delayed {
    /// Loading, too briefly to say so: the place stays blank.
    Waiting,
    /// The loader shows ([`Pending::at_once`]), the load running or its loader's hold not over.
    Shown,
    /// Loaded: the content shows.
    Done,
}

/// Whether the place `key` names shows its loader while `loading`: blank for the first
/// [`PENDING_GRACE`], then the loader, held at least [`PENDING_HOLD`] once shown. Ask it every
/// time the place draws, loading or not, so it knows when its load ends.
pub fn delayed(key: impl Into<SharedString>, loading: bool, window: &mut Window, cx: &mut App) -> Delayed {
    let key: SharedString = key.into();
    let state = window.use_keyed_state(
        gpui::ElementId::Name(format!("delayed-{key}").into()),
        cx,
        |_, _: &mut Context<DelayedLoad>| DelayedLoad::default(),
    );
    state.update(cx, |load, cx| {
        if load.shown && (!load.held || loading) {
            return Delayed::Shown;
        }
        if !loading {
            *load = DelayedLoad::default();
            return Delayed::Done;
        }
        if load.timer.is_none() {
            load.timer = Some(cx.spawn(async move |load, cx| {
                cx.background_executor().timer(PENDING_GRACE).await;
                let _ = load.update(cx, |load, cx| {
                    load.shown = true;
                    cx.notify();
                });
                cx.background_executor().timer(PENDING_HOLD).await;
                let _ = load.update(cx, |load, cx| {
                    load.held = true;
                    cx.notify();
                });
            }));
        }
        Delayed::Waiting
    })
}

/// A [`delayed`] place's load: whether its loader has shown and has been held long enough.
#[derive(Default)]
struct DelayedLoad {
    shown: bool,
    held: bool,
    timer: Option<gpui::Task<()>>,
}

/// Whether a load's grace has passed: a mark its timer sets, never the clock read while drawing.
struct PendingGrace {
    over: bool,
}

impl RenderOnce for Pending {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let wait = self.wait;
        let grace = window.use_keyed_state(
            gpui::ElementId::Name(format!("pending-{}", self.key).into()),
            cx,
            |_, cx: &mut Context<PendingGrace>| {
                if !wait {
                    return PendingGrace { over: true };
                }
                cx.spawn(async move |grace, cx| {
                    cx.background_executor().timer(PENDING_GRACE).await;
                    let _ = grace.update(cx, |grace, cx| {
                        grace.over = true;
                        cx.notify();
                    });
                })
                .detach();
                PendingGrace { over: false }
            },
        );
        let row = div()
            .id(gpui::ElementId::Name(format!("pending-row-{}", self.key).into()))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .min_h(px(20.0));
        if !grace.read(cx).over {
            return row;
        }
        // The theme as a live surface reads it, so a window with none set still draws.
        let theme = crate::live::chrome::theme(cx).clone();
        let view = window.current_view();
        row.role(gpui::Role::Status)
            .aria_label(self.text.clone())
            .child(mini_mono_spinner(self.key.clone(), 2.0, theme.text_muted, view, cx))
            .child(
                div()
                    .debug_selector(|| "pending-words".into())
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child(self.text),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// What the window last painted, sorted.
    fn painted(window: &Window) -> Vec<String> {
        let mut lines = window.painted_primitives();
        lines.sort_unstable();
        lines
    }

    /// What the window paints drawn from scratch at the moment the last frame showed.
    fn from_scratch(window: &mut Window, cx: &mut App) -> Vec<String> {
        window.repeat_frame_time();
        window.refresh();
        window.draw(cx).clear(cx);
        painted(window)
    }

    /// A spinner in a host that is drawn again for its own reasons.
    struct Host;

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(mini_mono_spinner("spin", 4.0, gpui::black(), cx.entity_id(), cx))
        }
    }

    /// The spinner is notified on the pulse's ticks and drawn from its last paint in
    /// between: a frame between ticks, and one drawn again from scratch then, show it as its tick
    /// drew it, and the next tick moves it on.
    #[gpui::test]
    fn a_spinner_drawn_between_ticks_shows_its_ticks_phase(cx: &mut gpui::TestAppContext) {
        let (host, cx) = cx.add_window_view(|_, _| Host);
        cx.run_until_parked();
        let first = cx.update(|window, _| painted(window));
        cx.executor().advance_clock(Duration::from_millis(20));
        host.update(cx, |_, cx| cx.notify());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let shown = cx.update(|window, _| painted(window));
        assert_eq!(shown, first, "drawn between ticks, the spinner is as its tick drew it");
        assert_eq!(cx.update(from_scratch), shown, "and so is a frame from scratch then");
        cx.executor().advance_clock(Duration::from_millis(200));
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let moved = cx.update(|window, _| painted(window));
        assert_ne!(moved, first, "the ticks move it on");
        assert_eq!(cx.update(from_scratch), moved, "a frame from scratch after the ticks");
    }

    #[test]
    fn mark_stagger_follows_flight_axis() {
        // Tail tip (720, 0) leads: near-maximal stagger (starts deepest into
        // the cycle); head (0, 840) trails with stagger 0.
        let tail = mark_cell_stagger(720.0, 0.0);
        let head = mark_cell_stagger(0.0, 840.0);
        assert!(tail > head, "tail {tail} should lead head {head}");
        assert!((head - 0.0).abs() < 1e-6, "head stagger ≈ 0, got {head}");
        assert!(tail <= MARK_SPREAD + 1e-6, "stagger capped at SPREAD");
        // Every logo cell stays inside [0, SPREAD].
        for &(x, y) in &MARK_CELLS {
            let s = mark_cell_stagger(x, y);
            assert!(
                (0.0..=MARK_SPREAD + 1e-6).contains(&s),
                "cell ({x},{y}) stagger {s}"
            );
        }
    }

    struct Loading;

    impl Render for Loading {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(200.0)).child(pending("file", "Loading file…"))
        }
    }

    /// A place whose load the test starts and ends.
    struct Place {
        loading: bool,
    }

    impl Render for Place {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let stands = delayed("place", self.loading, window, cx);
            div().child(div().debug_selector(move || format!("{stands:?}")).size(px(4.0)))
        }
    }

    /// A delayed loader stays blank through a quick load, shows once the load passes its grace,
    /// and once shown stays its hold though the load lands sooner.
    #[gpui::test]
    fn a_delayed_loader_never_flashes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (place, cx) = cx.add_window_view(|_, _| Place { loading: true });
        let at = |cx: &mut gpui::VisualTestContext, ms: u64, loading: bool| {
            cx.executor().advance_clock(Duration::from_millis(ms));
            cx.run_until_parked();
            place.update(cx, |place, cx| {
                place.loading = loading;
                cx.notify();
            });
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            ["Waiting", "Shown", "Done"]
                .into_iter()
                .find(|stands| cx.debug_bounds(stands).is_some())
                .expect("drawn")
        };
        assert_eq!(at(cx, 0, true), "Waiting");
        assert_eq!(at(cx, 100, false), "Done", "a quick load never shows its loader");
        assert_eq!(at(cx, 1000, false), "Done");
        assert_eq!(at(cx, 0, true), "Waiting");
        assert_eq!(at(cx, 150, true), "Shown");
        assert_eq!(at(cx, 50, false), "Shown", "held, though loaded");
        assert_eq!(at(cx, 300, false), "Shown");
        assert_eq!(at(cx, 60, false), "Done", "let go once held 400 ms");
    }

    #[gpui::test]
    fn a_load_says_so_only_after_its_grace(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (_view, cx) = cx.add_window_view(|_, _| Loading);
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        draw(cx);
        assert!(cx.debug_bounds("pending-words").is_none(), "a fast load never flashes");
        cx.executor().advance_clock(PENDING_GRACE);
        cx.run_until_parked();
        draw(cx);
        assert!(cx.debug_bounds("pending-words").is_some());
    }
}
