//! Speculative local echo, after mosh.
//!
//! A printable key pressed at a shell prompt almost always ends up on screen at the cursor. On a
//! slow link the client draws it immediately as a *prediction*, then reconciles against the next
//! authoritative frame: `Frame::input_ack` says which keys the worker had applied when the frame
//! was captured, so every acknowledged prediction is checked cell-for-cell. Hits raise
//! confidence; one miss clears the overlay and mutes prediction for a while.
//!
//! Visibility is adaptive: predictions are only drawn when the link is slow enough for them to
//! matter and the recent track record is clean, so a LAN session never sees a wrong glyph.
//!
//! The predictor is pure: no clocks, no I/O. Callers pass `now`.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use slopty_grid::{Cursor, Screen, TermModes};
use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};

/// When to draw predictions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Policy {
    /// Never draw (still tracks hits for diagnostics).
    Never,
    /// Draw when the link is slow and predictions have been confirmed recently.
    #[default]
    Adaptive,
    /// Always draw (testing, very slow links).
    Always,
}

/// Link RTT above which predictions are worth drawing at all.
pub const SLOW_LINK: Duration = Duration::from_millis(25);
/// RTT above which predictions draw without waiting for a confirmed hit.
pub const VERY_SLOW_LINK: Duration = Duration::from_millis(120);
/// A prediction unconfirmed for this long is treated as a miss.
pub const STALE: Duration = Duration::from_millis(1500);
/// After a miss, stay quiet for this long.
pub const MUTE: Duration = Duration::from_secs(2);
/// Hits needed before drawing on a merely slow link.
pub const WARMUP_HITS: u32 = 2;
/// Most predictions kept in flight; beyond this we stop guessing.
pub const MAX_PENDING: usize = 64;

/// One predicted cell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prediction {
    /// The key `seq` this came from.
    pub seq: u64,
    /// Screen row.
    pub row: u16,
    /// Column.
    pub col: u16,
    /// The glyph.
    pub text: String,
    /// When it was made.
    pub at: Instant,
}

/// What a reconcile step found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Reconciled {
    /// Predictions confirmed by this frame.
    pub hits: u32,
    /// Predictions contradicted (the overlay was cleared).
    pub misses: u32,
    /// Predictions still waiting.
    pub pending: usize,
}

/// The predictor.
#[derive(Clone, Debug)]
pub struct Predictor {
    policy: Policy,
    pending: VecDeque<Prediction>,
    rtt: Option<Duration>,
    hits: u32,
    total_hits: u64,
    total_misses: u64,
    muted_until: Option<Instant>,
    epoch: Option<u32>,
}

impl Default for Predictor {
    fn default() -> Self {
        Self::new(Policy::default())
    }
}

impl Predictor {
    /// A predictor with `policy`.
    #[must_use]
    pub const fn new(policy: Policy) -> Self {
        Self {
            policy,
            pending: VecDeque::new(),
            rtt: None,
            hits: 0,
            total_hits: 0,
            total_misses: 0,
            muted_until: None,
            epoch: None,
        }
    }

    /// Change the policy.
    pub const fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
    }

    /// Latest smoothed RTT from the transport.
    pub const fn set_rtt(&mut self, rtt: Option<Duration>) {
        self.rtt = rtt;
    }

    /// Lifetime hit / miss counts.
    #[must_use]
    pub const fn stats(&self) -> (u64, u64) {
        (self.total_hits, self.total_misses)
    }

    /// Predictions in flight, oldest first.
    #[must_use]
    pub const fn pending(&self) -> &VecDeque<Prediction> {
        &self.pending
    }

    /// Whether the overlay should be drawn right now.
    ///
    /// A guess older than [`STALE`] is never drawn, even while no frame has come to count it
    /// as a miss: a link that went quiet must not leave a guess on screen that the worker never
    /// confirmed.
    #[must_use]
    pub fn visible(&self, now: Instant) -> bool {
        let Some(oldest) = self.pending.front() else { return false };
        if now.saturating_duration_since(oldest.at) > STALE {
            return false;
        }
        match self.policy {
            Policy::Never => false,
            Policy::Always => true,
            Policy::Adaptive => {
                if self.muted_until.is_some_and(|until| now < until) {
                    return false;
                }
                let Some(rtt) = self.rtt else { return false };
                rtt >= VERY_SLOW_LINK || (rtt >= SLOW_LINK && self.hits >= WARMUP_HITS)
            }
        }
    }

    /// The cursor as it should be drawn: after the last pending prediction on the cursor row.
    #[must_use]
    pub fn cursor(&self, real: Cursor) -> Cursor {
        match self.pending.back() {
            Some(p) if p.row == real.row => Cursor { col: p.col.saturating_add(1), ..real },
            _ => real,
        }
    }

    /// A key is about to be sent. Returns the prediction made, if any.
    ///
    /// `cursor`/`modes`/`cols` describe the authoritative screen *plus* earlier predictions
    /// (use [`Self::cursor`]).
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        cursor: Cursor,
        cols: u16,
        modes: TermModes,
        now: Instant,
    ) -> Option<Prediction> {
        if self.policy == Policy::Never {
            return None;
        }
        if key.action == KeyAction::Release {
            return None;
        }
        // Erasing: a backspace takes back our own last prediction, and nothing more.
        if key.code == KeyCode::Backspace && key.mods.is_empty() {
            let _taken = self.pending.pop_back();
            return None;
        }
        if !safe_modes(modes) || !cursor.visible {
            self.flush();
            return None;
        }
        let text = printable(key)?;
        if self.pending.len() >= MAX_PENDING {
            return None;
        }
        let predicted = self.cursor(cursor);
        // Never predict a wrap; the shell may or may not autowrap the prompt.
        if predicted.col.saturating_add(1) >= cols {
            return None;
        }
        let p = Prediction { seq: key.seq, row: predicted.row, col: predicted.col, text, at: now };
        self.pending.push_back(p.clone());
        Some(p)
    }

    /// An authoritative frame was applied to `screen`. `input_ack` and `epoch` come from the
    /// frame. Checks every acknowledged prediction against the screen.
    pub fn on_frame(
        &mut self,
        screen: &Screen,
        input_ack: u64,
        epoch: u32,
        now: Instant,
    ) -> Reconciled {
        let mut out = Reconciled::default();
        let previous = self.epoch.replace(epoch);
        if previous.is_some_and(|e| e != epoch) {
            // Numbering changed (alt screen, reset, reflow): guesses are meaningless.
            self.flush();
            return out;
        }
        while let Some(front) = self.pending.front() {
            if front.seq > input_ack {
                break;
            }
            let Some(front) = self.pending.pop_front() else { break };
            let cell_text = screen
                .line(front.row)
                .and_then(|line| line.cells.get(usize::from(front.col)))
                .map(|cell| cell.text.as_str().to_owned());
            if cell_text.as_deref() == Some(front.text.as_str()) {
                out.hits = out.hits.saturating_add(1);
                self.hits = self.hits.saturating_add(1);
                self.total_hits = self.total_hits.saturating_add(1);
            } else {
                out.misses = out.misses.saturating_add(1);
                self.miss(now);
                break;
            }
        }
        // Anything unacknowledged for too long counts as a miss too.
        if self.pending.front().is_some_and(|p| now.duration_since(p.at) > STALE) {
            out.misses = out.misses.saturating_add(1);
            self.miss(now);
        }
        out.pending = self.pending.len();
        out
    }

    /// Drop every prediction (resize, detach, focus loss).
    pub fn flush(&mut self) {
        self.pending.clear();
    }

    fn miss(&mut self, now: Instant) {
        self.total_misses = self.total_misses.saturating_add(1);
        self.hits = 0;
        self.muted_until = now.checked_add(MUTE);
        self.pending.clear();
    }
}

/// Modes in which typing echoes at the cursor.
const fn safe_modes(modes: TermModes) -> bool {
    !modes.contains(TermModes::ALT_SCREEN)
        && !modes.contains(TermModes::ECHO_OFF)
        && !modes.contains(TermModes::CURSOR_HIDDEN)
        && !modes.contains(TermModes::MOUSE_TRACKING)
}

/// The text a key would echo, if it is a plain printable character.
fn printable(key: &KeyEvent) -> Option<String> {
    if key.mods.intersects(Mods::CTRL | Mods::SUPER) {
        return None;
    }
    let text = key.text.as_deref()?;
    let mut chars = text.chars();
    let c = chars.next()?;
    if chars.next().is_some() || c.is_control() || c == '\t' {
        return None;
    }
    // Wide glyphs occupy two cells; keep it to single-width text.
    if !c.is_ascii() {
        return None;
    }
    Some(text.to_owned())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{Cell, CursorShape, Line, RowUpdate};

    use super::*;

    fn key(seq: u64, text: &str) -> KeyEvent {
        KeyEvent {
            seq,
            action: KeyAction::Press,
            code: KeyCode::A,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            text: Some(text.to_owned()),
            unshifted: text.chars().next(),
            composing: false,
            option_as_alt: false,
        }
    }

    fn cursor(row: u16, col: u16) -> Cursor {
        Cursor { row, col, shape: CursorShape::Block, visible: true, blink: false }
    }

    fn screen_with(row: u16, text: &str) -> Screen {
        let mut screen = Screen::new(80, 24);
        let mut line = Line::blank(80);
        for (i, ch) in text.chars().enumerate() {
            if let Some(cell) = line.cells.get_mut(i) {
                *cell = Cell::narrow(ch, slopty_grid::Style::DEFAULT);
            }
        }
        screen.apply(RowUpdate { row, line }).unwrap();
        screen
    }

    #[test]
    fn predicts_printables_and_advances_cursor() {
        let mut p = Predictor::new(Policy::Always);
        let now = Instant::now();
        let a = p.on_key(&key(1, "a"), cursor(3, 5), 80, TermModes::CANONICAL, now).unwrap();
        assert_eq!((a.row, a.col, a.text.as_str()), (3, 5, "a"));
        let b = p.on_key(&key(2, "b"), cursor(3, 5), 80, TermModes::CANONICAL, now).unwrap();
        assert_eq!((b.row, b.col), (3, 6));
        assert_eq!(p.cursor(cursor(3, 5)).col, 7);
        assert!(p.visible(now));
        // Backspace retracts the last guess only.
        let mut bs = key(3, "");
        bs.code = KeyCode::Backspace;
        bs.text = None;
        assert!(p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).is_none());
        assert_eq!(p.pending().len(), 1);
    }

    #[test]
    fn hits_confirm_and_misses_mute() {
        let mut p = Predictor::new(Policy::Adaptive);
        p.set_rtt(Some(Duration::from_millis(60)));
        let now = Instant::now();
        let _a = p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::CANONICAL, now);
        let _b = p.on_key(&key(2, "b"), cursor(0, 0), 80, TermModes::CANONICAL, now);
        assert!(!p.visible(now), "no hits yet on a merely slow link");
        // Worker applied key 1 only: 'a' landed.
        let r = p.on_frame(&screen_with(0, "a"), 1, 0, now);
        assert_eq!(r, Reconciled { hits: 1, misses: 0, pending: 1 });
        let _c = p.on_key(&key(3, "c"), cursor(0, 1), 80, TermModes::CANONICAL, now);
        let r = p.on_frame(&screen_with(0, "ab"), 2, 0, now);
        assert_eq!(r.hits, 1);
        assert!(p.visible(now), "two hits: warm");
        // Worker disagrees on key 3 (say the shell rejected it).
        let r = p.on_frame(&screen_with(0, "ab"), 3, 0, now);
        assert_eq!((r.hits, r.misses, r.pending), (0, 1, 0));
        assert!(!p.visible(now));
        let later = now + MUTE + Duration::from_millis(1);
        let _d = p.on_key(&key(4, "d"), cursor(0, 2), 80, TermModes::CANONICAL, later);
        assert!(!p.visible(now), "muted after a miss");
        assert!(!p.visible(later), "after the mute a slow link must re-warm");
        p.set_rtt(Some(VERY_SLOW_LINK));
        assert!(p.visible(later), "a very slow link draws without warm-up");
        assert_eq!(p.stats(), (2, 1));
    }

    #[test]
    fn unsafe_modes_and_wraps_refuse() {
        let mut p = Predictor::new(Policy::Always);
        let now = Instant::now();
        assert!(p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::ALT_SCREEN, now).is_none());
        assert!(p.on_key(&key(2, "a"), cursor(0, 0), 80, TermModes::ECHO_OFF, now).is_none());
        assert!(p.on_key(&key(3, "a"), cursor(0, 79), 80, TermModes::empty(), now).is_none());
        let mut ctrl = key(4, "c");
        ctrl.mods = Mods::CTRL;
        assert!(p.on_key(&ctrl, cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.on_key(&key(5, "漢"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.on_key(&key(6, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_some());
        // Epoch change wipes guesses.
        let _r = p.on_frame(&Screen::new(80, 24), 0, 1, now);
        let r = p.on_frame(&Screen::new(80, 24), 0, 2, now);
        assert_eq!(r.pending, 0);
    }

    /// With no frame arriving to reconcile it, a guess past [`STALE`] stops being drawn.
    #[test]
    fn a_stale_guess_is_hidden_without_a_frame() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        let _a = p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::empty(), t0);
        assert!(p.visible(t0 + STALE), "at the limit it still shows");
        assert!(!p.visible(t0 + STALE + Duration::from_millis(1)), "past it, hidden");
    }

    #[test]
    fn stale_predictions_count_as_misses() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        let _a = p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::empty(), t0);
        let r = p.on_frame(&Screen::new(80, 24), 0, 0, t0 + STALE + Duration::from_millis(1));
        assert_eq!((r.misses, r.pending), (1, 0));
    }
}
