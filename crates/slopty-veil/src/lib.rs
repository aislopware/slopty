// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Streaming fade veil — per-appended-chunk opacity over already-committed text.
//!
//! The desktop app commits streamed text to layout instantly and dissolves a purely cosmetic
//! veil over the newly arrived characters. This crate is the renderer-agnostic part, shared by
//! the desktop transcript (`slopty-shell`, which recolors GPUI text runs) and the voice call
//! captions on desktop and mobile:
//!
//! - [`ElemVeil`] tracks, per rendered text element, the previously rendered flat text. Each append
//!   registers the new byte range as a chunk with its arrival time; a fast stream keeps several
//!   chunks fading concurrently (a rolling veil). A chunk fades exactly once — already-faded text
//!   never re-animates, and a fully settled element returns no spans at all.
//! - [`CaptionVeil`] veils a live caption shown as the tail of a long utterance, and fades the
//!   previous utterance out when the speaker turn changes.
//!
//! Per-chunk duration adapts to the stream's cadence — an EMA of inter-append gaps,
//! `duration = clamp(ema × 3, 120ms, 400ms)` — the dissolve eases with `veil = (1 − p)^1.6`
//! (text alpha `1 − veil`), and a backed-up stream (3+ concurrent chunks) gets a small speed
//! boost. **Zero translate** — opacity only, per the "no positional offset on streamed content"
//! rule.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod caption;
use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

pub use caption::{CAPTION_EXIT_MS, CaptionFrame, CaptionVeil, caption_tail};

/// EMA seed for the inter-append gap.
pub const VEIL_EMA_SEED_MS: f32 = 160.0;
/// The shortest a chunk's fade lasts, however fast the stream.
pub const VEIL_MIN_FADE_MS: f32 = 120.0;
/// The longest a chunk's fade lasts, however slow the stream.
pub const VEIL_MAX_FADE_MS: f32 = 400.0;
/// Dissolve exponent: the veil is `(1 − p)^1.6`.
pub const VEIL_CURVE_POW: f32 = 1.6;
/// The longest gap the EMA takes in: a pause counts as one second.
const VEIL_GAP_CLAMP_MS: f32 = 1000.0;

/// One appended chunk of text mid-fade.
#[derive(Debug, Clone)]
struct Chunk {
    /// Byte range in the element's flat text.
    range: Range<usize>,
    started: Instant,
    /// Fade duration fixed at arrival from the cadence EMA.
    duration_ms: f32,
}

/// A veiled byte range and its current opacity (0..1).
pub type VeilSpan = (Range<usize>, f32);

/// Text alpha for a fade progress `p` (0..1): the veil dissolves as
/// `(1 − p)^1.6`, so the text shows through at `1 − veil`. Pure.
pub fn veil_opacity(p: f32) -> f32 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powf(VEIL_CURVE_POW)
}

/// Chunk fade duration for the current inter-append EMA: `clamp(ema × 3, MIN, MAX)`.
pub fn veil_duration_ms(ema_ms: f32) -> f32 {
    (ema_ms * 3.0).clamp(VEIL_MIN_FADE_MS, VEIL_MAX_FADE_MS)
}

/// Fast-stream boost: from the third chunk fading concurrently, each one speeds the fades up by
/// 30 %: `1 + 0.3 × max(0, chunks − 2)`.
pub fn veil_boost(active_chunks: usize) -> f32 {
    let extra = u16::try_from(active_chunks.saturating_sub(2)).unwrap_or(u16::MAX);
    0.3_f32.mul_add(f32::from(extra), 1.0)
}

/// EMA update on a new append gap: `ema × 0.7 + min(gap, 1000) × 0.3`.
pub fn veil_ema_next(ema_ms: f32, gap_ms: f32) -> f32 {
    ema_ms.mul_add(0.7, gap_ms.min(VEIL_GAP_CLAMP_MS) * 0.3)
}

/// Per-element chunk tracker: remembers the last rendered flat text and fades
/// every newly appended suffix exactly once.
#[derive(Debug)]
pub struct ElemVeil {
    prev: String,
    chunks: Vec<Chunk>,
    /// EMA of inter-append gaps (drives per-chunk durations).
    ema_ms: f32,
    last_append: Option<Instant>,
}

impl Default for ElemVeil {
    fn default() -> Self {
        Self {
            prev: String::new(),
            chunks: Vec::new(),
            ema_ms: VEIL_EMA_SEED_MS,
            last_append: None,
        }
    }
}

/// Longest common prefix length, snapped back to a char boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    let p = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    b.floor_char_boundary(p)
}

impl ElemVeil {
    /// Adopt `text` as the committed baseline without fading it: content already on screen
    /// when the painter attaches never animates; only later appends do.
    fn seed(&mut self, text: &str) {
        self.prev.clear();
        self.prev.push_str(text);
    }

    /// Advance to `text` at `now`: registers a fading chunk for newly appended
    /// bytes, prunes settled chunks, and returns the active spans. Idempotent
    /// for unchanged text — safe to call once per frame (or twice, when a row
    /// is both measured and painted).
    pub fn advance(&mut self, text: &str, now: Instant) -> Vec<VeilSpan> {
        if text != self.prev {
            // Non-append rewrites (the incremental parser re-deriving a block's
            // flat text — e.g. `**bold` collapsing into a bold run) keep the
            // common prefix's committed fades and re-veil only the changed tail.
            let p = common_prefix(&self.prev, text);
            self.chunks.retain_mut(|c| {
                c.range.end = c.range.end.min(p);
                c.range.start < c.range.end
            });
            if text.len() > p {
                // Cadence-adaptive duration: update the EMA with the gap since the
                // previous append.
                if let Some(last) = self.last_append {
                    let gap = now.saturating_duration_since(last).as_secs_f32() * 1000.0;
                    self.ema_ms = veil_ema_next(self.ema_ms, gap);
                }
                self.last_append = Some(now);
                self.chunks.push(Chunk {
                    range: p..text.len(),
                    started: now,
                    duration_ms: veil_duration_ms(self.ema_ms),
                });
            }
            self.prev.clear();
            self.prev.push_str(text);
        }
        let boost = veil_boost(self.chunks.len());
        self.chunks.retain(|c| {
            let elapsed = now.saturating_duration_since(c.started).as_secs_f32() * 1000.0;
            elapsed * boost < c.duration_ms
        });
        let boost = veil_boost(self.chunks.len());
        self.chunks
            .iter()
            .map(|c| {
                let elapsed = now.saturating_duration_since(c.started).as_secs_f32() * 1000.0;
                let progress = (elapsed * boost / c.duration_ms).clamp(0.0, 1.0);
                (c.range.clone(), veil_opacity(progress))
            })
            .collect()
    }

    /// Any chunk still fading (as of the last [`advance`](Self::advance))?
    pub const fn is_fading(&self) -> bool {
        !self.chunks.is_empty()
    }
}

/// Veil state for one live streaming row, per rendered element.
///
/// Elements are keyed by the render tree's stable per-element discriminator (top-level block
/// ix / nested ix scheme — stable across appends because the incremental parser only reparses
/// the tail).
#[derive(Debug, Default)]
pub struct RowVeil {
    elems: HashMap<usize, ElemVeil>,
    /// Attach pass in progress: elements first seen while seeding adopt their
    /// current text as the committed baseline instead of fading it in. Set for
    /// rows that already carried text when the transcript (re)attached to the
    /// chat — switching back to a streaming session must not dissolve the
    /// whole existing reply.
    seeding: bool,
}

impl RowVeil {
    /// A veil whose first render pass seeds baselines instead of fading.
    pub fn seeded() -> Self {
        Self { elems: HashMap::new(), seeding: true }
    }

    /// The attach pass is over — elements that appear from here on are newly
    /// streamed content and fade normally.
    pub const fn finish_seeding(&mut self) {
        self.seeding = false;
    }

    /// Advance element `elem` to `text` at `now`, as [`ElemVeil::advance`] does, and return its
    /// active spans. An element first seen while seeding adopts `text` as its baseline and
    /// returns none.
    pub fn advance(&mut self, elem: usize, text: &str, now: Instant) -> Vec<VeilSpan> {
        if self.seeding && !self.elems.contains_key(&elem) {
            let mut baseline = ElemVeil::default();
            baseline.seed(text);
            self.elems.insert(elem, baseline);
            return Vec::new();
        }
        self.elems.entry(elem).or_default().advance(text, now)
    }

    /// Any element still fading? Drives the once-per-frame repaint request.
    pub fn is_fading(&self) -> bool {
        self.elems.values().any(ElemVeil::is_fading)
    }
}

/// Intersect spans with `[start, end)` and shift them to local offsets — used
/// by per-line code rendering where chunks are tracked on the whole code text.
pub fn slice_spans(spans: &[VeilSpan], start: usize, end: usize) -> Vec<VeilSpan> {
    spans
        .iter()
        .filter_map(|(r, a)| {
            let s = r.start.max(start);
            let e = r.end.min(end);
            (s < e).then(|| (s.saturating_sub(start)..e.saturating_sub(start), *a))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap()
    }

    #[test]
    fn first_text_fades_and_settles_once() {
        let t0 = Instant::now();
        let mut v = ElemVeil::default();
        let spans = v.advance("hello", t0);
        assert_eq!(spans, vec![(0..5, 0.0)]);
        // Mid-fade: opacity strictly between 0 and 1, range unchanged.
        let spans = v.advance("hello", at(t0, 250));
        assert_eq!(spans.len(), 1);
        assert!(spans[0].1 > 0.0 && spans[0].1 < 1.0);
        // Settled: pruned, never re-animates.
        assert_eq!(v.advance("hello", at(t0, 600)), Vec::<VeilSpan>::new());
        assert!(!v.is_fading());
        assert_eq!(v.advance("hello", at(t0, 700)), Vec::<VeilSpan>::new());
    }

    #[test]
    fn appended_chunks_fade_concurrently_and_independently() {
        let t0 = Instant::now();
        let mut v = ElemVeil::default();
        v.advance("one ", t0);
        let spans = v.advance("one two ", at(t0, 100));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 0..4);
        assert_eq!(spans[1].0, 4..8);
        // The older chunk is further along its fade than the newer one.
        assert!(spans[0].1 > spans[1].1);
        // After the first settles (past its adaptive duration), only the
        // newer chunk remains.
        let spans = v.advance("one two ", at(t0, 410));
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, 4..8);
    }

    #[test]
    fn fade_cadence() {
        // EMA seed 160 ms, fades between 120 and 400 ms, veil (1 − p)^1.6,
        // ema = ema × 0.7 + min(gap, 1000) × 0.3, boost = 1 + 0.3 × max(0, n − 2).
        // duration = clamp(ema × 3, 120, 400).
        assert_eq!(veil_duration_ms(160.0), 400.0); // seed → clamped at max
        assert_eq!(veil_duration_ms(30.0), 120.0); // fast stream → floor
        assert_eq!(veil_duration_ms(60.0), 180.0);
        // EMA update: a gap past a second counts as one second.
        assert!((veil_ema_next(160.0, 100.0) - 142.0).abs() < 1e-4);
        assert!((veil_ema_next(160.0, 5000.0) - 412.0).abs() < 1e-4);
        // Fast-stream boost kicks in at the 3rd concurrent chunk.
        assert_eq!(veil_boost(0), 1.0);
        assert_eq!(veil_boost(2), 1.0);
        assert!((veil_boost(3) - 1.3).abs() < 1e-6);
        assert!((veil_boost(5) - 1.9).abs() < 1e-6);
    }

    #[test]
    fn seeded_row_adopts_existing_text_without_fading() {
        // Switching back to a streaming session: the text already on screen is
        // the attach BASELINE — no full-reply fade.
        let t0 = Instant::now();
        let mut row = RowVeil::seeded();
        assert_eq!(row.advance(0, "already streamed text", t0), Vec::<VeilSpan>::new());
        assert_eq!(row.advance(1, "second block", t0), Vec::<VeilSpan>::new());
        assert!(!row.is_fading());
        // Appends AFTER the attach fade normally — only the new suffix.
        let spans = row.advance(0, "already streamed text plus", at(t0, 100));
        assert_eq!(spans, vec![(21..26, 0.0)]);
        // The attach pass ends after the first render: elements first seen
        // later are newly streamed content and fade from empty.
        row.finish_seeding();
        let spans = row.advance(2, "new block", at(t0, 200));
        assert_eq!(spans, vec![(0..9, 0.0)]);
    }

    #[test]
    fn default_row_veil_fades_first_text() {
        // Mid-stream new rows keep the old behavior: their first chunk fades.
        let t0 = Instant::now();
        let mut row = RowVeil::default();
        let spans = row.advance(0, "fresh", t0);
        assert_eq!(spans, vec![(0..5, 0.0)]);
    }

    #[test]
    fn faded_text_never_reanimates_on_append() {
        let t0 = Instant::now();
        let mut v = ElemVeil::default();
        v.advance("stable", t0);
        // Fully settled...
        assert_eq!(v.advance("stable", at(t0, 600)), Vec::<VeilSpan>::new());
        // ...then an append veils ONLY the new suffix.
        let spans = v.advance("stable more", at(t0, 700));
        assert_eq!(spans, vec![(6..11, 0.0)]);
    }

    #[test]
    fn non_append_rewrite_keeps_prefix_and_reveils_tail() {
        let t0 = Instant::now();
        let mut v = ElemVeil::default();
        v.advance("intro **bol", t0);
        // Markdown resolves: flat text loses the `**` marker.
        let spans = v.advance("intro bold", at(t0, 100));
        // The shared prefix's chunk is clamped, the changed tail is one chunk.
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 0..6);
        assert_eq!(spans[1].0, 6..10);
        assert!(spans[0].1 > spans[1].1);
    }

    #[test]
    fn common_prefix_respects_char_boundaries() {
        // "é" = 0xC3 0xA9, "è" = 0xC3 0xA8 — byte prefix would split the char.
        assert_eq!(common_prefix("é", "è"), 0);
        assert_eq!(common_prefix("abé", "abè"), 2);
        assert_eq!(common_prefix("same", "same"), 4);
    }

    #[test]
    fn veil_opacity_curve_endpoints() {
        // Text alpha = 1 - (1-p)^1.6: 0 at arrival, 1 when the veil is gone.
        assert_eq!(veil_opacity(0.0), 0.0);
        assert_eq!(veil_opacity(1.0), 1.0);
        let mid = veil_opacity(0.5);
        assert!(mid > 0.0 && mid < 1.0);
        // The pow-1.6 ease-out reveals faster than linear early on.
        assert!(mid > 0.5);
        assert!((mid - (1.0 - 0.5_f32.powf(1.6))).abs() < 1e-6);
        // Monotonic + clamped.
        assert!(veil_opacity(0.2) <= veil_opacity(0.4));
        assert_eq!(veil_opacity(-1.0), 0.0);
        assert_eq!(veil_opacity(2.0), 1.0);
    }

    #[test]
    fn slice_spans_shifts_to_local_offsets() {
        let spans = vec![(3..10, 0.4), (12..20, 0.1)];
        // A "line" covering bytes 5..15.
        let local = slice_spans(&spans, 5, 15);
        assert_eq!(local, vec![(0..5, 0.4), (7..10, 0.1)]);
        // Disjoint window → empty.
        assert_eq!(slice_spans(&spans, 25, 30), Vec::<VeilSpan>::new());
    }

    #[test]
    fn row_veil_tracks_elements_independently() {
        let t0 = Instant::now();
        let mut row = RowVeil::default();
        row.advance(0, "para", t0);
        row.advance(2, "code", t0);
        assert!(row.is_fading());
        row.advance(0, "para", at(t0, 600));
        assert!(row.is_fading(), "elem 2 hasn't been advanced past its fade");
        row.advance(2, "code", at(t0, 600));
        assert!(!row.is_fading());
    }
}
