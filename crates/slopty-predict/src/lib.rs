//! Speculative local echo, after mosh.
//!
//! A printable key pressed at a shell prompt almost always ends up on screen at the cursor. On a
//! slow link the client draws it immediately as a *prediction*, then reconciles against the next
//! authoritative frame: `Frame::input_ack` says which keys the worker had applied when the frame
//! was captured, so every acknowledged guess is checked against it. Hits raise confidence; one
//! miss clears the overlay and mutes prediction for a while. An acknowledged guess whose cells
//! still show what they showed before it is not a miss: the frame was cut from a read that held
//! other output, and the echo is still to come.
//!
//! At a shell's input row, where OSC 133 says the typed command starts, the line-editing keys are
//! guessed too, as mosh guesses them: ← and → move the cursor within the typed text, ⌫ deletes
//! the character before the cursor and pulls the rest of the text left, and a key typed inside the
//! text pushes the rest right. None of them is guessed past the input's start, and → is not
//! guessed at the end of the text, where a shell's suggestion (drawn in grey after it) would take
//! it. Without the OSC 133 mark, ⌫ still takes back what the predictor itself typed on the row.
//!
//! Visibility is adaptive: predictions are only drawn when the round trip is at least half a
//! display refresh, where a guess reaches the glass a frame ahead of the echo on most keys, and
//! the recent track record is clean, so a LAN session never sees a wrong glyph.
//!
//! A drawn guess looks like the text it continues, so a key looks final as soon as it is
//! pressed, as it does in a local terminal. It is *marked* (drawn apart from the worker's text)
//! only when the guess is less certain, as mosh flags its predictions: on a link of
//! [`MARK_LINK`] or more, while a guess has waited over [`GLITCH`] for its echo, and until
//! [`GLITCH_REPAIR`] guesses in a row have been echoed promptly after a slow one or a miss.
//!
//! The guesses run in *epochs*, as mosh's do. Any other key (Enter, Esc, ↑ ↓, a control chord,
//! ⌥ as Alt) moves the cursor where the predictor cannot follow: the guesses are dropped and none
//! is made until the worker acknowledges that key, so the next one lands where the cursor really
//! is. Input the predictor never sees (raw bytes, a paste) does the same through
//! [`Predictor::interrupt`]. The new epoch is *tentative*: its guesses are made and checked but
//! not drawn until one is confirmed by the worker's echo, so the prompt Enter led to shows nothing
//! typed unless it echoes (a password prompt never does). A miss ends the epoch the same way. A
//! miss in a tentative epoch showed nothing, so it does not mute the predictor.
//!
//! The predictor is pure: no clocks, no I/O. Callers pass `now`.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_grid::{
    Cell, CellText, CellWidth, Color, Cursor, Line, LineFlags, Screen, Style, StyleFlags, TermModes,
};
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

/// The display's refresh period until the caller says otherwise ([`Predictor::set_refresh`]).
///
/// 60 Hz, the slowest panel Slopty draws on, so an unknown display never guesses on a link a
/// faster one would not.
pub const DEFAULT_REFRESH: Duration = Duration::from_nanos(16_666_667);
/// RTT above which predictions draw without waiting for a confirmed hit.
pub const VERY_SLOW_LINK: Duration = Duration::from_millis(120);
/// A prediction unconfirmed for this long is treated as a miss.
pub const STALE: Duration = Duration::from_millis(1500);
/// After a miss, stay quiet for this long.
pub const MUTE: Duration = Duration::from_secs(2);
/// Hits needed before drawing on a merely slow link.
pub const WARMUP_HITS: u32 = 2;
/// Most keys kept in flight; beyond this we stop guessing.
pub const MAX_PENDING: usize = 64;
/// RTT from which drawn guesses are marked (mosh's `FLAG_TRIGGER_HIGH`)…
pub const MARK_LINK: Duration = Duration::from_millis(80);
/// …until the RTT falls under this (mosh's `FLAG_TRIGGER_LOW`), so a link near the line does
/// not flip the look from key to key.
pub const UNMARK_LINK: Duration = Duration::from_millis(50);
/// A guess not echoed within this is a glitch: guesses are marked (mosh's `GLITCH_THRESHOLD`).
pub const GLITCH: Duration = Duration::from_millis(250);
/// Guesses echoed within [`GLITCH`] in a row that end the marking after a glitch or a miss
/// (mosh's `GLITCH_REPAIR_COUNT`).
pub const GLITCH_REPAIR: u32 = 10;
/// Blank cells in a row that part the typed text from a right prompt.
pub const TEXT_GAP: u16 = 2;
/// A right prompt ends within this many columns of the row's end (zsh leaves one blank,
/// `ZLE_RPROMPT_INDENT`).
pub const RIGHT_PROMPT_EDGE: u16 = 2;

/// One predicted cell.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prediction {
    /// The key `seq` this came from.
    pub seq: u64,
    /// Screen row.
    pub row: u16,
    /// Column.
    pub col: u16,
    /// The glyph; a space where a key takes text away.
    pub text: String,
    /// When it was made.
    pub at: Instant,
}

/// What a reconcile step found.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Reconciled {
    /// Keys whose guesses this frame confirmed.
    pub hits: u32,
    /// Guesses contradicted (the overlay was cleared).
    pub misses: u32,
    /// Keys still waiting.
    pub pending: usize,
}

/// What a key does to the input line, as the predictor models it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stroke {
    /// A printable character, inserted at the cursor.
    Type(char),
    /// ←: the cursor one character left.
    Left,
    /// →: the cursor one character right.
    Right,
    /// ⌫: the character before the cursor deleted.
    Erase,
}

/// One cell a key changes.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Change {
    col: u16,
    cell: Cell,
    check: Check,
}

/// What the echo of a changed cell must show to confirm the guess.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    /// Nothing: a blank is no evidence (mosh: "too easy for this to trigger falsely", and a
    /// suggestion may fill it), nor is text the cell already showed.
    Nothing,
    /// The text.
    Text,
    /// The text, drawn as typed rather than in a suggestion's grey: a key typed over the
    /// suggestion's own next character.
    Typed,
}

/// A key in flight and what it is expected to do.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Guess {
    seq: u64,
    at: Instant,
    /// The cursor's column once the key is echoed.
    cursor: u16,
    /// The key moves the cursor other than by typing, so the echo's cursor is evidence.
    moves: bool,
    changes: Vec<Change>,
}

impl Guess {
    /// The echo can confirm or refute it.
    fn evidence(&self) -> bool {
        self.moves || self.changes.iter().any(|c| c.check != Check::Nothing)
    }
}

/// A row as the last frame showed it.
#[derive(Clone, Debug)]
struct Base {
    row: u16,
    line: Arc<Line>,
    /// The row below continues this one (a soft wrap): text pulled or pushed along would cross.
    wrapped_below: bool,
}

/// A right prompt as a frame showed it.
#[derive(Clone, Debug)]
struct RightPrompt {
    row: u16,
    start: u16,
    text: Vec<CellText>,
}

impl RightPrompt {
    /// Where it starts on row `row` reading `cells`, when the row shows it there again, after
    /// a blank. zsh draws its right prompt one blank from a line that has grown to it.
    fn on(&self, row: u16, cells: &[Cell]) -> Option<u16> {
        let start = usize::from(self.start);
        let shown = cells.get(start..start.checked_add(self.text.len())?)?;
        let before = cells.get(start.checked_sub(1)?)?;
        let same = shown.iter().zip(&self.text).all(|(cell, text)| cell.text == *text);
        (row == self.row && blank(before) && same).then_some(self.start)
    }
}

/// The predictor.
#[derive(Clone, Debug)]
pub struct Predictor {
    policy: Policy,
    /// Keys in flight with a guess, oldest first.
    guesses: VecDeque<Guess>,
    /// The row they edit.
    row: u16,
    /// What each cell the guesses touch showed before the first of them; `None` where no frame
    /// had shown the row.
    origin: Vec<(u16, Option<CellText>)>,
    /// The cursor's column before the first of them.
    origin_cursor: u16,
    /// The cells the guesses draw, left to right.
    drawn: VecDeque<Prediction>,
    /// The row the next guess edits, as the last frame left it.
    base: Option<Base>,
    /// The right prompt last found on the input row, kept so that it is known again when the
    /// line comes within one blank of it.
    right: Option<RightPrompt>,
    /// The row and leftmost column the predictor has typed at in this epoch: without an OSC 133
    /// mark, ⌫ never goes left of it.
    floor: Option<(u16, u16)>,
    rtt: Option<Duration>,
    /// The link is slow enough that drawn guesses are marked ([`MARK_LINK`]).
    slow_marks: bool,
    /// Prompt echoes still owed before guesses stop being marked after a glitch or a miss.
    unsure: u32,
    /// The refresh period of the display the guesses are drawn on.
    refresh: Duration,
    hits: u32,
    muted_until: Option<Instant>,
    /// The frames' line numbering.
    numbering: Option<u32>,
    /// The highest key the worker has acknowledged.
    acked: u64,
    /// The highest key the predictor has been shown.
    sent: u64,
    /// A key the predictor could not follow: nothing is guessed until the worker acknowledges
    /// it, when the frames show where the cursor went.
    barrier: Option<u64>,
    /// Input the predictor did not see went out: the next key is a barrier, whatever it is.
    interrupted: bool,
    /// The epoch is unconfirmed: guesses are made and checked but not drawn until one is.
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
            guesses: VecDeque::new(),
            row: 0,
            origin: Vec::new(),
            origin_cursor: 0,
            drawn: VecDeque::new(),
            base: None,
            floor: None,
            right: None,
            rtt: None,
            slow_marks: false,
            unsure: 0,
            refresh: DEFAULT_REFRESH,
            hits: 0,
            muted_until: None,
            numbering: None,
            acked: 0,
            sent: 0,
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
    pub fn set_rtt(&mut self, rtt: Option<Duration>) {
        self.rtt = rtt;
        match rtt {
            Some(rtt) if rtt >= MARK_LINK => self.slow_marks = true,
            Some(rtt) if rtt < UNMARK_LINK => self.slow_marks = false,
            Some(_) => {}
            None => self.slow_marks = false,
        }
    }

    /// The refresh period of the display the guesses are drawn on (13.3 ms at 75 Hz).
    pub const fn set_refresh(&mut self, period: Duration) {
        self.refresh = period;
    }

    /// The round trip from which guesses are drawn on a warmed-up link: half a refresh.
    ///
    /// A guess is painted as the key is pressed and its echo a round trip later, so the echo
    /// misses the refresh the guess is shown in on about `rtt / refresh` of the keys, and the
    /// gain is about the round trip on average. Through a shaped link (MEASUREMENTS, "the
    /// prediction threshold over a shaped link") a guess was on the glass 7, 11, 15 and 20 ms
    /// ahead of the echo at 5, 10, 15 and 20 ms, with no misses. On loopback it gains 1 to
    /// 4 ms, and each key then draws a frame of its own with its echo a refresh behind. From
    /// half a refresh on, most keys gain a whole frame.
    #[must_use]
    pub fn slow_link(&self) -> Duration {
        self.refresh.checked_div(2).unwrap_or(self.refresh)
    }

    /// The cells the guesses in flight draw, left to right: a typed glyph, text an edit moved,
    /// a space where a key took text away. Each carries the key that last changed it.
    #[must_use]
    pub const fn pending(&self) -> &VecDeque<Prediction> {
        &self.drawn
    }

    /// Whether the overlay (the cells and the cursor) should be drawn right now.
    ///
    /// A guess older than [`STALE`] is never drawn, even while no frame has come to count it
    /// as a miss: a link that went quiet must not leave a guess on screen that the worker never
    /// confirmed.
    #[must_use]
    pub fn visible(&self, now: Instant) -> bool {
        if self.tentative {
            return false;
        }
        let Some(oldest) = self.guesses.front() else { return false };
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
                rtt >= VERY_SLOW_LINK || (rtt >= self.slow_link() && self.hits >= WARMUP_HITS)
            }
        }
    }

    /// Whether the guesses drawn now are marked apart from the worker's text: the link is slow,
    /// the oldest guess has waited past [`GLITCH`], or a glitch or a miss has not yet been
    /// followed by [`GLITCH_REPAIR`] prompt echoes. Otherwise a guess looks like what it
    /// continues, and a key looks final the moment it is drawn.
    #[must_use]
    pub fn marked(&self, now: Instant) -> bool {
        self.slow_marks
            || self.unsure > 0
            || self.guesses.front().is_some_and(|g| now.saturating_duration_since(g.at) > GLITCH)
    }

    /// The cursor as it should be drawn: where the keys in flight leave it on its row.
    #[must_use]
    pub fn cursor(&self, real: Cursor) -> Cursor {
        match self.guesses.back() {
            Some(guess) if self.row == real.row => Cursor { col: guess.cursor, ..real },
            _ => real,
        }
    }

    /// A key is about to be sent. Returns the cell guessed for it, if any: the typed glyph, or
    /// what ⌫ leaves where the deleted character was. An arrow moves only the cursor
    /// ([`Self::cursor`]) and returns `None`.
    ///
    /// `cursor`, `cols` and `modes` are the authoritative screen's, as the last frame left it;
    /// the predictor adds the keys in flight itself.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        cursor: Cursor,
        cols: u16,
        modes: TermModes,
        now: Instant,
    ) -> Option<Prediction> {
        if self.policy == Policy::Never || key.action == KeyAction::Release {
            return None;
        }
        self.sent = self.sent.max(key.seq);
        let stroke = stroke(key);
        if std::mem::take(&mut self.interrupted) {
            self.hold_until(key.seq);
            return None;
        }
        let Some(stroke) = stroke else {
            self.hold_until(key.seq);
            return None;
        };
        if self.barrier.is_some_and(|barrier| self.acked < barrier) {
            // Unguessed, it moves the line too: the next guess waits for it as well.
            self.barrier = Some(key.seq);
            return None;
        }
        let lost = !self.guesses.is_empty() && cursor.row != self.row;
        if !modes.prediction_allowed() || !cursor.visible || lost {
            self.flush();
            return None;
        }
        if self.guesses.len() >= MAX_PENDING {
            self.flush();
            return None;
        }
        let line = self.line(cursor, cols);
        let Some(guess) = line.apply(stroke, key.seq, now, modes, self.floor) else {
            // A key typed where the predictor will not guess (the last column, before a wide
            // glyph) leaves the epoch as it was; an edit it cannot follow ends it.
            if matches!(stroke, Stroke::Type(_)) {
                self.flush();
            } else {
                self.hold_until(key.seq);
            }
            return None;
        };
        if self.guesses.is_empty() {
            self.row = line.row;
            self.origin_cursor = line.cursor;
            self.origin.clear();
        }
        for change in &guess.changes {
            if !self.origin.iter().any(|(col, _)| *col == change.col) {
                self.origin.push((change.col, line.shown(change.col)));
            }
        }
        if matches!(stroke, Stroke::Type(_)) {
            self.floor = Some(match self.floor {
                Some((row, floor)) if row == line.row => (row, floor.min(line.cursor)),
                _ => (line.row, line.cursor),
            });
        }
        let made = match stroke {
            Stroke::Type(_) => Some(line.cursor),
            Stroke::Erase => Some(guess.cursor),
            Stroke::Left | Stroke::Right => None,
        }
        .and_then(|col| guess.changes.iter().rev().find(|c| c.col == col))
        .map(|change| Prediction {
            seq: key.seq,
            row: line.row,
            col: change.col,
            text: drawn_text(&change.cell),
            at: now,
        });
        self.guesses.push_back(guess);
        self.redraw();
        made
    }

    /// An authoritative frame was applied to `screen`. `input_ack` and `epoch` come from the
    /// frame. Checks the acknowledged keys' guesses against the screen.
    ///
    /// The worker acknowledges every key written before the read a frame was cut from, and that
    /// read may hold other output (a spinner, a build) instead of the echo; a line editor also
    /// draws several keys read together at once. So the frame confirms the most keys it can: the
    /// latest acknowledged key after which every cell with evidence and the cursor read as
    /// guessed. A frame that still reads as before the first of them leaves them pending, until
    /// the echo lands or [`STALE`]; anything else is a miss.
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
        let row = if self.guesses.is_empty() { cursor.row } else { self.row };
        self.base = screen.lines().get(usize::from(row)).map(|line| Base {
            row,
            line: Arc::clone(line),
            wrapped_below: row
                .checked_add(1)
                .and_then(|below| screen.line(below))
                .is_some_and(|below| below.flags.contains(LineFlags::WRAPPED)),
        });
        self.learn_right_prompt(cursor, screen.cols());
        if self.guesses.is_empty() && self.floor.is_some_and(|(floor, _)| floor != cursor.row) {
            self.floor = None;
        }
        let previous = self.numbering.replace(epoch);
        if previous.is_some_and(|e| e != epoch) {
            // Numbering changed (alt screen, reset, reflow): guesses are meaningless.
            self.hold_until(self.sent);
            return out;
        }
        if !self.reconcile(screen, input_ack, now, &mut out) {
            out.misses = out.misses.saturating_add(1);
            self.miss(now);
        }
        // Anything unconfirmed for too long counts as a miss too.
        if self.guesses.front().is_some_and(|g| now.saturating_duration_since(g.at) > STALE) {
            out.misses = out.misses.saturating_add(1);
            self.miss(now);
        }
        out.pending = self.guesses.len();
        self.redraw();
        out
    }

    /// Drop every guess (resize, detach, focus loss): none is made until the worker has every
    /// key sent so far, so the next lands where the cursor really is.
    pub fn flush(&mut self) {
        self.guesses.clear();
        self.origin.clear();
        self.drawn.clear();
        self.floor = None;
        if self.sent > self.acked {
            self.barrier = Some(self.barrier.map_or(self.sent, |b| b.max(self.sent)));
        }
    }

    /// Input went out that the predictor does not see (raw bytes, a paste): the guesses are
    /// dropped, the next key waits to be acknowledged before any other is guessed, and what
    /// follows is tentative.
    pub fn interrupt(&mut self) {
        self.flush();
        self.interrupted = true;
    }

    /// Key `seq` moved the cursor where no guess can follow: nothing is guessed until the
    /// worker acknowledges it, and a new, tentative epoch begins.
    fn hold_until(&mut self, seq: u64) {
        self.flush();
        self.barrier = Some(seq);
        self.tentative = true;
    }

    /// A guess was wrong: the epoch ends. A drawn one also mutes the predictor and marks what
    /// follows; a tentative one was never seen.
    fn miss(&mut self, now: Instant) {
        if !self.tentative {
            self.hits = 0;
            self.unsure = GLITCH_REPAIR;
            self.muted_until = now.checked_add(MUTE);
        }
        self.hold_until(self.sent);
    }

    /// Remember the right prompt of the row the frame left the guesses on, when two blanks or
    /// more part it from the typed text, until the row no longer shows it there.
    fn learn_right_prompt(&mut self, cursor: Cursor, cols: u16) {
        let Some(base) = &self.base else { return };
        if self.right.as_ref().is_some_and(|seen| seen.on(base.row, &base.line.cells).is_some()) {
            // Still where it was: a line one blank from it would read as a longer prompt.
            return;
        }
        let from =
            base.line.mark.input_col().or_else(|| (cursor.row == base.row).then_some(cursor.col));
        let found = from.and_then(|from| right_prompt(&base.line.cells, from, cols));
        match found.filter(|start| cursor.row != base.row || *start > cursor.col) {
            Some(start) => {
                let cells = base.line.cells.get(usize::from(start)..).unwrap_or_default();
                let len =
                    cells.iter().rposition(|cell| !blank(cell)).map_or(0, |i| i.saturating_add(1));
                let text = cells.iter().take(len).map(|cell| cell.text.clone()).collect();
                self.right = Some(RightPrompt { row: base.row, start, text });
            }
            None if self.right.as_ref().is_some_and(|seen| seen.row != base.row) => {
                self.right = None;
            }
            None => {}
        }
    }

    /// The line the next key edits: the last frame's row with the guesses in flight applied.
    fn line(&self, real: Cursor, cols: u16) -> Edited {
        let (row, cursor) =
            self.guesses.back().map_or((real.row, real.col), |g| (self.row, g.cursor));
        let base = self.base.as_ref().filter(|base| base.row == row);
        let mut cells = base.map_or_else(Vec::new, |base| base.line.cells.clone());
        cells.resize(usize::from(cols), Cell::BLANK);
        for change in self.guesses.iter().flat_map(|g| &g.changes) {
            if let Some(cell) = cells.get_mut(usize::from(change.col)) {
                cell.clone_from(&change.cell);
            }
        }
        let input = base.and_then(|base| base.line.mark.input_col());
        let from = input.unwrap_or(real.col);
        // The right prompt the frame shows; the cursor is always in the input, never in it.
        let shown = base
            .and_then(|base| {
                self.right
                    .as_ref()
                    .and_then(|seen| seen.on(row, &base.line.cells))
                    .or_else(|| right_prompt(&base.line.cells, from, cols))
                    .map(|start| (start, last_text(&base.line.cells, start)))
            })
            .filter(|(start, _)| *start > cursor);
        let mut limit = cols;
        let mut hidden = Vec::new();
        if let Some((start, end)) = shown {
            let touched = start.checked_sub(1).and_then(|col| cells.get(usize::from(col)));
            if touched.is_some_and(|cell| !blank(cell)) {
                // The guesses pushed the text up to it: the shell hides what of it they left.
                let guessed =
                    |col: u16| self.guesses.iter().flat_map(|g| &g.changes).any(|c| c.col == col);
                hidden = (start..end).filter(|col| !guessed(*col)).collect();
                for &col in &hidden {
                    if let Some(cell) = cells.get_mut(usize::from(col)) {
                        *cell = Cell::BLANK;
                    }
                }
            } else {
                limit = start;
            }
        }
        let right_end = last_text(&cells, limit);
        Edited {
            row,
            cursor,
            cols,
            limit,
            right_end,
            hidden,
            cells,
            base: base.map(|base| Arc::clone(&base.line)),
            input,
            wrapped_below: base.is_some_and(|base| base.wrapped_below),
        }
    }

    /// Check the acknowledged guesses against the frame. `false` when it contradicts them.
    fn reconcile(
        &mut self,
        screen: &Screen,
        input_ack: u64,
        now: Instant,
        out: &mut Reconciled,
    ) -> bool {
        let due = self.guesses.iter().take_while(|g| g.seq <= input_ack).count();
        if due == 0 {
            return true;
        }
        let line = screen.line(self.row);
        let shows = |col: u16| line.and_then(|l| l.cells.get(usize::from(col)));
        let real = screen.cursor();
        let at = (real.row == self.row).then_some(real.col);
        let moves = self.guesses.iter().take(due).any(|g| g.moves);
        let mut evidence: Vec<u16> = self
            .guesses
            .iter()
            .take(due)
            .flat_map(|g| g.changes.iter().filter(|c| c.check != Check::Nothing).map(|c| c.col))
            .collect();
        evidence.sort_unstable();
        evidence.dedup();
        // Per column with evidence, what it read before the guesses and each change to it, by
        // the index of the guess that made it.
        let mut history: Vec<ColumnHistory<'_>> = evidence
            .iter()
            .map(|col| {
                let origin =
                    self.origin.iter().find(|(c, _)| c == col).and_then(|(_, t)| t.as_ref());
                (origin, Vec::new())
            })
            .collect();
        for (i, guess) in self.guesses.iter().take(due).enumerate() {
            for change in &guess.changes {
                if let Ok(at) = evidence.binary_search(&change.col)
                    && let Some((_, changes)) = history.get_mut(at)
                {
                    changes.push((i, change));
                }
            }
        }
        let reads_as = |after: usize| {
            let cursor = after
                .checked_sub(1)
                .and_then(|i| self.guesses.get(i))
                .map_or(self.origin_cursor, |g| g.cursor);
            (!moves || at == Some(cursor))
                && evidence.iter().zip(&history).all(|(&col, (origin, changes))| {
                    let shown = shows(col);
                    match changes.iter().rev().find(|(i, _)| *i < after) {
                        Some((_, change)) => match change.check {
                            Check::Nothing => true,
                            Check::Text => shown.is_some_and(|s| same(&s.text, &change.cell.text)),
                            Check::Typed => {
                                shown.is_some_and(|s| same(&s.text, &change.cell.text) && !ghost(s))
                            }
                        },
                        None => origin.zip(shown).is_some_and(|(text, s)| same(&s.text, text)),
                    }
                })
        };
        let Some(confirmed) = (0..=due).rev().find(|&after| reads_as(after)) else {
            return false;
        };
        let settled = confirmed
            .checked_sub(1)
            .and_then(|i| self.guesses.get(i))
            .map_or(self.origin_cursor, |g| g.cursor);
        for _ in 0..confirmed {
            let Some(guess) = self.guesses.pop_front() else { break };
            if !guess.evidence() {
                continue;
            }
            if now.saturating_duration_since(guess.at) > GLITCH {
                self.unsure = GLITCH_REPAIR;
            } else {
                self.unsure = self.unsure.saturating_sub(1);
            }
            self.tentative = false;
            out.hits = out.hits.saturating_add(1);
            self.hits = self.hits.saturating_add(1);
        }
        if confirmed > 0 {
            // The frame is where the keys still in flight start from.
            let mut origin = Vec::new();
            for change in self.guesses.iter().flat_map(|g| &g.changes) {
                if !origin.iter().any(|(col, _)| *col == change.col) {
                    origin.push((change.col, shows(change.col).map(|c| c.text.clone())));
                }
            }
            self.origin = origin;
            self.origin_cursor = settled;
        }
        true
    }

    /// Rebuild the cells drawn from the guesses in flight.
    fn redraw(&mut self) {
        let mut drawn: Vec<Prediction> = Vec::new();
        for guess in &self.guesses {
            for change in &guess.changes {
                let cell = Prediction {
                    seq: guess.seq,
                    row: self.row,
                    col: change.col,
                    text: drawn_text(&change.cell),
                    at: guess.at,
                };
                match drawn.iter_mut().find(|p| p.col == change.col) {
                    Some(slot) => *slot = cell,
                    None => drawn.push(cell),
                }
            }
        }
        drawn.sort_by_key(|p| p.col);
        self.drawn = drawn.into();
    }
}

/// What a column read before the guesses in flight, and each change they make to it, by the
/// index of the guess that made it.
type ColumnHistory<'a> = (Option<&'a CellText>, Vec<(usize, &'a Change)>);

/// The input row as the next key finds it.
#[derive(Debug)]
struct Edited {
    row: u16,
    cursor: u16,
    cols: u16,
    /// Where a right prompt starts, or `cols`: the typed text never reaches it.
    limit: u16,
    /// One past the right prompt's last cell.
    right_end: u16,
    /// A right prompt the frame shows that the guesses pushed the text up to: blank now.
    hidden: Vec<u16>,
    /// The row's cells, `cols` of them, with the guesses in flight applied.
    cells: Vec<Cell>,
    /// The row as a frame showed it, when one has.
    base: Option<Arc<Line>>,
    /// Where the typed command starts on this row (OSC 133).
    input: Option<u16>,
    wrapped_below: bool,
}

impl Edited {
    fn cell(&self, col: u16) -> Option<&Cell> {
        self.cells.get(usize::from(col))
    }

    /// What the last frame showed at `col`, before any guess.
    fn shown(&self, col: u16) -> Option<CellText> {
        self.base.as_ref().and_then(|line| line.cells.get(usize::from(col))).map(|c| c.text.clone())
    }

    /// One past the typed text from the cursor on: it ends where a suggestion's grey starts,
    /// and before a right prompt. Blanks inside it (`echo a  b`) are its own.
    fn text_end(&self) -> u16 {
        let from = usize::from(self.cursor);
        let rest = self.cells.get(from..usize::from(self.limit)).unwrap_or_default();
        let text =
            rest.get(..rest.iter().position(ghost).unwrap_or(rest.len())).unwrap_or_default();
        text.iter()
            .rposition(|cell| !blank(cell))
            .and_then(|last| u16::try_from(from.saturating_add(last).saturating_add(1)).ok())
            .unwrap_or(self.cursor)
    }

    /// The cells from `from` to `to` moved to start at `at`, or `None` when one of them cannot
    /// be drawn as a single narrow glyph.
    fn moved(&self, from: u16, to: u16, at: u16) -> Option<Vec<Change>> {
        let cells = self.cells.get(usize::from(from)..usize::from(to))?;
        let mut changes = Vec::with_capacity(cells.len());
        for (col, cell) in (at..).zip(cells) {
            if cell.width != CellWidth::Narrow || cell.text.as_str().chars().nth(1).is_some() {
                return None;
            }
            let differs = self.cell(col).is_none_or(|was| !same(&was.text, &cell.text));
            let check = if !blank(cell) && differs { Check::Text } else { Check::Nothing };
            changes.push(Change { col, cell: cell.clone(), check });
        }
        Some(changes)
    }

    /// Blanks over the suggestion from `from` on: the shell takes it away with the edit.
    fn clear_suggestion(&self, from: u16, changes: &mut Vec<Change>) {
        for (col, cell) in (from..self.cols).zip(self.cells.iter().skip(usize::from(from))) {
            if !ghost(cell) {
                break;
            }
            changes.push(Change { col, cell: Cell::BLANK, check: Check::Nothing });
        }
    }

    /// The guess for `stroke`, or `None` when the predictor cannot say what it does.
    fn apply(
        &self,
        stroke: Stroke,
        seq: u64,
        at: Instant,
        modes: TermModes,
        floor: Option<(u16, u16)>,
    ) -> Option<Guess> {
        let c = self.cursor;
        // The line editor's own keys, only where OSC 133 marks the input and a line editor
        // (not the tty's canonical mode, where an arrow echoes `^[[D`) reads it.
        let editing = || {
            let start = self.input.filter(|_| !modes.contains(TermModes::CANONICAL))?;
            (self.base.is_some() && c >= start).then_some(start)
        };
        let (cursor, moves, changes) =
            match stroke {
                Stroke::Type(ch) => {
                    // Never predict a wrap; the shell may or may not autowrap the prompt.
                    if c.saturating_add(1) >= self.cols {
                        return None;
                    }
                    let under = self.cell(c)?;
                    if under.width != CellWidth::Narrow {
                        return None;
                    }
                    let typed = Cell::narrow(ch, Style::DEFAULT);
                    let covered = same(&under.text, &typed.text);
                    let check = match (blank(&typed), covered, ghost(under)) {
                        (true, ..) | (false, true, false) => Check::Nothing,
                        (false, true, true) => Check::Typed,
                        (false, false, _) => Check::Text,
                    };
                    let mut changes = vec![Change { col: c, cell: typed, check }];
                    let end = self.text_end();
                    if end > c {
                        // Inside the text: the rest moves one right.
                        if self.wrapped_below || end.saturating_add(1) >= self.cols {
                            return None;
                        }
                        changes.extend(self.moved(c, end, c.saturating_add(1))?);
                    } else if ghost(under) && !covered {
                        self.clear_suggestion(c.saturating_add(1), &mut changes);
                    }
                    // Text that comes to touch a right prompt hides it, as zsh and fish do.
                    if end.max(c).saturating_add(1) >= self.limit {
                        changes.extend((self.limit..self.right_end).map(|col| Change {
                            col,
                            cell: Cell::BLANK,
                            check: Check::Nothing,
                        }));
                    }
                    (c.saturating_add(1), false, changes)
                }
                Stroke::Left => {
                    let start = editing()?;
                    let before = self.cell(c.checked_sub(1)?)?;
                    let step = if before.width == CellWidth::SpacerTail { 2 } else { 1 };
                    let to = c.checked_sub(step).filter(|to| *to >= start)?;
                    matches!(self.cell(to)?.width, CellWidth::Narrow | CellWidth::Wide)
                        .then_some((to, true, Vec::new()))?
                }
                Stroke::Right => {
                    editing()?;
                    if c >= self.text_end() {
                        // At the end a suggestion may take it, or nothing moves.
                        return None;
                    }
                    let step = match self.cell(c)?.width {
                        CellWidth::Narrow => 1,
                        CellWidth::Wide => 2,
                        CellWidth::SpacerTail | CellWidth::SpacerHead => return None,
                    };
                    let to = c.saturating_add(step);
                    (to < self.cols).then_some((to, true, Vec::new()))?
                }
                Stroke::Erase => {
                    let start = editing().or_else(|| {
                        floor.filter(|(row, _)| *row == self.row).map(|(_, col)| col)
                    })?;
                    let step = match self.cell(c.checked_sub(1)?)?.width {
                        CellWidth::Narrow => 1,
                        CellWidth::SpacerTail => 2,
                        CellWidth::Wide | CellWidth::SpacerHead => return None,
                    };
                    let to = c.checked_sub(step).filter(|to| *to >= start)?;
                    if step == 2 && self.cell(to)?.width != CellWidth::Wide {
                        return None;
                    }
                    let end = self.text_end();
                    let mut changes;
                    if end > c {
                        // Inside the text: the rest moves left over what was deleted.
                        if self.wrapped_below {
                            return None;
                        }
                        changes = self.moved(c, end, to)?;
                        let tail = end.checked_sub(step)?;
                        changes.extend((tail..end).map(|col| Change {
                            col,
                            cell: Cell::BLANK,
                            check: Check::Nothing,
                        }));
                    } else {
                        changes = (to..c)
                            .map(|col| Change { col, cell: Cell::BLANK, check: Check::Nothing })
                            .collect();
                        self.clear_suggestion(c, &mut changes);
                    }
                    (to, true, changes)
                }
            };
        // First, so that what the key moves there is drawn over it.
        let blanks =
            self.hidden.iter().map(|&col| Change { col, cell: Cell::BLANK, check: Check::Nothing });
        let changes = blanks.chain(changes).collect();
        Some(Guess { seq, at, cursor, moves, changes })
    }
}

/// What a key does, if it is one the predictor can follow. A ⌥ that is Alt makes the key a
/// chord (`ESC b` is a word back), whatever the text beside it; a modifier on an editing key
/// makes it another key (⇧← selects in some shells, ⌥⌫ deletes a word).
fn stroke(key: &KeyEvent) -> Option<Stroke> {
    let chord = key.mods.intersects(Mods::SHIFT | Mods::ALT | Mods::CTRL | Mods::SUPER);
    match key.code {
        KeyCode::Backspace if !chord => Some(Stroke::Erase),
        KeyCode::ArrowLeft if !chord => Some(Stroke::Left),
        KeyCode::ArrowRight if !chord => Some(Stroke::Right),
        KeyCode::Backspace | KeyCode::ArrowLeft | KeyCode::ArrowRight => None,
        _ => printable(key).map(Stroke::Type),
    }
}

/// The character a key would echo, if it is a plain printable one.
fn printable(key: &KeyEvent) -> Option<char> {
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
    c.is_ascii().then_some(c)
}

/// Where a right prompt starts on a row's `cells` (zsh's `RPROMPT`, fish's right prompt): text
/// that reaches the row's last [`RIGHT_PROMPT_EDGE`] columns after [`TEXT_GAP`] blanks or more
/// past `from`, the input's start. A prompt of several words keeps single blanks inside it.
fn right_prompt(cells: &[Cell], from: u16, cols: u16) -> Option<u16> {
    let last = cells.iter().rposition(|cell| !blank(cell))?;
    let edge = usize::from(cols.saturating_sub(RIGHT_PROMPT_EDGE));
    if last < edge || cells.get(last).is_some_and(ghost) {
        return None;
    }
    let mut blanks = 0_usize;
    for i in (usize::from(from)..last).rev() {
        if cells.get(i).is_some_and(blank) {
            blanks = blanks.saturating_add(1);
            if blanks >= usize::from(TEXT_GAP) {
                return u16::try_from(i.saturating_add(blanks)).ok();
            }
        } else {
            blanks = 0;
        }
    }
    None
}

/// One past the last cell with text from `from` on, or `from`.
fn last_text(cells: &[Cell], from: u16) -> u16 {
    cells
        .get(usize::from(from)..)
        .and_then(|rest| rest.iter().rposition(|cell| !blank(cell)))
        .and_then(|last| {
            u16::try_from(usize::from(from).saturating_add(last).saturating_add(1)).ok()
        })
        .unwrap_or(from)
}

/// A cell that shows nothing.
fn blank(cell: &Cell) -> bool {
    matches!(cell.width, CellWidth::Narrow | CellWidth::SpacerHead)
        && cell.text.as_str().trim().is_empty()
}

/// A suggestion's cell (zsh-autosuggestions, fish): text drawn faint or in a grey, after the
/// typed text, that → at the end of the line takes.
fn ghost(cell: &Cell) -> bool {
    let grey = match cell.style.fg {
        Color::Palette(index) => matches!(index, 8 | 59 | 102 | 145 | 188 | 232..=255),
        Color::Rgb(r, g, b) => r == g && g == b && (0x30..=0xd0).contains(&r),
        Color::Default => false,
    };
    !blank(cell) && (grey || cell.style.flags.contains(StyleFlags::FAINT))
}

/// Two cells read the same: equal text, a blank equal to a space.
fn same(a: &CellText, b: &CellText) -> bool {
    a == b || (a.as_str().trim().is_empty() && b.as_str().trim().is_empty())
}

/// A cell's text as the overlay draws it: a space for a blank.
fn drawn_text(cell: &Cell) -> String {
    if cell.text.is_empty() { " ".to_owned() } else { cell.text.as_str().to_owned() }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use slopty_grid::{CursorShape, RowUpdate, SemanticMark};

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
                *cell = Cell::narrow(ch, Style::DEFAULT);
            }
        }
        screen.apply(RowUpdate { row, line: line.into() }).unwrap();
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
        // ⌫ takes back what the predictor typed: the glyph gives way to a blank, drawn until
        // the echo, so an echo of `b` that lands first does not flash it back.
        let bs = special(4, KeyCode::Backspace);
        let erased = p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).unwrap();
        assert_eq!((erased.col, erased.text.as_str()), (6, " "));
        assert_eq!(drawn(&p), [(5, "a"), (6, " ")]);
        assert_eq!(p.cursor(cursor(3, 5)).col, 6);
        assert!(p.visible(now), "still shown");
        let bs = special(5, KeyCode::Backspace);
        assert!(p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).is_some());
        assert_eq!(p.cursor(cursor(3, 5)).col, 5);
        // Past what it typed, with no OSC 133 mark, ⌫ erases the shell's text: nothing follows.
        let bs = special(6, KeyCode::Backspace);
        assert!(p.on_key(&bs, cursor(3, 5), 80, TermModes::CANONICAL, now).is_none());
        assert!(p.pending().is_empty());
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
        let _d = p.on_key(&key(4, "d"), cursor(0, 3), 80, TermModes::CANONICAL, later);
        assert!(!p.visible(now), "muted after a miss");
        assert!(!p.visible(later), "a miss ends the epoch: the next is tentative");
        assert_eq!(p.on_frame(&screen_with(0, "abXd"), 4, 0, later).hits, 1);
        let _e = p.on_key(&key(5, "e"), cursor(0, 4), 80, TermModes::CANONICAL, later);
        assert!(!p.visible(later), "after the mute a slow link must re-warm");
        p.set_rtt(Some(VERY_SLOW_LINK));
        assert!(p.visible(later), "a very slow link draws without warm-up");
    }

    /// A warmed-up link draws its guesses from half the display's refresh on: 8.3 ms until the
    /// display is known, 6.7 ms at 75 Hz, 4.2 ms at 120 Hz.
    #[test]
    fn guesses_show_from_half_a_refresh() {
        let t0 = Instant::now();
        let warm = |refresh: Option<Duration>, rtt_us: u64| {
            let mut p = Predictor::new(Policy::Adaptive);
            if let Some(refresh) = refresh {
                p.set_refresh(refresh);
            }
            p.set_rtt(Some(Duration::from_micros(rtt_us)));
            for (seq, typed, echoed) in [(1, "a", "a"), (2, "b", "ab")] {
                let col = u16::try_from(seq).unwrap() - 1;
                let _guess = p.on_key(&key(seq, typed), cursor(0, col), 80, TermModes::empty(), t0);
                assert_eq!(p.on_frame(&screen_with(0, echoed), seq, 0, t0).hits, 1, "{echoed}");
            }
            let _c = p.on_key(&key(3, "c"), cursor(0, 2), 80, TermModes::empty(), t0);
            p.visible(t0)
        };
        assert!(!warm(None, 8_300), "under half of 60 Hz");
        assert!(warm(None, 8_400), "past half of 60 Hz");
        let hz75 = Some(Duration::from_nanos(13_333_333));
        assert!(!warm(hz75, 6_600), "under half of 75 Hz");
        assert!(warm(hz75, 6_700), "the tailnet's round trip and more");
        let hz120 = Some(Duration::from_nanos(8_333_333));
        assert!(!warm(hz120, 4_100), "under half of 120 Hz");
        assert!(warm(hz120, 4_200), "half of 120 Hz");
    }

    #[test]
    fn unsafe_modes_and_wraps_refuse() {
        let mut p = Predictor::new(Policy::Always);
        let now = Instant::now();
        assert!(p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::ALT_SCREEN, now).is_none());
        assert!(
            p.on_key(
                &key(2, "a"),
                cursor(0, 0),
                80,
                TermModes::ECHO_OFF | TermModes::CANONICAL,
                now
            )
            .is_none()
        );
        assert!(p.on_key(&key(3, "a"), cursor(0, 79), 80, TermModes::empty(), now).is_none());
        let hidden = Cursor { visible: false, ..cursor(0, 0) };
        assert!(p.on_key(&key(4, "a"), hidden, 80, TermModes::empty(), now).is_none());
        // Unguessed, those keys still move the line: nothing is guessed until the worker has
        // them all.
        assert!(p.on_key(&key(5, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
        let _r = p.on_frame(&Screen::new(80, 24), 5, 0, now);
        assert!(p.on_key(&key(6, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_some());
        let mut ctrl = key(60, "c");
        ctrl.mods = Mods::CTRL;
        assert!(p.on_key(&ctrl, cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.pending().is_empty(), "a chord drops the guesses");
        let _r = p.on_frame(&Screen::new(80, 24), 60, 0, now);
        assert!(p.on_key(&key(61, "漢"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
        assert!(p.on_key(&key(62, "x"), cursor(0, 0), 80, TermModes::empty(), now).is_none());
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
            line: Arc::clone(&screen_with(3, "⠋ building").lines()[3]),
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
        assert!(
            p.on_key(&key(5, "c"), moved.cursor(), 80, modes, t0).is_none(),
            "4 went unguessed"
        );
        let _r = p.on_frame(&moved, 5, 0, t0);
        let c = p.on_key(&key(50, "c"), moved.cursor(), 80, modes, t0).expect("guessed again");
        assert_eq!(c.col, 3, "where the frame put the cursor");

        let mut word_back = key(51, "b");
        word_back.mods = Mods::ALT;
        word_back.option_as_alt = true;
        assert!(p.on_key(&word_back, cursor(0, 4), 80, modes, t0).is_none(), "ESC b, not b");
        assert!(p.pending().is_empty());
        let _r = p.on_frame(&moved, 51, 0, t0);
        let mut at = key(52, "@");
        at.mods = Mods::ALT;
        assert!(p.on_key(&at, cursor(0, 3), 80, modes, t0).is_some(), "⌥ typing a symbol");

        p.interrupt();
        assert!(p.pending().is_empty());
        assert!(p.on_key(&key(53, "d"), cursor(0, 3), 80, modes, t0).is_none(), "after raw bytes");
        assert!(p.on_key(&key(54, "e"), cursor(0, 3), 80, modes, t0).is_none(), "53 not acked");
        let _r = p.on_frame(&moved, 53, 0, t0);
        assert!(p.on_key(&key(55, "f"), cursor(0, 3), 80, modes, t0).is_none(), "nor 54");
        let _r = p.on_frame(&moved, 55, 0, t0);
        assert!(p.on_key(&key(56, "f"), cursor(0, 3), 80, modes, t0).is_some());
    }

    /// A guess on a tailnet-like link looks like the text it continues. It is marked on a link
    /// of 80 ms or more (until the link is under 50 ms), while a guess waits past `GLITCH`, and
    /// after a slow echo or a miss until ten guesses in a row have been echoed promptly.
    #[test]
    fn a_guess_is_marked_only_when_it_is_unsure() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        confirmed(&mut p, t0);
        let echo = |p: &mut Predictor, seq: u64, typed: &str, at: Instant| {
            let col = u16::try_from(typed.chars().count()).unwrap() - 1;
            let last = typed.chars().last().unwrap().to_string();
            let _guess = p.on_key(&key(seq, &last), cursor(0, col), 80, TermModes::empty(), at);
            p.on_frame(&screen_with(0, typed), seq, 0, at + Duration::from_millis(12))
        };
        p.set_rtt(Some(Duration::from_millis(12)));
        let _a = p.on_key(&key(2, "a"), cursor(0, 0), 80, TermModes::empty(), t0);
        assert!(p.visible(t0) && !p.marked(t0), "a tailnet round trip: unmarked");
        assert!(p.marked(t0 + GLITCH + Duration::from_millis(1)), "waiting past GLITCH");
        let late = t0 + GLITCH + Duration::from_millis(1);
        assert_eq!(p.on_frame(&screen_with(0, "a"), 2, 0, late).hits, 1);
        assert!(p.marked(late), "a slow echo marks what follows");
        let line = "abcdefghijk";
        for (seq, len) in (3..).zip(2..=line.len()) {
            let typed: String = line.chars().take(len).collect();
            assert!(p.marked(late), "{typed}: still repairing");
            assert_eq!(echo(&mut p, seq, &typed, late).hits, 1);
        }
        assert!(!p.marked(late), "ten prompt echoes repair it");

        let _x = p.on_key(&key(13, "x"), cursor(0, 11), 80, TermModes::empty(), late);
        assert_eq!(p.on_frame(&screen_with(0, "abcdefghijky"), 13, 0, late).misses, 1);
        let _y = p.on_key(&key(14, "y"), cursor(0, 12), 80, TermModes::empty(), late);
        assert!(p.marked(late), "a miss marks what follows");

        let mut q = Predictor::new(Policy::Always);
        q.set_rtt(Some(MARK_LINK));
        assert!(q.marked(t0), "a slow link");
        q.set_rtt(Some(Duration::from_millis(60)));
        assert!(q.marked(t0), "between the two lines: as it was");
        q.set_rtt(Some(Duration::from_millis(49)));
        assert!(!q.marked(t0), "under the lower line");
        q.set_rtt(Some(Duration::from_millis(60)));
        assert!(!q.marked(t0), "between the two lines: as it was");
    }

    #[test]
    fn stale_predictions_count_as_misses() {
        let mut p = Predictor::new(Policy::Always);
        let t0 = Instant::now();
        let _a = p.on_key(&key(1, "a"), cursor(0, 0), 80, TermModes::empty(), t0);
        let r = p.on_frame(&Screen::new(80, 24), 0, 0, t0 + STALE + Duration::from_millis(1));
        assert_eq!((r.misses, r.pending), (1, 0));
    }

    /// A line editor at its prompt: echo off, not canonical.
    const EDITOR: TermModes = TermModes::ECHO_OFF;

    /// Row 0 at a shell prompt: `$ ` then `typed` (a non-ASCII glyph takes two cells), OSC 133
    /// input from column 2, then a suggestion `ghost` in bright black, the cursor at `col`.
    fn prompt(typed: &str, ghost: &str, col: u16) -> Screen {
        let grey = Style { fg: Color::Palette(8), ..Style::DEFAULT };
        let mut cells = Vec::new();
        for c in "$ ".chars().chain(typed.chars()) {
            if c.is_ascii() {
                cells.push(Cell::narrow(c, Style::DEFAULT));
            } else {
                cells.push(Cell::wide(&c.to_string(), Style::DEFAULT));
                cells.push(Cell::spacer_tail(Style::DEFAULT));
            }
        }
        cells.extend(ghost.chars().map(|c| Cell::narrow(c, grey)));
        let mut line = Line::blank(80);
        for (slot, cell) in line.cells.iter_mut().zip(cells) {
            *slot = cell;
        }
        line.mark = SemanticMark::Prompt { exit: None, input: (!typed.is_empty()).then_some(2) };
        let mut screen = Screen::new(80, 24);
        screen.apply(RowUpdate { row: 0, line: line.into() }).unwrap();
        *screen.cursor_mut() = cursor(0, col);
        screen
    }

    /// A predictor past its first tentative stretch, looking at `screen` with key 1 echoed.
    fn at_prompt(screen: &Screen, policy: Policy, t0: Instant) -> Predictor {
        let mut p = Predictor::new(Policy::Always);
        confirmed(&mut p, t0);
        p.set_policy(policy);
        assert_eq!(p.on_frame(screen, 1, 0, t0), Reconciled::default());
        p
    }

    fn press(p: &mut Predictor, seq: u64, code: KeyCode, on: &Screen) -> Option<Prediction> {
        p.on_key(&special(seq, code), on.cursor(), 80, EDITOR, Instant::now())
    }

    fn typing(p: &mut Predictor, seq: u64, text: &str, on: &Screen) -> Option<Prediction> {
        p.on_key(&key(seq, text), on.cursor(), 80, EDITOR, Instant::now())
    }

    /// The cells the guesses draw, as (column, text).
    fn drawn(p: &Predictor) -> Vec<(u16, &str)> {
        p.pending().iter().map(|g| (g.col, g.text.as_str())).collect()
    }

    /// Row 0 as drawn with the guesses over it, and the cursor as drawn.
    fn reads(p: &Predictor, screen: &Screen) -> (String, u16) {
        let mut cells: Vec<String> =
            screen.line(0).unwrap().cells.iter().map(|c| c.text.as_str().to_owned()).collect();
        for guess in p.pending() {
            cells[usize::from(guess.col)].clone_from(&guess.text);
        }
        let text: String = cells.iter().map(|t| if t.is_empty() { " " } else { t }).collect();
        (text.trim_end().to_owned(), p.cursor(screen.cursor()).col)
    }

    /// ← and → move the drawn cursor inside the typed command and draw no cell; one frame
    /// can echo several of them. ← at the input's start does nothing in a shell, so it is not
    /// guessed, and the epoch it ends starts tentative.
    #[test]
    fn arrows_move_the_cursor_within_the_input() {
        let t0 = Instant::now();
        let line = prompt("echo hi", "", 9);
        let mut p = at_prompt(&line, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::ArrowLeft, &line).is_none(), "an arrow draws no cell");
        assert_eq!(reads(&p, &line), ("$ echo hi".to_owned(), 8));
        assert!(p.visible(Instant::now()), "the moved cursor is drawn at once");
        for seq in 3..=8 {
            let _none = press(&mut p, seq, KeyCode::ArrowLeft, &line);
        }
        assert_eq!(p.cursor(line.cursor()).col, 2, "at the input's start");
        let _none = press(&mut p, 9, KeyCode::ArrowRight, &line);
        assert_eq!(p.cursor(line.cursor()).col, 3);
        let r = p.on_frame(&prompt("echo hi", "", 3), 9, 0, t0);
        assert_eq!(r, Reconciled { hits: 8, misses: 0, pending: 0 }, "one frame, eight keys");

        let start = prompt("echo hi", "", 2);
        let _r = p.on_frame(&start, 9, 0, t0);
        assert!(press(&mut p, 10, KeyCode::ArrowLeft, &start).is_none());
        assert_eq!(p.cursor(start.cursor()).col, 2, "not guessed past the start");
        let _none = press(&mut p, 11, KeyCode::ArrowRight, &start);
        assert!(p.pending().is_empty() && p.cursor(start.cursor()).col == 2, "waits for 10");
        let _r = p.on_frame(&start, 11, 0, t0);
        let _none = press(&mut p, 12, KeyCode::ArrowRight, &start);
        assert!(!p.visible(Instant::now()), "a new epoch is tentative");
        let _r = p.on_frame(&prompt("echo hi", "", 3), 12, 0, t0);
        let _none = press(&mut p, 13, KeyCode::ArrowRight, &start);
        assert!(p.visible(Instant::now()), "confirmed: drawn again");
    }

    /// → at the end of the typed text is the shell's: over a suggestion it takes it, and with
    /// none it moves nothing. A right prompt is not the text; two blanks inside it are.
    #[test]
    fn right_is_not_guessed_past_the_typed_text() {
        let t0 = Instant::now();
        let suggested = prompt("git co", "mmit", 8);
        let mut p = at_prompt(&suggested, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::ArrowRight, &suggested).is_none());
        assert_eq!(p.cursor(suggested.cursor()).col, 8);

        let mut rprompt = prompt("ls", "", 3);
        // zsh draws a right prompt up to one column short of the edge.
        let tail = screen_with(0, &format!("{:74}~/src", "$ ls"));
        let mut line = tail.line(0).unwrap().clone();
        line.mark = SemanticMark::Prompt { exit: None, input: Some(2) };
        rprompt.apply(RowUpdate { row: 0, line: line.into() }).unwrap();
        let mut p = at_prompt(&rprompt, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::ArrowRight, &rprompt).is_none(), "an arrow");
        assert_eq!(p.cursor(rprompt.cursor()).col, 4, "over the s");
        assert!(press(&mut p, 3, KeyCode::ArrowRight, &rprompt).is_none());
        assert!(p.pending().is_empty() && p.cursor(rprompt.cursor()).col == 3, "not into ~/src");

        let spaced = prompt("echo a  b", "", 7);
        let mut p = at_prompt(&spaced, Policy::Always, t0);
        for seq in 2..=4 {
            let _none = press(&mut p, seq, KeyCode::ArrowRight, &spaced);
        }
        assert_eq!(p.cursor(spaced.cursor()).col, 10, "blanks inside the text are its own");
        let _e = press(&mut p, 5, KeyCode::Backspace, &spaced);
        assert_eq!(reads(&p, &spaced), ("$ echo a b".to_owned(), 9));
    }

    /// A shell that did not do what ← was guessed to do: a frame cut before the echo leaves the
    /// guess waiting, a cursor anywhere else is a miss. The overlay goes, the real cursor shows,
    /// and what follows is marked and tentative.
    #[test]
    fn a_mispredicted_arrow_is_retracted() {
        let t0 = Instant::now();
        let line = prompt("echo hi", "", 9);
        let mut p = at_prompt(&line, Policy::Always, t0);
        let _none = press(&mut p, 2, KeyCode::ArrowLeft, &line);
        let r = p.on_frame(&line, 2, 0, t0);
        assert_eq!(r, Reconciled { hits: 0, misses: 0, pending: 1 }, "not echoed yet");
        assert_eq!(p.cursor(line.cursor()).col, 8);
        let jumped = prompt("echo hi", "", 2);
        let r = p.on_frame(&jumped, 2, 0, t0 + Duration::from_millis(5));
        assert_eq!(r, Reconciled { hits: 0, misses: 1, pending: 0 });
        assert_eq!(reads(&p, &jumped), ("$ echo hi".to_owned(), 2), "the shell's cursor");
        assert!(p.marked(t0), "what follows is marked");
        let _none = press(&mut p, 3, KeyCode::ArrowRight, &jumped);
        assert_eq!(p.cursor(jumped.cursor()).col, 3, "guessed from where it really is");
        assert!(!p.visible(Instant::now()), "but tentative");
    }

    /// ⌫ inside the text deletes before the cursor and pulls the rest left, leaving a blank at
    /// its end; a key typed there pushes it right again. The echo confirms both.
    #[test]
    fn erase_inside_the_text_pulls_the_rest_left() {
        let t0 = Instant::now();
        let line = prompt("echo hello", "", 9);
        let mut p = at_prompt(&line, Policy::Always, t0);
        let erased = press(&mut p, 2, KeyCode::Backspace, &line).unwrap();
        assert_eq!((erased.col, erased.text.as_str()), (8, "l"), "the cell where e was");
        assert_eq!(reads(&p, &line), ("$ echo hllo".to_owned(), 8));
        assert_eq!(drawn(&p), [(8, "l"), (9, "l"), (10, "o"), (11, " ")]);
        let typed = typing(&mut p, 3, "a", &line).unwrap();
        assert_eq!((typed.col, typed.text.as_str()), (8, "a"));
        assert_eq!(reads(&p, &line), ("$ echo hallo".to_owned(), 9));
        let r = p.on_frame(&prompt("echo hllo", "", 8), 2, 0, t0);
        assert_eq!(r, Reconciled { hits: 1, misses: 0, pending: 1 });
        assert_eq!(reads(&p, &prompt("echo hllo", "", 8)), ("$ echo hallo".to_owned(), 9));
        let r = p.on_frame(&prompt("echo hallo", "", 9), 3, 0, t0);
        assert_eq!(r, Reconciled { hits: 1, misses: 0, pending: 0 });
    }

    /// ⌫ at the input's start deletes nothing in a shell: not guessed. Without an OSC 133 mark
    /// it is guessed only over what the predictor typed.
    #[test]
    fn erase_is_not_guessed_before_the_input() {
        let t0 = Instant::now();
        let line = prompt("ls", "", 2);
        let mut p = at_prompt(&line, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::Backspace, &line).is_none());
        assert!(p.pending().is_empty());
        let mut bare = screen_with(0, "> ab");
        bare.cursor_mut().col = 4;
        let _r = p.on_frame(&bare, 2, 0, t0);
        assert!(press(&mut p, 3, KeyCode::Backspace, &bare).is_none(), "not the predictor's");
        let _r = p.on_frame(&bare, 3, 0, t0);
        assert!(typing(&mut p, 4, "c", &bare).is_some());
        assert!(press(&mut p, 5, KeyCode::Backspace, &bare).is_some(), "its own c");
        assert!(press(&mut p, 6, KeyCode::Backspace, &bare).is_none(), "b is not");
    }

    /// A frame that shows the line as no guess had it (the shell deleted a word, not a
    /// character) retracts every guess, mutes the predictor, and shows the shell's line.
    #[test]
    fn an_echo_that_disagrees_retracts_the_edit() {
        let t0 = Instant::now();
        let line = prompt("echo hello world", "", 12);
        let mut p = at_prompt(&line, Policy::Adaptive, t0);
        p.set_rtt(Some(VERY_SLOW_LINK));
        let _e = press(&mut p, 2, KeyCode::Backspace, &line).unwrap();
        assert_eq!(reads(&p, &line), ("$ echo hell world".to_owned(), 11));
        assert!(p.visible(t0));
        let word = prompt("echo  world", "", 7);
        let r = p.on_frame(&word, 2, 0, t0);
        assert_eq!(r, Reconciled { hits: 0, misses: 1, pending: 0 });
        assert_eq!(reads(&p, &word), ("$ echo  world".to_owned(), 7));
        let _e = press(&mut p, 3, KeyCode::Backspace, &word);
        let _r = p.on_frame(&prompt("echo world", "", 6), 3, 0, t0);
        let _e = press(&mut p, 4, KeyCode::Backspace, &word);
        assert!(!p.visible(t0 + Duration::from_millis(10)), "muted");
    }

    /// Enter, Esc, ↑ and ↓ end the epoch: the guesses go, nothing is guessed until the worker
    /// has the key, and the next guesses are drawn only once one is echoed.
    #[test]
    fn epochs_end_on_enter_esc_and_the_vertical_arrows() {
        let t0 = Instant::now();
        let line = prompt("ls -la", "", 8);
        for code in [KeyCode::Enter, KeyCode::Escape, KeyCode::ArrowUp, KeyCode::ArrowDown] {
            let mut p = at_prompt(&line, Policy::Always, t0);
            let _none = press(&mut p, 2, KeyCode::ArrowLeft, &line);
            assert!(press(&mut p, 3, code, &line).is_none(), "{code:?}");
            assert_eq!(p.cursor(line.cursor()).col, 8, "{code:?} drops the guesses");
            let _none = press(&mut p, 4, KeyCode::ArrowLeft, &line);
            assert_eq!(p.cursor(line.cursor()).col, 8, "{code:?}: waits for the worker");
            let _r = p.on_frame(&line, 4, 0, t0);
            let _none = press(&mut p, 5, KeyCode::ArrowLeft, &line);
            assert!(!p.visible(t0), "{code:?}: tentative");
            let _r = p.on_frame(&prompt("ls -la", "", 7), 5, 0, t0);
            let _none = press(&mut p, 6, KeyCode::ArrowLeft, &line);
            assert!(p.visible(t0), "{code:?}: confirmed");
        }
    }

    /// A guess that fails while the epoch is tentative was never drawn: the epoch ends, but
    /// nothing is muted or marked (mosh kills just that epoch). Here Esc put zsh in vi command
    /// mode, where x deletes instead of typing.
    #[test]
    fn a_tentative_miss_does_not_mute() {
        let t0 = Instant::now();
        let line = prompt("ls", "", 4);
        let mut p = at_prompt(&line, Policy::Adaptive, t0);
        p.set_rtt(Some(Duration::from_millis(60)));
        let _none = press(&mut p, 2, KeyCode::Escape, &line);
        let command = prompt("ls", "", 3);
        let _r = p.on_frame(&command, 2, 0, t0);
        assert!(typing(&mut p, 3, "x", &command).is_some(), "guessed");
        assert!(!p.visible(t0), "not drawn");
        let deleted = prompt("l", "", 2);
        assert_eq!(p.on_frame(&deleted, 3, 0, t0).misses, 1);
        assert!(!p.marked(t0), "nothing was shown wrong");
        // Back in insert mode at the end of the line.
        let inserting = prompt("l", "", 3);
        let _r = p.on_frame(&inserting, 3, 0, t0);
        let _y = typing(&mut p, 4, "y", &inserting);
        assert_eq!(p.on_frame(&prompt("ly", "", 4), 4, 0, t0).hits, 1);
        let _z = typing(&mut p, 5, "z", &prompt("ly", "", 4));
        assert!(p.visible(t0), "drawn at once: not muted, and the hits still count");
    }

    /// A line editor reading several keys at once draws them in one frame, and a frame may
    /// carry an acknowledgement for keys the shell has not drawn yet: it confirms what it
    /// shows and leaves the rest waiting.
    #[test]
    fn a_frame_confirms_what_it_shows() {
        let t0 = Instant::now();
        let line = prompt("abcdef", "", 8);
        let mut p = at_prompt(&line, Policy::Always, t0);
        for seq in 2..=4 {
            let _none = press(&mut p, seq, KeyCode::ArrowLeft, &line);
        }
        let r = p.on_frame(&prompt("abcdef", "", 5), 4, 0, t0);
        assert_eq!(r, Reconciled { hits: 3, misses: 0, pending: 0 });
        let line = prompt("abcdef", "", 5);
        let _none = press(&mut p, 5, KeyCode::ArrowLeft, &line);
        let _none = press(&mut p, 6, KeyCode::Backspace, &line);
        assert_eq!(reads(&p, &line), ("$ acdef".to_owned(), 3));
        let r = p.on_frame(&prompt("abcdef", "", 4), 6, 0, t0);
        assert_eq!(r, Reconciled { hits: 1, misses: 0, pending: 1 }, "only the ← drawn");
        let r = p.on_frame(&prompt("acdef", "", 3), 6, 0, t0);
        assert_eq!(r, Reconciled { hits: 1, misses: 0, pending: 0 });
    }

    /// A wide glyph is one character of two cells: ← → and ⌫ step over both. A key that would
    /// move one along the line (the overlay draws single cells) is not guessed.
    #[test]
    fn wide_glyphs_move_by_two_cells() {
        let t0 = Instant::now();
        // `$ a漢b`: a at 2, 漢 at 3 and 4, b at 5.
        let end = prompt("a漢b", "", 6);
        let mut p = at_prompt(&end, Policy::Always, t0);
        let _none = press(&mut p, 2, KeyCode::ArrowLeft, &end);
        let _none = press(&mut p, 3, KeyCode::ArrowLeft, &end);
        assert_eq!(p.cursor(end.cursor()).col, 3, "over 漢");
        let _none = press(&mut p, 4, KeyCode::ArrowRight, &end);
        assert_eq!(p.cursor(end.cursor()).col, 5, "back over 漢");

        let mut p = at_prompt(&end, Policy::Always, t0);
        let _b = press(&mut p, 2, KeyCode::Backspace, &end).unwrap();
        let wide = press(&mut p, 3, KeyCode::Backspace, &end).unwrap();
        assert_eq!((wide.col, p.cursor(end.cursor()).col), (3, 3), "漢 deleted whole");
        assert_eq!(drawn(&p), [(3, " "), (4, " "), (5, " ")]);
        let r = p.on_frame(&prompt("a", "", 3), 3, 0, t0);
        assert_eq!((r.hits, r.misses), (2, 0));

        let before = prompt("a漢b", "", 3);
        let mut p = at_prompt(&before, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::Backspace, &before).is_none(), "漢 would move");
        let mut p = at_prompt(&before, Policy::Always, t0);
        assert!(typing(&mut p, 2, "x", &before).is_none(), "漢 would move");
    }

    /// A key typed onto a suggestion's own next glyph is confirmed only when the echo draws it
    /// as typed text; one that leaves the suggestion takes the rest of it away.
    #[test]
    fn typing_meets_a_suggestion() {
        let t0 = Instant::now();
        let line = prompt("git c", "oqrst", 7);
        let mut p = at_prompt(&line, Policy::Always, t0);
        let _o = typing(&mut p, 2, "o", &line).unwrap();
        assert_eq!(drawn(&p), [(7, "o")], "the suggestion stays");
        let r = p.on_frame(&line, 2, 0, t0);
        assert_eq!(r, Reconciled { hits: 0, misses: 0, pending: 1 }, "still the grey o");
        let line = prompt("git co", "qrst", 8);
        assert_eq!(p.on_frame(&line, 2, 0, t0).hits, 1);
        let _x = typing(&mut p, 3, "x", &line).unwrap();
        assert_eq!(drawn(&p), [(8, "x"), (9, " "), (10, " "), (11, " ")]);
        assert_eq!(reads(&p, &line), ("$ git cox".to_owned(), 9));
        assert_eq!(p.on_frame(&prompt("git cox", "", 9), 3, 0, t0).hits, 1);
    }

    /// The editing keys are a line editor's: at a canonical read (a program's `read`), on the
    /// alternate screen or with no OSC 133 mark they are not guessed.
    #[test]
    fn arrows_are_guessed_only_at_a_line_editor() {
        let t0 = Instant::now();
        let line = prompt("ls", "", 4);
        for modes in [TermModes::CANONICAL, TermModes::ALT_SCREEN] {
            let mut p = at_prompt(&line, Policy::Always, t0);
            let left = special(2, KeyCode::ArrowLeft);
            assert!(p.on_key(&left, line.cursor(), 80, modes, t0).is_none());
            assert_eq!(p.cursor(line.cursor()).col, 4, "{modes:?}");
        }
        let mut bare = screen_with(0, "$ ls");
        bare.cursor_mut().col = 4;
        let mut p = at_prompt(&bare, Policy::Always, t0);
        assert!(press(&mut p, 2, KeyCode::ArrowLeft, &bare).is_none());
        assert_eq!(p.cursor(bare.cursor()).col, 4, "no mark");
        let mut shifted = special(3, KeyCode::ArrowLeft);
        shifted.mods = Mods::SHIFT;
        let _r = p.on_frame(&line, 3, 0, t0);
        assert!(p.on_key(&shifted, line.cursor(), 80, EDITOR, t0).is_none());
        assert_eq!(p.cursor(line.cursor()).col, 4, "⇧← is another key");
    }

    /// [`prompt`] with a right prompt `~/src` drawn as zsh draws it, one column short of the
    /// row's end (columns 74 to 78).
    fn with_right(typed: &str, col: u16) -> Screen {
        let mut screen = prompt(typed, "", col);
        let mut line = screen.line(0).unwrap().clone();
        for (cell, c) in line.cells.iter_mut().skip(74).zip("~/src".chars()) {
            *cell = Cell::narrow(c, Style::DEFAULT);
        }
        screen.apply(RowUpdate { row: 0, line: line.into() }).unwrap();
        screen
    }

    /// A right prompt is not the typed text: an edit moves the text and leaves it. Once seen,
    /// it is known where zsh draws it one blank from a line that has grown to it, and text
    /// pushed up to it hides it, as the shell will.
    #[test]
    fn a_right_prompt_is_not_the_text() {
        let t0 = Instant::now();
        let short = with_right("echo hi", 7);
        let mut p = at_prompt(&short, Policy::Always, t0);
        let _e = press(&mut p, 2, KeyCode::Backspace, &short).unwrap();
        assert_eq!(drawn(&p), [(6, "h"), (7, "i"), (8, " ")], "the right prompt stays put");

        // 71 typed cells end at column 72, one blank before the right prompt.
        let long = with_right(&format!("{}c", "ab".repeat(35)), 40);
        let mut p = at_prompt(&short, Policy::Always, t0);
        let _r = p.on_frame(&long, 1, 0, t0);
        let _e = press(&mut p, 2, KeyCode::Backspace, &long).unwrap();
        assert!(drawn(&p).iter().all(|(col, _)| *col < 73), "{:?}", drawn(&p));

        let mut p = at_prompt(&short, Policy::Always, t0);
        let _r = p.on_frame(&long, 1, 0, t0);
        let _z = typing(&mut p, 2, "z", &long).unwrap();
        let tail: Vec<_> = drawn(&p).into_iter().filter(|(col, _)| *col >= 72).collect();
        assert_eq!(
            tail,
            [(72, "b"), (73, "c"), (74, " "), (75, " "), (76, " "), (77, " "), (78, " ")]
        );

        let mut p = at_prompt(&long, Policy::Always, t0);
        let _e = press(&mut p, 2, KeyCode::Backspace, &long).unwrap();
        assert!(
            drawn(&p).iter().any(|(col, _)| *col >= 73),
            "never seen apart from the text, one blank from it reads as more text"
        );
    }
}
