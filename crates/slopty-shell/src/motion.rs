// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Animation kit — the shell's motion catalog as reusable helpers over gpui
//! [`Animation`]/[`AnimationExt`].
//!
//! Catalog:
//! - `fade-in`   0.5s  cubic-bezier(0.16,1,0.3,1), translateY 4→0 (entrances)
//! - `fade-quick` 0.15s
//! - `menu-in`   0.14s translateY −2→0 (popovers)
//! - `dialog-in` 0.18s fade + 2px rise
//! - `splash-out` 0.5s opacity + translateY −6, 0.15s delay
//! - the pulse loader: 2.4s staggered cell opacity 0.08→1, scale 0.9→1
//! - `gradient-spin-pulse` 750ms per-cell phase wave (working indicator)
//! - 200ms ease-out width/height transitions (sidebar/panes)
//!
//! Custom easing is a closure over gpui's `Fn(f32) -> f32` easing shape; CSS
//! `cubic-bezier()` is evaluated exactly by [`CubicBezier`].
//!
//! Reduced motion: gpui's `App::reduce_motion` flag is honored *automatically* by
//! every `with_animation` element — oneshot animations snap to their end state,
//! repeating ones to their start state, and no frames are scheduled. The
//! [`set_reduced_motion`]/[`reduced_motion`] wrappers make it a single global
//! switch; pure helpers take the flag explicitly where they run outside elements.
//! The switch follows the OS accessibility setting ([`set_system`]) and, when the
//! person asks, main-window focus ([`set_pause_in_background`]).
//! Activity grids use [`ActivityPulse`] to retain a gentle brightness cue under
//! system reduced motion; a paused background keeps them still.
//!
//! translateY is implemented as a relative-position `top` inset: taffy applies
//! relative insets after layout, so — like a CSS transform — siblings never move.
//! gpui has no scale transform for `div`s at the pinned rev (only `svg`
//! transformations), so `menu-in`/`dialog-in` approximate their scale component
//! with fade + translate; see the module report in ARCHITECTURE §4 follow-ups.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationElement, App, ElementId, EntityId, Global, Hsla, IntoElement,
    ParentElement, Rgba, SharedString, StatefulInteractiveElement, Styled, Window, px,
};

pub use gpui::AnimationExt;

// ---------------------------------------------------------------------------
// Pulse clock — throttled drive for the repeating loaders
// ---------------------------------------------------------------------------

/// Repeat-tick interval for the pulse/spinner loaders (~30fps).
///
/// The loaders used to run as gpui `with_animation(...repeating...)` elements,
/// which request a redraw every display frame for as long as they are mounted
/// — one Working session row pinned the whole window at 120Hz (measured 36%
/// CPU on an M-series laptop, with the always-hot Metal pipeline holding
/// hundreds of MB of graphics buffers). A shared 30fps clock is visually
/// equivalent for these chunky cell waves at a quarter of the redraws, and a
/// window with no spinner mounted schedules nothing at all.
const PULSE_TICK: Duration = Duration::from_millis(33);

/// How long a view stays on the tick list after its last spinner paint. One
/// lease outlives a few missed frames; an unmounted spinner stops renewing and
/// the view drops off, letting the clock park.
const PULSE_LEASE: Duration = Duration::from_millis(300);

// One process-wide epoch (`locked_phase`), so the clock's default is derived.
#[derive(Default)]
struct PulseClock {
    // No epoch of its own: phases count from [`process_epoch`].
    leases: HashMap<EntityId, PulseLease>,
    tick: u64,
    running: bool,
    // The moment each leased view shows (`pulse_moment`), and when the last tick came.
    shown: HashMap<EntityId, Shown>,
    ticked_at: Option<Instant>,
}

/// The moment a leased view shows, until a tick it is notified of draws it again.
struct Shown {
    at: Instant,
    /// A tick notified the view since it last drew.
    due: bool,
    /// The tick its pulse started on, kept while it stays leased: the start of the wait
    /// it shows, which paces it down ([`paced_phase`]).
    since: Instant,
}

struct PulseLease {
    until: Instant,
    stride: u64,
}

impl PulseLease {
    fn renew(&mut self, now: Instant, stride: u64) {
        self.until = now + PULSE_LEASE;
        self.stride = self.stride.min(stride);
    }

    fn take_tick(&mut self, tick: u64) -> bool {
        if !tick.is_multiple_of(self.stride) {
            return false;
        }
        // Each paint re-establishes the fastest mounted animation. A retired
        // 30Hz animation must not permanently pull a 15Hz loader up to 30Hz.
        self.stride = u64::MAX;
        true
    }
}

impl Global for PulseClock {}

// One clock for every repeating status animation in the process (Claude desktop
// re-anchors its infinite animations to `t mod period` of one clock), so every running dot in
// every row and every window moves in unison, whenever it mounted and whichever window drew it.
/// The instant every repeating animation's phase counts from, the same for the whole process.
fn process_epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(crate::clock::stopwatch) // an origin, not a moment drawn
}

/// The phase `[0,1)` at `now` of a repeating animation lasting `period`, on the process's one
/// clock: two animations of one period agree at any instant, however far apart they mounted.
pub fn locked_phase(period: Duration, now: Instant) -> f32 {
    if period.is_zero() {
        return 0.0;
    }
    let elapsed = now.saturating_duration_since(process_epoch()).as_secs_f64();
    #[expect(clippy::cast_possible_truncation, reason = "a phase in [0,1)")]
    let phase = (elapsed / period.as_secs_f64()).fract() as f32;
    phase
}

/// Current phase `[0,1)` of a repeating spec, plus a lease that keeps the
/// calling view re-rendering at [`PULSE_TICK`] while its spinner stays
/// mounted. All cells across all views share one epoch, so multi-instance
/// loaders stay phase-locked. Reduced motion returns a static 0 and schedules
/// nothing.
pub fn pulse_delta(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    pulse_delta_every(spec, view, 1, cx)
}

/// Coarser cell loaders on the shell need only 15Hz; their wall-clock period
/// and phase stay identical. Text dissolves still use the full 30Hz clock.
pub fn pulse_delta_slow(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    pulse_delta_every(spec, view, 2, cx)
}

fn pulse_delta_every(spec: &MotionSpec, view: EntityId, stride: u64, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 0.0;
    }
    pulse_lease_every(view, stride, cx);
    pulse_phase(spec, view, cx)
}

fn pulse_phase(spec: &MotionSpec, view: EntityId, cx: &mut App) -> f32 {
    // On the process's one clock ([`locked_phase`]), not the clock's first use, slowing
    // as the wait it shows grows long ([`paced_phase`]).
    let moment = pulse_moment(view, cx);
    let since = cx
        .try_global::<PulseClock>()
        .and_then(|clock| clock.shown.get(&view))
        .map(|shown| shown.since);
    paced_phase(spec.total(), moment, since)
}

// A long wait paces down (opencode's working indicators; item 22): a pulse runs at full
// pace as its wait starts and eases to 0.6 of it over the first minute, so a row left working for
// an hour reads calm, not urgent. The pace is integrated, so the phase never jumps, and the slack
// against the calm clock is taken up over the same minute, so every calm pulse is in step again.
/// How long a wait takes to reach its calm pace.
const PACE_RAMP: Duration = Duration::from_secs(60);
/// The calm pace, against the pace a wait starts at.
const PACE_CALM: f64 = 0.6;

/// The phase `[0,1)` at `now` of a repeating animation lasting `period` that has shown a wait
/// since `since`: [`locked_phase`] as the wait starts, and the phase of one process-wide clock
/// slowed to [`PACE_CALM`] once it is [`PACE_RAMP`] old, eased between them without a jump.
pub fn paced_phase(period: Duration, now: Instant, since: Option<Instant>) -> f32 {
    let Some(since) = since.filter(|_| !period.is_zero()) else {
        return locked_phase(period, now);
    };
    let epoch = process_epoch();
    let period = period.as_secs_f64();
    let ramp = PACE_RAMP.as_secs_f64();
    let start = since.saturating_duration_since(epoch).as_secs_f64();
    let age = now.saturating_duration_since(since).as_secs_f64();
    let u = (age / ramp).min(1.0);
    // ∫ pace over the wait, with pace = 1 − (1 − calm)·smoothstep(age / ramp).
    let slack = 1.0 - PACE_CALM;
    let travelled = if age < ramp {
        age - slack * ramp * (u.powi(3) - u.powi(4) / 2.0)
    } else {
        PACE_CALM * age + slack * ramp / 2.0
    };
    // What the calm clock is ahead of this wait's once both are calm, taken up over the ramp.
    let ahead = -(slack * ramp / 2.0 + slack * start) / period;
    let ahead = ahead - ahead.round();
    let smooth = u * u * (3.0 - 2.0 * u);
    #[expect(clippy::cast_possible_truncation, reason = "a phase in [0,1)")]
    let phase = ((start + travelled) / period + smooth * ahead).rem_euclid(1.0) as f32;
    phase
}

// What a pulse-driven view shows is the moment of the tick that drew it, not the frame's.
// The clock notifies it at 30 Hz (15 at stride 2) and it is drawn from its last paint in between,
// so a frame drawn between ticks, or one drawn again from scratch, must show it as that tick did.
/// The moment `view`, leased on the pulse ([`pulse_lease`]), shows: the tick's that it last drew
/// on after being notified of it, or the frame's when the clock is not running.
pub fn pulse_moment(view: EntityId, cx: &mut App) -> Instant {
    let frame = cx.frame_time();
    let Some(clock) = cx.try_global::<PulseClock>().filter(|clock| clock.running) else {
        return frame;
    };
    let tick = clock.ticked_at.unwrap_or(frame);
    let clock = cx.default_global::<PulseClock>();
    let shown = clock.shown.entry(view).or_insert(Shown { at: tick, due: false, since: tick });
    if std::mem::take(&mut shown.due) {
        shown.at = tick;
    }
    shown.at
}

/// Activity feedback uses a slow, uniform brightness pulse when the system
/// reduces motion. A paused background still stops all animation.
/// Decorative animation continues to use the reduced-motion-gated helpers.
#[derive(Clone, Copy)]
pub struct ActivityPulse {
    phase: f32,
    subtle: bool,
}

impl ActivityPulse {
    pub fn opacity(&self, cell_phase: f32, dim: f32) -> f32 {
        if self.subtle {
            // No travelling chase or size change, just a gentle 2.4s fade.
            0.6 + 0.2 * pulse_wave(self.phase)
        } else {
            gspin_opacity(self.phase + cell_phase, dim)
        }
    }
}

pub fn activity_pulse(view: EntityId, cx: &mut App) -> ActivityPulse {
    activity_pulse_every(view, 1, cx)
}

pub fn activity_pulse_slow(view: EntityId, cx: &mut App) -> ActivityPulse {
    activity_pulse_every(view, 2, cx)
}

fn activity_pulse_every(view: EntityId, stride: u64, cx: &mut App) -> ActivityPulse {
    let subtle = cx.reduce_motion();
    let animate = !subtle
        || cx.try_global::<MotionState>().is_some_and(|state| {
            state.system && !(state.pause_in_background && !state.active)
        });
    let phase = if animate {
        let stride = if subtle { 2 } else { stride };
        schedule_pulse_every(view, stride, cx);
        pulse_phase(if subtle { &SLOPTY_PULSE } else { &GRADIENT_SPIN }, view, cx)
    } else {
        0.0
    };
    ActivityPulse { phase, subtle }
}

/// Schedule cosmetic animation through the same bounded clock as loaders.
/// Renew only while painting an active animation; the clock parks after the
/// last lease expires, including when a view is hidden or removed.
pub fn pulse_lease(view: EntityId, cx: &mut App) {
    pulse_lease_every(view, 1, cx);
}

fn pulse_lease_every(view: EntityId, stride: u64, cx: &mut App) {
    if cx.reduce_motion() {
        return;
    }
    schedule_pulse_every(view, stride, cx);
}

fn schedule_pulse_every(view: EntityId, stride: u64, cx: &mut App) {
    let now = cx.background_executor().now(); // the executor's clock, a test's too
    let clock = cx.default_global::<PulseClock>();
    clock
        .leases
        .entry(view)
        .or_insert(PulseLease {
            until: now + PULSE_LEASE,
            stride,
        })
        .renew(now, stride);
    if !clock.running {
        clock.running = true;
        // The clock's first tick is now.
        clock.ticked_at = Some(now);
        cx.spawn(async move |cx| {
            #[cfg(windows)]
            let mut precise_clock = if cx
                .background_executor()
                .scheduler_executor()
                .scheduler()
                .as_test()
                .is_none()
            {
                windows_pulse::Clock::new(PULSE_TICK)
            } else {
                // A deterministic scheduler must own its timers and wakeups;
                // an OS thread cannot schedule its thread-local tasks safely.
                None
            };
            loop {
                #[cfg(windows)]
                let precise_tick = if let Some(clock) = precise_clock.as_mut() {
                    clock.tick().await
                } else {
                    false
                };
                #[cfg(not(windows))]
                let precise_tick = false;
                if !precise_tick {
                    cx.background_executor().timer(PULSE_TICK).await;
                }
                let parked = cx.update(|cx| {
                    let now = cx.background_executor().now();
                    let clock = cx.default_global::<PulseClock>();
                    clock.leases.retain(|_, lease| lease.until > now);
                    if clock.leases.is_empty() {
                        clock.running = false;
                        // Parked, what reads the pulse's moment reads the frame's.
                        clock.ticked_at = None;
                        clock.shown.clear();
                        return true;
                    }
                    clock.tick = clock.tick.wrapping_add(1);
                    clock.ticked_at = Some(now);
                    let leases = &clock.leases;
                    clock.shown.retain(|view, _| leases.contains_key(view));
                    let tick = clock.tick;
                    let views: Vec<EntityId> = clock
                        .leases
                        .iter_mut()
                        .filter_map(|(view, lease)| lease.take_tick(tick).then_some(*view))
                        .collect();
                    for view in &views {
                        if let Some(shown) = clock.shown.get_mut(view) {
                            shown.due = true;
                        }
                    }
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
}

// ---------------------------------------------------------------------------
// Cubic bezier
// ---------------------------------------------------------------------------

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function (endpoints fixed at
/// (0,0) and (1,1)). Evaluation solves x(t) = input by Newton iteration with a
/// bisection fallback — the standard UnitBezier approach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(a: f32, b: f32) -> (f32, f32, f32) {
        let c = 3.0 * a;
        let bb = 3.0 * (b - a) - c;
        let aa = 1.0 - c - bb;
        (aa, bb, c)
    }

    fn sample_x(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        ((a * t + b) * t + c) * t
    }

    fn sample_y(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.y1, self.y2);
        ((a * t + b) * t + c) * t
    }

    fn sample_x_derivative(&self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    /// Curve parameter `t` for a given progress `x` (both 0..1).
    fn solve_t_for_x(&self, x: f32) -> f32 {
        // Newton–Raphson.
        let mut t = x;
        for _ in 0..8 {
            let err = self.sample_x(t) - x;
            if err.abs() < 1e-6 {
                return t;
            }
            let d = self.sample_x_derivative(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        // Bisection fallback (x(t) is monotonic for valid CSS beziers).
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if self.sample_x(mid) < x {
                lo = mid
            } else {
                hi = mid
            }
        }
        (lo + hi) / 2.0
    }

    /// Eased output for input progress `x ∈ [0,1]` (clamped).
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        // f32 rounding can push sample_y a hair past 1.0 (observed 1.000000119
        // near the end of menu animations); gpui's animation element asserts
        // `delta ∈ [0,1]` and aborts, so clamp the output hard.
        self.sample_y(self.solve_t_for_x(x)).clamp(0.0, 1.0)
    }

    /// This curve as a gpui easing closure.
    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |x| self.eval(x)
    }
}

/// The signature entrance curve — CSS `cubic-bezier(0.16, 1, 0.3, 1)`.
pub const EASE_OUT_EXPO: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
/// CSS `ease-out` — width/height transitions.
pub const EASE_OUT: CubicBezier = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
/// CSS `ease` — quick fades, menu/dialog pops.
pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
/// `easeOutQuint` — CSS `cubic-bezier(0.22, 1, 0.36, 1)`.
pub const EASE_OUT_QUINT: CubicBezier = CubicBezier::new(0.22, 1.0, 0.36, 1.0);
/// Sidebar resort glide (used from M3b).
pub const EASE_RESORT: CubicBezier = EASE_OUT_QUINT;
/// CSS `ease-in-out` — the transcript scroll glide (browser smooth-scroll
/// shape: gentle start, cruise, gentle landing).
pub const EASE_IN_OUT: CubicBezier = CubicBezier::new(0.42, 0.0, 0.58, 1.0);

// ---------------------------------------------------------------------------
// Motion specs (the catalog)
// ---------------------------------------------------------------------------

/// One catalog entry: duration + optional delay + curve. The delay is folded into
/// the gpui animation timeline (gpui `Animation` has no native delay): the
/// animation runs for `delay + duration` and [`progress`](Self::progress) holds 0
/// until the delay has elapsed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration_ms: u64,
    pub delay_ms: u64,
    pub curve: CubicBezier,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: CubicBezier) -> Self {
        Self {
            duration_ms,
            delay_ms: 0,
            curve,
        }
    }

    pub const fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// Wall-clock span of the whole timeline (delay + duration).
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.delay_ms + self.duration_ms)
    }

    /// Eased progress (0..1) for a raw timeline delta (0..1 across [`total`](Self::total)).
    /// Pure — unit-testable without a window.
    pub fn progress(&self, raw_delta: f32) -> f32 {
        let total = (self.delay_ms + self.duration_ms) as f32;
        if total <= 0.0 || self.duration_ms == 0 {
            return 1.0;
        }
        let t =
            (raw_delta.clamp(0.0, 1.0) * total - self.delay_ms as f32) / self.duration_ms as f32;
        self.curve.eval(t.clamp(0.0, 1.0))
    }

    /// A oneshot gpui [`Animation`] for this spec (delay folded in).
    /// Wall-clock span honors [`speed_scale`] (measurement knob).
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(spec.total().mul_f32(speed_scale())).with_easing(move |d| spec.progress(d))
    }

    /// A repeating gpui [`Animation`] with linear easing over the raw period —
    /// for the pulse/wave loaders whose per-cell easing happens in the animator.
    pub fn repeating(&self) -> Animation {
        Animation::new(self.total()).repeat()
    }
}

/// Entrances: 0.5s expo-out fade + 4px rise.
pub const FADE_IN: MotionSpec = MotionSpec::new(500, EASE_OUT_EXPO);
/// Quick fade: 0.15s.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(150, EASE);
/// Wallpaper replacement: immediate attack with a short, soft landing.
pub const WALLPAPER_CROSSFADE: MotionSpec =
    MotionSpec::new(180, CubicBezier::new(1.0 / 3.0, 1.0, 2.0 / 3.0, 1.0));
/// How opaque a side pane's contents are `elapsed` into its width opening or closing
/// ([`PANE_CONTENT_IN`], [`PANE_CONTENT_OUT`]); honours [`speed_scale`].
pub fn pane_content_opacity(opening: bool, elapsed: Duration) -> f32 {
    let spec = if opening { PANE_CONTENT_IN } else { PANE_CONTENT_OUT };
    let total = spec.total().as_secs_f32() * speed_scale();
    let shown = spec.progress(elapsed.as_secs_f32() / total);
    if opening { shown } else { 1.0 - shown }
}

/// Popover-in: 0.14s (scale 0.96 approximated, translateY −2).
pub const MENU_IN: MotionSpec = MotionSpec::new(140, EASE);
/// Popover-out: 0.1s — quicker than the entrance (exits should get out of the
/// way; matches the Radix convention of a shorter close than open).
pub const MENU_OUT: MotionSpec = MotionSpec::new(100, EASE);
/// Dialog-in: 0.18s (scale 0.96→1 approximated).
pub const DIALOG_IN: MotionSpec = MotionSpec::new(180, EASE);
/// Boot splash exit: 0.5s fade + 6px lift after a 0.15s hold.
pub const SPLASH_OUT: MotionSpec = MotionSpec::new(500, EASE).with_delay(150);
/// Sidebar / pane width+height transitions: 200ms ease-out.
pub const RESIZE: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// Panel columns: 240 ms on the quint ease-out, quick off the mark and long on the landing.
/// Opencode's side panel, one duration whatever the distance (item 12).
pub const PANEL_RESIZE: MotionSpec = MotionSpec::new(240, EASE_OUT_QUINT);
/// A side pane's contents fade in a beat after its width starts to open (opencode's
/// 40 ms delay and 200 ms fade; item 12), so they never slide in half drawn.
pub const PANE_CONTENT_IN: MotionSpec = MotionSpec::new(200, EASE).with_delay(40);
/// Content fades out over the first 160 ms of a close, gone before the width lands.
pub const PANE_CONTENT_OUT: MotionSpec = MotionSpec::new(160, EASE);
// One fold for groups, cards and trays (Clanker's `Collapsible`; design-inspiration
// item 7). Opening, the height grows on [`FOLD_OPEN`] while the contents wait 80 ms and fade in on
// [`FOLD_REVEAL`], rising [`FOLD_RISE`] (standing in for its 2 px blur), so they never arrive half
// drawn; closing, they fade out on [`FOLD_HIDE`], gone before the height lands on [`FOLD_CLOSE`].
/// A fold opening: 280 ms on the quint ease-out.
pub const FOLD_OPEN: MotionSpec = MotionSpec::new(280, EASE_OUT_QUINT);
/// The ease a fold closes on: slow off, slow in (Clanker's ease-exit).
pub const EASE_EXIT: CubicBezier = CubicBezier::new(0.4, 0.0, 0.6, 1.0);
/// A fold closing: 220 ms on [`EASE_EXIT`].
pub const FOLD_CLOSE: MotionSpec = MotionSpec::new(220, EASE_EXIT);
/// A fold's contents fading in: 220 ms after an 80 ms wait.
pub const FOLD_REVEAL: MotionSpec = MotionSpec::new(220, EASE_OUT_QUINT).with_delay(80);
/// A fold's contents fading out: 150 ms.
pub const FOLD_HIDE: MotionSpec = MotionSpec::new(150, EASE_EXIT);
/// How far a fold's contents rise as they fade in, in pixels.
pub const FOLD_RISE: f32 = 3.0;

/// A fold `elapsed` into opening (or closing): its height's share of the way open, its contents'
/// opacity, and how far below their place they stand. Honours [`speed_scale`]; settled at once
/// under reduced motion (`still`).
pub fn fold_frame(opening: bool, elapsed: Duration, still: bool) -> FoldFrame {
    if still {
        let open = if opening { 1.0 } else { 0.0 };
        return FoldFrame { height: open, opacity: open, rise: 0.0 };
    }
    let at = |spec: MotionSpec| {
        let total = spec.total().as_secs_f32() * speed_scale();
        spec.progress(elapsed.as_secs_f32() / total)
    };
    if opening {
        let shown = at(FOLD_REVEAL);
        FoldFrame { height: at(FOLD_OPEN), opacity: shown, rise: FOLD_RISE * (1.0 - shown) }
    } else {
        FoldFrame { height: 1.0 - at(FOLD_CLOSE), opacity: 1.0 - at(FOLD_HIDE), rise: 0.0 }
    }
}

/// Where a fold stands: see [`fold_frame`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoldFrame {
    /// The height's share of the way open, 0 to 1.
    pub height: f32,
    /// The contents' opacity.
    pub opacity: f32,
    /// How far below their place the contents stand, in pixels.
    pub rise: f32,
}

/// Terminal tab drag-reorder sliding transforms: 150ms (§1.10).
pub const TAB_SLIDE: MotionSpec = MotionSpec::new(150, EASE_OUT);
/// Diff-pane per-file collapse: 180ms height (§1.11).
pub const COLLAPSE: MotionSpec = MotionSpec::new(180, EASE_OUT);
/// Reversible new-thread ↔ session handoff. The shared composer moves and
/// morphs on a fast-starting, soft-landing curve while the canvas/transcript
/// crossfade is staged around it. Slightly longer than a utility transition,
/// but still short enough to acknowledge a send immediately.
pub const NEW_THREAD_TRANSITION: MotionSpec = MotionSpec::new(420, EASE_RESORT);
/// Diff-pane chevron rotate: 200ms (§1.11; approximated as a crossfade — gpui
/// divs have no rotation transform at the pinned rev, same caveat as scale).
pub const CHEVRON: MotionSpec = MotionSpec::new(200, EASE);
/// Rail-tick / scroll-to-row glide: 500ms ease-in-out over the whole distance
/// (Electron parity — the original rail rode the browser's native smooth
/// scroll, a fixed-duration gentle ease, never percent-of-remaining).
pub const SCROLL_GLIDE: MotionSpec = MotionSpec::new(500, EASE_IN_OUT);
/// Tailwind's default transition curve — CSS `cubic-bezier(0.4, 0, 0.2, 1)`
/// (`transition-colors` et al. carry it unless overridden).
pub const EASE_TAILWIND: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);
/// CSS `transition-colors` default: 150ms over [`EASE_TAILWIND`] — the temporal
/// blend every interactive hover wash rides in the original.
pub const HOVER_FADE: MotionSpec = MotionSpec::new(150, EASE_TAILWIND);
/// Slopty Icons state morph: `--zi-duration: 280ms` over
/// `cubic-bezier(.22, 1, .36, 1)` — the sidebar glyph's panel open ↔ closed.
pub const GLYPH_STATE: MotionSpec = MotionSpec::new(280, EASE_OUT_QUINT);
/// CSS `cubic-bezier(0.2, 0, 0, 1)` — fast start, long soft landing.
pub const EASE_OUT_SOFT: CubicBezier = CubicBezier::new(0.2, 0.0, 0.0, 1.0);
// A tooltip enters as Claude desktop's does, quick and landing soft (`duration-snap`).
/// Tooltip-in: 120ms over [`EASE_OUT_SOFT`].
pub const TOOLTIP_IN: MotionSpec = MotionSpec::new(120, EASE_OUT_SOFT);
/// Selection indicator glide between the segments of a pill nav: 260ms.
pub const INDICATOR_SLIDE: MotionSpec = MotionSpec::new(260, EASE_OUT_SOFT);
/// Slopty loader pulse period: 2.4s.
pub const SLOPTY_PULSE: MotionSpec = MotionSpec::new(2400, EASE);
/// Gradient matrix spinner wave period: 750ms.
pub const GRADIENT_SPIN: MotionSpec = MotionSpec::new(750, EASE);

// ---------------------------------------------------------------------------
// Resize-edge feedback
// ---------------------------------------------------------------------------

/// Pane resize limits acknowledge a held pointer without persisting an
/// out-of-range size. The small displacement is shared by the shell panes and
/// nested surface splits so every seam has the same physical response.
pub const RESIZE_EDGE_NUDGE: f32 = 5.0;
pub const RESIZE_EDGE_BOUNCE_MS: u64 = 220;
pub const RESIZE_EDGE_BOUNCE_OUT_FRACTION: f32 = 0.32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Min,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResizeDragSample {
    pub width: f32,
    pub edge: Option<ResizeEdge>,
    pub starts_bounce: bool,
}

/// Clamp a resize sample while latching its constrained edge. A held pointer
/// starts one bounce rather than restarting it for every drag event.
pub fn resize_drag_sample(
    requested: f32,
    min: f32,
    max: f32,
    latched_edge: Option<ResizeEdge>,
    reduced_motion: bool,
) -> ResizeDragSample {
    debug_assert!(min <= max);
    let edge = if requested <= min {
        Some(ResizeEdge::Min)
    } else if requested >= max {
        Some(ResizeEdge::Max)
    } else {
        None
    };
    ResizeDragSample {
        width: requested.clamp(min, max),
        starts_bounce: !reduced_motion && edge.is_some() && edge != latched_edge,
        edge,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResizeEdgeBounce {
    pub edge: ResizeEdge,
    pub started: Instant,
}

impl ResizeEdgeBounce {
    pub fn new(edge: ResizeEdge, now: Instant) -> Self {
        Self {
            edge,
            started: now,
        }
    }
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Rounded two-phase pulse: ease out to the overshoot, then take a little
/// longer to ease home. Both joins have zero velocity.
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

// ---------------------------------------------------------------------------
// Element helpers (paint-layer entrances/exits)
// ---------------------------------------------------------------------------

/// Standard entrance: opacity 0→1 + translateY 4→0 over [`FADE_IN`].
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(4.0 * (1.0 - t)))
    })
}

/// New-thread composition entrance: opacity 0→1 while settling 10px down into
/// place over [`FADE_IN`]. Keeping the logo, target selectors, composer, and
/// checkout row under one animation makes the blank canvas arrive as a single
/// object instead of four independently moving pieces.
pub fn settle_down<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(-10.0 * (1.0 - t)))
    })
}

/// Quick opacity-only fade over [`FADE_QUICK`].
pub fn fade_quick<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_QUICK.animation(), |el, t| el.opacity(t))
}

/// A tooltip's entrance, a fade over [`TOOLTIP_IN`] (gpui has no div scale for the
/// reference's 0.98).
pub fn tooltip_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, TOOLTIP_IN.animation(), |el, t| el.opacity(t))
}

/// Popover entrance: fade + translateY −2→0 over [`MENU_IN`].
/// (No scale 0.96→1: divs have no scale transform in gpui.)
pub fn menu_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_IN.animation(), |el, t| {
        el.relative()
            .opacity(0.3 + 0.7 * t)
            .top(px(-2.0 * (1.0 - t)))
    })
}

/// [`menu_in`] travelling from the trigger's side: `from` is the signed
/// starting offset (negative above the resting place for dropdowns,
/// positive below it for menus that open upward).
pub fn menu_in_from<E>(id: impl Into<ElementId>, from: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_IN.animation(), move |el, t| {
        el.relative()
            .opacity(0.3 + 0.7 * t)
            .top(px(from * (1.0 - t)))
    })
}

/// [`menu_out`] retreating toward the trigger by half the entrance travel.
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

/// Popover exit: the reverse of [`menu_in`] — fade to 0 + translateY 0→−2 over
/// [`MENU_OUT`]. Unlike the entrances, the eased progress `t` comes from the
/// caller (computed off [`crate::popover::Popup`]'s closing instant at render
/// time): `with_animation`'s element-id-keyed clock replays from 0 on remount
/// (the hover-blend comment's warning), and a replay mid-exit is a full-opacity
/// flash. The wall-clock progress is monotonic by construction; the animation
/// wrapper here only pumps frames for the exit's span, its own delta unused.
pub fn menu_out<E>(id: impl Into<ElementId>, t: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_OUT.animation(), move |el, _| {
        el.relative().opacity(1.0 - t).top(px(-2.0 * t))
    })
}

/// Dialog entrance over [`DIALOG_IN`] (scale approximated with fade + 2px rise).
pub fn dialog_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, DIALOG_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(2.0 * (1.0 - t)))
    })
}

/// Boot-splash exit: hold 150ms, then fade out + lift 6px over 500ms.
pub fn splash_out<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, SPLASH_OUT.animation(), |el, t| {
        el.opacity(1.0 - t).top(px(-6.0 * t))
    })
}

// ---------------------------------------------------------------------------
// Loader math (pure; rendered by crate::loaders)
// ---------------------------------------------------------------------------

/// Slopty-pulse floor opacity.
// The loader constants and math live in `slopty_view_proto::motion` (pure phase
// functions); this crate animates them with gpui.
pub use slopty_view_proto::motion::{
    PULSE_MIN_OPACITY, PULSE_MIN_SCALE, PULSE_STAGGER, gspin_opacity, pulse_opacity, pulse_scale,
    pulse_wave, staggered_phase,
};

/// Gradient-matrix spinner wave: intensity (0..1) of cell `wave_index` out of
/// `wave_count` diagonals, at raw delta `raw_delta` of the 750ms period. The wave
/// front travels across diagonals once per period.
pub fn matrix_wave(raw_delta: f32, wave_index: usize, wave_count: usize) -> f32 {
    let count = wave_count.max(1) as f32;
    pulse_wave(staggered_phase(raw_delta, wave_index, 1.0 / count))
}

/// Linear interpolation (layout tweens).
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

// ---------------------------------------------------------------------------
// Hover color fades (CSS `transition-colors` parity)
// ---------------------------------------------------------------------------
//
// gpui `.hover()` styles snap by construction — the style applies the frame
// the pointer enters. Every interactive wash here fades instead, over Tailwind's
// `transition-colors` (150ms, cubic-bezier(0.4, 0, 0.2, 1)). This is the
// manual-drive tween for that (the shell `WidthTween` pattern — never
// `with_animation`, whose element-id-keyed clock replays on remount): a
// per-element-key hover progress, advanced from wall time on each evaluation,
// with the render tail requesting frames while any fade is mid-flight.
//
// The store is a main-thread `thread_local` rather than a gpui Global so the
// many free-function element builders (window-control buttons, popover menu
// rows, markdown code blocks) can blend colors without threading `cx` through
// every signature. All access happens on the UI thread (element builders,
// mouse listeners, the render tail).
//
// Staleness: an element that unmounts mid-hover never gets its leave event, so
// entries are stamped with a frame counter on every read and pruned by
// [`hover_fades_active`] (the once-per-frame tick) when a full frame passes
// without a read — a reopened menu never inherits a dead entry's wash.

/// One element's hover fade: progress runs `origin → target` over
/// [`HOVER_FADE`], re-anchored at `origin` whenever the pointer flips
/// direction mid-flight so the blend is continuous.
#[derive(Debug, Clone, Copy)]
struct FadeEntry {
    origin: f32,
    target: f32,
    started: Instant,
    /// Frame counter at the last read (liveness stamp — see module notes).
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
        self.origin == self.target || now.saturating_duration_since(self.started) >= duration
    }
}

/// Interruptible height tween for the shared collapsible sections.
/// The rendered element owns the frame clock; this state preserves the current
/// interpolated height when a second click reverses an in-flight transition.
#[derive(Clone, Copy)]
pub(crate) struct DisclosureMotion {
    pub(crate) epoch: u64,
    pub(crate) from: f32,
    pub(crate) to: f32,
    pub(crate) started: std::time::Instant,
}

impl DisclosureMotion {
    pub(crate) fn new(epoch: u64, from: f32, to: f32, now: Instant) -> Self {
        Self {
            epoch,
            from,
            to,
            started: now,
        }
    }

    pub(crate) fn current(self, now: Instant) -> f32 {
        let total = COLLAPSE.total().as_secs_f32();
        let raw = if total > 0.0 {
            now.saturating_duration_since(self.started).as_secs_f32() / total
        } else {
            1.0
        };
        lerp(self.from, self.to, COLLAPSE.progress(raw))
    }

    pub(crate) fn animating(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) < COLLAPSE.total() + Duration::from_millis(120)
    }
}

/// Interruptible slide of a selection indicator between segment slots.
/// `position` is measured in segments (`1.5` sits halfway between the second
/// and third), so a retarget mid-flight continues from the drawn position
/// instead of jumping. Pure core — callers pass `now`.
#[derive(Debug, Clone, Copy)]
pub struct IndicatorSlide {
    from: f32,
    to: f32,
    started: Instant,
}

impl IndicatorSlide {
    pub fn at(position: f32, now: Instant) -> Self {
        Self {
            from: position,
            to: position,
            started: now,
        }
    }

    fn duration() -> Duration {
        INDICATOR_SLIDE.total().mul_f32(speed_scale())
    }

    fn progress(&self, now: Instant) -> f32 {
        let duration = Self::duration();
        if duration.is_zero() {
            return 1.0;
        }
        let raw =
            now.saturating_duration_since(self.started).as_secs_f32() / duration.as_secs_f32();
        INDICATOR_SLIDE.progress(raw)
    }

    pub fn value_at(&self, now: Instant) -> f32 {
        lerp(self.from, self.to, self.progress(now))
    }

    /// Aim at a new slot. Reduced motion snaps.
    pub fn retarget(&mut self, to: f32, reduced: bool, now: Instant) {
        let from = if reduced { to } else { self.value_at(now) };
        *self = Self {
            from,
            to,
            started: now,
        };
    }

    pub fn animating_at(&self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.started) < Self::duration()
    }
}

/// Per-key hover progress store. Pure core (explicit `now`) — unit-testable;
/// the thread-local wrappers below feed it wall time.
#[derive(Default)]
pub struct HoverFades {
    entries: HashMap<String, FadeEntry>,
    frame: u64,
    /// How many elements laid out with a [`fade_driver`] stand behind each key, while
    /// they are drawn: afresh, or again from the last frame, which reads no fade.
    mounted: HashMap<String, usize>,
}

impl HoverFades {
    fn duration() -> Duration {
        HOVER_FADE.total().mul_f32(speed_scale())
    }

    /// Pointer entered (`hovered`) or left the element behind `key`. Reduced
    /// motion snaps straight to the endpoint.
    pub fn set_at(&mut self, key: &str, hovered: bool, reduced: bool, now: Instant) {
        let target = if hovered { 1.0 } else { 0.0 };
        let duration = Self::duration();
        let current = self
            .entries
            .get(key)
            .map(|e| e.value(now, duration))
            .unwrap_or(0.0);
        if target == 0.0 && !self.entries.contains_key(key) {
            return; // never-hovered element reporting a leave — nothing to do
        }
        let origin = if reduced { target } else { current };
        let seen = self.frame;
        self.entries.insert(
            key.to_string(),
            FadeEntry {
                origin,
                target,
                started: now,
                seen,
            },
        );
    }

    /// Hover progress (0..1) for `key` at `now`; stamps liveness.
    pub fn value_at(&mut self, key: &str, now: Instant) -> f32 {
        let frame = self.frame;
        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.seen = frame;
                entry.value(now, Self::duration())
            }
            None => 0.0,
        }
    }

    /// Whether `key`'s fade is under way at `now`.
    fn moving_at(&self, key: &str, now: Instant) -> bool {
        self.entries.get(key).is_some_and(|entry| !entry.settled(now, Self::duration()))
    }

    /// An element with `key`'s [`fade_driver`] is laid out, and stays until
    /// [`Self::unmount`]: the fades hold the key through frames that draw it from the last one.
    pub fn mount(&mut self, key: &str) {
        *self.mounted.entry(key.to_owned()).or_default() += 1;
    }

    /// An element [`Self::mount`] told of is gone: once none stands behind `key`, its
    /// leave will never come, and the fade lets it go.
    pub fn unmount(&mut self, key: &str) {
        let Some(count) = self.mounted.get_mut(key) else { return };
        *count -= 1;
        if *count == 0 {
            self.mounted.remove(key);
            self.entries.remove(key);
        }
    }

    /// Whether `key` is held though no read of the last frame stamped it.
    #[cfg(test)]
    fn unread(&self, key: &str) -> bool {
        self.entries.get(key).is_some_and(|entry| entry.seen + 1 < self.frame)
    }

    /// Once-per-frame bookkeeping: advance the frame counter, prune entries
    /// that settled back to rest or went a full frame unread (unmounted), and
    /// report whether any fade is still mid-flight (→ keep frames coming).
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame += 1;
        let frame = self.frame;
        let duration = Self::duration();
        let mut active = false;
        let mounted = &self.mounted;
        self.entries.retain(|key, entry| {
            // Unread through the whole previous frame: the element unmounted
            // (its leave event will never come) — drop the entry.
            // Unless a mounted element holds it, drawn from its last frame, which reads
            // nothing ([`HoverFades::mount`]).
            if entry.seen + 1 < frame && !mounted.contains_key(key) {
                return false;
            }
            let settled = entry.settled(now, duration);
            if !settled {
                active = true;
            }
            // Settled at rest — steady state, indistinguishable from absent.
            !(settled && entry.target == 0.0)
        });
        active
    }
}

thread_local! {
    static HOVER_FADES: RefCell<HoverFades> = RefCell::new(HoverFades::default());
    /// The moment of the frame the fades were last ticked for ([`hover_fades_active`]),
    /// which every read of the frame stands at.
    static HOVER_FRAME: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// The instant a hover flips at: the dispatcher's clock, which a test moves.
fn hover_now() -> Instant {
    crate::clock::event_now()
}

/// The instant a fade is read at: the moment of the frame the shell ticked the fades
/// for, so a frame drawn twice at one moment draws each fade alike; before any frame, now.
fn hover_frame() -> Instant {
    HOVER_FRAME.get().unwrap_or_else(hover_now)
}

/// Stamp this thread's hover flips on `executor`'s clock ([`crate::clock::run_on`]).
pub(crate) fn run_on(executor: gpui::BackgroundExecutor) {
    crate::clock::run_on(executor);
}

/// Hover progress (0..1) for `key` this frame.
pub fn hover_t(key: &str) -> f32 {
    HOVER_FADES.with(|fades| fades.borrow_mut().value_at(key, hover_frame()))
}

/// How far a control that a hover reveals shows: on a touch screen, where nothing hovers,
/// it is there at rest (opencode gates every reveal on `(hover: hover)`); elsewhere it is `key`'s
/// hover fade, as [`hover_t`].
pub fn revealed_by_hover(key: &str) -> f32 {
    if cfg!(target_os = "ios") { 1.0 } else { hover_t(key) }
}

/// Whether the fades hold `key`, hovered or fading.
pub fn hover_known(key: &str) -> bool {
    HOVER_FADES.with(|fades| fades.borrow().entries.contains_key(key))
}

/// Whether the fades hold `key` though no read of the last frame stamped it: its
/// element was drawn from an earlier frame, or not at all.
#[cfg(test)]
pub(crate) fn hover_unread(key: &str) -> bool {
    HOVER_FADES.with(|fades| fades.borrow().unread(key))
}

/// Record a hover flip for `key` (reduced motion snaps).
pub fn set_hover(key: &str, hovered: bool, reduced: bool) {
    HOVER_FADES.with(|fades| {
        fades
            .borrow_mut()
            .set_at(key, hovered, reduced, hover_now())
    });
}

/// An element's hover fade for `key`, read with [`hover_t`]/[`hover_blend`] in the
/// same view: the pointer's flips set it ([`hover_listener`]), and while it runs, each frame the
/// element is laid out in asks its own view for the next. The shell's frames keep coming while
/// any fade runs, but a view the window draws from its last frame reads no fade: without its
/// own ask, a view of its own (a settings page, a picker) would keep the frame the flip drew.
pub trait HoverFade: StatefulInteractiveElement + ParentElement {
    /// Fade `key` with the pointer over this element.
    #[must_use]
    fn hover_fade(self, key: impl Into<SharedString>) -> Self {
        let key = key.into();
        self.on_hover(hover_listener(key.clone())).child(fade_driver(key))
    }
}

impl<E: StatefulInteractiveElement + ParentElement> HoverFade for E {}

/// The part of [`HoverFade`] that keeps `key`'s fade moving, for an element whose own
/// hover listener calls [`hover_listener`]'s: nothing drawn, and while the fade runs, the view
/// it is laid out in is asked for the next frame.
///
/// It also holds the element's place in the fades ([`FadeMount`]). A view the window draws from
/// its last frame reads no fade, and the fades let go of a key no frame read, as an element gone
/// whose leave will never come: a card, row or trigger the pointer rests on would then come back
/// unlit when its view is next built, while the frame before drew it lit. GPUI keeps an
/// element's state for as long as the element is drawn, afresh or again from the last frame, and
/// drops it once a frame leaves the element out: the mount lives exactly as long as the element.
pub fn fade_driver(key: SharedString) -> impl IntoElement {
    gpui::canvas(
        move |_, window, _| {
            window.with_global_id(gpui::ElementId::Name(key.clone()), |id, window| {
                window.with_element_state::<FadeMount, _>(id, |mount, _| {
                    ((), mount.unwrap_or_else(|| FadeMount::new(key.clone())))
                })
            });
            if HOVER_FADES.with(|f| f.borrow().moving_at(&key, hover_frame())) {
                window.request_animation_frame();
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_0()
}

/// An element with a [`fade_driver`], for as long as GPUI keeps its state: mounted in
/// the fades when made, unmounted when dropped.
struct FadeMount(SharedString);

impl FadeMount {
    fn new(key: SharedString) -> Self {
        HOVER_FADES.with(|fades| fades.borrow_mut().mount(&key));
        Self(key)
    }
}

impl Drop for FadeMount {
    fn drop(&mut self) {
        // A window dropped as the thread ends finds the fades already gone.
        let _gone = HOVER_FADES.try_with(|fades| {
            if let Ok(mut fades) = fades.try_borrow_mut() {
                fades.unmount(&self.0);
            }
        });
    }
}

/// An `.on_hover` listener driving the fade for `key` — pair with
/// [`hover_t`]/[`hover_blend`] reads of the same key in the same element.
pub fn hover_listener(
    key: impl Into<SharedString>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let key = key.into();
    move |hovered, window, cx| {
        set_hover(&key, *hovered, reduced_motion(cx));
        // Event-dispatch context: `request_animation_frame` is draw-phase-only
        // (it resolves the current view) — `refresh` builds every view again
        // for the next frame, and the element's [`fade_driver`] keeps its own
        // view's frames coming while the fade is mid-flight.
        window.refresh();
    }
}

/// The frame's moment, which every hover read of the frame stands at: the shell sets
/// it as its render begins, before any element reads a fade.
pub fn begin_hover_frame(now: Instant) {
    HOVER_FRAME.set(Some(now));
}

/// Frame-drive hook: call ONCE per window frame (the shell render tail) with the frame's
/// moment, which the frame's reads stand at; true while any hover fade is mid-flight and
/// frames must keep coming.
pub fn hover_fades_active(now: Instant) -> bool {
    HOVER_FRAME.set(Some(now));
    HOVER_FADES.with(|fades| fades.borrow_mut().tick_at(now))
}

/// Blend two colors by `t` the way the browser transitions them: component
/// interpolation in sRGB with premultiplied alpha — a wash fading in from
/// transparent brightens without passing through grey.
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
        // Both endpoints (effectively) transparent — carry the target's hue.
        return Hsla::from(Rgba { a: 0.0, ..g });
    }
    Hsla::from(Rgba {
        r: lerp(f.r * f.a, g.r * g.a, t) / a,
        g: lerp(f.g * f.a, g.g * g.a, t) / a,
        b: lerp(f.b * f.a, g.b * g.a, t) / a,
        a,
    })
}

/// The standard hover blend: rest → hover color at `key`'s current progress.
pub fn hover_blend(key: &str, rest: Hsla, hover: Hsla) -> Hsla {
    mix(rest, hover, hover_t(key))
}

// ---------------------------------------------------------------------------
// Two-state glyph morphs (CSS `transition` on a `data-state` flip)
// ---------------------------------------------------------------------------
//
// Slopty Icons morph between two states (the sidebar glyph's panel narrows when
// the sidebar closes). Same manual clock and liveness rules as the hover
// fades, but keyed by the element's *state* rather than pointer events: the
// first read of a key snaps to its state (mounting never replays a morph), a
// flip re-anchors at the current value so a mid-flight toggle reverses
// smoothly, and an entry unread for a full frame is pruned.

#[derive(Debug, Clone, Copy)]
struct StateEntry {
    origin: f32,
    target: f32,
    started: Instant,
    spec: MotionSpec,
    seen: u64,
}

impl StateEntry {
    fn duration(&self) -> Duration {
        self.spec.total().mul_f32(speed_scale())
    }

    fn value(&self, now: Instant) -> f32 {
        let duration = self.duration();
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        let raw = elapsed.as_secs_f32() / duration.as_secs_f32();
        lerp(self.origin, self.target, self.spec.progress(raw))
    }

    fn settled(&self, now: Instant) -> bool {
        self.origin == self.target || now.saturating_duration_since(self.started) >= self.duration()
    }
}

/// Per-key state-morph store. Pure core (explicit `now`) — unit-testable.
#[derive(Default)]
pub struct StateMorphs {
    entries: HashMap<String, StateEntry>,
    frame: u64,
}

impl StateMorphs {
    /// Progress (0 = off, 1 = on) of `key` toward `on` at `now`; stamps
    /// liveness. Reduced motion snaps a flip straight to its endpoint.
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
                key.to_string(),
                StateEntry {
                    origin: target,
                    target,
                    started: now,
                    spec,
                    seen: frame,
                },
            );
            return target;
        };
        if entry.target != target {
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

    /// Once-per-frame bookkeeping: prune unmounted entries and report whether
    /// any morph is still mid-flight (→ keep frames coming).
    pub fn tick_at(&mut self, now: Instant) -> bool {
        self.frame += 1;
        let frame = self.frame;
        let mut active = false;
        self.entries.retain(|_, entry| {
            if entry.seen + 1 < frame {
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

/// Forget every hover fade and state morph, as a new app starts with none. The stores
/// are this thread's, which keeps them across a `#[gpui::test]`'s seeds, so a test that reads
/// them starts here and no seed inherits the hover the last one left (the nightly's GPUI seeds,
/// 2026-10-29).
#[cfg(test)]
pub(crate) fn forget_for_test() {
    HOVER_FADES.with(|fades| *fades.borrow_mut() = HoverFades::default());
    HOVER_FRAME.with(|frame| frame.set(None));
    STATE_MORPHS.with(|morphs| *morphs.borrow_mut() = StateMorphs::default());
}

/// Morph progress (0..1) of `key` toward `on` at `now`, the frame's moment (`cx.frame_time()`).
// The frame's moment, not the wall clock's, so a frame drawn again from scratch at the
// same moment stands the morph where it stood.
pub fn state_t(key: &str, on: bool, spec: MotionSpec, reduced: bool, now: Instant) -> f32 {
    STATE_MORPHS.with(|morphs| morphs.borrow_mut().value_at(key, on, spec, reduced, now))
}

/// Frame-drive hook, beside [`hover_fades_active`]: call ONCE per window frame, at its moment.
pub fn state_morphs_active(now: Instant) -> bool {
    STATE_MORPHS.with(|morphs| morphs.borrow_mut().tick_at(now))
}

// ---------------------------------------------------------------------------
// Reduced motion
// ---------------------------------------------------------------------------

/// Dev/measurement knob (`SLOPTY_MOTION_SCALE`, default 1): stretches every
/// catalog timeline by this factor — e.g. `SLOPTY_MOTION_SCALE=10` slows the
/// 200ms pane tweens to 2s so screenshot bursts can sample the geometry
/// per frame. Read once; never set in production.
pub fn speed_scale() -> f32 {
    static SCALE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        std::env::var("SLOPTY_MOTION_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|s| s.is_finite())
            .map(|s| s.clamp(0.01, 100.0))
            .unwrap_or(1.0)
    })
}

/// Global reduced-motion flag. gpui snaps every `with_animation` element when
/// set (end state for oneshots, rest state for loops) and schedules no frames.
pub fn set_reduced_motion(cx: &mut App, reduced: bool) {
    cx.set_reduce_motion(reduced);
}

/// Read the global reduced-motion flag.
pub fn reduced_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

/// The global reduced-motion flag as a view read it for its render pass, kept for the
/// code that runs without a `cx`. Only [`ReducedMotion::of`] makes one, from the flag: a view's
/// copy is the flag's, so a test asks for reduced motion through [`set_reduced_motion`], never by
/// writing the copy, which the next render would overwrite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReducedMotion(bool);

impl ReducedMotion {
    /// The flag now.
    pub fn of(cx: &App) -> Self {
        Self(reduced_motion(cx))
    }

    /// Whether motion is reduced.
    pub const fn get(self) -> bool {
        self.0
    }
}

/// Inputs to the global flag: the person's choice to pause in the background, plus what the
/// OS and the main window last reported.
struct MotionState {
    pause_in_background: bool,
    system: bool,
    active: bool,
    /// The flag as a holder fixed it ([`hold`]), over the OS's setting, a paused background
    /// and the window's activation.
    held: Option<bool>,
}

impl Global for MotionState {}

/// The OS's setting as the app heard it before [`init`] installed the state, for `init` to take
/// and start from.
struct HeardSystem(bool);

impl Global for HeardSystem {}

fn state_mut(cx: &mut App) -> Option<&mut MotionState> {
    cx.has_global::<MotionState>()
        .then(|| cx.global_mut::<MotionState>())
}

/// Combine the OS setting with a paused background and window focus.
const fn resolve(system: bool, pause_in_background: bool, active: bool) -> bool {
    system || (pause_in_background && !active)
}

/// Install the motion state and set the flag before the first window opens,
/// so boot animations already honor it.
pub fn init(pause_in_background: bool, cx: &mut App) {
    let system = cx.has_global::<HeardSystem>() && cx.remove_global::<HeardSystem>().0;
    cx.set_global(MotionState { pause_in_background, system, active: true, held: None });
    refresh_system(cx);
    apply(cx);
}

pub fn pause_in_background(cx: &App) -> bool {
    cx.try_global::<MotionState>()
        .is_some_and(|state| state.pause_in_background)
}

pub fn set_pause_in_background(enabled: bool, cx: &mut App) {
    let Some(state) = state_mut(cx) else {
        return;
    };
    if state.pause_in_background == enabled {
        return;
    }
    state.pause_in_background = enabled;
    apply(cx);
    crate::settings::update(crate::settings::SavePolicy::Immediate, cx, |settings| {
        settings.pause_animations_in_background = enabled;
    });
}

/// Fix the flag at `reduced` for good: the e2e socket asks for Reduce Motion so every
/// dump's frame can be judged, and neither the OS's setting, a paused background nor the
/// window becoming active (which reads the OS's again) may undo it.
pub fn hold(reduced: bool, cx: &mut App) {
    match state_mut(cx) {
        Some(state) => {
            state.held = Some(reduced);
            apply(cx);
        }
        None => set_reduced_motion(cx, reduced),
    }
}

/// Main-window focus moved. Regaining focus also re-reads the OS setting where no watch hears it
/// (Windows, Linux): changing it means visiting the system's settings, so coming back is the
/// moment it can have moved. A Mac, iPhone and iPad say each change through the app's watch
/// ([`set_system`]).
pub fn window_activation_changed(active: bool, cx: &mut App) {
    // Only the app installs motion state. Test windows activate too, and the
    // Linux portal read would wake the deterministic scheduler from a D-Bus
    // thread.
    if !cx.has_global::<MotionState>() {
        return;
    }
    if active {
        refresh_system(cx);
    }
    set_window_active(active, cx);
}

fn set_window_active(active: bool, cx: &mut App) {
    let Some(state) = state_mut(cx) else {
        return;
    };
    state.active = active;
    apply(cx);
}

/// The OS's Reduce Motion setting, as the app's watch heard it (`slopty_platform::motion`).
///
/// The shell alone writes GPUI's flag: it follows this, while a paused background and a hold
/// stand whatever the OS says. Heard before [`init`], it is kept for `init` to start from, so
/// the first window's frames already honour it.
pub fn set_system(system: bool, cx: &mut App) {
    let Some(state) = state_mut(cx) else {
        cx.set_global(HeardSystem(system));
        return;
    };
    state.system = system;
    apply(cx);
}

fn apply(cx: &mut App) {
    let Some(state) = cx.try_global::<MotionState>() else {
        return;
    };
    let reduced = state
        .held
        .unwrap_or_else(|| resolve(state.system, state.pause_in_background, state.active));
    // gpui refreshes every window only when the flag actually flips.
    set_reduced_motion(cx, reduced);
}

/// "Show animations in Windows" (Settings → Accessibility → Visual effects).
#[cfg(windows)]
fn refresh_system(cx: &mut App) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut enabled: windows_sys::core::BOOL = 1;
    let read = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
    };
    set_system(read != 0 && enabled == 0, cx);
}

/// The desktop portal's `reduced-motion` key, which GNOME and KDE map from
/// their own animation settings. A D-Bus round trip, so it stays off the
/// foreground; portals without the key read as no preference.
#[cfg(target_os = "linux")]
fn refresh_system(cx: &mut App) {
    use ashpd::desktop::settings::{ReducedMotion, Settings};
    use gpui::AppContext as _;
    let read = cx.background_spawn(async {
        let settings = Settings::new().await.ok()?;
        let motion = settings.reduced_motion().await.ok()?;
        Some(motion == ReducedMotion::ReducedMotion)
    });
    cx.spawn(async move |cx| {
        let reduce = read.await.unwrap_or(false);
        cx.update(|cx| set_system(reduce, cx));
    })
    .detach();
}

/// A Mac, iPhone and iPad have nothing to read here: the app's watch hears Reduce Motion as it
/// changes and says it with [`set_system`].
#[cfg(not(any(windows, target_os = "linux")))]
fn refresh_system(_cx: &mut App) {}

// A line of text that is alive while its work runs (opencode's `text-shimmer`): a band of
// strong ink sweeps across faint glyphs. Its values are opencode's: 45 ms per glyph, a 1200 ms
// cycle, a band 5.2 glyphs wide, linear and endless. The phase is the process's one clock
// ([`locked_phase`]), so every shimmer on screen moves in step.

/// Milliseconds the shimmer's band takes to move one glyph on.
pub const SHIMMER_STEP_MS: f32 = 45.0;
/// One sweep of the shimmer, in milliseconds.
pub const SHIMMER_PERIOD_MS: f32 = 1200.0;
/// The shimmer band's width, in glyphs.
pub const SHIMMER_BAND: f32 = 5.2;

/// How strongly glyph `index` is lit (0 faint, 1 full ink) at `phase_ms` into the cycle: the band
/// reaches each glyph [`SHIMMER_STEP_MS`] after the one before and lights it on a sine's hump.
pub fn shimmer_ink(index: usize, phase_ms: f32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a glyph index in a short title")]
    let delay = index as f32 * SHIMMER_STEP_MS;
    let local = (phase_ms - delay).rem_euclid(SHIMMER_PERIOD_MS);
    let pass = SHIMMER_BAND * SHIMMER_STEP_MS;
    if local >= pass {
        return 0.0;
    }
    (std::f32::consts::PI * local / pass).sin()
}

/// `text` as a shimmering line while `active`: each glyph inked from `theme.text_faint` to
/// `theme.text` as the band crosses it, asking for the next frame meanwhile. Settled, or under
/// reduced motion, it is plain `theme.text_muted`. The caller sets font and size around it.
pub fn shimmer_text(
    text: impl Into<SharedString>,
    active: bool,
    theme: &crate::theme::Theme,
    window: &mut Window,
    cx: &App,
) -> gpui::StyledText {
    let text: SharedString = text.into();
    if !active || reduced_motion(cx) || text.is_empty() {
        let len = text.len();
        return gpui::StyledText::new(text).with_highlights([(0..len, ink_style(theme.text_muted))]);
    }
    window.request_animation_frame();
    let period = Duration::from_secs_f32(SHIMMER_PERIOD_MS / 1000.0);
    let phase_ms = locked_phase(period, window.frame_time()) * SHIMMER_PERIOD_MS;
    let runs = shimmer_runs(&text, phase_ms, theme);
    gpui::StyledText::new(text).with_highlights(runs)
}

// The shimmer's crest passes through the accent hue (HextaUI's `text-shimmer`;
// design-inspiration item 6): faint to the accent over the band's first half, then the accent to
// a crest of the accent mixed toward the text (55 % in the dark, 35 % on light), colour where the
// crest was grey.
/// A shimmering glyph's colour at `ink` (0 unlit, 1 at the crest; [`shimmer_ink`]).
pub fn shimmer_color(ink: f32, theme: &crate::theme::Theme) -> Hsla {
    let toward_text = if theme.appearance.is_dark() { 0.55 } else { 0.35 };
    let crest = mix(theme.accent, theme.text, toward_text);
    if ink <= 0.5 {
        mix(theme.text_faint, theme.accent, ink * 2.0)
    } else {
        mix(theme.accent, crest, ink * 2.0 - 1.0)
    }
}

fn ink_style(color: Hsla) -> gpui::HighlightStyle {
    gpui::HighlightStyle { color: Some(color), ..Default::default() }
}

/// The colour runs of [`shimmer_text`] at `phase_ms`: one per lit glyph ([`shimmer_color`]), the
/// unlit stretches between them merged into one run of the faint ink.
fn shimmer_runs(
    text: &str,
    phase_ms: f32,
    theme: &crate::theme::Theme,
) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
    let faint = theme.text_faint;
    let mut runs = Vec::new();
    let mut unlit: Option<usize> = None;
    for (index, (start, ch)) in text.char_indices().enumerate() {
        let ink = shimmer_ink(index, phase_ms);
        if ink <= 0.0 {
            unlit.get_or_insert(start);
            continue;
        }
        if let Some(from) = unlit.take() {
            runs.push((from..start, ink_style(faint)));
        }
        runs.push((start..start + ch.len_utf8(), ink_style(shimmer_color(ink, theme))));
    }
    if let Some(from) = unlit {
        runs.push((from..text.len(), ink_style(faint)));
    }
    runs
}

#[cfg(test)]
mod tests {

    /// A fold opens its height first and its contents late: at 60 ms the height is well on its
    /// way while the contents are still under 0.2 opacity and below their place; closing, the
    /// contents are gone before the height lands; reduced motion settles at once.
    #[test]
    fn a_fold_opens_its_height_before_its_contents_and_hides_them_first() {
        use std::time::Duration;
        let ms = Duration::from_millis;
        let early = super::fold_frame(true, ms(60), false);
        assert!(early.height > 0.5, "height {}", early.height);
        assert!(early.opacity < 0.2, "contents {}", early.opacity);
        assert!(early.rise > 2.5);
        let open = super::fold_frame(true, ms(300), false);
        assert!(open.height > 0.999 && open.opacity > 0.999 && open.rise < 1e-3, "{open:?}");
        let closing = super::fold_frame(false, ms(150), false);
        assert!(closing.opacity < 1e-3, "{closing:?}");
        assert!(closing.height > 0.05, "the height still landing: {closing:?}");
        assert!(super::fold_frame(false, ms(220), false).height < 1e-3);
        assert_eq!(super::fold_frame(true, ms(0), true).opacity, 1.0);
        assert_eq!(super::fold_frame(false, ms(0), true).height, 0.0);
    }

    /// A hover fade read in a view of its own, which the window draws from its last frame while
    /// nothing notifies it, moves on through the fade: its element asks that view for each
    /// frame until the fade settles, and the view reads the fade where it stands each time.
    #[gpui::test]
    fn a_fade_in_a_view_of_its_own_moves_through_every_frame(cx: &mut gpui::TestAppContext) {
        use std::cell::RefCell;
        use std::rc::Rc;

        use gpui::{AppContext as _, Context, Entity, InteractiveElement as _, Render};

        struct Inner(Rc<RefCell<Vec<f32>>>);
        impl Render for Inner {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.0.borrow_mut().push(hover_t("inner-hover"));
                gpui::div().id("inner").size_full().hover_fade("inner-hover")
            }
        }
        struct Outer(Entity<Inner>);
        impl Render for Outer {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                gpui::div().size_full().child(self.0.clone())
            }
        }

        crate::clock::run_on(cx.executor());
        let read = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view(|_, cx| {
            let inner = cx.new(|_| Inner(Rc::clone(&read)));
            Outer(inner)
        });
        let frame = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
        };
        frame(cx);
        let inner = cx.debug_bounds("inner").unwrap_or_else(|| gpui::Bounds::new(
            gpui::point(gpui::px(10.), gpui::px(10.)),
            gpui::size(gpui::px(10.), gpui::px(10.)),
        ));
        cx.simulate_mouse_move(inner.center(), None, gpui::Modifiers::default());
        frame(cx);
        let start = read.borrow().len();
        for _ in 0..20 {
            cx.executor().advance_clock(Duration::from_millis(16));
            frame(cx);
        }
        let seen: Vec<f32> = read.borrow()[start..].to_vec();
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "on the way in: {seen:?}");
        assert!(seen.iter().any(|t| *t > 0.0 && *t < 1.0), "mid-fade frames drawn: {seen:?}");
        assert_eq!(seen.last().copied(), Some(1.0), "the fade drawn to its end: {seen:?}");
    }

    // Two status animations mounted 300 ms apart, even in two apps (two windows' worth
    // of pulse clocks), report one phase at one frame time.
    #[test]
    fn status_animations_mounted_apart_move_in_unison() {
        let first = process_epoch();
        std::thread::sleep(Duration::from_millis(30));
        let frame = Instant::now() + Duration::from_millis(300);
        assert_eq!(first, process_epoch(), "one clock, whenever each started");
        for spec in [&GRADIENT_SPIN, &SLOPTY_PULSE] {
            let early = locked_phase(spec.total(), frame);
            let late = locked_phase(spec.total(), frame);
            assert_eq!(early, late);
            assert!((0.0..1.0).contains(&early));
        }
        // A period later the phase is back where it was.
        let period = GRADIENT_SPIN.total();
        let now = Instant::now() + Duration::from_secs(5);
        let again = locked_phase(period, now + period);
        assert!((locked_phase(period, now) - again).abs() < 1e-3);
        assert_eq!(locked_phase(Duration::ZERO, now), 0.0);
    }

    #[test]
    fn indicator_slide_is_interruptible_and_snaps_when_reduced() {
        let t0 = Instant::now();
        let mut slide = IndicatorSlide::at(0.0, t0);
        assert!(!slide.animating_at(t0));
        slide.retarget(2.0, false, t0);
        assert_eq!(slide.value_at(t0), 0.0);
        assert!(slide.animating_at(t0 + Duration::from_millis(50)));
        let mid = t0 + Duration::from_millis(80);
        let drawn = slide.value_at(mid);
        assert!(drawn > 0.0 && drawn < 2.0);
        // Reversing continues from where the thumb is drawn.
        slide.retarget(0.0, false, mid);
        assert_eq!(slide.value_at(mid), drawn);
        let end = mid + INDICATOR_SLIDE.total();
        assert_eq!(slide.value_at(end), 0.0);
        assert!(!slide.animating_at(end));
        slide.retarget(1.0, true, end);
        assert_eq!(slide.value_at(end), 1.0);
        assert!(!slide.animating_at(end));
    }
    #[test]
    fn reduced_activity_pulse_is_slow_uniform_and_gentle() {
        for phase in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9] {
            let pulse = ActivityPulse {
                phase,
                subtle: true,
            };
            let opacity = pulse.opacity(0.0, 0.1);
            assert!((0.6..=0.8).contains(&opacity));
            assert_eq!(opacity, pulse.opacity(0.5, 0.1), "no travelling wave");
        }
        let rest = ActivityPulse {
            phase: 0.0,
            subtle: true,
        }
        .opacity(0.0, 0.1);
        let crest = ActivityPulse {
            phase: 0.5,
            subtle: true,
        }
        .opacity(0.0, 0.1);
        assert!(
            crest > rest,
            "the activity cue must visibly change brightness"
        );
    }

    #[gpui::test]
    fn activity_pulse_honors_a_paused_background_and_a_hold(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(|cx| {
            for (system, pause_in_background, active, held, animates) in [
                (false, false, true, None, true),
                (true, false, true, None, true),
                (true, false, false, None, true),
                (true, true, true, None, true),
                (true, true, false, None, false),
                (false, true, false, None, false),
                (false, false, true, Some(true), false),
            ] {
                cx.set_global(MotionState {
                    pause_in_background,
                    system,
                    active,
                    held,
                });
                apply(cx);
                let view = cx.new(|_| ());
                let pulse = activity_pulse(view.entity_id(), cx);
                let leased = cx
                    .try_global::<PulseClock>()
                    .is_some_and(|clock| clock.leases.contains_key(&view.entity_id()));
                assert_eq!(
                    leased, animates,
                    "system={system}, pause={pause_in_background}, active={active}, held={held:?}"
                );
                if reduced_motion(cx) {
                    assert!(pulse.subtle);
                    let decorative_view = cx.new(|_| ());
                    assert_eq!(
                        pulse_delta(&SLOPTY_PULSE, decorative_view.entity_id(), cx),
                        0.0
                    );
                    assert!(!cx.try_global::<PulseClock>().is_some_and(|clock| {
                        clock.leases.contains_key(&decorative_view.entity_id())
                    }));
                }
            }
        });
    }

    /// A held flag (the e2e socket's Reduce Motion) outlasts the window becoming active,
    /// which re-reads the OS's setting, the OS's setting changing, and a paused background.
    #[gpui::test]
    fn a_held_reduce_motion_outlasts_activation_and_the_setting(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(MotionState {
                pause_in_background: true,
                system: false,
                active: true,
                held: None,
            });
            apply(cx);
            assert!(!reduced_motion(cx));
            hold(true, cx);
            assert!(reduced_motion(cx));
            set_window_active(false, cx);
            set_window_active(true, cx);
            assert!(reduced_motion(cx), "activation keeps the held flag");
            set_system(true, cx);
            set_system(false, cx);
            assert!(reduced_motion(cx), "the OS's setting keeps it too");
            set_window_active(false, cx);
            hold(false, cx);
            assert!(!reduced_motion(cx), "the hold wins over a paused background");
        });
    }

    #[gpui::test]
    fn cached_activity_loaders_keep_renewing_under_system_reduced_motion(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{AppContext as _, Context, ParentElement as _, Render};

        struct Loaders;
        impl Render for Loaders {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let theme = crate::theme::Theme::dark();
                gpui::div()
                    .child(crate::loaders::gradient_spinner(
                        "working",
                        &theme,
                        2.5,
                        cx.entity_id(),
                        cx,
                    ))
                    .child(crate::loaders::mini_glyph_spinner(
                        "sidebar-working",
                        2.0,
                        theme.glyph,
                        cx.entity_id(),
                        cx,
                    ))
            }
        }

        cx.update(|cx| {
            cx.set_global(MotionState {
                pause_in_background: false,
                system: true,
                active: true,
                held: None,
            });
            apply(cx);
        });
        let window = cx.add_window(|_, _| Loaders);
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        cx.run_until_parked();
        for _ in 0..20 {
            cx.background_executor.advance_clock(PULSE_TICK);
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
        }
        cx.update(|cx| {
            let clock = cx.global::<PulseClock>();
            assert!(clock.tick >= 20);
            assert_eq!(
                clock.leases.len(),
                2,
                "transcript and cached sidebar loader"
            );
            assert!(
                clock.leases.values().all(|lease| lease.stride == 2),
                "both loaders renew at 15Hz"
            );
        });
    }

    #[gpui::test]
    fn system_reduced_motion_keeps_activity_loaders_ticking(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;

        cx.update(|cx| {
            cx.set_global(MotionState {
                pause_in_background: false,
                system: true,
                active: true,
                held: None,
            });
            apply(cx);
            assert!(reduced_motion(cx));

            let view = cx.new(|_| ());
            drop(crate::loaders::gradient_spinner(
                "working",
                &crate::theme::Theme::dark(),
                2.5,
                view.entity_id(),
                cx,
            ));
            assert!(
                cx.try_global::<PulseClock>()
                    .is_some_and(|clock| clock.leases.contains_key(&view.entity_id())),
                "a working indicator must keep receiving ticks with system reduced motion"
            );
        });
    }

    #[test]
    fn pulse_stride_reestablishes_after_each_paint() {
        let now = Instant::now();
        let mut lease = PulseLease {
            until: now + PULSE_LEASE,
            stride: 2,
        };
        assert!(!lease.take_tick(1), "slow loader skips odd ticks");
        assert!(lease.take_tick(2));
        lease.renew(now, 2);
        lease.renew(now, 1);
        assert!(lease.take_tick(3), "fast animation wins while mounted");
        lease.renew(now, 2);
        assert!(lease.take_tick(4));
        lease.renew(now, 2);
        assert!(
            !lease.take_tick(5),
            "retired fast animation leaves no sticky stride"
        );
        assert!(lease.take_tick(6));
        assert!(!lease.take_tick(7), "unpainted view cannot renew itself");
        assert!(lease.until <= now + PULSE_LEASE);
    }

    #[test]
    fn eval_never_escapes_unit_interval_dense_sweep() {
        // Regression: f32 rounding produced 1.000000119 near the tail of
        // EASE_OUT_EXPO, tripping gpui's `delta ∈ [0,1]` assert (SIGABRT on
        // the user's machine). Sweep densely, including the values right
        // below 1.0 where Newton lands closest to the endpoint.
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_RESORT, EASE_IN_OUT] {
            for i in 0..=100_000u32 {
                let x = i as f32 / 100_000.0;
                let y = curve.eval(x);
                assert!((0.0..=1.0).contains(&y), "eval({x}) = {y} escaped [0,1]");
            }
            for x in [0.999_999f32, 0.999_999_9, 1.0 - f32::EPSILON] {
                let y = curve.eval(x);
                assert!((0.0..=1.0).contains(&y), "eval({x}) = {y} escaped [0,1]");
            }
        }
    }

    use super::*;

    fn assert_close(actual: f32, expected: f32, tol: f32, ctx: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{ctx}: got {actual}, expected {expected} ±{tol}"
        );
    }

    #[test]
    fn bezier_linear_is_identity() {
        let linear = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
        for x in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            assert_close(linear.eval(x), x, 1e-4, "linear");
        }
    }

    #[test]
    fn bezier_known_values() {
        // References computed independently with 80-step bisection.
        let cases: [(&str, CubicBezier, [f32; 5]); 3] = [
            (
                "expo",
                EASE_OUT_EXPO,
                [0.494391, 0.825622, 0.971779, 0.997677, 0.999878],
            ),
            (
                "ease-out",
                EASE_OUT,
                [0.160572, 0.378138, 0.684643, 0.906535, 0.982973],
            ),
            (
                "ease",
                EASE,
                [0.094796, 0.408511, 0.802403, 0.960459, 0.994316],
            ),
        ];
        for (name, curve, expected) in cases {
            for (x, want) in [0.1, 0.25, 0.5, 0.75, 0.9].into_iter().zip(expected) {
                assert_close(curve.eval(x), want, 1e-3, name);
            }
        }
    }

    #[test]
    fn bezier_endpoints_and_clamping() {
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_RESORT, EASE_IN_OUT] {
            assert_eq!(curve.eval(0.0), 0.0);
            assert_eq!(curve.eval(1.0), 1.0);
            assert_eq!(curve.eval(-0.5), 0.0);
            assert_eq!(curve.eval(1.5), 1.0);
        }
    }

    #[test]
    fn bezier_is_monotonic_for_catalog_curves() {
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_RESORT, EASE_IN_OUT] {
            let mut last = 0.0;
            for i in 0..=100 {
                let y = curve.eval(i as f32 / 100.0);
                assert!(y >= last - 1e-4, "monotonicity violated at {i}");
                last = y;
            }
        }
    }

    #[test]
    fn spec_delay_holds_then_runs() {
        // SPLASH_OUT: 150ms delay + 500ms run = 650ms total.
        assert_eq!(SPLASH_OUT.total(), Duration::from_millis(650));
        assert_eq!(SPLASH_OUT.progress(0.0), 0.0);
        // Still inside the delay window at raw 0.2 (130ms < 150ms).
        assert_eq!(SPLASH_OUT.progress(0.2), 0.0);
        // Fully done at the end; clamped beyond.
        assert_eq!(SPLASH_OUT.progress(1.0), 1.0);
        assert_eq!(SPLASH_OUT.progress(2.0), 1.0);
        // Midway through the run: raw 0.65 → 272.5ms into the 500ms run.
        let mid = SPLASH_OUT.progress(0.65);
        assert!(mid > 0.0 && mid < 1.0);
        // No-delay specs pass straight through the curve.
        assert_close(
            FADE_IN.progress(0.5),
            EASE_OUT_EXPO.eval(0.5),
            1e-6,
            "no-delay",
        );
    }

    #[test]
    fn catalog_timings_match_slopty() {
        assert_eq!(FADE_IN.duration_ms, 500);
        assert_eq!(FADE_QUICK.duration_ms, 150);
        assert_eq!(MENU_IN.duration_ms, 140);
        assert_eq!(DIALOG_IN.duration_ms, 180);
        assert_eq!((SPLASH_OUT.duration_ms, SPLASH_OUT.delay_ms), (500, 150));
        assert_eq!(RESIZE.duration_ms, 200);
        assert_eq!(TAB_SLIDE.duration_ms, 150);
        assert_eq!(COLLAPSE.duration_ms, 180);
        assert_eq!(NEW_THREAD_TRANSITION.duration_ms, 420);
        assert_eq!(NEW_THREAD_TRANSITION.curve, EASE_RESORT);
        assert_eq!(CHEVRON.duration_ms, 200);
        assert_eq!(SLOPTY_PULSE.duration_ms, 2400);
        assert_eq!(GRADIENT_SPIN.duration_ms, 750);
        assert_eq!(EASE_OUT_EXPO, CubicBezier::new(0.16, 1.0, 0.3, 1.0));
        assert_eq!(EASE_OUT_QUINT, CubicBezier::new(0.22, 1.0, 0.36, 1.0));
    }

    #[test]
    fn pulse_wave_endpoints() {
        assert_close(pulse_wave(0.0), 0.0, 1e-6, "wave start");
        assert_close(pulse_wave(0.5), 1.0, 1e-6, "wave peak");
        assert_close(pulse_wave(1.0), 0.0, 1e-6, "wave end");
        assert_close(pulse_opacity(0.0), 0.08, 1e-6, "opacity floor");
        assert_close(pulse_opacity(0.5), 1.0, 1e-6, "opacity peak");
        assert_close(pulse_scale(0.0), 0.9, 1e-6, "scale floor");
        assert_close(pulse_scale(0.5), 1.0, 1e-6, "scale peak");
    }

    #[test]
    fn stagger_wraps_and_orders_cells() {
        // Cell 0 at delta 0 is at phase 0; later cells lag by the stagger.
        assert_close(staggered_phase(0.0, 0, PULSE_STAGGER), 0.0, 1e-6, "cell 0");
        assert_close(
            staggered_phase(0.0, 1, PULSE_STAGGER),
            1.0 - PULSE_STAGGER,
            1e-5,
            "cell 1 wraps",
        );
        // A full period later the phase is identical.
        assert_close(
            staggered_phase(0.3, 2, PULSE_STAGGER),
            staggered_phase(0.3 + 1.0, 2, PULSE_STAGGER),
            2e-6,
            "periodic",
        );
        // Matrix wave peaks travel: diagonal k peaks when the front reaches it.
        let peak0 = matrix_wave(0.5, 0, 5);
        assert_close(peak0, 1.0, 1e-5, "diag 0 peak at half period");
    }

    #[test]
    fn lerp_basics() {
        assert_eq!(lerp(208.0, 400.0, 0.0), 208.0);
        assert_eq!(lerp(208.0, 400.0, 1.0), 400.0);
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    #[test]
    fn hover_fade_ramps_and_reverses_continuously() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        // Enter: 0 at the flip, mid-flight strictly between, 1 at 150ms.
        fades.set_at("pill", true, false, t0);
        assert_eq!(fades.value_at("pill", t0), 0.0);
        let mid = fades.value_at("pill", ms(75));
        assert!(mid > 0.0 && mid < 1.0, "mid-flight enter: {mid}");
        assert_eq!(fades.value_at("pill", ms(150)), 1.0);
        assert_eq!(fades.value_at("pill", ms(400)), 1.0, "clamps past the end");

        // Leave mid-flight re-anchors at the current value — no jump.
        fades.set_at("pill", true, false, t0);
        let at_flip = fades.value_at("pill", ms(75));
        fades.set_at("pill", false, false, ms(75));
        let after_flip = fades.value_at("pill", ms(75));
        assert!(
            (after_flip - at_flip).abs() < 1e-4,
            "continuity: {at_flip} vs {after_flip}"
        );
        let falling = fades.value_at("pill", ms(140));
        assert!(falling < after_flip, "fades back down");
        assert_eq!(fades.value_at("pill", ms(225)), 0.0, "lands at rest");
    }

    #[test]
    fn state_morph_snaps_on_mount_and_reverses_continuously() {
        let mut morphs = StateMorphs::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let morph = |m: &mut StateMorphs, on, at| m.value_at("glyph", on, GLYPH_STATE, false, at);

        // First sight lands on the state — no replay at mount.
        assert_eq!(morph(&mut morphs, true, t0), 1.0);
        assert!(!morphs.tick_at(t0));

        // Flip: starts from 1, mid-flight strictly between, lands at 0.
        assert_eq!(morph(&mut morphs, false, ms(10)), 1.0);
        let mid = morph(&mut morphs, false, ms(80));
        assert!(mid > 0.0 && mid < 1.0, "mid-flight: {mid}");
        assert!(morphs.tick_at(ms(80)));

        // Reverse mid-flight re-anchors — no jump — then lands at 1.
        let at_flip = morph(&mut morphs, true, ms(80));
        assert!((at_flip - mid).abs() < 1e-4, "continuity: {mid} vs {at_flip}");
        assert_eq!(morph(&mut morphs, true, ms(400)), 1.0);
        assert!(!morphs.tick_at(ms(400)));

        // Reduced motion snaps a flip.
        assert_eq!(morphs.value_at("glyph", false, GLYPH_STATE, true, ms(500)), 0.0);

        // Unread for a full frame: pruned (a remount snaps again).
        morphs.tick_at(ms(600));
        morphs.tick_at(ms(700));
        assert!(morphs.entries.is_empty());
    }

    #[test]
    fn hover_fade_reduced_motion_snaps() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("row", true, true, t0);
        assert_eq!(fades.value_at("row", t0), 1.0, "enter snaps to 1");
        fades.set_at("row", false, true, t0);
        assert_eq!(fades.value_at("row", t0), 0.0, "leave snaps to 0");
    }

    #[test]
    fn hover_fade_leave_without_enter_is_inert() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("ghost", false, false, t0);
        assert!(fades.entries.is_empty(), "no entry for a leave-only key");
        assert_eq!(fades.value_at("ghost", t0), 0.0);
    }

    #[test]
    fn hover_tick_reports_flight_and_prunes() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        fades.set_at("a", true, false, t0);
        // Mid-flight: active, frames must keep coming (read each frame).
        assert!(fades.tick_at(ms(50)));
        fades.value_at("a", ms(50));
        assert!(fades.tick_at(ms(100)));
        fades.value_at("a", ms(100));
        // Settled hovered (still read): no more frames needed, entry kept.
        assert!(!fades.tick_at(ms(200)));
        fades.value_at("a", ms(200));
        assert_eq!(fades.value_at("a", ms(250)), 1.0);

        // Leave → fades → settles at rest → entry evicted.
        fades.set_at("a", false, false, ms(250));
        assert!(fades.tick_at(ms(300)));
        fades.value_at("a", ms(300));
        assert!(!fades.tick_at(ms(500)), "settled at rest");
        assert!(fades.entries.is_empty(), "rest entries are pruned");
    }

    #[test]
    fn hover_tick_evicts_unread_entries() {
        // An element that unmounts mid-hover never sends its leave — a full
        // frame without a read drops the entry so a remount starts clean.
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        fades.set_at("menu-row", true, false, t0);
        fades.tick_at(ms(16));
        fades.value_at("menu-row", ms(16)); // frame 1: mounted, read
        fades.tick_at(ms(32)); // frame 2: unmounted — no read
        fades.tick_at(ms(48)); // frame 3: a full unread frame has passed
        assert!(fades.entries.is_empty(), "unread entry evicted");
        assert_eq!(fades.value_at("menu-row", ms(64)), 0.0);
    }

    /// A mounted element drawn from its last frame keeps its fade through frames that
    /// read none of it; once the last element behind the key is gone, the fade goes with it.
    #[test]
    fn hover_tick_keeps_a_mounted_entry_unread() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        fades.mount("card");
        fades.mount("card"); // one row moving to another parent lays out twice for a frame
        fades.set_at("card", true, true, t0);
        for frame in 1..=4 {
            fades.tick_at(ms(16 * frame));
        }
        assert!(fades.unread("card"), "no frame read it");
        assert_eq!(fades.value_at("card", ms(80)), 1.0, "still lit when its view is next built");
        fades.unmount("card");
        fades.tick_at(ms(96));
        fades.tick_at(ms(112));
        assert_eq!(fades.value_at("card", ms(112)), 1.0, "one element still stands behind it");
        fades.unmount("card");
        assert!(fades.entries.is_empty() && fades.mounted.is_empty(), "the last one gone");
        fades.unmount("card");
        assert!(fades.mounted.is_empty(), "an unmount past the last is inert");
    }

    #[test]
    fn mix_endpoints_and_transparent_blend() {
        let rest = crate::theme::neutral(0.235);
        let hover = crate::theme::neutral(0.29);
        assert_eq!(mix(rest, hover, 0.0), rest);
        assert_eq!(mix(rest, hover, 1.0), hover);
        assert_eq!(mix(rest, hover, -1.0), rest, "t clamps low");
        assert_eq!(mix(rest, hover, 2.0), hover, "t clamps high");

        // Opaque blend: lightness moves monotonically between the endpoints.
        let mid = mix(rest, hover, 0.5);
        assert!(mid.l > rest.l && mid.l < hover.l, "mid lightness {}", mid.l);

        // Transparent → wash: alpha ramps, hue stays the wash's (premultiplied
        // — never a darkened grey mid-fade). `ink` reads the process-wide
        // appearance, which theme's tests flip — hold the lock so this test
        // never observes a mid-flip Light palette.
        let _guard = crate::theme::lock_appearance();
        let wash = crate::theme::ink(0.06);
        let half = mix(gpui::transparent_black(), wash, 0.5);
        assert!((half.a - 0.03).abs() < 1e-4, "alpha midpoint {}", half.a);
        let half_rgba = Rgba::from(half);
        assert!(
            half_rgba.r > 0.99 && half_rgba.g > 0.99 && half_rgba.b > 0.99,
            "white wash keeps its hue: {half_rgba:?}"
        );
    }

    #[test]
    fn hover_spec_matches_tailwind_transition_colors() {
        assert_eq!(HOVER_FADE.duration_ms, 150);
        assert_eq!(HOVER_FADE.delay_ms, 0);
        assert_eq!(EASE_TAILWIND, CubicBezier::new(0.4, 0.0, 0.2, 1.0));
    }

    #[test]
    fn gspin_pulse_shape() {
        // Full at the cycle start, dim through the rest band, rising at the tail.
        assert_close(gspin_opacity(0.0, 0.1), 1.0, 1e-6, "cycle start");
        assert_close(gspin_opacity(0.45, 0.1), 0.1, 1e-6, "fully dim");
        assert_close(gspin_opacity(0.9, 0.1), 0.1, 1e-6, "rest band");
        assert_close(gspin_opacity(1.0, 0.1), 1.0, 1e-6, "wraps to full");
        let mid_fall = gspin_opacity(0.2, 0.1);
        assert!(mid_fall > 0.1 && mid_fall < 1.0, "eases down");
        let mid_rise = gspin_opacity(0.96, 0.1);
        assert!(mid_rise > 0.1 && mid_rise < 1.0, "eases up");
    }

    #[test]
    fn background_pause_only_applies_when_enabled_and_unfocused() {
        assert!(resolve(false, true, false));
        assert!(!resolve(false, true, true), "focused window animates");
        assert!(!resolve(false, false, false), "opt-in only");
        assert!(resolve(true, false, true), "the system's word stands either way");
    }

    #[gpui::test]
    fn focus_and_the_system_drive_the_global_flag(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(MotionState {
                pause_in_background: false,
                system: false,
                active: true,
                held: None,
            });
            apply(cx);
            assert!(!reduced_motion(cx));

            set_window_active(false, cx);
            assert!(!reduced_motion(cx), "pause is opt-in");
            set_pause_in_background(true, cx);
            assert!(reduced_motion(cx), "unfocused window pauses");
            set_window_active(true, cx);
            assert!(!reduced_motion(cx), "refocus resumes");

            set_system(true, cx);
            assert!(reduced_motion(cx), "the flag follows the OS");
            set_system(false, cx);
            assert!(!reduced_motion(cx));
        });
    }

    /// The shimmer's band reaches each glyph 45 ms after the one before, lights it fully at the
    /// middle of its 234 ms pass, leaves the rest faint, and comes round every 1200 ms; its runs
    /// tile the text with the unlit stretches merged, and its crest passes through the accent.
    #[test]
    fn the_shimmer_sweeps_one_glyph_per_step() {
        let pass = super::SHIMMER_BAND * super::SHIMMER_STEP_MS;
        assert!((super::shimmer_ink(0, pass / 2.0) - 1.0).abs() < 1e-4);
        assert!((super::shimmer_ink(1, pass / 2.0 + 45.0) - 1.0).abs() < 1e-4);
        assert!(super::shimmer_ink(1, pass / 2.0) < 1.0);
        assert_eq!(super::shimmer_ink(0, pass + 1.0), 0.0);
        assert!((super::shimmer_ink(0, pass / 2.0 + 1200.0) - 1.0).abs() < 1e-3);

        let theme = crate::theme::Theme::dark();
        let text = "Running tests";
        let runs = super::shimmer_runs(text, pass / 2.0 + 3.0 * 45.0, &theme);
        let mut end = 0;
        for (range, _) in &runs {
            assert_eq!(range.start, end, "runs tile the text");
            end = range.end;
        }
        assert_eq!(end, text.len());
        assert!(runs.len() < text.len(), "unlit glyphs share a run");
        let crest = super::shimmer_color(1.0, &theme);
        assert!(
            runs.iter().any(|(_, style)| style.color == Some(crest)),
            "a glyph at the crest"
        );
        // The ramp passes through the accent hue, and the crest keeps colour, lighter than it.
        assert_eq!(super::shimmer_color(0.5, &theme), theme.accent);
        assert_eq!(super::shimmer_color(0.0, &theme), theme.text_faint);
        assert!(crest.s > 0.05, "a crest in colour, not grey");
        assert!(crate::theme::contrast(crest, theme.bg) > crate::theme::contrast(theme.accent, theme.bg));
    }

    /// The side pane moves over 240 ms on the quint ease-out; its contents wait 40 ms before
    /// fading in over 200, and are gone 160 ms into a close, before the width lands.
    #[test]
    fn a_pane_moves_on_the_quint_and_its_contents_fade_late_in_early_out() {
        use super::{PANEL_RESIZE, pane_content_opacity};
        let ms = Duration::from_millis;
        assert_eq!((PANEL_RESIZE.total(), PANEL_RESIZE.curve), (ms(240), super::EASE_OUT_QUINT));
        assert_eq!(pane_content_opacity(true, ms(0)), 0.0);
        assert_eq!(pane_content_opacity(true, ms(40)), 0.0, "nothing before the delay");
        let mid = pane_content_opacity(true, ms(140));
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(pane_content_opacity(true, ms(240)), 1.0, "whole as the width lands");
        assert_eq!(pane_content_opacity(false, ms(0)), 1.0);
        assert_eq!(pane_content_opacity(false, ms(160)), 0.0, "gone before the width lands");
    }

    /// A pulse starts in step with the process clock, slows without a jump over the first
    /// minute of its wait, and then runs on the calm clock, in step with every other calm pulse.
    #[test]
    fn a_long_wait_paces_down_without_a_jump_and_lands_in_step() {
        use super::{locked_phase, paced_phase};
        let period = Duration::from_millis(750);
        let gap = |a: f32, b: f32| {
            let d = (a - b).rem_euclid(1.0);
            d.min(1.0 - d)
        };
        let base = super::process_epoch() + Duration::from_secs(1000);
        for offset_ms in [0_u64, 137, 512] {
            let since = base + Duration::from_millis(offset_ms);
            assert!(gap(paced_phase(period, since, Some(since)), locked_phase(period, since)) < 1e-4);
            let calm = period.div_f64(0.6);
            for later in [60.0, 61.3, 3600.0] {
                let now = since + Duration::from_secs_f64(later);
                assert!(
                    gap(paced_phase(period, now, Some(since)), locked_phase(calm, now)) < 1e-3,
                    "{offset_ms} ms in, {later} s on: on the calm clock"
                );
            }
            // No jump: a 5 ms step never moves the phase more than a 5 ms step at full pace.
            let mut last = paced_phase(period, since, Some(since));
            for step in 1..=12_400 {
                let now = since + Duration::from_millis(5 * step);
                let phase = paced_phase(period, now, Some(since));
                assert!(gap(phase, last) <= 5.0 / 750.0 + 1e-4, "a jump at {step}");
                last = phase;
            }
        }
        let now = base + Duration::from_secs(30);
        assert_eq!(paced_phase(period, now, None), locked_phase(period, now));
    }
}

#[cfg(windows)]
#[path = "motion/windows_pulse.rs"]
mod windows_pulse;

// The copy span and the growing section every surface shares.
mod reveal;
pub use reveal::{COPIED_FOR, RevealHeight, copied, copy_feedback, copy_icon, reveal_height};

/// A bounded activation sheen for Fast service tier; GPUI handles reduced motion.
pub fn fast_tier(
    id: impl Into<ElementId>,
    element: impl IntoElement + gpui::Styled + 'static,
) -> impl IntoElement {
    element.with_animation(id, Animation::new(Duration::from_millis(700)), |el, t| {
        el.opacity(1.0 - 0.35 * (1.0 - t) * (std::f32::consts::PI * t).sin())
    })
}
