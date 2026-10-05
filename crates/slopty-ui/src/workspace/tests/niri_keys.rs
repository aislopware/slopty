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

fn full_width(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> bool {
    view.read_with(cx, |v, _| {
        let layout = v.layout();
        let pos = layout.position(tile).expect("placed");
        layout.workspaces()[pos.workspace].columns()[pos.column].is_full_width()
    })
}

fn navigator_shown(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> bool {
    view.read_with(cx, |v, _| v.layout().navigator().shown)
}

/// ⇧⌘↩ is focus mode: the focused column takes the working width and the docked navigator
/// steps aside; pressed again, both come back as they were. Ended some other way (the width
/// key), the navigator still comes back, and a navigator the person had put away stays away.
#[gpui::test]
fn focus_mode_gives_the_work_the_width_and_puts_everything_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, _, (s3, third)] = three_shells(&view, cx, &fake);
    assert!(terminal_focused(&view, cx, s3));
    let before = view.read_with(cx, |v, _| {
        let pos = v.layout().position(third).expect("placed");
        v.layout().workspaces()[pos.workspace].columns()[pos.column].width()
    });
    assert!(navigator_shown(&view, cx), "docked beside the strip at this width");

    press(cx, "cmd-shift-enter");
    assert!(full_width(&view, cx, third), "the work takes the width");
    assert!(!navigator_shown(&view, cx), "and the navigator steps aside");
    press(cx, "cmd-shift-enter");
    assert!(!full_width(&view, cx, third));
    assert!(navigator_shown(&view, cx), "both come back");
    let after = view.read_with(cx, |v, _| {
        let pos = v.layout().position(third).expect("placed");
        v.layout().workspaces()[pos.workspace].columns()[pos.column].width()
    });
    assert_eq!(after, before, "with the width the column had");

    press(cx, "cmd-shift-enter");
    assert!(!navigator_shown(&view, cx));
    view.update_in(cx, |v, _w, cx| {
        v.layout.toggle_full_width();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(navigator_shown(&view, cx), "ended another way, the navigator still comes back");

    view.update_in(cx, |v, window, cx| v.toggle_navigator(&ToggleNavigator, window, cx));
    cx.run_until_parked();
    press(cx, "cmd-shift-enter");
    press(cx, "cmd-shift-enter");
    assert!(!navigator_shown(&view, cx), "put away by the person, it stays away");
}
