//! Speculative local echo, after mosh.
//!
//! A printable key pressed at a shell prompt almost always ends up on screen at the cursor. On a
//! slow link the client draws it immediately as a *prediction*, then reconciles against the next
//! authoritative frame: `Frame::input_ack` says which keys the worker had applied when the frame
//! was captured, so every acknowledged prediction is checked cell-for-cell. Hits raise
//! confidence; one miss clears the overlay and mutes prediction for a while. An acknowledged
//! guess whose cell still shows what it covered is not a miss: the frame was cut from a read
//! that held other output, and the echo is still to come.
//!
//! Visibility is adaptive: predictions are only drawn when the link is slow enough for them to
//! matter and the recent track record is clean, so a LAN session never sees a wrong glyph.
//!
//! Any key that is not a plain printable one (Enter, an arrow, a control chord, ⌥ as Alt) moves
//! the cursor where the predictor cannot follow: the guesses are dropped and none is made until
//! the worker acknowledges that key, so the next one lands where the cursor really is. Input the
//! predictor never sees (raw bytes, a paste) does the same through [`Predictor::interrupt`]. And
//! after any of them the guesses are *tentative*, as in mosh: made and checked but not drawn
//! until one is confirmed by the worker's echo, so the prompt Enter led to shows nothing typed
//! unless it echoes (a password prompt never does).
//!
//! The predictor is pure: no clocks, no I/O. Callers pass `now`.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_grid::{CellText, Cursor, Line, Screen, TermModes};
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
    /// What each pending guess covers on the worker's screen, in step with `pending`; `None`
    /// when no frame had shown that cell yet.
    covered: VecDeque<Option<CellText>>,
    /// The cursor's row as the last frame left it: where the next guess lands.
    cursor_line: Option<(u16, Arc<Line>)>,
    rtt: Option<Duration>,
    hits: u32,
    total_hits: u64,
    total_misses: u64,
    muted_until: Option<Instant>,
    epoch: Option<u32>,
    /// The highest key the worker has acknowledged.
    acked: u64,
    /// A key the predictor could not follow: nothing is guessed until the worker acknowledges
    /// it, when the frames show where the cursor went.
    barrier: Option<u64>,
    /// Input the predictor did not see went out: the next key is a barrier, whatever it is.
    interrupted: bool,
    /// Guesses are made and checked but not drawn until one is confirmed.
    tentative: bool,
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
            covered: VecDeque::new(),
            cursor_line: None,
            rtt: None,
            hits: 0,
            total_hits: 0,
            total_misses: 0,
            muted_until: None,
            epoch: None,
            acked: 0,
            barrier: None,
            interrupted: false,
            // What the screen is waiting for when the view opens is unknown: it may be a
            // password prompt.
            tentative: true,
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
        if self.tentative {
            return false;
        }
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
        if std::mem::take(&mut self.interrupted) {
            self.hold_until(key.seq);
            return None;
        }
        // Erasing: a backspace takes back our own last prediction, and nothing more. With no
        // guess to take back it erases the shell's text, which is not ours to follow.
        if key.code == KeyCode::Backspace && key.mods.is_empty() && !self.pending.is_empty() {
            let _taken = self.pending.pop_back();
            let _uncovered = self.covered.pop_back();
            return None;
        }
        let Some(text) = printable(key) else {
            self.hold_until(key.seq);
            return None;
        };
        if self.barrier.is_some_and(|barrier| self.acked < barrier) {
            return None;
        }
        if !modes.prediction_allowed() || !cursor.visible {
            self.flush();
            return None;
        }
        if self.pending.len() >= MAX_PENDING {
            return None;
        }
        let predicted = self.cursor(cursor);
        // Never predict a wrap; the shell may or may not autowrap the prompt.
        if predicted.col.saturating_add(1) >= cols {
            return None;
        }
        let p = Prediction { seq: key.seq, row: predicted.row, col: predicted.col, text, at: now };
        let covered = self
            .cursor_line
            .as_ref()
            .filter(|(row, _)| *row == p.row)
            .and_then(|(_, line)| line.cells.get(usize::from(p.col)))
            .map(|cell| cell.text.clone());
        self.pending.push_back(p.clone());
        self.covered.push_back(covered);
        Some(p)
    }

    /// An authoritative frame was applied to `screen`. `input_ack` and `epoch` come from the
    /// frame. Checks every acknowledged prediction against the screen.
    ///
    /// The worker acknowledges every key written before the read a frame was cut from, and that
    /// read may hold other output (a spinner, a build) instead of the echo. So a guess whose
    /// cell still shows what it covered stays pending, until the echo lands or [`STALE`]; only
    /// a cell showing something else is a miss.
    pub fn on_frame(
        &mut self,
        screen: &Screen,
        input_ack: u64,
        epoch: u32,
        now: Instant,
    ) -> Reconciled {
        let mut out = Reconciled::default();
        self.acked = self.acked.max(input_ack);
        let cursor = screen.cursor();
        self.cursor_line =
            screen.lines().get(usize::from(cursor.row)).map(|line| (cursor.row, Arc::clone(line)));
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
            let cell = screen
                .line(front.row)
                .and_then(|line| line.cells.get(usize::from(front.col)))
                .map(|cell| &cell.text);
            if cell.is_some_and(|text| text.as_str() == front.text) {
                let _confirmed = self.pending.pop_front();
                let _uncovered = self.covered.pop_front();
                self.tentative = false;
                out.hits = out.hits.saturating_add(1);
                self.hits = self.hits.saturating_add(1);
                self.total_hits = self.total_hits.saturating_add(1);
            } else if cell.is_some() && cell == self.covered.front().and_then(Option::as_ref) {
                break;
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
        self.covered.clear();
    }

    /// Input went out that the predictor does not see (raw bytes, a paste): the guesses are
    /// dropped, the next key waits to be acknowledged before any other is guessed, and what
    /// follows is tentative.
    pub fn interrupt(&mut self) {
        self.flush();
        self.interrupted = true;
    }

    /// Key `seq` moved the cursor where no guess can follow: nothing is guessed until the
    /// worker acknowledges it, and what follows is tentative.
    fn hold_until(&mut self, seq: u64) {
        self.flush();
        self.barrier = Some(seq);
        self.tentative = true;
    }

    fn miss(&mut self, now: Instant) {
        self.total_misses = self.total_misses.saturating_add(1);
        self.hits = 0;
        self.muted_until = now.checked_add(MUTE);
        self.flush();
    }
}

/// The text a key would echo, if it is a plain printable character. A ⌥ that is Alt makes
/// the key a chord (`ESC b` is a word back), whatever the text beside it.
fn printable(key: &KeyEvent) -> Option<String> {
    if key.mods.intersects(Mods::CTRL | Mods::SUPER) || key.option_as_alt {
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

    /// A key with no text: Enter, an arrow.
    fn special(seq: u64, code: KeyCode) -> KeyEvent {
        KeyEvent { code, text: None, unshifted: None, ..key(seq, "") }
    }

    /// Get past the first tentative stretch: a guess at the bottom row, echoed. Leaves the
    /// predictor at seq 1 acknowledged.
    fn confirmed(p: &mut Predictor, now: Instant) {
        let _guess = p.on_key(&key(1, "w"), cursor(23, 0), 80, TermModes::empty(), now);
        assert_eq!(p.on_frame(&screen_with(23, "w"), 1, 0, now).hits, 1);
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
        confirmed(&mut p, now);
        let a = p.on_key(&key(2, "a"), cursor(3, 5), 80, TermModes::CANONICAL, now).unwrap();
        assert_eq!((a.row, a.col, a.text.as_str()), (3, 5, "a"));
        let b = p.on_key(&key(3, "b"), cursor(3, 5), 80, TermModes::CANONICAL, now).unwrap();
        assert_eq!((b.row, b.col), (3, 6));
        assert_eq!(p.cursor(cursor(3, 5)).col, 7);
        assert!(p.visible(now));
        // Backspace retracts the last guess only.
        let bs = special(4, KeyCode::Backspace);
        assert!(p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).is_none());
        assert_eq!(p.pending().len(), 1);
        assert!(p.visible(now), "still shown");
        // With no guess left to take back it erases the shell's text: nothing follows it.
        let _taken =
            p.on_key(&special(5, KeyCode::Backspace), cursor(3, 5), 80, TermModes::CANONICAL, now);
        let bs = special(6, KeyCode::Backspace);
        assert!(p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).is_none());
        assert!(p.on_key(&key(7, "c"), cursor(3, 5), 80, TermModes::CANONICAL, now).is_none());
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
        // Worker disagrees on key 3 (say the shell mapped it to another character).
        let r = p.on_frame(&screen_with(0, "abX"), 3, 0, now);
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
        let hidden = Cursor { visible: false, ..cursor(0, 0) };
        assert!(p.on_key(&key(4, "a"), hidden, 80, TermModes::empty(), now).is_none());
        assert!(p.on_key(&key(5, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_some());
        let mut ctrl = key(6, "c");
        ctrl.mods = Mods::CTRL;
        assert!(p.on_key(&ctrl, cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.pending().is_empty(), "a chord drops the guesses");
        assert!(p.on_key(&key(7, "漢"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.on_key(&key(8, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
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
        confirmed(&mut p, t0);
        let _a = p.on_key(&key(2, "a"), cursor(0, 0), 80, TermModes::empty(), t0);
        assert!(p.visible(t0 + STALE), "at the limit it still shows");
        assert!(!p.visible(t0 + STALE + Duration::from_millis(1)), "past it, hidden");
    }

    /// A frame that acknowledges a key but was cut from a read of other output (a spinner on
    /// another row) leaves the guess's cell as it was: the guess waits for its echo, and
    /// prediction stays on; a cell showing something else is still a miss.
    #[test]
    fn background_output_does_not_mute_prediction() {
        let mut p = Predictor::new(Policy::Adaptive);
        p.set_rtt(Some(Duration::from_millis(60)));
        let t0 = Instant::now();
        let mut shown = screen_with(0, "$ ");
        shown.cursor_mut().col = 2;
        let _r = p.on_frame(&shown, 0, 0, t0);
        for (seq, text, col, echoed) in [(1, "a", 2, "$ a"), (2, "b", 3, "$ ab")] {
            let _guess = p.on_key(&key(seq, text), cursor(0, col), 80, TermModes::CANONICAL, t0);
            let mut echo = screen_with(0, echoed);
            echo.cursor_mut().col = col + 1;
            assert_eq!(p.on_frame(&echo, seq, 0, t0).hits, 1, "{text} echoed");
        }
        let _c = p.on_key(&key(3, "c"), cursor(0, 4), 80, TermModes::CANONICAL, t0);
        assert!(p.visible(t0), "warm");

        // The spinner's read carries the ack; the echo has not been read yet.
        let mut spun = screen_with(0, "$ ab");
        spun.cursor_mut().col = 4;
        spun.apply(RowUpdate {
            row: 3,
            line: screen_with(3, "⠋ building").line(3).unwrap().clone(),
        })
        .unwrap();
        let later = t0 + Duration::from_millis(40);
        let r = p.on_frame(&spun, 3, 0, later);
        assert_eq!(r, Reconciled { hits: 0, misses: 0, pending: 1 }, "not echoed yet, not a miss");
        assert!(p.visible(later), "prediction stays on");

        let r = p.on_frame(&screen_with(0, "$ abc"), 3, 0, later + Duration::from_millis(5));
        assert_eq!((r.hits, r.misses, r.pending), (1, 0, 0), "the echo lands");

        let _d = p.on_key(&key(4, "d"), cursor(0, 5), 80, TermModes::CANONICAL, later);
        let r = p.on_frame(&screen_with(0, "$ abcX"), 4, 0, later);
        assert_eq!(r.misses, 1, "a cell showing something else is a miss");

        let quiet = later + MUTE + Duration::from_millis(1);
        let _e = p.on_key(&key(5, "e"), cursor(0, 6), 80, TermModes::CANONICAL, quiet);
        let r =
            p.on_frame(&screen_with(0, "$ abcX"), 5, 0, quiet + STALE + Duration::from_millis(1));
        assert_eq!(r.misses, 1, "an echo that never comes is a miss once stale");
        assert_eq!(p.stats(), (3, 2));
    }

    /// After Enter the next prompt may not echo (a password): what is typed there is guessed
    /// and checked, never drawn, however long it goes on; once a guess is echoed again the
    /// guesses show.
    #[test]
    fn a_prompt_after_enter_shows_nothing_typed_until_it_echoes() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        confirmed(&mut p, t0);
        assert!(p.on_key(&key(2, "a"), cursor(0, 0), 80, TermModes::CANONICAL, t0).is_some());
        assert!(p.visible(t0));
        assert!(
            p.on_key(&special(3, KeyCode::Enter), cursor(0, 1), 80, TermModes::CANONICAL, t0)
                .is_none()
        );
        assert!(p.pending().is_empty() && !p.visible(t0), "Enter drops and hides");
        assert!(
            p.on_key(&key(4, "s"), cursor(0, 1), 80, TermModes::CANONICAL, t0).is_none(),
            "Enter not acknowledged yet: the cursor is somewhere unknown"
        );
        let mut asked = screen_with(1, "Password:");
        asked.cursor_mut().row = 1;
        asked.cursor_mut().col = 9;
        let _r = p.on_frame(&asked, 4, 0, t0);
        for (seq, text) in (5..).zip(["h", "u", "n", "t", "e", "r", "2"]) {
            let at = p.cursor(asked.cursor());
            assert!(p.on_key(&key(seq, text), at, 80, TermModes::CANONICAL, t0).is_some());
            let _r = p.on_frame(&asked, seq, 0, t0);
            assert!(!p.visible(t0), "{text}: guessed, never drawn");
        }
        let later = t0 + STALE + Duration::from_millis(1);
        let r = p.on_frame(&asked, 11, 0, later);
        assert_eq!(r.misses, 1, "no echo came: a miss");
        assert!(!p.visible(later));

        let mut prompt = screen_with(2, "$ ");
        prompt.cursor_mut().row = 2;
        prompt.cursor_mut().col = 2;
        let _r = p.on_frame(&prompt, 11, 0, later);
        let _l = p.on_key(&key(12, "l"), cursor(2, 2), 80, TermModes::CANONICAL, later);
        assert!(!p.visible(later), "still tentative");
        let r = p.on_frame(&screen_with(2, "$ l"), 12, 0, later);
        assert_eq!(r.hits, 1);
        let quiet = later + MUTE;
        let _s = p.on_key(&key(13, "s"), cursor(2, 3), 80, TermModes::CANONICAL, quiet);
        assert!(p.visible(quiet), "an echo confirmed: shown again");
    }

    /// An arrow, a chord, ⌥ as Alt, or input the predictor never saw moves the cursor out of
    /// its sight: the guesses go, and the next ones wait until the worker has that key, then
    /// land where the frame put the cursor.
    #[test]
    fn a_key_it_cannot_follow_pauses_guessing_until_acknowledged() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        confirmed(&mut p, t0);
        let modes = TermModes::CANONICAL;
        let _a = p.on_key(&key(2, "a"), cursor(0, 5), 80, modes, t0);
        assert!(p.on_key(&special(3, KeyCode::ArrowLeft), cursor(0, 5), 80, modes, t0).is_none());
        assert!(p.pending().is_empty());
        assert!(p.on_key(&key(4, "b"), cursor(0, 6), 80, modes, t0).is_none(), "waits for 3");
        let mut moved = screen_with(0, "a");
        moved.cursor_mut().col = 3;
        let _r = p.on_frame(&moved, 3, 0, t0);
        let c = p.on_key(&key(5, "c"), moved.cursor(), 80, modes, t0).expect("guessed again");
        assert_eq!(c.col, 3, "where the frame put the cursor");

        let mut word_back = key(6, "b");
        word_back.mods = Mods::ALT;
        word_back.option_as_alt = true;
        assert!(p.on_key(&word_back, cursor(0, 4), 80, modes, t0).is_none(), "ESC b, not b");
        assert!(p.pending().is_empty());
        let _r = p.on_frame(&moved, 6, 0, t0);
        let mut at = key(7, "@");
        at.mods = Mods::ALT;
        assert!(p.on_key(&at, cursor(0, 3), 80, modes, t0).is_some(), "⌥ typing a symbol");

        p.interrupt();
        assert!(p.pending().is_empty());
        assert!(p.on_key(&key(8, "d"), cursor(0, 3), 80, modes, t0).is_none(), "after raw bytes");
        assert!(p.on_key(&key(9, "e"), cursor(0, 3), 80, modes, t0).is_none(), "8 not acked");
        let _r = p.on_frame(&moved, 8, 0, t0);
        assert!(p.on_key(&key(10, "f"), cursor(0, 3), 80, modes, t0).is_some());
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
