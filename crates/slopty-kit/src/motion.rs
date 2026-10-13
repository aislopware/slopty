// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Zeron's motion catalogue as helpers over GPUI's [`Animation`].
//!
//! The specs ([`FADE_IN`], [`MENU_IN`], [`DIALOG_IN`], [`RESIZE`], …) carry zeron's durations and
//! curves; [`CubicBezier`] evaluates CSS `cubic-bezier()` exactly. Hover washes fade through
//! [`hover_blend`] (150 ms, Tailwind's curve), two-state glyphs morph through [`state_t`], and
//! the repeating loaders ride one shared 30 Hz [`pulse_delta`] clock that parks when no loader
//! is mounted.
//!
//! Reduce Motion: GPUI snaps every `with_animation` element when [`App::reduce_motion`] is set,
//! the manual tweens read [`reduced_motion`], and the loaders hold still.
//!
//! A translate is a relative `top` inset, applied after layout as a CSS transform is; GPUI has
//! no scale for a `div`, so `menu-in` and `dialog-in` stand their scale in with fade and travel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub use gpui::AnimationExt;
use gpui::{
    Animation, AnimationElement, App, ElementId, EntityId, Global, Hsla, IntoElement, Rgba,
    SharedString, Styled, Window, px,
};

/// The repeating loaders' tick, about 30 frames a second.
const PULSE_TICK: Duration = Duration::from_millis(33);

/// How long a view stays on the tick list after its last loader paint.
const PULSE_LEASE: Duration = Duration::from_millis(300);

struct PulseClock {
    epoch: Instant,
    leases: HashMap<EntityId, PulseLease>,
    tick: u64,
    running: bool,
}

struct PulseLease {
    until: Instant,
    stride: u64,
}

impl PulseLease {
    fn renew(&mut self, now: Instant, stride: u64) {
        self.until = now.checked_add(PULSE_LEASE).unwrap_or(now);
        self.stride = self.stride.min(stride);
    }

    /// Whether this lease's view is due on `tick`; each paint re-establishes the fastest loader
    /// mounted, so a retired fast one does not keep a slow one fast.
    const fn take_tick(&mut self, tick: u64) -> bool {
        if self.stride == 0 || !tick.is_multiple_of(self.stride) {
            return false;
        }
        self.stride = u64::MAX;
        true
    }
}

impl Global for PulseClock {}

impl Default for PulseClock {
    fn default() -> Self {
        Self { epoch: Instant::now(), leases: HashMap::new(), tick: 0, running: false }
    }
}

/// The phase, 0 to 1, of a repeating spec.
///
/// It takes a lease that keeps `view` drawing at the pulse tick while its loader stays mounted.
/// Every loader shares one epoch, so they stay in phase.
/// Under Reduce Motion, 0 and nothing scheduled.
pub fn pulse_delta(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    pulse_delta_every(spec, view, 1, cx)
}

/// [`pulse_delta`] at 15 Hz, for the shell's coarser cell loaders.
pub fn pulse_delta_slow(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    pulse_delta_every(spec, view, 2, cx)
}

fn pulse_delta_every(spec: &MotionSpec, view: EntityId, stride: u64, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 0.0;
    }
    schedule_pulse_every(view, stride, cx);
    pulse_phase(spec, cx)
}

fn pulse_phase(spec: &MotionSpec, cx: &mut App) -> f32 {
    let clock = cx.default_global::<PulseClock>();
    let period = spec.total().as_secs_f32();
    if period <= 0.0 { 0.0 } else { (clock.epoch.elapsed().as_secs_f32() / period).fract() }
}

/// An activity grid's brightness at the current pulse phase.
#[derive(Debug, Clone, Copy)]
pub struct ActivityPulse {
    phase: f32,
    still: bool,
}

impl ActivityPulse {
    /// A cell's opacity at `cell_phase`, resting at `dim`; under Reduce Motion a steady mid
    /// tone.
    #[must_use]
    pub fn opacity(self, cell_phase: f32, dim: f32) -> f32 {
        if self.still { 0.6 } else { gspin_opacity(self.phase + cell_phase, dim) }
    }
}

/// The activity pulse for `view`, keeping it drawing while it is mounted.
pub fn activity_pulse(view: EntityId, cx: &mut App) -> ActivityPulse {
    activity_pulse_every(view, 1, cx)
}

/// [`activity_pulse`] at 15 Hz.
pub fn activity_pulse_slow(view: EntityId, cx: &mut App) -> ActivityPulse {
    activity_pulse_every(view, 2, cx)
}

fn activity_pulse_every(view: EntityId, stride: u64, cx: &mut App) -> ActivityPulse {
    if cx.reduce_motion() {
        return ActivityPulse { phase: 0.0, still: true };
    }
    schedule_pulse_every(view, stride, cx);
    ActivityPulse { phase: pulse_phase(&GRADIENT_SPIN, cx), still: false }
}

/// Keep `view` drawing on the pulse clock while a cosmetic animation is painted.
pub fn pulse_lease(view: EntityId, cx: &mut App) {
    if !cx.reduce_motion() {
        schedule_pulse_every(view, 1, cx);
    }
}

fn schedule_pulse_every(view: EntityId, stride: u64, cx: &mut App) {
    let clock = cx.default_global::<PulseClock>();
    let now = Instant::now();
    clock.leases.entry(view).or_insert(PulseLease { until: now, stride }).renew(now, stride);
    if clock.running {
        return;
    }
    clock.running = true;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PULSE_TICK).await;
            let parked = cx.update(|cx| {
                let clock = cx.default_global::<PulseClock>();
                let now = Instant::now();
                clock.leases.retain(|_, lease| lease.until > now);
                if clock.leases.is_empty() {
                    clock.running = false;
                    return true;
                }
                clock.tick = clock.tick.wrapping_add(1);
                let tick = clock.tick;
                let views: Vec<EntityId> = clock
                    .leases
                    .iter_mut()
                    .filter_map(|(view, lease)| lease.take_tick(tick).then_some(*view))
                    .collect();
                for view in views {
                    cx.notify(view);
                }
                false
            });
            if parked {
                break;
            }
        }
    })
    .detach();
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function, ends fixed at (0, 0) and (1, 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    /// The first control point's x.
    pub x1: f32,
    /// The first control point's y.
    pub y1: f32,
    /// The second control point's x.
    pub x2: f32,
    /// The second control point's y.
    pub y2: f32,
}

impl CubicBezier {
    /// The curve through these control points.
    #[must_use]
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(a: f32, b: f32) -> (f32, f32, f32) {
        let c = 3.0 * a;
        let bb = 3.0_f32.mul_add(b - a, -c);
        (1.0 - c - bb, bb, c)
    }

    fn sample(a: f32, b: f32, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(a, b);
        a.mul_add(t, b).mul_add(t, c) * t
    }

    fn sample_x_derivative(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        (3.0 * a).mul_add(t, 2.0 * b).mul_add(t, c)
    }

    /// The curve parameter for progress `x`: Newton's method, bisection when it stalls.
    fn solve_t_for_x(&self, x: f32) -> f32 {
        let mut t = x;
        for _ in 0..8 {
            let err = Self::sample(self.x1, self.x2, t) - x;
            if err.abs() < 1e-6 {
                return t;
            }
            let d = self.sample_x_derivative(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..32 {
            let mid = f32::midpoint(lo, hi);
            if Self::sample(self.x1, self.x2, mid) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        f32::midpoint(lo, hi)
    }

    /// The eased output for progress `x`, clamped to 0–1 (GPUI asserts its deltas there).
    #[must_use]
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        Self::sample(self.y1, self.y2, self.solve_t_for_x(x)).clamp(0.0, 1.0)
    }

    /// The curve as a GPUI easing closure.
    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |x| self.eval(x)
    }
}

/// Zeron's entrance curve, `cubic-bezier(0.16, 1, 0.3, 1)`.
pub const EASE_OUT_EXPO: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
/// CSS `ease-out`: width and height transitions.
pub const EASE_OUT: CubicBezier = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
/// CSS `ease`: quick fades, menu and dialog pops.
pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
/// `easeOutQuint`, `cubic-bezier(0.22, 1, 0.36, 1)`.
pub const EASE_OUT_QUINT: CubicBezier = CubicBezier::new(0.22, 1.0, 0.36, 1.0);
/// The sidebar's resort glide.
pub const EASE_RESORT: CubicBezier = EASE_OUT_QUINT;
/// CSS `ease-in-out`: the transcript's scroll glide.
pub const EASE_IN_OUT: CubicBezier = CubicBezier::new(0.42, 0.0, 0.58, 1.0);
/// Tailwind's default transition curve, `cubic-bezier(0.4, 0, 0.2, 1)`.
pub const EASE_TAILWIND: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);
/// `cubic-bezier(0.2, 0, 0, 1)`: fast start, long soft landing.
pub const EASE_OUT_SOFT: CubicBezier = CubicBezier::new(0.2, 0.0, 0.0, 1.0);

/// One catalogue entry: a duration, an optional delay and a curve. The delay is folded into the
/// timeline: the animation runs for both and [`Self::progress`] holds 0 through the delay.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    /// Milliseconds of motion.
    pub duration_ms: u64,
    /// Milliseconds held before it.
    pub delay_ms: u64,
    /// The easing.
    pub curve: CubicBezier,
}

impl MotionSpec {
    /// `duration_ms` along `curve`.
    #[must_use]
    pub const fn new(duration_ms: u64, curve: CubicBezier) -> Self {
        Self { duration_ms, delay_ms: 0, curve }
    }

    /// The same, held `delay_ms` first.
    #[must_use]
    pub const fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// The whole timeline, delay and motion.
    #[must_use]
    pub const fn total(&self) -> Duration {
        Duration::from_millis(self.delay_ms.saturating_add(self.duration_ms))
    }

    /// Eased progress for a raw delta across [`Self::total`].
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "milliseconds well inside f32's range")]
    pub fn progress(&self, raw_delta: f32) -> f32 {
        let total = self.delay_ms.saturating_add(self.duration_ms) as f32;
        if total <= 0.0 || self.duration_ms == 0 {
            return 1.0;
        }
        let t = raw_delta.clamp(0.0, 1.0).mul_add(total, -(self.delay_ms as f32))
            / self.duration_ms as f32;
        self.curve.eval(t.clamp(0.0, 1.0))
    }

    /// A one-shot animation of this spec.
    #[must_use]
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(spec.total()).with_easing(move |d| spec.progress(d))
    }

    /// A repeating animation, linear over the period, for the loaders.
    #[must_use]
    pub fn repeating(&self) -> Animation {
        Animation::new(self.total()).repeat()
    }
}

/// Entrances: 500 ms expo-out, a fade and a 4 px rise.
pub const FADE_IN: MotionSpec = MotionSpec::new(500, EASE_OUT_EXPO);
/// A quick fade: 150 ms.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(150, EASE);
/// A popover's entrance: 140 ms.
pub const MENU_IN: MotionSpec = MotionSpec::new(140, EASE);
/// A popover's exit: 100 ms, quicker than the entrance.
pub const MENU_OUT: MotionSpec = MotionSpec::new(100, EASE);
/// A dialog's entrance: 180 ms.
pub const DIALOG_IN: MotionSpec = MotionSpec::new(180, EASE);
/// The boot splash's exit: 500 ms after a 150 ms hold.
pub const SPLASH_OUT: MotionSpec = MotionSpec::new(500, EASE).with_delay(150);
/// Sidebar, pane and dock size: 200 ms ease-out.
pub const RESIZE: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// Panel columns opening and closing; wide moves stretch it to 240 ms.
pub const PANEL_RESIZE: MotionSpec = DIALOG_IN;
/// Tab drag reorder: 150 ms.
pub const TAB_SLIDE: MotionSpec = MotionSpec::new(150, EASE_OUT);
/// A fold, a disclosure, the composer's morph: 180 ms.
pub const COLLAPSE: MotionSpec = MotionSpec::new(180, EASE_OUT);
/// The composer's move from the new-thread canvas to the dock: 420 ms.
pub const NEW_THREAD_TRANSITION: MotionSpec = MotionSpec::new(420, EASE_RESORT);
/// A chevron's turn, as a crossfade: 200 ms.
pub const CHEVRON: MotionSpec = MotionSpec::new(200, EASE);
/// A rail click's glide to its row: 500 ms.
pub const SCROLL_GLIDE: MotionSpec = MotionSpec::new(500, EASE_IN_OUT);
/// Every hover wash: 150 ms over Tailwind's curve.
pub const HOVER_FADE: MotionSpec = MotionSpec::new(150, EASE_TAILWIND);
/// A two-state glyph's morph: 280 ms.
pub const GLYPH_STATE: MotionSpec = MotionSpec::new(280, EASE_OUT_QUINT);
/// A segmented control's selection glide: 260 ms.
pub const INDICATOR_SLIDE: MotionSpec = MotionSpec::new(260, EASE_OUT_SOFT);
/// The pixel-mark loader's period: 2.4 s.
pub const ZERON_PULSE: MotionSpec = MotionSpec::new(2400, EASE);
/// The 3×3 working spinner's period: 750 ms.
pub const GRADIENT_SPIN: MotionSpec = MotionSpec::new(750, EASE);

/// How far a pane held past its limit nudges, in pixels.
pub const RESIZE_EDGE_NUDGE: f32 = 5.0;
/// The nudge's whole bounce, in milliseconds.
pub const RESIZE_EDGE_BOUNCE_MS: u64 = 220;
/// The share of the bounce spent going out.
pub const RESIZE_EDGE_BOUNCE_OUT_FRACTION: f32 = 0.32;

/// Which limit a resize met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    /// The least size.
    Min,
    /// The most size.
    Max,
}

/// One resize sample: its clamped width and the limit it met.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeDragSample {
    /// The width to lay out.
    pub width: f32,
    /// The limit met, if any.
    pub edge: Option<ResizeEdge>,
    /// Whether this sample starts a bounce: a held pointer starts one, not one an event.
    pub starts_bounce: bool,
}

/// Clamp a resize sample, latching the limit it meets.
#[must_use]
pub fn resize_drag_sample(
    requested: f32,
    min: f32,
    max: f32,
    latched_edge: Option<ResizeEdge>,
    reduced_motion: bool,
) -> ResizeDragSample {
    let edge = if requested <= min {
        Some(ResizeEdge::Min)
    } else if requested >= max {
        Some(ResizeEdge::Max)
    } else {
        None
    };
    ResizeDragSample {
        width: requested.clamp(min, max.max(min)),
        starts_bounce: !reduced_motion && edge.is_some() && edge != latched_edge,
        edge,
    }
}

/// A bounce running at a limit.
#[derive(Debug, Clone, Copy)]
pub struct ResizeEdgeBounce {
    /// The limit.
    pub edge: ResizeEdge,
    /// When it started.
    pub started: Instant,
}

impl ResizeEdgeBounce {
    /// A bounce at `edge`, from now.
    #[must_use]
    pub fn new(edge: ResizeEdge) -> Self {
        Self { edge, started: Instant::now() }
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * 2.0_f32.mul_add(-t, 3.0)
}

/// The bounce's offset at `raw` (0–1): out to the nudge, then a little longer home, both joins
/// at zero velocity.
#[must_use]
pub fn resize_bounce_offset(edge: ResizeEdge, raw: f32) -> f32 {
    let raw = raw.clamp(0.0, 1.0);
    let magnitude = if raw < RESIZE_EDGE_BOUNCE_OUT_FRACTION {
        smoothstep(raw / RESIZE_EDGE_BOUNCE_OUT_FRACTION)
    } else {
        1.0 - smoothstep(
            (raw - RESIZE_EDGE_BOUNCE_OUT_FRACTION) / (1.0 - RESIZE_EDGE_BOUNCE_OUT_FRACTION),
        )
    } * RESIZE_EDGE_NUDGE;
    match edge {
        ResizeEdge::Min => -magnitude,
        ResizeEdge::Max => magnitude,
    }
}

/// The standard entrance: a fade and a 4 px rise over [`FADE_IN`].
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(4.0 * (1.0 - t)))
    })
}

/// The new-thread canvas's entrance: a fade while settling 10 px down.
pub fn settle_down<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(-10.0 * (1.0 - t)))
    })
}

/// A quick fade over [`FADE_QUICK`].
pub fn fade_quick<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_QUICK.animation(), Styled::opacity)
}

/// A popover's entrance: from 30% opacity and 2 px above, over [`MENU_IN`].
pub fn menu_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    menu_in_from(id, -2.0, element)
}

/// [`menu_in`] travelling from the trigger's side: `from` is the signed starting offset.
pub fn menu_in_from<E>(id: impl Into<ElementId>, from: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_IN.animation(), move |el, t| {
        el.relative().opacity(0.7_f32.mul_add(t, 0.3)).top(px(from * (1.0 - t)))
    })
}

/// A popover's exit at the caller's progress `t`, retreating by half `toward`. The caller
/// computes `t` from the close's instant: an element clock would replay from 0 on a remount.
pub fn menu_out_toward<E>(
    id: impl Into<ElementId>,
    toward: f32,
    t: f32,
    element: E,
) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_OUT.animation(), move |el, _| {
        el.relative().opacity(1.0 - t).top(px(toward * 0.5 * t))
    })
}

/// A popover's exit at the caller's progress `t`: fade and rise 2 px.
pub fn menu_out<E>(id: impl Into<ElementId>, t: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_OUT.animation(), move |el, _| {
        el.relative().opacity(1.0 - t).top(px(-2.0 * t))
    })
}

/// A dialog's entrance: a fade and a 2 px rise over [`DIALOG_IN`].
pub fn dialog_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, DIALOG_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(2.0 * (1.0 - t)))
    })
}

/// The boot splash's exit: a 150 ms hold, then a fade and a 6 px lift.
pub fn splash_out<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element
        .with_animation(id, SPLASH_OUT.animation(), |el, t| el.opacity(1.0 - t).top(px(-6.0 * t)))
}

/// Loader cells rest at this opacity between pulses.
pub const PULSE_MIN_OPACITY: f32 = 0.08;
/// …and at this scale.
pub const PULSE_MIN_SCALE: f32 = 0.9;
/// The per-cell stagger, a share of the pulse period (0.15 s of 2.4 s).
pub const PULSE_STAGGER: f32 = 0.15 / 2.4;
/// The 3×3 spinner's side.
pub const MATRIX_SIDE: usize = 3;
/// The 3×3 spinner's row tints: cool blue at the top, through amber, to pink.
pub const GSPIN_ROW_TINTS: [u32; MATRIX_SIDE] = [0x00b6_d3ef, 0x00ed_b185, 0x00f8_88a0];
/// The opacity a spinner cell rests at.
pub const GSPIN_DIM: f32 = 0.1;
/// Each cell's place on the 2×3 mini spinner's clockwise ring, row by row.
pub const MINI_RING: [[usize; 2]; 3] = [[0, 1], [5, 2], [4, 3]];
/// Cells on the mini spinner's ring.
pub const MINI_RING_LEN: f32 = 6.0;

/// Linear interpolation.
#[must_use]
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    (to - from).mul_add(t, from)
}

/// A cell's phase, from the loader's and the cell's index.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "a cell index")]
pub fn staggered_phase(raw_delta: f32, index: usize, stagger: f32) -> f32 {
    (index as f32).mul_add(-stagger, raw_delta).rem_euclid(1.0)
}

/// A cosine pulse: 0 at phase 0, 1 at a half, 0 again at 1.
#[must_use]
pub fn pulse_wave(phase: f32) -> f32 {
    0.5_f32.mul_add(-(phase * std::f32::consts::TAU).cos(), 0.5)
}

/// A loader cell's opacity at `phase`: 0.08 to 1 and back.
#[must_use]
pub fn pulse_opacity(phase: f32) -> f32 {
    (1.0 - PULSE_MIN_OPACITY).mul_add(pulse_wave(phase), PULSE_MIN_OPACITY)
}

/// A loader cell's scale at `phase`: 0.9 to 1 and back.
#[must_use]
pub fn pulse_scale(phase: f32) -> f32 {
    (1.0 - PULSE_MIN_SCALE).mul_add(pulse_wave(phase), PULSE_MIN_SCALE)
}

/// A spinner cell's opacity at local phase `t`: full at the start, down to `dim` by 45%,
/// resting until 92%, then back to full.
#[must_use]
pub fn gspin_opacity(t: f32, dim: f32) -> f32 {
    let t = t.rem_euclid(1.0);
    if t < 0.45 {
        lerp(1.0, dim, t / 0.45)
    } else if t < 0.92 {
        dim
    } else {
        lerp(dim, 1.0, (t - 0.92) / 0.08)
    }
}

/// A 3×3 spinner cell's phase offset: the pulse enters at the bottom and converges on the top
/// centre, so the wave reads as rising.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "a cell's row and column")]
pub fn gspin_cell_phase(row: usize, col: usize) -> f32 {
    let side = MATRIX_SIDE as f32;
    let centre = (side - 1.0) / 2.0;
    let max = side - 1.0 + centre;
    let d = side - 1.0 - row as f32 + (col as f32 - centre).abs();
    d / (max + 1.0)
}

/// A wave's intensity at diagonal `wave_index` of `wave_count`, at `raw_delta` of its period.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "a count of diagonals")]
pub fn matrix_wave(raw_delta: f32, wave_index: usize, wave_count: usize) -> f32 {
    let count = wave_count.max(1) as f32;
    pulse_wave(staggered_phase(raw_delta, wave_index, 1.0 / count))
}

/// One element's hover fade, re-anchored at its current value when the pointer turns back.
#[derive(Debug, Clone, Copy)]
struct FadeEntry {
    origin: f32,
    target: f32,
    started: Instant,
    seen: u64,
}

impl FadeEntry {
    fn value(&self, now: Instant, duration: Duration) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        let raw = elapsed.as_secs_f32() / duration.as_secs_f32();
        lerp(self.origin, self.target, HOVER_FADE.curve.eval(raw))
    }

    fn settled(&self, now: Instant, duration: Duration) -> bool {
        (self.origin - self.target).abs() < f32::EPSILON
            || now.saturating_duration_since(self.started) >= duration
    }
}

/// A height tween a second click reverses from where it stands.
#[derive(Debug, Clone, Copy)]
pub struct DisclosureMotion {
    /// Which toggle it belongs to.
    pub epoch: u64,
    /// The height it left.
    pub from: f32,
    /// The height it is going to.
    pub to: f32,
    /// When it started.
    pub started: Instant,
}

impl DisclosureMotion {
    /// A tween from `from` to `to`, from now.
    #[must_use]
    pub fn new(epoch: u64, from: f32, to: f32) -> Self {
        Self { epoch, from, to, started: Instant::now() }
    }

    /// The height now.
    #[must_use]
    pub fn current(self) -> f32 {
        let total = COLLAPSE.total().as_secs_f32();
        let raw = if total > 0.0 { self.started.elapsed().as_secs_f32() / total } else { 1.0 };
        lerp(self.from, self.to, COLLAPSE.progress(raw))
    }

    /// Whether it is still moving.
    #[must_use]
    pub fn animating(self) -> bool {
        self.started.elapsed() < COLLAPSE.total().saturating_add(Duration::from_millis(120))
    }
}

/// A selection indicator's slide between segment slots, measured in segments, so a retarget
/// continues from where it is drawn.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorSlide {
    from: f32,
    to: f32,
    started: Instant,
}

impl IndicatorSlide {
    /// Resting at `position`.
    #[must_use]
    pub const fn at(position: f32, now: Instant) -> Self {
        Self { from: position, to: position, started: now }
    }

    fn progress(&self, now: Instant) -> f32 {
        let duration = INDICATOR_SLIDE.total();
        if duration.is_zero() {
            return 1.0;
        }
        INDICATOR_SLIDE.progress(
            now.saturating_duration_since(self.started).as_secs_f32() / duration.as_secs_f32(),
        )
    }

    /// The position at `now`.
    #[must_use]
    pub fn value_at(&self, now: Instant) -> f32 {
        lerp(self.from, self.to, self.progress(now))
    }

    /// Aim at a new slot; Reduce Motion snaps.
    pub fn retarget(&mut self, to: f32, reduced: bool, now: Instant) {
        let from = if reduced { to } else { self.value_at(now) };
        *self = Self { from, to, started: now };
    }

    /// Whether it is still moving at `now`.
    #[must_use]
    pub fn animating_at(&self, now: Instant) -> bool {
        (self.from - self.to).abs() > f32::EPSILON
            && now.saturating_duration_since(self.started) < INDICATOR_SLIDE.total()
    }
}

/// Hover progress by element key, with explicit times so it tests without a window.
#[derive(Debug, Default)]
pub struct HoverFades {
    entries: HashMap<String, FadeEntry>,
    frame: u64,
}

impl HoverFades {
    const fn duration() -> Duration {
        HOVER_FADE.total()
    }

    /// The pointer entered or left `key`'s element; Reduce Motion snaps.
    pub fn set_at(&mut self, key: &str, hovered: bool, reduced: bool, now: Instant) {
        let target = if hovered { 1.0 } else { 0.0 };
        let Some(current) = self.entries.get(key).map(|e| e.value(now, Self::duration())) else {
            if !hovered {
                return;
            }
            let origin = if reduced { target } else { 0.0 };
            let seen = self.frame;
            self.entries.insert(key.to_owned(), FadeEntry { origin, target, started: now, seen });
            return;
        };
        let origin = if reduced { target } else { current };
        let seen = self.frame;
        self.entries.insert(key.to_owned(), FadeEntry { origin, target, started: now, seen });
    }

    /// `key`'s hover progress at `now`, marking it seen this frame.
    pub fn value_at(&mut self, key: &str, now: Instant) -> f32 {
        let frame = self.frame;
        self.entries.get_mut(key).map_or(0.0, |entry| {
            entry.seen = frame;
            entry.value(now, Self::duration())
        })
    }

    /// Once a frame: drop entries at rest or unread for a whole frame (unmounted), and say
    /// whether any fade is still moving.
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame = self.frame.wrapping_add(1);
        let frame = self.frame;
        let mut active = false;
        self.entries.retain(|_, entry| {
            if entry.seen.saturating_add(1) < frame {
                return false;
            }
            let settled = entry.settled(now, Self::duration());
            active |= !settled;
            !(settled && entry.target == 0.0)
        });
        active
    }
}

thread_local! {
    static HOVER_FADES: RefCell<HoverFades> = RefCell::new(HoverFades::default());
}

/// `key`'s hover progress this frame.
#[must_use]
pub fn hover_t(key: &str) -> f32 {
    HOVER_FADES.with(|fades| fades.borrow_mut().value_at(key, Instant::now()))
}

/// Record that the pointer entered or left `key`'s element.
pub fn set_hover(key: &str, hovered: bool, reduced: bool) {
    HOVER_FADES.with(|fades| fades.borrow_mut().set_at(key, hovered, reduced, Instant::now()));
}

/// An `.on_hover` listener driving `key`'s fade; read it back with [`hover_blend`].
pub fn hover_listener(
    key: impl Into<SharedString>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let key = key.into();
    move |hovered, window, cx| {
        set_hover(&key, *hovered, reduced_motion(cx));
        window.refresh();
    }
}

/// Once per window frame (the shell's render tail): whether a hover fade still moves.
#[must_use]
pub fn hover_fades_active() -> bool {
    HOVER_FADES.with(|fades| fades.borrow_mut().tick_at(Instant::now()))
}

/// Two colours blended as a browser transitions them: sRGB with premultiplied alpha, so a wash
/// fading in from transparent brightens without passing through grey.
#[must_use]
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let (f, g) = (Rgba::from(from), Rgba::from(to));
    let a = lerp(f.a, g.a, t);
    if a <= f32::EPSILON {
        return Hsla::from(Rgba { a: 0.0, ..g });
    }
    Hsla::from(Rgba {
        r: lerp(f.r * f.a, g.r * g.a, t) / a,
        g: lerp(f.g * f.a, g.g * g.a, t) / a,
        b: lerp(f.b * f.a, g.b * g.a, t) / a,
        a,
    })
}

/// The standard hover blend: `rest` to `hover` at `key`'s progress.
#[must_use]
pub fn hover_blend(key: &str, rest: Hsla, hover: Hsla) -> Hsla {
    mix(rest, hover, hover_t(key))
}

#[derive(Debug, Clone, Copy)]
struct StateEntry {
    origin: f32,
    target: f32,
    started: Instant,
    spec: MotionSpec,
    seen: u64,
}

impl StateEntry {
    fn value(&self, now: Instant) -> f32 {
        let duration = self.spec.total();
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        lerp(
            self.origin,
            self.target,
            self.spec.progress(elapsed.as_secs_f32() / duration.as_secs_f32()),
        )
    }

    fn settled(&self, now: Instant) -> bool {
        (self.origin - self.target).abs() < f32::EPSILON
            || now.saturating_duration_since(self.started) >= self.spec.total()
    }
}

/// Two-state morphs by key: the first read snaps to the state (a mount never replays a morph),
/// a flip continues from the current value.
#[derive(Debug, Default)]
pub struct StateMorphs {
    entries: HashMap<String, StateEntry>,
    frame: u64,
}

impl StateMorphs {
    /// `key`'s progress toward `on` at `now`; Reduce Motion snaps a flip.
    pub fn value_at(
        &mut self,
        key: &str,
        on: bool,
        spec: MotionSpec,
        reduced: bool,
        now: Instant,
    ) -> f32 {
        let target = if on { 1.0 } else { 0.0 };
        let frame = self.frame;
        let Some(entry) = self.entries.get_mut(key) else {
            self.entries.insert(
                key.to_owned(),
                StateEntry { origin: target, target, started: now, spec, seen: frame },
            );
            return target;
        };
        if (entry.target - target).abs() > f32::EPSILON {
            let current = entry.value(now);
            *entry = StateEntry {
                origin: if reduced { target } else { current },
                target,
                started: now,
                spec,
                seen: frame,
            };
        }
        entry.seen = frame;
        entry.value(now)
    }

    /// Once a frame: drop unmounted entries and say whether a morph still moves.
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame = self.frame.wrapping_add(1);
        let frame = self.frame;
        let mut active = false;
        self.entries.retain(|_, entry| {
            if entry.seen.saturating_add(1) < frame {
                return false;
            }
            active |= !entry.settled(now);
            true
        });
        active
    }
}

thread_local! {
    static STATE_MORPHS: RefCell<StateMorphs> = RefCell::new(StateMorphs::default());
}

/// `key`'s morph progress toward `on` this frame.
#[must_use]
pub fn state_t(key: &str, on: bool, spec: MotionSpec, reduced: bool) -> f32 {
    STATE_MORPHS.with(|morphs| morphs.borrow_mut().value_at(key, on, spec, reduced, Instant::now()))
}

/// Once per window frame, beside [`hover_fades_active`]: whether a morph still moves.
#[must_use]
pub fn state_morphs_active() -> bool {
    STATE_MORPHS.with(|morphs| morphs.borrow_mut().tick_at(Instant::now()))
}

/// Whether motion is reduced: the system's Reduce Motion, which gpui-kit reads into the app.
#[must_use]
pub fn reduced_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, what: &str) {
        assert!((a - b).abs() < 1e-3, "{what}: {a} vs {b}");
    }

    #[test]
    fn the_curves_meet_their_css_values() {
        close(EASE.eval(0.5), 0.802, "ease at a half");
        close(EASE_OUT.eval(0.5), 0.685, "ease-out at a half");
        close(EASE_IN_OUT.eval(0.5), 0.5, "ease-in-out is symmetric");
        for x in [0.0, 0.1, 0.5, 0.9, 1.0] {
            let y = EASE_OUT_EXPO.eval(x);
            assert!((0.0..=1.0).contains(&y), "{x} -> {y}");
        }
    }

    #[test]
    fn a_delay_holds_progress_at_zero() {
        close(SPLASH_OUT.progress(0.1), 0.0, "inside the hold");
        close(SPLASH_OUT.progress(1.0), 1.0, "the end");
    }

    #[test]
    fn a_hover_fade_reverses_from_where_it_stands() {
        let mut fades = HoverFades::default();
        let start = Instant::now();
        fades.set_at("row", true, false, start);
        let half = start.checked_add(Duration::from_millis(75)).expect("time");
        let midway = fades.value_at("row", half);
        assert!(midway > 0.0 && midway < 1.0, "mid-fade: {midway}");
        fades.set_at("row", false, false, half);
        close(fades.value_at("row", half), midway, "continuous at the turn");
        let later = half.checked_add(Duration::from_millis(200)).expect("time");
        close(fades.value_at("row", later), 0.0, "back at rest");
    }

    #[test]
    fn reduce_motion_snaps_a_hover() {
        let mut fades = HoverFades::default();
        let now = Instant::now();
        fades.set_at("row", true, true, now);
        close(fades.value_at("row", now), 1.0, "snapped");
    }

    #[test]
    fn a_morph_snaps_on_its_first_read() {
        let mut morphs = StateMorphs::default();
        let now = Instant::now();
        close(morphs.value_at("glyph", true, GLYPH_STATE, false, now), 1.0, "mounts on");
    }

    #[test]
    fn a_held_drag_bounces_once_per_limit() {
        let first = resize_drag_sample(100.0, 200.0, 400.0, None, false);
        assert!(first.starts_bounce && first.edge == Some(ResizeEdge::Min), "first touch");
        close(first.width, 200.0, "clamped");
        let held = resize_drag_sample(90.0, 200.0, 400.0, first.edge, false);
        assert!(!held.starts_bounce, "held does not restart");
        close(resize_bounce_offset(ResizeEdge::Max, RESIZE_EDGE_BOUNCE_OUT_FRACTION), 5.0, "peak");
    }

    #[test]
    fn the_spinner_wave_rises() {
        assert!(gspin_cell_phase(MATRIX_SIDE - 1, 1) < gspin_cell_phase(0, 1), "bottom leads");
        close(gspin_opacity(0.0, GSPIN_DIM), 1.0, "starts full");
        close(gspin_opacity(0.7, GSPIN_DIM), GSPIN_DIM, "rests dim");
    }

    #[test]
    fn a_wash_fading_in_keeps_its_hue() {
        let rest = gpui::hsla(0.0, 0.0, 1.0, 0.0);
        let hover = gpui::hsla(0.0, 0.0, 1.0, 0.11);
        let half = mix(rest, hover, 0.5);
        close(half.l, 1.0, "white all the way");
        close(half.a, 0.055, "half the alpha");
    }
}
