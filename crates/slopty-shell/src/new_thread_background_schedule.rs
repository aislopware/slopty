//! slopty: when the new-thread hero's glyph field shows, moves and rests.
//!
//! Claude desktop's landing texture waits until the landing has been on screen for 3 s, fades
//! in over 3 s, animates at 25 frames a second for at most 180 s and then settles to a still
//! frame over 8 s, wakes again for the pointer or the keyboard, pauses while its window is in
//! the background or hidden, and fades out over 500 ms when a session starts. Under reduced
//! motion it is one still frame (`.research/claude-desktop-2.42304.2-2026-10-19.md`, §A.5–A.7).
//! This is that timeline, as a pure function of the moments it is asked at, so a test steps it
//! by hand and the field's view only asks it what to draw and when to draw again.

use std::time::{Duration, Instant};

/// How long the hero must be on screen before the field starts to show.
pub(crate) const DELAY: Duration = Duration::from_secs(3);
/// How long the field takes to fade in, counted in time it animates.
pub(crate) const FADE_IN: f32 = 3.0;
/// How long it takes to fade out when the hero goes, or back in when it returns mid-fade.
pub(crate) const FADE_OUT: f32 = 0.5;
/// How long it animates before it rests, from its start or the last stir.
pub(crate) const BUDGET: f32 = 180.0;
/// How long the wind takes to settle to a stop, and to come back up after a stir.
const SETTLE: f32 = 8.0;
const RESUME: f32 = 3.0;
/// How long the wind takes to arrive.
const REVEAL: f32 = 4.0;
/// A frame every 40 ms (25 a second); while a ripple runs or the field fades out, every
/// display frame's worth.
pub(crate) const FRAME: Duration = Duration::from_millis(40);
pub(crate) const QUICK_FRAME: Duration = Duration::from_millis(16);
/// The longest step the wind takes between two frames, so a late frame never jumps.
const MAX_STEP: f32 = 0.25;
/// The wind's moment in the still frame drawn under reduced motion: long after it has arrived,
/// with gusts across the pane.
pub(crate) const STILL_WIND: f32 = 26.0;

/// What the field's surroundings are at a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Inputs {
    /// The hero is up and the field belongs on it: no chat selected, no wallpaper, no
    /// increased contrast.
    pub hero: bool,
    /// The window's frames are shown (not minimised, covered or on another Space).
    pub visible: bool,
    /// The window is the key window.
    pub active: bool,
    /// Motion is reduced.
    pub reduced: bool,
    /// A click's ripple is running, which wants every display frame.
    pub rippling: bool,
}

/// What to draw at a moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Shown {
    /// The field's opacity, 0 to 1; 0 draws nothing.
    pub opacity: f32,
    /// The wind's clock, in seconds.
    pub wind: f32,
    /// How much of the wind has arrived, 0 to 1.
    pub reveal: f32,
}

/// The field's timeline over one visit to the hero and the next.
#[derive(Debug, Clone, Default)]
pub(crate) struct Schedule {
    /// Since when the hero has been up in a shown window, without a break.
    since: Option<Instant>,
    /// Time the field has animated since it began to fade in; `None` before that.
    shown: Option<f32>,
    /// The wind's clock.
    wind: f32,
    /// How fast the wind's clock runs, 0 (still) to 1.
    rate: f32,
    /// Animated time at which the field rests unless stirred.
    budget_end: f32,
    /// The hero went at this moment, from this opacity: the field is fading out.
    leaving: Option<(Instant, f32)>,
    /// The hero came back at this moment while the field was fading out, at this opacity.
    returning: Option<(Instant, f32)>,
    /// The last moment the field was advanced while it animates.
    last: Option<Instant>,
    /// The visit ended: the field faded out, and the next one draws new weather.
    ended: bool,
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t) * (1.0 - t)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * 2.0f32.mul_add(-t, 3.0)
}

fn seconds(from: Instant, to: Instant) -> f32 {
    to.saturating_duration_since(from).as_secs_f32()
}

impl Schedule {
    /// Advance to `now` with the field's surroundings `inputs`, and say what to draw.
    pub(crate) fn advance(&mut self, now: Instant, inputs: Inputs) -> Shown {
        if !inputs.hero {
            self.since = None;
            self.returning = None;
            let opacity = self.resting_opacity(now, inputs.reduced);
            if self.shown.is_some() && self.leaving.is_none() {
                self.leaving = Some((now, opacity));
            }
            let gone = self.leaving.is_none_or(|(at, from)| {
                inputs.reduced || from <= 0.0 || seconds(at, now) >= FADE_OUT
            });
            if gone {
                if self.shown.is_some() {
                    self.ended = true;
                }
                *self = Self { ended: self.ended, ..Self::default() };
                return Shown { opacity: 0.0, wind: 0.0, reveal: 0.0 };
            }
            self.last = None;
            return self.shown_at(now, inputs.reduced);
        }
        if let Some((_, from)) = self.leaving.take() {
            // Back before the fade-out ended: the same visit fades back up.
            let opacity = self.leaving_opacity(now, from);
            self.returning = Some((now, opacity));
        }
        if inputs.reduced {
            self.since.get_or_insert(now);
            self.shown = Some(FADE_IN);
            self.wind = STILL_WIND;
            self.rate = 0.0;
            self.last = None;
            return Shown { opacity: 1.0, wind: STILL_WIND, reveal: 1.0 };
        }
        if self.shown.is_none() {
            if inputs.visible {
                self.since.get_or_insert(now);
            } else {
                // Hidden before it showed: the wait starts over when the window is seen again.
                self.since = None;
            }
        }
        let running = inputs.visible && inputs.active;
        if let Some(since) = self.since.filter(|_| self.shown.is_none() && running) {
            let due = since + DELAY;
            if now >= due {
                // On time, the fade counts from its due moment; after a pause, from now.
                let late = seconds(due, now);
                self.shown = Some(if late < MAX_STEP { late } else { 0.0 });
                self.rate = 1.0;
                self.budget_end = BUDGET;
                self.wind = self.shown.unwrap_or(0.0);
                self.last = Some(now);
            }
        }
        if let (Some(shown), true) = (self.shown, running) {
            let step = self.last.map_or(0.0, |last| seconds(last, now).min(MAX_STEP));
            let shown = shown + step;
            let target = if shown < self.budget_end { 1.0 } else { 0.0 };
            self.rate = if target > self.rate {
                (self.rate + step / RESUME).min(1.0)
            } else {
                (self.rate - step / SETTLE).max(0.0)
            };
            self.wind += smoothstep(self.rate) * step;
            self.shown = Some(shown);
            self.last = Some(now);
        } else {
            // Paused: the next frame takes no step for the time it was away.
            self.last = None;
        }
        self.shown_at(now, false)
    }

    /// The pointer or the keyboard stirred the field at `now`: it animates on for another
    /// budget, easing back up if it had settled.
    pub(crate) fn stir(&mut self) {
        if let Some(shown) = self.shown {
            self.budget_end = shown + BUDGET;
        }
    }

    /// Whether the visit ended since this was last asked: the next one draws new weather.
    pub(crate) fn take_ended(&mut self) -> bool {
        std::mem::take(&mut self.ended)
    }

    /// When the field next needs a frame after `now`, if it does: none while it rests, while
    /// its window is in the background or hidden, or under reduced motion.
    pub(crate) fn next_frame(&self, now: Instant, inputs: Inputs) -> Option<Duration> {
        if self.leaving.is_some() {
            return (!inputs.reduced).then_some(QUICK_FRAME);
        }
        if !inputs.hero || inputs.reduced || !inputs.visible || !inputs.active {
            return None;
        }
        let Some(shown) = self.shown else {
            return self.since.map(|since| (since + DELAY).saturating_duration_since(now));
        };
        let fading = shown < FADE_IN || self.returning.is_some_and(|(at, _)| seconds(at, now) < FADE_OUT);
        if inputs.rippling {
            Some(QUICK_FRAME)
        } else if self.rate > 0.0 || shown < self.budget_end || fading {
            Some(FRAME)
        } else {
            None
        }
    }

    fn fade_in(&self) -> f32 {
        self.shown.map_or(0.0, |shown| ease_out(shown / FADE_IN))
    }

    fn leaving_opacity(&self, now: Instant, from: f32) -> f32 {
        from * (1.0 - seconds(self.leaving.map_or(now, |(at, _)| at), now) / FADE_OUT).clamp(0.0, 1.0)
    }

    /// The opacity while the hero is up.
    fn resting_opacity(&self, now: Instant, reduced: bool) -> f32 {
        if reduced {
            return if self.shown.is_some() { 1.0 } else { 0.0 };
        }
        let opacity = self.fade_in();
        match self.returning {
            Some((at, from)) => {
                let t = seconds(at, now) / FADE_OUT;
                if t >= 1.0 { opacity } else { from + (opacity - from) * ease_out(t) }
            }
            None => opacity,
        }
    }

    fn shown_at(&self, now: Instant, reduced: bool) -> Shown {
        let opacity = match self.leaving {
            Some((_, from)) => self.leaving_opacity(now, from),
            None => self.resting_opacity(now, reduced),
        };
        Shown { opacity, wind: self.wind, reveal: smoothstep(self.wind / REVEAL) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: Inputs = Inputs { hero: true, visible: true, active: true, reduced: false, rippling: false };

    /// Step the schedule from `start` by `step` until `until`, asking for each frame it asks
    /// for; returns the last moment reached.
    fn run(schedule: &mut Schedule, start: Instant, until: Duration, inputs: Inputs) -> Shown {
        let mut now = start;
        let end = start + until;
        let mut shown = schedule.advance(now, inputs);
        while let Some(next) = schedule.next_frame(now, inputs) {
            now += next.max(Duration::from_millis(1));
            if now > end {
                break;
            }
            shown = schedule.advance(now, inputs);
        }
        shown
    }

    /// Nothing for 3 s, then a fade in over 3 s on 25 frames a second, the wind arriving with it.
    #[test]
    fn the_field_waits_three_seconds_then_fades_in_over_three() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        assert_eq!(schedule.advance(start, ON).opacity, 0.0);
        assert_eq!(schedule.next_frame(start, ON), Some(DELAY), "one wake at the delay's end");
        let at = |s: f32| start + Duration::from_secs_f32(s);
        assert_eq!(schedule.advance(at(2.9), ON).opacity, 0.0);
        let shown = schedule.advance(at(3.0), ON);
        assert_eq!(shown.opacity, 0.0, "the fade starts at 3 s");
        assert_eq!(schedule.next_frame(at(3.0), ON), Some(FRAME), "25 frames a second");
        let mut now = at(3.0);
        let mut last = 0.0;
        while now < at(6.0) {
            now += FRAME;
            let shown = schedule.advance(now, ON);
            assert!(shown.opacity >= last, "the fade only rises");
            last = shown.opacity;
        }
        assert!((last - 1.0).abs() < 1e-3, "faded in by 6 s: {last}");
        assert!(schedule.advance(at(4.5), ON).reveal > 0.0);
    }

    /// The first send fades the field out over 500 ms, then the visit ends.
    #[test]
    fn the_first_send_fades_the_field_out_over_half_a_second() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        run(&mut schedule, start, Duration::from_secs(10), ON);
        let sent = start + Duration::from_secs(10);
        let left = Inputs { hero: false, ..ON };
        assert_eq!(schedule.advance(sent, left).opacity, 1.0);
        assert_eq!(schedule.next_frame(sent, left), Some(QUICK_FRAME));
        let half = schedule.advance(sent + Duration::from_millis(250), left).opacity;
        assert!((half - 0.5).abs() < 0.01, "linear over 500 ms: {half}");
        assert_eq!(schedule.advance(sent + Duration::from_millis(500), left).opacity, 0.0);
        assert_eq!(schedule.next_frame(sent + Duration::from_millis(500), left), None);
        assert!(schedule.take_ended(), "the next visit draws new weather");
    }

    /// In the background or hidden, the field holds its frame and asks for none; back in
    /// front, the wind goes on from where it stood.
    #[test]
    fn a_window_in_the_background_pauses_the_field() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        let before = run(&mut schedule, start, Duration::from_secs(20), ON);
        let blurred = Inputs { active: false, ..ON };
        let away = start + Duration::from_secs(20);
        let paused = schedule.advance(away, blurred);
        assert_eq!(schedule.next_frame(away, blurred), None, "no frames in the background");
        let later = schedule.advance(away + Duration::from_secs(60), blurred);
        assert_eq!(paused.wind, later.wind, "the wind stands still");
        assert!(paused.wind >= before.wind);
        assert_eq!(later.opacity, 1.0, "the frame stays");
        let hidden = Inputs { visible: false, ..ON };
        assert_eq!(schedule.next_frame(away, hidden), None, "no frames hidden");
        let back = schedule.advance(away + Duration::from_secs(61), ON);
        assert_eq!(back.wind, later.wind, "no jump for the time away");
        assert_eq!(schedule.next_frame(away + Duration::from_secs(61), ON), Some(FRAME));
    }

    /// Hidden before the field showed, the 3 s wait starts again when the window is seen.
    #[test]
    fn hiding_the_window_restarts_the_wait() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        schedule.advance(start, ON);
        schedule.advance(start + Duration::from_secs(2), Inputs { visible: false, ..ON });
        let seen = start + Duration::from_secs(5);
        schedule.advance(seen, ON);
        assert_eq!(schedule.next_frame(seen, ON), Some(DELAY));
    }

    /// Under reduced motion the field is one still frame, at once, and asks for nothing.
    #[test]
    fn reduced_motion_draws_one_still_frame() {
        let start = Instant::now();
        let reduced = Inputs { reduced: true, ..ON };
        let mut schedule = Schedule::default();
        let first = schedule.advance(start, reduced);
        assert_eq!(first, Shown { opacity: 1.0, wind: STILL_WIND, reveal: 1.0 });
        assert_eq!(schedule.next_frame(start, reduced), None);
        assert_eq!(schedule.advance(start + Duration::from_secs(30), reduced), first);
        assert_eq!(schedule.advance(start, Inputs { hero: false, ..reduced }).opacity, 0.0, "it goes at once");
    }

    /// 180 s of animation, then the wind settles to a still frame that asks for nothing; a stir
    /// wakes it.
    #[test]
    fn the_field_rests_after_its_budget_and_a_stir_wakes_it() {
        let start = Instant::now();
        let mut schedule = Schedule::default();
        let end = Duration::from_secs_f32(3.0 + BUDGET + SETTLE + 1.0);
        let rested = run(&mut schedule, start, end + Duration::from_secs(60), ON);
        let now = start + end + Duration::from_secs(60);
        assert_eq!(schedule.next_frame(now, ON), None, "at rest: no frames");
        let still = schedule.advance(now + Duration::from_secs(5), ON);
        assert_eq!(still.wind, rested.wind, "a still frame");
        let at_budget = {
            let mut s = Schedule::default();
            run(&mut s, start, Duration::from_secs_f32(3.0 + BUDGET - 1.0), ON).wind
        };
        assert!(at_budget > BUDGET - 2.0, "full speed through the budget: {at_budget}");
        schedule.stir();
        assert_eq!(schedule.next_frame(now, ON), Some(FRAME), "a stir wakes it");
        let woken = run(&mut schedule, now, Duration::from_secs(10), ON);
        assert!(woken.wind > still.wind + 5.0, "back up to speed");
    }
}
