//! slopty: the shell's one clock. What the shell draws reads the moment of the frame being drawn
//! ([`gpui::App::frame_time`]), and what an event stamps reads the same clock between frames, so
//! a frame drawn twice at one moment draws alike (the retained check, the self-test's dump, the
//! fuzzer's `wall clock` oracle) and a headless test's clock is the dispatcher's, which moves only
//! when the test moves it. The wall clock is read here alone; clippy refuses it elsewhere in the
//! shell (`clippy.toml`, `disallowed-methods`).

use std::cell::Cell;
use std::time::Instant;

use chrono::{DateTime, Utc};
use gpui::App;

/// The moment the frame being drawn shows; between frames, now.
pub fn now(cx: &App) -> Instant {
    cx.frame_time()
}

/// The wall date at the moment the frame being drawn shows ([`now`]): the dates a row says
/// ("2m ago", "online") read it, so they stand still within a frame drawn twice. While the
/// self-test pins the clock ([`pin`]), the date it pinned.
pub fn utc(cx: &App) -> DateTime<Utc> {
    cx.try_global::<Pinned>().and_then(|pinned| pinned.0).unwrap_or_else(|| utc_at(cx.frame_time()))
}

/// The date [`utc`] reads while the self-test pins it.
struct Pinned(Option<DateTime<Utc>>);

impl gpui::Global for Pinned {}

/// Read every date the shell draws as at `at_ms`, Unix milliseconds, from now on, or by the wall
/// clock again for `None` (and for a moment no date can hold): the self-test's `PinClock`, so a
/// golden that shows an age ("now", "5m") or a date holds the same one whenever it is taken. A
/// surface that keeps the date it last read (a page's clock, the session list's) reads the pin
/// the next time it reads the clock, as its state moves or its tick comes, so a test pins before
/// the step it draws. The caller draws the window again.
pub fn pin(at_ms: Option<u64>, cx: &mut App) {
    let at = at_ms.and_then(|ms| i64::try_from(ms).ok()).and_then(DateTime::from_timestamp_millis);
    cx.set_global(Pinned(at));
}

thread_local! {
    /// The app's dispatcher on this thread, whose clock [`event_now`] reads.
    static DISPATCHER: std::cell::RefCell<Option<gpui::BackgroundExecutor>> =
        const { std::cell::RefCell::new(None) };
}

/// Read this thread's [`event_now`] from `executor`'s clock: the app's dispatcher (the shell
/// sets it as it starts), or a test's, which the test moves.
pub fn run_on(executor: gpui::BackgroundExecutor) {
    DISPATCHER.with(|slot| *slot.borrow_mut() = Some(executor));
}

/// Now on the dispatcher's clock, for a stamp made where no `App` is at hand (a hover's flip, a
/// scroll seen): the clock [`now`] reads between frames. Before the shell starts, a stopwatch's.
pub fn event_now() -> Instant {
    DISPATCHER.with(|slot| slot.borrow().as_ref().map_or_else(stopwatch, gpui::BackgroundExecutor::now))
}

/// A stopwatch's start, for what measures work (a log line's milliseconds, a frame's cost, a
/// deadline a task waits out): never what a frame draws.
#[allow(clippy::disallowed_methods)] // a measure of work, not a moment drawn
pub fn stopwatch() -> Instant {
    Instant::now()
}

/// The time since a [`stopwatch`] started.
#[allow(clippy::disallowed_methods)] // a measure of work, not a moment drawn
pub fn spent(since: Instant) -> std::time::Duration {
    since.elapsed()
}

/// How far the wall date may run from the monotonic clock before it is read again: a Mac that
/// slept, or the date set by hand. Below a second, a moment maps to one date.
const DRIFT: chrono::TimeDelta = chrono::TimeDelta::seconds(1);

thread_local! {
    /// A monotonic moment and the wall date read with it.
    static ANCHOR: Cell<Option<(Instant, DateTime<Utc>)>> = const { Cell::new(None) };
}

/// The wall date at the monotonic moment `at`: the date read once with a moment, moved by the
/// time between, and read again only when the two clocks drift apart.
#[allow(clippy::disallowed_methods)] // the shell's one wall-clock read
pub fn utc_at(at: Instant) -> DateTime<Utc> {
    let (mono, wall) = (Instant::now(), Utc::now());
    let (mono, wall) = ANCHOR.with(|anchor| match anchor.get() {
        Some((then, date)) if (shift(date, then, mono) - wall).abs() < DRIFT => (then, date),
        _ => {
            anchor.set(Some((mono, wall)));
            (mono, wall)
        }
    });
    shift(wall, mono, at)
}

/// `date` at `mono`, moved to `at`.
fn shift(date: DateTime<Utc>, mono: Instant, at: Instant) -> DateTime<Utc> {
    let delta = |d: std::time::Duration| chrono::TimeDelta::from_std(d).unwrap_or(chrono::TimeDelta::MAX);
    let moved = if at >= mono {
        date.checked_add_signed(delta(at - mono))
    } else {
        date.checked_sub_signed(delta(mono - at))
    };
    moved.unwrap_or(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_moment_is_one_date() {
        let at = Instant::now();
        let first = utc_at(at);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(utc_at(at), first);
        let later = utc_at(at + std::time::Duration::from_secs(90));
        assert_eq!(later - first, chrono::TimeDelta::seconds(90));
    }

    /// A pinned date holds however the clock moves, and the wall clock runs again once the pin
    /// is lifted, or when the pin names a moment no date holds.
    #[gpui::test]
    fn a_pinned_date_holds_until_the_pin_lifts(cx: &mut gpui::TestAppContext) {
        const PINNED: i64 = 1_792_000_000_000;
        let pinned = DateTime::from_timestamp_millis(PINNED).unwrap_or_default();
        let running = |cx: &mut gpui::TestAppContext| cx.update(|cx| utc(cx) != pinned);
        cx.update(|cx| pin(u64::try_from(PINNED).ok(), cx));
        assert_eq!(cx.update(|cx| utc(cx)), pinned);
        cx.executor().advance_clock(std::time::Duration::from_secs(90));
        assert_eq!(cx.update(|cx| utc(cx)), pinned, "the clock moved; the pinned date did not");
        cx.update(|cx| pin(None, cx));
        assert!(running(cx), "unpinned, the wall clock runs");
        let before = cx.update(|cx| utc(cx));
        cx.executor().advance_clock(std::time::Duration::from_secs(90));
        assert_eq!(cx.update(|cx| utc(cx)) - before, chrono::TimeDelta::seconds(90));
        cx.update(|cx| pin(Some(u64::MAX), cx));
        assert!(running(cx), "a moment no date holds pins nothing");
    }
}
