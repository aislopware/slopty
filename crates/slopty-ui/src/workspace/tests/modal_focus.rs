//! A modal holds the keyboard while it is open and hands it back where the focused tile keeps
//! it on every way out; a keyboard whose holder left the frame comes back on its own.

use super::*;

fn palette_open(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |v, _| v.palette.is_some())
}

/// The palette takes the keyboard. Tab stays in it rather than walking out to the shell behind
/// it, and Esc gives the keyboard back to the shell, not to the workspace's own handle, which
/// left the shell's cursor hollow.
#[gpui::test]
fn the_palette_holds_the_keyboard_and_hands_it_back_to_the_shell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = SessionId::new();
    let _tile = opens(&view, cx, &studio, shell, studio.me, 1);
    assert!(terminal_focused(&view, cx, shell), "the shell has the keyboard");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(palette_open(&view, cx));
    for _ in 0..4 {
        cx.simulate_keystrokes("tab");
        assert!(!terminal_focused(&view, cx, shell), "Tab stays in the palette");
    }
    assert!(palette_open(&view, cx));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!palette_open(&view, cx));
    assert!(terminal_focused(&view, cx, shell), "Esc hands the keyboard back to the shell");
}

/// What held the keyboard left the frame: with no modal open the keyboard goes back where
/// the focused tile keeps it; with the palette open, to the palette.
#[gpui::test]
fn a_lost_keyboard_comes_back_where_it_belongs(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = SessionId::new();
    let _tile = opens(&view, cx, &studio, shell, studio.me, 1);
    let lose = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.blur(cx);
            window.refresh();
        });
        cx.run_until_parked();
    };
    lose(cx);
    assert!(terminal_focused(&view, cx, shell), "back to the shell");

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    lose(cx);
    assert!(!terminal_focused(&view, cx, shell), "the open palette takes it back");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!palette_open(&view, cx), "and Esc reaches the palette");
}
