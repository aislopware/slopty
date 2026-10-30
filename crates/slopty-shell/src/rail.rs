// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The transcript's jump to a row, gliding (zeron's `MessageRail` without its rail: slopty).
//!
//! TUI-first, the transcript is the inspector's timeline, a side pane narrower than
//! the 48rem the rail of prompt ticks needed, so the rail went (`docs/decisions/ui.md`, "A
//! session is its terminal, its inspector beside it"). What stays is the jump it made, which
//! find and the held group's press take: a glide to a row on one eased timeline, landing marked.

use gpui::{Context, ListOffset, px};

use crate::motion;
use crate::transcript::Transcript;

/// How far below the list's top a row a jump lands on stands (slopty).
///
/// Past the scroll edge the transcript fades under at the top of its pane, at the inset a sent
/// prompt is held at. The first row carries its own gap.
pub const fn landing_inset(row: usize) -> f32 {
    if row == 0 { 0.0 } else { crate::transcript::OWN_SEND_TOP_INSET_PX }
}

/// A jump's glide under way, stepped once a display frame (#13).
pub(crate) struct Glide {
    /// The row it lands on, and how far under the viewport's top.
    target: usize,
    land: f32,
    /// Whether the landing flashes the row.
    flash: bool,
    started: std::time::Instant,
    timeline: GlideTimeline,
    /// The rows' height estimate where they were not measured.
    height_ema: Option<f32>,
    /// The frames it has taken, against a glide that never lands.
    steps: usize,
}

/// The most frames a glide takes before it lands exactly: [`motion::SCROLL_GLIDE`] at
/// 120 Hz and room to spare, at the motion's own speed ([`glide_max_steps`]).
const GLIDE_MAX_STEPS: u16 = 240;

/// [`GLIDE_MAX_STEPS`] stretched by [`motion::speed_scale`], so a slowed motion glides
/// its whole length rather than jumping at the bound.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "240 frames at least (a NaN scale counts as 1) and capped at u16::MAX first"
)]
fn glide_max_steps() -> usize {
    let steps = (f32::from(GLIDE_MAX_STEPS) * motion::speed_scale().max(1.0))
        .ceil()
        .min(f32::from(u16::MAX));
    usize::from(steps as u16)
}

/// A row index or a count of rows in item space, as an `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24 (16.7 million), which no transcript's rows near"
)]
const fn count(rows: usize) -> f32 {
    rows as f32
}

/// The whole row at or before `position` in item space, `last` at most.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "floored and kept at 0 or more; past usize the cast saturates, and `last` bounds it"
)]
fn row_at(position: f32, last: usize) -> usize {
    (position.floor().max(0.0) as usize).min(last)
}

// ---------------------------------------------------------------------------
// Duration-based glide timeline (pure)
// ---------------------------------------------------------------------------

/// Duration-based scroll glide (browser smooth-scroll parity: the Electron
/// rail used `scrollToItem({behavior:"smooth"})` — a fixed-duration gentle
/// ease over the WHOLE distance, never percent-of-remaining).
///
/// Rows above the viewport are unmeasured, so the total pixel distance can
/// only be ESTIMATED per frame. The timeline therefore hands out each frame's
/// movement as a fraction of whatever distance currently remains:
/// `(e_now − e_prev) / (1 − e_prev)` for eased progress `e`. With a stable
/// estimate this telescopes to exactly `start + e(t)·total` — the fixed eased
/// timeline. When the estimate changes mid-flight (a row got measured, the
/// bottom-aligned layout re-glued an anchor), the SAME timeline simply
/// continues over the corrected remainder — no restart, no compensating jump.
#[derive(Debug, Clone, Copy)]
pub struct GlideTimeline {
    eased_prev: f32,
}

impl Default for GlideTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl GlideTimeline {
    /// A timeline at its start.
    pub const fn new() -> Self {
        Self { eased_prev: 0.0 }
    }

    /// Fraction of the CURRENT remaining distance to consume for eased
    /// progress `eased` (monotone, `0..=1`; `1.0` lands exactly).
    pub fn step(&mut self, eased: f32) -> f32 {
        let eased = eased.clamp(self.eased_prev, 1.0);
        let denom = 1.0 - self.eased_prev;
        let frac = if denom <= 1e-6 { 1.0 } else { (eased - self.eased_prev) / denom };
        self.eased_prev = eased;
        frac.clamp(0.0, 1.0)
    }
}

/// `SLOPTY_SCROLL_TRACE=1` logs per-frame glide positions at `warn` level —
/// the smoothness measurement knob (same family as `SLOPTY_FRAME_STATS`).
fn scroll_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("SLOPTY_SCROLL_TRACE").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

// ---------------------------------------------------------------------------
// Smooth scroll (Transcript extension)
// ---------------------------------------------------------------------------

impl Transcript {
    /// Smooth-scroll the list so `target` sits at the viewport top, where it lands with a brief
    /// flash (slopty, #13); any running stick/jump animation yields.
    pub fn scroll_to_row(&mut self, target: usize, cx: &mut Context<Self>) {
        self.glide_to_row(target, true, cx);
    }

    /// Smooth-scroll the list so `target` sits at the viewport top; `flash` marks where it
    /// landed (a find's step has its own mark, its match).
    ///
    /// A [`motion::SCROLL_GLIDE`] (500ms ease-in-out) timeline drives every
    /// frame's position; per-frame movement comes from the timeline, never
    /// from a percent of the remaining distance:
    ///
    /// - a glued bottom anchor (`item_ix == len`, one viewport BELOW the visible top) is first
    ///   materialized as the true viewport-top anchor — stepping straight from the glued anchor
    ///   lands inside the re-glue band and layout undoes it every frame (the old stall→double-jump
    ///   path);
    /// - rows above the viewport are unmeasured, so the anchor glides in item space along the same
    ///   timeline, estimating sub-row offsets from a local row-height EMA; the position is read
    ///   back each frame, so a measurement correcting the estimate just re-enters the timeline;
    /// - once the target row is measured the glide is pixel-exact.
    ///
    /// It steps once a display frame (`Transcript::step_glide`, from the render's
    /// next-frame callback, as the stick spring does), not on a 16 ms timer, so a 120 Hz
    /// display glides at 120 Hz (#13).
    pub fn glide_to_row(&mut self, target: usize, flash: bool, cx: &mut Context<Self>) {
        self.begin_scroll_navigation();
        // The prompt lands below the titlebar the transcript fades under, where a sent
        // one is parked, not behind it; the first row carries its own titlebar gap.
        let land = landing_inset(target);
        if motion::reduced_motion(cx) {
            self.list_state().scroll_to(ListOffset { item_ix: target, offset_in_item: px(-land) });
            cx.notify();
            return;
        }
        self.set_glide(Some(Glide {
            target,
            land,
            flash,
            started: crate::clock::now(cx),
            timeline: GlideTimeline::new(),
            height_ema: None,
            steps: 0,
        }));
        cx.notify();
    }

    /// One frame of the glide under way: its position on the timeline at this frame's moment,
    /// and at its end the landing (and its flash).
    pub(crate) fn step_glide(&mut self, cx: &mut Context<Self>) {
        let Some(mut glide) = self.take_glide() else {
            return;
        };
        let (target, land) = (glide.target, glide.land);
        let total = motion::SCROLL_GLIDE.total().mul_f32(motion::speed_scale());
        let now = crate::clock::now(cx);
        let raw = (now.saturating_duration_since(glide.started).as_secs_f32()
            / total.as_secs_f32())
        .min(1.0);
        glide.steps = glide.steps.saturating_add(1);
        let list = self.list_state().clone();
        // The timeline runs out (it shouldn't, on frames): land exactly.
        if raw >= 1.0 || glide.steps > glide_max_steps() {
            list.scroll_to(ListOffset { item_ix: target, offset_in_item: px(-land) });
            if glide.flash {
                self.flash_landing(target, cx);
            }
            cx.notify();
            return;
        }
        let eased = motion::SCROLL_GLIDE.curve.eval(raw);
        let frac = glide.timeline.step(eased);
        // Materialize the glued bottom representation as the true
        // top anchor (same visual position, sticky anchor).
        let viewport = f32::from(list.viewport_bounds().size.height);
        if self.is_glued() && viewport > 0.0 {
            list.scroll_by(px(-(viewport + 0.5)));
        }
        let top = list.logical_scroll_top();
        let top_height =
            list.bounds_for_item(top.item_ix).map(|b| f32::from(b.size.height).max(1.0));
        // Row-height estimate for unmeasured territory: the mean
        // over the whole visible span, recomputed per frame (the
        // ~dozen mixed row kinds in a viewport average out — a
        // single-row estimate whipsaws between paragraphs and
        // code blocks and modulates the per-frame step visibly).
        if viewport > 0.0 {
            let bottom = f32::from(list.viewport_bounds().bottom());
            let mut ix = top.item_ix;
            let mut rows = 0.0_f32;
            while let Some(b) = list.bounds_for_item(ix) {
                if f32::from(b.top()) >= bottom {
                    break;
                }
                rows += 1.0;
                ix = ix.saturating_add(1);
            }
            if rows > 0.0 {
                let mean = viewport / rows;
                let ema = glide.height_ema.get_or_insert(mean);
                *ema = f32::midpoint(*ema, mean);
            }
        }
        if glide.height_ema.is_none() {
            glide.height_ema = top_height;
        }
        let height_ema = glide.height_ema;
        // Where the viewport top actually is, in fractional item
        // space — read back per frame (self-correcting: an anchor
        // the layout adjusted or re-glued keeps its real remaining
        // distance and continues the same timeline).
        let here = count(top.item_ix)
            + top_height.map_or(0.0, |h| (f32::from(top.offset_in_item) / h).clamp(0.0, 1.0));
        if scroll_trace_enabled() {
            tracing::warn!(
                ms = u64::try_from(now.saturating_duration_since(glide.started).as_millis())
                    .unwrap_or(u64::MAX),
                eased,
                here,
                dist = self.distance_from_bottom(),
                "scroll-glide"
            );
        }
        self.set_glide(Some(glide));
        if target < top.item_ix {
            // Above the viewport (unmeasured): progressive
            // item-space anchoring within the eased timeline.
            let next = frac.mul_add(count(target) - here, here);
            // Small steps ride `scroll_by` — the list keeps a
            // 320px measured leading overdraw, so a step that
            // fits inside it crosses rows at their TRUE heights
            // (pixel-exact frames through the gentle start and
            // landing, where jitter would show most).
            let step_px = (here - next) * height_ema.unwrap_or(0.0);
            if step_px > 0.0 && step_px <= crate::transcript::OVERDRAW_PX * 0.8 {
                list.scroll_by(px(-step_px));
                cx.notify();
                return;
            }
            let ix = row_at(next, top.item_ix);
            let within = next - count(ix);
            let offset = if ix == top.item_ix {
                // Same row as the current anchor: measured height,
                // pixel-exact — and never below the current offset,
                // so motion stays monotone even when a height
                // estimate was corrected.
                top_height.map_or(0.0, |h| (within * h).min(f32::from(top.offset_in_item)))
            } else {
                within * height_ema.unwrap_or(0.0)
            };
            list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(offset) });
            cx.notify();
            return;
        }
        if let Some(bounds) = list.bounds_for_item(target) {
            // Measured: pixel-exact step along the timeline.
            let delta = f32::from(bounds.top() - list.viewport_bounds().top()) - land;
            list.scroll_by(px(frac * delta));
        } else {
            // Below but unmeasured: item space, same timeline.
            let next = frac.mul_add(count(target) - here, here);
            let ix = row_at(next, target);
            let within = next - count(ix);
            list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(within * height_ema.unwrap_or(0.0)),
            });
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Consuming `(e'−e)/(1−e)` of the current remainder telescopes to exactly
    /// the absolute eased timeline `start + e(t)·total` when the distance
    /// estimate is stable — the glide is timeline-driven, not
    /// percent-of-remaining.
    #[test]
    fn glide_timeline_matches_absolute_eased_interpolation() {
        let curve = motion::SCROLL_GLIDE.curve;
        let mut timeline = GlideTimeline::new();
        let (start, target) = (1000.0_f32, 0.0_f32);
        let mut pos = start;
        for i in 1_u8..=60 {
            let t = f32::from(i) / 60.0;
            let eased = curve.eval(t);
            let frac = timeline.step(eased);
            pos = frac.mul_add(target - pos, pos);
            let absolute = eased.mul_add(target - start, start);
            assert!((pos - absolute).abs() < 0.05, "frame {i}: pos {pos} != absolute {absolute}");
        }
        assert_eq!(pos, target); // eased hits 1.0 → frac 1.0 → exact landing.
    }

    /// A mid-flight distance re-estimate (anchor re-glued / row measured)
    /// continues the SAME timeline over the corrected remainder: no restart,
    /// no compensating jump, exact landing.
    #[test]
    fn glide_timeline_survives_remaining_distance_reestimate() {
        let curve = motion::SCROLL_GLIDE.curve;
        let mut timeline = GlideTimeline::new();
        let mut pos = 500.0_f32;
        let mut prev_frac = 0.0_f32;
        for i in 1_u8..=60 {
            let t = f32::from(i) / 60.0;
            let frac = timeline.step(curve.eval(t));
            if i == 30 {
                // The layout re-glued the anchor: remaining distance doubles.
                pos *= 2.0;
            }
            pos = (-frac).mul_add(pos, pos);
            // Fractions depend only on the timeline — the re-estimate cannot
            // make a step consume a larger share than the curve dictates.
            assert!((0.0..=1.0).contains(&frac));
            if i > 1 && i < 55 {
                assert!(frac >= prev_frac - 0.05, "frame {i}: frac regressed");
            }
            prev_frac = frac;
        }
        assert_eq!(pos, 0.0);
    }

    /// Timeline steps clamp: regressions in eased input yield zero movement,
    /// and completion always yields the full remainder.
    #[test]
    fn glide_timeline_step_clamps() {
        let mut timeline = GlideTimeline::new();
        assert_eq!(timeline.step(0.4), 0.4);
        assert_eq!(timeline.step(0.3), 0.0); // non-monotone input → no move
        assert_eq!(timeline.step(1.0), 1.0); // done → land exactly
        assert_eq!(timeline.step(1.0), 1.0); // idempotent at the end
    }

    /// The first 16ms frame of the 500ms glide covers under 2% of the
    /// distance — no first-frame majority jump by construction.
    #[test]
    fn glide_first_frame_is_gentle() {
        let spec = motion::SCROLL_GLIDE;
        assert_eq!(spec.duration_ms, 500);
        let first = spec.curve.eval(16.0 / 500.0);
        assert!(first < 0.02, "first frame covered {first} of the distance");
        // And the ease-in-out midpoint is exactly half the distance.
        let mid = spec.curve.eval(0.5);
        assert!((mid - 0.5).abs() < 0.01);
    }
}
