//! When a companion's one-off moment (a wave, a hop) draws its next frame.
//!
//! A working companion steps on the working mark's clock ([`crate::icons::wake_at_next_step`])
//! and adds no frame of its own. A wave or a hop changes its frame only every few steps, and
//! may play while nothing works and the clock sleeps, so it asks for the one step its next frame
//! falls on. That step is on the same grid as the clock's (whole steps since its start), and a
//! view the clock wakes at its next step anyway is left to it: woken twice at one moment, a
//! window would build twice.

use std::time::Duration;

use gpui::{App, EntityId, Global, Window};

use crate::icons::{SPIN_STEP, steps_now, steps_wake};

/// The views waiting for a companion's next frame, and the one timer that is out.
#[derive(Default)]
struct Wakes {
    /// When each view draws again, as time since the spin clock's start.
    due: Vec<(Duration, EntityId)>,
    /// The time the timer out is for, and its number: one whose number is not the latest was
    /// let go and does nothing when it fires.
    armed: Option<Duration>,
    timers: u64,
}

impl Global for Wakes {}

impl Wakes {
    fn get(cx: &mut App) -> &mut Self {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::default());
        }
        cx.global_mut::<Self>()
    }

    /// Set the timer for `at`, `now` being the clock's time now.
    fn arm(cx: &mut App, at: Duration, now: Duration) {
        let wakes = Self::get(cx);
        wakes.armed = Some(at);
        wakes.timers = wakes.timers.wrapping_add(1);
        let number = wakes.timers;
        let timer = cx.background_executor().timer(at.saturating_sub(now));
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                if Self::get(cx).timers == number {
                    Self::fire(cx);
                }
            });
        })
        .detach();
    }

    /// Wake every view whose time came, but one the spin clock wakes at its next step (this
    /// one, or the one after it once it drew this), and set the timer for the next.
    fn fire(cx: &mut App) {
        let now = steps_now(cx);
        let wakes = Self::get(cx);
        wakes.armed = None;
        let mut woken = Vec::new();
        wakes.due.retain(|(at, view)| {
            let due = *at <= now;
            if due && !woken.contains(view) {
                woken.push(*view);
            }
            !due
        });
        let next = wakes.due.iter().map(|(at, _)| *at).min();
        for view in woken {
            if !steps_wake(cx, view) {
                cx.notify(view);
            }
        }
        if let Some(at) = next {
            Self::arm(cx, at, now);
        }
    }
}

/// The start of the clock's step `at` falls in, or of the one after when `at` is inside one:
/// a moment on the clock's grid.
#[must_use]
pub(super) fn on_the_grid(at: Duration) -> Duration {
    let step = SPIN_STEP.as_nanos();
    let into = at.as_nanos().checked_rem(step).unwrap_or(0);
    if into == 0 {
        return at;
    }
    let up = at.as_nanos().saturating_add(step.saturating_sub(into));
    Duration::from_nanos(u64::try_from(up).unwrap_or(u64::MAX))
}

/// The view being painted draws again at `at` (time since the spin clock's start), put on the
/// clock's grid.
pub(super) fn wake_at(window: &Window, cx: &mut App, at: Duration) {
    let at = on_the_grid(at);
    let view = window.current_view();
    let now = steps_now(cx);
    let wakes = Wakes::get(cx);
    if !wakes.due.contains(&(at, view)) {
        wakes.due.push((at, view));
    }
    if wakes.armed.is_none_or(|armed| at < armed) {
        Wakes::arm(cx, at, now);
    }
}
