//! ⌘W through the keymap and the shell window's own handler: it closes the focused pane of the
//! window it was pressed in, whichever window the system calls active. ⌘W is bound on Apple's
//! keyboards, a Mac's here.
#![cfg(target_os = "macos")]

use gpui::{TestAppContext, VisualTestContext};

use super::panes::{Direction, Pane};
use super::retention_tests::{DESKTOP, Screen, launch};
use super::*;

/// An iPad's screen, landscape, as a touch screen's.
const IPAD: Screen = Screen { size: (1366.0, 1024.0), scale: 2.0, touch: true, reduced: true };

/// Chat `a` open, its session's terminal in its own pane.
fn open_session(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    shell.update(cx, |shell, cx| {
        shell.state.update(cx, |state, cx| {
            state.apply_session_terminals([("a".to_owned(), "t-a".to_owned())].into());
            cx.notify();
        });
        shell.open_chat("a".into(), cx);
    });
    draw(cx);
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
}

/// ⌘W closes the focused pane of the window it was pressed in, a view beside the session and
/// then a split shell, and with only the session's own pane left the window itself, while the
/// app is in the background and no window is the active one (as the e2e guest's app always is).
#[gpui::test]
fn cmd_w_closes_the_focused_pane_of_its_window_with_the_app_in_the_background(
    cx: &mut TestAppContext,
) {
    let (shell, cx) = launch(cx, DESKTOP);
    open_session(&shell, cx);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.split_pane(Some(Direction::Right), cx)));
    draw(cx);
    let split = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
    assert!(matches!(split, Pane::Shell(_)), "the split has the keyboard: {split:?}");
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.add_diff_surface(window, cx)));
    draw(cx);
    let panes = |cx: &mut VisualTestContext| shell.read_with(cx, |s, cx| s.panes_now(cx).panes());
    let view = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
    assert!(matches!(view, Pane::View(RightSurface::Diff(_))), "the view has it: {view:?}");
    assert_eq!(panes(cx).len(), 3, "{:?}", panes(cx));
    cx.update(|window, cx| {
        assert!(shell.read(cx).pane_holds_keyboard(view, window, cx), "in the view's own keys");
    });

    cx.deactivate_window();
    assert!(cx.update(|_, cx| cx.active_window()).is_none(), "the app in the background");
    cx.simulate_keystrokes("cmd-w");
    draw(cx);
    let left = panes(cx);
    assert!(
        left.len() == 2 && left.contains(&Pane::Session) && left.contains(&split),
        "the view closed, the rest kept: {left:?}"
    );
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.focus_pane(split, cx)));
    draw(cx);
    cx.simulate_keystrokes("cmd-w");
    draw(cx);
    assert_eq!(panes(cx), [Pane::Session], "the split closed");
    assert_eq!(cx.windows().len(), 1, "the window kept while a pane closed");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 0, "the session's own pane closes the window");
}

/// On an iPad's keyboard ⌘W closes the focused pane as on a Mac's, a split shell beside the
/// session; with the session's own pane alone it closes nothing, for the window is the app's
/// scene there, which only the system shows and puts away.
#[gpui::test]
fn cmd_w_on_an_ipad_closes_the_split_and_keeps_the_scene(cx: &mut TestAppContext) {
    let (shell, cx) = launch(cx, IPAD);
    open_session(&shell, cx);
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.split_pane(Some(Direction::Right), cx)));
    draw(cx);
    let panes = |cx: &mut VisualTestContext| shell.read_with(cx, |s, cx| s.panes_now(cx).panes());
    let split = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
    assert!(matches!(split, Pane::Shell(_)), "an iPad splits, the split focused: {split:?}");
    assert_eq!(panes(cx).len(), 2, "{:?}", panes(cx));
    cx.simulate_keystrokes("cmd-w");
    draw(cx);
    assert_eq!(panes(cx), [Pane::Session], "the split closed");
    cx.simulate_keystrokes("cmd-w");
    draw(cx);
    assert_eq!(cx.windows().len(), 1, "the scene's window stays");
    assert_eq!(panes(cx), [Pane::Session], "and the session in it");
    force_touch(false);
}
