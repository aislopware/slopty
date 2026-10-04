//! The wall clock the readouts are drawn by: how long a turn has run, when a record was
//! stamped, when a limit lifts, how old a line's author is.
//!
//! It is the system's clock, unless something pins it to one moment: the self-test does, so a
//! frame that shows a time shows the same one whenever it is drawn, and a golden holding it
//! never moves with the hour it was taken at. The working marks are pinned with it, to the
//! moment they stand upright and whole ([`crate::icons::PINNED_STEPS`]). What is kept or sent (a
//! backup's time, a visit counted) reads the system's clock itself.

use gpui::{App, Global};
use slopty_core::WallMs;

/// The moment the readouts are pinned to, when they are.
struct Pinned(Option<WallMs>);

impl Global for Pinned {}

/// Now, as the readouts show it.
#[must_use]
pub fn now(cx: &App) -> WallMs {
    cx.try_global::<Pinned>().and_then(|pinned| pinned.0).unwrap_or_else(WallMs::now)
}

/// Draw every readout as at `at` from now on, and every working mark still, or by the system's
/// clock again for `None`. The caller draws the window again: nothing that showed a time was told.
pub fn pin(at: Option<WallMs>, cx: &mut App) {
    cx.set_global(Pinned(at));
    crate::icons::pin_steps(at.map(|_| crate::icons::PINNED_STEPS), cx);
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use slopty_core::WallMs;

    /// Unpinned it is the system's clock; pinned, the moment it was pinned to whatever the hour;
    /// unpinned again, the system's.
    #[test]
    fn a_pinned_clock_holds_its_moment_until_let_go() {
        let cx = TestAppContext::single();
        let before = WallMs::now();
        let unpinned = cx.update(|cx| super::now(cx));
        assert!(unpinned >= before, "the system's clock");
        let moment = WallMs::from_millis(1_759_568_520_000);
        cx.update(|cx| super::pin(Some(moment), cx));
        assert_eq!(cx.update(|cx| super::now(cx)), moment);
        cx.update(|cx| super::pin(None, cx));
        assert!(cx.update(|cx| super::now(cx)) >= unpinned, "the system's again");
    }
}
