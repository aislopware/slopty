//! The niri ops past the arrows reach the layout by key: the workspace keys and carrying a
//! column to either end of the strip.

use super::*;

fn press(cx: &mut VisualTestContext, keys: &str) {
    cx.simulate_keystrokes(keys);
    cx.run_until_parked();
}

fn workspace_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> usize {
    view.read_with(cx, |v, _| v.layout().position(tile).map(|p| p.workspace)).expect("placed")
}

fn active_workspace(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> usize {
    view.read_with(cx, |v, _| v.layout().active_workspace())
}

/// ⌘⌥⇟/⇞ step between workspaces, ⌘⌥N jumps to one, and the keyboard follows the focus into
/// each terminal.
#[gpui::test]
fn the_workspace_keys_switch_workspaces(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, (s2, second), (s3, third)] = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_window_down_or_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(workspace_of(&view, cx, third), 1);

    press(cx, "cmd-alt-pageup");
    assert_eq!((active_workspace(&view, cx), focused(&view, cx)), (0, Some(second)));
    assert!(terminal_focused(&view, cx, s2), "the keyboard followed");
    press(cx, "cmd-alt-pagedown");
    assert_eq!((active_workspace(&view, cx), focused(&view, cx)), (1, Some(third)));
    assert!(terminal_focused(&view, cx, s3));

    press(cx, "cmd-alt-1");
    assert_eq!(active_workspace(&view, cx), 0);
    press(cx, "cmd-alt-2");
    assert_eq!(active_workspace(&view, cx), 1);
    press(cx, "cmd-alt-9");
    assert_eq!(active_workspace(&view, cx), 2, "past the last: the trailing empty one");
}

/// ⌘⌥⇧Home/End carry the focused column to the start and the end of the strip.
#[gpui::test]
fn the_end_keys_carry_a_column_to_the_ends_of_the_strip(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), (_, third)] = three_shells(&view, cx, &fake);
    press(cx, "cmd-3");
    assert_eq!(focused(&view, cx), Some(third));

    press(cx, "cmd-alt-shift-home");
    assert_eq!(column_of(&view, cx, third), 0);
    assert_eq!((column_of(&view, cx, first), column_of(&view, cx, second)), (1, 2));
    assert_eq!(focused(&view, cx), Some(third), "and still focused");
    press(cx, "cmd-alt-shift-end");
    assert_eq!(column_of(&view, cx, third), 2);
}

/// Every one of these ops has a palette line that shows its key.
#[test]
fn every_niri_op_has_a_palette_line_with_its_key() {
    let items = palette_items();
    for label in [
        "Workspace above",
        "Workspace below",
        "First workspace",
        "Move column to the start",
        "Move column to the end",
    ] {
        let line = items.iter().find(|i| i.label == label).unwrap_or_else(|| panic!("{label}"));
        assert!(!line.keys.is_empty(), "{label} shows no key");
    }
}
