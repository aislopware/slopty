//! The wall clock the readouts are drawn by: how long a turn has run, when a record was
//! stamped, when a limit lifts, how old a line's author is, how long a command took.
//!
//! It is the system's clock, unless something pins it to one moment: the self-test does, so a
//! frame that shows a time shows the same one whenever it is drawn, and a golden holding it
//! never moves with the hour it was taken at. What is kept or sent (a backup's time, a visit
//! counted) reads the system's clock itself.

use std::time::{Duration, Instant};

use gpui::{App, Global};
use slopty_core::WallMs;

/// The moment the readouts are pinned to, when they are.
struct Pinned(Option<WallMs>);

impl Global for Pinned {}

/// How long since `started`, as a readout says it: nothing while the clock is pinned, so a span
/// measured under the self-test (a command's run) reads the same in every run.
#[must_use]
pub fn since(started: Instant, cx: &App) -> Duration {
    let pinned = cx.try_global::<Pinned>().is_some_and(|pinned| pinned.0.is_some());
    if pinned { Duration::ZERO } else { started.elapsed() }
}

/// Draw every readout as at `at` from now on, or by the system's clock again for `None`. The
/// caller draws the window again: nothing that showed a time was told.
pub fn pin(at: Option<WallMs>, cx: &mut App) {
    cx.set_global(Pinned(at));
}

#[cfg(test)]
mod tests {}
