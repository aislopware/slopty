//! Slopty's mark on the empty workspace: lit while a worker is reachable, its cursor blinking
//! at the caret's cadence without drawing anything round it, steady under Reduce Motion.

use slopty_theme::Motion;

use super::*;

fn cursor_lit(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |v, cx| v.empty_mark.read(cx).cursor_lit())
}

/// The empty workspace leads with the mark. Its cursor sits at the unlit level while no worker
/// is reachable, is lit once one is, and dims again when that worker's link drops.
#[gpui::test]
fn the_empty_workspaces_mark_is_lit_while_a_worker_is_reachable(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    cx.run_until_parked();
    assert!(cx.debug_bounds("empty-mark").is_some(), "the empty workspace shows the mark");
    assert!(!cursor_lit(&view, cx), "no worker: the cursor is unlit");
    let studio = connect(&view, cx, 1, "studio");
    assert!(cursor_lit(&view, cx), "a worker is reachable");
    let (mark, words) = (cx.debug_bounds("empty-mark"), cx.debug_bounds("empty-worker-0"));
    let (mark, words) = (mark.expect("the mark"), words.expect("the page's rows"));
    assert!(mark.bottom() <= words.top(), "over the page's words");
    let centre = |b: Bounds<Pixels>| b.left() + b.size.width / 2.0;
    assert!((centre(mark) - centre(words)).abs() < px(1.0), "centred: {mark:?} over {words:?}");
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    assert!(!cursor_lit(&view, cx), "its link dropped: unlit again");
}

/// The cursor blinks at the caret's cadence, and a blink draws the mark alone: the panes it
/// sits on are not built again. Under Reduce Motion it holds steady, its clock stopped.
#[gpui::test]
fn the_cursor_blinks_without_building_the_strip_and_holds_under_reduce_motion(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    cx.run_until_parked();
    let builds = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.drawn.builds.get());
    let before = builds(cx);
    let mut phases = Vec::new();
    for _ in 0..4 {
        cx.executor().advance_clock(Motion::DEFAULT.blink);
        cx.run_until_parked();
        phases.push(cursor_lit(&view, cx));
    }
    assert_eq!(phases, [false, true, false, true], "on and off at the caret's cadence");
    assert_eq!(builds(cx), before, "a blink draws the mark alone");

    cx.update(|_w, cx| cx.set_reduce_motion(true));
    for _ in 0..3 {
        cx.executor().advance_clock(Motion::DEFAULT.blink);
        cx.run_until_parked();
    }
    assert!(cursor_lit(&view, cx), "steady, and lit");
    let running = view.read_with(cx, |v, cx| v.empty_mark.read(cx).blinking());
    assert!(!running, "no clock runs for a steady cursor");
}
