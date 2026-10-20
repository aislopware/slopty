// slopty: Slopty's own tests (not in zeron).
//! slopty: a frame can begin past the moment a view's words change before the timer that says
//! so has run, and draw that view again from the last frame: what it shows then must be what a
//! frame drawn from scratch at its moment shows (the e2e frame checker's stale frames). Each case
//! moves the dispatcher's clock with nothing run that came due, as a display's frame does.
use super::retention_tests::{DESKTOP, launch, stale};
use super::*;
use gpui::{TestAppContext, VisualTestContext};
use slopty_view_doc::dock::{ChatDock, GoalLine};

/// The dispatcher's clock moved on by `by` with nothing run that came due by then.
fn clock_ahead(cx: &VisualTestContext, by: Duration) {
    cx.executor().dispatcher().as_test().expect("a test dispatcher").scheduler().clock().advance(by);
}

/// A frame drawn with no view told to draw again, every view drawn again from the last frame
/// (as one an animation elsewhere asks for), against one drawn from scratch at its moment: where
/// they differ.
fn frame(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    stale(cx)
}

/// The chat at work opened, and its composer.
fn opened(cx: &mut TestAppContext) -> (Entity<Shell>, Entity<Composer>, &mut VisualTestContext) {
    let (shell, cx) = launch(cx, DESKTOP);
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    let composer = shell.read_with(cx, |shell, _| shell.composer.clone());
    (shell, composer, cx)
}

/// A turn whose lease runs out says so when the state does: a frame at the deadline, before the
/// state's timer has run, still shows Stop as a frame from scratch does, and Send comes once the
/// timer has told the composer.
#[gpui::test]
fn a_stale_lease_flips_stop_when_the_state_says_so(cx: &mut TestAppContext) {
    let (shell, composer, cx) = opened(cx);
    let live = |cx: &mut VisualTestContext| composer.read_with(cx, |composer, cx| composer.run_live(cx));
    assert!(live(cx), "the turn under way: Stop");
    let now = crate::state::clock_now();
    let deadline = shell
        .read_with(cx, |shell, cx| shell.state.read(cx).next_clock_transition(now))
        .expect("the lease's deadline");
    clock_ahead(cx, (deadline - now).to_std().unwrap());
    assert_eq!(frame(cx), None, "at the deadline, before the state said it");
    assert!(live(cx), "Stop until the state says the lease ran out");
    cx.run_until_parked();
    assert!(!live(cx), "Send once it has");
    assert_eq!(frame(cx), None, "once said");
}

/// The goal's words in the footer, as the last frame's accessibility tree says them.
fn goal_words(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|window, _| {
        let tree = window.a11y_tree()?;
        tree.nodes
            .iter()
            .filter_map(|(_, node)| node.label())
            .find(|label| label.starts_with("Goal"))
            .map(str::to_owned)
    })
}

/// A goal's age turns with the dock's tick, not with the frame: a frame past the age's turn,
/// before the tick has run, says the age a frame from scratch says, and the tick turns it.
#[gpui::test]
fn a_goals_age_turns_with_the_docks_tick(cx: &mut TestAppContext) {
    let (_shell, composer, cx) = opened(cx);
    cx.update(|window, _| window.set_a11y_active(true));
    composer.update(cx, |composer, cx| {
        let set_at = crate::clock::utc(cx).timestamp_millis() - 5_400;
        let goal = GoalLine { condition: "all tests pass".into(), set_at, ..GoalLine::default() };
        composer.apply_dock("a", ChatDock { goal: Some(goal), ..ChatDock::default() }, cx);
    });
    // Its footer comes in and settles, well inside the dock's second.
    let drawn = super::retention_tests::frames(cx, Duration::from_millis(16), 15);
    assert!(drawn < 15, "the footer settled");
    assert_eq!(frame(cx), None, "the goal shown");
    assert_eq!(goal_words(cx).as_deref(), Some("Goal · 5s"));
    // 0.9 s after it came: six seconds and three tenths old, the tick still a tenth away.
    let settled = Duration::from_millis(16).saturating_mul(u32::try_from(drawn).unwrap_or(0));
    clock_ahead(cx, Duration::from_millis(900).saturating_sub(settled));
    assert_eq!(frame(cx), None, "past its sixth second, before the tick ran");
    assert_eq!(goal_words(cx).as_deref(), Some("Goal · 5s"), "until the tick");
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(frame(cx), None, "the tick ran");
    assert_eq!(goal_words(cx).as_deref(), Some("Goal · 6s"), "the tick turned it");
}
