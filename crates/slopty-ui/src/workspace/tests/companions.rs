//! Companions in the workspace: in the mark slots, in the yard, and the frames they cost.

use std::time::Instant;

use gpui::Modifiers;
use slopty_theme::Companions;

use super::*;
use crate::companions::Pose;

/// `debug_bounds` wants a static selector; tests may leak a handful.
fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// The workspace with companions at `mode`.
fn companions(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, mode: Companions) {
    view.update(cx, |v, cx| {
        let mut theme = Theme::default();
        theme.behaviour.companions = mode;
        v.set_theme(theme, cx);
    });
    cx.run_until_parked();
}

/// The workspace's builds in one simulated second at 120 Hz.
fn frames_in_a_second(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> u64 {
    let before = view.read_with(cx, |v, _| v.drawn.builds.get());
    for _ in 0..120 {
        cx.executor().advance_clock(Duration::from_nanos(8_333_333));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    }
    view.read_with(cx, |v, _| v.drawn.builds.get()).wrapping_sub(before)
}

/// Every one of `sessions` reports `status`.
fn report(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    sessions: &[(SessionId, AgentStatus)],
) {
    view.update_in(cx, |v, _w, cx| {
        for (session, status) in sessions {
            v.agent_event(AgentEvent { status: status.clone(), ..blocked(*session) }, cx);
        }
    });
    cx.run_until_parked();
}

/// Five agents on one worker, each in its own tile.
fn five(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<(SessionId, TileRef)> {
    let studio = connect(view, cx, 1, "studio");
    (1..=5)
        .map(|version| {
            let session = SessionId::new();
            (session, opens(view, cx, &studio, session, studio.me, version))
        })
        .collect()
}

/// A crowd in mixed states: one at rest, one done, one waiting on the person, and the two
/// opened last, whose tiles are on screen, at work.
fn mixed(sessions: &[(SessionId, TileRef)]) -> Vec<(SessionId, AgentStatus)> {
    let states = [
        AgentStatus::Idle,
        AgentStatus::Done,
        AgentStatus::Blocked(BlockReason::Question),
        AgentStatus::Working,
        AgentStatus::Working,
    ];
    sessions.iter().zip(states).map(|((s, _), st)| (*s, st)).collect()
}

/// The same crowd at rest.
fn resting(sessions: &[(SessionId, TileRef)]) -> Vec<(SessionId, AgentStatus)> {
    sessions.iter().map(|(s, _)| (*s, AgentStatus::Idle)).collect()
}

/// Companions add no frame: with a crowd in mixed states the workspace draws the working
/// mark's twelve a second whether they are off, quiet or lively, and none once all rest.
#[gpui::test]
fn companions_draw_no_frame_the_working_mark_does_not(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let sessions = five(&view, cx);
    for mode in [Companions::Off, Companions::Quiet, Companions::Lively] {
        companions(&view, cx, mode);
        report(&view, cx, &mixed(&sessions));
        frames_in_a_second(&view, cx);
        let working = frames_in_a_second(&view, cx);
        assert!((11..=13).contains(&working), "{mode:?}: {working} frames a second at work");
        report(&view, cx, &resting(&sessions));
        // Lively, two finished turns hop once in tiles not focused: half a second of steps.
        assert!(frames_in_a_second(&view, cx) <= 8, "{mode:?}: the last steps");
        assert_eq!(frames_in_a_second(&view, cx), 0, "{mode:?}: at rest, nothing draws");
    }
}

/// Lively, every agent stands in the yard, needs you first; each is a button named for who it
/// is and what it does, and a click goes to its tile. Quiet has no yard.
#[gpui::test]
fn the_yard_stands_needs_you_first_and_a_click_goes_to_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let sessions = five(&view, cx);
    report(&view, cx, &mixed(&sessions));
    let x = |cx: &mut VisualTestContext, session: SessionId| {
        cx.debug_bounds(leak(format!("yard-{session}")))
            .unwrap_or_else(|| panic!("{session} is not in the yard"))
            .left()
    };
    let (waiting, working, resting) = (sessions[2].0, sessions[4].0, sessions[0].0);
    assert!(x(cx, waiting) < x(cx, working), "needs you stands first");
    assert!(x(cx, working) < x(cx, resting), "then work, then rest");
    let poses = view.read_with(cx, |v, cx| v.yard_poses(false, cx));
    assert_eq!(poses.first(), Some(&Pose::NeedsYou), "{poses:?}");

    let (session, tile) = sessions[3];
    let at = cx.debug_bounds(leak(format!("yard-{session}"))).expect("drawn").center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.focused()), Some(tile), "the click went to its tile");

    companions(&view, cx, Companions::Quiet);
    assert!(cx.debug_bounds("yard").is_none(), "quiet has no yard");
}

/// Two minutes after the person's last input everyone in the yard sleeps but who waits on
/// them, and an unattended yard asks for no frame.
#[gpui::test]
fn the_yard_sleeps_while_the_person_is_away(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let sessions = five(&view, cx);
    report(&view, cx, &mixed(&sessions));
    cx.update(|_w, cx| crate::companions::assume_away(cx, true));
    let poses = view.read_with(cx, |v, cx| v.yard_poses(true, cx));
    assert_eq!(poses.first(), Some(&Pose::NeedsYou), "who waits on the person stays up");
    assert!(poses.iter().skip(1).all(|p| *p == Pose::Asleep), "{poses:?}");
    report(&view, cx, &resting(&sessions));
    frames_in_a_second(&view, cx);
    assert_eq!(frames_in_a_second(&view, cx), 0, "nothing draws");
}

/// What the companions cost, headless: frames and the time spent drawing them in a second, a
/// crowd of eight in mixed states and then at rest, off against quiet and lively. Prints the
/// table recorded in `docs/MEASUREMENTS.md` ("companions on the step clock"):
/// `cargo nextest run -p slopty-ui --lib -E 'test(companions_cost)' --run-ignored only
/// --no-capture`.
#[gpui::test]
#[ignore = "a measurement: prints its numbers"]
fn companions_cost(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions: Vec<(SessionId, TileRef)> = (1..=8)
        .map(|version| {
            let session = SessionId::new();
            (session, opens(&view, cx, &studio, session, studio.me, version))
        })
        .collect();
    let crowd: Vec<(SessionId, AgentStatus)> = sessions
        .iter()
        .zip([
            AgentStatus::Idle,
            AgentStatus::Idle,
            AgentStatus::Idle,
            AgentStatus::Done,
            AgentStatus::Blocked(BlockReason::Question),
            AgentStatus::Working,
            AgentStatus::Working,
            AgentStatus::Working,
        ])
        .map(|((s, _), st)| (*s, st))
        .collect();
    for mode in [Companions::Off, Companions::Quiet, Companions::Lively] {
        companions(&view, cx, mode);
        for (arm, states) in [("mixed", crowd.clone()), ("rest", resting(&sessions))] {
            report(&view, cx, &states);
            frames_in_a_second(&view, cx);
            frames_in_a_second(&view, cx);
            let started = Instant::now();
            let frames: u64 =
                std::iter::repeat_with(|| frames_in_a_second(&view, cx)).take(5).sum();
            let spent = started.elapsed().checked_div(5).unwrap_or_default();
            let rate = frames.checked_div(5).unwrap_or_default();
            eprintln!("{mode:?} {arm}: {rate} frames a second, {spent:?} a second");
        }
    }
}
